//! A WebDriver BiDi session: a mapper over pageon's CDP session, the way Chrome's
//! chromium-bidi maps BiDi onto CDP. Each BiDi command becomes CDP calls on the wrapped
//! [`crate::cdp::Session`]; its responses and events become BiDi responses and events, sent
//! to subscribers only. It runs where the CDP session runs (the caged content process by
//! default), so virtual time, policy, vault and recording behave as for CDP.

pub(crate) mod context;
mod input;
mod script;

use std::collections::HashMap;

use pageon_env::Env;
use serde_json::{Value as Json, json};

use crate::LoadOptions;

/// A BiDi error: its code and message.
#[derive(Debug, Clone)]
pub(crate) struct BidiError {
    code: &'static str,
    message: String,
}

impl BidiError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid argument", message)
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self::new("unsupported operation", message)
    }

    fn no_frame(id: &str) -> Self {
        Self::new("no such frame", format!("no browsing context {id:?}"))
    }

    /// A CDP error, by what it says.
    fn from_cdp(e: &Json) -> Self {
        let m = e["message"].as_str().unwrap_or("unknown error").to_string();
        let code = if m.contains("Could not find object") {
            "no such handle"
        } else if m.contains("No node") || m.contains("doesn't reference a Node") {
            "no such node"
        } else if m.contains("No target") || m.contains("Cannot find context") {
            "no such frame"
        } else if e["code"] == -32602 {
            "invalid argument"
        } else {
            "unknown error"
        };
        Self::new(code, m)
    }
}

type Answer = Result<Json, BidiError>;

/// What a CDP call gave back.
enum Cdp {
    Ok(Json),
    Err(Json),
    /// The answer comes later (an awaited promise); its CDP id.
    Deferred(i64),
}

/// A BiDi command waiting for a deferred CDP answer.
struct Waiter {
    id: Json,
    realm: String,
    root: bool,
    session: String,
}

/// A browsing context: a tab (top level) or a frame.
#[derive(Debug, Clone)]
struct Context {
    id: String,
    parent: Option<String>,
    /// The CDP session of its tab.
    session: String,
    url: String,
    /// The current navigation (CDP loader id).
    navigation: Option<String>,
    /// Navigations already announced (`navigationStarted`).
    started: Vec<String>,
}

/// A realm: an execution context of a frame.
#[derive(Debug, Clone)]
struct Realm {
    id: String,
    ctx: i64,
    session: String,
    context: String,
    origin: String,
    sandbox: Option<String>,
}

/// A subscription.
#[derive(Debug, Clone)]
struct Sub {
    id: String,
    events: Vec<String>,
    /// Top-level contexts (empty: all).
    contexts: Vec<String>,
}

/// What pageon knows of a request until it completes.
#[derive(Debug, Clone)]
struct Request {
    context: String,
    navigation: Option<String>,
    redirects: u64,
    request: Json,
    response: Option<Json>,
}

/// Every event pageon emits, by module.
const EVENTS: &[&str] = &[
    "browsingContext.contextCreated",
    "browsingContext.contextDestroyed",
    "browsingContext.navigationStarted",
    "browsingContext.navigationCommitted",
    "browsingContext.navigationFailed",
    "browsingContext.fragmentNavigated",
    "browsingContext.historyUpdated",
    "browsingContext.domContentLoaded",
    "browsingContext.load",
    "script.realmCreated",
    "script.realmDestroyed",
    "log.entryAdded",
    "network.beforeRequestSent",
    "network.responseStarted",
    "network.responseCompleted",
    "network.fetchError",
];

/// Events and modules of the spec that pageon never emits (subscribing is fine, nothing comes).
const SILENT: &[&str] = &[
    "browsingContext",
    "script",
    "log",
    "network",
    "input",
    "browsingContext.userPromptOpened",
    "browsingContext.userPromptClosed",
    "browsingContext.navigationAborted",
    "browsingContext.downloadWillBegin",
    "browsingContext.downloadEnd",
    "script.message",
    "network.authRequired",
    "input.fileDialogOpened",
];

/// Commands of the spec pageon does not have, and what to do instead.
const UNSUPPORTED: &[(&str, &str)] = &[
    (
        "network.",
        "request interception is not available; limit where requests go with pageon's egress policy (--origin, --approve) or decide per request with --guard COMMAND",
    ),
    (
        "script.addPreloadScript",
        "running client code before the page's own scripts is not supported; evaluate after browsingContext.navigate",
    ),
    (
        "script.removePreloadScript",
        "preload scripts are not supported",
    ),
    (
        "browser.createUserContext",
        "pageon has one user context per process; start another `pageon cdp` for isolation",
    ),
    (
        "browser.removeUserContext",
        "pageon has one user context per process",
    ),
    (
        "browser.setClientWindowState",
        "pageon has no windows; a tab is laid out at the session's viewport",
    ),
    (
        "browser.setDownloadBehavior",
        "downloads are reported in pageon:act outcomes",
    ),
    (
        "emulation.",
        "identity comes from the session's profile (--profile); the viewport from browsingContext.setViewport",
    ),
    (
        "browsingContext.print",
        "pageon does not print to PDF; use browsingContext.captureScreenshot",
    ),
    (
        "browsingContext.startScreencast",
        "pageon paints on demand and does not stream; use browsingContext.captureScreenshot",
    ),
    (
        "browsingContext.stopScreencast",
        "pageon paints on demand and does not stream",
    ),
    ("input.setFiles", "file uploads are not supported yet"),
    ("permissions.", "permissions are not supported"),
    ("webExtension.", "web extensions are not supported"),
    ("bluetooth.", "Web Bluetooth is not supported"),
    (
        "userAgentClientHints.",
        "client hints come from the session's profile (--profile)",
    ),
    ("speculation.", "speculation rules are not supported"),
];

/// One client's BiDi session over a pageon [`crate::Session`].
pub struct Session<'s, E: Env> {
    cdp: crate::cdp::Session<'s, E>,
    started: bool,
    ended: bool,
    subs: Vec<Sub>,
    next_sub: u64,
    next_cdp: i64,
    next_nav: u64,
    waiting: HashMap<i64, Waiter>,
    contexts: Vec<Context>,
    realms: Vec<Realm>,
    requests: HashMap<String, Request>,
    input: HashMap<String, input::State>,
    out: Vec<String>,
}

impl<'s, E: Env> Session<'s, E> {
    /// A BiDi session over a fresh CDP session (one blank tab).
    pub fn new(session: &'s crate::Session<E>, options: LoadOptions) -> Self {
        Self::over(crate::cdp::Session::new(session, options))
    }

    /// A BiDi session over an existing CDP session (its tabs become contexts).
    pub fn over(cdp: crate::cdp::Session<'s, E>) -> Self {
        Self {
            cdp,
            started: false,
            ended: false,
            subs: Vec::new(),
            next_sub: 0,
            next_cdp: 0,
            next_nav: 0,
            waiting: HashMap::new(),
            contexts: Vec::new(),
            realms: Vec::new(),
            requests: HashMap::new(),
            input: HashMap::new(),
            out: Vec::new(),
        }
    }

    /// Whether the client ended the session or closed the browser.
    pub fn closed(&self) -> bool {
        self.ended || self.cdp.closed()
    }

    /// Handles one client message and returns the messages to send, in order.
    pub fn handle(&mut self, message: &str) -> Vec<String> {
        let msg: Json = match serde_json::from_str(message) {
            Ok(m @ Json::Object(_)) => m,
            Ok(_) | Err(_) => {
                self.error(
                    &Json::Null,
                    BidiError::invalid("a command is a JSON object {id, method, params}"),
                );
                return std::mem::take(&mut self.out);
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Json::Null);
        let (Some(_), Some(method)) = (id.as_u64(), msg.get("method").and_then(Json::as_str))
        else {
            self.error(
                &id,
                BidiError::invalid("a command needs an integer id and a method"),
            );
            return std::mem::take(&mut self.out);
        };
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            self.error(&id, BidiError::invalid("params must be an object"));
            return std::mem::take(&mut self.out);
        }
        let method = method.to_string();
        match self.command(&id, &method, &params) {
            Ok(Some(result)) => self.success(&id, result),
            Ok(None) => {}
            Err(e) => self.error(&id, e),
        }
        std::mem::take(&mut self.out)
    }

    /// The client has been idle this long (real ms): the page's clock may move.
    pub fn idle(&mut self, elapsed_ms: u64) -> Vec<String> {
        let out = self.cdp.idle(elapsed_ms);
        self.take_cdp(out, None);
        std::mem::take(&mut self.out)
    }

    fn success(&mut self, id: &Json, result: Json) {
        self.out
            .push(json!({"type": "success", "id": id, "result": result}).to_string());
    }

    fn error(&mut self, id: &Json, e: BidiError) {
        let id = if id.is_u64() { id.clone() } else { Json::Null };
        self.out.push(
            json!({"type": "error", "id": id, "error": e.code, "message": e.message}).to_string(),
        );
    }

    /// Runs a command: `Ok(None)` when the answer comes later.
    fn command(&mut self, id: &Json, method: &str, p: &Json) -> Result<Option<Json>, BidiError> {
        if !self.started && !matches!(method, "session.status" | "session.new") {
            return Err(BidiError::new(
                "invalid session id",
                "no session: send session.new first",
            ));
        }
        let r = match method {
            "session.status" => Ok(if self.started {
                json!({"ready": false, "message": "a session already exists"})
            } else {
                json!({"ready": true, "message": "pageon is ready"})
            }),
            "session.new" => self.session_new(p),
            "session.end" => {
                self.ended = true;
                Ok(json!({}))
            }
            "session.subscribe" => self.subscribe(p),
            "session.unsubscribe" => self.unsubscribe(p),
            "browser.close" => {
                self.call("Browser.close", json!({}), None)?;
                Ok(json!({}))
            }
            "browser.getUserContexts" => Ok(json!({"userContexts": [{"userContext": "default"}]})),
            "browser.getClientWindows" => {
                let v = crate::Viewport::default();
                Ok(json!({"clientWindows": [{
                    "clientWindow": CLIENT_WINDOW, "active": true, "state": "normal",
                    "x": 0, "y": 0, "width": v.width, "height": v.height,
                }]}))
            }
            "script.evaluate" | "script.callFunction" => return self.run_script(id, method, p),
            "script.disown" => self.disown(p),
            "script.getRealms" => self.get_realms(p),
            // As browsingContext.setViewport: Puppeteer sends it with every viewport.
            "emulation.setScreenOrientationOverride" => Ok(json!({})),
            "input.performActions" => self.perform_actions(p),
            "input.releaseActions" => self.release_actions(p),
            m if m.starts_with("browsingContext.")
                || m.starts_with("storage.")
                || m.starts_with("pageon:") =>
            {
                if let Some(r) = self.context_command(m, p) {
                    r
                } else {
                    Err(unsupported(m))
                }
            }
            m => Err(unsupported(m)),
        };
        r.map(Some)
    }

    fn session_new(&mut self, p: &Json) -> Answer {
        if self.started {
            return Err(BidiError::new(
                "session not created",
                "this connection already has a session",
            ));
        }
        let caps = p.get("capabilities").cloned().unwrap_or_else(|| json!({}));
        let always = caps
            .get("alwaysMatch")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let first: Vec<Json> = match caps.get("firstMatch").and_then(Json::as_array) {
            Some(a) if !a.is_empty() => a.clone(),
            _ => vec![json!({})],
        };
        let mut why = String::new();
        let mut chosen = None;
        for f in first {
            let mut merged = always.as_object().cloned().unwrap_or_default();
            for (k, v) in f.as_object().into_iter().flatten() {
                merged.insert(k.clone(), v.clone());
            }
            match matches(&merged) {
                Ok(()) => {
                    chosen = Some(merged);
                    break;
                }
                Err(e) => why = e,
            }
        }
        let Some(merged) = chosen else {
            return Err(BidiError::new("session not created", why));
        };
        self.started = true;
        self.call("Target.setDiscoverTargets", json!({"discover": true}), None)?;
        self.call(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "flatten": true, "waitForDebuggerOnStart": false}),
            None,
        )?;
        let user_agent = self
            .call("Browser.getVersion", json!({}), None)
            .ok()
            .and_then(|v| v["userAgent"].as_str().map(str::to_string))
            .unwrap_or_default();
        let mut c = json!({
            "acceptInsecureCerts": false,
            "browserName": "pageon",
            "browserVersion": env!("CARGO_PKG_VERSION"),
            "platformName": platform(),
            "proxy": {},
            "setWindowRect": false,
            "userAgent": user_agent,
            "unhandledPromptBehavior": {"default": "dismiss"},
        });
        if merged.get("webSocketUrl") == Some(&json!(true)) {
            c["webSocketUrl"] = json!("ws://127.0.0.1/session");
        }
        Ok(json!({"sessionId": SESSION_ID, "capabilities": c}))
    }

    fn subscribe(&mut self, p: &Json) -> Answer {
        let names = p
            .get("events")
            .and_then(Json::as_array)
            .filter(|a| !a.is_empty())
            .ok_or_else(|| {
                BidiError::invalid("events: a non-empty list of event or module names")
            })?;
        let mut events = Vec::new();
        for n in names {
            let n = n
                .as_str()
                .ok_or_else(|| BidiError::invalid("events: strings expected"))?;
            let known = EVENTS.contains(&n) || SILENT.contains(&n);
            if !known {
                return Err(BidiError::invalid(format!(
                    "{n:?} is not an event or module pageon knows (modules: browsingContext, script, log, network, input)"
                )));
            }
            if n.contains('.') {
                events.push(n.to_string());
            } else {
                events.extend(
                    EVENTS
                        .iter()
                        .filter(|e| e.split('.').next() == Some(n))
                        .map(|e| e.to_string()),
                );
            }
        }
        let mut contexts = Vec::new();
        for c in p
            .get("contexts")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
        {
            let c = c
                .as_str()
                .ok_or_else(|| BidiError::invalid("contexts: strings expected"))?;
            let top = self.top_of(c).ok_or_else(|| BidiError::no_frame(c))?;
            contexts.push(top);
        }
        if let Some(u) = p.get("userContexts").and_then(Json::as_array)
            && u.iter().any(|u| u != "default")
        {
            return Err(BidiError::new(
                "no such user context",
                "pageon has one user context: \"default\"",
            ));
        }
        self.next_sub += 1;
        let id = format!("sub-{}", self.next_sub);
        self.subs.push(Sub {
            id: id.clone(),
            events,
            contexts,
        });
        Ok(json!({"subscription": id}))
    }

    fn unsubscribe(&mut self, p: &Json) -> Answer {
        if let Some(ids) = p.get("subscriptions").and_then(Json::as_array) {
            let ids: Vec<&str> = ids.iter().filter_map(Json::as_str).collect();
            if let Some(bad) = ids.iter().find(|i| !self.subs.iter().any(|s| s.id == **i)) {
                return Err(BidiError::invalid(format!("no subscription {bad:?}")));
            }
            self.subs.retain(|s| !ids.contains(&s.id.as_str()));
            return Ok(json!({}));
        }
        // The deprecated form: remove these events (for these contexts) from every subscription.
        let names: Vec<String> = p
            .get("events")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .flat_map(|n| {
                if n.contains('.') {
                    vec![n.to_string()]
                } else {
                    EVENTS
                        .iter()
                        .filter(|e| e.split('.').next() == Some(n))
                        .map(|e| e.to_string())
                        .collect()
                }
            })
            .collect();
        if names.is_empty() {
            return Err(BidiError::invalid(
                "unsubscribe needs subscriptions (ids) or events",
            ));
        }
        for s in &mut self.subs {
            s.events.retain(|e| !names.contains(e));
        }
        self.subs.retain(|s| !s.events.is_empty());
        Ok(json!({}))
    }

    // --- CDP plumbing ---------------------------------------------------------------------

    /// Sends one CDP command and translates everything it produced.
    fn cdp(&mut self, method: &str, params: Json, session: Option<&str>) -> Cdp {
        self.next_cdp -= 1;
        let id = self.next_cdp;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let out = self.cdp.handle(&msg.to_string());
        match self.take_cdp(out, Some(id)) {
            Some(Ok(r)) => Cdp::Ok(r),
            Some(Err(e)) => Cdp::Err(e),
            None => Cdp::Deferred(id),
        }
    }

    /// A CDP call that must answer at once.
    fn call(&mut self, method: &str, params: Json, session: Option<&str>) -> Answer {
        match self.cdp(method, params, session) {
            Cdp::Ok(r) => Ok(r),
            Cdp::Err(e) => Err(BidiError::from_cdp(&e)),
            Cdp::Deferred(_) => Err(BidiError::new(
                "unknown error",
                format!("{method} did not answer"),
            )),
        }
    }

    /// Translates CDP output in order; returns the answer to `mine`, if it is there.
    fn take_cdp(&mut self, out: Vec<String>, mine: Option<i64>) -> Option<Result<Json, Json>> {
        let mut answer = None;
        for m in out {
            let Ok(m) = serde_json::from_str::<Json>(&m) else {
                continue;
            };
            if let Some(id) = m.get("id").and_then(Json::as_i64) {
                let r = match m.get("error") {
                    Some(e) => Err(e.clone()),
                    None => Ok(m["result"].clone()),
                };
                if Some(id) == mine {
                    answer = Some(r);
                } else if let Some(w) = self.waiting.remove(&id) {
                    self.finish_script(w, r);
                }
                continue;
            }
            let method = m["method"].as_str().unwrap_or_default().to_string();
            let session = m["sessionId"].as_str().map(str::to_string);
            self.on_cdp_event(&method, &m["params"], session.as_deref());
        }
        answer
    }

    // --- Events ---------------------------------------------------------------------------

    /// Sends an event to its subscribers.
    fn emit(&mut self, method: &str, params: Json, context: Option<&str>) {
        let top = context.and_then(|c| self.top_of(c));
        let wanted = self.subs.iter().any(|s| {
            s.events.iter().any(|e| e == method)
                && (s.contexts.is_empty() || top.as_ref().is_some_and(|t| s.contexts.contains(t)))
        });
        if wanted {
            self.out
                .push(json!({"type": "event", "method": method, "params": params}).to_string());
        }
    }

    fn top_of(&self, id: &str) -> Option<String> {
        let mut c = self.contexts.iter().find(|c| c.id == id)?;
        while let Some(p) = &c.parent {
            c = self.contexts.iter().find(|x| &x.id == p)?;
        }
        Some(c.id.clone())
    }

    fn context_of(&self, id: &str) -> Result<&Context, BidiError> {
        self.contexts
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| BidiError::no_frame(id))
    }

    fn info(&self, c: &Context, depth: Option<u64>) -> Json {
        let children: Json = if depth == Some(0) {
            Json::Null
        } else {
            Json::Array(
                self.contexts
                    .iter()
                    .filter(|x| x.parent.as_deref() == Some(c.id.as_str()))
                    .map(|x| self.info(x, depth.map(|d| d - 1)))
                    .collect(),
            )
        };
        json!({
            "context": c.id, "url": c.url, "children": children, "parent": c.parent,
            "userContext": "default", "originalOpener": null, "clientWindow": CLIENT_WINDOW,
        })
    }

    fn now(&self) -> u64 {
        self.cdp.now_ms()
    }

    fn add_context(&mut self, c: Context) {
        if self.contexts.iter().any(|x| x.id == c.id) {
            return;
        }
        let info = self.info(&c, Some(0));
        let id = c.id.clone();
        self.contexts.push(c);
        self.emit("browsingContext.contextCreated", info, Some(&id));
    }

    /// Removes a context and its descendants, deepest first.
    fn remove_context(&mut self, id: &str) {
        let kids: Vec<String> = self
            .contexts
            .iter()
            .filter(|c| c.parent.as_deref() == Some(id))
            .map(|c| c.id.clone())
            .collect();
        for k in kids {
            self.remove_context(&k);
        }
        let gone: Vec<Realm> = self
            .realms
            .iter()
            .filter(|r| r.context == id)
            .cloned()
            .collect();
        for r in gone {
            self.drop_realm(&r.id);
        }
        if let Some(i) = self.contexts.iter().position(|c| c.id == id) {
            let info = self.info(&self.contexts[i], Some(0));
            self.emit("browsingContext.contextDestroyed", info, Some(id));
            self.contexts.remove(i);
            self.input.remove(id);
        }
    }

    fn drop_realm(&mut self, id: &str) {
        if let Some(i) = self.realms.iter().position(|r| r.id == id) {
            let r = self.realms.remove(i);
            self.emit(
                "script.realmDestroyed",
                json!({"realm": r.id}),
                Some(&r.context),
            );
        }
    }

    /// A new tab: its context, its frames, then its realms.
    fn attached(&mut self, session: &str, target: &Json) {
        let id = target["targetId"].as_str().unwrap_or_default().to_string();
        self.add_context(Context {
            id: id.clone(),
            parent: None,
            session: session.to_string(),
            url: target["url"].as_str().unwrap_or("about:blank").to_string(),
            navigation: None,
            started: Vec::new(),
        });
        let _ = self.cdp("Page.enable", json!({}), Some(session));
        let _ = self.cdp(
            "Page.setLifecycleEventsEnabled",
            json!({"enabled": true}),
            Some(session),
        );
        if let Cdp::Ok(tree) = self.cdp("Page.getFrameTree", json!({}), Some(session)) {
            if let Some(nav) = tree["frameTree"]["frame"]["loaderId"].as_str()
                && let Some(c) = self.contexts.iter_mut().find(|c| c.id == id)
            {
                c.navigation = Some(nav.to_string());
                c.started.push(nav.to_string());
            }
            self.frame_tree(session, &tree["frameTree"]);
        }
        let _ = self.cdp("Network.enable", json!({}), Some(session));
        let _ = self.cdp("Runtime.enable", json!({}), Some(session));
    }

    fn frame_tree(&mut self, session: &str, t: &Json) {
        for c in t["childFrames"].as_array().into_iter().flatten() {
            let f = &c["frame"];
            self.add_context(Context {
                id: f["id"].as_str().unwrap_or_default().to_string(),
                parent: f["parentId"].as_str().map(str::to_string),
                session: session.to_string(),
                url: f["url"].as_str().unwrap_or_default().to_string(),
                navigation: f["loaderId"].as_str().map(str::to_string),
                started: f["loaderId"]
                    .as_str()
                    .map(str::to_string)
                    .into_iter()
                    .collect(),
            });
            self.frame_tree(session, c);
        }
    }

    /// `navigationStarted`, once per navigation.
    fn navigation_started(&mut self, context: &str, navigation: &str, url: &str) {
        let Some(c) = self.contexts.iter_mut().find(|c| c.id == context) else {
            return;
        };
        if c.started.iter().any(|n| n == navigation) {
            return;
        }
        c.started.push(navigation.to_string());
        if c.started.len() > 64 {
            c.started.remove(0);
        }
        c.navigation = Some(navigation.to_string());
        let ts = self.now();
        self.emit(
            "browsingContext.navigationStarted",
            json!({"context": context, "navigation": navigation, "timestamp": ts, "url": url}),
            Some(context),
        );
    }

    fn on_cdp_event(&mut self, method: &str, p: &Json, session: Option<&str>) {
        let s = |k: &str| p[k].as_str().unwrap_or_default().to_string();
        let ts = self.now();
        match method {
            "Target.attachedToTarget" => {
                let sid = s("sessionId");
                self.attached(&sid, &p["targetInfo"]);
            }
            "Target.detachedFromTarget" => {
                let target = s("targetId");
                self.remove_context(&target);
            }
            "Page.frameAttached" => {
                let Some(session) = session else { return };
                self.add_context(Context {
                    id: s("frameId"),
                    parent: Some(s("parentFrameId")),
                    session: session.to_string(),
                    url: "about:blank".into(),
                    navigation: None,
                    started: Vec::new(),
                });
            }
            "Page.frameDetached" => {
                let id = s("frameId");
                self.remove_context(&id);
            }
            "Page.frameNavigated" => {
                let f = &p["frame"];
                let id = f["id"].as_str().unwrap_or_default().to_string();
                let url = f["url"].as_str().unwrap_or_default().to_string();
                let nav = f["loaderId"].as_str().unwrap_or_default().to_string();
                if !self.contexts.iter().any(|c| c.id == id)
                    && let Some(session) = session
                {
                    self.add_context(Context {
                        id: id.clone(),
                        parent: f["parentId"].as_str().map(str::to_string),
                        session: session.to_string(),
                        url: url.clone(),
                        navigation: None,
                        started: Vec::new(),
                    });
                }
                self.navigation_started(&id, &nav, &url);
                if let Some(c) = self.contexts.iter_mut().find(|c| c.id == id) {
                    c.url = url.clone();
                    c.navigation = Some(nav.clone());
                }
                self.emit(
                    "browsingContext.navigationCommitted",
                    json!({"context": id, "navigation": nav, "timestamp": ts, "url": url}),
                    Some(&id),
                );
            }
            "Page.navigatedWithinDocument" => {
                let id = s("frameId");
                let url = s("url");
                self.next_nav += 1;
                let nav = format!("fragment-{}", self.next_nav);
                if let Some(c) = self.contexts.iter_mut().find(|c| c.id == id) {
                    c.url = url.clone();
                }
                if p["navigationType"].as_str() == Some("historyApi") {
                    self.emit(
                        "browsingContext.historyUpdated",
                        json!({"context": id, "timestamp": ts, "url": url}),
                        Some(&id),
                    );
                } else {
                    self.emit(
                        "browsingContext.fragmentNavigated",
                        json!({"context": id, "navigation": nav, "timestamp": ts, "url": url}),
                        Some(&id),
                    );
                }
            }
            "Page.lifecycleEvent" => {
                let event = match p["name"].as_str() {
                    Some("DOMContentLoaded") => "browsingContext.domContentLoaded",
                    Some("load") => "browsingContext.load",
                    _ => return,
                };
                let id = s("frameId");
                let url = self
                    .contexts
                    .iter()
                    .find(|c| c.id == id)
                    .map(|c| c.url.clone())
                    .unwrap_or_default();
                let nav = s("loaderId");
                self.emit(
                    event,
                    json!({"context": id, "navigation": nav, "timestamp": ts, "url": url}),
                    Some(&id),
                );
            }
            "Runtime.executionContextCreated" => {
                let Some(session) = session else { return };
                let c = &p["context"];
                let frame = c["auxData"]["frameId"].as_str().unwrap_or_default();
                let default = c["auxData"]["isDefault"].as_bool().unwrap_or(true);
                let r = Realm {
                    id: c["uniqueId"].as_str().unwrap_or_default().to_string(),
                    ctx: c["id"].as_i64().unwrap_or(0),
                    session: session.to_string(),
                    context: frame.to_string(),
                    origin: c["origin"].as_str().unwrap_or_default().to_string(),
                    sandbox: (!default).then(|| c["name"].as_str().unwrap_or_default().to_string()),
                };
                let mut ev = json!({"realm": r.id, "origin": r.origin, "type": "window", "context": r.context});
                if let Some(sb) = &r.sandbox {
                    ev["sandbox"] = json!(sb);
                }
                let ctx = r.context.clone();
                self.realms.retain(|x| x.id != r.id);
                self.realms.push(r);
                self.emit("script.realmCreated", ev, Some(&ctx));
            }
            "Runtime.executionContextDestroyed" => {
                let id = p["executionContextUniqueId"].as_str().map(str::to_string);
                let ctx = p["executionContextId"].as_i64();
                let found = self
                    .realms
                    .iter()
                    .find(|r| {
                        Some(&r.id) == id.as_ref()
                            || (Some(r.ctx) == ctx && Some(r.session.as_str()) == session)
                    })
                    .map(|r| r.id.clone());
                if let Some(r) = found {
                    self.drop_realm(&r);
                }
            }
            "Runtime.executionContextsCleared" => {
                let gone: Vec<String> = self
                    .realms
                    .iter()
                    .filter(|r| Some(r.session.as_str()) == session)
                    .map(|r| r.id.clone())
                    .collect();
                for r in gone {
                    self.drop_realm(&r);
                }
            }
            "Runtime.consoleAPICalled" => self.console(p, session),
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                // The error's message, as Firefox reports it (`Error: x` → `x`).
                let described = d["exception"]["description"]
                    .as_str()
                    .or(d["text"].as_str())
                    .unwrap_or_default();
                let first = described.lines().next().unwrap_or_default();
                let text = match first.split_once(": ") {
                    Some((name, msg))
                        if d["exception"]["subtype"] == "error"
                            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
                    {
                        msg.to_string()
                    }
                    _ => first.to_string(),
                };
                let realm = self.realm_by_ctx(session, d["executionContextId"].as_i64());
                let (realm_id, context) = realm.map(|r| (r.id, r.context)).unwrap_or_default();
                let frames = stack(&d["stackTrace"]);
                self.emit(
                    "log.entryAdded",
                    json!({
                        "type": "javascript", "level": "error", "text": text, "timestamp": ts,
                        "source": {"realm": realm_id, "context": context},
                        "stackTrace": {"callFrames": frames},
                    }),
                    Some(&context),
                );
            }
            "Network.requestWillBeSent" => self.request_sent(p),
            "Network.responseReceived" => {
                let id = s("requestId");
                let r = &p["response"];
                let data = json!({
                    "url": r["url"], "protocol": r["protocol"].as_str().unwrap_or("http/1.1"),
                    "status": r["status"], "statusText": r["statusText"].as_str().unwrap_or_default(),
                    "fromCache": false, "headers": headers(&r["headers"]),
                    "mimeType": r["mimeType"].as_str().unwrap_or_default(),
                    "bytesReceived": r["encodedDataLength"].as_u64().unwrap_or(0),
                    "headersSize": null, "bodySize": null, "content": {"size": 0},
                });
                let Some(req) = self.requests.get_mut(&id) else {
                    return;
                };
                req.response = Some(data);
                let ev = network_event(req, ts);
                let ctx = req.context.clone();
                self.emit("network.responseStarted", ev, Some(&ctx));
            }
            "Network.loadingFinished" => {
                let id = s("requestId");
                let Some(mut req) = self.requests.remove(&id) else {
                    return;
                };
                if let Some(r) = req.response.as_mut() {
                    let n = p["encodedDataLength"].as_u64().unwrap_or(0);
                    r["bodySize"] = json!(n);
                    r["content"] = json!({"size": n});
                }
                let ev = network_event(&req, ts);
                self.emit("network.responseCompleted", ev, Some(&req.context));
            }
            "Network.loadingFailed" => {
                let id = s("requestId");
                let Some(req) = self.requests.remove(&id) else {
                    return;
                };
                let mut ev = network_event(&req, ts);
                ev["errorText"] = json!(s("errorText"));
                self.emit("network.fetchError", ev, Some(&req.context));
            }
            _ => {}
        }
    }

    fn request_sent(&mut self, p: &Json) {
        let id = p["requestId"].as_str().unwrap_or_default().to_string();
        let context = p["frameId"].as_str().unwrap_or_default().to_string();
        let r = &p["request"];
        let url = r["url"].as_str().unwrap_or_default().to_string();
        let document = p["type"] == "Document";
        let navigation = if document {
            let nav = p["loaderId"].as_str().unwrap_or_default().to_string();
            self.navigation_started(&context, &nav, &url);
            Some(nav)
        } else {
            None
        };
        let redirects = match self.requests.remove(&id) {
            Some(prev) if !p["redirectResponse"].is_null() => prev.redirects + 1,
            _ => 0,
        };
        let body = r["postData"].as_str().map(|b| b.len());
        let request = json!({
            "request": id, "url": url, "method": r["method"],
            "headers": headers(&r["headers"]), "cookies": [], "headersSize": 0,
            "bodySize": body, "destination": if document { "document" } else { "" },
            "initiatorType": null,
            "timings": {
                "timeOrigin": 0, "requestTime": 0, "redirectStart": 0, "redirectEnd": 0,
                "fetchStart": 0, "dnsStart": 0, "dnsEnd": 0, "connectStart": 0, "connectEnd": 0,
                "tlsStart": 0, "requestStart": 0, "responseStart": 0, "responseEnd": 0,
            },
        });
        let req = Request {
            context: context.clone(),
            navigation,
            redirects,
            request,
            response: None,
        };
        let mut ev = network_event(&req, self.now());
        ev["initiator"] = json!({"type": if document { "other" } else { "script" }});
        self.requests.insert(id, req);
        self.emit("network.beforeRequestSent", ev, Some(&context));
    }

    /// The realm of a CDP execution context; without one, the session's top-level document.
    fn realm_by_ctx(&self, session: Option<&str>, ctx: Option<i64>) -> Option<Realm> {
        let on = |r: &&Realm| Some(r.session.as_str()) == session;
        match ctx {
            Some(ctx) => self.realms.iter().filter(on).find(|r| r.ctx == ctx),
            None => self.realms.iter().filter(on).find(|r| {
                r.sandbox.is_none()
                    && self
                        .contexts
                        .iter()
                        .any(|c| c.id == r.context && c.parent.is_none())
            }),
        }
        .cloned()
    }

    fn console(&mut self, p: &Json, session: Option<&str>) {
        let Some(realm) = self.realm_by_ctx(session, p["executionContextId"].as_i64()) else {
            return;
        };
        let method = p["type"].as_str().unwrap_or("log").to_string();
        let level = match method.as_str() {
            "debug" | "trace" => "debug",
            "warning" | "warn" => "warn",
            "error" | "assert" => "error",
            _ => "info",
        };
        let mut args = Vec::new();
        let mut text = Vec::new();
        for a in p["args"].as_array().into_iter().flatten() {
            text.push(
                a["value"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| a.get("value").map(|v| v.to_string()))
                    .or_else(|| a["unserializableValue"].as_str().map(str::to_string))
                    .or_else(|| a["description"].as_str().map(str::to_string))
                    .unwrap_or_else(|| a["type"].as_str().unwrap_or_default().to_string()),
            );
            args.push(self.remote_of(&realm, a));
        }
        let method = if method == "warning" {
            "warn".into()
        } else {
            method
        };
        let ts = self.now();
        self.emit(
            "log.entryAdded",
            json!({
                "type": "console", "method": method, "level": level, "text": text.join(" "),
                "timestamp": ts, "args": args,
                "source": {"realm": realm.id, "context": realm.context},
                "stackTrace": {"callFrames": stack(&p["stackTrace"])},
            }),
            Some(&realm.context),
        );
    }

    /// A BiDi RemoteValue for a CDP RemoteObject (objects are serialized deeply, one level).
    fn remote_of(&mut self, realm: &Realm, a: &Json) -> Json {
        if let Some(oid) = a["objectId"].as_str() {
            let r = self.cdp(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": "function () { return this }", "objectId": oid,
                    "serializationOptions": {"serialization": "deep", "maxDepth": 1},
                }),
                Some(&realm.session),
            );
            let _ = self.cdp(
                "Runtime.releaseObject",
                json!({"objectId": oid}),
                Some(&realm.session),
            );
            if let Cdp::Ok(r) = r {
                return self.bidi_value(&r["result"]["deepSerializedValue"]);
            }
            return json!({"type": "object"});
        }
        match a["type"].as_str() {
            Some("undefined") => json!({"type": "undefined"}),
            Some("object") if a["subtype"] == "null" => json!({"type": "null"}),
            Some("bigint") => {
                json!({"type": "bigint", "value": a["unserializableValue"].as_str().unwrap_or("0").trim_end_matches('n')})
            }
            Some("number") => match a["unserializableValue"].as_str() {
                Some(u) => json!({"type": "number", "value": u}),
                None => json!({"type": "number", "value": a["value"]}),
            },
            Some(t @ ("string" | "boolean")) => json!({"type": t, "value": a["value"]}),
            Some(t) => json!({"type": t}),
            None => json!({"type": "undefined"}),
        }
    }

    /// Chrome's deepSerializedValue → BiDi RemoteValue: `internalId` and `sharedId`.
    fn bidi_value(&self, v: &Json) -> Json {
        let mut v = v.clone();
        fix_value(&mut v);
        v
    }
}

/// The one session id (one session per connection).
const SESSION_ID: &str = "pageon-bidi-session";
/// pageon's one "window".
const CLIENT_WINDOW: &str = "pageon";

fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "mac",
        "windows" => "windows",
        _ => "linux",
    }
}

/// Whether merged capabilities can be met; else which one cannot.
fn matches(c: &serde_json::Map<String, Json>) -> Result<(), String> {
    if let Some(b) = c.get("browserName").and_then(Json::as_str)
        && b != "pageon"
    {
        return Err(format!(
            "browserName {b:?} cannot be met: this browser is \"pageon\""
        ));
    }
    if c.get("acceptInsecureCerts") == Some(&json!(true)) {
        return Err("acceptInsecureCerts cannot be met: pageon always checks certificates".into());
    }
    if let Some(p) = c.get("platformName").and_then(Json::as_str)
        && !p.eq_ignore_ascii_case(platform())
    {
        return Err(format!(
            "platformName {p:?} cannot be met: this is {:?}",
            platform()
        ));
    }
    if let Some(v) = c.get("browserVersion").and_then(Json::as_str)
        && v != env!("CARGO_PKG_VERSION")
    {
        return Err(format!(
            "browserVersion {v:?} cannot be met: this is pageon {}",
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(())
}

fn unsupported(method: &str) -> BidiError {
    match UNSUPPORTED.iter().find(|(m, _)| {
        if m.ends_with('.') {
            method.starts_with(m)
        } else {
            method == *m
        }
    }) {
        Some((_, why)) => BidiError::unsupported(format!("{method} is not supported: {why}")),
        None => BidiError::new(
            "unknown command",
            format!(
                "{method} is not a command pageon knows (pageon implements the WebDriver BiDi subset Puppeteer uses, plus pageon:observe and pageon:act; see `pageon cdp --help`)"
            ),
        ),
    }
}

fn headers(h: &Json) -> Json {
    Json::Array(
        h.as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| {
                json!({"name": k, "value": {"type": "string", "value": v.as_str().map_or_else(|| v.to_string(), str::to_string)}})
            })
            .collect(),
    )
}

fn network_event(r: &Request, ts: u64) -> Json {
    let mut ev = json!({
        "context": r.context, "navigation": r.navigation, "redirectCount": r.redirects,
        "request": r.request, "timestamp": ts, "isBlocked": false,
    });
    if let Some(resp) = &r.response {
        ev["response"] = resp.clone();
    }
    ev
}

fn stack(s: &Json) -> Json {
    Json::Array(
        s["callFrames"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|f| {
                json!({
                    "url": f["url"].as_str().unwrap_or_default(),
                    "functionName": f["functionName"].as_str().unwrap_or_default(),
                    "lineNumber": f["lineNumber"].as_u64().unwrap_or(0),
                    "columnNumber": f["columnNumber"].as_u64().unwrap_or(0),
                })
            })
            .collect(),
    )
}

/// Renames Chrome's fields to BiDi's, in place: `weakLocalObjectReference` → `internalId`,
/// node `backendNodeId`/`loaderId`/`frameId` → `sharedId`.
fn fix_value(v: &mut Json) {
    match v {
        Json::Object(m) => {
            if let Some(r) = m.remove("weakLocalObjectReference") {
                m.insert("internalId".into(), json!(r.to_string()));
            }
            if m.get("type") == Some(&json!("node"))
                && let Some(Json::Object(val)) = m.get_mut("value")
            {
                let b = val.remove("backendNodeId");
                let l = val.remove("loaderId");
                let f = val.remove("frameId");
                if let (Some(b), Some(Json::String(l)), Some(Json::String(f))) = (b, l, f) {
                    m.insert(
                        "sharedId".into(),
                        json!(shared_id(&f, &l, b.as_u64().unwrap_or(0))),
                    );
                }
            }
            for (_, c) in m.iter_mut() {
                fix_value(c);
            }
        }
        Json::Array(a) => a.iter_mut().for_each(fix_value),
        _ => {}
    }
}

/// A node's `sharedId`: its frame, document and node.
fn shared_id(frame: &str, loader: &str, backend: u64) -> String {
    format!("f.{frame}.d.{loader}.e.{backend}")
}

/// The parts of a `sharedId`.
fn parse_shared_id(s: &str) -> Option<(&str, &str, u64)> {
    let rest = s.strip_prefix("f.")?;
    let (frame, rest) = rest.split_once(".d.")?;
    let (loader, node) = rest.split_once(".e.")?;
    Some((frame, loader, node.parse().ok()?))
}
