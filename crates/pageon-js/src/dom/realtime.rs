//! `WebSocket`, `CloseEvent` and `EventSource`: realtime connections, held by the broker.
//!
//! A document queues what it does (open, send, close); the engine performs it while the page
//! settles, outside any task, through the session's network state (cookies, the egress
//! checks) and `Env::stream` (policy, secrets, recording). Messages are collected when the
//! engine is idle and delivered one task each, on the virtual clock.

use std::collections::{HashMap, VecDeque};

use pageon_env::{NetError, StreamCall, StreamEvent, StreamKind, StreamReply};
use pageon_net::Dest;
use url::Url;

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::dom::event_target;
use crate::generated::{CloseEventInit, EventSourceInit, Iface};
use crate::realm::{EventData, Slot, Target, Timer};

/// `CONNECTING`.
const CONNECTING: u16 = 0;
/// `OPEN`.
const OPEN: u16 = 1;
/// `CLOSING`.
const CLOSING: u16 = 2;
/// `CLOSED`.
const CLOSED: u16 = 3;

/// The real time an action that sent a frame waits for the first reply (ms).
pub const REPLY_WAIT_MS: u64 = 1000;

/// A `WebSocket`.
#[derive(Debug, Clone)]
pub struct WsData {
    url: String,
    origin: String,
    state: u16,
    protocols: Vec<String>,
    protocol: String,
    arraybuffer: bool,
    buffered: u64,
    conn: Option<u64>,
}

/// An `EventSource`.
#[derive(Debug, Clone)]
pub struct SseData {
    url: String,
    origin: String,
    credentials: bool,
    state: u16,
    conn: Option<u64>,
    /// Bytes not yet split into lines.
    buf: Vec<u8>,
    started: bool,
    data: String,
    event: String,
    last_id: String,
    retry: u64,
    /// Bumped by `close()`: a reconnection timer of an earlier generation does nothing.
    generation: u32,
}

/// What a document asked of its connections, performed in order while the page settles.
#[derive(Debug)]
pub enum Op {
    /// Open (or reopen) the connection of `obj`.
    Open {
        /// Realm.
        realm: usize,
        /// The `WebSocket` or `EventSource`.
        obj: u32,
    },
    /// Send a frame.
    Send {
        /// Realm.
        realm: usize,
        /// The `WebSocket`.
        obj: u32,
        /// The frame.
        bytes: Vec<u8>,
        /// A text frame.
        text: bool,
    },
    /// Close.
    Close {
        /// Realm.
        realm: usize,
        /// The `WebSocket` or `EventSource`.
        obj: u32,
        /// Close code.
        code: u16,
        /// Close reason.
        reason: String,
    },
}

/// The page's realtime connections.
#[derive(Debug, Default)]
pub struct Streams {
    /// Queued operations.
    pub ops: VecDeque<Op>,
    /// Open connections: the broker's id → realm and object.
    pub open: HashMap<u64, (usize, u32)>,
    /// A frame was sent since the last reply wait.
    pub sent: bool,
}

fn queue(cx: &Cx<'_, '_, '_>, op: Op) {
    cx.realm.shared.borrow_mut().streams.ops.push_back(op);
}

fn time(cx: &Cx<'_, '_, '_>) -> f64 {
    let sh = cx.realm.shared.borrow();
    sh.now - sh.origin
}

fn illegal() -> Error {
    Error::Type("Illegal invocation".into())
}

fn ws<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&mut WsData) -> T) -> R<T> {
    match cx.realm.st.borrow_mut().slot_mut(id) {
        Some(Slot::WebSocket(w)) => Ok(f(w)),
        _ => Err(illegal()),
    }
}

fn sse<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&mut SseData) -> T) -> R<T> {
    match cx.realm.st.borrow_mut().slot_mut(id) {
        Some(Slot::EventSource(s)) => Ok(f(s)),
        _ => Err(illegal()),
    }
}

fn base(cx: &Cx<'_, '_, '_>) -> Option<Url> {
    Url::parse(&cx.realm.st.borrow().url).ok()
}

/// A `MessageEvent` with `data`, dispatched at `obj`.
fn message(
    cx: &mut Cx<'_, '_, '_>,
    obj: u32,
    ty: &str,
    data: Value,
    origin: String,
    last_id: String,
) {
    let ev = EventData {
        ty: ty.into(),
        initialized: true,
        trusted: true,
        time: time(cx),
        detail: Some(data),
        message: Some(Box::new((origin, last_id, None))),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::MessageEvent, Slot::Event(Box::new(ev)));
    let _ = event_target::dispatch(cx, Target::Object(obj), e);
}

fn close_event(cx: &mut Cx<'_, '_, '_>, obj: u32, clean: bool, code: u16, reason: String) {
    let ev = EventData {
        ty: "close".into(),
        initialized: true,
        trusted: true,
        time: time(cx),
        close: Some(Box::new((clean, code, reason))),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::CloseEvent, Slot::Event(Box::new(ev)));
    let _ = event_target::dispatch(cx, Target::Object(obj), e);
}

fn console(cx: &Cx<'_, '_, '_>, msg: String) {
    cx.realm
        .shared
        .borrow_mut()
        .console
        .push(("error".into(), msg));
}

/// The handshake call of `obj`'s connection, or `None` when it failed at once (the failure
/// is reported to the page).
pub(crate) fn handshake(cx: &mut Cx<'_, '_, '_>, obj: u32) -> Option<StreamCall> {
    let (url, dest, credentials, kind, last_id) = match cx.realm.st.borrow().slot(obj) {
        Some(Slot::WebSocket(w)) if w.state == CONNECTING => (
            w.url.clone(),
            Dest::WebSocket,
            true,
            StreamKind::WebSocket {
                protocols: w.protocols.clone(),
            },
            String::new(),
        ),
        Some(Slot::EventSource(s)) if s.state == CONNECTING => (
            s.url.clone(),
            Dest::EventSource,
            s.credentials,
            StreamKind::EventSource,
            s.last_id.clone(),
        ),
        _ => return None,
    };
    let parsed = Url::parse(&url).ok()?;
    let now = cx.realm.shared.borrow().now as u64;
    let (client, net) = {
        let st = cx.realm.st.borrow();
        (st.client.clone(), st.net.clone())
    };
    let req = net
        .borrow_mut()
        .stream_request(&client, &parsed, dest, credentials, now);
    match req {
        Ok(mut req) => {
            if !last_id.is_empty() {
                req.headers.push(("last-event-id".into(), last_id));
            }
            Some(StreamCall::Open { req, kind })
        }
        Err(e) => {
            console(cx, format!("connection to '{url}' failed: {e}"));
            fail(cx, obj, false);
            None
        }
    }
}

/// The handshake's answer: the connection's id when it is open.
pub(crate) fn opened(
    cx: &mut Cx<'_, '_, '_>,
    obj: u32,
    reply: Result<StreamReply, NetError>,
) -> Option<u64> {
    let (url, dest) = match cx.realm.st.borrow().slot(obj) {
        Some(Slot::WebSocket(w)) => (w.url.clone(), Dest::WebSocket),
        Some(Slot::EventSource(s)) => (s.url.clone(), Dest::EventSource),
        _ => return None,
    };
    let net = cx.realm.st.borrow().net.clone();
    let (id, status, headers) = match reply {
        Ok(StreamReply::Opened {
            id,
            status,
            headers,
        }) => (id, status, headers),
        Ok(_) => return None,
        Err(e) => {
            let outcome = match &e {
                NetError::Blocked(why) => pageon_net::Outcome::Blocked(why.clone()),
                e => pageon_net::Outcome::Failed(e.to_string()),
            };
            net.borrow_mut()
                .log_stream("GET", &url, dest, None, outcome);
            console(cx, format!("connection to '{url}' failed: {e}"));
            // An event stream that could not connect tries again; a refusal is final.
            let retry = dest == Dest::EventSource && !matches!(e, NetError::Blocked(_));
            fail(cx, obj, retry);
            return None;
        }
    };
    let header = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.trim().to_string())
    };
    // What the page may not accept, checked here (the connection is then closed).
    let refused = match dest {
        Dest::WebSocket => {
            let offered = match cx.realm.st.borrow().slot(obj) {
                Some(Slot::WebSocket(w)) => w.protocols.clone(),
                _ => Vec::new(),
            };
            match header("sec-websocket-protocol") {
                Some(p) if !offered.contains(&p) => Some(format!(
                    "the server chose subprotocol '{p}', which was not offered"
                )),
                Some(p) => {
                    let _ = ws(cx, obj, |w| w.protocol = p);
                    None
                }
                None => None,
            }
        }
        _ => {
            let ty = header("content-type").unwrap_or_default();
            let (origin, credentials) = match cx.realm.st.borrow().slot(obj) {
                Some(Slot::EventSource(s)) => (s.origin.clone(), s.credentials),
                _ => (String::new(), false),
            };
            let page = cx
                .realm
                .st
                .borrow()
                .client
                .url
                .origin()
                .ascii_serialization();
            if status != 200 {
                Some(format!("the response's status is {status}"))
            } else if !ty.to_ascii_lowercase().starts_with("text/event-stream") {
                Some(format!(
                    "the response's type is '{ty}', not text/event-stream"
                ))
            } else if origin != page
                && !matches!(header("access-control-allow-origin").as_deref(),
                    Some(o) if o == page || (o == "*" && !credentials))
            {
                Some("CORS: the response does not allow this origin".into())
            } else {
                None
            }
        }
    };
    if let Some(why) = refused {
        net.borrow_mut().log_stream(
            "GET",
            &url,
            dest,
            Some(status),
            pageon_net::Outcome::Failed(why.clone()),
        );
        console(cx, format!("connection to '{url}' failed: {why}"));
        cx.realm
            .shared
            .borrow_mut()
            .streams
            .ops
            .push_front(Op::Close {
                realm: cx.realm.id,
                obj,
                code: 1002,
                reason: String::new(),
            });
        let _ = set_conn(cx, obj, id);
        fail(cx, obj, false);
        return None;
    }
    net.borrow_mut()
        .log_stream("GET", &url, dest, Some(status), pageon_net::Outcome::Ok);
    let still = set_conn(cx, obj, id);
    if !still {
        // `close()` ran first: the connection closes again at once.
        return Some(id);
    }
    let _ = event_target::fire(cx, Target::Object(obj), "open", false, false);
    Some(id)
}

/// Records the connection's id and opens it; false when the page closed it meanwhile.
fn set_conn(cx: &Cx<'_, '_, '_>, obj: u32, id: u64) -> bool {
    match cx.realm.st.borrow_mut().slot_mut(obj) {
        Some(Slot::WebSocket(w)) => {
            w.conn = Some(id);
            if w.state == CONNECTING {
                w.state = OPEN;
                return true;
            }
            false
        }
        Some(Slot::EventSource(s)) => {
            s.conn = Some(id);
            s.buf.clear();
            s.started = false;
            if s.state == CONNECTING {
                s.state = OPEN;
                return true;
            }
            false
        }
        _ => false,
    }
}

/// Fails a connection: a WebSocket closes (`error`, then `close` with 1006); an event
/// stream reconnects after its retry time (`retry`) or closes for good.
pub(crate) fn fail(cx: &mut Cx<'_, '_, '_>, obj: u32, retry: bool) {
    let kind = match cx.realm.st.borrow_mut().slot_mut(obj) {
        // The connection's id stays: a close queued for it still finds it.
        Some(Slot::WebSocket(w)) => {
            let was = w.state;
            w.state = CLOSED;
            Some(was != CLOSED)
        }
        Some(Slot::EventSource(s)) => {
            if s.state == CLOSED {
                return;
            }
            s.state = if retry { CONNECTING } else { CLOSED };
            None
        }
        _ => return,
    };
    match kind {
        Some(true) => {
            let _ = event_target::fire(cx, Target::Object(obj), "error", false, false);
            close_event(cx, obj, false, 1006, String::new());
        }
        Some(false) => {}
        None => {
            let _ = event_target::fire(cx, Target::Object(obj), "error", false, false);
            if retry {
                schedule_reconnect(cx, obj);
            }
        }
    }
}

fn schedule_reconnect(cx: &mut Cx<'_, '_, '_>, obj: u32) {
    let Ok((retry, generation)) = sse(cx, obj, |s| (s.retry, s.generation)) else {
        return;
    };
    let data = v8::Number::new(
        cx.scope,
        f64::from(obj) * 65_536.0 + f64::from(generation & 0xffff),
    );
    let Some(f) = v8::Function::builder(reconnect)
        .data(data.into())
        .build(cx.scope)
    else {
        return;
    };
    let cb = cx.keep(f.into());
    let realm = cx.realm.id;
    cx.realm.shared.borrow_mut().schedule(
        Timer {
            id: 0,
            realm,
            cb,
            args: Vec::new(),
            interval: None,
            frame: false,
            idle: false,
        },
        retry as f64,
    );
}

/// An event stream's reconnection timer: reopens unless closed meanwhile.
fn reconnect(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let n = args.data().number_value(scope).unwrap_or(0.0) as u64;
    let (obj, generation) = ((n >> 16) as u32, (n & 0xffff) as u32);
    crate::cv::call(scope, &args, &mut rv, |cx, _| {
        let again = sse(cx, obj, |s| {
            s.state == CONNECTING && s.generation & 0xffff == generation
        })?;
        if again {
            queue(
                cx,
                Op::Open {
                    realm: cx.realm.id,
                    obj,
                },
            );
        }
        Ok(crate::cv::Out::Undefined)
    });
}

/// The call that sends a frame of `obj`, if it is open.
pub(crate) fn send_call(
    cx: &Cx<'_, '_, '_>,
    obj: u32,
    bytes: Vec<u8>,
    text: bool,
) -> Option<StreamCall> {
    let (id, url) = ws(cx, obj, |w| (w.conn, w.url.clone())).ok()?;
    let id = id?;
    let mut req = pageon_env::Request::get(url);
    req.method = "SEND".into();
    req.body = bytes;
    req.labels = vec![pageon_env::WEBSOCKET.into()];
    req.cause = cx.realm.st.borrow().net.borrow().cause.clone();
    Some(StreamCall::Send { id, req, text })
}

/// A frame's result: logged; a refused frame fails the connection (closed with 1008 and the
/// reason). Returns the code to close the broker's connection with, if any.
pub(crate) fn sent(
    cx: &mut Cx<'_, '_, '_>,
    obj: u32,
    len: usize,
    result: Result<StreamReply, NetError>,
) -> Option<(u64, String)> {
    let Ok((url, conn)) = ws(cx, obj, |w| {
        w.buffered = w.buffered.saturating_sub(len as u64);
        (w.url.clone(), w.conn)
    }) else {
        return None;
    };
    let net = cx.realm.st.borrow().net.clone();
    match result {
        Ok(_) => {
            net.borrow_mut().log_stream(
                "SEND",
                &url,
                Dest::WebSocket,
                None,
                pageon_net::Outcome::Ok,
            );
            cx.realm.shared.borrow_mut().streams.sent = true;
            None
        }
        Err(e) => {
            let why = match e {
                NetError::Blocked(w) => w,
                e => e.to_string(),
            };
            net.borrow_mut().log_stream(
                "SEND",
                &url,
                Dest::WebSocket,
                None,
                pageon_net::Outcome::Blocked(why.clone()),
            );
            console(cx, format!("WebSocket frame to '{url}' refused: {why}"));
            let _ = ws(cx, obj, |w| {
                w.state = CLOSED;
                w.conn = None;
            });
            let _ = event_target::fire(cx, Target::Object(obj), "error", false, false);
            close_event(cx, obj, false, 1008, why.clone());
            conn.map(|c| (c, why))
        }
    }
}

/// One message (or the end) of `obj`'s connection, as a task.
pub(crate) fn deliver(cx: &mut Cx<'_, '_, '_>, obj: u32, event: StreamEvent) {
    let is_ws = matches!(cx.realm.st.borrow().slot(obj), Some(Slot::WebSocket(_)));
    if is_ws {
        deliver_ws(cx, obj, event);
    } else {
        deliver_sse(cx, obj, event);
    }
}

fn deliver_ws(cx: &mut Cx<'_, '_, '_>, obj: u32, event: StreamEvent) {
    let Ok((state, origin, arraybuffer)) =
        ws(cx, obj, |w| (w.state, w.origin.clone(), w.arraybuffer))
    else {
        return;
    };
    match event {
        StreamEvent::Text(t) if state == OPEN => {
            let v: v8::Local<v8::Value> = match v8::String::new(cx.scope, &t) {
                Some(s) => s.into(),
                None => return,
            };
            let data = cx.keep(v);
            message(cx, obj, "message", data, origin, String::new());
        }
        StreamEvent::Binary(b) if state == OPEN => {
            let data = if arraybuffer {
                let store = v8::ArrayBuffer::new_backing_store_from_vec(b).make_shared();
                let ab: v8::Local<v8::Value> =
                    v8::ArrayBuffer::with_backing_store(cx.scope, &store).into();
                cx.keep(ab)
            } else {
                let blob = crate::dom::blobs::BlobData {
                    bytes: b.into(),
                    content_type: String::new(),
                    file: None,
                };
                let o = cx.alloc(Iface::Blob, Slot::Blob(std::rc::Rc::new(blob)));
                let w: v8::Local<v8::Value> = cx.wrap(o).into();
                cx.keep(w)
            };
            message(cx, obj, "message", data, origin, String::new());
        }
        StreamEvent::Closed {
            code,
            reason,
            clean,
        } => {
            let _ = ws(cx, obj, |w| {
                w.state = CLOSED;
                w.conn = None;
            });
            if state == CLOSED {
                return;
            }
            if !clean {
                let _ = event_target::fire(cx, Target::Object(obj), "error", false, false);
            }
            close_event(cx, obj, clean, code, reason);
        }
        _ => {}
    }
}

fn deliver_sse(cx: &mut Cx<'_, '_, '_>, obj: u32, event: StreamEvent) {
    match event {
        StreamEvent::Binary(b) => {
            let Ok(lines) = sse(cx, obj, |s| {
                if s.state != OPEN {
                    return Some(Vec::new());
                }
                s.buf.extend_from_slice(&b);
                let lines = take_lines(s);
                // A line or an event that never ends is bounded like a WebSocket message.
                (s.buf.len() <= pageon_env::MAX_MESSAGE && s.data.len() <= pageon_env::MAX_MESSAGE)
                    .then_some(lines)
            }) else {
                return;
            };
            let Some(lines) = lines else {
                console(
                    cx,
                    format!(
                        "event stream: an event was over {} MiB",
                        pageon_env::MAX_MESSAGE >> 20
                    ),
                );
                queue(
                    cx,
                    Op::Close {
                        realm: cx.realm.id,
                        obj,
                        code: 1009,
                        reason: String::new(),
                    },
                );
                fail(cx, obj, false);
                return;
            };
            for line in lines {
                field(cx, obj, &line);
                if s_too_big(cx, obj) {
                    console(
                        cx,
                        format!(
                            "event stream: an event was over {} MiB",
                            pageon_env::MAX_MESSAGE >> 20
                        ),
                    );
                    queue(
                        cx,
                        Op::Close {
                            realm: cx.realm.id,
                            obj,
                            code: 1009,
                            reason: String::new(),
                        },
                    );
                    fail(cx, obj, false);
                    return;
                }
            }
        }
        StreamEvent::Text(t) => deliver_sse(cx, obj, StreamEvent::Binary(t.into_bytes())),
        StreamEvent::Closed { .. } => fail(cx, obj, true),
    }
}

fn s_too_big(cx: &Cx<'_, '_, '_>, obj: u32) -> bool {
    sse(cx, obj, |s| s.data.len() > pageon_env::MAX_MESSAGE).unwrap_or(false)
}

/// The complete lines in the buffer (CRLF, LF or CR), as text; the BOM of the stream dropped.
fn take_lines(s: &mut SseData) -> Vec<String> {
    let mut out = Vec::new();
    if !s.started {
        if s.buf.len() < 3 && b"\xEF\xBB\xBF".starts_with(&s.buf) {
            return out;
        }
        if s.buf.starts_with(b"\xEF\xBB\xBF") {
            s.buf.drain(..3);
        }
        s.started = true;
    }
    let mut start = 0;
    let mut i = 0;
    while i < s.buf.len() {
        match s.buf[i] {
            b'\n' => {
                out.push(String::from_utf8_lossy(&s.buf[start..i]).into_owned());
                start = i + 1;
            }
            b'\r' => {
                // A CR at the end may be the first half of CRLF: wait for more.
                if i + 1 == s.buf.len() {
                    break;
                }
                out.push(String::from_utf8_lossy(&s.buf[start..i]).into_owned());
                if s.buf[i + 1] == b'\n' {
                    i += 1;
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    s.buf.drain(..start);
    out
}

/// One line of an event stream (HTML's "interpreting an event stream").
fn field(cx: &mut Cx<'_, '_, '_>, obj: u32, line: &str) {
    if line.is_empty() {
        let Ok(Some((ty, data, origin, last_id))) = sse(cx, obj, |s| {
            if s.data.is_empty() {
                s.event.clear();
                return None;
            }
            let mut data = std::mem::take(&mut s.data);
            if data.ends_with('\n') {
                data.pop();
            }
            let ty = std::mem::take(&mut s.event);
            let ty = if ty.is_empty() {
                "message".to_string()
            } else {
                ty
            };
            Some((ty, data, s.origin.clone(), s.last_id.clone()))
        }) else {
            return;
        };
        let Some(v) = v8::String::new(cx.scope, &data) else {
            return;
        };
        let data = cx.keep(v.into());
        message(cx, obj, &ty, data, origin, last_id);
        return;
    }
    if line.starts_with(':') {
        return;
    }
    let (name, value) = match line.split_once(':') {
        Some((n, v)) => (n, v.strip_prefix(' ').unwrap_or(v)),
        None => (line, ""),
    };
    let _ = sse(cx, obj, |s| match name {
        "event" => s.event = value.to_string(),
        "data" => {
            s.data.push_str(value);
            s.data.push('\n');
        }
        "id" if !value.contains('\0') => s.last_id = value.to_string(),
        "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
            s.retry = value.parse().unwrap_or(s.retry);
        }
        _ => {}
    });
}

/// `WebSocket`.
#[allow(missing_docs)]
pub mod web_socket {
    use super::*;

    fn data<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&mut WsData) -> T) -> R<T> {
        ws(cx, this.slab()?, f)
    }

    fn syntax(msg: String) -> Error {
        Error::Dom("SyntaxError", msg)
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, url: String, protocols: Value) -> R<Obj> {
        let fail = |m: &str| syntax(format!("Failed to construct 'WebSocket': {m}"));
        let mut u = match base(cx) {
            Some(b) => b.join(&url),
            None => Url::parse(&url),
        }
        .map_err(|_| fail(&format!("The URL '{url}' is invalid.")))?;
        let scheme = match u.scheme() {
            "http" => Some("ws"),
            "https" => Some("wss"),
            "ws" | "wss" => None,
            s => {
                return Err(fail(&format!(
                    "The URL's scheme must be either 'http', 'https', 'ws', or 'wss'. '{s}' is not allowed."
                )));
            }
        };
        if let Some(s) = scheme {
            let text = u.as_str().replacen(u.scheme(), s, 1);
            u = Url::parse(&text).map_err(|_| fail("The URL is invalid."))?;
        }
        if u.fragment().is_some() {
            return Err(fail(&format!(
                "The URL contains a fragment identifier ('{}'). Fragment identifiers are not allowed in WebSocket URLs.",
                u.fragment().unwrap_or_default()
            )));
        }
        let pv = cx.local(&protocols);
        let list: Vec<String> = if pv.is_string() {
            vec![crate::cv::string(cx, pv)?]
        } else if let Ok(a) = v8::Local::<v8::Array>::try_from(pv) {
            let mut out = Vec::new();
            for i in 0..a.length() {
                let v = a.get_index(cx.scope, i).ok_or(Error::Terminated)?;
                out.push(crate::cv::string(cx, v)?);
            }
            out
        } else if pv.is_null_or_undefined() {
            Vec::new()
        } else {
            vec![crate::cv::string(cx, pv)?]
        };
        for (i, p) in list.iter().enumerate() {
            let token = !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b));
            if !token || list[..i].contains(p) {
                return Err(fail(&format!("The subprotocol '{p}' is invalid.")));
            }
        }
        if !crate::dom::net::count_request(cx) {
            return Err(fail("Too many requests."));
        }
        let origin = u.origin().ascii_serialization();
        let o = cx.alloc(
            Iface::WebSocket,
            Slot::WebSocket(Box::new(WsData {
                url: u.to_string(),
                origin,
                state: CONNECTING,
                protocols: list,
                protocol: String::new(),
                arraybuffer: false,
                buffered: 0,
                conn: None,
            })),
        );
        queue(
            cx,
            Op::Open {
                realm: cx.realm.id,
                obj: o.slab()?,
            },
        );
        Ok(o)
    }

    pub fn get_url(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        data(cx, this, |w| w.url.clone())
    }

    pub fn get_ready_state(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        data(cx, this, |w| w.state)
    }

    pub fn get_buffered_amount(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u64> {
        data(cx, this, |w| w.buffered)
    }

    pub fn get_extensions(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(String::new())
    }

    pub fn get_protocol(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        data(cx, this, |w| w.protocol.clone())
    }

    pub fn get_binary_type(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        data(cx, this, |w| {
            if w.arraybuffer { "arraybuffer" } else { "blob" }.to_string()
        })
    }

    pub fn set_binary_type(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        // Another value is ignored (WebIDL enum attributes).
        match v.as_str() {
            "blob" => data(cx, this, |w| w.arraybuffer = false),
            "arraybuffer" => data(cx, this, |w| w.arraybuffer = true),
            _ => Ok(()),
        }
    }

    pub fn close(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        code: Option<u16>,
        reason: Option<String>,
    ) -> R<()> {
        if let Some(c) = code
            && c != 1000
            && !(3000..=4999).contains(&c)
        {
            return Err(Error::Dom(
                "InvalidAccessError",
                format!(
                    "Failed to execute 'close' on 'WebSocket': The close code must be either 1000, or between 3000 and 4999. {c} is neither."
                ),
            ));
        }
        let reason = reason.unwrap_or_default();
        if reason.len() > 123 {
            return Err(syntax(
                "Failed to execute 'close' on 'WebSocket': The close reason must not be greater than 123 UTF-8 bytes.".into(),
            ));
        }
        let id = this.slab()?;
        let state = data(cx, this, |w| w.state)?;
        if state == CLOSING || state == CLOSED {
            return Ok(());
        }
        data(cx, this, |w| w.state = CLOSING)?;
        queue(
            cx,
            Op::Close {
                realm: cx.realm.id,
                obj: id,
                code: code.unwrap_or(1000),
                reason,
            },
        );
        Ok(())
    }

    pub fn send(cx: &mut Cx<'_, '_, '_>, this: Obj, value: Value) -> R<()> {
        let state = data(cx, this, |w| w.state)?;
        if state == CONNECTING {
            return Err(Error::Dom(
                "InvalidStateError",
                "Failed to execute 'send' on 'WebSocket': Still in CONNECTING state.".into(),
            ));
        }
        let v = cx.local(&value);
        let (bytes, text) = if v.is_string() {
            (crate::cv::string(cx, v)?.into_bytes(), true)
        } else {
            (crate::dom::net::body_init(cx, v)?.0, false)
        };
        let len = bytes.len() as u64;
        data(cx, this, |w| w.buffered += len)?;
        if state != OPEN {
            return Ok(());
        }
        queue(
            cx,
            Op::Send {
                realm: cx.realm.id,
                obj: this.slab()?,
                bytes,
                text,
            },
        );
        Ok(())
    }
}

/// `CloseEvent`.
#[allow(missing_docs)]
pub mod close_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: CloseEventInit) -> R<Obj> {
        let data = EventData {
            ty,
            bubbles: init.bubbles,
            cancelable: init.cancelable,
            composed: init.composed,
            initialized: true,
            time: time(cx),
            close: Some(Box::new((init.was_clean, init.code, init.reason))),
            ..EventData::default()
        };
        Ok(cx.alloc(Iface::CloseEvent, Slot::Event(Box::new(data))))
    }

    fn fields<T>(
        cx: &Cx<'_, '_, '_>,
        this: Obj,
        f: impl FnOnce(&(bool, u16, String)) -> T,
    ) -> R<T> {
        match cx.realm.st.borrow().slot(this.slab()?) {
            Some(Slot::Event(e)) => Ok(f(e.close.as_deref().unwrap_or(&(false, 0, String::new())))),
            _ => Err(illegal()),
        }
    }

    pub fn get_was_clean(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        fields(cx, this, |c| c.0)
    }

    pub fn get_code(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        fields(cx, this, |c| c.1)
    }

    pub fn get_reason(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        fields(cx, this, |c| c.2.clone())
    }
}

/// `EventSource`.
#[allow(missing_docs)]
pub mod event_source {
    use super::*;

    fn data<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&mut SseData) -> T) -> R<T> {
        sse(cx, this.slab()?, f)
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, url: String, init: EventSourceInit) -> R<Obj> {
        let u = match base(cx) {
            Some(b) => b.join(&url),
            None => Url::parse(&url),
        }
        .map_err(|_| {
            Error::Dom(
                "SyntaxError",
                format!("Failed to construct 'EventSource': Cannot open an EventSource to '{url}'. The URL is invalid."),
            )
        })?;
        if !crate::dom::net::count_request(cx) {
            return Err(Error::Type(
                "Failed to construct 'EventSource': too many requests".into(),
            ));
        }
        let origin = u.origin().ascii_serialization();
        let o = cx.alloc(
            Iface::EventSource,
            Slot::EventSource(Box::new(SseData {
                url: u.to_string(),
                origin,
                credentials: init.with_credentials,
                state: CONNECTING,
                conn: None,
                buf: Vec::new(),
                started: false,
                data: String::new(),
                event: String::new(),
                last_id: String::new(),
                retry: 3000,
                generation: 0,
            })),
        );
        queue(
            cx,
            Op::Open {
                realm: cx.realm.id,
                obj: o.slab()?,
            },
        );
        Ok(o)
    }

    pub fn get_url(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        data(cx, this, |s| s.url.clone())
    }

    pub fn get_with_credentials(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        data(cx, this, |s| s.credentials)
    }

    pub fn get_ready_state(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        data(cx, this, |s| s.state)
    }

    pub fn close(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = this.slab()?;
        let had = data(cx, this, |s| {
            let had = s.state != CLOSED;
            s.state = CLOSED;
            s.generation = s.generation.wrapping_add(1);
            had
        })?;
        if had {
            queue(
                cx,
                Op::Close {
                    realm: cx.realm.id,
                    obj: id,
                    code: 1000,
                    reason: String::new(),
                },
            );
        }
        Ok(())
    }
}

/// The broker's id of `obj`'s connection.
pub(crate) fn conn(cx: &Cx<'_, '_, '_>, obj: u32) -> Option<u64> {
    match cx.realm.st.borrow().slot(obj) {
        Some(Slot::WebSocket(w)) => w.conn,
        Some(Slot::EventSource(s)) => s.conn,
        _ => None,
    }
}
