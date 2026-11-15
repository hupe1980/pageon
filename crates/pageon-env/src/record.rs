//! Recording: a live environment that keeps a log of everything it served.

use std::cell::RefCell;
use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::steps::{Cursor, Divergence, Kind, Recorder, Step};
use crate::{Env, NetError, Request, Response, user_agent};

/// One recorded exchange.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exchange {
    /// Method.
    pub method: String,
    /// URL.
    pub url: String,
    /// Status.
    pub status: u16,
    /// Headers (lower-case names, arrival order).
    pub headers: Vec<(String, String)>,
    /// Body, when it is valid UTF-8.
    pub body: Option<String>,
    /// Body, base64, when it is not valid UTF-8.
    pub body_base64: Option<String>,
    /// The error, if the exchange failed (replayed as the same error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The request body, when it is valid UTF-8 (absent when empty).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    /// The request body, base64, when it is not valid UTF-8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body_base64: Option<String>,
}

fn text_or_base64(bytes: &[u8]) -> (Option<String>, Option<String>) {
    if bytes.is_empty() {
        return (None, None);
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (Some(s.to_string()), None),
        Err(_) => (
            None,
            Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        ),
    }
}

impl Exchange {
    fn from_response(req: &Request, r: &Response) -> Self {
        let (body, body_base64) = match std::str::from_utf8(&r.body) {
            Ok(s) => (Some(s.to_string()), None),
            Err(_) => (
                None,
                Some(base64::engine::general_purpose::STANDARD.encode(&r.body)),
            ),
        };
        let (request_body, request_body_base64) = text_or_base64(&req.body);
        Self {
            method: req.method.clone(),
            url: req.url.clone(),
            status: r.status,
            headers: r.headers.clone(),
            body,
            body_base64,
            error: None,
            request_body,
            request_body_base64,
        }
    }

    /// The response this exchange recorded.
    pub fn response(&self) -> Result<Response, NetError> {
        if let Some(e) = &self.error {
            return Err(if let Some(what) = e.strip_prefix("blocked: ") {
                NetError::Blocked(what.to_string())
            } else if let Some(what) = e.strip_prefix("no recorded ") {
                // A miss of the recording it was made from (robots.txt of an older
                // recording) stays a miss when served again.
                NetError::ReplayMiss(what.to_string())
            } else {
                NetError::Network(e.clone())
            });
        }
        let body = match (&self.body, &self.body_base64) {
            (Some(s), _) => s.as_bytes().to_vec(),
            (None, Some(b)) => base64::engine::general_purpose::STANDARD
                .decode(b)
                .map_err(|e| {
                    NetError::ReplayMiss(format!("valid body for {} (bad base64: {e})", self.url))
                })?,
            (None, None) => Vec::new(),
        };
        Ok(Response {
            status: self.status,
            headers: self.headers.clone(),
            body,
        })
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's signature
fn is_agent(p: &crate::Profile) -> bool {
    *p == crate::Profile::Agent
}

/// A recording file (version 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Log {
    /// Format version (1).
    pub version: u32,
    /// The user agent the recording was made with.
    pub user_agent: String,
    /// The identity profile it was made with (absent: `agent`).
    #[serde(default, skip_serializing_if = "is_agent")]
    pub profile: crate::Profile,
    /// Exchanges in order.
    pub exchanges: Vec<Exchange>,
    /// Clock readings in order (ms since the Unix epoch).
    pub clock: Vec<u64>,
    /// Random draws in order.
    pub random: Vec<u64>,
    /// The agent's steps: what it did and what the page was after each step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    /// Realtime connections (WebSocket, event streams): opens, messages with the virtual time
    /// they were delivered at, and sent frames, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub realtime: Vec<crate::Rt>,
    /// The recording stopped at its body limit: what came after is not in it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

impl Log {
    /// The log as it was when step `k` ended (streams and steps cut there).
    pub fn cut(&self, k: usize) -> Option<Log> {
        let at = self.steps.get(k)?.at;
        let mut l = self.clone();
        l.exchanges.truncate(at.exchanges);
        l.clock.truncate(at.clock);
        l.random.truncate(at.random);
        l.realtime.truncate(at.realtime);
        l.steps.truncate(k + 1);
        Some(l)
    }
}

impl Default for Log {
    fn default() -> Self {
        Self {
            version: 1,
            user_agent: user_agent(),
            profile: crate::Profile::Agent,
            exchanges: Vec::new(),
            clock: Vec::new(),
            random: Vec::new(),
            steps: Vec::new(),
            realtime: Vec::new(),
            truncated: false,
        }
    }
}

/// Wraps an environment and records everything it serves.
///
/// It can also **resume** a recording: the steps of a cut log are re-run, served
/// from it (no egress) or from `inner`, each checked against the recorded one; then it records
/// on. This one mechanism gives replay validation, `--live` divergence checks and reset.
pub struct Recording<E: Env> {
    inner: E,
    log: RefCell<Log>,
    redact: RefCell<Vec<String>>,
    resume: RefCell<Option<Resume>>,
    divergence: RefCell<Option<Divergence>>,
    /// While a lookahead runs: pass everything through, keep nothing.
    paused: std::cell::Cell<bool>,
    /// Most bytes of response bodies to keep (`usize::MAX`: no limit).
    body_limit: std::cell::Cell<usize>,
    body_bytes: std::cell::Cell<usize>,
    /// Over the body limit: nothing more is kept, for good (unlike `paused`).
    full: std::cell::Cell<bool>,
}

/// Re-running recorded steps.
struct Resume {
    /// Serve from the log (no egress), or ask `inner` (a live check).
    serve: bool,
    /// The steps to check, in order.
    expected: Vec<Step>,
    checked: usize,
    used: Vec<bool>,
    clock: usize,
    random: usize,
    realtime: crate::realtime::RtServe,
}

/// `Set-Cookie: sid=abc; Path=/` → `sid=pageon-redacted; Path=/`: the cookie and its scope
/// stay (a replayed page still has it), the value does not reach the disk.
fn redact_cookie(v: &str) -> String {
    let (pair, attrs) = v.split_once(';').map_or((v, ""), |(p, a)| (p, a));
    let name = pair.split_once('=').map_or(pair, |(n, _)| n).trim();
    if attrs.is_empty() {
        format!("{name}=pageon-redacted")
    } else {
        format!("{name}=pageon-redacted;{attrs}")
    }
}

impl<E: Env> Recording<E> {
    /// Starts recording.
    pub fn new(inner: E) -> Self {
        let profile = inner.profile();
        Self {
            inner,
            log: RefCell::new(Log {
                user_agent: profile.user_agent(),
                profile,
                ..Log::default()
            }),
            redact: RefCell::new(Vec::new()),
            resume: RefCell::new(None),
            paused: std::cell::Cell::new(false),
            divergence: RefCell::new(None),
            body_limit: std::cell::Cell::new(usize::MAX),
            body_bytes: std::cell::Cell::new(0),
            full: std::cell::Cell::new(false),
        }
    }

    /// Keeps at most `bytes` of response bodies; past that, the recording stops for good
    /// (steps and exchanges after it are not kept) and [`Log::truncated`] is set.
    pub fn with_body_limit(self, bytes: usize) -> Self {
        self.body_limit.set(bytes);
        self
    }

    /// Whether the recording stopped at its body limit.
    pub fn truncated(&self) -> bool {
        self.full.get()
    }

    /// Nothing is kept now (a lookahead, or over the body limit).
    fn off(&self) -> bool {
        self.paused.get() || self.full.get()
    }

    /// Resumes `log`: its steps `0..=upto` are re-run and checked, served from the log when
    /// `serve` (no egress; a request it lacks is refused) or from `inner` (a live check); then
    /// it records on.
    pub fn resume(log: Log, upto: usize, inner: E, serve: bool) -> Result<Self, String> {
        let r = Self::new(inner);
        *r.log.borrow_mut() = log;
        r.start(upto, serve)?;
        Ok(r)
    }

    fn start(&self, k: usize, serve: bool) -> Result<(), String> {
        let n = self.log.borrow().steps.len();
        let cut = self.log.borrow().cut(k).ok_or_else(|| match n {
            0 => "the recording has no steps".to_string(),
            n => format!("step {k} does not exist (steps 0 to {})", n - 1),
        })?;
        *self.resume.borrow_mut() = Some(Resume {
            serve,
            expected: cut.steps.clone(),
            checked: 0,
            used: vec![false; cut.exchanges.len()],
            clock: 0,
            random: 0,
            realtime: crate::realtime::RtServe::default(),
        });
        *self.log.borrow_mut() = cut;
        *self.divergence.borrow_mut() = None;
        Ok(())
    }

    /// Ends a resume early (after a divergence): the steps checked so far stay, and it records
    /// on from there.
    pub fn cancel(&self) {
        let Some(r) = self.resume.borrow_mut().take() else {
            return;
        };
        let mut log = self.log.borrow_mut();
        *log = match r.checked {
            0 => Log {
                user_agent: log.user_agent.clone(),
                profile: log.profile,
                ..Log::default()
            },
            n => log.cut(n - 1).unwrap_or_default(),
        };
    }

    fn diverge(&self, d: Divergence) {
        let mut slot = self.divergence.borrow_mut();
        if slot.is_none() {
            *slot = Some(d);
        }
    }

    /// The step being re-run and its recorded action, while resuming.
    fn current(&self) -> (usize, String) {
        self.resume
            .borrow()
            .as_ref()
            .map_or((0, String::new()), |r| {
                let i = r.checked.min(r.expected.len().saturating_sub(1));
                (
                    r.checked,
                    r.expected
                        .get(i)
                        .map(|s| s.action.clone())
                        .unwrap_or_default(),
                )
            })
    }

    /// Values (secrets) to replace with `[redacted]` wherever they occur in the log: request
    /// and response bodies (text), headers and URLs. Applies to [`Recording::log`] and
    /// [`Recording::save`].
    pub fn redact(&self, values: impl IntoIterator<Item = String>) {
        self.redact
            .borrow_mut()
            .extend(values.into_iter().filter(|v| !v.is_empty()));
    }

    /// The environment inside.
    pub fn inner(&self) -> &E {
        &self.inner
    }

    /// The log so far (redacted: secret values, credential URL parameters, and the value of
    /// every `HttpOnly` cookie a server set).
    pub fn log(&self) -> Log {
        let mut log = self.log.borrow().clone();
        for ex in &mut log.exchanges {
            for (k, v) in &mut ex.headers {
                // HttpOnly values: scripts never read them, so replays are unchanged.
                if k.eq_ignore_ascii_case("set-cookie")
                    && v.split(';')
                        .skip(1)
                        .any(|a| a.trim().eq_ignore_ascii_case("httponly"))
                {
                    *v = redact_cookie(v);
                }
            }
        }
        let values = self.redact.borrow();
        let secrets = |s: &mut String| {
            for v in values.iter() {
                if s.contains(v.as_str()) {
                    *s = s.replace(v.as_str(), "[redacted]");
                }
            }
        };
        let clean = |s: &mut String| {
            secrets(s);
            if let std::borrow::Cow::Owned(r) = crate::redact_params(s) {
                *s = r;
            }
        };
        for step in &mut log.steps {
            clean(&mut step.action);
            let original = step.input.to_string();
            let mut input = original.clone();
            clean(&mut input);
            if input != original
                && let Ok(v) = serde_json::from_str(&input)
            {
                step.input = v;
            }
        }
        for e in &mut log.realtime {
            match e {
                crate::Rt::Sent { text: Some(t), .. }
                | crate::Rt::Event {
                    event: crate::StreamEvent::Text(t),
                    ..
                } => clean(t),
                crate::Rt::Open { url, .. } => clean(url),
                _ => {}
            }
        }
        for ex in &mut log.exchanges {
            clean(&mut ex.url);
            for (k, h) in &mut ex.headers {
                // Cookies are not URL parameters: scripts read them back as they were.
                if k.eq_ignore_ascii_case("set-cookie") || k.eq_ignore_ascii_case("cookie") {
                    secrets(h);
                } else {
                    clean(h);
                }
            }
            for b in [&mut ex.body, &mut ex.request_body, &mut ex.error]
                .into_iter()
                .flatten()
            {
                clean(b);
            }
        }
        log
    }

    /// Writes the recording as pretty JSON.
    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&self.log()).map_err(std::io::Error::other)?;
        crate::write_private(path, (text + "\n").as_bytes())
    }
}

impl<E: Env> Env for Recording<E> {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        if self.off() {
            return self.inner.fetch(req);
        }
        let serving = self.resume.borrow().as_ref().map(|r| r.serve);
        match serving {
            Some(true) => {
                let found = {
                    let mut guard = self.resume.borrow_mut();
                    let r = guard.as_mut().ok_or(NetError::ReplayMiss(String::new()))?;
                    let log = self.log.borrow();
                    let i = log.exchanges.iter().enumerate().position(|(i, e)| {
                        !r.used[i]
                            && e.method == req.method
                            && crate::redact::same_url(&e.url, &req.url)
                    });
                    i.map(|i| {
                        r.used[i] = true;
                        log.exchanges[i].response()
                    })
                };
                return found.unwrap_or_else(|| {
                    let (step, action) = self.current();
                    self.diverge(Divergence {
                        step,
                        action,
                        kind: Kind::Request,
                        detail: format!(
                            "the page requested {} {}, which the recording does not have",
                            req.method, req.url
                        ),
                        expected: None,
                        actual: None,
                    });
                    Err(NetError::ReplayMiss(format!(
                        "response for {} {} (not in the recording)",
                        req.method, req.url
                    )))
                });
            }
            Some(false) => return self.inner.fetch(req),
            None => {}
        }
        let r = self.inner.fetch(req);
        let ex = match &r {
            Ok(resp) => Exchange::from_response(req, resp),
            Err(e) => {
                let (request_body, request_body_base64) = text_or_base64(&req.body);
                Exchange {
                    method: req.method.clone(),
                    url: req.url.clone(),
                    status: 0,
                    headers: Vec::new(),
                    body: None,
                    body_base64: None,
                    error: Some(e.to_string()),
                    request_body,
                    request_body_base64,
                }
            }
        };
        let bytes = ex.body.as_ref().map_or(0, String::len)
            + ex.body_base64.as_ref().map_or(0, String::len);
        let total = self.body_bytes.get().saturating_add(bytes);
        if total > self.body_limit.get() {
            self.full.set(true);
            self.log.borrow_mut().truncated = true;
            return r;
        }
        self.body_bytes.set(total);
        self.log.borrow_mut().exchanges.push(ex);
        r
    }

    fn stream(&self, call: &crate::StreamCall) -> Result<crate::StreamReply, NetError> {
        use crate::{Rt, StreamCall, StreamReply};
        if self.off() {
            return self.inner.stream(call);
        }
        let serving = self.resume.borrow().as_ref().map(|r| r.serve);
        match serving {
            Some(true) => {
                let mut guard = self.resume.borrow_mut();
                let r = guard.as_mut().ok_or(NetError::ReplayMiss(String::new()))?;
                let log = self.log.borrow();
                return r.realtime.call(&log.realtime, call);
            }
            Some(false) => return self.inner.stream(call),
            None => {}
        }
        let reply = self.inner.stream(call);
        let mut entries = Vec::new();
        match (call, &reply) {
            (
                StreamCall::Open { req, .. },
                Ok(StreamReply::Opened {
                    id,
                    status,
                    headers,
                }),
            ) => {
                entries.push(Rt::Open {
                    id: *id,
                    url: req.url.clone(),
                    status: *status,
                    headers: headers.clone(),
                    error: None,
                });
            }
            (StreamCall::Send { id, req, .. }, Ok(_)) => entries.push(Rt::sent(*id, &req.body)),
            (StreamCall::Close { id, code, .. }, Ok(_)) => entries.push(Rt::Close {
                id: *id,
                code: *code,
            }),
            (StreamCall::Poll { now, .. }, Ok(StreamReply::Events(events))) => {
                for (id, event) in events {
                    entries.push(Rt::Event {
                        id: *id,
                        at: *now,
                        event: event.clone(),
                    });
                }
            }
            _ => {}
        }
        let bytes: usize = entries
            .iter()
            .map(|e| match e {
                Rt::Event {
                    event: crate::StreamEvent::Text(t),
                    ..
                } => t.len(),
                Rt::Event {
                    event: crate::StreamEvent::Binary(b),
                    ..
                } => b.len(),
                Rt::Sent { text, base64, .. } => {
                    text.as_ref().map_or(0, String::len) + base64.as_ref().map_or(0, String::len)
                }
                _ => 0,
            })
            .sum();
        let total = self.body_bytes.get().saturating_add(bytes);
        if total > self.body_limit.get() {
            self.full.set(true);
            self.log.borrow_mut().truncated = true;
            return reply;
        }
        self.body_bytes.set(total);
        self.log.borrow_mut().realtime.extend(entries);
        reply
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        if self.off() {
            return self.inner.now_ms();
        }
        if let Some(r) = self.resume.borrow_mut().as_mut() {
            if !r.serve {
                return self.inner.now_ms();
            }
            if let Some(&t) = self.log.borrow().clock.get(r.clock) {
                r.clock += 1;
                return Ok(t);
            }
        }
        let t = self.inner.now_ms()?;
        self.log.borrow_mut().clock.push(t);
        Ok(t)
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        if self.off() {
            return self.inner.random_u64();
        }
        if let Some(r) = self.resume.borrow_mut().as_mut() {
            if !r.serve {
                return self.inner.random_u64();
            }
            if let Some(&v) = self.log.borrow().random.get(r.random) {
                r.random += 1;
                return Ok(v);
            }
        }
        let v = self.inner.random_u64()?;
        self.log.borrow_mut().random.push(v);
        Ok(v)
    }

    fn recorder(&self) -> Option<&dyn Recorder> {
        Some(self)
    }

    fn profile(&self) -> crate::Profile {
        self.log.borrow().profile
    }

    /// Secrets pass through and are never recorded.
    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        self.inner.secret(name, origin)
    }

    /// Passkey operations pass through and are never recorded.
    fn passkey(&self, call: &crate::PasskeyCall) -> Result<crate::PasskeyReply, NetError> {
        self.inner.passkey(call)
    }

    /// Staged files pass through and are never recorded (a replay stages them again).
    fn file(&self, name: &str, origin: &str) -> Result<crate::StagedFile, NetError> {
        self.inner.file(name, origin)
    }
}

impl<E: Env> Recorder for Recording<E> {
    fn pause(&self, paused: bool) {
        self.paused.set(paused);
    }

    fn truncated(&self) -> bool {
        self.full.get()
    }

    fn step(&self, mut step: Step) {
        if self.off() {
            return;
        }
        let mut resume = self.resume.borrow_mut();
        if let Some(r) = resume.as_mut() {
            let i = r.checked;
            if let Some(want) = r.expected.get(i) {
                let d = if want.action != step.action {
                    Some((
                        Kind::Action,
                        format!(
                            "the recording did `{}` here, the re-run did `{}`",
                            want.action, step.action
                        ),
                    ))
                } else if want.hash != step.hash {
                    Some((Kind::Page, "the page differs".to_string()))
                } else {
                    None
                };
                if let Some((kind, detail)) = d {
                    let page = kind == Kind::Page;
                    let d = Divergence {
                        step: i,
                        action: want.action.clone(),
                        kind,
                        detail,
                        expected: page.then(|| want.view.clone()),
                        actual: page.then(|| step.view.clone()),
                    };
                    drop(resume);
                    self.diverge(d);
                    resume = self.resume.borrow_mut();
                }
            }
            if let Some(r) = resume.as_mut() {
                r.checked += 1;
                if r.checked >= r.expected.len() {
                    *resume = None; // all re-run: record on
                }
            }
            return;
        }
        drop(resume);
        let mut log = self.log.borrow_mut();
        step.at = Cursor {
            exchanges: log.exchanges.len(),
            clock: log.clock.len(),
            random: log.random.len(),
            realtime: log.realtime.len(),
        };
        log.steps.push(step);
    }

    fn steps(&self) -> Vec<Step> {
        self.log.borrow().steps.clone()
    }

    fn len(&self) -> usize {
        self.log.borrow().steps.len()
    }

    fn rewind(&self, k: usize) -> Result<(), String> {
        self.cancel();
        self.start(k, true)
    }

    fn cancel(&self) {
        Recording::cancel(self);
    }

    fn divergence(&self) -> Option<Divergence> {
        self.divergence.borrow().clone()
    }

    fn replaying(&self) -> bool {
        self.resume.borrow().as_ref().is_some_and(|r| r.serve)
    }
}

#[cfg(test)]
mod steps_tests {
    use super::*;
    use crate::Replay;

    fn site() -> Log {
        let page = |url: &str, body: &str| Exchange {
            method: "GET".into(),
            url: url.into(),
            status: 200,
            body: Some(body.into()),
            ..Exchange::default()
        };
        Log {
            exchanges: vec![
                page("https://a.test/", "one"),
                page("https://a.test/2", "two"),
            ],
            clock: (0..10).collect(),
            random: (0..10).collect(),
            ..Log::default()
        }
    }

    fn step(action: &str, hash: &str) -> Step {
        Step {
            action: action.into(),
            hash: hash.into(),
            view: format!("view {hash}"),
            ..Step::default()
        }
    }

    /// Records a two-step episode over a replay of `site()`.
    fn recorded() -> Log {
        let rec = Recording::new(Replay::new(site()));
        rec.fetch(&Request::get("https://a.test/")).unwrap();
        rec.now_ms().unwrap();
        rec.step(step("open https://a.test/", "h0"));
        rec.fetch(&Request::get("https://a.test/2")).unwrap();
        rec.random_u64().unwrap();
        rec.step(step("click e1", "h1"));
        rec.log()
    }

    #[test]
    fn steps_mark_the_streams() {
        let log = recorded();
        assert_eq!(log.steps.len(), 2);
        assert_eq!(
            log.steps[0].at,
            Cursor {
                exchanges: 1,
                clock: 1,
                random: 0,
                realtime: 0
            }
        );
        assert_eq!(
            log.steps[1].at,
            Cursor {
                exchanges: 2,
                clock: 1,
                random: 1,
                realtime: 0
            }
        );
        // Old files (no steps) still load.
        let old: Log = serde_json::from_str(
            r#"{"version":1,"user_agent":"x","exchanges":[],"clock":[],"random":[]}"#,
        )
        .unwrap();
        assert!(old.steps.is_empty());
    }

    struct Nothing;
    impl Env for Nothing {
        fn fetch(&self, _: &Request) -> Result<Response, NetError> {
            panic!("no egress while serving")
        }
        fn now_ms(&self) -> Result<u64, NetError> {
            Ok(1000)
        }
        fn random_u64(&self) -> Result<u64, NetError> {
            Ok(7)
        }
    }

    #[test]
    fn resume_serves_checks_then_records() {
        let r = Recording::resume(recorded(), 1, Nothing, true).unwrap();
        assert!(r.replaying());
        assert_eq!(
            r.fetch(&Request::get("https://a.test/")).unwrap().body,
            b"one"
        );
        assert_eq!(r.now_ms().unwrap(), 0);
        r.step(step("open https://a.test/", "h0"));
        r.fetch(&Request::get("https://a.test/2")).unwrap();
        r.step(step("click e1", "h1"));
        assert!(r.divergence().is_none());
        assert!(!r.replaying(), "all steps re-run: recording on");
        assert_eq!(r.now_ms().unwrap(), 1000, "live after the last step");
        r.step(step("click e2", "h2"));
        assert_eq!(r.len(), 3);
    }

    #[test]
    fn a_paused_recording_keeps_nothing() {
        let r = Recording::new(Nothing);
        r.now_ms().unwrap();
        let before = r.log();
        r.pause(true);
        assert_eq!(r.now_ms().unwrap(), 1000);
        assert_eq!(r.random_u64().unwrap(), 7);
        r.step(step("click e1", "h1"));
        r.pause(false);
        assert_eq!(r.log(), before, "a lookahead leaves no trace");
        r.random_u64().unwrap();
        assert_eq!(r.log().random, [7]);
    }

    #[test]
    fn divergences_name_step_and_kind() {
        // A different page.
        let r = Recording::resume(recorded(), 1, Nothing, true).unwrap();
        r.step(step("open https://a.test/", "XX"));
        let d = r.divergence().unwrap();
        assert_eq!((d.step, d.kind), (0, Kind::Page));
        assert_eq!(d.diff(), ["- view h0", "+ view XX"]);
        // A request the recording lacks.
        let r = Recording::resume(recorded(), 1, Nothing, true).unwrap();
        r.step(step("open https://a.test/", "h0"));
        assert!(r.fetch(&Request::get("https://a.test/new")).is_err());
        let d = r.divergence().unwrap();
        assert_eq!((d.step, d.kind), (1, Kind::Request));
        assert!(d.detail.contains("GET https://a.test/new"), "{d}");
        // A different action.
        let r = Recording::resume(recorded(), 1, Nothing, true).unwrap();
        r.step(step("open https://a.test/", "h0"));
        r.step(step("click e9", "h1"));
        assert_eq!(r.divergence().unwrap().kind, Kind::Action);
    }

    #[test]
    fn rewind_cuts_and_cancel_keeps_checked_steps() {
        let r = Recording::new(Replay::new(site()));
        *r.log.borrow_mut() = recorded();
        r.rewind(0).unwrap();
        assert_eq!(r.len(), 1, "later steps are gone");
        assert_eq!(r.log().exchanges.len(), 1);
        r.fetch(&Request::get("https://a.test/")).unwrap();
        r.step(step("open https://a.test/", "h0"));
        assert!(!r.replaying());
        assert!(r.rewind(5).unwrap_err().contains("steps 0 to 0"));
        // A failed resume, cancelled: back to recording.
        let r = Recording::resume(recorded(), 1, Replay::new(site()), true).unwrap();
        r.step(step("open https://a.test/", "h0"));
        r.cancel();
        assert!(!r.replaying());
        assert_eq!(r.len(), 1);
    }

    /// P-08: cookie values a server set do not reach a saved recording; the cookie,
    /// its name and attributes do.
    #[test]
    fn cookie_values_are_not_recorded() {
        let mut log = site();
        log.exchanges[0].headers.push((
            "set-cookie".into(),
            "sid=s3cr3t-session; Path=/; HttpOnly".into(),
        ));
        log.exchanges[0]
            .headers
            .push(("Set-Cookie".into(), "theme=dark".into()));
        log.exchanges[0]
            .headers
            .push(("Set-Cookie".into(), "tok=abc; HTTPONLY".into()));
        let rec = Recording::new(crate::Replay::new(log));
        let _ = rec.fetch(&Request::get("https://a.test/"));
        let saved = serde_json::to_string(&rec.log()).unwrap_or_default();
        assert!(!saved.contains("s3cr3t-session"), "{saved}");
        assert!(
            saved.contains("sid=pageon-redacted; Path=/; HttpOnly"),
            "{saved}"
        );
        assert!(
            saved.contains("theme=dark"),
            "script-visible cookies stay: {saved}"
        );
        assert!(saved.contains("tok=pageon-redacted; HTTPONLY"), "{saved}");
    }

    /// An in-memory recording stops for good past its body limit.
    #[test]
    fn a_body_limit_stops_the_recording() {
        let rec = Recording::new(crate::Replay::new(site())).with_body_limit(2);
        let _ = rec.fetch(&Request::get("https://a.test/"));
        assert!(rec.truncated());
        rec.pause(false);
        let _ = rec.fetch(&Request::get("https://a.test/2"));
        let log = rec.log();
        assert!(log.truncated && log.exchanges.is_empty(), "{log:?}");
    }
}
