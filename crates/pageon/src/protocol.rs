//! The broker ↔ content protocol (version 1): one JSON object per line over the content
//! process's standard input and output. The broker sends commands; while one runs, the content
//! process asks the broker for its `Env` (network, clock, randomness, secrets), then reports
//! the command's result.

use std::io::{BufRead, Write};

use pageon_env::{NetError, Request, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{Action, Dump, LoadOptions};

/// Protocol version.
pub(crate) const VERSION: u32 = 1;

/// Longest line accepted (a response body is base64 in one line).
const MAX_LINE: usize = 256 << 20;

/// A command for the content process.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub(crate) enum Op {
    Open {
        url: String,
        options: LoadOptions,
    },
    Act {
        tab: u32,
        action: Action,
        /// A person's input (takeover): not the agent's step.
        #[serde(default)]
        person: bool,
    },
    Observe {
        tab: u32,
        full: bool,
    },
    Read {
        tab: u32,
        read: crate::Read,
    },
    View {
        tab: u32,
        kind: Dump,
        width: u32,
        height: u32,
        budget: crate::Budget,
    },
    Status {
        tab: u32,
    },
    /// Paint the tab.
    Screenshot {
        tab: u32,
        options: crate::ScreenshotOptions,
    },
    /// Paint one element of the tab, with provenance.
    Crop {
        tab: u32,
        reference: String,
        margin: f32,
        format: crate::Format,
        scale: f32,
    },
    /// The tab's URL and title.
    Url {
        tab: u32,
    },
    Close {
        tab: u32,
    },
    /// Drop every tab and start a fresh session (a reset).
    Clear,
    GetState,
    SetState {
        state: pageon_net::StorageState,
    },
    Probe {
        home: String,
    },
    /// One CDP client message; the answer is the messages to send back.
    Cdp {
        message: String,
        options: LoadOptions,
    },
    /// The CDP client has been idle this long (real ms); the answer is the messages to send.
    CdpIdle {
        elapsed_ms: u64,
    },
    /// One WebDriver BiDi client message; the answer is the messages to send back.
    Bidi {
        message: String,
        options: LoadOptions,
    },
    /// The BiDi client has been idle this long (real ms); the answer is the messages to send.
    BidiIdle {
        elapsed_ms: u64,
    },
    /// Fork, run `actions` on the copy of `tab`, observe, and end the copy.
    Lookahead {
        tab: u32,
        actions: Vec<Action>,
    },
}

/// Broker → content.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToContent {
    Cmd {
        id: u64,
        op: Op,
    },
    Reply(EnvReply),
    /// Instead of a reply: a lookahead branch reached a request that is held. The
    /// branch ends at once.
    Stop,
}

/// What the content process asks of the broker's `Env`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EnvCall {
    Fetch(Request),
    Now,
    Random,
    Secret {
        name: String,
        origin: String,
    },
    /// A staged file for an upload.
    File {
        name: String,
        origin: String,
    },
    /// Whether the broker's environment keeps steps.
    Records,
    /// A step that just ended.
    Step(Box<pageon_env::Step>),
    /// The identity profile of the broker's environment.
    Profile,
    /// A realtime connection call (WebSocket, event stream).
    Stream(Box<pageon_env::StreamCall>),
}

/// The broker's answer.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EnvReply {
    Fetch(Result<Response, NetError>),
    Num(Result<u64, NetError>),
    Secret(Result<String, NetError>),
    File(Result<pageon_env::StagedFile, NetError>),
    Flag(bool),
    Profile(pageon_env::Profile),
    Stream(Result<pageon_env::StreamReply, NetError>),
}

/// Content → broker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FromContent {
    Hello {
        pageon: u32,
    },
    Env(EnvCall),
    Done {
        id: u64,
        value: serde_json::Value,
    },
    Failed {
        id: u64,
        message: String,
    },
    /// One way: an action of a lookahead branch finished (kept if the branch is stopped).
    Ran(Box<crate::Outcome>),
}

/// A broken exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProtocolError {
    /// The other side closed its end.
    Closed,
    /// I/O failed.
    Io(String),
    /// A line that is not a message of this protocol.
    Malformed(String),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => f.write_str("the other side closed the channel"),
            Self::Io(e) => write!(f, "channel I/O failed: {e}"),
            Self::Malformed(e) => write!(f, "malformed message: {e}"),
        }
    }
}

pub(crate) fn send<T: Serialize>(w: &mut impl Write, msg: &T) -> Result<(), ProtocolError> {
    let mut line = serde_json::to_vec(msg).map_err(|e| ProtocolError::Malformed(e.to_string()))?;
    line.push(b'\n');
    w.write_all(&line)
        .and_then(|()| w.flush())
        .map_err(|e| ProtocolError::Io(e.to_string()))
}

pub(crate) fn recv<T: DeserializeOwned>(r: &mut impl BufRead) -> Result<T, ProtocolError> {
    let mut line = Vec::new();
    let mut limited = std::io::Read::take(&mut *r, MAX_LINE as u64);
    let n = limited
        .read_until(b'\n', &mut line)
        .map_err(|e| ProtocolError::Io(e.to_string()))?;
    if n == 0 {
        return Err(ProtocolError::Closed);
    }
    if line.len() >= MAX_LINE && !line.ends_with(b"\n") {
        return Err(ProtocolError::Malformed("line too long".into()));
    }
    serde_json::from_slice(&line).map_err(|e| {
        ProtocolError::Malformed(format!(
            "{e} in {:?}",
            String::from_utf8_lossy(&line[..line.len().min(120)])
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// random lines (garbage, truncated and mutated messages) never panic the decoder.
    #[test]
    fn random_lines_never_panic() {
        let samples = [
            r#"{"hello":{"pageon":1}}"#,
            r#"{"env":{"fetch":{"method":"GET","url":"https://a.test/","headers":[],"body":"","allow_private":false}}}"#,
            r#"{"env":"now"}"#,
            r#"{"done":{"id":1,"value":{"Ok":[1,{"action":"x"}]}}}"#,
            r#"{"failed":{"id":2,"message":"x"}}"#,
            r#"{"cmd":{"id":1,"op":{"op":"open","url":"https://a.test/","options":{}}}}"#,
            r#"{"reply":{"fetch":{"Ok":{"status":200,"headers":[],"body":"aGk="}}}}"#,
        ];
        let mut x: u64 = 3;
        let mut next = move || {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (x >> 33) as usize
        };
        for _ in 0..10_000 {
            let base = samples[next() % samples.len()].as_bytes().to_vec();
            let mut line = base.clone();
            for _ in 0..next() % 6 {
                if line.is_empty() {
                    break;
                }
                let i = next() % line.len();
                match next() % 3 {
                    0 => line[i] = (next() % 256) as u8,
                    1 => line.truncate(i),
                    _ => {
                        let noise = b"{}[]\":,0a\\\x00";
                        line.insert(i, noise[next() % noise.len()]);
                    }
                }
            }
            line.retain(|&b| b != b'\n');
            line.push(b'\n');
            let _ = recv::<FromContent>(&mut &line[..]);
            let _ = recv::<ToContent>(&mut &line[..]);
        }
        assert_eq!(
            recv::<FromContent>(&mut &b""[..]).err(),
            Some(ProtocolError::Closed)
        );
    }
}
