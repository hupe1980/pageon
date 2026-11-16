//! The viewer: a supervision page a session serves on loopback while an agent works. It shows
//! every step (the action, its outcome, the text the model read, the page after it while
//! someone watches), pauses the agent, and decides the writes the policy holds.
//!
//! The session records into shared state; a listener thread serves the page, its event
//! stream, step images and two write endpoints, each connection on its own thread. Only the
//! first browser to open the printed URL is the viewer (an HttpOnly, SameSite=Strict cookie);
//! writes also need the page's token header and a matching `Origin`. Page text reaches the
//! browser as JSON and is shown as text (a strict CSP forbids inline script).

use std::collections::{BTreeSet, VecDeque};
use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::Effect;
use crate::takeover::{Request, plain, read_request, respond, token};

/// Steps kept (the oldest go first).
const MAX_STEPS: usize = 1000;
/// Images kept (the newest).
const MAX_IMAGES: usize = 100;
/// The longest step text kept.
const MAX_TEXT: usize = 64 * 1024;
/// Connections served at once (event streams included).
const MAX_CONNECTIONS: usize = 8;
/// How long a held write waits for a decision.
pub const DECISION_TIMEOUT: Duration = Duration::from_secs(600);
/// How long an event stream waits before a keep-alive comment.
const HEARTBEAT: Duration = Duration::from_secs(15);

const HTML: &str = include_str!("viewer.html");
const CSS: &str = include_str!("viewer.css");
const JS: &str = include_str!("viewer.js");

/// One tool call, as the viewer shows it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Step {
    /// The record's index in this session (images are found by it).
    pub n: u64,
    /// The reply's step number, when the call made one.
    pub step: Option<u64>,
    /// The tool called.
    pub tool: String,
    /// The call's arguments (strings clipped and redacted).
    pub input: Value,
    /// The call succeeded.
    pub ok: bool,
    /// What the model read (already redacted), at most 64 KiB.
    pub text: String,
    /// The current tab's URL afterwards (redacted).
    pub url: Option<String>,
    /// Wall time the call took.
    pub ms: u64,
    /// o200k tokens of what the model read.
    pub tokens: u64,
    /// An image of the page after it exists.
    pub image: bool,
}

/// What became of a held write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Waiting,
    Approved,
    Granted,
    Denied,
    Expired,
}

#[derive(Debug, Clone, serde::Serialize)]
struct Approval {
    id: u64,
    effect: Effect,
    state: Decision,
}

#[derive(Default)]
struct State {
    steps: VecDeque<Step>,
    next_n: u64,
    images: VecDeque<(u64, Vec<u8>)>,
    approvals: VecDeque<Approval>,
    next_id: u64,
    grants: BTreeSet<(String, String)>,
    paused: bool,
    /// Open event streams.
    watchers: usize,
    connections: usize,
    /// The viewer's cookie, once a browser opened the page.
    cookie: Option<String>,
    /// Bumped on every change, so event streams know when to look.
    version: u64,
}

struct Inner {
    state: Mutex<State>,
    changed: Condvar,
    port: u16,
    /// `/v/<token>`.
    path: String,
    /// Sent with every write by the page (from its `<meta>`).
    page_token: String,
}

/// A running viewer (a handle; clones share it).
#[derive(Clone)]
pub struct Viewer {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer")
            .field("port", &self.inner.port)
            .finish_non_exhaustive()
    }
}

impl Viewer {
    /// Listens on a free loopback port with a fresh token and serves until the process ends.
    pub fn start() -> Result<Self, String> {
        let listener =
            TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("cannot listen: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let inner = Arc::new(Inner {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            port,
            path: format!("/v/{}", token()?),
            page_token: token()?,
        });
        let serving = inner.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                {
                    let mut s = serving.lock();
                    if s.connections >= MAX_CONNECTIONS {
                        drop(s);
                        plain(
                            &stream,
                            "503 Service Unavailable",
                            "too many viewer connections",
                        );
                        continue;
                    }
                    s.connections += 1;
                }
                let inner = serving.clone();
                std::thread::spawn(move || {
                    let _ = inner.handle(&stream);
                    inner.lock().connections -= 1;
                });
            }
        });
        Ok(Self { inner })
    }

    /// The URL for the person.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.inner.port, self.inner.path)
    }

    /// Someone has the viewer open (images are painted only then).
    pub fn watched(&self) -> bool {
        self.inner.lock().watchers > 0
    }

    /// Records a tool call; returns its record number.
    pub fn record(&self, mut step: Step, image: Option<Vec<u8>>) -> u64 {
        let mut s = self.inner.lock();
        step.n = s.next_n;
        s.next_n += 1;
        if step.text.len() > MAX_TEXT {
            let mut cut = MAX_TEXT;
            while !step.text.is_char_boundary(cut) {
                cut -= 1;
            }
            step.text.truncate(cut);
            step.text.push_str("\n…");
        }
        step.image = image.is_some();
        if let Some(img) = image {
            s.images.push_back((step.n, img));
            if s.images.len() > MAX_IMAGES {
                s.images.pop_front();
            }
        }
        let n = step.n;
        s.steps.push_back(step);
        if s.steps.len() > MAX_STEPS {
            s.steps.pop_front();
        }
        s.version += 1;
        drop(s);
        self.inner.changed.notify_all();
        n
    }

    /// Waits while the person has paused the session; `note` is told once that it waits.
    pub fn wait_if_paused(&self, note: &mut dyn FnMut(&str)) {
        let mut s = self.inner.lock();
        if !s.paused {
            return;
        }
        note("paused by the person watching; waiting for them to resume");
        while s.paused {
            s = self
                .inner
                .changed
                .wait(s)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// The approver for the session's policy: a held write waits for the person's decision
    /// (or a grant they gave), at most [`DECISION_TIMEOUT`].
    pub fn approver(&self) -> impl Fn(&Effect) -> bool + 'static {
        let viewer = self.clone();
        move |effect| viewer.decide(effect, DECISION_TIMEOUT)
    }

    #[allow(clippy::disallowed_methods)] // a person decides in wall time
    fn decide(&self, effect: &Effect, timeout: Duration) -> bool {
        let mut s = self.inner.lock();
        if s.grants
            .contains(&(effect.method.clone(), effect.origin.clone()))
        {
            return true;
        }
        let id = s.next_id;
        s.next_id += 1;
        s.approvals.push_back(Approval {
            id,
            effect: effect.clone(),
            state: Decision::Waiting,
        });
        if s.approvals.len() > MAX_STEPS {
            s.approvals.pop_front();
        }
        s.version += 1;
        self.inner.changed.notify_all();
        let until = Instant::now() + timeout;
        loop {
            let state = s
                .approvals
                .iter()
                .find(|a| a.id == id)
                .map_or(Decision::Expired, |a| a.state);
            match state {
                Decision::Approved | Decision::Granted => return true,
                Decision::Denied | Decision::Expired => return false,
                Decision::Waiting => {}
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                if let Some(a) = s.approvals.iter_mut().find(|a| a.id == id) {
                    a.state = Decision::Expired;
                }
                s.version += 1;
                self.inner.changed.notify_all();
                return false;
            }
            s = self
                .inner
                .changed
                .wait_timeout(s, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn handle(&self, stream: &TcpStream) -> Result<(), String> {
        let req = read_request(stream)?;
        let own = |h: &str| {
            h == format!("127.0.0.1:{}", self.port) || h == format!("localhost:{}", self.port)
        };
        if !req.header("host").is_some_and(own) {
            plain(stream, "400 Bad Request", "wrong Host");
            return Ok(());
        }
        let Some(rest) = req
            .path
            .split('?')
            .next()
            .and_then(|p| p.strip_prefix(self.path.as_str()))
            .map(str::to_string)
        else {
            plain(stream, "404 Not Found", "no such viewer");
            return Ok(());
        };
        let cookie = self.lock().cookie.clone();
        let is_viewer = cookie
            .as_deref()
            .is_some_and(|c| req.cookie("pageon_viewer") == Some(c));
        if req.method == "GET" && (rest.is_empty() || rest == "/") {
            return self.page(stream, cookie.is_some(), is_viewer);
        }
        if !is_viewer {
            plain(stream, "409 Conflict", "open the viewer page first");
            return Ok(());
        }
        match (req.method.as_str(), rest.as_str()) {
            ("GET", "/app.js") => respond(
                stream,
                "200 OK",
                "text/javascript; charset=utf-8",
                "",
                JS.as_bytes(),
            ),
            ("GET", "/app.css") => respond(
                stream,
                "200 OK",
                "text/css; charset=utf-8",
                "",
                CSS.as_bytes(),
            ),
            ("GET", "/events") => return self.events(stream),
            ("GET", shot) if shot.starts_with("/shot/") => {
                let n: Option<u64> = shot
                    .strip_prefix("/shot/")
                    .and_then(|s| s.strip_suffix(".jpg"))
                    .and_then(|s| s.parse().ok());
                let img = n.and_then(|n| {
                    self.lock()
                        .images
                        .iter()
                        .find(|(m, _)| *m == n)
                        .map(|(_, b)| b.clone())
                });
                match img {
                    Some(b) => respond(
                        stream,
                        "200 OK",
                        "image/jpeg",
                        "Cache-Control: private, max-age=3600\r\n",
                        &b,
                    ),
                    None => plain(stream, "404 Not Found", "no image for this step"),
                }
            }
            ("POST", w) => {
                if let Some(why) = self.refuse_write(&req) {
                    plain(stream, "403 Forbidden", why);
                    return Ok(());
                }
                match w {
                    "/pause" | "/resume" => {
                        let mut s = self.lock();
                        s.paused = w == "/pause";
                        s.version += 1;
                        drop(s);
                        self.changed.notify_all();
                        respond(stream, "204 No Content", "text/plain", "", b"");
                    }
                    "/decide" => self.decide(stream, &req),
                    _ => plain(stream, "404 Not Found", "no such route"),
                }
            }
            _ => plain(stream, "404 Not Found", "no such route"),
        }
        Ok(())
    }

    /// Why a write is refused: it must carry the page token and come from the page.
    fn refuse_write(&self, req: &Request) -> Option<&'static str> {
        if req.header("x-pageon-token") != Some(self.page_token.as_str()) {
            return Some("missing or wrong page token");
        }
        if let Some(o) = req.header("origin")
            && o != format!("http://127.0.0.1:{}", self.port)
            && o != format!("http://localhost:{}", self.port)
        {
            return Some("foreign Origin");
        }
        None
    }

    fn page(&self, stream: &TcpStream, opened: bool, is_viewer: bool) -> Result<(), String> {
        if opened && !is_viewer {
            plain(
                stream,
                "409 Conflict",
                "another browser is viewing this session",
            );
            return Ok(());
        }
        let mut extra = String::new();
        if !opened {
            let c = token()?;
            extra = format!(
                "Set-Cookie: pageon_viewer={c}; Path={}; HttpOnly; SameSite=Strict\r\n",
                self.path
            );
            self.lock().cookie = Some(c);
        }
        extra.push_str("Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n");
        let html = HTML
            .replace("{{base}}", &self.path)
            .replace("{{token}}", &self.page_token);
        respond(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            &extra,
            html.as_bytes(),
        );
        Ok(())
    }

    fn decide(&self, stream: &TcpStream, req: &Request) {
        let v: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = v["id"].as_u64();
        let verdict = match v["verdict"].as_str() {
            Some("once") => Decision::Approved,
            Some("session") => Decision::Granted,
            Some("deny") => Decision::Denied,
            _ => {
                plain(
                    stream,
                    "400 Bad Request",
                    "send {id, verdict: once|session|deny}",
                );
                return;
            }
        };
        let mut s = self.lock();
        let Some(a) = s
            .approvals
            .iter_mut()
            .find(|a| Some(a.id) == id && a.state == Decision::Waiting)
        else {
            drop(s);
            plain(stream, "404 Not Found", "no such waiting write");
            return;
        };
        a.state = verdict;
        let grant = (a.effect.method.clone(), a.effect.origin.clone());
        if verdict == Decision::Granted {
            s.grants.insert(grant.clone());
            // Other writes waiting under the same grant go too.
            for other in s.approvals.iter_mut() {
                if other.state == Decision::Waiting
                    && (other.effect.method.clone(), other.effect.origin.clone()) == grant
                {
                    other.state = Decision::Granted;
                }
            }
        }
        s.version += 1;
        drop(s);
        self.changed.notify_all();
        respond(stream, "204 No Content", "text/plain", "", b"");
    }

    /// Server-sent events: everything so far, then each change.
    fn events(&self, stream: &TcpStream) -> Result<(), String> {
        let mut out = stream;
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n";
        out.write_all(head.as_bytes()).map_err(|e| e.to_string())?;
        self.lock().watchers += 1;
        let r = self.stream_changes(&mut out);
        self.lock().watchers -= 1;
        self.changed.notify_all();
        r
    }

    fn stream_changes(&self, out: &mut &TcpStream) -> Result<(), String> {
        let mut sent_n: Option<u64> = None;
        let mut sent: std::collections::HashMap<u64, Decision> = std::collections::HashMap::new();
        let mut last_status = Value::Null;
        let mut version = u64::MAX;
        loop {
            let mut buf = String::new();
            {
                let mut s = self.lock();
                while s.version == version {
                    let (g, t) = self
                        .changed
                        .wait_timeout(s, HEARTBEAT)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    s = g;
                    if t.timed_out() && s.version == version {
                        buf.push_str(": keep-alive\n\n");
                        break;
                    }
                }
                version = s.version;
                let after = sent_n;
                for st in s.steps.iter().filter(|st| after.is_none_or(|n| st.n > n)) {
                    buf.push_str(&event("step", &json!(st)));
                    sent_n = Some(st.n);
                }
                for a in &s.approvals {
                    if sent.get(&a.id) != Some(&a.state) {
                        buf.push_str(&event("approval", &json!(a)));
                        sent.insert(a.id, a.state);
                    }
                }
                let status = json!({
                    "paused": s.paused,
                    "waiting": s.approvals.iter().filter(|a| a.state == Decision::Waiting).count(),
                    "grants": s.grants.iter().map(|(m, o)| format!("{m} {o}")).collect::<Vec<_>>(),
                    "steps": s.next_n,
                });
                if status != last_status {
                    buf.push_str(&event("status", &status));
                    last_status = status;
                }
            }
            if !buf.is_empty() {
                out.write_all(buf.as_bytes()).map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
            }
        }
    }
}

fn event(kind: &str, data: &Value) -> String {
    format!("event: {kind}\ndata: {data}\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(text: &str) -> Step {
        Step {
            n: 0,
            step: None,
            tool: "browser_find".into(),
            input: Value::Null,
            ok: true,
            text: text.into(),
            url: None,
            ms: 0,
            tokens: 0,
            image: false,
        }
    }

    #[test]
    fn steps_images_and_text_are_bounded() {
        let v = Viewer::start().unwrap_or_else(|e| panic!("{e}"));
        let long = "x".repeat(MAX_TEXT * 2);
        for i in 0..2000 {
            v.record(
                step(if i == 1999 { &long } else { "ok" }),
                Some(vec![0; 10]),
            );
        }
        let s = v.inner.lock();
        assert_eq!(s.steps.len(), MAX_STEPS);
        assert_eq!(s.images.len(), MAX_IMAGES);
        assert_eq!(s.steps.front().map(|s| s.n), Some(1000));
        assert!(s.steps.back().is_some_and(|s| s.text.len() <= MAX_TEXT + 4));
    }

    #[test]
    fn connections_are_bounded() {
        let v = Viewer::start().unwrap_or_else(|e| panic!("{e}"));
        let port = v.inner.port;
        // Connections that send nothing hold a slot until their 5 s deadline.
        let held: Vec<TcpStream> = (0..MAX_CONNECTIONS)
            .filter_map(|_| TcpStream::connect(("127.0.0.1", port)).ok())
            .collect();
        std::thread::sleep(Duration::from_millis(200));
        let mut extra = TcpStream::connect(("127.0.0.1", port)).unwrap_or_else(|e| panic!("{e}"));
        let _ = extra.set_read_timeout(Some(Duration::from_secs(5)));
        let mut answer = String::new();
        let _ = std::io::Read::read_to_string(&mut extra, &mut answer);
        assert!(answer.starts_with("HTTP/1.1 503"), "{answer}");
        drop(held);
    }

    #[test]
    fn grants_cover_one_method_and_origin() {
        let v = Viewer::start().unwrap_or_else(|e| panic!("{e}"));
        let effect = |method: &str, origin: &str| Effect {
            method: method.into(),
            url: format!("{origin}/x"),
            origin: origin.into(),
            class: crate::EffectClass::Write,
            labels: Vec::new(),
            cause: None,
        };
        v.inner
            .lock()
            .grants
            .insert(("POST".into(), "https://a.test".into()));
        assert!(v.decide(&effect("POST", "https://a.test"), Duration::from_millis(10)));
        // Another method or origin still asks; nobody answers, so it expires, refused.
        assert!(!v.decide(
            &effect("DELETE", "https://a.test"),
            Duration::from_millis(10)
        ));
        assert!(!v.decide(&effect("POST", "https://b.test"), Duration::from_millis(10)));
        let s = v.inner.lock();
        assert!(s.approvals.iter().all(|a| a.state == Decision::Expired));
    }
}
