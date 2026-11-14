//! Load errors: what happened, what to do next, and the CLI exit code.

use std::fmt;

use pageon_env::NetError;

/// Why a page could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LoadError {
    /// The input is not a URL or path pageon understands.
    InvalidUrl {
        /// What was given.
        input: String,
        /// Why it is invalid.
        reason: String,
    },
    /// The URL scheme is not supported.
    UnsupportedScheme(String),
    /// The server could not be reached.
    Network(String),
    /// TLS failed.
    Tls(String),
    /// The load took longer than the timeout.
    Timeout(u64),
    /// The response exceeded the size limit.
    TooLarge(u64),
    /// The response is not a web page.
    UnsupportedContentType(String),
    /// More than the allowed number of redirects.
    TooManyRedirects(Vec<String>),
    /// A redirect led back to a URL already visited.
    RedirectLoop(Vec<String>),
    /// Replay: the recording has no entry for a request.
    ReplayMiss(String),
    /// A local file could not be read.
    Io(String),
    /// The sandbox failed (the content process could not start, or broke).
    Sandbox(String),
    /// A local file was asked for by an agent-facing session started without `--allow-files`.
    LocalFilesOff(String),
    /// Refused by the egress policy: a private-network address, or mixed content.
    Blocked {
        /// The refused URL.
        url: String,
        /// Why.
        reason: String,
    },
}

impl LoadError {
    /// What to do next.
    pub fn guidance(&self) -> String {
        match self {
            Self::InvalidUrl { .. } => {
                "pass an absolute http(s) URL, a file:// URL or a path to a local file".into()
            }
            Self::UnsupportedScheme(_) => "pageon loads http:, https: and file: URLs".into(),
            Self::LocalFilesOff(_) => {
                "local files are off for agents; start pageon with --allow-files to allow them (LoadOptions::allow_files)".into()
            }
            Self::Network(_) => "check the address and your connection, then retry".into(),
            Self::Tls(_) => {
                "the server's certificate could not be verified; check the address (https vs http)"
                    .into()
            }
            Self::Timeout(_) => "retry, or allow more time with --timeout <seconds>".into(),
            Self::TooLarge(n) => format!("allow more with --max-bytes (currently {n})"),
            Self::UnsupportedContentType(_) => {
                "pageon loads HTML and plain text; open other files with a dedicated tool".into()
            }
            Self::TooManyRedirects(_) | Self::RedirectLoop(_) => {
                "open the last URL of the chain directly, or report the loop to the site".into()
            }
            Self::ReplayMiss(what) => match what.strip_prefix("response for GET ") {
                Some(url) => format!("re-record with `pageon fetch {url} --record <file>`"),
                None => "re-record the page; the recording is incomplete".into(),
            },
            Self::Io(_) => "check that the file exists and is readable".into(),
            Self::Blocked { reason, .. } if reason.contains("private network") => {
                "pageon refuses the private network by default; allow it with \
                 --private-network allow (LoadOptions::allow_private_network)"
                    .into()
            }
            Self::Blocked { reason, .. } if reason.starts_with("robots.txt") => {
                "the site asks agents not to open this page; if a person directs this visit, \
                 run with --robots ignore (LoadOptions::robots = false)"
                    .into()
            }
            Self::Blocked { reason, .. } if reason.starts_with("policy") => {
                "the task's origin policy refused it; add the origin (--origin / --read-origin) if the task needs it"
                    .into()
            }
            Self::Blocked { .. } => "load the http: page directly".into(),
            Self::Sandbox(_) => {
                "retry; if the host cannot sandbox pages, run with --no-sandbox".into()
            }
        }
    }

    /// CLI exit code: 3 for load failures, 5 for replay misses.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::ReplayMiss(_) => 5,
            _ => 3,
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl { input, reason } => {
                write!(f, "{input:?} is not a valid URL or path: {reason}")
            }
            Self::UnsupportedScheme(s) => write!(f, "the {s}: scheme is not supported"),
            Self::LocalFilesOff(u) => write!(f, "{u} is a local file, and local files are off"),
            Self::Network(m) => write!(f, "could not reach the server: {m}"),
            Self::Tls(m) => write!(f, "TLS failed: {m}"),
            Self::Timeout(s) => write!(f, "the page did not load within {s} s"),
            Self::TooLarge(n) => write!(f, "the response is larger than {n} bytes"),
            Self::UnsupportedContentType(m) => write!(f, "content type {m} is not a web page"),
            Self::TooManyRedirects(c) => write!(f, "more than 20 redirects: {}", c.join(" → ")),
            Self::RedirectLoop(c) => write!(f, "redirect loop: {}", c.join(" → ")),
            Self::ReplayMiss(what) => write!(f, "no recorded {what}"),
            Self::Io(m) => write!(f, "could not read the file: {m}"),
            Self::Blocked { url, reason } => write!(f, "blocked {url}: {reason}"),
            Self::Sandbox(m) => write!(f, "sandbox: {m}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<NetError> for LoadError {
    fn from(e: NetError) -> Self {
        match e {
            NetError::Network(m) => Self::Network(m),
            NetError::Tls(m) => Self::Tls(m),
            NetError::Timeout(d) => Self::Timeout(d.as_secs()),
            NetError::TooLarge(n) => Self::TooLarge(n),
            NetError::Io(m) => Self::Io(m),
            NetError::ReplayMiss(w) => Self::ReplayMiss(w),
            NetError::NoSecret(m) => Self::Network(format!("no secret: {m}")),
            NetError::Blocked(m) => Self::Blocked {
                url: String::new(),
                reason: m,
            },
        }
    }
}

impl From<pageon_net::FetchError> for LoadError {
    fn from(e: pageon_net::FetchError) -> Self {
        use pageon_net::FetchError as F;
        match e {
            F::Blocked { url, reason } => Self::Blocked {
                url,
                reason: reason.to_string(),
            },
            F::RedirectLoop(c) => Self::RedirectLoop(c),
            F::TooManyRedirects(c) => Self::TooManyRedirects(c),
            F::Redirect(m) | F::Cors(m) => Self::Network(m),
            F::Network(n) => n.into(),
        }
    }
}
