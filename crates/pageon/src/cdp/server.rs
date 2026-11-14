//! The DevTools endpoint: Chrome's HTTP discovery (`/json/version`, `/json/list`) and one
//! WebSocket client, on loopback only: CDP at `/devtools/browser/<id>`, WebDriver BiDi at
//! `/session`.
//!
//! An acceptor thread answers discovery, refuses foreign `Host`s (DNS rebinding) and `Origin`s
//! (a web page in the user's browser), and refuses a second client; the one WebSocket
//! connection is served on the calling thread, where the engine lives. The server ends when
//! that client disconnects or sends `Browser.close`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use serde_json::{Value as Json, json};
use tungstenite::protocol::{Role, WebSocketConfig};
use tungstenite::{Message, WebSocket};

/// The browser target's id (fixed: sessions are deterministic).
pub fn browser_id() -> String {
    super::hex(0xB0_0000, 1)
}

/// The protocol a client speaks, chosen by the WebSocket path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    /// Chrome DevTools Protocol (`/devtools/browser/<id>`).
    Cdp,
    /// WebDriver BiDi (`/session`).
    Bidi,
}

/// The session behind the endpoint: a client's protocol and input → the messages to send.
pub type Handler<'a> = dyn FnMut(Proto, Input<'_>) -> Result<Vec<String>, String> + 'a;

/// What the session is given: a client message, or the time the client has been idle.
#[derive(Debug, Clone, Copy)]
pub enum Input<'a> {
    /// A client message.
    Message(&'a str),
    /// The client sent nothing for this many ms (real time): the page's clock may move.
    Idle(u64),
}

/// Real time without a client message after which the page's clock moves (a polling client
/// is waiting for the page; see `cdp::Session::idle`).
const IDLE: std::time::Duration = std::time::Duration::from_millis(50);

/// Endpoint options.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    /// Port on 127.0.0.1 (0: pick a free one).
    pub port: u16,
    /// Origins allowed to open the WebSocket (requests without `Origin` are always allowed).
    pub allow_origins: Vec<String>,
}

/// A bound endpoint.
pub struct Server {
    listener: TcpListener,
    port: u16,
    options: ServerOptions,
}

/// One parsed HTTP request head.
struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// How long a client may take to send its request head. Heads are read one connection at a
/// time, so an idle or trickling connection must not hold up the clients behind it.
const HEAD_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// A stream whose reads fail once a deadline has passed (a total, not a per-read, timeout).
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: std::time::Instant,
}

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        #[allow(clippy::disallowed_methods)] // a network deadline is real time
        let left = self
            .until
            .saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        (&mut &*self.stream).read(buf)
    }
}

fn read_head(stream: &TcpStream) -> Option<Head> {
    #[allow(clippy::disallowed_methods)] // a network deadline is real time
    let until = std::time::Instant::now() + HEAD_DEADLINE;
    let mut r = BufReader::new(Deadline { stream, until }.take(64 * 1024));
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Some(Head {
        method,
        path,
        headers,
    })
}

fn respond(mut stream: &TcpStream, status: &str, content_type: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

fn loopback_host(host: Option<&str>) -> bool {
    let Some(h) = host else { return false };
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']')
            .next()
            .map(|v6| format!("[{v6}]"))
            .unwrap_or_default()
    } else {
        h.rsplit_once(':').map_or(h, |(n, _)| n).to_string()
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "[::1]"
    )
}

impl Server {
    /// Binds 127.0.0.1:`port`.
    pub fn bind(options: ServerOptions) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", options.port))?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            port,
            options,
        })
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The browser WebSocket URL (`ws://127.0.0.1:<port>/devtools/browser/<id>`).
    pub fn ws_url(&self) -> String {
        format!(
            "ws://127.0.0.1:{}/devtools/browser/{}",
            self.port,
            browser_id()
        )
    }

    /// The WebDriver BiDi WebSocket URL (`ws://127.0.0.1:<port>/session`).
    pub fn bidi_url(&self) -> String {
        format!("ws://127.0.0.1:{}/session", self.port)
    }

    /// Serves one client: every message goes to `handle`, with the client's protocol, which
    /// returns the messages to send.
    pub fn serve(self, handle: &mut Handler<'_>) -> Result<(), String> {
        let pages: Arc<Mutex<Json>> = Arc::new(Mutex::new(json!([])));
        let busy = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<(TcpStream, Proto)>();
        refresh_pages(handle, &pages, self.port)?;
        {
            let (pages, busy) = (pages.clone(), busy.clone());
            let (port, allow) = (self.port, self.options.allow_origins.clone());
            let ws_url = self.ws_url();
            let listener = self.listener;
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let Some(head) = read_head(&stream) else {
                        continue;
                    };
                    if !loopback_host(head.header("host")) {
                        respond(
                            &stream,
                            "400 Bad Request",
                            "text/plain",
                            "Host header is not a loopback name (pageon only serves 127.0.0.1)\n",
                        );
                        continue;
                    }
                    let path = head
                        .path
                        .split('?')
                        .next()
                        .unwrap_or_default()
                        .trim_end_matches('/');
                    let upgrade = head
                        .header("upgrade")
                        .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
                    if !upgrade {
                        match (head.method.as_str(), path) {
                            ("GET", "/json/version") => {
                                let body = json!({
                                    "Browser": format!("pageon/{}", env!("CARGO_PKG_VERSION")),
                                    "Protocol-Version": "1.3",
                                    "User-Agent": pageon_env::user_agent(),
                                    "V8-Version": "",
                                    "WebKit-Version": "",
                                    "webSocketDebuggerUrl": ws_url,
                                });
                                respond(
                                    &stream,
                                    "200 OK",
                                    "application/json; charset=UTF-8",
                                    &body.to_string(),
                                );
                            }
                            ("GET", "/json" | "/json/list") => {
                                let body = pages
                                    .lock()
                                    .map(|p| p.to_string())
                                    .unwrap_or_else(|_| "[]".into());
                                respond(
                                    &stream,
                                    "200 OK",
                                    "application/json; charset=UTF-8",
                                    &body,
                                );
                            }
                            _ => respond(
                                &stream,
                                "404 Not Found",
                                "text/plain",
                                "pageon serves /json/version, /json/list and the browser WebSocket\n",
                            ),
                        }
                        continue;
                    }
                    if let Some(origin) = head.header("origin")
                        && !allow
                            .iter()
                            .any(|a| a == "*" || a.eq_ignore_ascii_case(origin))
                    {
                        respond(
                            &stream,
                            "403 Forbidden",
                            "text/plain",
                            &format!(
                                "Rejected an incoming WebSocket connection from the {origin} origin. Use `pageon cdp --allow-origin {origin}` to allow it.\n"
                            ),
                        );
                        continue;
                    }
                    let proto = if path == format!("/devtools/browser/{}", browser_id()) {
                        Proto::Cdp
                    } else if path == "/session" {
                        Proto::Bidi
                    } else {
                        respond(
                            &stream,
                            "404 Not Found",
                            "text/plain",
                            &format!(
                                "pageon serves CDP at ws://127.0.0.1:{port}/devtools/browser/{} and WebDriver BiDi at ws://127.0.0.1:{port}/session\n",
                                browser_id()
                            ),
                        );
                        continue;
                    };
                    if busy.swap(true, Ordering::SeqCst) {
                        respond(
                            &stream,
                            "409 Conflict",
                            "text/plain",
                            "pageon serves one client at a time; start another `pageon cdp`\n",
                        );
                        continue;
                    }
                    let Some(key) = head.header("sec-websocket-key") else {
                        busy.store(false, Ordering::SeqCst);
                        respond(
                            &stream,
                            "400 Bad Request",
                            "text/plain",
                            "missing Sec-WebSocket-Key\n",
                        );
                        continue;
                    };
                    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
                    let mut s = &stream;
                    let ok = write!(
                        s,
                        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                    )
                    .and_then(|()| s.flush());
                    if ok.is_err() || tx.send((stream, proto)).is_err() {
                        break;
                    }
                }
            });
        }
        let Ok((stream, proto)) = rx.recv() else {
            return Err("the endpoint stopped accepting connections".into());
        };
        let _ = stream.set_nodelay(true);
        let config = WebSocketConfig::default()
            .max_message_size(Some(256 << 20))
            .max_frame_size(Some(256 << 20));
        let _ = stream.set_read_timeout(Some(IDLE));
        let mut ws = WebSocket::from_raw_socket(stream, Role::Server, Some(config));
        #[allow(clippy::disallowed_methods)] // idle time is real time on purpose
        let mut last = std::time::Instant::now();
        loop {
            let msg = match ws.read() {
                Ok(Message::Text(t)) => t.to_string(),
                Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    let idle = u64::try_from(last.elapsed().as_millis()).unwrap_or(u64::MAX);
                    #[allow(clippy::disallowed_methods)]
                    {
                        last = std::time::Instant::now();
                    }
                    for out in handle(proto, Input::Idle(idle))? {
                        if ws.send(Message::text(out)).is_err() {
                            return Ok(());
                        }
                    }
                    continue;
                }
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => continue,
            };
            #[allow(clippy::disallowed_methods)]
            {
                last = std::time::Instant::now();
            }
            // Only a command that is one ends the server, not its name inside a message.
            let method = serde_json::from_str::<Json>(&msg)
                .ok()
                .and_then(|m| m.get("method")?.as_str().map(str::to_owned));
            let closing = matches!(
                (proto, method.as_deref()),
                (Proto::Cdp, Some("Browser.close"))
                    | (Proto::Bidi, Some("browser.close" | "session.end"))
            );
            for out in handle(proto, Input::Message(&msg))? {
                if ws.send(Message::text(out)).is_err() {
                    return Ok(());
                }
            }
            if proto == Proto::Cdp {
                refresh_pages(handle, &pages, self.port)?;
            }
            if closing {
                let _ = ws.close(None);
                let _ = ws.flush();
                break;
            }
        }
        Ok(())
    }
}

/// Updates the `/json/list` answer from the session's targets.
fn refresh_pages(handle: &mut Handler<'_>, pages: &Mutex<Json>, port: u16) -> Result<(), String> {
    let out = handle(
        Proto::Cdp,
        Input::Message(r#"{"id":-1,"method":"Target.getTargets"}"#),
    )?;
    let infos = out
        .iter()
        .filter_map(|m| serde_json::from_str::<Json>(m).ok())
        .find(|m| m["id"] == -1)
        .map(|m| m["result"]["targetInfos"].clone())
        .unwrap_or_else(|| json!([]));
    let list: Vec<Json> = infos
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            json!({
                "description": "",
                "devtoolsFrontendUrl": "",
                "id": t["targetId"],
                "title": t["title"],
                "type": "page",
                "url": t["url"],
                "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/browser/{}", browser_id()),
            })
        })
        .collect();
    if let Ok(mut p) = pages.lock() {
        *p = Json::Array(list);
    }
    Ok(())
}
