//! Realtime connections: WebSocket and server-sent events, through [`Env::stream`].
//!
//! Opening and sending carry ordinary [`Request`]s (`GET` with the `websocket` or
//! `eventsource` label; `SEND` with the frame as body), so every layer that judges requests
//! (policy, secrets, guard, recording) judges these too. Messages are collected by
//! [`StreamCall::Poll`] at the engine's idle points and stamped with its virtual time, so a
//! replay delivers them at the same points.
//!
//! [`Env::stream`]: crate::Env::stream

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{Duration, Instant};

use crate::{NetError, Request, b64};

/// What a connection is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    /// A WebSocket, offering these subprotocols.
    WebSocket {
        /// `Sec-WebSocket-Protocol` values offered.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        protocols: Vec<String>,
    },
    /// A `text/event-stream` response, delivered as body chunks.
    EventSource,
}

/// A call on realtime connections.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamCall {
    /// Opens a connection (the handshake runs now).
    Open {
        /// `GET` of the `ws(s):` or `http(s):` URL, with the handshake's extra headers.
        req: Request,
        /// WebSocket or event stream.
        kind: StreamKind,
    },
    /// Sends a WebSocket frame.
    Send {
        /// The connection.
        id: u64,
        /// `SEND` of the connection's URL, the frame as body.
        req: Request,
        /// A text frame (else binary).
        text: bool,
    },
    /// Closes a connection (the page's `close()`, or its document going away).
    Close {
        /// The connection.
        id: u64,
        /// Close code (1000 normal, 1001 going away).
        code: u16,
        /// Close reason.
        reason: String,
    },
    /// Messages that arrived, stamped with the virtual time `now`; a live environment may
    /// block up to `wait_ms` of real time for the first.
    Poll {
        /// Virtual time (ms since the Unix epoch).
        now: u64,
        /// Real time to wait for a first message (0: do not wait).
        wait_ms: u64,
    },
}

/// The answer to a [`StreamCall`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamReply {
    /// The connection is open.
    Opened {
        /// Its id.
        id: u64,
        /// The handshake's status (101, or 200 for an event stream).
        status: u16,
        /// The handshake's response headers (lower-case names).
        headers: Vec<(String, String)>,
    },
    /// Done (a send or a close).
    Done,
    /// Messages in arrival order, per connection.
    Events(Vec<(u64, StreamEvent)>),
}

/// Something that arrived on a connection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamEvent {
    /// A text message.
    Text(String),
    /// A binary message (WebSocket), or a body chunk (event stream).
    Binary(#[serde(with = "b64")] Vec<u8>),
    /// The connection ended.
    Closed {
        /// Close code (1006 when it broke without a close frame).
        code: u16,
        /// Close reason.
        reason: String,
        /// A clean close (a close handshake, or the end of an event stream).
        clean: bool,
    },
}

/// Most connections a session holds open.
pub const MAX_STREAMS: usize = 16;
/// Largest message received (bytes); a larger one closes the connection with 1009.
pub const MAX_MESSAGE: usize = 4 << 20;
/// Longest handshake.
pub const HANDSHAKE: Duration = Duration::from_secs(30);

/// The label of a WebSocket's requests.
pub const WEBSOCKET: &str = "websocket";
/// The label of an event stream's request.
pub const EVENTSOURCE: &str = "eventsource";

/// What a live connection's thread is told.
enum Out {
    Frame(Vec<u8>, bool),
    Close(u16, String),
}

/// A handshake's status and response headers.
pub(crate) type Head = (u16, Vec<(String, String)>);

/// Opens an event stream, sending its body chunks to the channel.
pub(crate) type OpenStream = dyn Fn(&Request, Sender<StreamEvent>) -> Result<Head, NetError>;

struct Conn {
    events: Receiver<StreamEvent>,
    out: Option<Sender<Out>>,
    open: bool,
}

/// Live connections, one thread each.
#[derive(Default)]
pub(crate) struct Streams {
    conns: HashMap<u64, Conn>,
    next: u64,
}

impl Streams {
    /// Serves one call against the network.
    pub(crate) fn call(
        &mut self,
        call: &StreamCall,
        sse: &OpenStream,
    ) -> Result<StreamReply, NetError> {
        match call {
            StreamCall::Open { req, kind } => {
                if self.conns.values().filter(|c| c.open).count() >= MAX_STREAMS {
                    return Err(NetError::Blocked(format!(
                        "{MAX_STREAMS} realtime connections are open, the most a session holds"
                    )));
                }
                let (tx, rx) = channel();
                let (status, headers, out) = match kind {
                    StreamKind::WebSocket { protocols } => {
                        let ((status, headers), out) = websocket(req, protocols, tx)?;
                        (status, headers, Some(out))
                    }
                    StreamKind::EventSource => {
                        let (status, headers) = sse(req, tx)?;
                        (status, headers, None)
                    }
                };
                self.next += 1;
                let id = self.next;
                self.conns.insert(
                    id,
                    Conn {
                        events: rx,
                        out,
                        open: true,
                    },
                );
                Ok(StreamReply::Opened {
                    id,
                    status,
                    headers,
                })
            }
            StreamCall::Send { id, req, text } => {
                if let Some(out) = self.conns.get(id).and_then(|c| c.out.as_ref()) {
                    let _ = out.send(Out::Frame(req.body.clone(), *text));
                }
                Ok(StreamReply::Done)
            }
            StreamCall::Close { id, code, reason } => {
                if let Some(c) = self.conns.get_mut(id) {
                    if let Some(out) = c.out.take() {
                        let _ = out.send(Out::Close(*code, reason.clone()));
                    }
                    // An event stream's thread ends when its receiver is gone.
                    c.open = false;
                }
                Ok(StreamReply::Done)
            }
            StreamCall::Poll { wait_ms, .. } => Ok(StreamReply::Events(self.poll(*wait_ms))),
        }
    }

    #[allow(clippy::disallowed_methods)] // waiting for the network is real time
    fn poll(&mut self, wait_ms: u64) -> Vec<(u64, StreamEvent)> {
        let until = Instant::now() + Duration::from_millis(wait_ms);
        loop {
            let mut out = Vec::new();
            let mut ids: Vec<u64> = self.conns.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                let Some(c) = self.conns.get_mut(&id) else {
                    continue;
                };
                loop {
                    match c.events.try_recv() {
                        Ok(e) => {
                            let end = matches!(e, StreamEvent::Closed { .. });
                            out.push((id, e));
                            if end {
                                c.open = false;
                                break;
                            }
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            if c.open {
                                out.push((
                                    id,
                                    StreamEvent::Closed {
                                        code: 1006,
                                        reason: String::new(),
                                        clean: false,
                                    },
                                ));
                            }
                            c.open = false;
                            break;
                        }
                    }
                }
            }
            self.conns.retain(|_, c| c.open || c.out.is_some());
            if !out.is_empty() || Instant::now() >= until || !self.conns.values().any(|c| c.open) {
                return out;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Streams {
    fn drop(&mut self) {
        for c in self.conns.values_mut() {
            if let Some(out) = c.out.take() {
                let _ = out.send(Out::Close(1001, String::new()));
            }
        }
    }
}

/// The address to connect to: resolved here, refused when private unless allowed, so the
/// socket goes to the address that was checked (no second lookup to rebind).
fn vetted(host: &str, port: u16, allow_private: bool) -> Result<SocketAddr, NetError> {
    use std::net::ToSocketAddrs as _;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| NetError::Network(format!("{host}: {e}")))?
        .collect();
    let first = addrs
        .first()
        .copied()
        .ok_or_else(|| NetError::Network(format!("{host} has no address")))?;
    if allow_private {
        return Ok(first);
    }
    addrs
        .iter()
        .copied()
        .find(|a| crate::is_public(a.ip()))
        .ok_or_else(|| {
            NetError::Blocked(format!(
                "{host} resolves to the private network address {}",
                first.ip()
            ))
        })
}

/// A blocking byte stream: plain TCP, or TLS over it.
trait Io: Read + Write + Send {
    fn tcp(&self) -> &TcpStream;
}

impl Io for TcpStream {
    fn tcp(&self) -> &TcpStream {
        self
    }
}

impl Io for rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    fn tcp(&self) -> &TcpStream {
        &self.sock
    }
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, NetError> {
    use rustls_platform_verifier::ConfigVerifierExt as _;
    // The process-wide provider rustls's platform verifier uses (already installed: kept).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    rustls::ClientConfig::with_platform_verifier()
        .map(Arc::new)
        .map_err(|e| NetError::Tls(e.to_string()))
}

/// Opens a WebSocket: vetted address, TLS for `wss:`, the handshake, then a thread that
/// relays frames both ways.
#[allow(clippy::disallowed_methods)] // a network deadline is real time
fn websocket(
    req: &Request,
    protocols: &[String],
    events: Sender<StreamEvent>,
) -> Result<(Head, Sender<Out>), NetError> {
    use tungstenite::client::IntoClientRequest as _;
    let url = url::Url::parse(&req.url).map_err(|e| NetError::Network(e.to_string()))?;
    let tls = match url.scheme() {
        "wss" => true,
        "ws" => false,
        s => return Err(NetError::Network(format!("{s}: is not a WebSocket URL"))),
    };
    if !req.allow_private
        && let Some(host) = crate::private_host_of(&req.url)
    {
        return Err(NetError::Blocked(format!(
            "{host} is on the private network"
        )));
    }
    let host = url
        .host_str()
        .ok_or_else(|| NetError::Network("no host".into()))?
        .to_string();
    let port = url
        .port_or_known_default()
        .unwrap_or(if tls { 443 } else { 80 });
    let addr = vetted(&host, port, req.allow_private)?;
    let tcp = TcpStream::connect_timeout(&addr, HANDSHAKE)
        .map_err(|e| NetError::Network(format!("{host}: {e}")))?;
    tcp.set_read_timeout(Some(HANDSHAKE))
        .and_then(|()| tcp.set_write_timeout(Some(HANDSHAKE)))
        .map_err(|e| NetError::Network(e.to_string()))?;
    let _ = tcp.set_nodelay(true);
    let io: Box<dyn Io> = if tls {
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|e| NetError::Tls(e.to_string()))?;
        let conn = rustls::ClientConnection::new(tls_config()?, name)
            .map_err(|e| NetError::Tls(e.to_string()))?;
        Box::new(rustls::StreamOwned::new(conn, tcp))
    } else {
        Box::new(tcp)
    };
    let mut handshake = req
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| NetError::Network(e.to_string()))?;
    {
        let h = handshake.headers_mut();
        for (k, v) in &req.headers {
            if let (Ok(k), Ok(v)) = (
                tungstenite::http::HeaderName::from_bytes(k.as_bytes()),
                tungstenite::http::HeaderValue::from_str(v),
            ) {
                h.append(k, v);
            }
        }
        if !protocols.is_empty()
            && let Ok(v) = tungstenite::http::HeaderValue::from_str(&protocols.join(", "))
        {
            h.insert("sec-websocket-protocol", v);
        }
    }
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    let (ws, resp) = tungstenite::client::client_with_config(handshake, io, Some(config)).map_err(
        |e| match e {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)) => NetError::Network(
                format!("the server refused the WebSocket (HTTP {})", r.status()),
            ),
            tungstenite::HandshakeError::Failure(e) => NetError::Network(e.to_string()),
            tungstenite::HandshakeError::Interrupted(_) => NetError::Timeout(HANDSHAKE),
        },
    )?;
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into(),
            )
        })
        .collect();
    // Short reads let the thread interleave sending and receiving.
    let _ = ws
        .get_ref()
        .tcp()
        .set_read_timeout(Some(Duration::from_millis(20)));
    let (out_tx, out_rx) = channel::<Out>();
    std::thread::spawn(move || relay(ws, out_rx, events));
    Ok(((resp.status().as_u16(), headers), out_tx))
}

/// Moves frames between the socket and the channels until either side ends.
fn relay(
    mut ws: tungstenite::WebSocket<Box<dyn Io>>,
    out: Receiver<Out>,
    events: Sender<StreamEvent>,
) {
    use tungstenite::{Error, Message};
    let mut closing = false;
    loop {
        loop {
            match out.try_recv() {
                Ok(Out::Frame(bytes, text)) => {
                    let m = if text {
                        Message::text(String::from_utf8_lossy(&bytes).into_owned())
                    } else {
                        Message::binary(bytes)
                    };
                    let _ = ws.send(m);
                }
                Ok(Out::Close(code, reason)) => {
                    closing = true;
                    let _ = ws.close(Some(tungstenite::protocol::CloseFrame {
                        code: code.into(),
                        reason: reason.into(),
                    }));
                    let _ = ws.flush();
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !closing {
                        closing = true;
                        let _ = ws.close(None);
                        let _ = ws.flush();
                    }
                    break;
                }
            }
        }
        let event = match ws.read() {
            Ok(Message::Text(t)) => StreamEvent::Text(t.to_string()),
            Ok(Message::Binary(b)) => StreamEvent::Binary(b.to_vec()),
            Ok(Message::Close(f)) => {
                let (code, reason) = f.map_or((1005, String::new()), |f| {
                    (f.code.into(), f.reason.to_string())
                });
                let _ = ws.flush();
                let _ = events.send(StreamEvent::Closed {
                    code,
                    reason,
                    clean: true,
                });
                return;
            }
            Ok(_) => continue,
            Err(Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(Error::ConnectionClosed) => {
                let _ = events.send(StreamEvent::Closed {
                    code: 1000,
                    reason: String::new(),
                    clean: true,
                });
                return;
            }
            Err(Error::Capacity(_)) => {
                let _ = ws.close(Some(tungstenite::protocol::CloseFrame {
                    code: 1009u16.into(),
                    reason: "message too big".into(),
                }));
                let _ = ws.flush();
                let _ = events.send(StreamEvent::Closed {
                    code: 1009,
                    reason: format!("a message was over {} MiB", MAX_MESSAGE >> 20),
                    clean: false,
                });
                return;
            }
            Err(_) => {
                let _ = events.send(StreamEvent::Closed {
                    code: 1006,
                    reason: String::new(),
                    clean: false,
                });
                return;
            }
        };
        if events.send(event).is_err() {
            return;
        }
    }
}

/// One realtime entry of a recording, in the order they happened.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rt {
    /// A connection opened (or failed to).
    Open {
        /// The connection's id.
        id: u64,
        /// Its URL.
        url: String,
        /// The handshake's status.
        #[serde(default)]
        status: u16,
        /// The handshake's response headers.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        headers: Vec<(String, String)>,
        /// Why it failed, if it did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A message delivered to the page at virtual time `at`.
    Event {
        /// The connection.
        id: u64,
        /// Virtual time it was delivered at (ms since the Unix epoch).
        at: u64,
        /// What arrived.
        #[serde(flatten)]
        event: StreamEvent,
    },
    /// A frame the page sent (kept for the record; replay sends nothing).
    Sent {
        /// The connection.
        id: u64,
        /// The frame, as text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// The frame, as base64 (binary).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base64: Option<String>,
    },
    /// The page closed a connection.
    Close {
        /// The connection.
        id: u64,
        /// The close code.
        code: u16,
    },
}

impl Rt {
    /// A sent frame's entry.
    pub(crate) fn sent(id: u64, body: &[u8]) -> Self {
        use base64::Engine as _;
        match std::str::from_utf8(body) {
            Ok(t) => Rt::Sent {
                id,
                text: Some(t.to_string()),
                base64: None,
            },
            Err(_) => Rt::Sent {
                id,
                text: None,
                base64: Some(base64::engine::general_purpose::STANDARD.encode(body)),
            },
        }
    }
}

/// Serves realtime calls from recorded entries: opens matched by URL in order; messages of
/// the opened connections delivered once their virtual time has come and, per connection, in
/// log order: a message recorded after a frame the page sent (or a close) waits until the
/// replayed page has sent it, so a reply never comes before its question.
#[derive(Debug, Default)]
pub(crate) struct RtServe {
    used: Vec<bool>,
    opened: Vec<u64>,
}

impl RtServe {
    pub(crate) fn call(&mut self, log: &[Rt], call: &StreamCall) -> Result<StreamReply, NetError> {
        if self.used.len() < log.len() {
            self.used.resize(log.len(), false);
        }
        match call {
            StreamCall::Open { req, .. } => {
                let i = log
                    .iter()
                    .enumerate()
                    .position(|(i, e)| {
                        !self.used[i] && matches!(e, Rt::Open { url, .. } if *url == req.url)
                    })
                    .ok_or_else(|| {
                        NetError::ReplayMiss(format!("realtime connection to {}", req.url))
                    })?;
                self.used[i] = true;
                let Rt::Open {
                    id,
                    status,
                    headers,
                    error,
                    ..
                } = &log[i]
                else {
                    return Err(NetError::ReplayMiss(req.url.clone()));
                };
                if let Some(e) = error {
                    return Err(NetError::Network(e.clone()));
                }
                self.opened.push(*id);
                Ok(StreamReply::Opened {
                    id: *id,
                    status: *status,
                    headers: headers.clone(),
                })
            }
            StreamCall::Poll { now, .. } => {
                let mut out = Vec::new();
                let mut held: Vec<u64> = Vec::new();
                for (i, e) in log.iter().enumerate() {
                    if self.used[i] {
                        continue;
                    }
                    match e {
                        Rt::Sent { id, .. } | Rt::Close { id, .. } => held.push(*id),
                        Rt::Event { id, at, event } if self.opened.contains(id) => {
                            if *at <= *now && !held.contains(id) {
                                self.used[i] = true;
                                out.push((*id, event.clone()));
                            } else {
                                held.push(*id);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(StreamReply::Events(out))
            }
            StreamCall::Send { id, .. } | StreamCall::Close { id, .. } => {
                let sent = matches!(call, StreamCall::Send { .. });
                if let Some(i) = log.iter().enumerate().position(|(i, e)| {
                    !self.used[i]
                        && match e {
                            Rt::Sent { id: x, .. } => sent && x == id,
                            Rt::Close { id: x, .. } => !sent && x == id,
                            _ => false,
                        }
                }) {
                    self.used[i] = true;
                }
                Ok(StreamReply::Done)
            }
        }
    }
}
