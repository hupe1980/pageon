//! Streamable HTTP for MCP 2026-07-28: one POST endpoint on 127.0.0.1 at a path with a
//! per-run token, one request at a time. A reply is JSON, or an SSE stream once the call sends
//! a notification (a takeover's progress); the final response ends the stream.
//!
//! Refused before anything reaches the browser: a foreign `Host` (DNS rebinding), another
//! path, a method other than POST, an `Origin` not allowed (a web page), a body without
//! `Content-Length` or over 4 MiB, a request head and body slower than 2 s, and headers that
//! disagree with the body (`MCP-Protocol-Version`, `Mcp-Method`, `Mcp-Name`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Backend, MAX_LINE, META_VERSION, Server};

/// The HTTP transport's options.
#[derive(Debug, Clone, Default)]
pub struct HttpOptions {
    /// Port on 127.0.0.1 (0: pick a free one).
    pub port: u16,
    /// Web origins allowed to call (`*` for any); requests without `Origin` are allowed.
    pub allow_origins: Vec<String>,
}

/// How long a request's head and body may take to arrive: requests are served one at a time.
const DEADLINE: Duration = Duration::from_secs(2);

/// Binds, prints the endpoint URL on stderr, and serves until the process ends.
pub(super) fn serve(
    b: &mut dyn Backend,
    options: &HttpOptions,
    otlp: Option<crate::otlp::Exporter>,
) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", options.port)).map_err(|e| {
        format!(
            "cannot listen on 127.0.0.1:{}: {e}; pick another port, or 0 for a free one",
            options.port
        )
    })?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let token = crate::takeover::token()?;
    let path = format!("/mcp/{token}");
    eprintln!("MCP endpoint: http://127.0.0.1:{port}{path}");
    let mut server = Server::new(true, otlp);
    for stream in listener.incoming().flatten() {
        handle(b, &mut server, &stream, &path, &options.allow_origins);
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Result<Vec<u8>, &'static str>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Reads under one deadline for the whole request, however the bytes trickle in.
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl Read for Deadline<'_> {
    #[allow(clippy::disallowed_methods)] // a network deadline is real time
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.stream.set_read_timeout(Some(left))?;
        (&mut &*self.stream).read(buf)
    }
}

#[allow(clippy::disallowed_methods)] // a network deadline is real time
fn read_request(stream: &TcpStream) -> Option<Request> {
    let until = Instant::now() + DEADLINE;
    let mut r = BufReader::new(Deadline { stream, until }.take(64 * 1024 + MAX_LINE as u64));
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    let mut head = line.len();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 {
            return None;
        }
        head += h.len();
        if head > 64 * 1024 {
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
    let mut req = Request {
        method,
        path,
        headers,
        body: Err("411 Length Required"),
    };
    if req.method != "POST" {
        return Some(req);
    }
    let Some(len) = req
        .header("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return Some(req);
    };
    if len > MAX_LINE {
        req.body = Err("413 Content Too Large");
        return Some(req);
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    req.body = Ok(body);
    Some(req)
}

fn respond(mut s: &TcpStream, status: &str, extra: &str, body: &[u8]) {
    let ctype = if body.is_empty() {
        String::new()
    } else {
        "Content-Type: application/json\r\n".to_string()
    };
    let head = format!(
        "HTTP/1.1 {status}\r\n{ctype}Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
    let _ = s.flush();
}

/// An error without a request id (transport-level refusals).
fn refuse(s: &TcpStream, status: &str, code: i64, message: &str) {
    let body = json!({"jsonrpc": "2.0", "error": {"code": code, "message": message}});
    respond(s, status, "", body.to_string().as_bytes());
}

/// A header value, decoded from the `=?base64?…?=` form the spec uses for unsafe values.
fn decoded(v: &str) -> Option<String> {
    use base64::Engine as _;
    match v
        .strip_prefix("=?base64?")
        .and_then(|r| r.strip_suffix("?="))
    {
        Some(b) => base64::engine::general_purpose::STANDARD
            .decode(b)
            .ok()
            .and_then(|b| String::from_utf8(b).ok()),
        None => Some(v.to_string()),
    }
}

/// Which header disagrees with the body, if any.
fn mismatch(req: &Request, msg: &Value) -> Option<String> {
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = msg.get("params").unwrap_or(&Value::Null);
    let check = |header: &str, body: Option<&str>| -> Option<String> {
        let got = req.header(header).and_then(decoded);
        (got.as_deref() != body).then(|| match got {
            None => format!("Header mismatch: {header} header is missing"),
            Some(g) => format!(
                "Header mismatch: {header} header value '{g}' does not match body value '{}'",
                body.unwrap_or_default()
            ),
        })
    };
    let version = params
        .get("_meta")
        .and_then(|m| m.get(META_VERSION))
        .and_then(Value::as_str);
    if version.is_some()
        && let Some(m) = check("mcp-protocol-version", version)
    {
        return Some(m);
    }
    if let Some(m) = check("mcp-method", Some(method)) {
        return Some(m);
    }
    if method == "tools/call" {
        return check("mcp-name", params.get("name").and_then(Value::as_str));
    }
    None
}

fn handle(
    b: &mut dyn Backend,
    server: &mut Server,
    stream: &TcpStream,
    path: &str,
    allow: &[String],
) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    let Some(req) = read_request(stream) else {
        return; // too slow, too large a head, or not HTTP: drop the connection
    };
    if !crate::cdp::server::loopback_host(req.header("host")) {
        return refuse(
            stream,
            "400 Bad Request",
            -32600,
            "Host header is not a loopback name (pageon only serves 127.0.0.1)",
        );
    }
    if req.path.split('?').next() != Some(path) {
        return refuse(
            stream,
            "404 Not Found",
            -32600,
            "not found: use the URL pageon mcp --http printed",
        );
    }
    if req.method != "POST" {
        return respond(stream, "405 Method Not Allowed", "Allow: POST\r\n", b"");
    }
    if let Some(origin) = req.header("origin")
        && !allow
            .iter()
            .any(|a| a == "*" || a.eq_ignore_ascii_case(origin))
    {
        return refuse(
            stream,
            "403 Forbidden",
            -32600,
            &format!(
                "the {origin} origin may not call this server: start it with --allow-origin {origin} to allow it"
            ),
        );
    }
    let body = match &req.body {
        Ok(b) => b,
        Err(status) => {
            return refuse(
                stream,
                status,
                -32600,
                "send the body with a Content-Length of at most 4 MiB",
            );
        }
    };
    let msg: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return refuse(
                stream,
                "400 Bad Request",
                -32700,
                &format!("parse error: {e}"),
            );
        }
    };
    // Notifications and responses: accepted, nothing to answer.
    if msg.get("id").is_none() || msg.get("method").is_none() {
        return respond(stream, "202 Accepted", "", b"");
    }
    if let Some(m) = mismatch(&req, &msg) {
        let body =
            json!({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32020, "message": m}});
        return respond(stream, "400 Bad Request", "", body.to_string().as_bytes());
    }
    // A notification turns the reply into an event stream; the reply ends it.
    let mut streaming = false;
    let event = |m: &Value, streaming: &mut bool| {
        let mut s = stream;
        if !*streaming {
            *streaming = true;
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\n");
        }
        let _ = write!(s, "event: message\ndata: {m}\n\n");
        let _ = s.flush();
    };
    let reply = server.handle(b, &msg, &mut |n| event(&n, &mut streaming));
    let Some(reply) = reply else {
        return respond(stream, "202 Accepted", "", b"");
    };
    if streaming {
        return event(&reply, &mut streaming);
    }
    let status = match reply.pointer("/error/code").and_then(Value::as_i64) {
        None => "200 OK",
        Some(-32601) => "404 Not Found",
        Some(-32603) => "500 Internal Server Error",
        Some(_) => "400 Bad Request",
    };
    respond(stream, status, "", reply.to_string().as_bytes());
}
