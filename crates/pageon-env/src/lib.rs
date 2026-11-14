//! pageon's single seam for nondeterminism.
//!
//! Everything the engine asks of the outside world — network responses, the current time and
//! random numbers — goes through [`Env`]. [`Live`] talks to the world, [`Recording`] does the
//! same and keeps a log, and [`Replay`] serves only that log, byte-identically and offline.
//!
//! ```no_run
//! use pageon_env::{Env, Live, Recording, Replay, Request};
//!
//! let rec = Recording::new(Live::new(Default::default()));
//! let page = rec.fetch(&Request::get("https://example.com"))?;
//! rec.save("example.rec.json")?;
//!
//! let replay = Replay::load("example.rec.json")?;
//! assert_eq!(replay.fetch(&Request::get("https://example.com"))?.body, page.body);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod addr;
#[cfg(feature = "chrome")]
mod chrome_client;
mod identity;
mod live;
mod profile;
mod record;
mod replay;
mod steps;

use std::fmt;
use std::time::Duration;

pub use addr::{is_private_host, is_public, private_host_of};
pub use identity::Identity;

/// Writes `bytes` to `path` readable by the owner only (mode 0600 on Unix), for files that
/// hold session data: recordings, saved state, workflows. An existing file is narrowed to
/// 0600 before it is rewritten.
pub fn write_private(path: impl AsRef<std::path::Path>, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = path.as_ref();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        options.mode(0o600);
        if path.exists() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    options.open(path)?.write_all(bytes)
}
pub use live::Live;
pub use profile::{Navigator, Profile, UaData, brand_header, chrome, chrome_brands};
pub use record::{Exchange, Log, Recording};
pub use replay::Replay;
pub use steps::{Cursor, Divergence, Kind, Recorder, Step, Target, diff_lines, fnv64};

/// pageon's honest user agent.
pub fn user_agent() -> String {
    format!(
        "pageon/{} (automated agent; +https://github.com/hupe1980/pageon)",
        env!("CARGO_PKG_VERSION")
    )
}

/// A request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Request {
    /// Method (`GET`).
    pub method: String,
    /// Absolute URL (`http`, `https` or `file`).
    pub url: String,
    /// Extra headers.
    pub headers: Vec<(String, String)>,
    /// Body (empty for none).
    #[serde(with = "b64")]
    pub body: Vec<u8>,
    /// Whether the request may reach the private network (loopback, private, link-local, …).
    /// When false, [`Live`] refuses such hosts, both as URL literals and as DNS answers.
    pub allow_private: bool,
    /// Data labels the broker attached: `secret:<name>` for each vault secret the
    /// request carries. Never sent, never recorded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// The agent action that caused the request, as the engine reports it; `None`
    /// for requests a page makes by itself. Advisory: for guards, never sent or recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Cause>,
    /// The URL as guards and approvers see it: vault secrets it carries replaced by their
    /// handles. Set by the broker's secrets layer only (never deserialized, so a content
    /// process cannot choose what the guard sees).
    #[serde(skip)]
    pub shown_url: Option<String>,
}

/// The agent action behind a request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cause {
    /// The action text (`click e5`).
    pub action: String,
    /// The agent's view line of its target (`button e5 "Pay now"`), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// `target` is page-authored text (a view line), clipped: data, never instructions.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub target_untrusted: bool,
}

impl Request {
    /// A `GET` request that may not reach the private network.
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            method: "GET".into(),
            url: url.into(),
            headers: Vec::new(),
            body: Vec::new(),
            allow_private: false,
            labels: Vec::new(),
            cause: None,
            shown_url: None,
        }
    }
}

/// A response. Redirects are not followed: a 3xx response is returned as is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Response {
    /// HTTP status (200 for `file:` URLs).
    pub status: u16,
    /// Headers, names lower-cased, in arrival order.
    pub headers: Vec<(String, String)>,
    /// Body, decompressed.
    #[serde(with = "b64")]
    pub body: Vec<u8>,
}

impl Response {
    /// The first header with this (lower-case) name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Limits of live loading.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Total time for one exchange, including the body.
    pub timeout: Duration,
    /// Maximum body size in bytes.
    pub max_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_bytes: 50 * 1024 * 1024,
        }
    }
}

/// Network and replay errors.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NetError {
    /// The host could not be reached (DNS, connection refused, reset).
    Network(String),
    /// TLS failed (certificate, handshake).
    Tls(String),
    /// The exchange took longer than the timeout.
    Timeout(Duration),
    /// The body exceeded the size limit.
    TooLarge(u64),
    /// A `file:` URL could not be read.
    Io(String),
    /// The recording has no entry for this request (or no more clock or random values).
    ReplayMiss(String),
    /// Refused before connecting: the destination is on the private network, or the egress
    /// policy refused it.
    Blocked(String),
    /// No secret by this name, or not for this origin.
    NoSecret(String),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(m) => write!(f, "could not reach the server: {m}"),
            Self::Tls(m) => write!(f, "TLS failed: {m}"),
            Self::Timeout(d) => write!(f, "timed out after {}s", d.as_secs()),
            Self::TooLarge(n) => write!(f, "response larger than {n} bytes"),
            Self::Io(m) => write!(f, "could not read the file: {m}"),
            Self::ReplayMiss(what) => write!(f, "no recorded {what}"),
            Self::Blocked(what) => write!(f, "blocked: {what}"),
            Self::NoSecret(what) => write!(f, "no secret: {what}"),
        }
    }
}

impl std::error::Error for NetError {}

/// The single source of network responses, time and randomness.
pub trait Env {
    /// Performs one exchange (redirects are not followed).
    fn fetch(&self, req: &Request) -> Result<Response, NetError>;
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> Result<u64, NetError>;
    /// A random number.
    fn random_u64(&self) -> Result<u64, NetError>;
    /// The value of secret `name` for a page of `origin` (`https://shop.test`). Environments
    /// without a vault have no secrets.
    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        let _ = origin;
        Err(NetError::NoSecret(format!(
            "secret://{name} (no secrets are configured)"
        )))
    }

    /// The recording that keeps this session's steps, if any.
    fn recorder(&self) -> Option<&dyn Recorder> {
        None
    }

    /// Who this environment says it is: the network client, headers and script
    /// values all follow it.
    fn profile(&self) -> Profile {
        Profile::Agent
    }
}

/// Byte bodies as base64 in serialized requests and responses.
mod b64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(serde::de::Error::custom)
    }
}

impl<E: Env + ?Sized> Env for Box<E> {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        (**self).fetch(req)
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        (**self).now_ms()
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        (**self).random_u64()
    }

    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        (**self).secret(name, origin)
    }

    fn recorder(&self) -> Option<&dyn Recorder> {
        (**self).recorder()
    }

    fn profile(&self) -> Profile {
        (**self).profile()
    }
}

impl<E: Env + ?Sized> Env for &E {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        (**self).fetch(req)
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        (**self).now_ms()
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        (**self).random_u64()
    }

    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        (**self).secret(name, origin)
    }

    fn recorder(&self) -> Option<&dyn Recorder> {
        (**self).recorder()
    }

    fn profile(&self) -> Profile {
        (**self).profile()
    }
}
