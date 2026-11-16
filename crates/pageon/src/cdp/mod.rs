//! A Chrome DevTools Protocol session: enough of CDP for Puppeteer, over pageon's
//! tabs. One message in, the messages out (responses and events), synchronously: time is
//! virtual, so nothing happens between messages unless a client is waiting on a promise, and
//! then the page's clock runs ahead to the next due task.
//!
//! It runs where the page runs: in the caged content process by default (the broker only
//! carries messages, see `server.rs`), or in-process.

mod frames;
pub(crate) mod page;
mod runtime;
pub mod server;
mod webmcp;

use std::collections::HashMap;

use pageon_dom::NodeId;
use pageon_env::Env;
use pageon_js::Handle;
use serde_json::{Value as Json, json};

use crate::{LoadOptions, Outcome, Tab};

/// The browser context id every target reports (pageon has one).
const CONTEXT: &str = "9A6E0F0A0C0000000000000000000001";
/// Virtual time a client's awaited promise may run the clock ahead (Puppeteer's default timeout).
const AWAIT_BUDGET_MS: u64 = 30_000;

/// A CDP error.
#[derive(Debug, Clone)]
pub(crate) struct CdpError {
    code: i64,
    message: String,
    data: Option<Json>,
}

impl CdpError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// A server error (`-32000`), as Chrome reports most failures.
    fn server(message: impl Into<String>) -> Self {
        Self::new(-32000, message)
    }

    fn params(detail: impl std::fmt::Display) -> Self {
        Self::new(-32602, format!("Invalid parameters: {detail}"))
    }

    /// The message.
    pub(crate) fn message(&self) -> String {
        self.message.clone()
    }

    /// A method pageon does not have: what to do instead.
    fn unsupported(method: &str, instead: &str) -> Self {
        Self::server(format!("pageon: {method} is not supported: {instead}"))
    }
}

type Reply = Result<Json, CdpError>;

/// An execution context: a document's main world or an isolated world (same V8 context here).
#[derive(Debug, Clone)]
struct Ctx {
    id: i64,
    name: String,
    unique: String,
    /// The frame (pageon's frame index).
    frame: usize,
}

/// A page target.
struct Target<'s, E: Env> {
    id: String,
    session_id: Option<String>,
    /// More sessions on the same page (`createCDPSession`): answered, but events go to the
    /// first session.
    extra_sessions: Vec<String>,
    tab: Tab<'s, E>,
    loader: String,
    /// Bumped per document: object ids of older documents are refused.
    epoch: u64,
    contexts: Vec<Ctx>,
    frames: Vec<frames::FrameInfo>,
    worlds: Vec<String>,
    runtime: bool,
    page: bool,
    network: bool,
    lifecycle: bool,
    objects: HashMap<String, (i64, Handle)>,
    groups: HashMap<String, Vec<String>>,
    next_object: u64,
    bindings: Vec<String>,
    nodes: HashMap<(usize, NodeId), u64>,
    backends: HashMap<u64, (usize, NodeId)>,
    /// Where the mouse button went down, and whether it moved since (a drag).
    pressed: Option<((f64, f64), bool)>,
    /// `WebMCP.enable`d, and the tools last reported: (id, name, frame).
    webmcp: bool,
    tools_seen: Vec<(u64, String, usize)>,
}

/// A call waiting on a promise.
struct Pending {
    id: Json,
    session_id: String,
    target: String,
    epoch: u64,
    ctx: i64,
    promise: Handle,
    ser: Ser,
    group: Option<String>,
    budget_ms: u64,
}

/// How a result is returned: a CDP `RemoteObject` (by value or by reference), or that plus
/// Chrome's `deepSerializedValue` (`serializationOptions: {serialization: "deep"}`).
#[derive(Debug, Clone, Copy)]
pub(crate) enum Ser {
    Cdp { by_value: bool },
    Deep(pageon_js::Depth),
}

impl Ser {
    fn by_value(self) -> bool {
        matches!(self, Ser::Cdp { by_value: true })
    }

    /// From a call's `returnByValue` and `serializationOptions`.
    fn of(params: &Json) -> Result<Self, CdpError> {
        let by_value = params
            .get("returnByValue")
            .and_then(Json::as_bool)
            .unwrap_or(false);
        let Some(o) = params.get("serializationOptions") else {
            return Ok(Ser::Cdp { by_value });
        };
        match o.get("serialization").and_then(Json::as_str) {
            Some("deep") => {
                let depth = |v: Option<&Json>| v.and_then(Json::as_u64).map(|d| d.min(1000) as u32);
                Ok(Ser::Deep(pageon_js::Depth {
                    objects: depth(o.get("maxDepth")),
                    nodes: depth(o.pointer("/additionalParameters/maxNodeDepth")).unwrap_or(0),
                }))
            }
            Some("json") => Ok(Ser::Cdp { by_value: true }),
            Some("idOnly") => Ok(Ser::Cdp { by_value: false }),
            other => Err(CdpError::params(format!(
                "serializationOptions.serialization {other:?}: expected deep, json or idOnly"
            ))),
        }
    }
}

/// One client's CDP session over a pageon [`crate::Session`].
pub struct Session<'s, E: Env> {
    session: &'s crate::Session<E>,
    options: LoadOptions,
    browser_id: String,
    targets: Vec<Target<'s, E>>,
    discover: bool,
    auto_attach: bool,
    counter: u64,
    next_context: i64,
    next_backend: u64,
    next_request: u64,
    out: Vec<String>,
    /// Events that follow the reply to the current command (`WebMCP.invokeTool`).
    after_reply: Vec<String>,
    pending: Vec<Pending>,
    closed: bool,
}

/// The session id of `Target.attachToBrowserTarget` (one browser target, one session).
fn browser_session() -> String {
    hex(0xB5_5500, 1)
}

fn hex(prefix: u64, n: u64) -> String {
    format!("{prefix:08X}{n:024X}")
}

impl<'s, E: Env> Session<'s, E> {
    /// A session with one blank page, like a freshly started browser.
    pub fn new(session: &'s crate::Session<E>, options: LoadOptions) -> Self {
        let mut s = Self {
            session,
            options,
            browser_id: hex(0xB0_0000, 1),
            targets: Vec::new(),
            discover: false,
            auto_attach: false,
            counter: 0,
            next_context: 0,
            next_backend: 0,
            next_request: 0,
            out: Vec::new(),
            after_reply: Vec::new(),
            pending: Vec::new(),
            closed: false,
        };
        let _ = s.create_target("about:blank");
        s.out.clear();
        s
    }

    /// The browser target's id (the WebSocket path is `/devtools/browser/<id>`).
    pub fn browser_id(&self) -> &str {
        &self.browser_id
    }

    /// The target attached on CDP session `sid` (for the WebDriver BiDi mapper).
    pub(crate) fn target_of(&self, sid: &str) -> Option<usize> {
        self.targets.iter().position(|t| {
            t.session_id.as_deref() == Some(sid) || t.extra_sessions.iter().any(|s| s == sid)
        })
    }

    /// The value behind an object id of target `ti`.
    /// The execution context an object id belongs to.
    pub(crate) fn object_context(&self, ti: usize, object_id: &str) -> Option<i64> {
        self.targets
            .get(ti)?
            .objects
            .get(object_id)
            .map(|(c, _)| *c)
    }

    pub(crate) fn handle_of(&self, ti: usize, object_id: &str) -> Option<Handle> {
        self.targets
            .get(ti)?
            .objects
            .get(object_id)
            .map(|(_, h)| *h)
    }

    /// The session's clock (ms since the epoch, virtual).
    pub(crate) fn now_ms(&self) -> u64 {
        self.session.now_ms()
    }

    /// Whether the client closed the browser (`Browser.close`).
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// The pages, as `/json/list` shows them: (id, title, url).
    pub fn pages(&self) -> Vec<(String, String, String)> {
        self.targets
            .iter()
            .map(|t| (t.id.clone(), t.tab.title(), t.tab.url().to_string()))
            .collect()
    }

    /// Handles one client message and returns the messages to send, in order.
    pub fn handle(&mut self, message: &str) -> Vec<String> {
        let msg: Json = match serde_json::from_str(message) {
            Ok(m) => m,
            Err(e) => {
                self.out.push(
                    json!({"id": 0, "error": {"code": -32700, "message": format!("Message must be a valid JSON ({e})")}})
                        .to_string(),
                );
                return std::mem::take(&mut self.out);
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Json::Null);
        let method = msg.get("method").and_then(Json::as_str).unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let session = msg
            .get("sessionId")
            .and_then(Json::as_str)
            .map(str::to_string);
        let reply = match &session {
            None => self.browser(method, &params, None),
            Some(sid) if *sid == browser_session() => {
                self.browser(method, &params, Some(sid.as_str()))
            }
            Some(sid) => match self.targets.iter().position(|t| {
                t.session_id.as_deref() == Some(sid) || t.extra_sessions.contains(sid)
            }) {
                None => Err(CdpError::new(-32001, "Session with given id not found.")),
                Some(ti) => self.page_method(ti, &id, method, &params),
            },
        };
        // A call waiting on a promise is answered later (see `drive_pending`).
        if !self.is_deferred(&id, session.as_deref()) {
            self.respond(&id, session.as_deref(), reply);
        }
        self.out.append(&mut self.after_reply);
        self.drive_pending();
        self.take_out()
    }

    /// The messages to send, with every secret typed in the session replaced by its handle:
    /// page scripts can read and echo a field's value, and the client must
    /// not see it. Prefixes of 4+ characters are replaced too, since typing goes key by key.
    fn take_out(&mut self) -> Vec<String> {
        let out = std::mem::take(&mut self.out);
        let mut secrets: Vec<(String, String)> = Vec::new();
        for (value, handle) in self.session.secrets().iter() {
            let chars: Vec<char> = value.chars().collect();
            if chars.len() < 4 {
                secrets.push((value.clone(), handle.clone()));
                continue;
            }
            for k in (4..=chars.len()).rev() {
                let prefix: String = chars[..k].iter().collect();
                secrets.push((prefix, handle.clone()));
            }
        }
        if secrets.is_empty() {
            return out;
        }
        out.into_iter()
            .map(|mut m| {
                for (value, handle) in &secrets {
                    // As it appears inside a JSON string.
                    let escaped = serde_json::to_string(value).unwrap_or_default();
                    let escaped = escaped.trim_matches('"');
                    if m.contains(escaped) {
                        m = m.replace(escaped, handle);
                    }
                }
                m
            })
            .collect()
    }

    fn is_deferred(&self, id: &Json, session: Option<&str>) -> bool {
        self.pending
            .iter()
            .any(|p| &p.id == id && Some(p.session_id.as_str()) == session)
    }

    fn respond(&mut self, id: &Json, session: Option<&str>, reply: Reply) {
        let mut m = match reply {
            Ok(result) => json!({"id": id, "result": result}),
            Err(e) => {
                let mut err = json!({"code": e.code, "message": e.message});
                if let Some(d) = e.data {
                    err["data"] = d;
                }
                json!({"id": id, "error": err})
            }
        };
        if let Some(s) = session {
            m["sessionId"] = json!(s);
        }
        self.out.push(m.to_string());
    }

    fn event(&mut self, method: &str, params: Json, session: Option<&str>) {
        let mut m = json!({"method": method, "params": params});
        if let Some(s) = session {
            m["sessionId"] = json!(s);
        }
        self.out.push(m.to_string());
    }

    /// A page target's event, sent on its session (if attached).
    fn target_event(&mut self, ti: usize, method: &str, params: Json) {
        if let Some(s) = self.targets[ti].session_id.clone() {
            self.event(method, params, Some(&s));
        }
    }

    fn next_id(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    fn timestamp(&self) -> f64 {
        self.session.now_ms() as f64 / 1000.0
    }

    fn target_info(&self, ti: usize) -> Json {
        let t = &self.targets[ti];
        json!({
            "targetId": t.id,
            "type": "page",
            "title": t.tab.title(),
            "url": t.tab.url(),
            "attached": t.session_id.is_some(),
            "canAccessOpener": false,
            "browserContextId": CONTEXT,
        })
    }

    fn create_target(&mut self, url: &str) -> Reply {
        // Protocol tabs report no steps: CDP input is not an agent's step.
        let tab = crate::Tab::open(self.session, url, self.options.clone(), false)
            .or_else(|e| {
                // A page that fails to load still makes a target (Chrome shows an error page).
                let _ = e;
                crate::Tab::open(self.session, "about:blank", self.options.clone(), false)
            })
            .map_err(|e| CdpError::server(e.to_string()))?;
        let n = self.next_id();
        let id = hex(0x7A_0000, n);
        let loader = hex(0x10_AD00, n);
        self.targets.push(Target {
            id: id.clone(),
            session_id: None,
            extra_sessions: Vec::new(),
            tab,
            loader,
            epoch: n,
            contexts: Vec::new(),
            frames: Vec::new(),
            worlds: Vec::new(),
            runtime: false,
            page: false,
            network: false,
            lifecycle: false,
            objects: HashMap::new(),
            groups: HashMap::new(),
            next_object: 0,
            bindings: Vec::new(),
            nodes: HashMap::new(),
            backends: HashMap::new(),
            pressed: None,
            webmcp: false,
            tools_seen: Vec::new(),
        });
        let ti = self.targets.len() - 1;
        self.rebuild_frames(ti);
        self.new_contexts(ti);
        if self.discover {
            let info = self.target_info(ti);
            self.event("Target.targetCreated", json!({"targetInfo": info}), None);
        }
        if self.auto_attach {
            self.attach(ti);
        }
        Ok(json!({"targetId": id}))
    }

    fn attach(&mut self, ti: usize) -> String {
        if let Some(s) = &self.targets[ti].session_id {
            return s.clone();
        }
        let n = self.next_id();
        let sid = hex(0x5E_5500, n);
        self.targets[ti].session_id = Some(sid.clone());
        let info = self.target_info(ti);
        self.event(
            "Target.attachedToTarget",
            json!({"sessionId": sid, "targetInfo": info, "waitingForDebugger": false}),
            None,
        );
        sid
    }

    fn target_by_id(&self, params: &Json) -> Result<usize, CdpError> {
        let id = params
            .get("targetId")
            .and_then(Json::as_str)
            .unwrap_or_default();
        self.targets
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| CdpError::server("No target with given id found"))
    }

    /// Browser-level methods (no `sessionId`).
    /// `via`: the browser session the message came on (`attachToBrowserTarget`), if any.
    fn browser(&mut self, method: &str, params: &Json, via: Option<&str>) -> Reply {
        match method {
            "Browser.getVersion" => Ok(json!({
                "protocolVersion": "1.3",
                "product": format!("pageon/{}", env!("CARGO_PKG_VERSION")),
                "revision": "",
                "userAgent": pageon_env::user_agent(),
                "jsVersion": "",
            })),
            "Browser.close" => {
                self.closed = true;
                Ok(json!({}))
            }
            "Target.getBrowserContexts" => Ok(json!({"browserContextIds": []})),
            "Target.attachToBrowserTarget" => Ok(json!({"sessionId": browser_session()})),
            "Target.setDiscoverTargets" => {
                let on = params
                    .get("discover")
                    .and_then(Json::as_bool)
                    .unwrap_or(false);
                if on && !self.discover {
                    for ti in 0..self.targets.len() {
                        let info = self.target_info(ti);
                        self.event("Target.targetCreated", json!({"targetInfo": info}), None);
                    }
                }
                self.discover = on;
                Ok(json!({}))
            }
            "Target.setAutoAttach" => {
                self.auto_attach = params
                    .get("autoAttach")
                    .and_then(Json::as_bool)
                    .unwrap_or(false);
                if self.auto_attach {
                    for ti in 0..self.targets.len() {
                        self.attach(ti);
                    }
                }
                Ok(json!({}))
            }
            "Target.createTarget" => {
                let url = params
                    .get("url")
                    .and_then(Json::as_str)
                    .unwrap_or("about:blank");
                let url = if url.is_empty() { "about:blank" } else { url };
                self.create_target(url)
            }
            "Target.attachToTarget" => {
                let ti = self.target_by_id(params)?;
                if self.targets[ti].session_id.is_none() {
                    return Ok(json!({"sessionId": self.attach(ti)}));
                }
                // Another session on an attached page, announced on the session that asked.
                let n = self.next_id();
                let sid = hex(0x5E_5500, n);
                self.targets[ti].extra_sessions.push(sid.clone());
                let info = self.target_info(ti);
                self.event(
                    "Target.attachedToTarget",
                    json!({"sessionId": sid, "targetInfo": info, "waitingForDebugger": false}),
                    via,
                );
                Ok(json!({"sessionId": sid}))
            }
            "Target.detachFromTarget" => {
                let sid = params
                    .get("sessionId")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let Some(ti) = self.targets.iter().position(|t| {
                    t.session_id.as_deref() == Some(sid)
                        || t.extra_sessions.iter().any(|s| s == sid)
                }) else {
                    return Err(CdpError::server("No session with given id"));
                };
                let t = &mut self.targets[ti];
                if t.session_id.as_deref() == Some(sid) {
                    t.session_id = None;
                    let target = t.id.clone();
                    self.event(
                        "Target.detachedFromTarget",
                        json!({"sessionId": sid, "targetId": target}),
                        None,
                    );
                } else {
                    t.extra_sessions.retain(|s| s != sid);
                }
                Ok(json!({}))
            }
            "Target.closeTarget" => {
                let ti = self.target_by_id(params)?;
                let t = self.targets.remove(ti);
                self.pending.retain(|p| p.target != t.id);
                if let Some(sid) = &t.session_id {
                    self.event(
                        "Target.detachedFromTarget",
                        json!({"sessionId": sid, "targetId": t.id}),
                        None,
                    );
                }
                if self.discover {
                    self.event("Target.targetDestroyed", json!({"targetId": t.id}), None);
                }
                Ok(json!({"success": true}))
            }
            "Target.getTargets" => {
                let infos: Vec<Json> = (0..self.targets.len())
                    .map(|i| self.target_info(i))
                    .collect();
                Ok(json!({"targetInfos": infos}))
            }
            "Target.getTargetInfo" => match params.get("targetId") {
                Some(_) => {
                    let ti = self.target_by_id(params)?;
                    Ok(json!({"targetInfo": self.target_info(ti)}))
                }
                None => Ok(json!({"targetInfo": {
                    "targetId": self.browser_id, "type": "browser", "title": "", "url": "",
                    "attached": true, "canAccessOpener": false,
                }})),
            },
            "Target.activateTarget"
            | "Runtime.runIfWaitingForDebugger"
            | "Browser.setDownloadBehavior" => Ok(json!({})),
            "Storage.getCookies" => Ok(json!({"cookies": page::cookies_json(self.session, None)})),
            "Storage.setCookies" => page::set_cookies(self.session, params, None),
            "Storage.clearCookies" => {
                self.session.net().borrow_mut().clear_cookies();
                Ok(json!({}))
            }
            "Target.createBrowserContext" | "Target.disposeBrowserContext" => {
                Err(CdpError::unsupported(
                    method,
                    "pageon has one browser context per process; start another `pageon cdp` for isolation",
                ))
            }
            _ => Err(unknown(method)),
        }
    }

    /// Page-session methods.
    fn page_method(&mut self, ti: usize, id: &Json, method: &str, params: &Json) -> Reply {
        let (domain, _) = method.split_once('.').unwrap_or((method, ""));
        match domain {
            "Runtime" | "DOM" => self.runtime_method(ti, id, method, params),
            "WebMCP" => self.webmcp_method(ti, method, params),
            _ => self.page_domain(ti, method, params),
        }
    }

    /// The main frame's URL changed in place (the History API, a fragment):
    /// `Page.navigatedWithinDocument`, as Chrome sends it; contexts and objects stay.
    fn within_document(&mut self, ti: usize, url: &str) {
        let Some(frame) = self.targets[ti].frames.first_mut() else {
            return;
        };
        let old = std::mem::replace(&mut frame.url, url.to_string());
        let id = frame.id.clone();
        let before = |u: &str| u.split('#').next().unwrap_or(u).to_string();
        let kind = if before(&old) == before(url) && url.contains('#') {
            "fragment"
        } else {
            "historyApi"
        };
        if self.targets[ti].page {
            self.target_event(
                ti,
                "Page.navigatedWithinDocument",
                json!({"frameId": id, "url": url, "navigationType": kind}),
            );
        }
        if self.discover {
            let info = self.target_info(ti);
            self.event(
                "Target.targetInfoChanged",
                json!({"targetInfo": info}),
                None,
            );
        }
    }

    /// Turns what an action or a script did into events: requests, then a new document.
    fn after_outcome(&mut self, ti: usize, out: &Outcome) {
        if out.same_document
            && let Some(url) = &out.navigated
        {
            self.within_document(ti, url);
        }
        let navigated = out.navigated.is_some() && !out.same_document;
        if navigated {
            self.begin_document(ti);
        }
        self.network_events(ti, out, navigated);
        if navigated {
            self.finish_document(ti);
        }
        // Console messages and binding calls of every frame whose scripts run, each from its
        // frame's main world.
        let mut calls = Vec::new();
        let mut console = Vec::new();
        for fi in 0..self.targets[ti].frames.len() {
            let Some(l) = self.targets[ti].tab.live_if_started(fi) else {
                continue;
            };
            let ctx = self.targets[ti]
                .contexts
                .iter()
                .find(|c| c.frame == fi && c.name.is_empty())
                .map_or(0, |c| c.id);
            let mut e = l.engine.borrow_mut();
            calls.extend(
                e.take_binding_calls()
                    .into_iter()
                    .map(|(_, n, p)| (ctx, n, p)),
            );
            console.extend(
                e.take_console()
                    .into_iter()
                    .map(|(level, text)| (ctx, level, text)),
            );
        }
        if self.targets[ti].runtime {
            let ts = self.timestamp() * 1000.0;
            for (ctx, level, text) in console {
                let ty = match level.as_str() {
                    "warn" => "warning",
                    "error" | "info" | "debug" | "log" => level.as_str(),
                    _ => "log",
                };
                self.target_event(
                    ti,
                    "Runtime.consoleAPICalled",
                    json!({"type": ty, "args": [{"type": "string", "value": text}], "executionContextId": ctx, "timestamp": ts}),
                );
            }
            for e in &out.errors {
                self.next_request += 1;
                let message = e
                    .message
                    .trim_start_matches("Uncaught (in promise) ")
                    .trim_start_matches("Uncaught ");
                self.target_event(
                    ti,
                    "Runtime.exceptionThrown",
                    json!({"timestamp": ts, "exceptionDetails": {
                        "exceptionId": self.next_request, "text": "Uncaught",
                        "lineNumber": e.line.saturating_sub(1), "columnNumber": e.column.saturating_sub(1),
                        "url": e.source,
                        "exception": {"type": "object", "subtype": "error", "className": "Error", "description": message},
                    }}),
                );
            }
        }
        for (ctx, name, payload) in calls {
            self.target_event(
                ti,
                "Runtime.bindingCalled",
                json!({"name": name, "payload": payload, "executionContextId": ctx}),
            );
        }
        self.webmcp_sync(ti);
    }

    /// The client sent nothing for `elapsed_ms` of real time: each page's clock moves by that
    /// time (at most 30 s per tick), running what falls due, so a polling client (Playwright's
    /// auto-wait) sees the page's timers fire as the wall clock passes, as in Chrome. Unlike
    /// an awaited promise, idleness does not skip ahead: a pause must not run a page's
    /// deadlines early.
    pub fn idle(&mut self, elapsed_ms: u64) -> Vec<String> {
        let env = self.session.env();
        for ti in 0..self.targets.len() {
            for frame in 0..self.targets[ti].frames.len() {
                let Some(live) = self.targets[ti].tab.live_if_started(frame) else {
                    continue;
                };
                if live.engine.borrow().next_due_ms().is_none() {
                    continue;
                }
                let step = elapsed_ms.min(AWAIT_BUDGET_MS);
                if let Ok((_, out)) = self.targets[ti]
                    .tab
                    .script_in(frame, "idle", |e, _| e.wait(step, env))
                {
                    self.after_outcome(ti, &out);
                }
            }
        }
        self.drive_pending();
        self.take_out()
    }

    /// Runs awaited promises: answers settled ones, and runs the clock ahead while something is
    /// due and a call is still waiting.
    fn drive_pending(&mut self) {
        let mut rounds = 0;
        while !self.pending.is_empty() && rounds < 10_000 {
            rounds += 1;
            self.answer_settled();
            // Advance the clock of one target with a waiting call.
            let Some(p) = self.pending.iter().position(|p| p.budget_ms > 0) else {
                return;
            };
            let target = self.pending[p].target.clone();
            let Some(ti) = self.targets.iter().position(|t| t.id == target) else {
                self.pending.remove(p);
                continue;
            };
            let frame = self.ctx_frame(ti, self.pending[p].ctx).unwrap_or(0);
            let due = self.targets[ti]
                .tab
                .live_in(frame)
                .ok()
                .and_then(|l| l.engine.borrow().next_due_ms());
            let Some(due) = due else { return };
            let step = due.clamp(1, self.pending[p].budget_ms);
            for q in self.pending.iter_mut().filter(|q| q.target == target) {
                q.budget_ms = q.budget_ms.saturating_sub(step);
            }
            let env = self.session.env();
            match self.targets[ti]
                .tab
                .script_in(frame, "wait", |e, _| e.wait(step, env))
            {
                Ok((_, out)) => self.after_outcome(ti, &out),
                Err(_) => return,
            }
        }
    }

    /// Answers every waiting call whose promise settled (or whose context went away).
    fn answer_settled(&mut self) {
        let mut i = 0;
        while i < self.pending.len() {
            let p = &self.pending[i];
            let Some(ti) = self.targets.iter().position(|t| t.id == p.target) else {
                self.pending.remove(i);
                continue;
            };
            if self.targets[ti].epoch != p.epoch {
                let p = self.pending.remove(i);
                self.respond(
                    &p.id,
                    Some(&p.session_id),
                    Err(CdpError::server("Execution context was destroyed.")),
                );
                continue;
            }
            let (promise, ser) = (p.promise, p.ser);
            let Ok((_, live, at)) = self.live_ctx(ti, p.ctx) else {
                i += 1;
                continue;
            };
            let state = live.engine.borrow_mut().promise_state(at, promise);
            let settled = match state {
                pageon_js::PromiseState::Pending => None,
                pageon_js::PromiseState::Fulfilled(v) => Some((v, false)),
                pageon_js::PromiseState::NotPromise => Some((promise, false)),
                pageon_js::PromiseState::Rejected(v) => Some((v, true)),
            };
            let Some((value, rejected)) = settled else {
                i += 1;
                continue;
            };
            let p = self.pending.remove(i);
            let reply = if rejected {
                self.exception_reply(
                    ti,
                    p.ctx,
                    value,
                    "Uncaught (in promise)",
                    0,
                    0,
                    p.group.as_deref(),
                    ser,
                )
            } else {
                self.value_reply(ti, p.ctx, value, ser, p.group.as_deref())
            };
            self.respond(&p.id, Some(&p.session_id), reply);
        }
    }
}

/// An unknown method: Chrome's message, and where to look.
fn unknown(method: &str) -> CdpError {
    CdpError::new(
        -32601,
        format!(
            "'{method}' wasn't found (pageon implements the CDP subset Puppeteer and Playwright use; see `pageon cdp --help`)"
        ),
    )
}
