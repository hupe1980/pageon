//! The box tree: one node per rendered element, text, pseudo-element and anonymous block,
//! built from pageon's document (flat tree) with the styler's styles. Layout state (Taffy's
//! cache and results, inline layouts) lives on its nodes.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use html5ever::{LocalName, local_name};
use pageon_dom::{Dom, NodeData, NodeId};
use style::properties::ComputedValues;
use style::selector_parser::PseudoElement;
use style::servo_arc::Arc;
use style::values::computed::Display;

use crate::images::{Decoded, Images};

/// A box's index in [`Tree::nodes`].
pub(crate) type BoxId = usize;

/// The deepest nesting laid out as written (deeper elements are flattened onto that level).
/// Layout recurses per level; debug builds use far larger frames, so they flatten earlier to
/// survive a 2 MiB thread stack (Rust's default for spawned threads).
pub(crate) const MAX_DEPTH: usize = if cfg!(debug_assertions) { 96 } else { 200 };

/// What a box is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// The document: the root of the tree, laid out as a block.
    Document,
    /// An element of the document.
    Element,
    /// A text node of the document (or a pseudo-element's generated text).
    Text,
    /// A block wrapping inline content among blocks (CSS 2 anonymous block box).
    Anonymous,
    /// A `::before` or `::after` pseudo-element.
    Pseudo,
}

/// The brush of a span of text: the box whose style it has.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct TextBrush {
    pub(crate) id: BoxId,
}

impl TextBrush {
    pub(crate) fn from_id(id: BoxId) -> Self {
        Self { id }
    }
}

/// An inline formatting context: its text and Parley layout.
#[derive(Clone, Default)]
pub(crate) struct TextLayout {
    pub(crate) text: String,
    pub(crate) content_widths: Option<parley::ContentWidths>,
    pub(crate) layout: parley::Layout<TextBrush>,
    /// Block-axis offset (CSS px) of the line boxes from the top of the content box
    /// (`align-content`).
    pub(crate) block_offset: f32,
    /// The out-of-flow positions for which an inline span above an inline box is a
    /// containing block (`SPAN_*_CB`); the inline root claims those boxes in its place.
    pub(crate) span_cb_flags: stylo_taffy::StyleFlags,
}

/// A list item's marker.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Marker {
    Char(char),
    String(String),
}

/// Where a list item's marker goes.
#[derive(Clone)]
pub(crate) enum ListItemLayoutPosition {
    Inside,
    Outside(Box<parley::Layout<TextBrush>>),
}

/// A list item's marker and its layout.
#[derive(Clone)]
pub(crate) struct ListItemLayout {
    pub(crate) marker: Marker,
    pub(crate) position: ListItemLayoutPosition,
}

/// A form control's text (its value or label), laid out on its own.
#[derive(Clone)]
pub(crate) struct ControlText {
    /// The text shown (secrets as bullets; a password's value as bullets).
    pub(crate) text: String,
    pub(crate) multiline: bool,
    pub(crate) layout: parley::Layout<TextBrush>,
    /// Vertical scroll of a multi-line control's text (CSS px).
    pub(crate) scroll: f32,
}

/// Data some elements have by their kind.
#[derive(Default)]
pub(crate) enum Special {
    #[default]
    None,
    /// An image (`<img>`, `<input type=image>`): the decoded image, when known.
    Image(Option<Arc<Decoded>>),
    /// A text control (`<input>` of a text type, `<textarea>`).
    TextInput(Box<ControlText>),
    /// A checkbox or radio button: checked.
    Checkbox(bool),
    /// A table's grid (set by construction).
    Table(std::rc::Rc<super::table::TableContext>),
    /// A frame's document, laid out on its own (`<iframe>`).
    SubDocument(Box<crate::Layout>),
}

/// Layout results and Taffy's cache.
#[derive(Clone, Default)]
pub(crate) struct LayoutData {
    pub(crate) cache: taffy::Cache,
    pub(crate) unrounded: taffy::Layout,
    pub(crate) layout: taffy::Layout,
    /// Content scroll offset (CSS px).
    pub(crate) scroll: kurbo::Vec2,
}

/// Construction state of a box.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Flags {
    /// Lays out its content as one inline formatting context (Parley).
    pub(crate) inline_root: bool,
    /// A table laid out on Taffy's grid.
    pub(crate) table_root: bool,
}

/// A box.
pub(crate) struct Node {
    pub(crate) id: BoxId,
    /// The document node it is for (`None` for anonymous blocks, pseudo-elements and their
    /// text).
    pub(crate) dom: Option<NodeId>,
    pub(crate) kind: Kind,
    /// The element's local name (`div` for anonymous blocks and pseudo-elements).
    pub(crate) tag: LocalName,
    /// Attributes layout reads (`type`, `rows`, `cols`, `width`, `height`, `start`,
    /// `reversed`, `value`, `colspan`, `rowspan`, `span`); secrets as bullets.
    pub(crate) attrs: Vec<(LocalName, String)>,
    /// A text box's text (secrets as bullets).
    pub(crate) text: String,
    /// Computed style (elements, anonymous blocks, pseudo-elements; `None` for text).
    pub(crate) style: Option<Arc<ComputedValues>>,
    /// The parent in the flat tree (an anonymous block's or pseudo-element's element).
    pub(crate) parent: Option<BoxId>,
    /// Children in the flat tree (elements and text).
    pub(crate) children: Vec<BoxId>,
    pub(crate) before: Option<BoxId>,
    pub(crate) after: Option<BoxId>,
    /// The parent in the box tree (anonymous blocks and inline roots included).
    pub(crate) layout_parent: Cell<Option<BoxId>>,
    /// Children in the box tree: what Taffy lays out (set by construction).
    pub(crate) layout_children: RefCell<Option<Vec<BoxId>>>,
    /// Out-of-flow boxes this box is the containing block of (Taffy's out-of-flow pass);
    /// their locations are relative to this box's border box.
    pub(crate) hoisted_children: RefCell<Vec<BoxId>>,
    /// For a hoisted box: the box whose `hoisted_children` holds it.
    pub(crate) oof_containing_block: Cell<Option<BoxId>>,
    pub(crate) flags: Flags,
    pub(crate) special: Special,
    /// An inline root's layout.
    pub(crate) inline_layout: Option<Box<TextLayout>>,
    pub(crate) list_item: Option<Box<ListItemLayout>>,
    pub(crate) layout: LayoutData,
    pub(crate) detailed_layout_info: taffy::DetailedLayoutInfo<style::Atom>,
}

impl Node {
    fn new(id: BoxId, kind: Kind, dom: Option<NodeId>) -> Self {
        Self {
            id,
            dom,
            kind,
            tag: LocalName::from(""),
            attrs: Vec::new(),
            text: String::new(),
            style: None,
            parent: None,
            children: Vec::new(),
            before: None,
            after: None,
            layout_parent: Cell::new(None),
            layout_children: RefCell::new(None),
            hoisted_children: RefCell::new(Vec::new()),
            oof_containing_block: Cell::new(None),
            flags: Flags::default(),
            special: Special::None,
            inline_layout: None,
            list_item: None,
            layout: LayoutData::default(),
            detailed_layout_info: taffy::DetailedLayoutInfo::None,
        }
    }

    pub(crate) fn attr(&self, name: &LocalName) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn has_attr(&self, name: &LocalName) -> bool {
        self.attr(name).is_some()
    }

    pub(crate) fn primary_styles(&self) -> Option<&Arc<ComputedValues>> {
        self.style.as_ref()
    }

    /// The computed `display` (`None` for text).
    pub(crate) fn display_style(&self) -> Option<Display> {
        self.style.as_ref().map(|s| s.clone_display())
    }

    pub(crate) fn is_element(&self) -> bool {
        matches!(self.kind, Kind::Element | Kind::Anonymous | Kind::Pseudo)
    }

    pub(crate) fn is_text(&self) -> bool {
        self.kind == Kind::Text
    }

    pub(crate) fn is_anonymous(&self) -> bool {
        self.kind == Kind::Anonymous
    }

    pub(crate) fn is(&self, tag: &LocalName) -> bool {
        self.kind == Kind::Element && &self.tag == tag
    }

    /// Text that collapses away entirely (only spaces, tabs and newlines).
    pub(crate) fn is_whitespace(&self) -> bool {
        self.kind == Kind::Text
            && self
                .text
                .chars()
                .all(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}'))
    }

    pub(crate) fn final_layout(&self) -> &taffy::Layout {
        &self.layout.layout
    }

    pub(crate) fn unrounded_layout(&self) -> &taffy::Layout {
        &self.layout.unrounded
    }

    pub(crate) fn is_inline_root(&self) -> bool {
        self.flags.inline_root
    }

    pub(crate) fn is_table_root(&self) -> bool {
        self.flags.table_root
    }

    /// Taffy's view of the style (replaced elements and buttons are flagged).
    pub(crate) fn layout_style(&self) -> stylo_taffy::TaffyStyloStyle<&ComputedValues> {
        let style = self
            .style
            .as_deref()
            .unwrap_or_else(|| panic!("layout style of a box without style"));
        let mut flags = stylo_taffy::StyleFlags::empty();
        if self.kind == Kind::Element {
            if super::replaced::is_replaced_element(&self.tag) {
                flags |= stylo_taffy::StyleFlags::IS_REPLACED;
            }
            if self.tag == local_name!("button") {
                flags |= stylo_taffy::StyleFlags::IS_BUTTON;
            }
            if self.tag == local_name!("input")
                && matches!(
                    self.attr(&local_name!("type")),
                    Some("button" | "submit" | "reset")
                )
            {
                flags |= stylo_taffy::StyleFlags::IS_BUTTON_INPUT;
            }
        }
        stylo_taffy::TaffyStyloStyle::new(style, flags)
    }

    /// Whether the box is out of flow (absolutely or fixed positioned).
    pub(crate) fn is_out_of_flow(&self) -> bool {
        self.style
            .as_ref()
            .is_some_and(|s| s.clone_position().is_absolutely_positioned())
    }

    pub(crate) fn is_floated(&self) -> bool {
        self.style.as_ref().is_some_and(|s| s.clone_float().is_floating())
    }

    pub(crate) fn scroll_offset(&self) -> kurbo::Vec2 {
        self.layout.scroll
    }
}

/// The box tree of a document.
pub(crate) struct Tree {
    pub(crate) nodes: Vec<Node>,
    /// Boxes by document node.
    pub(crate) by_dom: HashMap<NodeId, BoxId>,
    /// Decoded images (`<img>`, CSS backgrounds), by absolute URL.
    pub(crate) images: Option<Images>,
    /// Image URLs the document uses that `images` does not know.
    pub(crate) wanted: Vec<String>,
    /// How many known images were used.
    pub(crate) applied: usize,
    pub(crate) base: Option<url::Url>,
    pub(crate) viewport: crate::Viewport,
}

/// What a build takes besides the document.
pub(crate) struct Build<'a> {
    pub(crate) styler: &'a pageon_style::Styler,
    pub(crate) images: Option<&'a Images>,
    pub(crate) redact: &'a [String],
    pub(crate) base: &'a str,
    pub(crate) viewport: crate::Viewport,
}

/// Attributes layout reads.
const ATTRS: [&str; 11] = [
    "type", "rows", "cols", "width", "height", "start", "reversed", "value", "colspan",
    "rowspan", "span",
];

pub(crate) fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), &"•".repeat(s.chars().count()));
        }
    }
    out
}

/// The URL of a `srcset`'s `1x` candidate, else its first.
fn srcset_1x(srcset: &str) -> Option<String> {
    let candidates: Vec<(&str, Option<&str>)> = srcset
        .split(',')
        .filter_map(|c| {
            let mut parts = c.split_whitespace();
            Some((parts.next()?, parts.next()))
        })
        .collect();
    candidates
        .iter()
        .find(|(_, d)| matches!(d, None | Some("1x")))
        .or(candidates.first())
        .map(|(u, _)| (*u).to_string())
}

/// Text types of `<input>` (a text box with the value).
fn text_input(kind: Option<&str>) -> bool {
    matches!(
        kind.map(str::to_ascii_lowercase).as_deref(),
        None | Some(
            "text" | "password" | "email" | "number" | "search" | "tel" | "url" | "date"
                | "time" | "datetime-local" | "month" | "week"
        )
    )
}

impl Tree {
    /// Builds the tree of the rendered part of `dom`'s flat tree with the styler's styles.
    pub(crate) fn build(dom: &Dom, b: &Build<'_>) -> Self {
        let mut t = Self {
            nodes: Vec::new(),
            by_dom: HashMap::new(),
            images: b.images.cloned(),
            wanted: Vec::new(),
            applied: 0,
            base: url::Url::parse(b.base).ok(),
            viewport: b.viewport,
        };
        let root = t.push(Node::new(0, Kind::Document, Some(dom.root())));
        t.by_dom.insert(dom.root(), root);
        // Slot assignment, once per host.
        let mut slotted: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for (host, _) in dom.shadow_hosts() {
            for c in dom.children(host) {
                if let Some(s) = dom.assigned_slot(c) {
                    slotted.entry(s).or_default().push(c);
                }
            }
        }
        let kids: Vec<NodeId> = dom.children(dom.root()).collect();
        for c in kids {
            t.add(dom, b, &slotted, c, root, 1);
        }
        t
    }

    fn push(&mut self, mut n: Node) -> BoxId {
        let id = self.nodes.len();
        n.id = id;
        self.nodes.push(n);
        id
    }

    /// Flat-tree children of `id`: a host's shadow tree, a slot's assigned nodes (else its
    /// fallback), otherwise its children.
    fn flat_children(dom: &Dom, slotted: &HashMap<NodeId, Vec<NodeId>>, id: NodeId) -> Vec<NodeId> {
        if let Some(root) = dom.shadow_root(id) {
            return dom.children(root).collect();
        }
        if let Some(mine) = slotted.get(&id) {
            return mine.clone();
        }
        dom.children(id).collect()
    }

    fn add(
        &mut self,
        dom: &Dom,
        b: &Build<'_>,
        slotted: &HashMap<NodeId, Vec<NodeId>>,
        id: NodeId,
        parent: BoxId,
        depth: usize,
    ) {
        let Some(node) = dom.get(id) else { return };
        match &node.data {
            NodeData::Text(text) => {
                // Text directly in a closed <details> is not rendered (only the summary is).
                if self.nodes[parent].is(&local_name!("details"))
                    && !self.nodes[parent].has_attr(&LocalName::from("open"))
                {
                    return;
                }
                let mut n = Node::new(0, Kind::Text, Some(id));
                n.text = redact(text, b.redact);
                n.parent = Some(parent);
                let t = self.push(n);
                self.by_dom.insert(id, t);
                self.nodes[parent].children.push(t);
            }
            NodeData::Element { name, attrs, .. } => {
                let Some(styles) = b.styler.element_styles(dom, id) else {
                    return;
                };
                let Some(style) = styles.get_primary().cloned() else {
                    return;
                };
                let local = name.local.clone();
                let mut n = Node::new(0, Kind::Element, Some(id));
                n.attrs = attrs
                    .iter()
                    .filter(|a| ATTRS.contains(&&*a.qual_name().local))
                    .map(|a| (a.qual_name().local.clone(), redact(&a.value, b.redact)))
                    .collect();
                // `open` on <details> decides whether its text is shown.
                if local == local_name!("details") && node.attr("open").is_some() {
                    n.attrs.push((LocalName::from("open"), String::new()));
                }
                n.tag = local.clone();
                n.style = Some(style.clone());
                n.parent = Some(parent);
                let undisplayed = style.clone_display().is_none();
                let me = self.push(n);
                self.by_dom.insert(id, me);
                // Deeper than MAX_DEPTH: laid out as a child of the ancestor at that depth.
                let mut at = parent;
                if depth > MAX_DEPTH {
                    at = self.nodes[parent].parent.unwrap_or(parent);
                    self.nodes[me].parent = Some(at);
                }
                self.nodes[at].children.push(me);
                if undisplayed {
                    return;
                }
                self.special(dom, b, id, me, &local, node);
                // Pseudo-elements with generated content.
                for (pseudo, before) in [(PseudoElement::Before, true), (PseudoElement::After, false)]
                {
                    let Some(ps) = styles.pseudos.get(&pseudo).cloned() else {
                        continue;
                    };
                    let mut p = Node::new(0, Kind::Pseudo, None);
                    p.tag = local_name!("div");
                    p.parent = Some(me);
                    let text = super::construct::content_text(&ps);
                    p.style = Some(ps);
                    let pid = self.push(p);
                    if let Some(text) = text {
                        let mut tn = Node::new(0, Kind::Text, None);
                        tn.text = text;
                        tn.parent = Some(pid);
                        let tid = self.push(tn);
                        self.nodes[pid].children.push(tid);
                    }
                    if before {
                        self.nodes[me].before = Some(pid);
                    } else {
                        self.nodes[me].after = Some(pid);
                    }
                }
                // Replaced content: an input's or textarea's children, a sheet's text, an
                // image's or frame's fallback are not rendered.
                if matches!(&*local, "input" | "textarea" | "style" | "img" | "iframe") {
                    return;
                }
                for c in Self::flat_children(dom, slotted, id) {
                    self.add(dom, b, slotted, c, me, depth + 1);
                }
            }
            _ => {}
        }
    }

    /// Data an element has by its kind: images, control text, checked state.
    fn special(
        &mut self,
        dom: &Dom,
        b: &Build<'_>,
        id: NodeId,
        me: BoxId,
        local: &LocalName,
        node: &pageon_dom::Node,
    ) {
        let state = dom.control(id);
        match &**local {
            "img" => {
                let src = node
                    .attr("src")
                    .map(str::to_string)
                    .or_else(|| node.attr("srcset").and_then(srcset_1x));
                let image = src.and_then(|s| self.image(&s));
                self.nodes[me].special = Special::Image(image);
            }
            "input" => {
                let kind = node.attr("type");
                let lower = kind.map(str::to_ascii_lowercase);
                match lower.as_deref() {
                    Some("checkbox" | "radio") => {
                        let checked = state
                            .and_then(|c| c.checked)
                            .unwrap_or_else(|| node.attr("checked").is_some());
                        self.nodes[me].special = Special::Checkbox(checked);
                    }
                    Some("image") => {
                        let image = node.attr("src").and_then(|s| self.image(s));
                        self.nodes[me].special = Special::Image(image);
                    }
                    _ if text_input(kind) => {
                        let value = state
                            .and_then(|c| c.value.clone())
                            .or_else(|| node.attr("value").map(str::to_string))
                            .unwrap_or_default();
                        let mut text = redact(&value, b.redact);
                        if lower.as_deref() == Some("password") {
                            text = "•".repeat(text.chars().count());
                        }
                        self.nodes[me].special = Special::TextInput(Box::new(ControlText {
                            text,
                            multiline: false,
                            layout: parley::Layout::new(),
                            scroll: 0.0,
                        }));
                    }
                    _ => {}
                }
            }
            "textarea" => {
                let value = state.and_then(|c| c.value.clone()).unwrap_or_else(|| {
                    dom.children(id)
                        .filter_map(|c| match dom.get(c).map(|n| &n.data) {
                            Some(NodeData::Text(t)) => Some(t.as_str()),
                            _ => None,
                        })
                        .collect()
                });
                self.nodes[me].special = Special::TextInput(Box::new(ControlText {
                    text: redact(&value, b.redact),
                    multiline: true,
                    layout: parley::Layout::new(),
                    scroll: 0.0,
                }));
            }
            _ => {}
        }
    }

    /// A decoded image by URL relative to the document, recording URLs not known yet.
    pub(crate) fn image(&mut self, url: &str) -> Option<Arc<Decoded>> {
        let abs = match &self.base {
            Some(b) => b.join(url).ok()?.to_string(),
            None => url::Url::parse(url).ok()?.to_string(),
        };
        let images = self.images.as_ref()?;
        match images.get(&abs) {
            Some(d @ Decoded::Raster { .. }) => {
                self.applied += 1;
                Some(Arc::new(d.clone()))
            }
            Some(Decoded::Failed(_)) => None,
            None => {
                if !self.wanted.contains(&abs) {
                    self.wanted.push(abs);
                }
                None
            }
        }
    }

    /// The box of a document node.
    pub(crate) fn of(&self, id: NodeId) -> Option<&Node> {
        self.by_dom.get(&id).map(|&b| &self.nodes[b])
    }

    pub(crate) fn root(&self) -> BoxId {
        0
    }
}

impl std::ops::Index<BoxId> for Tree {
    type Output = Node;
    fn index(&self, i: BoxId) -> &Node {
        &self.nodes[i]
    }
}

impl std::ops::IndexMut<BoxId> for Tree {
    fn index_mut(&mut self, i: BoxId) -> &mut Node {
        &mut self.nodes[i]
    }
}
