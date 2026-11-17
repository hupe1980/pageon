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

/// The realm whose script is running (the entered context): the caller when a script calls a
/// function of another realm, as the sender of `postMessage` is.
pub fn entered(scope: &v8::PinScope<'_, '_>) -> Option<Rc<Realm>> {
    let context = scope.get_entered_or_microtask_context();
    let global = context.global(scope);
    let key = global
        .get_private(scope, realm_key(scope))?
        .uint32_value(scope)?;
    REALMS.with(|r| r.borrow().get(&key).cloned())
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
    /// The place of an event handler (`onerror = f`) of this world among the listeners: it
    /// runs where it was first set, not as a listener of its own.
    pub handler: Option<u32>,
}

/// An `ErrorEvent`'s message, filename, lineno, colno and error.
pub type ErrorInfo = (String, String, u32, u32, Option<Value>);

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
    /// `SubmitEvent.submitter`.
    pub submitter: Option<NodeId>,
    /// `SubmitEvent.agentInvoked`: an agent's tool call submitted the form.
    pub agent: bool,
    /// `ToolActivatedEvent.toolName`, `ToolCancelEvent.toolName`.
    pub tool: String,
    /// `ToggleEvent.oldState` and `newState`.
    pub toggle: (String, String),
    /// `ToggleEvent.source`: the element that showed or hid the popover.
    pub source: Option<NodeId>,
    /// The path of the current dispatch (for `composedPath()`).
    pub path: Vec<Target>,
    /// `ProgressEvent`: `lengthComputable`, `loaded`, `total`.
    pub progress: (bool, f64, f64),
    /// `MediaQueryListEvent`: media and matches.
    pub media: Option<(String, bool)>,
    /// `MessageEvent`: origin, lastEventId and source (data is in `detail`).
    pub message: Option<Box<(String, String, Option<crate::cv::Obj>)>>,
    /// `ErrorEvent`: message, filename, lineno, colno, error.
    pub error: Option<Box<ErrorInfo>>,
    /// `PromiseRejectionEvent`: promise and reason.
    pub rejection: Option<Box<(Value, Value)>>,
    /// `CloseEvent`: wasClean, code, reason.
    pub close: Option<Box<(bool, u16, String)>>,
    /// `DragEvent`, `ClipboardEvent`, `InputEvent`: its `DataTransfer` (slab id).
    pub drag: Option<u32>,
    /// `InputEvent.getTargetRanges()`.
    pub ranges: Vec<(pageon_dom::Boundary, pageon_dom::Boundary)>,
    /// `InputEvent`: `inputType`, `data`, `isComposing`.
    pub input_kind: Option<Box<(String, Option<String>, bool)>>,
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
    /// `form.elements[name]` when several controls share it (`RadioNodeList`).
    Radio(NodeId, String),
    /// `table.rows`: its rows, in its head, bodies and foot.
    TableRows(NodeId),
    /// `table.tBodies`.
    TableBodies(NodeId),
    /// `document.images` / `links` / `forms` / `scripts`.
    Doc(&'static str),
}

/// A non-node object.
#[derive(Debug, Clone)]
pub enum Slot {
    /// A canvas `TextMetrics`.
    TextMetrics(pageon_layout::CanvasTextMetrics),
    /// An event.
    Event(Box<EventData>),
    /// A plain `EventTarget` (`new EventTarget()`, or a class that extends it).
    Target,
    /// `document.fonts`.
    Fonts,
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
    /// A sheet's `cssRules` (the sheet's slab id).
    RuleList(u32),
    /// A rule of a sheet: the sheet's slab id and the rule's index.
    Rule(u32, u32),
    /// `document.styleSheets` (or a shadow root's).
    SheetList(pageon_dom::NodeId),
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
    /// `screen`.
    Screen,
    /// `crypto.subtle`.
    Subtle,
    /// A Web Crypto key.
    CryptoKey(Box<crate::dom::subtle::Key>),
    /// A `MessagePort` of a `MessageChannel`.
    Port(Box<crate::dom::basics::Port>),
    /// A `Range`: a live range of the document.
    Range(pageon_dom::RangeId),
    /// A `StaticRange`.
    StaticRange(pageon_dom::Boundary, pageon_dom::Boundary),
    /// The document's `Selection`.
    Selection,
    /// A `CaretPosition`: node and offset.
    CaretPosition(NodeId, u32),
    /// `navigator.clipboard` and `navigator.credentials`.
    Clipboard,
    /// `navigator.permissions`, `navigator.userActivation`, `screen.orientation`,
    /// `navigator.plugins` and `navigator.mimeTypes`: stateless singletons.
    Platform,
    /// A `PermissionStatus`: the permission's name.
    Permission(String),
    /// A `Plugin` (index into the PDF viewer list).
    Plugin(u8),
    /// A `MimeType` (0: `application/pdf`, 1: `text/pdf`) of a plugin.
    MimeType(u8, u8),
    /// A `PublicKeyCredential`.
    Credential(Box<crate::dom::credentials::Cred>),
    /// An authenticator response.
    AuthResponse(Box<crate::dom::credentials::Resp>),
    /// A `MessageChannel`: its two ports.
    Channel(u32, u32),
    /// A `Worker`.
    Worker(Box<crate::dom::workers::Handle>),
    /// A `PerformanceMark` or `PerformanceMeasure`.
    PerfEntry(Box<crate::dom::timing::Entry>),
    /// A `PerformanceObserver`.
    PerfObserver(Box<crate::dom::timing::ObserverData>),
    /// A `PerformanceObserverEntryList`.
    PerfEntryList(Vec<u32>),
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
    /// A `WebSocket`.
    WebSocket(Box<crate::dom::realtime::WsData>),
    /// A `DataTransfer`.
    DataTransfer(Box<crate::dom::dnd::DtData>),
    /// A `DataTransferItemList` (its data transfer).
    DtItems(u32),
    /// A `DataTransferItem` (its data transfer and index).
    DtItem(u32, usize),
    /// An `EventSource`.
    EventSource(Box<crate::dom::realtime::SseData>),
    /// A `URL` (and its `searchParams` object, once created).
    Url(Box<url::Url>, Option<u32>),
    /// `URLSearchParams` (and the `URL` it belongs to).
    Params(Vec<(String, String)>, Option<u32>),
    /// `FormData`.
    FormData(Vec<(String, crate::dom::blobs::Entry)>),
    /// A `Blob` or `File`.
    Blob(std::rc::Rc<crate::dom::blobs::BlobData>),
    /// A `FileList`.
    FileList(Vec<std::rc::Rc<crate::dom::blobs::BlobData>>),
    /// A `FileReader`.
    Reader(Box<crate::dom::blobs::ReaderData>),
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
    /// The page asked for notifications and was refused (pageon grants no permission).
    pub notifications_denied: bool,
    /// Listeners per target.
    pub listeners: HashMap<Target, Vec<Listener>>,
    /// Event handler IDL attributes: (world, target, event type) → callback.
    pub handlers: HashMap<(u32, Target, String), Value>,
    /// The script element being run.
    pub current_script: Option<NodeId>,
    /// While the parser runs: what `document.write` wrote during the current script, for the
    /// parser to read next. `None` once parsing has ended (writing then throws).
    pub written: Option<String>,
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
    /// Styles computed on the document itself (made on the first computed-style read), with
    /// the viewport it was made for and the author sheets it was given.
    pub styler: Option<Styles>,
    /// Open modal dialogs, in the order they were opened.
    pub modals: Vec<NodeId>,
    /// The focused element.
    pub focused: Option<NodeId>,
    /// What `navigator` says: the session's identity profile.
    pub navigator: pageon_env::Navigator,
    /// Whether script changed an `<iframe>` (frames must be (re)loaded).
    pub frames_changed: bool,
    /// Layouts computed for this document's scripts (geometry reads after changes).
    pub layouts: u32,
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
    /// Form tools (WebMCP's declarative API) seen so far: form, name and the id they were
    /// given, so a form keeps its tool's id while it keeps its name.
    pub declared: Vec<(NodeId, String, u64)>,
    /// An agent's tool call is submitting a form (`SubmitEvent.agentInvoked`).
    pub agent_submit: bool,
    /// What the page answered an agent's submission with (`SubmitEvent.respondWith`).
    pub agent_response: Option<Value>,
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
    /// The files of each file input (set by uploads).
    pub files: std::collections::HashMap<NodeId, Vec<std::rc::Rc<crate::dom::blobs::BlobData>>>,
    /// The file input whose file chooser the last activation opened (taken by uploads).
    pub chooser: Option<NodeId>,
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
    /// The pointer's button is held (pressed on this element, at this point).
    pub pressed: Option<crate::dom::input::Press>,
    /// An HTML drag and drop in progress.
    pub drag: Option<crate::dom::input::DragSession>,
    /// Marks and measures on the performance timeline (slab ids, in order).
    pub timing: Vec<u32>,
    /// `PerformanceObserver`s observing (slab ids).
    pub perf_observers: Vec<u32>,
    /// `NodeIterator`s (slab ids), adjusted when nodes are removed.
    pub iterators: Vec<u32>,
    /// The `Range` object of the selection's range, once a script asked for it.
    pub sel_obj: Option<(pageon_dom::RangeId, u32)>,
    /// Text controls whose selection changed (`selectionchange` at them).
    pub control_sel: Vec<NodeId>,
    /// Values the vault filled into this page: never put on the clipboard.
    pub vault_values: Vec<String>,
    /// The modifier keys held during the current input action.
    pub mods: crate::dom::input::Mods,
    /// The mouse button of the current input action.
    pub button: crate::dom::input::Button,
    /// The click count of the current click (1–3; `detail` of mouse events).
    pub click_count: u8,
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
    /// The layout (mutations, scroll generation) the resize observers last measured; `None`
    /// after a target was added.
    pub resize_seen: Option<(u64, u64)>,
    /// The document's `crypto` stream.
    pub crypto: crate::dom::platform::CryptoStream,
    /// The page's frame tree and this document's context in it (`None`: a document alone).
    pub frame: Option<(Rc<RefCell<crate::frames::Frames>>, u32)>,
    /// Windows of other frames as this realm sees them, and their cross-origin members, by
    /// (world, context, member; `""` for the window itself).
    pub remote: HashMap<(u32, u32, &'static str), v8::Global<v8::Object>>,
    /// A worker that ended (`terminate`, `close`): no further task runs in it.
    pub closed: bool,
    /// The scripts a worker's `importScripts` may run, loaded when it started (URL → text).
    pub imports: HashMap<String, String>,
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
    /// Set by the agent's current action, directly or through timers it set (see
    /// [`crate::Engine::begin_action`]).
    pub caused: bool,
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
    /// Realtime connections (`WebSocket`, `EventSource`) and what documents asked of them.
    pub streams: crate::dom::realtime::Streams,
    /// Messages for `MessagePort`s (realm, port, data), in posting order.
    pub port_mail: std::collections::VecDeque<(usize, u32, Value)>,
    /// The session clipboard, as this page holds it.
    pub clipboard: Option<crate::dom::editing::Clip>,
    /// The page changed the clipboard (copy, cut, `writeText`) since the last action.
    pub clipboard_changed: bool,
    /// Passkey calls waiting for the environment.
    pub passkey_calls: std::collections::VecDeque<crate::dom::credentials::Waiting>,
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
    /// A timer set before the current action is running: timers it sets are not the
    /// action's.
    pub uncaused: bool,
    /// The virtual frame (1/60 s) of the last intersection update: observations are made at
    /// most once a frame, as browsers make them when they render.
    pub frame: u64,
    /// Realms whose intersection update waits for the next frame (or for the page to idle).
    pub render_due: std::collections::BTreeSet<usize>,
    /// Next timer id.
    pub next_id: u32,
    /// Next WebMCP tool id (a new one per registration, imperative or declarative).
    pub tool_seq: u64,
    /// Page errors.
    pub errors: Vec<PageError>,
    /// Console messages (level, text): the latest 1,000.
    pub console: Vec<(String, String)>,
    /// Console messages ever recorded (so readers can ask for what is new).
    pub console_total: u64,
    /// Timers cancelled while running (a `clearInterval` from inside its own callback).
    pub cancelled: std::collections::HashSet<u32>,
    /// Protocol binding calls (realm, name, payload), drained by the client.
    pub bindings: Vec<(usize, String, String)>,
    /// The engine's id (distinct for every engine in the process).
    pub engine: u64,
    /// Messages and errors between workers and their owners, in posting order.
    pub worker_mail: std::collections::VecDeque<crate::dom::workers::Mail>,
    /// Workers running in this engine.
    pub workers: usize,
    /// `blob:` URLs (`URL.createObjectURL`): bytes and type.
    pub blob_urls: HashMap<String, (Vec<u8>, String)>,
    /// The engine's realms, by index (weak: the engine owns them).
    pub realms: Vec<std::rc::Weak<Realm>>,
    /// Realms made while a script ran (an `about:blank` frame's), with their contexts and
    /// registry keys: the engine adopts them after the task.
    pub adopted: Vec<(v8::Global<v8::Context>, Rc<Realm>, u32)>,
    /// `<iframe>`s a script inserted that get an `about:blank` document: (realm, element).
    pub blank_frames: Vec<(usize, NodeId)>,
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
            t.caused = !self.uncaused;
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
    /// A dedicated worker's global, linked to its `Worker` in the realm that created it.
    pub worker: Option<crate::dom::workers::Link>,
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

/// A realm's styles computed on the document itself.
pub struct Styles {
    styler: pageon_style::Styler,
    viewport: Viewport,
    /// The author sheets in cascade order with their hashes, and how many loaded sheets they
    /// were built with.
    sheets: Vec<(u64, String)>,
    loaded: usize,
}

impl std::fmt::Debug for Styles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Styles").finish_non_exhaustive()
    }
}


impl Realm {
    /// The computed value of `property` (not a used value: see [`Layout::needs_boxes`])
    /// from styles computed on the document itself, brought up to date from its change log;
    /// `None` for shorthands.
    pub fn computed(&self, id: NodeId, property: &str) -> Option<String> {
        if !self.sync_styles() {
            return None;
        }
        let dom = self.dom.borrow();
        let st = self.st.borrow();
        st.styler.as_ref()?.styler.value(&dom, id, property)
    }

    /// Brings the styles computed on the document up to date with its changes (made on first
    /// use). Whether the document has them (not while it is borrowed for a change).
    fn sync_styles(&self) -> bool {
        let Ok(mut dom) = self.dom.try_borrow_mut() else {
            return false;
        };
        let changes = dom.take_style_changes();
        let mut st = self.st.borrow_mut();
        let st = &mut *st;
        let sheets_may_differ = changes.iter().any(|c| {
            matches!(
                c,
                pageon_dom::StyleChange::Sheets | pageon_dom::StyleChange::All
            )
        });
        let fresh = st
            .styler
            .as_ref()
            .is_none_or(|s| s.viewport != st.viewport);
        if fresh {
            st.styler = Some(Styles {
                styler: pageon_style::Styler::with_metrics(
                    st.viewport.width as f32,
                    st.viewport.height as f32,
                    &st.url,
                    pageon_layout::font_metrics(),
                ),
                viewport: st.viewport,
                sheets: Vec::new(),
                loaded: usize::MAX,
            });
        }
        let defined = st.custom.defined();
        let Some(styles) = st.styler.as_mut() else {
            return false;
        };
        if fresh || sheets_may_differ || styles.loaded != st.stylesheets.len() {
            styles.sheets = pageon_layout::author_sheets(&dom, &st.stylesheets);
            styles.loaded = st.stylesheets.len();
        }
        let texts: Vec<(u64, &str)> = styles
            .sheets
            .iter()
            .map(|(h, t)| (*h, t.as_str()))
            .collect();
        styles.styler.set_sheets(&texts);
        // A styler made now restyles everything; the log before it is not needed.
        let changes = if fresh {
            vec![pageon_dom::StyleChange::All]
        } else {
            changes
        };
        let linked: Vec<(NodeId, &str)> = st
            .stylesheets
            .iter()
            .map(|s| (s.link, s.css()))
            .collect();
        styles.styler.sync(
            &dom,
            &changes,
            pageon_style::Page {
                top_layer: &st.modals,
                defined: &defined,
                linked: &linked,
            },
        );
        true
    }

    /// The current layout of the document (rebuilt after mutations).
    pub fn layout(&self) -> Rc<Layout> {
        self.laid_out(false)
    }

    /// The document's current styles, without boxes unless a layout is at hand: for computed
    /// style values that do not depend on layout ([`Layout::needs_boxes`]).
    pub fn styles(&self) -> Rc<Layout> {
        self.laid_out(true)
    }

    fn laid_out(&self, styles_only: bool) -> Rc<Layout> {
        let st = self.st.borrow();
        let key = (st.mutations, st.scroll.generation);
        let (vx, vy) = st.scroll.viewport;
        if let Some((k, l)) = &st.layout
            && *k == key
            && (styles_only || l.has_boxes())
        {
            let l = l.clone();
            l.set_viewport_scroll(vx as f32, vy as f32);
            return l;
        }
        // Styles first (they take the document's change log), then the layout with them.
        drop(st);
        let native = self.sync_styles();
        let mut st = self.st.borrow_mut();
        let dom = self.dom.borrow();
        st.scroll.elements.retain(|&id, _| dom.get(id).is_some());
        let offsets: Vec<(NodeId, f64, f64)> = st
            .scroll
            .elements
            .iter()
            .map(|(&id, &(x, y))| (id, x, y))
            .collect();
        if !styles_only {
            st.layouts = st.layouts.saturating_add(1);
        }
        // The last layout, when nothing else holds it, is laid out again: its stylesheets stay
        // parsed (pages that change and measure themselves lay out many times).
        let prev = st.layout.take().and_then(|(_, l)| Rc::try_unwrap(l).ok());
        let defined = st.custom.defined();
        let st = &mut *st;
        let opts = pageon_layout::LayoutOptions {
            modals: &st.modals,
            defined: &defined,
            scroll: &offsets,
            styles_only,
            styles: if native {
                st.styler.as_ref().map(|s| &s.styler)
            } else {
                None
            },
            ..pageon_layout::LayoutOptions::default()
        };
        let l = Rc::new(Layout::again(
            &dom,
            st.viewport,
            &st.stylesheets,
            Some(&st.url),
            &opts,
            prev,
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
/// answered as usual and counted.
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

    /// A sandboxed document without `allow-scripts`: none of its own scripts, inline handlers
    /// or `javascript:` URLs run.
    pub fn scripting_disabled(&self) -> bool {
        self.client.sandbox.is_some_and(|s| !s.scripts)
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
