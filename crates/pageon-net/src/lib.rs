//! pageon's browser networking, over [`pageon_env::Env`].
//!
//! One place applies the browser's rules to every request a page makes (document, stylesheet,
//! script, frame, `fetch`, XHR):
//! - the egress policy: the private network is refused unless allowed; mixed content is
//!   blocked;
//! - honest request headers (`Accept`, `Origin`, `Referer`, `Sec-Fetch-*`, `Cookie`);
//! - redirects, CORS (with preflight) and response tainting, per the Fetch standard;
//! - an RFC 6265bis cookie jar ([`cookie`]) and Web Storage ([`storage`]);
//! - a request log, so callers see what a page fetched and what was refused.
//!
//! It is deterministic: time is passed in, and the only I/O is the `Env` argument.
//!
//! ```
//! use pageon_env::{Exchange, Log, Replay};
//! use pageon_net::{Client, Dest, FetchRequest, Net};
//!
//! let mut log = Log::default();
//! log.exchanges.push(Exchange {
//!     method: "GET".into(),
//!     url: "https://a.test/".into(),
//!     status: 200,
//!     headers: vec![("set-cookie".into(), "sid=1; Secure".into())],
//!     body: Some("hi".into()),
//!     ..Exchange::default()
//! });
//! let mut net = Net::default();
//! let req = FetchRequest::navigate(url::Url::parse("https://a.test/").unwrap());
//! let res = net.fetch(&Replay::new(log), req, 0).unwrap();
//! assert_eq!(res.body, b"hi");
//! assert_eq!(net.cookies(0)[0].name, "sid");
//! ```

mod cache;
pub mod cookie;
mod fetch;
pub mod refusal;
pub mod robots;
pub mod site;
pub mod state;
pub mod storage;

use std::collections::HashMap;
use std::fmt;

pub use cookie::{Cookie, SameSite};
pub use refusal::{Refusal, RefusalKind};
pub use robots::Robots;
pub use state::{Item, OriginState, StorageState};
pub use storage::{QuotaExceeded, Storage};
use url::Url;

/// What a request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Dest {
    /// A top-level document.
    Document,
    /// A frame's document.
    Frame,
    /// A classic script.
    Script,
    /// A stylesheet.
    Style,
    /// An image (fetched when a tab is first painted).
    Image,
    /// `fetch()`.
    Fetch,
    /// `XMLHttpRequest`.
    Xhr,
    /// The site's robots.txt, fetched before its first page.
    Robots,
    /// A WebSocket's handshake (`GET`) and frames (`SEND`).
    WebSocket,
    /// An `EventSource`'s stream.
    EventSource,
}

impl Dest {
    /// The name in the request log.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Frame => "frame",
            Self::Script => "script",
            Self::Style => "style",
            Self::Image => "image",
            Self::Fetch => "fetch",
            Self::Xhr => "xhr",
            Self::Robots => "robots",
            Self::WebSocket => "websocket",
            Self::EventSource => "eventsource",
        }
    }
}

/// Request mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A navigation.
    Navigate,
    /// Same-origin only.
    SameOrigin,
    /// CORS.
    Cors,
    /// No CORS: cross-origin responses are opaque to script.
    NoCors,
}

/// Credentials mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credentials {
    /// Never send or store cookies.
    Omit,
    /// Only for same-origin requests (the `fetch` default).
    SameOrigin,
    /// Always.
    Include,
}

/// Redirect mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redirect {
    /// Follow (up to `max_redirects`).
    Follow,
    /// A redirect is a network error.
    Error,
    /// Return an opaque-redirect response.
    Manual,
}

/// The page that makes a request.
#[derive(Debug, Clone)]
pub struct Client {
    /// The requesting document's URL (origin, referrer, mixed content).
    pub url: Url,
    /// The top-level site (`https://example.com`), for `SameSite` and partitioned cookies.
    pub top_site: String,
    /// Whether every frame from the top down to this document is same-site with the top.
    pub same_site_ancestors: bool,
    /// The top-level document's origin.
    pub top_origin: String,
    /// Whether the "tools" permissions-policy feature (WebMCP) is allowed here: the
    /// top-level document and same-origin frames; cross-origin frames only when delegated
    /// with `<iframe allow="tools">`.
    pub tools: bool,
}

impl Client {
    /// The top-level document at `url`.
    pub fn top(url: &Url) -> Self {
        Self {
            url: url.clone(),
            top_site: site::site(url),
            same_site_ancestors: true,
            top_origin: url.origin().ascii_serialization(),
            tools: true,
        }
    }

    /// A frame at `url` inside this document; `delegated` when its `<iframe>` allows "tools".
    pub fn frame(&self, url: &Url) -> Self {
        Self {
            url: url.clone(),
            top_site: self.top_site.clone(),
            same_site_ancestors: self.same_site_ancestors && site::site(url) == self.top_site,
            top_origin: self.top_origin.clone(),
            tools: self.tools && url.origin() == self.url.origin(),
        }
    }

    /// Whether this document's origin differs from the top-level document's.
    pub fn third_party(&self) -> bool {
        self.url.origin().ascii_serialization() != self.top_origin
    }
}

/// A request.
#[derive(Debug, Clone)]
pub struct FetchRequest {
    /// Method (upper-case for the standard ones).
    pub method: String,
    /// URL.
    pub url: Url,
    /// Headers set by the page (forbidden names are dropped).
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: Vec<u8>,
    /// Destination.
    pub dest: Dest,
    /// Mode.
    pub mode: Mode,
    /// Credentials mode.
    pub credentials: Credentials,
    /// Redirect mode.
    pub redirect: Redirect,
    /// Maximum redirects followed.
    pub max_redirects: usize,
    /// The requesting page; `None` for a load the caller asked for.
    pub client: Option<Client>,
}

impl FetchRequest {
    /// A top-level navigation the caller asked for.
    pub fn navigate(url: Url) -> Self {
        Self {
            method: "GET".into(),
            url,
            headers: Vec::new(),
            body: Vec::new(),
            dest: Dest::Document,
            mode: Mode::Navigate,
            credentials: Credentials::Include,
            redirect: Redirect::Follow,
            max_redirects: 20,
            client: None,
        }
    }

    /// A subresource of `client` (script, stylesheet, frame): `GET`, no CORS, with cookies.
    pub fn subresource(url: Url, dest: Dest, client: Client) -> Self {
        Self {
            mode: if dest == Dest::Frame {
                Mode::Navigate
            } else {
                Mode::NoCors
            },
            dest,
            client: Some(client),
            ..Self::navigate(url)
        }
    }
}

/// Response type, as script sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Same-origin.
    Basic,
    /// Cross-origin, allowed by CORS.
    Cors,
    /// Cross-origin `no-cors`: no status, headers or body.
    Opaque,
    /// `redirect: "manual"`.
    OpaqueRedirect,
}

impl Kind {
    /// `Response.type`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Cors => "cors",
            Self::Opaque => "opaque",
            Self::OpaqueRedirect => "opaqueredirect",
        }
    }
}

/// A response.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// Type.
    pub kind: Kind,
    /// Status (0 for opaque responses).
    pub status: u16,
    /// Status text (`""` for opaque responses).
    pub status_text: String,
    /// Headers visible for `kind` (never `Set-Cookie`).
    pub headers: Vec<(String, String)>,
    /// Body (empty for opaque responses).
    pub body: Vec<u8>,
    /// Final URL.
    pub url: Url,
    /// Whether a redirect was followed.
    pub redirected: bool,
    /// Every URL visited, the first included.
    pub chain: Vec<String>,
}

impl Fetched {
    /// The first header with this (lower-case) name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Why a request was refused before it was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// The host is on the private network (`private network address 10.0.0.5`, or what DNS
    /// answered).
    PrivateNetwork(String),
    /// An `http:` subresource of an `https:` page.
    MixedContent,
    /// The site's robots.txt disallows the page for pageon (`robots.txt disallows /x (rule
    /// "Disallow: /x")`).
    Robots(String),
    /// A `file:` URL requested by a web page, or reached by a redirect from the web.
    LocalFile,
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PrivateNetwork(why) => f.write_str(why),
            Self::MixedContent => f.write_str("mixed content (http from an https page)"),
            Self::Robots(why) => f.write_str(why),
            Self::LocalFile => {
                f.write_str("file URL from a web page (only local pages may load local files)")
            }
        }
    }
}

/// A failed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchError {
    /// Refused by the egress policy.
    Blocked {
        /// The refused URL.
        url: String,
        /// Why.
        reason: Block,
    },
    /// CORS refused the response (or the preflight).
    Cors(String),
    /// The same URL came back in a redirect chain.
    RedirectLoop(Vec<String>),
    /// More redirects than allowed.
    TooManyRedirects(Vec<String>),
    /// Any other redirect failure (`redirect: "error"`, a bad `Location`).
    Redirect(String),
    /// The transport failed.
    Network(pageon_env::NetError),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blocked { url, reason } => write!(f, "blocked {url}: {reason}"),
            Self::Cors(m) => write!(f, "CORS: {m}"),
            Self::RedirectLoop(c) => write!(f, "redirect loop: {}", c.join(" → ")),
            Self::TooManyRedirects(c) => write!(f, "too many redirects ({})", c.len() - 1),
            Self::Redirect(m) => write!(f, "redirect: {m}"),
            Self::Network(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// How a logged request ended.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Outcome {
    /// A response arrived (any status).
    Ok,
    /// Refused by the egress policy.
    Blocked(String),
    /// Refused by CORS.
    Cors(String),
    /// Failed (network, redirects).
    Failed(String),
}

/// One request a load made.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequestEntry {
    /// Method.
    pub method: String,
    /// URL requested.
    pub url: String,
    /// Destination.
    pub dest: Dest,
    /// Final status, if a response arrived.
    pub status: Option<u16>,
    /// Final URL, when redirected.
    pub final_url: Option<String>,
    /// Outcome.
    pub outcome: Outcome,
    /// `Set-Cookie` headers that were not stored: `name (reason)`.
    pub cookies_rejected: Vec<String>,
    /// Served from the session's HTTP cache (no request was sent).
    pub from_cache: bool,
}

impl fmt::Display for RequestEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = match (&self.outcome, self.status) {
            (Outcome::Ok, Some(s)) => s.to_string(),
            // A frame a page sent: no response of its own.
            (Outcome::Ok, None) if self.method == "SEND" => "sent".into(),
            (Outcome::Blocked(_), _) => "blocked".into(),
            (Outcome::Cors(_), _) => "cors-blocked".into(),
            _ => "failed".into(),
        };
        write!(
            f,
            "{} {status} {} {}",
            self.method,
            self.dest.as_str(),
            self.url
        )?;
        if let Some(to) = &self.final_url {
            write!(f, " → {to}")?;
        }
        if self.from_cache {
            f.write_str(" (cache)")?;
        }
        match &self.outcome {
            Outcome::Ok => {}
            Outcome::Blocked(r) | Outcome::Cors(r) | Outcome::Failed(r) => write!(f, " ({r})")?,
        }
        for c in &self.cookies_rejected {
            write!(f, "\n  cookie rejected: {c}")?;
        }
        Ok(())
    }
}

/// A session's network state: cookies, Web Storage, the egress policy and the request log.
#[derive(Debug, Clone, Default)]
pub struct Net {
    jar: cookie::Jar,
    pub(crate) cache: cache::Cache,
    local: HashMap<String, Storage>,
    session: HashMap<String, Storage>,
    /// Whether requests may reach the private network (default: no).
    pub allow_private: bool,
    /// Whether top-level documents honour robots.txt (`LoadOptions::robots`).
    pub robots: bool,
    /// The agent action being performed, copied into every request it causes.
    pub cause: Option<pageon_env::Cause>,
    /// Engine labels for the requests of the current action (`tool:NAME`, `consequential`
    /// for WebMCP tool calls).
    pub labels: Vec<String>,
    /// robots.txt per origin, fetched once per session.
    pub(crate) robots_txt: HashMap<String, robots::Robots>,
    log: Vec<RequestEntry>,
}

impl Net {
    /// Performs `req` through `env` at `now` (ms since the Unix epoch), and logs it.
    pub fn fetch(
        &mut self,
        env: &dyn pageon_env::Env,
        req: FetchRequest,
        now: u64,
    ) -> Result<Fetched, FetchError> {
        fetch::fetch(self, env, req, now)
    }

    /// The handshake request of a realtime connection a document opens: a WebSocket
    /// (`ws:`/`wss:`, cookies as for its `http(s):` URL, always with `Origin`) or an event
    /// stream (`http(s):`, cookies when same-origin or `with_credentials`). The egress checks
    /// of a fetch apply (private network, mixed content); a refusal is logged and returned.
    pub fn stream_request(
        &mut self,
        client: &Client,
        url: &Url,
        dest: Dest,
        with_credentials: bool,
        now: u64,
    ) -> Result<pageon_env::Request, FetchError> {
        fetch::stream_request(self, client, url, dest, with_credentials, now)
    }

    /// Logs a realtime connection's handshake or a frame (`SEND`, no status).
    pub fn log_stream(
        &mut self,
        method: &str,
        url: &str,
        dest: Dest,
        status: Option<u16>,
        outcome: Outcome,
    ) {
        self.log.push(RequestEntry {
            method: method.into(),
            url: url.into(),
            dest,
            status,
            final_url: None,
            outcome,
            cookies_rejected: Vec::new(),
            from_cache: false,
        });
    }

    /// The requests logged since the last call.
    pub fn take_log(&mut self) -> Vec<RequestEntry> {
        std::mem::take(&mut self.log)
    }

    /// The cookies still valid at `now`.
    pub fn cookies(&self, now: u64) -> Vec<Cookie> {
        self.jar.all(now)
    }

    /// Adds cookies as they are (an import), replacing cookies with the same name, domain and
    /// path.
    pub fn add_cookies(&mut self, cookies: &[Cookie]) {
        self.jar.add(cookies);
    }

    /// Removes every cookie.
    pub fn clear_cookies(&mut self) {
        self.jar.clear();
    }

    /// `document.cookie` for a document of `client` at `now`.
    pub fn document_cookie(&self, client: &Client, now: u64) -> String {
        self.jar.header(&script_cx(client), now)
    }

    /// `document.cookie = value` (ignored silently when rejected, as in browsers).
    pub fn set_document_cookie(&mut self, client: &Client, value: &str, now: u64) {
        let _ = self.jar.set(value, &script_cx(client), now);
    }

    /// The `localStorage` area of an origin.
    pub fn local_storage(&mut self, origin: &str) -> &mut Storage {
        self.local.entry(origin.to_string()).or_default()
    }

    /// The `sessionStorage` area of an origin.
    pub fn session_storage(&mut self, origin: &str) -> &mut Storage {
        self.session.entry(origin.to_string()).or_default()
    }
}

fn script_cx(client: &Client) -> cookie::Context<'_> {
    cookie::Context {
        url: &client.url,
        top_site: &client.top_site,
        same_site: client.same_site_ancestors && site::site(&client.url) == client.top_site,
        top_level_safe_nav: false,
        script: true,
    }
}
