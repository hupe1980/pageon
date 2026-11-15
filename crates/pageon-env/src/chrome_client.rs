//! The browser-compatible network client (feature `chrome`): wreq over BoringSSL,
//! with wreq-util's Chrome 149 TLS and HTTP/2 configuration for the profile's platform. Headers are pageon's own, from
//! [`Profile::BrowserCompatible`](crate::Profile) (wreq-util's default headers are off), so
//! one definition feeds every layer.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)]

use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::StreamExt;

use crate::{Limits, NetError, Request, Response, chrome as pin};

/// Two clients, as in [`crate::Live`]: one whose resolver drops non-public addresses.
pub(crate) struct Chrome {
    public: wreq::Client,
    open: wreq::Client,
    platform: crate::Platform,
}

struct PublicOnly;

impl wreq::dns::Resolve for PublicOnly {
    fn resolve(&self, name: wreq::dns::Name) -> wreq::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = crate::live::resolve_public(&host).await.map_err(|e| {
                Box::new(NetError::Blocked(e)) as Box<dyn std::error::Error + Send + Sync>
            })?;
            Ok(Box::new(addrs.into_iter()) as wreq::dns::Addrs)
        })
    }
}

fn client(
    proxy: Option<&str>,
    public_only: bool,
    verify: bool,
    platform: crate::Platform,
) -> Result<wreq::Client, NetError> {
    let emulation = wreq_util::Emulation::builder()
        .profile(wreq_util::Profile::Chrome149)
        .platform(match platform {
            crate::Platform::Windows => wreq_util::Platform::Windows,
            crate::Platform::Macos => wreq_util::Platform::MacOS,
            crate::Platform::Linux => wreq_util::Platform::Linux,
        })
        .headers(false)
        .build();
    let mut b = wreq::Client::builder()
        .emulation(emulation)
        .user_agent(crate::Profile::BrowserCompatible(platform).user_agent())
        .redirect(wreq::redirect::Policy::none())
        .retry(wreq::retry::Policy::never())
        .tls_cert_verification(verify);
    if public_only {
        b = b.dns_resolver(Arc::new(PublicOnly));
    }
    if let Some(p) = proxy {
        let p = wreq::Proxy::all(p).map_err(|e| NetError::Network(format!("proxy {p}: {e}")))?;
        b = b.proxy(p);
    }
    b.build()
        .map_err(|e| NetError::Network(format!("the Chrome-compatible client: {e}")))
}

impl Chrome {
    pub(crate) fn new(proxy: Option<&str>, platform: crate::Platform) -> Result<Self, NetError> {
        Ok(Self {
            public: client(proxy, true, true, platform)?,
            open: client(proxy, false, true, platform)?,
            platform,
        })
    }

    /// Tests only: trust any certificate (a local TLS server with a test certificate).
    pub(crate) fn insecure(platform: crate::Platform) -> Result<Self, NetError> {
        Ok(Self {
            public: client(None, true, false, platform)?,
            open: client(None, false, false, platform)?,
            platform,
        })
    }

    /// The platform it presents.
    pub(crate) fn platform(&self) -> crate::Platform {
        self.platform
    }

    pub(crate) async fn fetch(
        &self,
        req: &Request,
        signature: Option<[(String, String); 3]>,
        limits: Limits,
    ) -> Result<Response, NetError> {
        let client = if req.allow_private {
            &self.open
        } else {
            &self.public
        };
        let method = wreq::Method::from_bytes(req.method.as_bytes())
            .map_err(|_| NetError::Network(format!("invalid method {}", req.method)))?;
        let mut rb = client.request(method, &req.url);
        if !req
            .headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
        {
            rb = rb.header("accept-encoding", pin::ACCEPT_ENCODING);
        }
        for (k, v) in req.headers.iter().chain(signature.iter().flatten()) {
            rb = rb.header(k.as_str(), v.as_str());
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
    }
}

fn classify(e: &wreq::Error, limits: Limits) -> NetError {
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
    if detail.contains("Ssl") || detail.contains("certificate") || detail.contains("Tls") {
        NetError::Tls(msg)
    } else {
        NetError::Network(msg)
    }
}
