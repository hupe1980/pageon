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
mod realtime;
mod record;
mod redact;
mod replay;
mod steps;

use std::fmt;
use std::time::Duration;

pub use addr::{is_private_host, is_public, private_host_of};
pub use identity::{DEFAULT_TAG, Identity, check_tag};
pub use realtime::{
    EVENTSOURCE, HANDSHAKE, MAX_MESSAGE, MAX_STREAMS, Rt, StreamCall, StreamEvent, StreamKind,
    StreamReply, WEBSOCKET,
};

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
pub use profile::{Navigator, Platform, Profile, UaData, brand_header, chrome, chrome_brands};
pub use record::{Exchange, Log, Recording};
pub use redact::{REDACTED, redact_params};
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
    /// The request carries cookies, in this context (stated by the engine, which knows frames
    /// and initiators): the broker adds the `HttpOnly` cookies it holds. `None`: no cookies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie_scope: Option<CookieScope>,
}

/// A file the operator staged for upload: its base name, type and bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StagedFile {
    /// The base name the page sees (`cv.pdf`).
    pub name: String,
    /// MIME type.
    pub content_type: String,
    /// Bytes.
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
}

/// The cookie context of a request (RFC 6265bis): which cookies may go along.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CookieScope {
    /// The top-level site (`https://example.com`).
    pub top_site: String,
    /// Same-site with the top-level site and every ancestor frame (`Strict`/`Lax` go along).
    pub same_site: bool,
    /// A top-level navigation with a safe method (`Lax` goes along).
    pub top_level_safe_nav: bool,
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
            cookie_scope: None,
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

    /// A passkey operation for a page (WebAuthn): environments without a passkey store have
    /// none. The answer carries public data only.
    fn passkey(&self, call: &PasskeyCall) -> Result<PasskeyReply, NetError> {
        let _ = call;
        Err(NetError::NoSecret(
            "no passkeys are configured (configure a store with --passkeys FILE)".into(),
        ))
    }

    /// Staged file `name` for a page of `origin` (an upload). Environments without staged
    /// files have none; a host path is never read.
    fn file(&self, name: &str, origin: &str) -> Result<StagedFile, NetError> {
        let _ = origin;
        Err(NetError::Blocked(format!(
            "file \"{name}\" is not staged (stage it with --file {name}@ORIGIN=PATH)"
        )))
    }

    /// Realtime connections (WebSocket, server-sent events). Environments without a network
    /// have none.
    fn stream(&self, call: &StreamCall) -> Result<StreamReply, NetError> {
        match call {
            StreamCall::Open { req, .. } => Err(NetError::Blocked(format!(
                "realtime connections are not available here ({})",
                req.url
            ))),
            StreamCall::Poll { .. } => Ok(StreamReply::Events(Vec::new())),
            _ => Ok(StreamReply::Done),
        }
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

/// Byte bodies as base64 in serialized requests and responses (`#[serde(with = …)]`).
pub mod b64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Bytes as a base64 string.
    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    /// A base64 string as bytes.
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

    fn passkey(&self, call: &PasskeyCall) -> Result<PasskeyReply, NetError> {
        (**self).passkey(call)
    }

    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        (**self).secret(name, origin)
    }

    fn file(&self, name: &str, origin: &str) -> Result<StagedFile, NetError> {
        (**self).file(name, origin)
    }

    fn stream(&self, call: &StreamCall) -> Result<StreamReply, NetError> {
        (**self).stream(call)
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

    fn passkey(&self, call: &PasskeyCall) -> Result<PasskeyReply, NetError> {
        (**self).passkey(call)
    }

    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        (**self).secret(name, origin)
    }

    fn file(&self, name: &str, origin: &str) -> Result<StagedFile, NetError> {
        (**self).file(name, origin)
    }

    fn stream(&self, call: &StreamCall) -> Result<StreamReply, NetError> {
        (**self).stream(call)
    }

    fn recorder(&self) -> Option<&dyn Recorder> {
        (**self).recorder()
    }

    fn profile(&self) -> Profile {
        (**self).profile()
    }
}

/// A passkey operation a page asks for (WebAuthn), as the browser forwards it to its
/// authenticator. Buffers are raw bytes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PasskeyCall {
    /// Whether a store exists (`isUserVerifyingPlatformAuthenticatorAvailable`).
    Available,
    /// `navigator.credentials.create()`: a new credential for `rp_id`.
    Create {
        /// The page's origin.
        origin: String,
        /// The relying party id the page asked for.
        rp_id: String,
        /// The user handle.
        user_id: Vec<u8>,
        /// The user's name, as the store lists it.
        user_name: String,
        /// Credentials that must not exist already.
        exclude: Vec<Vec<u8>>,
        /// SHA-256 of the client data.
        client_data_hash: Vec<u8>,
    },
    /// `navigator.credentials.get()`: an assertion over the client data.
    Get {
        /// The page's origin.
        origin: String,
        /// The relying party id.
        rp_id: String,
        /// Allowed credential ids (empty: any of the rp id's).
        allow: Vec<Vec<u8>>,
        /// SHA-256 of the client data.
        client_data_hash: Vec<u8>,
    },
}

/// What a passkey operation returns: public data only.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PasskeyReply {
    /// Whether a store exists.
    Available(bool),
    /// A new credential.
    Created {
        /// The credential id.
        id: Vec<u8>,
        /// The authenticator data (with the attested credential data).
        authenticator_data: Vec<u8>,
        /// The CBOR attestation object (`none`).
        attestation_object: Vec<u8>,
        /// The public key (SubjectPublicKeyInfo DER).
        public_key: Vec<u8>,
    },
    /// An assertion.
    Signed {
        /// The credential id.
        id: Vec<u8>,
        /// The authenticator data.
        authenticator_data: Vec<u8>,
        /// The ES256 signature (DER).
        signature: Vec<u8>,
        /// The user handle.
        user_handle: Vec<u8>,
    },
    /// The page's request was refused: a DOMException name and message.
    Refused {
        /// `NotAllowedError`, `SecurityError`, `InvalidStateError`, …
        name: String,
        /// Why.
        message: String,
    },
}
