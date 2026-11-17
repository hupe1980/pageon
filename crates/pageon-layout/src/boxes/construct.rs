//! Adapted from Blitz (blitz-dom `layout/construct.rs`, MIT OR Apache-2.0).
//!
//! Box construction: each box's layout children (anonymous blocks around inline content among
//! blocks, `display: contents` hoisted), inline formatting contexts (Parley layouts with inline
//! boxes), tables, list markers and form controls.

use html5ever::local_name;
use parley::{
    FontContext, InlineBox, InlineBoxKind, LayoutContext, StyleProperty, TreeBuilder,
    WhiteSpaceCollapse,
};
use style::computed_values::position::T as PositionProperty;
use style::computed_values::white_space_collapse::T as StyloWhiteSpaceCollapse;
use style::properties::ComputedValues;
use style::values::computed::{Content, ContentItem, Display, Float};
use style::values::specified::box_::{DisplayInside, DisplayOutside};

use super::list::{BULLET_FONT_FAMILY, collect_list_item_children};
use super::parley_style;
use super::replaced::is_inline_box_element;
use super::table::build_table_context;
use super::text_transform::{CaseTransform, TextTransformer};
use super::tree::{
    BoxId, ControlText, Kind, ListItemLayout, ListItemLayoutPosition, Marker, Node, Special,
    TextBrush, TextLayout, Tree,
};

/// The text a pseudo-element's `content` generates (strings only).
pub(crate) fn content_text(style: &ComputedValues) -> Option<String> {
    match &style.get_counters().content {
        Content::Items(item_data) => {
            let items = &item_data.items[0..item_data.alt_start];
            let mut text = String::new();
            let mut has_string = false;
            for item in items {
                if let ContentItem::String(s) = item {
                    text.push_str(s);
                    has_string = true;
                }
            }
            has_string.then_some(text)
        }
        _ => None,
    }
}

/// What kind of node a box is, as construction distinguishes them (pseudo-elements are
/// anonymous blocks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeKind {
    Document,
    Element,
    AnonymousBlock,
    Text,
}

pub(crate) fn kind(n: &Node) -> NodeKind {
    match n.kind {
        Kind::Document => NodeKind::Document,
        Kind::Element => NodeKind::Element,
        Kind::Anonymous | Kind::Pseudo => NodeKind::AnonymousBlock,
        Kind::Text => NodeKind::Text,
    }
}

/// An element, anonymous block or pseudo-element (a box with element data).
pub(crate) fn has_element_data(n: &Node) -> bool {
    matches!(n.kind, Kind::Element | Kind::Anonymous | Kind::Pseudo)
}

/// Whether the box is block-level, or an in-flow inline (flow) box with a block-level
/// descendant.
pub(crate) fn is_or_contains_block(tree: &Tree, id: BoxId) -> bool {
    let n = &tree[id];
    let style = n.primary_styles();
    // Out-of-flow boxes are ignored.
    let position = style
        .map(|s| s.clone_position())
        .unwrap_or(PositionProperty::Relative);
    let in_flow = matches!(
        position,
        PositionProperty::Static | PositionProperty::Relative | PositionProperty::Sticky
    );
    if !in_flow {
        return false;
    }
    // Floats do not break up the inline flow: they take part in it as out-of-flow inline
    // boxes.
    if style.is_some_and(|s| s.clone_float().is_floating()) {
        return false;
    }
    let display = n.display_style().unwrap_or(Display::inline());
    match display.outside() {
        DisplayOutside::None => false,
        DisplayOutside::Block => true,
        _ => {
            display.inside() == DisplayInside::Flow
                && n.children.iter().any(|&c| is_or_contains_block(tree, c))
        }
    }
}

/// An empty or whitespace-only text box whose whitespace collapses away under its parent's
/// `white-space-collapse`.
pub(crate) fn is_collapsible_whitespace(tree: &Tree, id: BoxId) -> bool {
    let n = &tree[id];
    if n.kind != Kind::Text {
        return false;
    }
    if n.text.is_empty() {
        return true;
    }
    if !n.text.chars().all(|c| c.is_ascii_whitespace()) {
        return false;
    }
    let collapse = n
        .parent
        .and_then(|p| tree[p].primary_styles())
        .map(|s| s.clone_white_space_collapse());
    match collapse {
        Some(StyloWhiteSpaceCollapse::Preserve | StyloWhiteSpaceCollapse::BreakSpaces) => false,
        Some(StyloWhiteSpaceCollapse::PreserveBreaks) => !n.text.contains('\n'),
        Some(StyloWhiteSpaceCollapse::Collapse) | None => true,
    }
}

/// A whitespace-only text box.
fn is_whitespace_node(n: &Node) -> bool {
    n.kind == Kind::Text && n.text.chars().all(|c| c.is_ascii_whitespace())
}

/// A flex or grid item's `order` (0 for out-of-flow boxes and text).
fn order(n: &Node) -> i32 {
    n.primary_styles()
        .filter(|s| !s.clone_position().is_absolutely_positioned())
        .map(|s| s.clone_order())
        .unwrap_or(0)
}

/// Construction state: the tree being built and what building it takes.
pub(crate) struct Construct<'a> {
    pub(crate) tree: &'a mut Tree,
    pub(crate) styler: &'a pageon_style::Styler,
    pub(crate) fonts: &'a mut FontContext,
    pub(crate) layouts: &'a mut LayoutContext<TextBrush>,
    /// Inline roots whose Parley layout is built once every box is constructed.
    deferred: Vec<BoxId>,
}

/// The layout children being collected, and the open anonymous block (if any).
#[derive(Default)]
pub(crate) struct LayoutChildren {
    pub(crate) children: Vec<BoxId>,
    anonymous_block_id: Option<BoxId>,
}

/// Wrapping policy of the nearest container that is not `display: contents`: children hoisted
/// out of `display: contents` boxes are wrapped as if they were the container's own.
#[derive(Clone, Copy)]
struct WrapContext {
    /// The container whose style anonymous blocks inherit.
    container: BoxId,
    needs_wrap: fn(NodeKind, DisplayOutside) -> bool,
}

fn block_item_needs_wrap(kind: NodeKind, outside: DisplayOutside) -> bool {
    kind == NodeKind::Text || outside == DisplayOutside::Inline
}

/// For flex and grid containers, and flow containers whose children are all out of flow (inline
/// elements are pushed as they are, but bare text cannot be laid out alone).
fn text_item_needs_wrap(kind: NodeKind, _outside: DisplayOutside) -> bool {
    kind == NodeKind::Text
}

/// The classification of a flow container's in-flow children.
struct FlowClassification {
    all_block: bool,
    all_inline: bool,
    all_out_of_flow: bool,
    has_contents: bool,
}

impl Default for FlowClassification {
    fn default() -> Self {
        Self {
            all_block: true,
            all_inline: true,
            all_out_of_flow: true,
            has_contents: false,
        }
    }
}

impl<'a> Construct<'a> {
    pub(crate) fn new(
        tree: &'a mut Tree,
        styler: &'a pageon_style::Styler,
        fonts: &'a mut FontContext,
        layouts: &'a mut LayoutContext<TextBrush>,
    ) -> Self {
        Self {
            tree,
            styler,
            fonts,
            layouts,
            deferred: Vec::new(),
        }
    }

    /// Constructs every box from the document box down, then builds the inline layouts.
    pub(crate) fn run(mut self) {
        let root = self.tree.root();
        self.resolve_layout_children(root);
        self.resolve_deferred_tasks();
        // Pseudo-elements construction never reached generate no box.
        for n in &mut self.tree.nodes {
            if !n.pseudos_flushed {
                n.before = None;
                n.after = None;
            }
        }
    }

    /// The `::before` box of `id`, once construction reached it.
    fn before(&self, id: BoxId) -> Option<BoxId> {
        let n = &self.tree[id];
        n.before.filter(|_| n.pseudos_flushed)
    }

    fn after(&self, id: BoxId) -> Option<BoxId> {
        let n = &self.tree[id];
        n.after.filter(|_| n.pseudos_flushed)
    }

    /// Fills in the layout children of `id` and, recursively, of each of them.
    fn resolve_layout_children(&mut self, id: BoxId) {
        let mut collected = LayoutChildren::default();
        self.collect_layout_children(id, &mut collected, None);
        let children = collected.children;
        for &c in &children {
            self.resolve_layout_children(c);
            self.tree[c].layout_parent.set(Some(id));
        }
        *self.tree[id].layout_children.borrow_mut() = Some(children);
    }

    /// Builds the Parley layout of every inline root.
    fn resolve_deferred_tasks(&mut self) {
        let mut ids = std::mem::take(&mut self.deferred);
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let mut layout = Box::new(TextLayout::default());
            build_inline_layout_into(self.tree, self.layouts, self.fonts, &mut layout, 1.0, id);
            let n = &mut self.tree[id];
            n.layout.cache.clear();
            n.inline_layout = Some(layout);
        }
    }

    /// Pseudo-elements become boxes when construction reaches their element.
    fn flush_pseudo_elements(&mut self, id: BoxId) {
        if !has_element_data(&self.tree[id]) {
            return;
        }
        self.tree[id].pseudos_flushed = true;
        let n = &self.tree[id];
        for pe in [n.before, n.after].into_iter().flatten() {
            self.tree[pe].layout_parent.set(Some(id));
        }
    }

    fn push(&mut self, out: &mut LayoutChildren, child: BoxId) {
        self.maybe_push_anon_block(out);
        out.children.push(child);
    }

    fn extend(&mut self, out: &mut LayoutChildren, slice: &[BoxId]) {
        self.maybe_push_anon_block(out);
        out.children.extend_from_slice(slice);
    }

    /// Closes the open anonymous block, dropping it when it holds only whitespace.
    fn maybe_push_anon_block(&mut self, out: &mut LayoutChildren) {
        if let Some(anon) = out.anonymous_block_id {
            let only_whitespace = self.tree[anon]
                .children
                .iter()
                .all(|&c| is_collapsible_whitespace(self.tree, c));
            if only_whitespace {
                // By identity: hoisted `display: contents` children may follow it.
                if let Some(pos) = out.children.iter().rposition(|&c| c == anon) {
                    out.children.remove(pos);
                }
            }
        }
        out.anonymous_block_id = None;
    }

    fn push_wrapped(&mut self, out: &mut LayoutChildren, container: BoxId, child: BoxId) {
        let anon = match out.anonymous_block_id {
            Some(a) => a,
            None => self.create_anonymous_block(out, container),
        };
        self.tree[anon].children.push(child);
    }

    fn create_anonymous_block(&mut self, out: &mut LayoutChildren, container: BoxId) -> BoxId {
        let parent_style = self.tree[container]
            .primary_styles()
            .cloned()
            .unwrap_or_else(|| panic!("anonymous block in a box without style"));
        let mut n = Node::new(0, Kind::Anonymous, None);
        n.tag = local_name!("div");
        n.style = Some(self.styler.anonymous_style(&parent_style));
        n.parent = Some(container);
        n.layout_parent.set(Some(container));
        let id = self.tree.nodes.len();
        n.id = id;
        self.tree.nodes.push(n);
        out.children.push(id);
        out.anonymous_block_id = Some(id);
        id
    }

    /// Pushes a hoisted child, recursing through `display: contents` boxes and wrapping text
    /// and inline children per the container's [`WrapContext`].
    fn push_hoisted_child(
        &mut self,
        child: BoxId,
        out: &mut LayoutChildren,
        wrap: Option<WrapContext>,
    ) {
        let c = &self.tree[child];
        let display = c.display_style().unwrap_or(Display::inline());
        if matches!(display.inside(), DisplayInside::Contents) {
            self.collect_layout_children(child, out, wrap);
            return;
        }
        if let Some(w) = wrap {
            let outside = if is_or_contains_block(self.tree, child) {
                DisplayOutside::Block
            } else {
                display.outside()
            };
            if (w.needs_wrap)(kind(c), outside) {
                self.push_wrapped(out, w.container, child);
                return;
            }
        }
        self.push(out, child);
    }

    /// Pushes the container's children and pseudo-elements, hoisting through `display:
    /// contents` boxes and leaving out collapsible whitespace.
    fn push_hoisted_children_and_pseudos(
        &mut self,
        container: BoxId,
        out: &mut LayoutChildren,
        wrap: Option<WrapContext>,
    ) {
        if let Some(b) = self.before(container) {
            self.push_hoisted_child(b, out, wrap);
        }
        let children = self.tree[container].children.clone();
        for c in children {
            if is_collapsible_whitespace(self.tree, c) {
                continue;
            }
            self.push_hoisted_child(c, out, wrap);
        }
        if let Some(a) = self.after(container) {
            self.push_hoisted_child(a, out, wrap);
        }
    }

    fn push_non_whitespace_children_and_pseudos(&self, out: &mut Vec<BoxId>, id: BoxId) {
        out.extend(self.before(id));
        out.extend(
            self.tree[id]
                .children
                .iter()
                .copied()
                .filter(|&c| !is_whitespace_node(&self.tree[c])),
        );
        out.extend(self.after(id));
    }

    fn push_children_and_pseudos(&self, out: &mut Vec<BoxId>, id: BoxId) {
        out.extend(self.before(id));
        out.extend(self.tree[id].children.iter().copied());
        out.extend(self.after(id));
    }

    /// Classifies a flow container's children (and `display: contents` pseudo-elements) as
    /// all-block, all-inline and/or all-out-of-flow, recursing through `display: contents`.
    fn classify_flow_children(&self, container: BoxId, c: &mut FlowClassification) {
        let n = &self.tree[container];
        // `display: contents` pseudo-elements are transparent: their children vote. Others
        // generate a box of their own, which every construction arm pushes.
        let pseudos = self
            .before(container)
            .into_iter()
            .chain(self.after(container))
            .filter(|&pe| {
                let d = self.tree[pe].display_style().unwrap_or(Display::inline());
                matches!(d.inside(), DisplayInside::Contents)
            });
        let ids: Vec<BoxId> = n.children.iter().copied().chain(pseudos).collect();
        for child_id in ids {
            let child = &self.tree[child_id];
            let style = child.primary_styles();
            let display = child.display_style().unwrap_or(Display::inline());
            if matches!(display.inside(), DisplayInside::Contents) {
                c.has_contents = true;
                self.classify_flow_children(child_id, c);
            } else if matches!(display.inside(), DisplayInside::None) {
                continue;
            } else {
                let position = style
                    .map(|s| s.clone_position())
                    .unwrap_or(PositionProperty::Static);
                let float = style.map(|s| s.clone_float()).unwrap_or(Float::None);
                if is_collapsible_whitespace(self.tree, child_id) {
                    continue;
                }
                if matches!(display.outside(), DisplayOutside::None) {
                    continue;
                }
                let in_flow = matches!(
                    position,
                    PositionProperty::Static
                        | PositionProperty::Relative
                        | PositionProperty::Sticky
                ) && matches!(float, Float::None);
                if !in_flow {
                    continue;
                }
                c.all_out_of_flow = false;
                match display.outside() {
                    DisplayOutside::None => {}
                    DisplayOutside::Block
                    | DisplayOutside::TableCaption
                    | DisplayOutside::InternalTable => c.all_inline = false,
                    DisplayOutside::Inline => {
                        c.all_block = false;
                        // An inline containing a block needs the anonymous-block fix-up.
                        if is_or_contains_block(self.tree, child_id) {
                            c.all_inline = false;
                        }
                    }
                }
            }
        }
    }

    fn collect_layout_children(
        &mut self,
        container: BoxId,
        out: &mut LayoutChildren,
        wrap: Option<WrapContext>,
    ) {
        {
            let n = &mut self.tree[container];
            n.flags = Default::default();
            n.inline_layout = None;
        }
        self.flush_pseudo_elements(container);

        if has_element_data(&self.tree[container]) {
            let n = &self.tree[container];
            let tag = n.tag.clone();
            if matches!(&*tag, "input" | "textarea") {
                let type_attr = n.attr(&local_name!("type"));
                if &*tag == "textarea" {
                    self.create_text_editor(container, true);
                    return;
                } else if matches!(
                    type_attr,
                    None | Some(
                        "text" | "password" | "email" | "number" | "search" | "tel" | "url"
                    )
                ) {
                    self.create_text_editor(container, false);
                    return;
                } else if matches!(type_attr, Some("checkbox" | "radio")) {
                    self.create_checkbox_input(container);
                    return;
                }
            }
            // Only `<ol>` has `start` and `reversed`.
            let (mut index, reversed) = if &*tag == "ol" {
                (
                    n.attr(&local_name!("start"))
                        .and_then(|v| v.parse::<usize>().ok())
                        .map(|start| start.saturating_sub(1))
                        .unwrap_or(0),
                    n.attr(&local_name!("reversed"))
                        .and_then(|v| v.parse::<bool>().ok())
                        .unwrap_or(false),
                )
            } else {
                (0, false)
            };
            collect_list_item_children(self, &mut index, reversed, container);
        }

        // Nothing to construct without children or pseudo-elements.
        if self.tree[container].children.is_empty()
            && self.before(container).is_none()
            && self.after(container).is_none()
        {
            return;
        }

        let display =
            self.tree[container]
                .display_style()
                .unwrap_or(match kind(&self.tree[container]) {
                    NodeKind::AnonymousBlock => Display::Block,
                    _ => Display::Inline,
                });

        match display.inside() {
            DisplayInside::None => {}
            DisplayInside::Contents => {
                // Transparent for box generation: the children themselves are hoisted into the
                // parent, recursing only through nested `display: contents`.
                self.push_hoisted_children_and_pseudos(container, out, wrap);
            }
            DisplayInside::Flex | DisplayInside::Grid => {
                let n = &self.tree[container];
                let has_text_or_contents = n
                    .children
                    .iter()
                    .copied()
                    .chain(self.before(container))
                    .chain(self.after(container))
                    .map(|c| &self.tree[c])
                    .any(|c| {
                        let d = c.display_style().unwrap_or(Display::inline());
                        d.inside() == DisplayInside::Contents || kind(c) == NodeKind::Text
                    });
                if !has_text_or_contents {
                    self.push_non_whitespace_children_and_pseudos(&mut out.children, container);
                } else {
                    self.collect_complex_layout_children(
                        container,
                        out,
                        true,
                        text_item_needs_wrap,
                    );
                }
                // Flex and grid items are laid out in order-modified document order.
                if !out.children.iter().all(|&c| order(&self.tree[c]) == 0) {
                    let tree = &*self.tree;
                    out.children.sort_by_cached_key(|&c| order(&tree[c]));
                }
            }
            DisplayInside::Table => {
                let (context, children) = build_table_context(self.tree, container);
                let n = &mut self.tree[container];
                n.flags.table_root = true;
                n.special = Special::Table(std::rc::Rc::new(context));
                if let Some(b) = self.before(container) {
                    self.push(out, b);
                }
                self.extend(out, &children);
                if let Some(a) = self.after(container) {
                    self.push(out, a);
                }
            }
            // Flow, flow-root and table cells, and table-internal displays outside a table
            // (laid out as flow containers, so their text is wrapped).
            _ => {
                let mut c = FlowClassification::default();
                self.classify_flow_children(container, &mut c);

                if c.all_out_of_flow {
                    // A `display: contents` child may hold the out-of-flow boxes.
                    let wrap = Some(WrapContext {
                        container,
                        needs_wrap: text_item_needs_wrap,
                    });
                    self.push_hoisted_children_and_pseudos(container, out, wrap);
                    return;
                }

                if c.all_inline {
                    self.deferred.push(container);
                    self.tree[container].flags.inline_root = true;
                    self.find_inline_layout_embedded_boxes(container, &mut out.children);
                    return;
                }

                if c.all_block && !c.has_contents {
                    self.push_non_whitespace_children_and_pseudos(&mut out.children, container);
                    return;
                } else if c.all_inline && !c.has_contents {
                    self.push_children_and_pseudos(&mut out.children, container);
                    return;
                }

                self.collect_complex_layout_children(container, out, false, block_item_needs_wrap);
            }
        }
    }

    /// Children where text or inline boxes need wrapping in anonymous blocks.
    fn collect_complex_layout_children(
        &mut self,
        container: BoxId,
        out: &mut LayoutChildren,
        hide_whitespace: bool,
        needs_wrap: fn(NodeKind, DisplayOutside) -> bool,
    ) {
        let ids: Vec<BoxId> = self
            .before(container)
            .into_iter()
            .chain(self.tree[container].children.iter().copied())
            .chain(self.after(container))
            .collect();
        for child in ids {
            let c = &self.tree[child];
            let child_kind = kind(c);
            let contains_block = is_or_contains_block(self.tree, child);
            let display = c.display_style().unwrap_or(Display::inline());
            let inside = display.inside();
            let outside = if contains_block {
                DisplayOutside::Block
            } else {
                display.outside()
            };
            // `display: none` children stay: Taffy hides them. Whitespace among flex and grid
            // items is left out.
            if hide_whitespace && is_whitespace_node(c) {
            } else if inside == DisplayInside::Contents {
                let wrap = Some(WrapContext {
                    container,
                    needs_wrap,
                });
                self.collect_layout_children(child, out, wrap);
            } else if needs_wrap(child_kind, outside) {
                self.push_wrapped(out, container, child);
            } else {
                self.push(out, child);
            }
        }
        self.maybe_push_anon_block(out);
    }

    /// A text control's text laid out with its style.
    fn create_text_editor(&mut self, id: BoxId, multiline: bool) {
        if !matches!(self.tree[id].special, Special::TextInput(_)) {
            let text = if multiline {
                String::new()
            } else {
                self.tree[id]
                    .attr(&local_name!("value"))
                    .unwrap_or("")
                    .to_string()
            };
            self.tree[id].special = Special::TextInput(Box::new(ControlText {
                text,
                multiline,
                layout: parley::Layout::new(),
                scroll: 0.0,
            }));
        }
        let layout = control_layout(self.tree, self.fonts, self.layouts, id, None);
        if let Special::TextInput(c) = &mut self.tree[id].special {
            c.layout = layout;
        }
    }

    fn create_checkbox_input(&mut self, id: BoxId) {
        if !matches!(self.tree[id].special, Special::Checkbox(_)) {
            let checked = self.tree[id].has_attr(&local_name!("checked"));
            self.tree[id].special = Special::Checkbox(checked);
        }
    }

    /// The layout children of an inline root (its inline boxes), without building its layout
    /// yet.
    fn find_inline_layout_embedded_boxes(&mut self, root: BoxId, out: &mut Vec<BoxId>) {
        self.flush_inline_pseudos_recursive(root);
        let ids: Vec<BoxId> = self
            .before(root)
            .into_iter()
            .chain(self.tree[root].children.iter().copied())
            .chain(self.after(root))
            .collect();
        for c in ids {
            self.find_embedded_boxes_recursive(root, c, out);
        }
    }

    fn flush_inline_pseudos_recursive(&mut self, id: BoxId) {
        let children = self.tree[id].children.clone();
        for c in children {
            self.flush_pseudo_elements(c);
            // The box whose children are visited decides (as the engine this is ported from
            // does): its own children are flushed, theirs only below inline and contents boxes.
            let display = self.tree[id].display_style().unwrap_or(Display::inline());
            let recurse = matches!(
                (display.outside(), display.inside()),
                (DisplayOutside::None, DisplayInside::Contents)
                    | (DisplayOutside::Inline, DisplayInside::Flow)
            );
            if recurse {
                self.flush_inline_pseudos_recursive(c);
            }
        }
    }

    fn find_embedded_boxes_recursive(&mut self, parent: BoxId, id: BoxId, out: &mut Vec<BoxId>) {
        self.tree[id].layout_parent.set(Some(parent));
        let n = &self.tree[id];
        if !has_element_data(n) {
            return;
        }
        if &*n.tag == "input" && n.attr(&local_name!("type")) == Some("hidden") {
            return;
        }
        let display = n.display_style().unwrap_or(Display::inline());
        match (display.outside(), display.inside()) {
            (DisplayOutside::None, DisplayInside::None) => {}
            (DisplayOutside::None, DisplayInside::Contents) => {
                let children = n.children.clone();
                for c in children {
                    self.find_embedded_boxes_recursive(parent, c, out);
                }
            }
            (DisplayOutside::Inline, DisplayInside::Flow) => {
                if is_inline_box_element(&n.tag) {
                    out.push(id);
                } else if n.tag == local_name!("br") {
                } else {
                    let ids: Vec<BoxId> = self
                        .before(id)
                        .into_iter()
                        .chain(n.children.iter().copied())
                        .chain(self.after(id))
                        .collect();
                    for c in ids {
                        self.find_embedded_boxes_recursive(id, c, out);
                    }
                }
            }
            // An inline box.
            _ => out.push(id),
        }
    }
}

/// The parley style properties a text control lays its text out with.
fn control_styles(style: &ComputedValues, id: BoxId) -> Vec<StyleProperty<'static, TextBrush>> {
    let p = parley_style::style(id, style);
    vec![
        StyleProperty::FontFamily(p.font_family),
        StyleProperty::FontSize(p.font_size),
        StyleProperty::FontWidth(p.font_width),
        StyleProperty::FontStyle(p.font_style),
        StyleProperty::FontWeight(p.font_weight),
        StyleProperty::FontVariations(p.font_variations),
        StyleProperty::FontFeatures(p.font_features),
        StyleProperty::LineHeight(p.line_height),
        StyleProperty::WordSpacing(p.word_spacing),
        StyleProperty::LetterSpacing(p.letter_spacing),
        StyleProperty::WordBreak(p.word_break),
        StyleProperty::OverflowWrap(p.overflow_wrap),
        StyleProperty::TextWrapMode(p.text_wrap_mode),
        StyleProperty::WhiteSpaceCollapse(p.white_space_collapse),
        StyleProperty::Brush(p.brush),
    ]
}

/// A text control's text laid out with its style, its lines broken at `width` (none: not
/// broken), as a plain-text editor lays it out.
pub(crate) fn control_layout(
    tree: &Tree,
    fonts: &mut FontContext,
    layouts: &mut LayoutContext<TextBrush>,
    id: BoxId,
    width: Option<f32>,
) -> parley::Layout<TextBrush> {
    let n = &tree[id];
    let text = match &n.special {
        Special::TextInput(c) => c.text.as_str(),
        _ => "",
    };
    let style = n.primary_styles();
    let alignment = style
        .map(|s| parley_style::text_align(s.clone_text_align()))
        .unwrap_or(parley::Alignment::Start);
    let direction = style
        .map(|s| parley_style::base_direction(s.clone_direction(), s.clone_unicode_bidi()))
        .unwrap_or(parley::BaseDirection::Auto);
    let mut builder = layouts.ranged_builder(fonts, text, 1.0, true);
    builder.set_base_direction(direction);
    if let Some(s) = style {
        for prop in control_styles(s, id) {
            builder.push_default(prop);
        }
    }
    let mut layout = builder.build(text);
    layout.break_all_lines(width);
    layout.align(alignment, parley::AlignmentOptions::default());
    layout
}

/// Builds the Parley layout of the inline root `root`.
pub(crate) fn build_inline_layout_into(
    tree: &Tree,
    layouts: &mut LayoutContext<TextBrush>,
    fonts: &mut FontContext,
    text_layout: &mut TextLayout,
    scale: f32,
    root: BoxId,
) {
    let root_node = &tree[root];
    let root_style = root_node
        .primary_styles()
        .or_else(|| root_node.parent.and_then(|p| tree[p].primary_styles()));
    let parley_style = root_style
        .map(|s| parley_style::style(root, s))
        .unwrap_or_else(|| parley::TextStyle {
            white_space_collapse: WhiteSpaceCollapse::Collapse,
            ..Default::default()
        });

    let mut builder = layouts.tree_builder(fonts, scale, true, &parley_style);
    if let Some(style) = root_style {
        builder.set_base_direction(parley_style::base_direction(
            style.clone_direction(),
            style.clone_unicode_bidi(),
        ));
    }
    let text_transform = root_style
        .map(|s| CaseTransform::from_style(s))
        .unwrap_or(CaseTransform::NONE);

    // An inside list marker.
    if let Some(ListItemLayout {
        marker,
        position: ListItemLayoutPosition::Inside,
    }) = root_node.list_item.as_deref()
    {
        match marker {
            // Bullet glyphs are in the bullet font.
            Marker::Char(char) => {
                let mut marker_style = parley_style.clone();
                marker_style.font_family = BULLET_FONT_FAMILY.into();
                builder.push_style_span(marker_style);
                builder.push_text(&format!("{char} "));
                builder.pop_style_span();
            }
            Marker::String(s) => builder.push_text(s),
        }
    }
    // The marker is a box of its own: words in the content do not continue from it.
    let mut transformer = TextTransformer::default();
    transformer.word_break(&builder);

    let ids = root_node
        .before
        .filter(|_| root_node.pseudos_flushed)
        .into_iter()
        .chain(root_node.children.iter().copied())
        .chain(root_node.after.filter(|_| root_node.pseudos_flushed));
    for c in ids {
        build_inline_layout_recursive(&mut builder, &mut transformer, tree, c, &text_transform);
    }
    text_layout.text = builder.build_into(&mut text_layout.layout);
}

fn inline_box(id: BoxId, kind: InlineBoxKind, style: Option<&ComputedValues>) -> InlineBox {
    InlineBox {
        id: id as u64,
        kind,
        // Set by `push_inline_box`.
        index: 0,
        // Sized by layout.
        width: 0.0,
        height: 0.0,
        baseline: None,
        vertical_align: style.map(parley_style::vertical_align).unwrap_or_default(),
    }
}

fn build_inline_layout_recursive(
    builder: &mut TreeBuilder<'_, TextBrush>,
    transformer: &mut TextTransformer,
    tree: &Tree,
    id: BoxId,
    parent_transform: &CaseTransform,
) {
    let n = &tree[id];
    let style = n.primary_styles();
    let text_transform = style
        .map(|s| CaseTransform::from_style(s))
        .unwrap_or(CaseTransform::NONE);
    match n.kind {
        Kind::Element | Kind::Anonymous | Kind::Pseudo => {
            if &*n.tag == "input" && n.attr(&local_name!("type")) == Some("hidden") {
                return;
            }
            let display = n.display_style().unwrap_or(Display::inline());
            let position = style
                .map(|s| s.clone_position())
                .unwrap_or(PositionProperty::Static);
            let float = style.map(|s| s.clone_float()).unwrap_or(Float::None);
            let box_kind = if position.is_absolutely_positioned() {
                InlineBoxKind::OutOfFlow
            } else if float.is_floating() {
                InlineBoxKind::CustomOutOfFlow
            } else {
                InlineBoxKind::InFlow
            };
            let pseudo = |p: Option<BoxId>| p.filter(|_| n.pseudos_flushed);
            match (display.outside(), display.inside()) {
                (DisplayOutside::None, DisplayInside::None) => {}
                (DisplayOutside::None, DisplayInside::Contents) => {
                    let whitespace = style.map(|s| {
                        [
                            StyleProperty::WhiteSpaceCollapse(parley_style::white_space_collapse(
                                s.clone_white_space_collapse(),
                            )),
                            StyleProperty::TextWrapMode(parley_style::text_wrap_mode(
                                s.clone_text_wrap_mode(),
                            )),
                        ]
                    });
                    builder.push_style_modification_span(
                        whitespace.as_ref().map_or(&[][..], |s| s.as_slice()),
                    );
                    for &c in &n.children {
                        build_inline_layout_recursive(
                            builder,
                            transformer,
                            tree,
                            c,
                            &text_transform,
                        );
                    }
                    builder.pop_style_span();
                }
                (DisplayOutside::Inline, DisplayInside::Flow) => {
                    if is_inline_box_element(&n.tag) {
                        if box_kind == InlineBoxKind::InFlow {
                            transformer.word_break(builder);
                        }
                        builder.push_inline_box(inline_box(id, box_kind, style.map(|s| &**s)));
                    } else if n.tag == local_name!("br") {
                        builder.push_style_modification_span(&[StyleProperty::WhiteSpaceCollapse(
                            WhiteSpaceCollapse::Preserve,
                        )]);
                        builder.push_text("\n");
                        builder.pop_style_span();
                        transformer.word_break(builder);
                    } else {
                        let span = style
                            .map(|s| parley_style::style(id, s))
                            .unwrap_or_else(|| parley::TextStyle {
                                white_space_collapse: WhiteSpaceCollapse::Collapse,
                                ..Default::default()
                            });
                        builder.push_style_span(span);
                        let ids = pseudo(n.before)
                            .into_iter()
                            .chain(n.children.iter().copied())
                            .chain(pseudo(n.after));
                        for c in ids {
                            build_inline_layout_recursive(
                                builder,
                                transformer,
                                tree,
                                c,
                                &text_transform,
                            );
                        }
                        builder.pop_style_span();
                    }
                }
                // An inline box.
                _ => {
                    if box_kind == InlineBoxKind::InFlow {
                        transformer.word_break(builder);
                    }
                    builder.push_inline_box(inline_box(id, box_kind, style.map(|s| &**s)));
                }
            }
        }
        Kind::Text => {
            let text = transformer.transform(&n.text, parent_transform, builder);
            builder.push_text(text);
        }
        Kind::Document => {}
    }
}
