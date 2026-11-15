//! Human takeover: a live view of a tab for one person, at a loopback URL with
//! a random token. It shows the page and relays the person's clicks, wheel and keys as
//! trusted input; **Done** hands the tab back. pageon never solves CAPTCHAs: it hands the
//! page to a human.
//!
//! The server is synchronous: the call that opens it holds the tab until the person is done or
//! the time is up, so agent actions cannot interleave.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use crate::Action;

/// What a takeover drives: a tab's image and its input.
pub trait Driver {
    /// The current viewport as PNG.
    fn frame(&mut self) -> Result<Vec<u8>, String>;
    /// Trusted input: `ClickAt` or `Press`.
    fn input(&mut self, action: Action) -> Result<(), String>;
}

/// How a takeover ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    /// The person pressed Done.
    Done,
    /// Nobody pressed Done in time.
    TimedOut,
}

/// The result of a takeover.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Takeover {
    /// How it ended.
    pub ended: Ended,
    /// Inputs relayed to the page.
    pub inputs: usize,
}

/// Longest takeover.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);

/// 128 random bits as hex.
pub(crate) fn token() -> Result<String, String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| format!("no randomness for the token: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Serves the live view until Done or `timeout`. `ready` gets the URL as soon as it listens.
pub fn serve(
    reason: &str,
    timeout: Duration,
    driver: &mut dyn Driver,
    ready: &mut dyn FnMut(&str),
) -> Result<Takeover, String> {
    let live = Live::start(reason)?;
    ready(live.url());
    live.wait(timeout, driver)
}

/// A live view that listens but is not yet served: a person's browser waits until
/// [`Live::wait`] runs (for a client that first shows the URL to its user).
pub struct Live {
    listener: TcpListener,
    url: String,
    view: View,
}

impl Live {
    /// Listens on a free loopback port with a fresh token.
    pub fn start(reason: &str) -> Result<Self, String> {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("cannot listen: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let token = token()?;
        Ok(Self {
            listener,
            url: format!("http://127.0.0.1:{port}/t/{token}"),
            view: View {
                port,
                path: format!("/t/{token}"),
                reason: reason.to_string(),
                viewer: None,
                inputs: 0,
                done: false,
            },
        })
    }

    /// The URL for the person.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Serves the view until Done or `timeout`.
    // A person works in wall-clock time; the page's own clock (virtual, through `Env`) is not
    // involved.
    #[allow(clippy::disallowed_methods)]
    pub fn wait(mut self, timeout: Duration, driver: &mut dyn Driver) -> Result<Takeover, String> {
        let deadline = Instant::now() + timeout.min(MAX_TIMEOUT);
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    // Accepted sockets may inherit the listener's non-blocking mode (BSD, macOS):
                    // a request whose bytes have not arrived yet would fail at once.
                    let _ = stream.set_nonblocking(false);
                    // A broken request only affects its own connection.
                    let _ = self.view.handle(stream, driver);
                    if self.view.done {
                        return Ok(Takeover {
                            ended: Ended::Done,
                            inputs: self.view.inputs,
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Ok(Takeover {
                            ended: Ended::TimedOut,
                            inputs: self.view.inputs,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(format!("the live view failed: {e}")),
            }
        }
    }
}

struct View {
    port: u16,
    path: String,
    reason: String,
    /// The viewer's cookie value, once someone opened the page.
    viewer: Option<String>,
    inputs: usize,
    done: bool,
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            (k == name).then_some(v)
        })
    }
}

/// How long a request may take to arrive, and a response to be written: a slow client cannot
/// hold the view (it is served one connection at a time).
const IO_DEADLINE: Duration = Duration::from_secs(5);

/// A socket read under one deadline for the whole request, however the bytes trickle in.
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl std::io::Read for Deadline<'_> {
    #[allow(clippy::disallowed_methods)] // a socket deadline is real time
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the request took too long to arrive",
            ));
        }
        self.stream.set_read_timeout(Some(left))?;
        std::io::Read::read(&mut self.stream, buf)
    }
}

#[allow(clippy::disallowed_methods)] // a socket deadline is real time
fn read_request(stream: &TcpStream) -> Result<Request, String> {
    stream
        .set_write_timeout(Some(IO_DEADLINE))
        .map_err(|e| e.to_string())?;
    let deadline = Deadline {
        stream,
        until: Instant::now() + IO_DEADLINE,
    };
    let mut r = BufReader::new(deadline.take(64 * 1024));
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut parts = line.split_whitespace();
    let (method, path) = (
        parts.next().unwrap_or_default().to_string(),
        parts.next().unwrap_or_default().to_string(),
    );
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).map_err(|e| e.to_string())? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len: usize = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0)
        .min(16 * 1024);
    let mut body = vec![0; len];
    r.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn respond(mut s: &TcpStream, status: &str, ctype: &str, extra: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
    let _ = s.flush();
}

fn plain(s: &TcpStream, status: &str, msg: &str) {
    respond(s, status, "text/plain; charset=utf-8", "", msg.as_bytes());
}

impl View {
    fn handle(&mut self, stream: TcpStream, driver: &mut dyn Driver) -> Result<(), String> {
        let req = read_request(&stream)?;
        // Only this loopback server, by name: no DNS rebinding.
        let host_ok = req.header("host").is_some_and(|h| {
            h == format!("127.0.0.1:{}", self.port) || h == format!("localhost:{}", self.port)
        });
        if !host_ok {
            plain(&stream, "400 Bad Request", "wrong Host");
            return Ok(());
        }
        // Only the live view's own page may post (no cross-site requests).
        if let Some(o) = req.header("origin")
            && o != format!("http://127.0.0.1:{}", self.port)
            && o != format!("http://localhost:{}", self.port)
        {
            plain(&stream, "403 Forbidden", "foreign Origin");
            return Ok(());
        }
        let Some(rest) = req
            .path
            .split('?')
            .next()
            .and_then(|p| p.strip_prefix(self.path.as_str()))
        else {
            plain(&stream, "404 Not Found", "no such takeover");
            return Ok(());
        };
        let rest = rest.to_string();
        let is_viewer = self
            .viewer
            .as_deref()
            .is_some_and(|v| req.cookie("pageon_viewer") == Some(v));
        match (req.method.as_str(), rest.as_str()) {
            ("GET", "" | "/") => {
                if self.viewer.is_some() && !is_viewer {
                    plain(&stream, "409 Conflict", "someone else is using this tab");
                    return Ok(());
                }
                let mut extra = String::new();
                if self.viewer.is_none() {
                    let v = token()?;
                    extra = format!(
                        "Set-Cookie: pageon_viewer={v}; Path={}; HttpOnly; SameSite=Strict\r\n",
                        self.path
                    );
                    self.viewer = Some(v);
                }
                extra.push_str(
                    "Content-Security-Policy: default-src 'none'; img-src 'self'; connect-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'\r\n",
                );
                respond(
                    &stream,
                    "200 OK",
                    "text/html; charset=utf-8",
                    &extra,
                    page(&self.path, &self.reason).as_bytes(),
                );
            }
            _ if !is_viewer => {
                plain(&stream, "409 Conflict", "open the live view page first");
            }
            ("GET", "/frame.png") => match driver.frame() {
                Ok(png) => respond(&stream, "200 OK", "image/png", "", &png),
                Err(e) => plain(&stream, "500 Internal Server Error", &e),
            },
            ("POST", "/input") => {
                let v: serde_json::Value =
                    serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
                let actions: Vec<Action> = if let Some([x, y]) = v
                    .get("click")
                    .and_then(serde_json::Value::as_array)
                    .map(Vec::as_slice)
                {
                    vec![Action::ClickAt(
                        x.as_f64().unwrap_or(0.0),
                        y.as_f64().unwrap_or(0.0),
                    )]
                } else if let Some([dx, dy]) = v
                    .get("wheel")
                    .and_then(serde_json::Value::as_array)
                    .map(Vec::as_slice)
                {
                    vec![Action::Scroll {
                        target: None,
                        by: crate::ScrollBy::Pixels(
                            dx.as_f64().filter(|v| v.is_finite()).unwrap_or(0.0),
                            dy.as_f64().filter(|v| v.is_finite()).unwrap_or(0.0),
                        ),
                        point: None,
                    }]
                } else if let Some(k) = v.get("key").and_then(serde_json::Value::as_str) {
                    vec![Action::Press(k.to_string())]
                } else if let Some(t) = v.get("text").and_then(serde_json::Value::as_str) {
                    t.chars().map(|c| Action::Press(c.to_string())).collect()
                } else {
                    plain(
                        &stream,
                        "400 Bad Request",
                        "send {click:[x,y]}, {wheel:[dx,dy]}, {key} or {text}",
                    );
                    return Ok(());
                };
                for a in actions {
                    if let Err(e) = driver.input(a) {
                        plain(&stream, "422 Unprocessable Entity", &e);
                        return Ok(());
                    }
                    self.inputs += 1;
                }
                respond(&stream, "204 No Content", "text/plain", "", b"");
            }
            ("POST", "/done") => {
                self.done = true;
                respond(&stream, "204 No Content", "text/plain", "", b"");
            }
            _ => plain(&stream, "404 Not Found", "no such route"),
        }
        Ok(())
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The live view: the tab's image, input relay and a Done button.
fn page(path: &str, reason: &str) -> String {
    format!(
        r#"<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content="width=device-width">
<title>pageon: your turn</title>
<style>
body{{margin:0;font:14px system-ui,sans-serif;background:#1d1f21;color:#eee}}
header{{display:flex;gap:12px;align-items:center;padding:8px 12px;position:sticky;top:0;background:#1d1f21}}
#reason{{flex:1}} #status{{color:#f99}}
button{{font:inherit;padding:6px 16px;border:0;border-radius:4px;background:#3b82f6;color:#fff;cursor:pointer}}
img{{display:block;max-width:100%;cursor:crosshair;outline:none}} img:focus{{box-shadow:0 0 0 2px #3b82f6}}
</style>
<header><b>pageon</b><span id=reason>The agent asks for your help. Its words, unverified: “{reason}”</span><span id=status></span><button id=done>Done</button></header>
<img id=screen tabindex=0 alt="The page" src="{path}/frame.png">
<script>
const base = {path:?}, img = document.getElementById('screen'), status = document.getElementById('status');
let busy = false;
const refresh = () => {{ img.src = base + '/frame.png?' + Date.now(); }};
async function send(body) {{
  busy = true;
  try {{
    const r = await fetch(base + '/input', {{method: 'POST', headers: {{'content-type': 'application/json'}}, body: JSON.stringify(body)}});
    status.textContent = r.ok ? '' : await r.text();
  }} finally {{ busy = false; refresh(); }}
}}
img.addEventListener('click', (e) => {{
  img.focus();
  const s = img.naturalWidth / img.clientWidth;
  send({{click: [Math.round(e.offsetX * s), Math.round(e.offsetY * s)]}});
}});
let wheel = [0, 0];
img.addEventListener('wheel', (e) => {{
  e.preventDefault();
  wheel = [wheel[0] + e.deltaX, wheel[1] + e.deltaY];
  if (busy) return;
  const d = wheel; wheel = [0, 0];
  send({{wheel: d}});
}}, {{passive: false}});
img.addEventListener('keydown', (e) => {{
  if (e.metaKey || e.ctrlKey || e.altKey) return;
  e.preventDefault();
  send(e.key.length === 1 ? {{text: e.key}} : {{key: e.key}});
}});
document.getElementById('done').onclick = async () => {{
  await fetch(base + '/done', {{method: 'POST'}});
  document.body.innerHTML = '<header>Handed back to the agent. You can close this tab.</header>';
}};
setInterval(() => {{ if (!busy) refresh(); }}, 500);
img.focus();
</script>
"#,
        // The agent's words may carry page text: clipped, and shown as theirs.
        reason = escape(&pageon_agent::quote::clip(reason, 200))
    )
}
