//! The engine: V8 (single-threaded), limits, the watchdog, realms and the event loop.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

use pageon_dom::{Dom, NodeId};
use pageon_layout::{Stylesheet, Viewport};

use crate::cv::{self, Cx};
use crate::dom::scroll::{Amount, Scroller, Wheeled};

/// Scroll containers' offsets: `(node, x, y)`.
pub type ScrollOffsets = Vec<(NodeId, f64, f64)>;
use crate::realm::{Realm, Shared, State, Target, Timer};
use crate::{PageError, PageErrorKind};

/// Limits on page scripts.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Wall time one task (script, timer, event) may run.
    pub task: Duration,
    /// JavaScript heap limit in bytes.
    pub heap: usize,
    /// Tasks run per settle.
    pub tasks: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            task: Duration::from_secs(2),
            heap: 256 << 20,
            tasks: 10_000,
        }
    }
}

/// A document's settings.
#[derive(Debug, Clone)]
pub struct RealmConfig {
    /// Document URL.
    pub url: String,
    /// Origin.
    pub origin: String,
    /// What `navigator` says (user agent, platform, `webdriver`, `userAgentData`): from the
    /// session's identity profile.
    pub navigator: pageon_env::Navigator,
    /// Viewport.
    pub viewport: Viewport,
    /// Stylesheets loaded with the page.
    pub stylesheets: Vec<Stylesheet>,
    /// The document as a network client (origin, top-level site).
    pub client: pageon_net::Client,
    /// The session's network state.
    pub net: Rc<RefCell<pageon_net::Net>>,
    /// Accept dialogs instead of dismissing them.
    pub accept_dialogs: bool,
    /// The key of the document's `crypto` stream: 32 bytes from the environment (the OS when
    /// live, the recording when replaying).
    pub crypto_key: [u8; 32],
}

/// What settling did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Settled {
    /// Tasks run.
    pub tasks: usize,
    /// Timers still pending.
    pub pending: usize,
    /// Script requests not sent (the task limit stopped settling).
    pub pending_requests: usize,
    /// Virtual time spent (ms).
    pub virtual_ms: u64,
}

thread_local! {
    static CURRENT: RefCell<Option<(v8::IsolateHandle, std::rc::Rc<std::cell::Cell<bool>>)>> = const { RefCell::new(None) };
    /// Rejected promises without a handler, reported after the task's microtask checkpoint
    /// unless a handler is added by then.
    static REJECTED: RefCell<Vec<(v8::Global<v8::Promise>, PageError)>> = const { RefCell::new(Vec::new()) };
}

extern "C" fn promise_rejected(msg: v8::PromiseRejectMessage) {
    // Nothing may unwind out of a V8 callback.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        v8::callback_scope!(unsafe scope, &msg);
        let promise = msg.get_promise();
        let Some(context) = promise.get_creation_context(scope) else {
            return;
        };
        let scope = &mut v8::ContextScope::new(scope, context);
        match msg.get_event() {
            v8::PromiseRejectEvent::PromiseRejectWithNoHandler => {
                let value = msg
                    .get_value()
                    .unwrap_or_else(|| v8::undefined(scope).into());
                let message = v8::Exception::create_message(scope, value);
                let mut err = page_error(scope, value, Some(message));
                err.message = format!(
                    "Uncaught (in promise) {}",
                    err.message.trim_start_matches("Uncaught ")
                );
                let g = v8::Global::new(scope, promise);
                REJECTED.with(|r| r.borrow_mut().push((g, err)));
            }
            v8::PromiseRejectEvent::PromiseHandlerAddedAfterReject => {
                REJECTED.with(|r| {
                    r.borrow_mut()
                        .retain(|(p, _)| v8::Local::new(&*scope, p) != promise);
                });
            }
            _ => {}
        }
    }));
}

/// Reports the rejections still unhandled after a microtask checkpoint.
pub(crate) fn report_rejections(shared: &RefCell<Shared>) {
    let list = REJECTED.with(|r| std::mem::take(&mut *r.borrow_mut()));
    let mut sh = shared.borrow_mut();
    for (_, e) in list {
        if sh.errors.len() < 1000 {
            sh.errors.push(e);
        }
    }
}

/// Called by V8 near the heap limit: ends the running script and grants headroom so it can
/// unwind (a safe `extern "C" fn` coerces to V8's callback type).
extern "C" fn near_heap_limit(
    _data: *mut std::ffi::c_void,
    current: usize,
    _initial: usize,
) -> usize {
    CURRENT.with(|c| {
        if let Some((h, hit)) = c.borrow().as_ref() {
            hit.set(true);
            h.terminate_execution();
        }
    });
    current + (64 << 20)
}

fn platform() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        v8::V8::set_flags_from_string("--single-threaded --stack-size=900");
        let p = v8::new_single_threaded_default_platform(false).make_shared();
        v8::V8::initialize_platform(p);
        v8::V8::initialize();
    });
}

/// Context indices from here on are isolated worlds (below are documents' main worlds).
pub const WORLD_BASE: usize = 1 << 24;

/// The JavaScript engine of one page (its documents share a clock and a heap).
///
/// The V8 isolate is not entered at rest: every operation locks it for its duration, so
/// engines can be created and dropped in any order, and pages can stay live in tabs.
pub struct Engine {
    templates: Rc<Vec<v8::Global<v8::FunctionTemplate>>>,
    pub(crate) realms: Vec<(v8::Global<v8::Context>, Rc<Realm>)>,
    /// Isolated worlds' contexts (`(context, realm, world)`), addressed as `WORLD_BASE + i`.
    pub(crate) worlds: Vec<(v8::Global<v8::Context>, Rc<Realm>, u32)>,
    pub(crate) shared: Rc<RefCell<Shared>>,
    limits: Limits,
    memory_hit: Rc<std::cell::Cell<bool>>,
    pub(crate) scripts: usize,
    /// Registry keys of this engine's realms.
    keys: Vec<u32>,
    /// Values held for protocol clients (`eval`).
    pub(crate) handles: std::collections::HashMap<u32, cv::Value>,
    pub(crate) next_handle: u32,
    /// The security token of the engine's main-world contexts (one origin per engine).
    token: Option<v8::Global<v8::Value>>,
    handle: v8::IsolateHandle,
    // Last: handles above are released before the isolate goes.
    isolate: v8::SharedIsolate,
}

/// Turns a new isolate into one that is entered only while locked.
#[allow(unsafe_code)]
fn share(isolate: v8::OwnedIsolate) -> v8::SharedIsolate {
    // SAFETY: `try_into_shared` requires the embedder state attached to the isolate (context
    // slots holding `Rc<Realm>`, callbacks) to be `Send`, because a shared isolate may move
    // between threads. It never does here: the isolate lives inside `Engine`, which is `!Send`
    // (it holds `Rc`s), so it is locked and dropped only on the thread that created it. No
    // `Global::open` reference is live, the isolate is the current one, and it has no weak
    // handles, cppgc heap or snapshot creator.
    match unsafe { isolate.try_into_shared() } {
        Ok(shared) => shared,
        Err(e) => panic!("V8 isolate could not be shared: {e}"),
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        for k in self.keys.drain(..) {
            crate::realm::unregister(k);
        }
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("realms", &self.realms.len())
            .finish_non_exhaustive()
    }
}

const PRELUDE: &str = r#"(() => {
  const host = globalThis.__pageon;
  delete globalThis.__pageon;
  const NativeDate = globalThis.Date;
  const Date = function Date(...a) {
    if (!new.target) return new NativeDate(host.now()).toString();
    return Reflect.construct(NativeDate, a.length ? a : [host.now()], new.target === Date ? NativeDate : new.target);
  };
  Object.setPrototypeOf(Date, NativeDate);
  Date.prototype = NativeDate.prototype;
  Object.defineProperty(NativeDate.prototype, 'constructor', { value: Date, writable: true, configurable: true });
  Date.now = () => Math.floor(host.now());
  Date.parse = NativeDate.parse;
  Date.UTC = NativeDate.UTC;
  Object.defineProperty(globalThis, 'Date', { value: Date, writable: true, configurable: true });
  Object.defineProperty(Math, 'random', { value: host.random, writable: true, configurable: true });
})();"#;

impl Engine {
    /// A new engine whose clock starts at `now_ms` (Unix ms) and whose `Math.random` is seeded
    /// with `seed`, both from the environment.
    pub fn new(now_ms: u64, seed: u64, limits: Limits) -> Self {
        platform();
        let params = v8::CreateParams::default().heap_limits(0, limits.heap);
        let mut isolate = v8::Isolate::new(params);
        isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
        isolate.set_promise_reject_callback(promise_rejected);
        isolate.set_host_import_module_dynamically_callback(crate::modules::dynamic_import_hook);
        isolate.set_host_initialize_import_meta_object_callback(crate::modules::import_meta_hook);
        REJECTED.with(|r| r.borrow_mut().clear());
        isolate.add_near_heap_limit_callback(near_heap_limit, std::ptr::null_mut());
        let templates = {
            let hs = std::pin::pin!(v8::HandleScope::new(&mut isolate));
            let hs = &mut hs.init();
            crate::generated::templates(hs)
        };
        let handle = isolate.thread_safe_handle();
        let isolate = share(isolate);
        static NEXT_ENGINE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let shared = Shared {
            now: now_ms as f64,
            origin: now_ms as f64,
            rng: seed | 1,
            engine: NEXT_ENGINE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ..Shared::default()
        };
        Self {
            isolate,
            handle,
            templates: Rc::new(templates),
            realms: Vec::new(),
            worlds: Vec::new(),
            shared: Rc::new(RefCell::new(shared)),
            limits,
            memory_hit: Rc::new(std::cell::Cell::new(false)),
            scripts: 0,
            keys: Vec::new(),
            handles: Default::default(),
            next_handle: 0,
            token: None,
        }
    }

    /// Adds a document; returns its realm number.
    pub fn add_realm(&mut self, dom: Rc<RefCell<Dom>>, cfg: RealmConfig) -> usize {
        let id = self.realms.len();
        // `navigator.userAgentData` is [SecureContext], and exists only where the profile has
        // client hints.
        let secure = pageon_net::site::trustworthy(&cfg.client.url);
        let ua_data = cfg.navigator.ua_data.is_some() && secure;
        let realm = Rc::new(Realm {
            id,
            dom,
            st: RefCell::new(State {
                url: cfg.url,
                origin: cfg.origin,
                wrappers: Default::default(),
                slab: Vec::new(),
                same: Default::default(),
                window_same: Default::default(),
                listeners: Default::default(),
                handlers: Default::default(),
                current_script: None,
                ready_state: "loading",
                mutations: 0,
                layout: None,
                viewport: cfg.viewport,
                stylesheets: cfg.stylesheets,
                modals: Vec::new(),
                focused: None,
                navigator: cfg.navigator,
                frames_changed: false,
                started_scripts: Default::default(),
                dialog_return: Default::default(),
                window_name: String::new(),
                canvases: Default::default(),
                tools: Vec::new(),
                client: cfg.client,
                net: cfg.net,
                navigation: None,
                dialogs: Vec::new(),
                popups: Vec::new(),
                blocked: Vec::new(),
                sheets: Default::default(),
                refused: Default::default(),
                files: Default::default(),
                chooser: None,
                dialogs_more: 0,
                popups_more: 0,
                requests_made: 0,
                modules_loaded: 0,
                hovered: None,
                accept_dialogs: cfg.accept_dialogs,
                observers: Vec::new(),
                intersections: Vec::new(),
                scroll: crate::dom::scroll::ScrollState::default(),
                custom: crate::dom::custom_elements::Registry::default(),
                slots: std::collections::HashMap::new(),
                focus_seq: 0,
                modules: Vec::new(),
                import_map: Default::default(),
                history: Default::default(),
                media_lists: Vec::new(),
                media_viewport: None,
                resize_observers: Vec::new(),
                crypto: crate::dom::platform::CryptoStream::new(cfg.crypto_key),
                frame: None,
                remote: Default::default(),
            }),
            templates: self.templates.clone(),
            shared: self.shared.clone(),
            worlds: RefCell::new(Vec::new()),
        });
        let (g, kept) = self.new_context(&realm, 0, ua_data, secure);
        realm.worlds.borrow_mut().push(crate::realm::World {
            name: String::new(),
            context: kept,
            index: id,
        });
        self.shared.borrow_mut().realms.push(Rc::downgrade(&realm));
        self.realms.push((g, realm));
        id
    }

    /// This engine's id (distinct for every engine in the process).
    pub fn id(&self) -> u64 {
        self.shared.borrow().engine
    }

    /// Places a realm in a page's frame tree, as context `context`.
    pub fn set_frame(
        &mut self,
        realm: usize,
        frames: Rc<RefCell<crate::frames::Frames>>,
        context: u32,
    ) {
        if let Some(r) = self.realm(realm) {
            r.st.borrow_mut().frame = Some((frames, context));
        }
    }

    /// Whether messages wait for this engine's documents.
    pub fn has_mail(&self) -> bool {
        let Some(frames) = self.frame_tree() else {
            return false;
        };
        let id = self.id();
        let f = frames.borrow();
        f.mailbox.iter().any(|m| f.engine_of(m.to) == Some(id))
    }

    /// The frame tree the engine's documents are in.
    pub(crate) fn frame_tree(&self) -> Option<Rc<RefCell<crate::frames::Frames>>> {
        self.realms
            .iter()
            .find_map(|(_, r)| r.st.borrow().frame.as_ref().map(|(f, _)| f.clone()))
    }

    /// The page errors of one realm (documents sharing an engine have their own).
    pub fn errors_of(&self, realm: usize) -> Vec<PageError> {
        let mut sh = self.shared.borrow_mut();
        sh.tag_errors(usize::MAX);
        sh.errors
            .iter()
            .zip(&sh.error_realms)
            .filter(|(_, r)| **r == realm || **r == usize::MAX)
            .map(|(e, _)| e.clone())
            .collect()
    }

    /// A context for `realm` in `world`, with the window's interfaces, the extras and the
    /// clock and RNG prelude: the main world's, or an isolated world's (same document, its own
    /// globals and prototypes).
    fn new_context(
        &mut self,
        realm: &Rc<Realm>,
        world: u32,
        ua_data: bool,
        secure: bool,
    ) -> (v8::Global<v8::Context>, v8::Global<v8::Context>) {
        let mut locker = self.isolate.lock();
        let hs = std::pin::pin!(v8::HandleScope::new(&mut *locker));
        let hs = &mut hs.init();
        let window = v8::Local::new(
            hs,
            &self.templates[crate::generated::Iface::Window as usize],
        );
        let global_template = window.instance_template(hs);
        let context = v8::Context::new(
            hs,
            v8::ContextOptions {
                global_template: Some(global_template),
                ..Default::default()
            },
        );
        let scope = &mut v8::ContextScope::new(hs, context);
        // The documents of an engine share an origin, so their main worlds share a security
        // token and reach each other's windows; isolated worlds keep their own.
        if world == 0 {
            let token = match &self.token {
                Some(t) => v8::Local::new(scope, t),
                None => {
                    let t: v8::Local<'_, v8::Value> = v8::Object::new(scope).into();
                    self.token = Some(v8::Global::new(scope, t));
                    t
                }
            };
            context.set_security_token(token);
        }
        let global = context.global(scope);
        let key = crate::realm::register(scope, global, realm.clone(), world);
        self.keys.push(key);
        crate::generated::expose(scope, global, &self.templates);
        crate::dom::window::install_extras(scope, global);
        if !ua_data {
            hide(scope, global, "Navigator", "userAgentData");
        }
        // `document.modelContext` (WebMCP) and `crypto.randomUUID` are [SecureContext].
        if !secure {
            hide(scope, global, "Document", "modelContext");
            hide(scope, global, "Crypto", "randomUUID");
        }
        // The clock and RNG seam, then the prelude that rebinds Date and Math.random to it.
        let host = v8::Object::new(scope);
        if let (Some(k), Some(func)) = (
            v8::String::new(scope, "now"),
            v8::Function::new(scope, host_now),
        ) {
            host.set(scope, k.into(), func.into());
        }
        if let (Some(k), Some(func)) = (
            v8::String::new(scope, "random"),
            v8::Function::new(scope, host_random),
        ) {
            host.set(scope, k.into(), func.into());
        }
        if let Some(k) = v8::String::new(scope, "__pageon") {
            global.set(scope, k.into(), host.into());
        }
        {
            v8::tc_scope!(let tc, scope);
            if let Some(src) = v8::String::new(tc, PRELUDE) {
                let ran = v8::Script::compile(tc, src, None).and_then(|s| s.run(tc));
                debug_assert!(ran.is_some(), "the prelude must run");
            }
        }
        // Two handles, made while the isolate is locked: the engine's and the realm's.
        (
            v8::Global::new(scope, context),
            v8::Global::new(scope, context),
        )
    }

    /// The context index of `realm`'s isolated world `name`, created on first use (`""` is the
    /// main world). A document holds at most [`crate::realm::MAX_WORLDS`] worlds.
    pub fn world(&mut self, realm: usize, name: &str) -> Result<usize, String> {
        let Some(r) = self.realm(realm).cloned() else {
            return Err("the document is gone".into());
        };
        if let Some(w) = r.worlds.borrow().iter().find(|w| w.name == name) {
            return Ok(w.index);
        }
        let n = r.worlds.borrow().len();
        if n > crate::realm::MAX_WORLDS {
            return Err(format!(
                "a document holds at most {} isolated worlds",
                crate::realm::MAX_WORLDS
            ));
        }
        let (secure, ua_data) = {
            let st = r.st.borrow();
            let secure = pageon_net::site::trustworthy(&st.client.url);
            (secure, st.navigator.ua_data.is_some() && secure)
        };
        let world = u32::try_from(n).unwrap_or(u32::MAX);
        let (g, kept) = self.new_context(&r, world, ua_data, secure);
        let index = WORLD_BASE + self.worlds.len();
        self.worlds.push((g, r.clone(), world));
        r.worlds.borrow_mut().push(crate::realm::World {
            name: name.to_string(),
            context: kept,
            index,
        });
        Ok(index)
    }

    /// A context by index: a document's main world, or an isolated world.
    pub(crate) fn entry(&self, i: usize) -> Option<(&v8::Global<v8::Context>, &Rc<Realm>, u32)> {
        if i >= WORLD_BASE {
            self.worlds.get(i - WORLD_BASE).map(|(c, r, w)| (c, r, *w))
        } else {
            self.realms.get(i).map(|(c, r)| (c, r, 0))
        }
    }

    /// The realm's state, for the loader.
    pub fn realm(&self, id: usize) -> Option<&Rc<Realm>> {
        self.entry(id).map(|(_, r, _)| r)
    }

    /// Scripts run so far.
    pub fn scripts_run(&self) -> usize {
        self.scripts
    }

    /// Page errors so far.
    pub fn errors(&self) -> Vec<PageError> {
        self.shared.borrow().errors.clone()
    }

    /// Records a page error (e.g. a script that failed to load).
    pub fn report(
        &mut self,
        kind: PageErrorKind,
        message: impl Into<String>,
        source: impl Into<String>,
    ) {
        self.shared.borrow_mut().error(kind, message, source);
    }

    /// Whether the memory limit was hit (no more scripts run on this page).
    pub fn exhausted(&self) -> bool {
        self.memory_hit.get()
    }

    /// Runs `f` with the time-limit watchdog armed; returns whether it terminated the task.
    pub(crate) fn guarded<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> (T, bool) {
        let handle = self.handle.clone();
        CURRENT.with(|c| *c.borrow_mut() = Some((handle.clone(), self.memory_hit.clone())));
        let (tx, rx) = mpsc::channel::<()>();
        let limit = self.limits.task;
        let (r, fired) = std::thread::scope(|s| {
            let watchdog = s.spawn(move || {
                if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(limit) {
                    handle.terminate_execution();
                    true
                } else {
                    false
                }
            });
            let r = f(self);
            drop(tx);
            (r, watchdog.join().unwrap_or(false))
        });
        let locker = self.isolate.lock();
        if fired || locker.is_execution_terminating() {
            locker.cancel_terminate_execution();
        }
        (r, fired)
    }

    pub(crate) fn terminated(&mut self, fired: bool, source: &str) {
        if self.memory_hit.get() {
            if !self
                .shared
                .borrow()
                .errors
                .iter()
                .any(|e| e.kind == PageErrorKind::MemoryLimit)
            {
                self.report(
                    PageErrorKind::MemoryLimit,
                    "script terminated: memory limit",
                    source,
                );
            }
        } else if fired {
            self.report(
                PageErrorKind::TimeLimit,
                "script terminated: time limit",
                source,
            );
        }
    }

    /// Runs a classic script in a realm (`script` is its element, for `document.currentScript`).
    pub fn run_script(&mut self, realm: usize, source: &str, name: &str, script: Option<NodeId>) {
        if self.exhausted() || realm >= self.realms.len() {
            return;
        }
        self.scripts += 1;
        let (_, fired) = self.guarded(|e| {
            let (ctx, r) = (&e.realms[realm].0, e.realms[realm].1.clone());
            r.st.borrow_mut().current_script = script;
            let mut locker = e.isolate.lock();
            let hs = std::pin::pin!(v8::HandleScope::new(&mut *locker));
            let hs = &mut hs.init();
            let context = v8::Local::new(hs, ctx);
            let scope = &mut v8::ContextScope::new(hs, context);
            {
                v8::tc_scope!(let tc, scope);
                let (Some(code), Some(n)) =
                    (v8::String::new(tc, source), v8::String::new(tc, name))
                else {
                    return;
                };
                let origin = v8::ScriptOrigin::new(
                    tc,
                    n.into(),
                    0,
                    0,
                    false,
                    0,
                    None,
                    false,
                    false,
                    false,
                    None,
                );
                let ok = v8::Script::compile(tc, code, Some(&origin))
                    .and_then(|s| s.run(tc))
                    .is_some();
                if !ok
                    && !tc.is_execution_terminating()
                    && let Some(ex) = tc.exception()
                {
                    let msg = tc.message();
                    let err = page_error(tc, ex, msg);
                    r.shared.borrow_mut().errors.push(err);
                }
            }
            if !scope.is_execution_terminating() {
                scope.perform_microtask_checkpoint();
                let mut cx = Cx {
                    scope,
                    realm: r.clone(),
                    world: 0,
                };
                notify_observers(&mut cx);
            }
            r.st.borrow_mut().current_script = None;
        });
        report_rejections(&self.shared);
        self.terminated(fired, name);
        self.shared.borrow_mut().tag_errors(realm);
    }

    /// Records a page error of one document (e.g. its script failed to load).
    pub fn report_in(
        &mut self,
        realm: usize,
        kind: PageErrorKind,
        message: impl Into<String>,
        source: impl Into<String>,
    ) {
        let mut sh = self.shared.borrow_mut();
        sh.tag_errors(usize::MAX);
        sh.error(kind, message, source);
        sh.tag_errors(realm);
    }

    /// Sets `document.readyState` and fires `readystatechange` at the document.
    pub fn set_ready_state(&mut self, realm: usize, state: &'static str) {
        let Some(r) = self.realm(realm).cloned() else {
            return;
        };
        r.st.borrow_mut().ready_state = state;
        let root = r.dom.borrow().root();
        self.fire(realm, Target::Node(root), "readystatechange", false);
    }

    /// Fires a simple event (`DOMContentLoaded`, `load`) at a target.
    pub fn fire(&mut self, realm: usize, target: Target, ty: &str, bubbles: bool) {
        if self.exhausted() || realm >= self.realms.len() {
            return;
        }
        let (_, fired) = self.guarded(|e| {
            e.with_cx(realm, |cx| {
                let _ = crate::dom::event_target::fire(cx, target, ty, bubbles, false);
            });
        });
        self.terminated(fired, ty);
    }

    /// Runs `f` with a call context in a realm, then a microtask checkpoint.
    pub fn with_cx<T>(
        &mut self,
        realm: usize,
        f: impl FnOnce(&mut Cx<'_, '_, '_>) -> T,
    ) -> Option<T> {
        let (ctx, r, world) = if realm >= WORLD_BASE {
            let (c, r, w) = self.worlds.get(realm - WORLD_BASE)?;
            (c, r.clone(), *w)
        } else {
            let (c, r) = self.realms.get(realm)?;
            (c, r.clone(), 0)
        };
        let r_id = r.id;
        let mut locker = self.isolate.lock();
        let hs = std::pin::pin!(v8::HandleScope::new(&mut *locker));
        let hs = &mut hs.init();
        let context = v8::Local::new(hs, ctx);
        let scope = &mut v8::ContextScope::new(hs, context);
        let out = {
            let mut cx = Cx {
                scope,
                realm: r.clone(),
                world,
            };
            f(&mut cx)
        };
        if !scope.is_execution_terminating() {
            scope.perform_microtask_checkpoint();
            let mut cx = Cx {
                scope,
                realm: r,
                world,
            };
            notify_observers(&mut cx);
        }
        report_rejections(&self.shared);
        self.shared.borrow_mut().tag_errors(r_id);
        Some(out)
    }

    /// Runs the event loop on the virtual clock until nothing is due within `window_ms`, or the
    /// task limit is reached: timers due now, then script requests in issue order (sent
    /// through the session's network state and `env`, outside any task, so network time never
    /// counts against a task's time limit), then the next timer.
    ///
    /// Realtime connections: queued opens, frames and closes are performed in order with the
    /// requests; while connections are open, the messages that arrived are collected whenever
    /// nothing is due now and run one task each. When the page sent a frame, it waits up to
    /// [`REPLY_WAIT_MS`](crate::dom::realtime::REPLY_WAIT_MS) of real time for a first reply
    /// (no time at all in a replay).
    pub fn settle(&mut self, window_ms: f64, env: &dyn pageon_env::Env) -> Settled {
        let start = self.shared.borrow().now;
        let mut s = self.run_loop(window_ms, env);
        let reply = {
            let mut sh = self.shared.borrow_mut();
            std::mem::take(&mut sh.streams.sent) && !sh.streams.open.is_empty()
        };
        if reply && !self.exhausted() {
            let n = self.stream_poll(env, crate::dom::realtime::REPLY_WAIT_MS);
            if n > 0 {
                let left = (start + window_ms - self.shared.borrow().now).max(0.0);
                let more = self.run_loop(left, env);
                s.tasks += n + more.tasks;
                s.pending = more.pending;
                s.pending_requests = more.pending_requests;
            }
            self.shared.borrow_mut().streams.sent = false;
        }
        s.virtual_ms = (self.shared.borrow().now - start).max(0.0) as u64;
        s
    }

    fn run_loop(&mut self, window_ms: f64, env: &dyn pageon_env::Env) -> Settled {
        let start = self.shared.borrow().now;
        let deadline = start + window_ms;
        let mut tasks = 0;
        while tasks < self.limits.tasks && !self.exhausted() {
            // Inserted scripts and dynamic imports run before any other task.
            let ran = self.run_scripts(env);
            if ran > 0 {
                tasks += ran;
                continue;
            }
            // Messages posted to this engine's documents, in posting order.
            let delivered = self.deliver_mail();
            if delivered > 0 {
                tasks += delivered;
                continue;
            }
            let due_now = {
                let sh = self.shared.borrow();
                sh.timers
                    .first_key_value()
                    .is_some_and(|(k, _)| k.0 as f64 / 1000.0 <= sh.now)
            };
            if !due_now {
                let op = self.shared.borrow_mut().streams.ops.pop_front();
                if let Some(op) = op {
                    tasks += 1;
                    self.stream_op(op, env);
                    continue;
                }
                let pending = self.shared.borrow_mut().requests.pop_front();
                if let Some(p) = pending {
                    tasks += 1;
                    self.complete(p, env);
                    continue;
                }
                if !self.shared.borrow().streams.open.is_empty() {
                    let n = self.stream_poll(env, 0);
                    if n > 0 {
                        tasks += n;
                        continue;
                    }
                }
            }
            let next = {
                let mut sh = self.shared.borrow_mut();
                let Some((&key, _)) = sh.timers.first_key_value() else {
                    break;
                };
                let mut key = key;
                // An idle callback waits for the other timers due by then.
                if sh.timers.get(&key).is_some_and(|t| t.idle)
                    && let Some((&k, _)) = sh
                        .timers
                        .iter()
                        .find(|(k, t)| !t.idle && k.0 <= key.0.max((sh.now * 1000.0) as u64))
                {
                    key = k;
                }
                let due = key.0 as f64 / 1000.0;
                if due > deadline {
                    break;
                }
                sh.now = sh.now.max(due);
                sh.timers.remove(&key)
            };
            let Some(timer) = next else { break };
            tasks += 1;
            let realm = timer.realm;
            let (_, fired) = self.guarded(|e| {
                e.with_cx(realm, |cx| run_timer(cx, &timer));
            });
            self.terminated(fired, "timer");
            self.shared.borrow_mut().tag_errors(realm);
            if let Some(every) = timer.interval {
                let mut sh = self.shared.borrow_mut();
                if !fired && !sh.cancelled.contains(&timer.id) {
                    // Rescheduling keeps the same callback; the timer is moved, not cloned
                    // (cloning a handle needs the isolate's lock).
                    sh.schedule(timer, every.max(1.0));
                }
            }
        }
        let sh = self.shared.borrow();
        Settled {
            tasks,
            pending: sh.timers.len(),
            pending_requests: sh.requests.len(),
            virtual_ms: (sh.now - start).max(0.0) as u64,
        }
    }

    /// Performs one queued realtime operation (outside tasks; the page's events in tasks).
    fn stream_op(&mut self, op: crate::dom::realtime::Op, env: &dyn pageon_env::Env) {
        use crate::dom::realtime::{self as rt, Op};
        use pageon_env::StreamCall;
        match op {
            Op::Open { realm, obj } => {
                let Some(call) = self.with_cx(realm, |cx| rt::handshake(cx, obj)).flatten() else {
                    return;
                };
                let reply = env.stream(&call);
                let (id, fired) =
                    self.guarded(|e| e.with_cx(realm, |cx| rt::opened(cx, obj, reply)).flatten());
                self.terminated(fired, "network task");
                if let Some(id) = id {
                    self.shared
                        .borrow_mut()
                        .streams
                        .open
                        .insert(id, (realm, obj));
                }
            }
            Op::Send {
                realm,
                obj,
                bytes,
                text,
            } => {
                let len = bytes.len();
                let Some(call) = self
                    .with_cx(realm, |cx| rt::send_call(cx, obj, bytes, text))
                    .flatten()
                else {
                    return;
                };
                let result = env.stream(&call);
                let (close, fired) = self.guarded(|e| {
                    e.with_cx(realm, |cx| rt::sent(cx, obj, len, result))
                        .flatten()
                });
                self.terminated(fired, "network task");
                if let Some((id, why)) = close {
                    self.shared.borrow_mut().streams.open.remove(&id);
                    let reason: String = why.chars().take(60).collect();
                    let _ = env.stream(&StreamCall::Close {
                        id,
                        code: 1008,
                        reason,
                    });
                }
            }
            Op::Close {
                realm,
                obj,
                code,
                reason,
            } => {
                let Some(id) = self.with_cx(realm, |cx| rt::conn(cx, obj)).flatten() else {
                    return;
                };
                let _ = env.stream(&StreamCall::Close { id, code, reason });
                // An event stream ends here; a WebSocket when the server's close arrives.
                let sse = self.realm(realm).is_some_and(|r| {
                    matches!(
                        r.st.borrow().slot(obj),
                        Some(crate::realm::Slot::EventSource(_))
                    )
                });
                if sse {
                    self.shared.borrow_mut().streams.open.remove(&id);
                }
            }
        }
    }

    /// Runs the messages that arrived as tasks, in arrival order (a live environment waits
    /// up to `wait_ms` of real time for the first); the number of tasks run.
    fn stream_poll(&mut self, env: &dyn pageon_env::Env, wait_ms: u64) -> usize {
        use pageon_env::{StreamCall, StreamEvent, StreamReply};
        let now = self.shared.borrow().now as u64;
        let Ok(StreamReply::Events(events)) = env.stream(&StreamCall::Poll { now, wait_ms }) else {
            return 0;
        };
        let mut n = 0;
        for (id, ev) in events {
            let entry = self.shared.borrow().streams.open.get(&id).copied();
            let Some((realm, obj)) = entry else { continue };
            if matches!(ev, StreamEvent::Closed { .. }) {
                self.shared.borrow_mut().streams.open.remove(&id);
            }
            n += 1;
            let (_, fired) = self.guarded(|e| {
                e.with_cx(realm, |cx| crate::dom::realtime::deliver(cx, obj, ev));
            });
            self.terminated(fired, "message");
            self.shared.borrow_mut().tag_errors(realm);
        }
        n
    }

    /// Closes every realtime connection of the page (its documents are going away).
    pub fn close_streams(&mut self, env: &dyn pageon_env::Env) {
        let ids: Vec<u64> = {
            let mut sh = self.shared.borrow_mut();
            sh.streams.ops.clear();
            sh.streams.open.drain().map(|(id, _)| id).collect()
        };
        for id in ids {
            let _ = env.stream(&pageon_env::StreamCall::Close {
                id,
                code: 1001,
                reason: String::new(),
            });
        }
    }

    /// Whether the page holds realtime connections (or is about to open one).
    pub fn has_streams(&self) -> bool {
        let sh = self.shared.borrow();
        !sh.streams.open.is_empty() || !sh.streams.ops.is_empty()
    }

    /// Sends one script request and runs its completion as a task.
    fn complete(&mut self, p: crate::realm::Pending, env: &dyn pageon_env::Env) {
        let Some(realm) = self.realm(p.realm).cloned() else {
            return;
        };
        if crate::dom::net::aborted_before_send(&realm, &p.done) {
            return;
        }
        let now = self.shared.borrow().now as u64;
        let net = realm.st.borrow().net.clone();
        let result = net.borrow_mut().fetch(env, p.req, now);
        let (_, fired) = self.guarded(|e| {
            e.with_cx(p.realm, |cx| {
                let _ = crate::dom::net::complete(cx, p.done, result);
            });
        });
        self.terminated(fired, "network task");
    }

    /// Runs one input task (`f` gets the realm's call context) under the time limit, with a
    /// microtask checkpoint; `None` if the realm is gone or the engine is exhausted.
    fn input<T>(
        &mut self,
        realm: usize,
        what: &str,
        f: impl FnOnce(&mut Cx<'_, '_, '_>) -> crate::cv::R<T>,
    ) -> Option<T> {
        if self.exhausted() || realm >= self.realms.len() {
            return None;
        }
        let (out, fired) = self.guarded(|e| e.with_cx(realm, f));
        self.terminated(fired, what);
        out.and_then(Result::ok)
    }

    /// Records a user activation (HTML's transient activation): trusted clicks, keys and
    /// selections.
    fn activate(&self, realm: usize) {
        let now = {
            let sh = self.shared.borrow();
            sh.now - sh.origin
        };
        if let Some(r) = self.realm(realm) {
            r.st.borrow_mut().history.activated_at = Some(now);
        }
    }

    /// The history requests the page made since the last call (applied by the tab).
    pub fn take_history(&mut self, realm: usize) -> Vec<crate::dom::session::HistoryOp> {
        self.realm(realm)
            .map(|r| std::mem::take(&mut r.st.borrow_mut().history.ops))
            .unwrap_or_default()
    }

    /// Tells the document where it is in the tab's history.
    pub fn set_history(&mut self, realm: usize, index: u32, length: u32) {
        if let Some(r) = self.realm(realm) {
            let mut st = r.st.borrow_mut();
            st.history.index = index;
            st.history.length = length;
        }
    }

    /// The document's current history state and `scrollRestoration` (manual: true).
    pub fn history_state(&self, realm: usize) -> (Option<crate::dom::session::State>, bool) {
        self.realm(realm).map_or((None, false), |r| {
            let st = r.st.borrow();
            (st.history.state.clone(), st.history.manual_scroll)
        })
    }

    /// Sets the state of a document restored from history (a cross-document traversal), before
    /// its scripts read it.
    pub fn set_history_state(&mut self, realm: usize, state: Option<crate::dom::session::State>) {
        if let Some(r) = self.realm(realm) {
            r.st.borrow_mut().history.state = state;
        }
    }

    /// Moves the document to one of its own history entries: URL, state, `popstate` and
    /// (fragment change) `hashchange`, then the entry's viewport scroll if given
    /// (`scrollRestoration` `auto`), after the page re-rendered for it.
    pub fn restore_history(
        &mut self,
        realm: usize,
        entry: (&str, Option<crate::dom::session::State>),
        (index, length): (u32, u32),
        scroll: Option<(f64, f64)>,
    ) {
        let (url, state) = entry;
        let _ = self.input(realm, "popstate", |cx| {
            crate::dom::session::restore(cx, url, state, index, length)?;
            if let Some((x, y)) = scroll {
                crate::dom::scroll::set(cx, Scroller::Viewport, x, y);
            }
            Ok(())
        });
    }

    /// The document's URL (it changes in place with the history API and fragments).
    pub fn url(&self, realm: usize) -> Option<String> {
        self.realm(realm).map(|r| r.st.borrow().url.clone())
    }

    /// A trusted click on an element, at `point` (viewport coordinates; default its centre):
    /// pointer, mouse, focus, click, then its default action.
    pub fn click(&mut self, realm: usize, node: NodeId, point: Option<(f64, f64)>) {
        self.activate(realm);
        let _ = self.input(realm, "click", |cx| {
            crate::dom::input::click(cx, node, point)
        });
    }

    /// Whether `node` is an enabled file input, and whether it takes several files.
    pub fn file_input(&self, realm: usize, node: NodeId) -> Option<bool> {
        let r = self.realm(realm)?;
        let dom = r.dom.borrow();
        let n = dom.get(node)?;
        (n.is_html("input")
            && pageon_dom::forms::input_type(&dom, node) == "file"
            && !pageon_dom::forms::disabled(&dom, node))
        .then(|| n.attr("multiple").is_some())
    }

    /// The file input whose chooser the last activation opened, if any (cleared).
    pub fn take_chooser(&mut self, realm: usize) -> Option<NodeId> {
        self.realm(realm)?.st.borrow_mut().chooser.take()
    }

    /// Puts files into a file input as a person picking them does (`input`, `change`).
    /// Each file is `(name, type, bytes, lastModified ms)`.
    pub fn set_files(
        &mut self,
        realm: usize,
        node: NodeId,
        files: Vec<(String, String, Vec<u8>, i64)>,
    ) {
        self.activate(realm);
        let files = files
            .into_iter()
            .map(|(n, t, b, m)| crate::dom::blobs::BlobData::file(n, t, b, m))
            .collect();
        let _ = self.input(realm, "upload", |cx| {
            crate::dom::input::set_files(cx, node, files)
        });
    }

    /// Replaces an editable element's text, typing it key by key.
    pub fn type_text(&mut self, realm: usize, node: NodeId, text: &str) {
        self.activate(realm);
        let _ = self.input(realm, "type", |cx| {
            crate::dom::input::type_text(cx, node, text)
        });
    }

    /// Presses a key on the focused element.
    pub fn press(&mut self, realm: usize, key: &str) {
        self.activate(realm);
        let _ = self.input(realm, "press", |cx| crate::dom::input::press(cx, key));
    }

    /// Selects the options of a `<select>` by value or label; returns the values that matched
    /// no option (then nothing changed).
    pub fn select(&mut self, realm: usize, node: NodeId, values: &[String]) -> Vec<String> {
        self.activate(realm);
        self.input(realm, "select", |cx| {
            crate::dom::input::select(cx, node, values)
        })
        .unwrap_or_default()
    }

    /// The option labels of a `<select>`.
    pub fn option_labels(&mut self, realm: usize, node: NodeId) -> Vec<String> {
        self.with_cx(realm, |cx| crate::dom::input::option_labels(cx, node))
            .unwrap_or_default()
    }

    /// Upgrades the document's custom elements that have a definition and were created since
    /// (the parser's elements, between scripts). Cheap when nothing is defined.
    pub fn upgrade_custom_elements(&mut self, realm: usize) {
        let active = self
            .realms
            .get(realm)
            .is_some_and(|r| r.1.st.borrow().custom.active());
        if active {
            let _ = self.input(realm, "upgrade", |cx| {
                let root = cx.dom().root();
                crate::dom::custom_elements::upgrade_tree(cx, root)
            });
        }
    }

    /// Scrolls `node` into view through every scroll container (centred, if it is not fully
    /// visible already): what a person does before acting on it. Returns whether anything moved.
    pub fn scroll_into_view(&mut self, realm: usize, node: NodeId) -> bool {
        self.input(realm, "scroll", |cx| {
            Ok(crate::dom::scroll::into_view(
                cx, node, "center", "center", true,
            ))
        })
        .unwrap_or(false)
    }

    /// A user scroll by the wheel: a trusted `wheel` event at `point` (default: the centre of
    /// `target`, else the last pointer position, else the viewport's centre), then — unless the
    /// page cancels it — `amount` on the innermost scroller from `target` (or what is under
    /// the point) that can move that way, ending at the viewport.
    pub fn wheel(
        &mut self,
        realm: usize,
        point: Option<(f64, f64)>,
        target: Option<NodeId>,
        amount: Amount,
    ) -> Wheeled {
        self.input(realm, "wheel", |cx| {
            let p = point
                .or_else(|| {
                    target.and_then(|t| {
                        cx.realm.layout().client_rect(t).map(|r| {
                            (
                                f64::from(r.x + r.width / 2.0),
                                f64::from(r.y + r.height / 2.0),
                            )
                        })
                    })
                })
                .or_else(|| cx.st().scroll.pointer);
            let p = p.unwrap_or_else(|| {
                let v = cx.st().viewport;
                (f64::from(v.width) / 2.0, f64::from(v.height) / 2.0)
            });
            crate::dom::scroll::wheel(cx, p, target, amount)
        })
        .unwrap_or(Wheeled::AtEnd)
    }

    /// Scrolls to a URL fragment (after a load whose URL has one). Returns whether it moved.
    pub fn scroll_to_fragment(&mut self, realm: usize, fragment: &str) -> bool {
        self.input(realm, "scroll", |cx| {
            Ok(crate::dom::scroll::to_fragment(cx, fragment))
        })
        .unwrap_or(false)
    }

    /// The scroll offset of the viewport (`None`) or of a scroll container.
    pub fn scroll_offset(&mut self, realm: usize, target: Option<NodeId>) -> (f64, f64) {
        self.with_cx(realm, |cx| {
            let s = target.map_or(Scroller::Viewport, |t| {
                crate::dom::scroll::scroller_of(cx, t)
            });
            crate::dom::scroll::offset(cx, s)
        })
        .unwrap_or_default()
    }

    /// The realm's scroll positions: the viewport's, and scroll containers' (`(node, x, y)`).
    pub fn scroll_state(&self, realm: usize) -> ((f64, f64), ScrollOffsets) {
        self.realms.get(realm).map_or_else(Default::default, |r| {
            let st = r.1.st.borrow();
            (
                st.scroll.viewport,
                st.scroll
                    .elements
                    .iter()
                    .map(|(&k, &(x, y))| (k, x, y))
                    .collect(),
            )
        })
    }

    /// Moves the pointer onto an element (at `point`; default its centre).
    pub fn hover(&mut self, realm: usize, node: NodeId, point: Option<(f64, f64)>) {
        let _ = self.input(realm, "hover", |cx| {
            crate::dom::input::hover(cx, node, point)
        });
    }

    /// Whether an element takes typed text: `Some(true)` for form fields, `Some(false)` for
    /// `contenteditable`, `None` otherwise.
    pub fn editable(&mut self, realm: usize, node: NodeId) -> Option<bool> {
        self.with_cx(realm, |cx| crate::dom::input::editable(cx, node))
            .flatten()
    }

    /// Lets `ms` of virtual time pass, running what falls due.
    ///
    /// While the page holds realtime connections, real time passes too: up to `ms`, until a
    /// first message arrives (none in a replay, where messages come at their recorded times).
    pub fn wait(&mut self, ms: u64, env: &dyn pageon_env::Env) -> Settled {
        let target = self.shared.borrow().now + ms as f64;
        let mut s = self.settle(ms as f64, env);
        if !self.shared.borrow().streams.open.is_empty() && !self.exhausted() {
            let n = self.stream_poll(env, ms);
            if n > 0 {
                let left = (target - self.shared.borrow().now).max(0.0);
                let more = self.settle(left, env);
                s.tasks += n + more.tasks;
            }
        }
        let mut sh = self.shared.borrow_mut();
        sh.now = sh.now.max(target);
        s
    }

    /// When focus last moved in a realm, if an element there still holds it (else 0).
    pub fn focus_seq(&self, realm: usize) -> u64 {
        self.realm(realm).map_or(0, |r| {
            let st = r.st.borrow();
            if st.focused.is_some() {
                st.focus_seq
            } else {
                0
            }
        })
    }

    /// Console messages (level, text) since the last call.
    pub fn take_console(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.shared.borrow_mut().console)
    }

    /// Virtual ms until the next timer is due (`None` when no timer is pending).
    pub fn next_due_ms(&self) -> Option<u64> {
        let sh = self.shared.borrow();
        let &(due_us, _) = sh.timers.keys().next()?;
        let now_us = (sh.now * 1000.0) as u64;
        Some(due_us.saturating_sub(now_us).div_ceil(1000))
    }

    /// The virtual time (ms since the Unix epoch).
    pub fn now_ms(&self) -> u64 {
        self.shared.borrow().now as u64
    }

    /// What a realm's document asked for since the last call: a navigation, dialogs, popups.
    pub fn take_requests(
        &mut self,
        realm: usize,
    ) -> (
        Option<crate::realm::NavRequest>,
        Vec<crate::realm::Dialog>,
        Vec<String>,
    ) {
        self.realm(realm).map_or_else(Default::default, |r| {
            let mut st = r.st.borrow_mut();
            (
                st.navigation.take(),
                std::mem::take(&mut st.dialogs),
                std::mem::take(&mut st.popups),
            )
        })
    }

    /// The controls whose validation blocked a submission (or failed `reportValidity()`) in a
    /// realm since the last call.
    pub fn take_blocked(&mut self, realm: usize) -> Vec<crate::dom::validation::Blocked> {
        self.realm(realm)
            .map(|r| std::mem::take(&mut r.st.borrow_mut().blocked))
            .unwrap_or_default()
    }

    /// Dialogs and popups beyond the per-action cap in a realm since the last call.
    pub fn take_dropped(&mut self, realm: usize) -> (usize, usize) {
        self.realm(realm).map_or((0, 0), |r| {
            let mut st = r.st.borrow_mut();
            (
                std::mem::take(&mut st.dialogs_more),
                std::mem::take(&mut st.popups_more),
            )
        })
    }

    /// Whether script changed an iframe in a realm since the last call (frames must be loaded).
    pub fn take_frames_changed(&mut self, realm: usize) -> bool {
        self.realm(realm)
            .is_some_and(|r| std::mem::take(&mut r.st.borrow_mut().frames_changed))
    }

    /// Open modal dialogs of a realm (for layout and the views).
    pub fn modals(&self, realm: usize) -> Vec<NodeId> {
        self.realm(realm)
            .map(|r| r.st.borrow().modals.clone())
            .unwrap_or_default()
    }
}

fn run_timer(cx: &mut Cx<'_, '_, '_>, t: &Timer) {
    let f = cx.local(&t.cb);
    if f.is_function() {
        // A timer runs in the world that set it (`this` is that world's window).
        let world = cx.world_of(&t.cb);
        cx.in_world(world, |cx| {
            let global = cx.scope.get_current_context().global(cx.scope);
            let args: Vec<v8::Local<v8::Value>> = if t.idle {
                let d = crate::dom::platform::idle_deadline(cx);
                vec![cx.wrap(d).into()]
            } else if t.frame {
                let now = {
                    let sh = cx.realm.shared.borrow();
                    sh.now - sh.origin
                };
                vec![v8::Number::new(cx.scope, now).into()]
            } else {
                t.args.iter().map(|a| cx.local(a)).collect()
            };
            let _ = cx.invoke(&t.cb, global.into(), &args);
        });
    } else if f.is_string() {
        // `setTimeout("code", …)`: compile and run as a script.
        let code = cv::string(cx, f).unwrap_or_default();
        let scope = &mut *cx.scope;
        v8::tc_scope!(let tc, scope);
        if let Some(src) = v8::String::new(tc, &code) {
            let ok = v8::Script::compile(tc, src, None)
                .and_then(|s| s.run(tc))
                .is_some();
            if !ok
                && !tc.is_execution_terminating()
                && let Some(ex) = tc.exception()
            {
                let msg = tc.message();
                let err = page_error(tc, ex, msg);
                cx.realm.shared.borrow_mut().errors.push(err);
            }
        }
    }
}

/// A page error from a V8 exception and its message.
pub fn page_error(
    scope: &v8::PinScope<'_, '_>,
    ex: v8::Local<'_, v8::Value>,
    msg: Option<v8::Local<'_, v8::Message>>,
) -> PageError {
    let text = msg
        .map(|m| m.get(scope).to_rust_string_lossy(scope))
        .unwrap_or_else(|| ex.to_rust_string_lossy(scope));
    let source = msg
        .and_then(|m| m.get_script_resource_name(scope))
        .map(|n| n.to_rust_string_lossy(scope))
        .unwrap_or_default();
    let line = msg.and_then(|m| m.get_line_number(scope)).unwrap_or(0);
    // V8 reports "no column" (an error raised natively) as -1, which arrives as `usize::MAX`:
    // it wraps to 0, "unknown".
    let column = msg.map_or(0, |m| m.get_start_column().wrapping_add(1));
    PageError {
        kind: PageErrorKind::Exception,
        message: text,
        source,
        line: u32::try_from(line).unwrap_or(0),
        column: u32::try_from(column).unwrap_or(0),
    }
}

/// Notifies mutation observers until no callback queues more records (bounded), with a
/// microtask checkpoint after each round.
fn notify_observers(cx: &mut Cx<'_, '_, '_>) {
    crate::dom::sheets::sync(cx);
    let mut resize_pass = 0;
    for _ in 0..100 {
        let _ = crate::dom::custom_elements::react(cx);
        let mutations = crate::dom::observers::deliver(cx);
        // HTML's rendering update order: scroll events, then intersection observations.
        let slots = crate::dom::shadow::deliver_slotchange(cx);
        let scrolled = crate::dom::scroll::deliver(cx);
        let intersections = crate::dom::intersection::deliver(cx);
        let media = crate::dom::platform::deliver_media(cx);
        let resized = crate::dom::platform::deliver_resize(cx, resize_pass);
        if matches!(resized, Ok(true)) {
            resize_pass += 1;
        }
        let ran = matches!(mutations, Ok(true))
            || matches!(slots, Ok(true))
            || matches!(scrolled, Ok(true))
            || matches!(intersections, Ok(true))
            || matches!(media, Ok(true))
            || matches!(resized, Ok(true));
        if !ran || cx.scope.is_execution_terminating() {
            break;
        }
        cx.scope.perform_microtask_checkpoint();
    }
}

fn host_now(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    if let Some(r) = crate::realm::current(scope) {
        rv.set_double(r.shared.borrow().now);
    }
}

fn host_random(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    if let Some(r) = crate::realm::current(scope) {
        rv.set_double(r.shared.borrow_mut().random());
    }
}

/// Removes `member` from `iface.prototype`: a member this realm does not expose (exposure is
/// decided per realm, like [SecureContext]).
fn hide(
    scope: &mut v8::PinScope<'_, '_>,
    global: v8::Local<'_, v8::Object>,
    iface: &str,
    member: &str,
) {
    let (Some(i), Some(p), Some(m)) = (
        v8::String::new(scope, iface),
        v8::String::new(scope, "prototype"),
        v8::String::new(scope, member),
    ) else {
        return;
    };
    let proto = global
        .get(scope, i.into())
        .and_then(|c| c.to_object(scope))
        .and_then(|c| c.get(scope, p.into()))
        .and_then(|p| p.to_object(scope));
    if let Some(proto) = proto {
        let _ = proto.delete(scope, m.into());
    }
}
