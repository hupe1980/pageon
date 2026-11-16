//! MCP 2026-07-28 over a Unix socket, for the command line's named sessions: one connection
//! at a time, each a few request lines answered on the same stream (progress first, then the
//! reply). The socket is created 0600 in a directory the caller made private.
//!
//! `pageon/close` ends the session (after its reply); `pageon/status` lists its tabs. A
//! session idle for its idle time asks itself to close, so state and recordings are written
//! as on `close`. A request line must arrive within 5 s and under 4 MiB, so a stalled client
//! cannot hold the session.

use std::io::{BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Backend, MAX_LINE, Server, error, read_line_bounded, send};

/// The socket transport's options.
#[derive(Debug, Clone)]
pub struct SocketOptions {
    /// Where to bind (its directory must exist and be private).
    pub path: PathBuf,
    /// Close after this long without a request (`None`: never).
    pub idle: Option<Duration>,
}

/// How long a request line may take to arrive.
const DEADLINE: Duration = Duration::from_secs(5);

/// Removes the socket when the session ends, however it ends.
struct Bound(PathBuf);

impl Drop for Bound {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Binds the socket and serves until `pageon/close` or the idle time.
#[allow(clippy::disallowed_methods)] // idle time is real time
pub(super) fn serve(
    b: &mut dyn Backend,
    options: &SocketOptions,
    otlp: Option<crate::otlp::Exporter>,
) -> Result<(), String> {
    let path = &options.path;
    let listener = UnixListener::bind(path)
        .map_err(|e| format!("cannot listen on {}: {e}", path.display()))?;
    let _bound = Bound(path.clone());
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let start = Instant::now();
    let last = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    if let Some(idle) = options.idle {
        watch(path.clone(), idle, start, last.clone(), done.clone());
    }
    let mut server = Server::new(true, otlp);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let close = connection(b, &mut server, &stream);
        last.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
        if close {
            break;
        }
    }
    done.store(true, Ordering::Relaxed);
    Ok(())
}

/// Asks the session to close once it has been idle for `idle` (checked twice a minute, or
/// more often for short idle times).
#[allow(clippy::disallowed_methods)] // idle time is real time
fn watch(
    path: PathBuf,
    idle: Duration,
    start: Instant,
    last: Arc<AtomicU64>,
    done: Arc<AtomicBool>,
) {
    let tick = (idle / 4).clamp(Duration::from_millis(50), Duration::from_secs(30));
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(tick);
            if done.load(Ordering::Relaxed) {
                return;
            }
            let since = Duration::from_millis(last.load(Ordering::Relaxed));
            if start.elapsed().saturating_sub(since) >= idle {
                let _ = request(
                    &path,
                    &json!({"jsonrpc": "2.0", "id": 0, "method": "pageon/close"}),
                );
                return;
            }
        }
    });
}

/// Sends one request and drops the reply (the watchdog's close).
fn request(path: &Path, msg: &Value) -> std::io::Result<()> {
    let mut s = UnixStream::connect(path)?;
    let mut line = serde_json::to_vec(msg).unwrap_or_default();
    line.push(b'\n');
    s.write_all(&line)?;
    s.shutdown(std::net::Shutdown::Write)
}

/// Serves one connection's request lines; true when the session should close.
fn connection(b: &mut dyn Backend, server: &mut Server, stream: &UnixStream) -> bool {
    let mut input = BufReader::new(stream);
    let mut out = stream;
    let mut line = String::new();
    loop {
        // The deadline is per line; a running tool call (a takeover) may take as long as it
        // needs.
        let _ = stream.set_read_timeout(Some(DEADLINE));
        line.clear();
        match read_line_bounded(&mut input, &mut line, MAX_LINE) {
            None => return false,
            Some(true) => {}
            Some(false) => {
                send(
                    &mut out,
                    &error(Value::Null, -32600, "request line over 4 MiB"),
                );
                return false;
            }
        }
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(
                    &mut out,
                    &error(Value::Null, -32700, &format!("parse error: {e}")),
                );
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        match msg.get("method").and_then(Value::as_str) {
            Some("pageon/close") => {
                send(&mut out, &json!({"jsonrpc": "2.0", "id": id, "result": {}}));
                return true;
            }
            Some("pageon/status") => {
                let call = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
                    "name": "browser_tabs", "arguments": {"action": "list"},
                    "_meta": {super::META_VERSION: super::MODERN, super::META_CAPS: {}}}});
                if let Some(r) = server.handle(b, &call, &mut |_| {}) {
                    send(&mut out, &r);
                }
            }
            _ => {
                let reply = server.handle(b, &msg, &mut |n| send(&mut out, &n));
                if let Some(r) = reply {
                    send(&mut out, &r);
                }
            }
        }
    }
}
