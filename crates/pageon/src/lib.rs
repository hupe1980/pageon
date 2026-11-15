//! pageon: a browser engine for autonomous agents.
//!
//! pageon loads, runs and lays out web pages by itself (no Chromium) and gives an agent what it
//! needs to act on them: a compact view with stable element refs, typed actions with typed
//! outcomes, pixels on demand, and a sandbox, egress policy and secrets vault around the page.
//! Every request, clock reading and random draw goes through one [`Env`] ([`Live`], a
//! [`Recording`] of it, or an offline [`Replay`]), so sessions replay byte for byte.
//!
//! ```no_run
//! use pageon::{Live, LoadOptions, Session};
//!
//! let session = Session::new(Live::default());
//! let mut tab = session.open("https://example.com/login", LoadOptions::default())?;
//! println!("{}", tab.observe());                     // the agent view, with refs
//! let out = tab.act("type e3 ada".parse()?)?;        // a typed outcome
//! println!("{out}");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! A one-shot load, without a tab:
//!
//! ```no_run
//! use pageon::{Dump, Live, LoadOptions};
//!
//! let doc = pageon::load("https://example.com", &Live::default(), LoadOptions::default())?;
//! println!("{} ({})", doc.final_url(), doc.status());
//! println!("{}", doc.dump(Dump::Text));
//! # Ok::<(), pageon::LoadError>(())
//! ```
//!
//! Parsing a string needs no environment:
//!
//! ```
//! let doc = pageon::parse("<p>Hello <b>world</b></p>", "https://example.com/");
//! assert_eq!(doc.dump(pageon::Dump::Text), "Hello world");
//! ```

pub mod bidi;
mod boxes;
mod capture;
pub mod cdp;
mod config;
#[doc(hidden)]
pub mod content;
mod error;
mod files;
mod frames;
mod guard;
mod load;
mod markdown;
pub mod mcp;
mod paint;
mod passkeys;
mod policy;
mod protocol;
pub mod replay;
mod sandbox;
mod scripts;
mod secrets;
mod session;
mod tab;
pub mod takeover;
pub mod threat_model;
pub mod workflow;

pub use config::{Config, ConfigError};
pub use error::LoadError;
pub use files::{Files, mime_type};
pub use frames::{Frame, FrameState, MAX_DEPTH, MAX_FRAMES};
pub use guard::{CommandGuard, GUARD_TIMEOUT};
pub use load::{DialogPolicy, LoadOptions, load};
pub use pageon_agent::Filter;
pub use pageon_agent::{Budget, Capture, Episode, Mode, Target, Tokenizer};
pub use pageon_dom::{Namespace, NodeId, QuirksMode};
pub use pageon_env::{Env, Identity, Limits, Live, Profile, Recording, Replay, Request};
pub use pageon_js::Dialog;
pub use pageon_js::{DragKind, Dragged};
pub use pageon_js::{PageError, PageErrorKind};
pub use pageon_layout::{Layout, Pixmap, Rect, Viewport, Visibility};
pub use pageon_net::{
    Cookie, Dest, Outcome as RequestOutcome, RequestEntry, SameSite, StorageState,
};
pub use pageon_net::{Refusal, RefusalKind};
pub use paint::{Area, Format, Image, PaintError, Provenance, ScreenshotOptions, image_tokens};
pub use passkeys::{PasskeyError, Passkeys};
pub use policy::{Effect, EffectClass, Policy, Verdict, check_origin};
pub use sandbox::{Lookahead, Pending, RemoteTab, Sandbox, SandboxError, SandboxOptions};
pub use secrets::{Secret, Secrets};
pub use session::Session;
pub use tab::{
    Action, ActionError, At, Direction, Download, Outcome, Reason, ScrollBy, Settle, Tab,
};

use std::rc::Rc;

use pageon_dom::{Dom, NodeData};

/// A loaded or parsed document.
#[derive(Debug, Clone)]
pub struct Document {
    dom: Dom,
    final_url: String,
    status: u16,
    content_type: String,
    encoding: Encoding,
    summary: Summary,
    stylesheets: Vec<pageon_layout::Stylesheet>,
    frames: Vec<Frame>,
    nav: u32,
    js: JsState,
    requests: Vec<RequestEntry>,
    /// The site's refusal of the agent, read from the final top-level response.
    refusal: Option<pageon_net::Refusal>,
    /// When the load started (ms since the Unix epoch, from the environment).
    loaded_at: u64,
    /// The document as a network client (`None` for parsed strings).
    pub(crate) client: Option<pageon_net::Client>,
    /// Its scripts, kept running in a tab.
    pub(crate) live: Option<Rc<LiveDoc>>,
    /// The tab's decoded images, once it has been painted: they take part in
    /// every layout from then on.
    pub(crate) images: Option<Rc<pageon_layout::Images>>,
    /// The page's frames and engines (the top-level document of a page that ran scripts).
    pub(crate) page: Option<Rc<frames::Page>>,
    /// The document's context in its page's frame tree (0: the top-level document).
    pub(crate) context: u32,
}

/// A document whose scripts keep running (in a tab): its engine, realm and live DOM.
pub(crate) struct LiveDoc {
    /// The engine, shared by the documents of one origin in a page.
    pub(crate) engine: Rc<std::cell::RefCell<pageon_js::Engine>>,
    pub(crate) realm: usize,
    pub(crate) dom: Rc<std::cell::RefCell<Dom>>,
}

impl std::fmt::Debug for LiveDoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveDoc")
            .field("realm", &self.realm)
            .finish_non_exhaustive()
    }
}

/// A document's scroll positions (CSS px).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Scroll {
    pub(crate) viewport: (f64, f64),
    pub(crate) elements: Vec<(NodeId, f64, f64)>,
}

impl Scroll {
    pub(crate) fn of(engine: &pageon_js::Engine, realm: usize) -> Self {
        let (viewport, elements) = engine.scroll_state(realm);
        Self { viewport, elements }
    }
}

/// What a document's scripts left behind.
#[derive(Debug, Clone, Default)]
pub(crate) struct JsState {
    pub(crate) errors: Vec<PageError>,
    pub(crate) scripts: usize,
    pub(crate) pending_timers: usize,
    pub(crate) virtual_ms: u64,
    pub(crate) modals: Vec<NodeId>,
    /// Elements listening for `drop` (they get refs: drag onto them, drop files on them).
    pub(crate) drop_targets: Vec<NodeId>,
    pub(crate) focused: Option<NodeId>,
    pub(crate) pending_requests: usize,
    /// The viewport's scroll offset, and scroll containers' offsets.
    pub(crate) scroll: Scroll,
    /// A navigation the scripts asked for while loading (followed by the loader).
    pub(crate) navigation: Option<pageon_js::NavRequest>,
    pub(crate) dialogs: Vec<pageon_js::Dialog>,
    pub(crate) popups: Vec<String>,
}

/// A number per loaded or parsed document, so observations of different documents are never
/// mistaken for the same page (element refs do not carry over between documents).
pub(crate) fn next_nav() -> u32 {
    static NAV: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    NAV.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The encoding a document was decoded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoding {
    /// WHATWG name, e.g. `UTF-8`.
    pub name: &'static str,
    /// Where it came from: `bom`, `http`, `meta` or `default` (`none` for parsed strings).
    pub source: &'static str,
}

/// Facts about a load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Body bytes of the final response.
    pub bytes: usize,
    /// Load time in milliseconds (from the environment's clock: deterministic in replay).
    pub ms: u64,
    /// Redirects followed.
    pub redirects: usize,
    /// External stylesheets loaded.
    pub stylesheets: usize,
    /// External stylesheets that failed to load (skipped).
    pub stylesheets_failed: usize,
    /// Scripts run.
    pub scripts: usize,
    /// Page errors (uncaught exceptions, limits, unsupported scripts, failed script loads).
    pub errors: usize,
    /// Timers still pending when the page settled.
    pub pending_timers: usize,
    /// Virtual time the page settled for (ms).
    pub virtual_ms: u64,
    /// Requests the load made, frames and preflights included (see [`Document::requests`]).
    pub requests: usize,
    /// Requests refused by the egress policy (private network, mixed content).
    pub blocked: usize,
    /// Script requests (`fetch`, XHR) still unanswered when the page settled.
    pub pending_requests: usize,
}

/// What a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// The document.
    Document,
    /// A document fragment (template contents).
    Fragment,
    /// A doctype.
    Doctype,
    /// An element.
    Element,
    /// Text.
    Text,
    /// A comment.
    Comment,
    /// A processing instruction.
    ProcessingInstruction,
}

/// A read-only view of one node.
#[derive(Debug, Clone, Copy)]
pub struct NodeRef<'a> {
    node: &'a pageon_dom::Node,
}

impl<'a> NodeRef<'a> {
    /// What the node is.
    pub fn kind(&self) -> NodeKind {
        match &self.node.data {
            NodeData::Document => NodeKind::Document,
            NodeData::DocumentFragment | NodeData::ShadowRoot { .. } => NodeKind::Fragment,
            NodeData::Doctype { .. } => NodeKind::Doctype,
            NodeData::Element { .. } => NodeKind::Element,
            NodeData::Text(_) => NodeKind::Text,
            NodeData::Comment(_) => NodeKind::Comment,
            NodeData::ProcessingInstruction { .. } => NodeKind::ProcessingInstruction,
        }
    }

    /// Element local name (`div`), if an element.
    pub fn local_name(&self) -> Option<&'a str> {
        self.node.local_name()
    }

    /// Element namespace, if an element.
    pub fn namespace(&self) -> Option<Namespace> {
        self.node.namespace()
    }

    /// Attribute value by name (no namespace).
    pub fn attr(&self, name: &str) -> Option<&'a str> {
        self.node.attr(name)
    }

    /// All attributes as `(qualified name, value)`.
    pub fn attrs(&self) -> impl Iterator<Item = (String, &'a str)> + 'a {
        self.node
            .attrs()
            .iter()
            .map(|a| (a.qualified_name(), a.value.as_str()))
    }

    /// Text of a text or comment node.
    pub fn text(&self) -> Option<&'a str> {
        match &self.node.data {
            NodeData::Text(t) | NodeData::Comment(t) => Some(t),
            _ => None,
        }
    }
}

/// A read of a tab with the agent's refs, for the vendor toolsets (`read_page`, `find`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Read {
    /// The `ax` view filtered (viewport, interactive, all), `depth` levels deep, below `root`.
    Page {
        /// Which elements.
        filter: pageon_agent::Filter,
        /// Levels of the accessibility tree.
        depth: u32,
        /// A ref whose subtree alone is read.
        root: Option<String>,
        /// The view's budget.
        budget: Budget,
    },
    /// Elements matching the words of a query, at most `limit`.
    Find {
        /// The query.
        query: String,
        /// At most this many.
        limit: usize,
    },
    /// One element's line, by ref.
    Line(String),
    /// What is at a viewport point.
    At(f64, f64),
    /// The page as Markdown within a budget.
    Markdown(Budget),
}

/// Dump formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dump {
    /// Readable text (innerText approximation without CSS).
    Text,
    /// HTML serialization.
    Html,
    /// Indented element outline with key attributes and quoted text.
    Tree,
    /// Element outline with boxes and flags at the default viewport (see [`Document::boxes`]).
    Boxes,
    /// The agent view at the default viewport and a 4,000-token budget (see [`Document::ax`]).
    Ax,
    /// The main content as Markdown, same defaults (see [`Document::markdown`]).
    Markdown,
    /// Every request of the load, one per line (see [`Document::requests`]).
    Requests,
    /// Script errors of the page and its frames, one per line.
    Errors,
}

/// Parses an HTML string as if it were loaded from `base_url` (no network, no environment).
/// `srcdoc` frames are loaded; `src` frames are kept as failed (no environment).
pub fn parse(html: &str, base_url: &str) -> Document {
    let mut doc = parse_without_frames(html, base_url);
    frames::load_all(
        &mut doc,
        None,
        &Default::default(),
        LoadOptions::default(),
        None,
    );
    doc
}

pub(crate) fn parse_without_frames(html: &str, base_url: &str) -> Document {
    Document {
        dom: pageon_dom::parse_document(html, pageon_dom::ParseOptions::default()),
        final_url: base_url.to_string(),
        status: 200,
        content_type: "text/html".into(),
        encoding: Encoding {
            name: "UTF-8",
            source: "none",
        },
        summary: Summary {
            bytes: html.len(),
            ..Summary::default()
        },
        stylesheets: Vec::new(),
        frames: Vec::new(),
        nav: next_nav(),
        js: JsState::default(),
        requests: Vec::new(),
        refusal: None,
        loaded_at: 0,
        client: None,
        live: None,
        images: None,
        page: None,
        context: 0,
    }
}

impl Document {
    /// The URL after redirects.
    pub fn final_url(&self) -> &str {
        &self.final_url
    }

    /// HTTP status of the final response (200 for files and strings).
    pub fn status(&self) -> u16 {
        self.status
    }

    /// MIME type essence, e.g. `text/html`.
    pub fn content_type(&self) -> &str {
        &self.content_type
    }

    /// The encoding the bytes were decoded with.
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Load facts.
    pub fn summary(&self) -> Summary {
        self.summary
    }

    /// Document mode.
    pub fn mode(&self) -> QuirksMode {
        self.dom.quirks_mode
    }

    /// The document node.
    pub fn root(&self) -> NodeId {
        self.dom.root()
    }

    /// The node behind a handle; `None` once removed (never another node).
    pub fn node(&self, id: NodeId) -> Option<NodeRef<'_>> {
        self.dom.get(id).map(|node| NodeRef { node })
    }

    /// Parent.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.dom.parent(id)
    }

    /// Children in order.
    pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.dom.children(id)
    }

    /// First child.
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.dom.first_child(id)
    }

    /// Next sibling.
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.dom.next_sibling(id)
    }

    /// Serializes a node (the document: the whole page) as HTML.
    pub fn to_html(&self, id: NodeId) -> String {
        self.dom.to_html(id)
    }

    /// A dump of the whole document.
    pub fn dump(&self, kind: Dump) -> String {
        match kind {
            Dump::Text => {
                let flat = self.flat_dom();
                flat.text(flat.root())
            }
            Dump::Html => self.dom.to_html(self.dom.root()),
            Dump::Tree => self.dom.tree(self.dom.root()),
            Dump::Boxes => self.boxes(Viewport::default()),
            Dump::Ax => self.ax(Viewport::default(), Budget::o200k(4000)),
            Dump::Markdown => self.markdown(Viewport::default(), Budget::o200k(4000)),
            Dump::Errors => {
                let mut out = Vec::new();
                let mut stack = vec![self];
                while let Some(d) = stack.pop() {
                    out.extend(d.errors().iter().map(ToString::to_string));
                    stack.extend(d.frames().iter().filter_map(|f| f.document()));
                }
                if out.is_empty() {
                    "no script errors".into()
                } else {
                    out.join("\n")
                }
            }
            Dump::Requests => self
                .requests
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// A view at a viewport and token budget (`boxes`, `ax` and `markdown` use them; the
    /// other dumps ignore them).
    pub fn view(&self, kind: Dump, viewport: Viewport, budget: Budget) -> String {
        match kind {
            Dump::Boxes => self.boxes(viewport),
            Dump::Ax => self.ax(viewport, budget),
            Dump::Markdown => self.markdown(viewport, budget),
            other => self.dump(other),
        }
    }

    /// One line about the load: URL, status, type, encoding, size, time, and the counts that
    /// are not zero (redirects, stylesheets, scripts, errors, timers, requests, blocked).
    /// The site's refusal of the agent (a block, a bot challenge, a rate limit), read from the
    /// final top-level response after the page settled. pageon never solves it.
    pub fn refusal(&self) -> Option<&pageon_net::Refusal> {
        self.refusal.as_ref()
    }

    /// The page's status line.
    pub fn status_line(&self) -> String {
        let s = self.summary;
        let enc = self.encoding;
        let extras = [
            (s.redirects > 0).then(|| format!(" · {} redirect(s)", s.redirects)),
            (s.stylesheets + s.stylesheets_failed > 0)
                .then(|| format!(" · {} stylesheet(s)", s.stylesheets)),
            (s.stylesheets_failed > 0).then(|| format!(" · {} failed", s.stylesheets_failed)),
            (s.scripts > 0).then(|| format!(" · {} script(s)", s.scripts)),
            (s.errors > 0).then(|| format!(" · {} error(s) (--dump errors)", s.errors)),
            (s.pending_timers > 0).then(|| format!(" · {} timer(s) pending", s.pending_timers)),
            (s.requests > 1).then(|| format!(" · {} requests", s.requests)),
            (s.blocked > 0).then(|| format!(" · {} blocked (--dump requests)", s.blocked)),
            (s.pending_requests > 0)
                .then(|| format!(" · {} request(s) pending", s.pending_requests)),
            self.refusal.as_ref().map(|r| format!(" · refused: {r}")),
        ];
        format!(
            "{} · {} · {} · {} ({}) · {} B · {} ms{}",
            self.final_url,
            self.status,
            self.content_type,
            enc.name,
            enc.source,
            s.bytes,
            s.ms,
            extras.into_iter().flatten().collect::<String>()
        )
    }

    /// Every request the load made, in order: the document, its redirects' end, stylesheets,
    /// scripts, frames, `fetch` and XHR (with CORS preflights), each with its outcome. Blocked
    /// and CORS-refused requests are listed with the reason; so are rejected cookies. Kept on
    /// the top-level document (frames' requests included).
    pub fn requests(&self) -> &[RequestEntry] {
        &self.requests
    }

    /// Errors of the document's scripts (frames have their own).
    pub fn errors(&self) -> &[PageError] {
        &self.js.errors
    }

    /// Dialogs the scripts opened with `showModal()`, oldest first.
    pub fn modals(&self) -> &[NodeId] {
        &self.js.modals
    }

    /// The elements listening for `drop` when the page settled.
    pub(crate) fn drop_targets(&self) -> &[NodeId] {
        &self.js.drop_targets
    }

    /// The element focused when the page settled.
    pub fn focused(&self) -> Option<NodeId> {
        self.js.focused
    }

    /// The frames of this document (each frame's document has its own frames).
    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// The page state the agent views read (DOM, layout and accessibility tree of every frame),
    /// laid out at `viewport`. Feed it to an [`Episode`] for refs that persist across steps and
    /// for diffs.
    pub fn capture(&self, viewport: Viewport) -> Capture {
        capture::build(self, viewport).0
    }

    /// The agent view: landmarks, headings, quoted text and actionable elements with refs, cut to
    /// `budget` (format v1).
    pub fn ax(&self, viewport: Viewport, budget: Budget) -> String {
        pageon_agent::ax(&self.capture(viewport), budget)
    }

    /// The main content as Markdown: visible text only, links absolute and carrying the refs of
    /// the [`Document::ax`] view, cut to `budget`.
    pub fn markdown(&self, viewport: Viewport, budget: Budget) -> String {
        markdown::render(self, viewport, budget)
    }

    /// The node a [`Target`] (from [`Episode::resolve`]) points at: the frame index (`0` = this
    /// document, then frames breadth-first, as in the capture) and the node in that frame's
    /// document.
    pub fn node_for(&self, target: &Target) -> Option<(usize, NodeId)> {
        let frame: usize = target.frame_id.strip_prefix('F')?.parse().ok()?;
        let doc = capture::frame_documents(self).into_iter().nth(frame)??;
        let (index, generation) = capture::dom_index(target.backend_node_id)?;
        // Shadow trees are walked through the composed tree (same ids).
        let composed = doc.dom().has_shadow().then(|| doc.dom().composed().dom);
        let dom = composed.as_ref().unwrap_or(doc.dom());
        let id = dom.traverse(dom.root()).find_map(|e| match e {
            pageon_dom::Edge::Enter(id)
                if id.index() == index && id.generation() & 0xffff == generation =>
            {
                Some(id)
            }
            _ => None,
        })?;
        Some((frame, id))
    }

    /// Styles and lays out the document at `viewport` (stylesheets loaded with the page apply).
    pub fn layout(&self, viewport: Viewport) -> Layout {
        let opts = pageon_layout::LayoutOptions {
            modals: &self.js.modals,
            images: self.images.as_deref(),
            redact: &[],
            scroll: &self.js.scroll.elements,
        };
        let l = Layout::with_options(
            &self.dom,
            viewport,
            &self.stylesheets,
            Some(&self.final_url),
            &opts,
        );
        let (x, y) = self.js.scroll.viewport;
        l.set_viewport_scroll(x as f32, y as f32);
        l
    }

    /// The element outline with boxes (`x,y w×h`, whole CSS px) and flags (`hidden`,
    /// `offscreen`, `covered-by:<tag>`); elements that are not rendered are omitted.
    pub fn boxes(&self, viewport: Viewport) -> String {
        boxes::dump(&self.dom, &self.layout(viewport))
    }

    /// The DOM as it renders: the composed tree when the page has shadow trees (same ids),
    /// else the DOM itself.
    pub fn flat_dom(&self) -> std::borrow::Cow<'_, Dom> {
        if self.dom.has_shadow() {
            std::borrow::Cow::Owned(self.dom.composed().dom)
        } else {
            std::borrow::Cow::Borrowed(&self.dom)
        }
    }

    /// The underlying document model (mutations, traversal).
    pub fn dom(&self) -> &Dom {
        &self.dom
    }

    /// Mutable access to the document model.
    pub fn dom_mut(&mut self) -> &mut Dom {
        &mut self.dom
    }
}
