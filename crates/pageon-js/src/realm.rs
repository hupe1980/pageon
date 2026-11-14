//! A realm: one document's global, wrappers, listeners and objects; and the page-wide clock.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use pageon_dom::{Dom, NodeId};
use pageon_layout::{Layout, Stylesheet, Viewport};

use crate::cv::Value;
use crate::generated::Iface;
use crate::{PageError, PageErrorKind};

thread_local! {
    /// Live realms by key: a context finds its realm through a private property of its global
    /// (context slots use weak handles, which shared isolates do not support).
    static REALMS: RefCell<HashMap<u32, Rc<Realm>>> = RefCell::new(HashMap::new());
    static NEXT_KEY: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}

fn realm_key<'s>(scope: &v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let name = v8::String::new(scope, "pageon.realm");
    v8::Private::for_api(scope, name)
}

fn world_key<'s>(scope: &v8::PinScope<'s, '_>) -> v8::Local<'s, v8::Private> {
    let name = v8::String::new(scope, "pageon.world");
    v8::Private::for_api(scope, name)
}

/// The most isolated worlds one document may have.
pub const MAX_WORLDS: usize = 16;

/// Registers a realm's context and tags its global with the realm's key and its world (0 is
/// the main world; isolated worlds are other contexts of the same realm).
pub(crate) fn register(
    scope: &v8::PinScope<'_, '_>,
    global: v8::Local<'_, v8::Object>,
    realm: Rc<Realm>,
    world: u32,
) -> u32 {
    let key = NEXT_KEY.with(|k| {
        let v = k.get();
        k.set(v.wrapping_add(1).max(1));
        v
    });
    REALMS.with(|r| r.borrow_mut().insert(key, realm));
    let private = realm_key(scope);
    let value = v8::Integer::new_from_unsigned(scope, key);
    global.set_private(scope, private, value.into());
    if world > 0 {
        let private = world_key(scope);
        let value = v8::Integer::new_from_unsigned(scope, world);
        global.set_private(scope, private, value.into());
    }
    key
}

/// The world of a context, by its global (0: the main world).
pub(crate) fn world_of_global(
    scope: &v8::PinScope<'_, '_>,
    global: v8::Local<'_, v8::Object>,
) -> u32 {
    global
        .get_private(scope, world_key(scope))
        .filter(|v| v.is_uint32())
        .and_then(|v| v.uint32_value(scope))
        .unwrap_or(0)
}

/// The world of the current context.
pub fn current_world(scope: &v8::PinScope<'_, '_>) -> u32 {
    let global = scope.get_current_context().global(scope);
    world_of_global(scope, global)
}

/// Forgets a realm (its engine is going).
pub(crate) fn unregister(key: u32) {
    let _ = REALMS.try_with(|r| r.borrow_mut().remove(&key));
}

/// The realm of the current context.
pub fn current(scope: &v8::PinScope<'_, '_>) -> Option<Rc<Realm>> {
    let context = scope.get_current_context();
    let global = context.global(scope);
    let key = global
        .get_private(scope, realm_key(scope))?
        .uint32_value(scope)?;
    REALMS.with(|r| r.borrow().get(&key).cloned())
}

/// The engine realm whose global (window) `o` is, if it is one.
pub(crate) fn of_global(
    scope: &v8::PinScope<'_, '_>,
    o: v8::Local<'_, v8::Object>,
) -> Option<usize> {
    let key = o.get_private(scope, realm_key(scope))?;
    if !key.is_uint32() {
        return None;
    }
    let key = key.uint32_value(scope)?;
    REALMS.with(|r| r.borrow().get(&key).map(|r| r.id))
}

/// An event target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// The window.
    Window,
    /// A node.
    Node(NodeId),
    /// A non-node event target (`XMLHttpRequest`, `AbortSignal`) in the slab.
    Object(u32),
}

/// An event listener.
#[derive(Debug, Clone)]
pub struct Listener {
    /// Event type.
    pub ty: String,
    /// Callback (function or object with `handleEvent`).
    pub cb: Value,
    /// Capture phase.
    pub capture: bool,
    /// Removed after the first call.
    pub once: bool,
    /// Removed (a removal during dispatch must take effect at once).
    pub removed: Rc<std::cell::Cell<bool>>,
}

/// An event's state.
#[derive(Debug, Clone, Default)]
pub struct EventData {
    /// `type`.
    pub ty: String,
    /// `bubbles`.
    pub bubbles: bool,
    /// `cancelable`.
    pub cancelable: bool,
    /// `composed`.
    pub composed: bool,
    /// Set by `preventDefault` (cancelable events only).
    pub canceled: bool,
    /// `stopPropagation`.
    pub stop: bool,
    /// `stopImmediatePropagation`.
    pub stop_immediate: bool,
    /// Being dispatched.
    pub dispatching: bool,
    /// Initialized (constructor or `initEvent`).
    pub initialized: bool,
    /// Dispatched by the engine (user input), not by script.
    pub trusted: bool,
    /// `target`.
    pub target: Option<Target>,
    /// `currentTarget`.
    pub current: Option<Target>,
    /// `eventPhase`.
    pub phase: u16,
    /// `timeStamp` (ms since the time origin).
    pub time: f64,
    /// `detail` (CustomEvent: any; UIEvent: long).
    pub detail: Option<Value>,
    /// `UIEvent.detail`.
    pub ui_detail: i32,
    /// Mouse/keyboard fields.
    pub input: InputData,
    /// `relatedTarget`.
    pub related: Option<Target>,
    /// The path of the current dispatch (for `composedPath()`).
    pub path: Vec<Target>,
    /// `ProgressEvent`: `lengthComputable`, `loaded`, `total`.
    pub progress: (bool, f64, f64),
    /// `MediaQueryListEvent`: media and matches.
    pub media: Option<(String, bool)>,
    /// `MessageEvent`: origin, lastEventId and source (data is in `detail`).
    pub message: Option<Box<(String, String, Option<crate::cv::Obj>)>>,
}

/// Mouse and keyboard event fields.
#[derive(Debug, Clone, Default)]
pub struct InputData {
    /// `screenX`, `screenY`, `clientX`, `clientY`.
    pub screen: (i32, i32),
    /// Client coordinates.
    pub client: (f64, f64),
    /// Modifier keys: ctrl, shift, alt, meta.
    pub mods: (bool, bool, bool, bool),
    /// `button`, `buttons`.
    pub button: (i16, u16),
    /// `key`, `code`.
    pub key: (String, String),
    /// `location`, `repeat`, `isComposing`.
    pub location: u32,
    /// `repeat`.
    pub repeat: bool,
    /// Legacy `charCode`, `keyCode`.
    pub legacy: (u32, u32),
    /// `PointerEvent`: `pointerId`, `pointerType`, `isPrimary`.
    pub pointer: (i32, String, bool),
    /// The viewport's scroll when the event was created (`pageX` = `clientX` + this).
    pub page_offset: (f64, f64),
    /// `WheelEvent`: `deltaX`, `deltaY`, `deltaZ`, `deltaMode`.
    pub delta: (f64, f64, f64, u32),
}

/// A navigation a document asked for (link, form, `location`), performed by the tab after the
/// task that asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavRequest {
    /// Absolute URL.
    pub url: String,
    /// `GET` or `POST`.
    pub method: String,
    /// Body (`POST`).
    pub body: Vec<u8>,
    /// `Content-Type` of the body.
    pub content_type: Option<String>,
    /// Replace the current history entry (`location.replace`).
    pub replace: bool,
}

/// A dialog a page opened (`alert`, `confirm`, `prompt`), answered at once by the policy.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Dialog {
    /// `alert`, `confirm` or `prompt`.
    pub kind: String,
    /// The message.
    pub message: String,
    /// Whether it was accepted (alerts are always acknowledged).
    pub accepted: bool,
}

/// A live collection's definition.
#[derive(Debug, Clone)]
pub enum Coll {
    /// `getElementsByTagName` (qualified name, `*` for all).
    Tag(NodeId, String),
    /// `getElementsByTagNameNS`.
    TagNs(NodeId, Option<String>, String),
    /// `getElementsByClassName`.
    Class(NodeId, Vec<String>),
    /// `getElementsByName`.
    Name(String),
    /// `children`.
    Children(NodeId),
    /// `select.options`.
    Options(NodeId),
    /// `form.elements`.
    FormElements(NodeId),
    /// `document.images` / `links` / `forms` / `scripts`.
    Doc(&'static str),
}

/// A non-node object.
#[derive(Debug, Clone)]
pub enum Slot {
    /// An event.
    Event(Box<EventData>),
    /// `classList` and other `DOMTokenList`s: element and attribute.
    TokenList(NodeId, &'static str),
    /// An element's `ValidityState`.
    Validity(NodeId),
    /// A custom element's `ElementInternals`.
    Internals(NodeId),
    /// A custom element's `CustomStateSet`.
    States(NodeId),
    /// A constructed `CSSStyleSheet` (index into the realm's sheets).
    Sheet(u32),
    /// `element.style`.
    Style(NodeId),
    /// `getComputedStyle(element)`.
    Computed(NodeId),
    /// A static `NodeList` (`querySelectorAll`).
    Static(Vec<NodeId>),
    /// `childNodes` (live).
    ChildNodes(NodeId),
    /// A live `HTMLCollection`.
    Collection(Coll),
    /// `dataset`.
    DataSet(NodeId),
    /// `location`.
    Location,
    /// `navigator`.
    Navigator,
    /// `navigator.userAgentData`.
    UaData,
    /// `document.modelContext` (WebMCP).
    ModelContext,
    /// `visualViewport`.
    VisualViewport,
    /// `customElements`.
    CustomElements,
    /// `performance`.
    Performance,
    /// A `DOMRect`: x, y, width, height.
    Rect([f64; 4]),
    /// `History`.
    History,
    /// `Crypto`.
    Crypto,
    /// A `MediaQueryList`.
    MediaQuery(Box<crate::dom::platform::MediaData>),
    /// A `TextEncoder`.
    Encoder,
    /// A `TextDecoder`.
    Decoder(Box<crate::dom::platform::DecoderData>),
    /// A `ResizeObserver`.
    Resize(Box<crate::dom::platform::ResizeData>),
    /// A `ResizeObserverEntry`.
    ResizeEntry(Box<crate::dom::platform::ResizeEntryData>),
    /// A `ResizeObserverSize`: inline, block.
    ResizeSize([f64; 2]),
    /// An `IdleDeadline`: the end of its idle period (page time).
    Idle(f64),
    /// A `DOMRectList`.
    RectList(Vec<[f64; 4]>),
    /// `element.attributes`.
    NamedNodeMap(NodeId),
    /// An attribute (element, qualified name).
    Attr(NodeId, String),
    /// `document.implementation`.
    Implementation,
    /// A canvas's 2D context (by element).
    Canvas(NodeId),
    /// `Headers`: the list and whether script may change it.
    Headers(crate::dom::net::HeaderList),
    /// A `Request`.
    Request(Box<crate::dom::net::RequestData>),
    /// A `Response`.
    Response(Box<crate::dom::net::ResponseData>),
    /// A `MutationObserver`.
    Observer(Box<crate::dom::observers::ObserverData>),
    /// A `MutationRecord`.
    Record(Box<crate::dom::observers::RecordData>),
    /// An `IntersectionObserver`.
    Intersection(Box<crate::dom::intersection::IoData>),
    /// An `IntersectionObserverEntry`.
    IoEntry(Box<crate::dom::intersection::EntryData>),
    /// An `XMLSerializer` (stateless).
    Serializer,
    /// A `TreeWalker`.
    Walker(Box<crate::dom::traversal::WalkerData>),
    /// An `AbortController` (its signal).
    Controller(u32),
    /// An `AbortSignal`.
    Signal(crate::dom::net::SignalData),
    /// An `XMLHttpRequest`.
    Xhr(Box<crate::dom::net::XhrData>),
    /// A `URL` (and its `searchParams` object, once created).
    Url(Box<url::Url>, Option<u32>),
    /// `URLSearchParams` (and the `URL` it belongs to).
    Params(Vec<(String, String)>, Option<u32>),
    /// `FormData`.
    FormData(Vec<(String, String)>),
    /// `localStorage` (`true`) or `sessionStorage`.
    Storage(bool),
    /// A `DOMException`.
    Exception {
        /// Name.
        name: String,
        /// Message.
        message: String,
    },
}

/// A slab entry.
#[derive(Debug)]
pub struct SlabEntry {
    /// Interface of the wrapper.
    pub iface: Iface,
    /// The object.
    pub slot: Slot,
    /// The wrappers, once created: one per world.
    pub wrappers: Vec<(u32, v8::Global<v8::Object>)>,
}

/// A process-wide order of focus changes across realms (frames have their own engines).
pub fn next_focus_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// One document's script-side state.
#[derive(Debug)]
pub struct State {
    /// Document URL.
    pub url: String,
    /// Origin (`scheme://host[:port]`).
    pub origin: String,
    /// Node wrappers, per world.
    pub wrappers: HashMap<(u32, NodeId), v8::Global<v8::Object>>,
    /// Non-node objects.
    pub slab: Vec<SlabEntry>,
    /// `[SameObject]` attributes: (node, attribute) → slab id.
    pub same: HashMap<(NodeId, &'static str), u32>,
    /// Window-level `[SameObject]` attributes.
    pub window_same: HashMap<&'static str, u32>,
    /// Listeners per target.
    pub listeners: HashMap<Target, Vec<Listener>>,
    /// Event handler IDL attributes: (world, target, event type) → callback.
    pub handlers: HashMap<(u32, Target, String), Value>,
    /// The script element being run.
    pub current_script: Option<NodeId>,
    /// `document.readyState`.
    pub ready_state: &'static str,
    /// Mutations so far (invalidates the layout cache).
    pub mutations: u64,
    /// Cached layout: ((mutations, element scroll generation) at build time, layout).
    pub layout: Option<((u64, u64), Rc<Layout>)>,
    /// Scroll positions.
    pub scroll: crate::dom::scroll::ScrollState,
    /// Custom element definitions and element states.
    pub custom: crate::dom::custom_elements::Registry,
    /// Each slot's assigned nodes at the last `slotchange` check.
    pub slots: HashMap<NodeId, Vec<NodeId>>,
    /// Viewport.
    pub viewport: Viewport,
    /// Stylesheets loaded with the page.
    pub stylesheets: Vec<Stylesheet>,
    /// Open modal dialogs, in the order they were opened.
    pub modals: Vec<NodeId>,
    /// The focused element.
    pub focused: Option<NodeId>,
    /// What `navigator` says: the session's identity profile.
    pub navigator: pageon_env::Navigator,
    /// Whether script changed an `<iframe>` (frames must be (re)loaded).
    pub frames_changed: bool,
    /// Script elements that must not run (inserted through markup, or already run).
    pub started_scripts: std::collections::HashSet<NodeId>,
    /// `dialog.returnValue`.
    pub dialog_return: HashMap<NodeId, String>,
    /// `window.name`.
    pub window_name: String,
    /// Canvas bitmaps and context state, by element.
    pub canvases: crate::dom::Canvases,
    /// Tools the page registered (WebMCP), with their provenance.
    pub tools: Vec<crate::dom::webmcp::Tool>,
    /// The document as a network client (origin, top-level site).
    pub client: pageon_net::Client,
    /// The session's network state (cookies, storage).
    pub net: Rc<RefCell<pageon_net::Net>>,
    /// The navigation the document asked for, if any.
    pub navigation: Option<NavRequest>,
    /// Dialogs opened since the last check.
    pub dialogs: Vec<Dialog>,
    /// Popup URLs (`window.open`, `target=_blank`) since the last check.
    pub popups: Vec<String>,
    /// Controls whose validation blocked a submission (or failed `reportValidity()`) since the
    /// last check.
    pub blocked: Vec<crate::dom::validation::Blocked>,
    /// Constructed stylesheets and the trees adopting them.
    pub sheets: crate::dom::sheets::Sheets,
    /// Caps already reported (one page error per kind).
    pub refused: std::collections::HashSet<&'static str>,
    /// Dialogs beyond [`MAX_PER_ACTION`] since the last check (answered, counted, not kept).
    pub dialogs_more: usize,
    /// Popups beyond [`MAX_PER_ACTION`] since the last check (not opened, counted).
    pub popups_more: usize,
    /// Requests the document has made (`fetch`, XHR), against [`crate::dom::net::MAX_REQUESTS`].
    pub requests_made: u32,
    /// Modules the document has fetched, against [`crate::modules::MAX_MODULES`].
    pub modules_loaded: u32,
    /// The element under the pointer.
    pub hovered: Option<NodeId>,
    /// Accept dialogs (`confirm` → true, `prompt` → its default) instead of dismissing them.
    pub accept_dialogs: bool,
    /// `MutationObserver`s with registrations (slab ids, creation order).
    pub observers: Vec<u32>,
    /// `IntersectionObserver`s with targets to report.
    pub intersections: Vec<u32>,
    /// When focus last moved in this realm ([`next_focus_seq`] order; 0: never), so input can
    /// go to the frame that holds focus.
    pub focus_seq: u64,
    /// The module map.
    pub modules: Vec<crate::modules::ModuleRecord>,
    /// The import map.
    pub import_map: crate::modules::ImportMap,
    /// The session history as this document sees it, and its requests for the tab.
    pub history: crate::dom::session::HistoryState,
    /// `MediaQueryList`s (slab ids), re-evaluated when the viewport changes.
    pub media_lists: Vec<u32>,
    /// The viewport the media query lists were last evaluated at.
    pub media_viewport: Option<Viewport>,
    /// `ResizeObserver`s with targets (slab ids, creation order).
    pub resize_observers: Vec<u32>,
    /// The document's `crypto` stream.
    pub crypto: crate::dom::platform::CryptoStream,
    /// The page's frame tree and this document's context in it (`None`: a document alone).
    pub frame: Option<(Rc<RefCell<crate::frames::Frames>>, u32)>,
    /// Windows of other frames as this realm sees them, and their cross-origin members, by
    /// (world, context, member; `""` for the window itself).
    pub remote: HashMap<(u32, u32, &'static str), v8::Global<v8::Object>>,
}

impl State {
    /// Adds a non-node object.
    pub fn alloc(&mut self, iface: Iface, slot: Slot) -> u32 {
        let id = u32::try_from(self.slab.len()).unwrap_or(u32::MAX);
        self.slab.push(SlabEntry {
            iface,
            slot,
            wrappers: Vec::new(),
        });
        id
    }

    /// The slot of an object.
    pub fn slot(&self, id: u32) -> Option<&Slot> {
        self.slab.get(id as usize).map(|e| &e.slot)
    }

    /// The mutable slot of an object.
    pub fn slot_mut(&mut self, id: u32) -> Option<&mut Slot> {
        self.slab.get_mut(id as usize).map(|e| &mut e.slot)
    }
}

/// A timer or animation frame callback.
#[derive(Debug, Clone)]
pub struct Timer {
    /// Id (`setTimeout`'s return value).
    pub id: u32,
    /// Realm it belongs to.
    pub realm: usize,
    /// Callback.
    pub cb: Value,
    /// Extra arguments.
    pub args: Vec<Value>,
    /// Repeat interval (ms) for `setInterval`.
    pub interval: Option<f64>,
    /// An animation frame callback (gets the frame time).
    pub frame: bool,
    /// An idle callback (gets an `IdleDeadline`; runs after other timers due at that time).
    pub idle: bool,
}

/// What to do when a script request completes.
#[derive(Debug)]
pub enum Done {
    /// Resolve or reject `fetch()`'s promise (and stop if the signal aborted).
    Fetch {
        /// The promise.
        resolver: v8::Global<v8::PromiseResolver>,
        /// The request's `AbortSignal`.
        signal: Option<u32>,
    },
    /// Fire an `XMLHttpRequest`'s events.
    Xhr(u32),
}

/// A script request waiting to be sent.
#[derive(Debug)]
pub struct Pending {
    /// The realm that sent it.
    pub realm: usize,
    /// The request.
    pub req: pageon_net::FetchRequest,
    /// The completion.
    pub done: Done,
}

/// Page-wide state: the virtual clock, the timer queue, the RNG and the errors.
#[derive(Debug, Default)]
pub struct Shared {
    /// Script requests in issue order.
    pub requests: std::collections::VecDeque<Pending>,
    /// Inserted scripts and dynamic imports, run before other tasks.
    pub scripts: std::collections::VecDeque<crate::modules::ScriptJob>,
    /// Virtual time in ms since the Unix epoch.
    pub now: f64,
    /// The time origin (`performance.timeOrigin`).
    pub origin: f64,
    /// xorshift64* state for `Math.random`.
    pub rng: u64,
    /// Timers by (due in µs, sequence).
    pub timers: BTreeMap<(u64, u64), Timer>,
    /// Next sequence number.
    pub seq: u64,
    /// Next timer id.
    pub next_id: u32,
    /// Page errors.
    pub errors: Vec<PageError>,
    /// Console messages (level, text), capped.
    pub console: Vec<(String, String)>,
    /// Timers cancelled while running (a `clearInterval` from inside its own callback).
    pub cancelled: std::collections::HashSet<u32>,
    /// Protocol binding calls (realm, name, payload), drained by the client.
    pub bindings: Vec<(usize, String, String)>,
    /// The engine's id (distinct for every engine in the process).
    pub engine: u64,
    /// The engine's realms, by index (weak: the engine owns them).
    pub realms: Vec<std::rc::Weak<Realm>>,
    /// The realm each page error happened in (parallel to `errors`, filled as realms run).
    pub error_realms: Vec<usize>,
}

impl Shared {
    /// Attributes the errors recorded since the last call to `realm`.
    pub fn tag_errors(&mut self, realm: usize) {
        let n = self.errors.len();
        self.error_realms.resize(n, realm);
    }

    /// The next random number in [0, 1).
    pub fn random(&mut self) -> f64 {
        let mut x = self.rng.max(1);
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (r >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Schedules a timer `delay` ms from now; returns its id.
    pub fn schedule(&mut self, mut t: Timer, delay: f64) -> u32 {
        if t.id == 0 {
            self.next_id += 1;
            t.id = self.next_id;
        }
        let id = t.id;
        let due = ((self.now + delay.max(0.0)) * 1000.0) as u64;
        self.seq += 1;
        self.timers.insert((due, self.seq), t);
        id
    }

    /// Cancels a timer.
    pub fn cancel(&mut self, id: u32) {
        self.timers.retain(|_, t| t.id != id);
        self.cancelled.insert(id);
    }

    /// Records a page error.
    pub fn error(
        &mut self,
        kind: PageErrorKind,
        message: impl Into<String>,
        source: impl Into<String>,
    ) {
        if self.errors.len() < 1000 {
            self.errors.push(PageError {
                kind,
                message: message.into(),
                source: source.into(),
                line: 0,
                column: 0,
            });
        }
    }
}

/// A document's realm.
pub struct Realm {
    /// Index of this realm in the engine.
    pub id: usize,
    /// The document.
    pub dom: Rc<RefCell<Dom>>,
    /// Script-side state.
    pub st: RefCell<State>,
    /// Interface templates (shared by every realm of the engine).
    pub templates: Rc<Vec<v8::Global<v8::FunctionTemplate>>>,
    /// Page-wide clock, timers and errors.
    pub shared: Rc<RefCell<Shared>>,
    /// The realm's contexts by world: the main world first, then isolated worlds by name.
    pub worlds: RefCell<Vec<World>>,
}

/// One context of a realm.
pub struct World {
    /// The world's name (empty for the main world).
    pub name: String,
    /// The context.
    pub context: v8::Global<v8::Context>,
    /// The engine's index for the context (the realm id for the main world).
    pub index: usize,
}

impl std::fmt::Debug for Realm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Realm")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Realm {
    /// The current layout of the document (rebuilt after mutations).
    pub fn layout(&self) -> Rc<Layout> {
        let mut st = self.st.borrow_mut();
        let key = (st.mutations, st.scroll.generation);
        let (vx, vy) = st.scroll.viewport;
        if let Some((k, l)) = &st.layout
            && *k == key
        {
            let l = l.clone();
            l.set_viewport_scroll(vx as f32, vy as f32);
            return l;
        }
        let dom = self.dom.borrow();
        st.scroll.elements.retain(|&id, _| dom.get(id).is_some());
        let offsets: Vec<(NodeId, f64, f64)> = st
            .scroll
            .elements
            .iter()
            .map(|(&id, &(x, y))| (id, x, y))
            .collect();
        let opts = pageon_layout::LayoutOptions {
            modals: &st.modals,
            scroll: &offsets,
            ..pageon_layout::LayoutOptions::default()
        };
        let l = Rc::new(Layout::with_options(
            &dom,
            st.viewport,
            &st.stylesheets,
            Some(&st.url),
            &opts,
        ));
        // Content can shrink: positions are clamped to what the new layout allows.
        let (x, y) = l.set_viewport_scroll(vx as f32, vy as f32);
        st.scroll.viewport = (f64::from(x), f64::from(y));
        for (id, _, _) in &offsets {
            let (x, y) = l.scroll_of(*id);
            st.scroll.elements.insert(*id, (f64::from(x), f64::from(y)));
        }
        st.layout = Some((key, l.clone()));
        l
    }
}

/// Most dialogs, popups (and, in the tab, downloads) one action reports: the rest are
/// answered as usual and counted (SR-007).
pub const MAX_PER_ACTION: usize = 20;

impl State {
    /// Records a dialog (kept up to [`MAX_PER_ACTION`], then counted).
    pub fn push_dialog(&mut self, d: Dialog) {
        if self.dialogs.len() < MAX_PER_ACTION {
            self.dialogs.push(d);
        } else {
            self.dialogs_more += 1;
        }
    }

    /// Records a popup URL (kept up to [`MAX_PER_ACTION`], then counted).
    pub fn push_popup(&mut self, url: String) {
        if self.popups.len() < MAX_PER_ACTION {
            self.popups.push(url);
        } else {
            self.popups_more += 1;
        }
    }
}
