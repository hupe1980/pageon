//! Replay: serves only what a recording contains; never touches the network.

use std::cell::{Cell, RefCell};
use std::path::Path;

use crate::{Env, Exchange, Log, NetError, Request, Response};

/// Serves a recording: exchanges matched by method and URL in recorded order, clock and
/// random values in order. Anything not recorded is a [`NetError::ReplayMiss`].
pub struct Replay {
    log: Log,
    used: RefCell<Vec<bool>>,
    clock: Cell<usize>,
    random: Cell<usize>,
    realtime: RefCell<crate::realtime::RtServe>,
}

/// Errors loading a recording.
#[derive(Debug)]
pub enum LoadError {
    /// The file could not be read.
    Io(std::io::Error),
    /// The file is not a valid recording.
    Format(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read the recording: {e}"),
            Self::Format(e) => write!(f, "not a pageon recording: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl Replay {
    /// Replays a log.
    pub fn new(log: Log) -> Self {
        let n = log.exchanges.len();
        Self {
            log,
            used: RefCell::new(vec![false; n]),
            clock: Cell::new(0),
            random: Cell::new(0),
            realtime: RefCell::default(),
        }
    }

    /// Loads a recording file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, LoadError> {
        let text = std::fs::read_to_string(path).map_err(LoadError::Io)?;
        let log: Log = serde_json::from_str(&text).map_err(|e| LoadError::Format(e.to_string()))?;
        if log.version != 1 {
            return Err(LoadError::Format(format!(
                "version {} (expected 1)",
                log.version
            )));
        }
        Ok(Self::new(log))
    }

    /// The recorded exchanges.
    pub fn exchanges(&self) -> &[Exchange] {
        &self.log.exchanges
    }
}

impl Env for Replay {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        let mut used = self.used.borrow_mut();
        let i = self
            .log
            .exchanges
            .iter()
            .enumerate()
            .position(|(i, e)| {
                !used[i] && e.method == req.method && crate::redact::same_url(&e.url, &req.url)
            })
            .ok_or_else(|| {
                NetError::ReplayMiss(format!("response for {} {}", req.method, req.url))
            })?;
        used[i] = true;
        self.log.exchanges[i].response()
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        let i = self.clock.get();
        let v = *self
            .log
            .clock
            .get(i)
            .ok_or_else(|| NetError::ReplayMiss("clock reading".into()))?;
        self.clock.set(i + 1);
        Ok(v)
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        let i = self.random.get();
        let v = *self
            .log
            .random
            .get(i)
            .ok_or_else(|| NetError::ReplayMiss("random value".into()))?;
        self.random.set(i + 1);
        Ok(v)
    }

    fn stream(&self, call: &crate::StreamCall) -> Result<crate::StreamReply, NetError> {
        self.realtime.borrow_mut().call(&self.log.realtime, call)
    }

    fn profile(&self) -> crate::Profile {
        self.log.profile
    }
}
