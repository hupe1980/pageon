//! The live environment: real network, clock and randomness. The only module allowed to read
//! them (see `clippy.toml`).
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;

use crate::{Env, Identity, Limits, NetError, Profile, Request, Response, is_public};

/// Real network (HTTP/1.1 and HTTP/2 over rustls), real clock, real randomness.
///
/// Requests that may not reach the private network ([`Request::allow_private`] false) go
/// through a client whose DNS resolver drops every non-public address, so a public name that
/// resolves to `10.0.0.5` (or rebinds to it) is refused before connecting.
///
/// With [`Live::with_identity`], every request it sends is signed with Web Bot Auth.
/// With [`Live::browser_compatible`] (feature `chrome`), it connects as the pinned Chrome.
pub struct Live {
    public: reqwest::Client,
    open: reqwest::Client,
    runtime: tokio::runtime::Runtime,
    limits: Limits,
    identity: Option<Identity>,
    #[cfg(feature = "chrome")]
    chrome: Option<crate::chrome_client::Chrome>,
    /// Realtime connections (WebSocket, event streams), one thread each.
    streams: std::cell::RefCell<crate::realtime::Streams>,
    /// Requests go through a proxy (realtime connections do not, yet: refused).
    proxied: bool,
}

/// Resolves names and keeps only public addresses.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<SocketAddr> =
                all.iter().copied().filter(|a| is_public(a.ip())).collect();
            if public.is_empty() && !all.is_empty() {
                return Err(Box::new(NetError::Blocked(format!(
                    "{host} resolves to the private network address {}",
                    all[0].ip()
                )))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Resolves `host` the way [`Live`] does for requests that may not reach the private network.
pub(crate) async fn resolve_public(host: &str) -> Result<Vec<SocketAddr>, String> {
    use reqwest::dns::Resolve as _;
    let name: reqwest::dns::Name = host.parse().map_err(|_| format!("bad name {host}"))?;
    PublicOnly
        .resolve(name)
        .await
        .map(Iterator::collect)
        .map_err(|e| e.to_string())
}

fn client(proxy: Option<&reqwest::Proxy>, public_only: bool) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .user_agent(Profile::Agent.user_agent())
        .redirect(reqwest::redirect::Policy::none());
    if public_only {
        b = b.dns_resolver(Arc::new(PublicOnly));
    }
    if let Some(p) = proxy {
        b = b.proxy(p.clone());
    }
    b.build().unwrap_or_else(|e| panic!("HTTP client: {e}"))
}

impl Live {
    /// A live environment with the given limits. Proxies come from `HTTP_PROXY`,
    /// `HTTPS_PROXY` and `NO_PROXY`, or [`Live::with_proxy`].
    pub fn new(limits: Limits) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("runtime: {e}"));
        Self {
            public: client(None, true),
            open: client(None, false),
            runtime,
            limits,
            identity: None,
            #[cfg(feature = "chrome")]
            chrome: None,
            streams: std::cell::RefCell::default(),
            proxied: false,
        }
    }

    /// A live environment that connects as the pinned Chrome of
    /// [`Profile::BrowserCompatible`]: its TLS ClientHello and HTTP/2 settings. Needs
    /// the `chrome` feature; without it this says how to get it.
    pub fn browser_compatible(limits: Limits, platform: crate::Platform) -> Result<Self, String> {
        #[cfg(feature = "chrome")]
        {
            let mut live = Self::new(limits);
            live.chrome =
                Some(crate::chrome_client::Chrome::new(None, platform).map_err(|e| e.to_string())?);
            Ok(live)
        }
        #[cfg(not(feature = "chrome"))]
        {
            let _ = (limits, platform);
            Err(
                "this build has no browser-compatible profile\n  install one with \
                 `cargo install pageon --features chrome` (building it needs cmake and clang)"
                    .into(),
            )
        }
    }

    /// Tests only: the browser-compatible client, trusting any certificate (a local TLS
    /// server with a test certificate).
    #[doc(hidden)]
    #[cfg(feature = "chrome")]
    pub fn browser_compatible_trusting_any(limits: Limits) -> Result<Self, NetError> {
        let mut live = Self::new(limits);
        live.chrome = Some(crate::chrome_client::Chrome::insecure(
            crate::Platform::host(),
        )?);
        Ok(live)
    }

    /// Signs every request with this identity (Web Bot Auth). The key stays in this process.
    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// The identity requests are signed with.
    pub fn identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }

    /// The Web Bot Auth headers for a request to `url`, if this environment signs.
    fn signature(&self, url: &str) -> Option<[(String, String); 3]> {
        let id = self.identity.as_ref()?;
        let u = url::Url::parse(url).ok()?;
        // Over plain http an on-path attacker could lift the signature and replay it over https
        // while it is valid; only https requests carry one.
        if u.scheme() != "https" {
            return None;
        }
        let host = u.host_str()?;
        let authority = match u.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };
        let mut nonce = [0u8; 32];
        aws_lc_rs::rand::fill(&mut nonce).ok()?;
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs();
        Some(id.sign(&authority, created, &nonce))
    }

    /// Sends every request through this proxy (`http://`, `https://`; credentials in the URL).
    /// Behind a proxy, DNS happens at the proxy: pageon still refuses private hosts written in
    /// URLs, but cannot see what the proxy resolves names to.
    pub fn with_proxy(mut self, url: &str) -> Result<Self, NetError> {
        let p =
            reqwest::Proxy::all(url).map_err(|e| NetError::Network(format!("proxy {url}: {e}")))?;
        self.public = client(Some(&p), true);
        self.open = client(Some(&p), false);
        self.proxied = true;
        #[cfg(feature = "chrome")]
        if let Some(c) = &self.chrome {
            let platform = c.platform();
            self.chrome = Some(crate::chrome_client::Chrome::new(Some(url), platform)?);
        }
        Ok(self)
    }

    /// Runs a future on this environment's runtime (tests of the resolver).
    #[doc(hidden)]
    pub fn resolve_public(&self, host: &str) -> Result<Vec<SocketAddr>, String> {
        self.runtime.block_on(resolve_public(host))
    }
}

impl Default for Live {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

fn classify(e: &reqwest::Error, limits: Limits) -> NetError {
    if e.is_timeout() {
        return NetError::Timeout(limits.timeout);
    }
    let mut src: Option<&dyn std::error::Error> = Some(e);
    while let Some(s) = src {
        if let Some(NetError::Blocked(m)) = s.downcast_ref::<NetError>() {
            return NetError::Blocked(m.clone());
        }
        src = s.source();
    }
    let detail = format!("{e:?}");
    let msg = std::error::Error::source(e).map_or_else(|| e.to_string(), |s| s.to_string());
    if detail.contains("rustls") || detail.contains("certificate") || detail.contains("Tls") {
        NetError::Tls(msg)
    } else {
        NetError::Network(msg)
    }
}

impl Env for Live {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        if let Some(path) = req.url.strip_prefix("file://") {
            return read_file(path, self.limits);
        }
        if !req.allow_private
            && let Some(host) = crate::private_host_of(&req.url)
        {
            return Err(NetError::Blocked(format!(
                "{host} is on the private network"
            )));
        }
        let limits = self.limits;
        let signature = self.signature(&req.url);
        #[cfg(feature = "chrome")]
        if let Some(c) = &self.chrome {
            return self.runtime.block_on(async {
                tokio::time::timeout(limits.timeout, c.fetch(req, signature, limits))
                    .await
                    .unwrap_or(Err(NetError::Timeout(limits.timeout)))
            });
        }
        let client = if req.allow_private {
            &self.open
        } else {
            &self.public
        };
        let fut = async {
            let method = reqwest::Method::from_bytes(req.method.as_bytes())
                .map_err(|_| NetError::Network(format!("invalid method {}", req.method)))?;
            let mut rb = client.request(method, &req.url);
            for (k, v) in req.headers.iter().chain(signature.iter().flatten()) {
                rb = rb.header(k, v);
            }
            if !req.body.is_empty() {
                rb = rb.body(req.body.clone());
            }
            let resp = rb.send().await.map_err(|e| classify(&e, limits))?;
            let status = resp.status().as_u16();
            let headers = resp
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_ascii_lowercase(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect();
            let mut body = Vec::new();
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| classify(&e, limits))?;
                if (body.len() + chunk.len()) as u64 > limits.max_bytes {
                    return Err(NetError::TooLarge(limits.max_bytes));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Response {
                status,
                headers,
                body,
            })
        };
        self.runtime
            .block_on(async { tokio::time::timeout(limits.timeout, fut).await })
            .unwrap_or(Err(NetError::Timeout(limits.timeout)))
    }

    fn stream(&self, call: &crate::StreamCall) -> Result<crate::StreamReply, NetError> {
        let mut call = call.clone();
        if let crate::StreamCall::Open { req, .. } = &mut call {
            if self.proxied {
                return Err(NetError::Blocked(
                    "realtime connections do not go through --proxy yet".into(),
                ));
            }
            if let Some(sig) = self.signature(&req.url.replacen("wss://", "https://", 1)) {
                req.headers.extend(sig);
            }
        }
        self.streams.borrow_mut().call(&call, &event_stream)
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        Ok(SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64)
    }

    /// Randomness from the OS (pages derive `crypto.getRandomValues` from it).
    fn random_u64(&self) -> Result<u64, NetError> {
        let mut b = [0u8; 8];
        aws_lc_rs::rand::fill(&mut b)
            .map_err(|_| NetError::Io("no randomness from the operating system".into()))?;
        Ok(u64::from_le_bytes(b))
    }

    fn profile(&self) -> Profile {
        #[cfg(feature = "chrome")]
        if let Some(c) = &self.chrome {
            return Profile::BrowserCompatible(c.platform());
        }
        Profile::Agent
    }
}

fn read_file(path: &str, limits: Limits) -> Result<Response, NetError> {
    let path = percent_decode(path);
    let meta = std::fs::metadata(&path).map_err(|e| NetError::Io(format!("{path}: {e}")))?;
    if meta.len() > limits.max_bytes {
        return Err(NetError::TooLarge(limits.max_bytes));
    }
    let body = std::fs::read(&path).map_err(|e| NetError::Io(format!("{path}: {e}")))?;
    let lower = path.to_ascii_lowercase();
    let ct = if lower.ends_with(".html") || lower.ends_with(".htm") || lower.ends_with(".xhtml") {
        "text/html"
    } else if lower.ends_with(".md") || lower.ends_with(".markdown") {
        "text/markdown"
    } else if lower.ends_with(".txt") {
        "text/plain"
    } else if lower.ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    };
    Ok(Response {
        status: 200,
        headers: vec![("content-type".into(), ct.into())],
        body,
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Opens an event stream: the response head now, then body chunks from a thread with its own
/// runtime until the body ends or the receiver is gone.
fn event_stream(
    req: &Request,
    events: std::sync::mpsc::Sender<crate::StreamEvent>,
) -> Result<(u16, Vec<(String, String)>), NetError> {
    use crate::StreamEvent;
    if !req.allow_private
        && let Some(host) = crate::private_host_of(&req.url)
    {
        return Err(NetError::Blocked(format!(
            "{host} is on the private network"
        )));
    }
    let (head_tx, head_rx) = std::sync::mpsc::channel();
    let req = req.clone();
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            let _ = head_tx.send(Err(NetError::Network("no runtime".into())));
            return;
        };
        rt.block_on(async move {
            let c = client(None, !req.allow_private);
            let mut rb = c.get(&req.url);
            for (k, v) in &req.headers {
                rb = rb.header(k, v);
            }
            let limits = Limits::default();
            let resp = match tokio::time::timeout(crate::HANDSHAKE, rb.send()).await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    let _ = head_tx.send(Err(classify(&e, limits)));
                    return;
                }
                Err(_) => {
                    let _ = head_tx.send(Err(NetError::Timeout(crate::HANDSHAKE)));
                    return;
                }
            };
            let headers = resp
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_ascii_lowercase(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect();
            if head_tx.send(Ok((resp.status().as_u16(), headers))).is_err() {
                return;
            }
            let mut body = resp.bytes_stream();
            let mut total = 0usize;
            while let Some(chunk) = body.next().await {
                let event = match chunk {
                    Ok(b) => {
                        total += b.len();
                        StreamEvent::Binary(b.to_vec())
                    }
                    Err(_) => StreamEvent::Closed {
                        code: 1006,
                        reason: String::new(),
                        clean: false,
                    },
                };
                let end = matches!(event, StreamEvent::Closed { .. });
                if events.send(event).is_err() || end {
                    return;
                }
                // Event streams are long-lived; what a page buffers is bounded by the engine.
                let _ = total;
            }
            let _ = events.send(StreamEvent::Closed {
                code: 1000,
                reason: String::new(),
                clean: true,
            });
        });
    });
    head_rx
        .recv_timeout(crate::HANDSHAKE)
        .map_err(|_| NetError::Timeout(crate::HANDSHAKE))?
}
