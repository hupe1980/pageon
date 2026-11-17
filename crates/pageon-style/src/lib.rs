//! CSS styling of pageon's document with Stylo, directly on its tree: style data is kept beside
//! the nodes (by node id), so nothing is copied into a second tree.

mod hints;
mod node;

use std::cell::Cell;

use pageon_dom::{Dom, Edge, NodeData, NodeId, StyleChange};
use selectors::matching::ElementSelectorFlags;
use style::context::{
    QuirksMode, RegisteredSpeculativePainter, RegisteredSpeculativePainters, SharedStyleContext,
    StyleContext,
};
use style::device::Device;
use style::device::servo::FontMetricsProvider;
use style::dom::{TElement, TNode};
use style::font_metrics::FontMetrics;
use style::global_style_data::GLOBAL_STYLE_DATA;
use style::invalidation::element::restyle_hints::RestyleHint;
use style::media_queries::{MediaList, MediaType};
use style::properties::ComputedValues;
use style::properties::style_structs::Font;
use style::queries::values::PrefersColorScheme;
use style::selector_parser::SnapshotMap;
use style::servo::media_features::PointerCapabilities;
use style::servo_arc::Arc;
use style::shared_lock::{SharedRwLock, StylesheetGuards};
use style::stylesheets::{AllowImportRules, DocumentStyleSheet, Origin, Stylesheet, UrlExtraData};
use style::stylist::Stylist;
use style::thread_state::ThreadState;
use style::traversal::{DomTraversal, recalc_style_at};
use style::traversal_flags::TraversalFlags;
use style::values::computed::font::QueryFontMetricsFlags;
use style::values::computed::{CSSPixelLength, Length, font::GenericFontFamily};
use style_dom::ElementState;

use node::{Handle, N, Slot, Tree};

/// pageon's user-agent sheet, beside the engine's default one: form controls, the top layer,
/// popovers and other things pageon renders its own way.
pub const UA_CSS: &str = include_str!("ua.css");

/// Font metrics for `ex`, `ch` and friends without fonts: the CSS defaults.
#[derive(Debug)]
struct Metrics;

impl FontMetricsProvider for Metrics {
    fn query_font_metrics(
        &self,
        _vertical: bool,
        _font: &Font,
        _base_size: CSSPixelLength,
        _flags: QueryFontMetricsFlags,
    ) -> FontMetrics {
        FontMetrics::default()
    }

    fn base_size_for_generic(&self, generic: GenericFontFamily) -> Length {
        Length::new(if generic == GenericFontFamily::Monospace {
            13.0
        } else {
            16.0
        })
    }
}

struct NoPainters;

impl RegisteredSpeculativePainters for NoPainters {
    fn get(&self, _name: &style::Atom) -> Option<&dyn RegisteredSpeculativePainter> {
        None
    }
}

/// What styles read that the document does not hold: the top layer (open modal dialogs) and
/// the custom elements whose definitions ran.
#[derive(Debug, Default, Clone, Copy)]
pub struct Page<'a> {
    /// Elements in the top layer.
    pub top_layer: &'a [NodeId],
    /// Defined custom elements (`:defined`).
    pub defined: &'a [NodeId],
    /// Loaded stylesheets of `<link>` elements (for those in shadow trees).
    pub linked: &'a [(NodeId, &'a str)],
}

/// What one restyle did.
#[derive(Debug, Default, Clone, Copy)]
pub struct Restyled {
    /// Elements whose style was computed.
    pub styled: usize,
}

/// Styles a document: the stylist with the user-agent and author sheets, and the style data
/// of every element.
pub struct Styler {
    stylist: Stylist,
    guard: SharedRwLock,
    snapshots: SnapshotMap,
    slots: Vec<Slot>,
    url: UrlExtraData,
    /// The author sheets in cascade order, by the hash of their text.
    authors: Vec<(u64, DocumentStyleSheet)>,
    /// The sheets or the top layer changed: the next sync restyles everything.
    all: bool,
    top_layer: Vec<NodeId>,
    defined: Vec<NodeId>,
    /// Each shadow root's sheets and their cascade data.
    shadows: std::collections::HashMap<NodeId, ShadowStyles>,
}

/// A shadow root's styles: its sheets (`<style>`, loaded `<link>`, adopted) by hash, and their
/// cascade data.
pub(crate) struct ShadowStyles {
    pub(crate) styles: style::author_styles::AuthorStyles<DocumentStyleSheet>,
    hashes: Vec<u64>,
}

/// The lock of all style data: one per process, so parsed sheets are shared between pages.
static GUARD: std::sync::LazyLock<SharedRwLock> = std::sync::LazyLock::new(SharedRwLock::new);

/// Parsed sheets by text, base URL and origin: a page that loads a sheet another page loaded
/// (the same site, a library's CSS) does not parse it again.
fn parsed(css: &str, url: &UrlExtraData, origin: Origin) -> DocumentStyleSheet {
    type Cache = std::collections::HashMap<(u64, usize, u64, bool), DocumentStyleSheet>;
    thread_local! {
        static SHEETS: std::cell::RefCell<(Cache, usize)> = std::cell::RefCell::default();
    }
    let key = (
        hash(css),
        css.len(),
        hash(url.as_str()),
        origin == Origin::Author,
    );
    if let Some(s) = SHEETS.with(|c| c.borrow().0.get(&key).cloned()) {
        return s;
    }
    let sheet = DocumentStyleSheet(Arc::new(Stylesheet::from_str(
        css,
        url.clone(),
        origin,
        Arc::new(GUARD.wrap(MediaList::empty())),
        GUARD.clone(),
        None,
        None,
        QuirksMode::NoQuirks,
        AllowImportRules::Yes,
    )));
    SHEETS.with(|c| {
        let mut c = c.borrow_mut();
        // Past 64 MiB of sheet text the cache starts over.
        if c.1 + css.len() > 64 << 20 {
            c.0.clear();
            c.1 = 0;
        }
        c.1 += css.len();
        c.0.insert(key, sheet.clone());
    });
    sheet
}

impl std::fmt::Debug for Styler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Styler").finish_non_exhaustive()
    }
}

impl Styler {
    /// A styler for a viewport of `width`×`height` CSS px; URLs in sheets resolve against
    /// `base`.
    pub fn new(width: f32, height: f32, base: &str) -> Self {
        Self::with_metrics(width, height, base, Box::new(Metrics))
    }

    /// [`Styler::new`] with the fonts' metrics (`ex`, `ch`, `cap`, `ic` resolve against
    /// them, as layout shapes text).
    pub fn with_metrics(
        width: f32,
        height: f32,
        base: &str,
        metrics: Box<dyn FontMetricsProvider>,
    ) -> Self {
        let device = Device::new(
            MediaType::screen(),
            QuirksMode::NoQuirks,
            euclid::Size2D::new(width, height),
            euclid::Size2D::new(width, height),
            euclid::Scale::new(1.0),
            metrics,
            ComputedValues::initial_values_with_font_override(Font::initial_values()),
            PrefersColorScheme::Light,
            PointerCapabilities::default(),
            PointerCapabilities::default(),
        );
        let url = url::Url::parse(base)
            .or_else(|_| url::Url::parse("about:blank"))
            .map(|u| UrlExtraData(Arc::new(u)))
            .unwrap_or_else(|_| {
                UrlExtraData(Arc::new(
                    url::Url::parse("data:,").unwrap_or_else(|e| panic!("{e}")),
                ))
            });
        let mut s = Self {
            stylist: Stylist::new(device, QuirksMode::NoQuirks),
            guard: GUARD.clone(),
            snapshots: SnapshotMap::new(),
            slots: Vec::new(),
            url,
            authors: Vec::new(),
            all: true,
            top_layer: Vec::new(),
            defined: Vec::new(),
            shadows: std::collections::HashMap::new(),
        };
        for css in [blitz_dom::DEFAULT_CSS, UA_CSS] {
            let sheet = s.sheet(css, Origin::UserAgent);
            s.stylist.append_stylesheet(sheet, &s.guard.read());
        }
        s
    }

    fn sheet(&self, css: &str, origin: Origin) -> DocumentStyleSheet {
        parsed(css, &self.url, origin)
    }

    /// The document's author sheets in cascade order, each with its [`text_hash`]. Unchanged
    /// texts keep their parsed sheet.
    pub fn set_sheets(&mut self, sheets: &[(u64, &str)]) {
        if sheets.len() == self.authors.len()
            && sheets.iter().zip(&self.authors).all(|((h, _), (a, _))| h == a)
        {
            return;
        }
        self.all = true;
        let old = std::mem::take(&mut self.authors);
        for (_, sheet) in old {
            self.stylist.remove_stylesheet(sheet, &self.guard.read());
        }
        for &(h, css) in sheets {
            let sheet = self.sheet(css, Origin::Author);
            self.stylist
                .append_stylesheet(sheet.clone(), &self.guard.read());
            self.authors.push((h, sheet));
        }
    }

    /// Styles every element of `dom` (all of them restyled).
    pub fn restyle_all(&mut self, dom: &Dom) -> Restyled {
        self.restyle_all_with(dom, Page::default())
    }

    /// [`Styler::restyle_all`] with what the page holds outside the document.
    pub fn restyle_all_with(&mut self, dom: &Dom, page: Page<'_>) -> Restyled {
        self.top_layer = page.top_layer.to_vec();
        self.defined = page.defined.to_vec();
        self.sync_shadows(dom, page.linked);
        let elements = self.refresh(dom);
        self.traverse(dom, &elements, &[], &[], true)
    }

    /// Runs Stylo's traversal: over everything (`all`), or down to `elements` (their
    /// snapshots say what to restyle).
    fn traverse(
        &mut self,
        dom: &Dom,
        elements: &[NodeId],
        hinted: &[NodeId],
        inline: &[NodeId],
        all: bool,
    ) -> Restyled {
        // Every node has a slot of its own: a node the log did not name (one inside a nested
        // shadow tree, say) gets a fresh one and is styled.
        let mut elements = elements.to_vec();
        let bound = dom.index_bound();
        if self.slots.len() < bound {
            self.slots.resize_with(bound, Slot::default);
        }
        for i in 0..bound {
            if let Some(id) = dom.node_at(i)
                && self.ensure_slot(id)
            {
                self.refresh_one(dom, id);
                if dom.get(id).is_some_and(|n| n.local_name().is_some()) {
                    elements.push(id);
                }
            }
        }
        let elements = &elements[..];
        // Slotted nodes and their slots, once per restyle.
        let assigned: std::collections::HashMap<NodeId, NodeId> = dom
            .shadow_hosts()
            .flat_map(|(host, _)| dom.children(host).collect::<Vec<_>>())
            .filter_map(|c| dom.assigned_slot(c).map(|s| (c, s)))
            .collect();
        style::thread_state::enter(ThreadState::LAYOUT);
        let guards = StylesheetGuards {
            author: &self.guard.read(),
            ua_or_user: &self.guard.read(),
        };
        let tree = Tree {
            dom,
            slots: &self.slots,
            guard: &self.guard,
            handles: std::cell::OnceCell::new(),
            shadows: &self.shadows,
            assigned: &assigned,
        };
        let handles: Vec<Handle<'_>> = (0..self.slots.len())
            .map(|i| Handle {
                t: &tree,
                id: dom.node_at(i).unwrap_or_else(|| dom.root()),
            })
            .collect();
        let _ = tree.handles.set(&handles);
        let Some(root) = root_element(&tree) else {
            style::thread_state::exit(ThreadState::LAYOUT);
            return Restyled::default();
        };
        self.stylist
            .flush(&guards)
            .process_style(root, Some(&self.snapshots));
        if all {
            if let Some(mut data) = root.mutate_data() {
                data.hint |= RestyleHint::restyle_subtree();
            }
            // Every element is walked.
            for s in &self.slots {
                s.dirty_descendants.set(true);
            }
        } else {
            for &id in elements {
                N(&handles[id.index() as usize]).mark_ancestors_dirty();
            }
            for &id in inline {
                let n = N(&handles[id.index() as usize]);
                if let Some(mut data) = n.mutate_data() {
                    data.hint |= RestyleHint::RESTYLE_STYLE_ATTRIBUTE;
                }
                n.mark_ancestors_dirty();
            }
            // Subtrees restyled in full (siblings that depend on position, changed state).
            for &id in hinted {
                let n = N(&handles[id.index() as usize]);
                if let Some(mut data) = n.mutate_data() {
                    data.hint |= RestyleHint::restyle_subtree();
                }
                n.mark_ancestors_dirty();
            }
        }
        let context = SharedStyleContext {
            traversal_flags: TraversalFlags::empty(),
            stylist: &self.stylist,
            options: GLOBAL_STYLE_DATA.options.clone(),
            guards,
            visited_styles_enabled: false,
            animations: Default::default(),
            current_time_for_animations: 0.0,
            snapshot_map: &self.snapshots,
            registered_speculative_painters: &NoPainters,
        };
        let count = Cell::new(0usize);
        let traversal = Recalc {
            context,
            count: &count,
        };
        let token = Recalc::pre_traverse(root, traversal.shared_context());
        if token.should_traverse() {
            style::driver::traverse_dom(&traversal, token, None);
        }
        style::thread_state::exit(ThreadState::LAYOUT);
        Restyled {
            styled: count.get(),
        }
    }

    /// Brings styles up to date with `changes` (from [`Dom::take_style_changes`]) and the
    /// top layer (open modal dialogs): changed attributes get a snapshot of their values
    /// before, so Stylo restyles only what they can affect; inserted elements are styled;
    /// siblings are restyled when the parent's selectors depend on position; state changes
    /// restyle their parent's subtree. Sheet changes and [`StyleChange::All`] restyle
    /// everything.
    pub fn sync(&mut self, dom: &Dom, changes: &[StyleChange], page: Page<'_>) -> Restyled {
        let shadows_changed = self.sync_shadows(dom, page.linked);
        if self.all
            || shadows_changed
            || self.top_layer != page.top_layer
            || changes.iter().any(|c| matches!(c, StyleChange::All))
        {
            self.all = false;
            return self.restyle_all_with(dom, page);
        }
        // Custom elements defined since the last sync change state like any other.
        let newly: Vec<StyleChange> = page
            .defined
            .iter()
            .filter(|d| !self.defined.contains(d))
            .chain(self.defined.iter().filter(|d| !page.defined.contains(d)))
            .map(|&element| StyleChange::State { element })
            .collect();
        self.defined = page.defined.to_vec();
        let changes: Vec<StyleChange> = changes.iter().cloned().chain(newly).collect();
        let changes = &changes[..];
        let mut snapped: Vec<NodeId> = Vec::new();
        let mut fresh: Vec<NodeId> = Vec::new();
        let mut hinted: Vec<NodeId> = Vec::new();
        // Elements whose inline style changed: their declarations, not their matching.
        let mut inline: Vec<NodeId> = Vec::new();
        for c in changes {
            match c {
                StyleChange::Attribute { element, name, .. } => {
                    if !snapped.contains(element) && dom.get(*element).is_some() {
                        snapped.push(*element);
                    }
                    if name == "style" && !inline.contains(element) {
                        inline.push(*element);
                    }
                }
                StyleChange::Children { parent } => {
                    if dom.get(*parent).is_none() {
                        continue;
                    }
                    for child in dom.children(*parent) {
                        // Moved here from elsewhere (or styled under this parent before it
                        // changed): restyled with what it inherits now.
                        let parent_now = self
                            .slots
                            .get(parent.index() as usize)
                            .filter(|s| s.has_data.get())
                            .and_then(|s| {
                                s.data.borrow().styles.get_primary().map(|p| std::ptr::from_ref::<ComputedValues>(p) as usize)
                            })
                            .unwrap_or(0);
                        if let Some(s) = self.slots.get(child.index() as usize)
                            && s.generation.get() == Some(child.generation())
                            && s.has_data.get()
                            && (s.styled_under.get() != Some(*parent)
                                || s.inherited_from.get() != parent_now)
                            && !hinted.contains(&child)
                        {
                            hinted.push(child);
                        }
                        for e in dom.traverse(child) {
                            let Edge::Enter(id) = e else { continue };
                            // New here, or never styled (it was in a subtree not rendered).
                            let new = self.ensure_slot(id);
                            if new || !self.slots[id.index() as usize].has_data.get() {
                                self.refresh_one(dom, id);
                                if dom.get(id).is_some_and(|n| n.local_name().is_some()) {
                                    fresh.push(id);
                                }
                            }
                        }
                    }
                    // A select shows its first enabled option when none is chosen: the order
                    // of its options decides which.
                    self.select_changed(dom, *parent, &mut hinted);
                    let flags = self
                        .slots
                        .get(parent.index() as usize)
                        .map_or(ElementSelectorFlags::empty(), |s| s.selector_flags.get());
                    if flags.intersects(
                        ElementSelectorFlags::HAS_SLOW_SELECTOR
                            | ElementSelectorFlags::HAS_SLOW_SELECTOR_LATER_SIBLINGS
                            | ElementSelectorFlags::HAS_EDGE_CHILD_SELECTOR
                            | ElementSelectorFlags::HAS_EMPTY_SELECTOR,
                    ) && !hinted.contains(parent)
                    {
                        hinted.push(*parent);
                    }
                }
                StyleChange::State { element } => {
                    if dom.get(*element).is_none() {
                        continue;
                    }
                    self.ensure_slot(*element);
                    self.refresh_one(dom, *element);
                    // A select's options show its choice: their markers follow it.
                    self.select_changed(dom, *element, &mut hinted);
                    let at = dom.parent(*element).unwrap_or(*element);
                    if !hinted.contains(&at) {
                        hinted.push(at);
                    }
                }
                // Sheets are compared by text in `set_sheets`.
                StyleChange::Sheets | StyleChange::All => {}
            }
        }
        self.snapshot(dom, changes, &snapped);
        let r = self.traverse(
            dom,
            &[&snapped[..], &fresh[..]].concat(),
            &hinted,
            &inline,
            false,
        );
        for &id in &snapped {
            if let Some(s) = self.slots.get(id.index() as usize) {
                s.has_snapshot.set(false);
            }
        }
        self.snapshots.clear();
        r
    }

    /// When `at` is or is inside a `<select>`: its options' markers refreshed and the select
    /// restyled (which option shows depends on the choice and on their order).
    fn select_changed(&mut self, dom: &Dom, at: NodeId, hinted: &mut Vec<NodeId>) {
        let select = std::iter::once(at)
            .chain(dom.ancestors(at))
            .find(|&a| dom.get(a).is_some_and(|n| n.is_html("select")));
        let Some(sel) = select else { return };
        for e in dom.traverse(sel) {
            if let Edge::Enter(id) = e {
                self.ensure_slot(id);
                self.refresh_one(dom, id);
            }
        }
        if !hinted.contains(&sel) {
            hinted.push(sel);
        }
    }

    /// The slot of `id`, made fresh if it belonged to another node (or none). Whether it was.
    fn ensure_slot(&mut self, id: NodeId) -> bool {
        let i = id.index() as usize;
        if self.slots.len() <= i {
            self.slots.resize_with(i + 1, Slot::default);
        }
        let slot = &mut self.slots[i];
        if slot.generation.get() == Some(id.generation()) {
            return false;
        }
        *slot = Slot::default();
        slot.generation.set(Some(id.generation()));
        true
    }

    /// Snapshots of `elements`' attributes before `changes` (the earliest old value of each
    /// name), for Stylo's invalidation; their matching data refreshed.
    fn snapshot(&mut self, dom: &Dom, changes: &[StyleChange], elements: &[NodeId]) {
        use style::attr::AttrValue;
        use style::selector_parser::ServoElementSnapshot;
        for &id in elements {
            let Some(node) = dom.get(id) else { continue };
            let mut attrs: Vec<(String, Option<String>)> = node
                .attrs()
                .iter()
                .map(|a| (a.qual_name().local.to_string(), Some(a.value.clone())))
                .collect();
            let mut snap = ServoElementSnapshot::new();
            let mut seen: Vec<&str> = Vec::new();
            for c in changes {
                let StyleChange::Attribute { element, name, old } = c else {
                    continue;
                };
                if *element != id || seen.contains(&name.as_str()) {
                    continue;
                }
                seen.push(name);
                match attrs.iter_mut().find(|(n, _)| n == name) {
                    Some(a) => a.1.clone_from(old),
                    None => attrs.push((name.clone(), old.clone())),
                }
                match name.as_str() {
                    "class" => snap.class_changed = true,
                    "id" => snap.id_changed = true,
                    _ => snap.other_attributes_changed = true,
                }
                snap.changed_attrs
                    .push(style::LocalName::from(name.as_str()));
            }
            snap.attrs = Some(
                attrs
                    .into_iter()
                    .filter_map(|(n, v)| {
                        let v = v?;
                        let value = match n.as_str() {
                            "id" => AttrValue::from_atomic(v),
                            "class" => AttrValue::from_serialized_tokenlist(v),
                            _ => AttrValue::String(v),
                        };
                        let local = style::LocalName::from(n.as_str());
                        Some((
                            style::attr::AttrIdentifier {
                                local_name: local.clone(),
                                name: local,
                                namespace: style::Namespace::default(),
                                prefix: None,
                            },
                            value,
                        ))
                    })
                    .collect(),
            );
            let key = (u64::from(id.generation()) << 32) | u64::from(id.index());
            self.snapshots
                .insert(style::dom::OpaqueNode(key as usize), snap);
            self.ensure_slot(id);
            self.refresh_one(dom, id);
            let slot = &self.slots[id.index() as usize];
            slot.has_snapshot.set(true);
            slot.handled_snapshot.set(false);
        }
    }

    /// An element's styles as computed (primary and pseudo-elements), for a layout engine to
    /// lay out with; elements in subtrees not rendered may hold styles from before.
    pub fn element_styles(&self, dom: &Dom, id: NodeId) -> Option<style::data::ElementStyles> {
        let slot = self.slots.get(id.index() as usize)?;
        if slot.generation.get() != Some(id.generation()) || !slot.has_data.get() {
            return None;
        }
        dom.get(id)?;
        Some(slot.data.borrow().styles.clone())
    }

    /// The style of an anonymous box (a block wrapping inline content) inside `parent`.
    pub fn anonymous_style(&self, parent: &ComputedValues) -> Arc<ComputedValues> {
        let guard = self.guard.read();
        self.stylist.style_for_anonymous::<N<'_>>(
            &StylesheetGuards::same(&guard),
            &style::selector_parser::PseudoElement::ServoAnonymousBox,
            parent,
        )
    }

    /// The computed style of an element, once styled.
    pub fn style(&self, dom: &Dom, id: NodeId) -> Option<Arc<ComputedValues>> {
        let slot = self.slots.get(id.index() as usize)?;
        if slot.generation.get() != Some(id.generation()) || !slot.has_data.get() {
            return None;
        }
        dom.get(id)?;
        // Outside the flat tree (an unassigned child of a host) or inside an undisplayed
        // subtree nothing is styled: data from before is stale.
        let mut at = id;
        while at != dom.root() {
            let p = dom.flat_parent(at)?;
            if matches!(dom.get(p).map(|n| &n.data), Some(NodeData::Element { .. })) {
                let s = self.slots.get(p.index() as usize)?;
                if !s.has_data.get() {
                    return None;
                }
                let shown = s
                    .data
                    .borrow()
                    .styles
                    .get_primary()
                    .is_some_and(|p| !p.clone_display().is_none());
                if !shown {
                    return None;
                }
            }
            at = p;
        }
        slot.data.borrow().styles.get_primary().cloned()
    }

    /// The computed value of a longhand or custom property (`display`, `--accent`), serialized;
    /// `None` for shorthands (their value is built from layout's resolved values).
    pub fn value(&self, dom: &Dom, id: NodeId, property: &str) -> Option<String> {
        let style = self.style(dom, id)?;
        let p = style::properties::PropertyId::parse_enabled_for_all_content(property).ok()?;
        match p.as_shorthand() {
            Ok(_) => None,
            Err(declaration) => Some(style.computed_value_to_string(declaration)),
        }
    }

    /// Refreshes what matching reads of one element (after its attributes changed).
    fn refresh_one(&mut self, dom: &Dom, id: NodeId) {
        let Some(node) = dom.get(id) else { return };
        let NodeData::Element { name, .. } = &node.data else {
            return;
        };
        let local = name.local.clone();
        let style_attribute = node.attr("style").map(|css| {
            Arc::new(self.guard.wrap(style::properties::parse_style_attribute(
                css,
                &self.url,
                None,
                QuirksMode::NoQuirks,
                style::stylesheets::CssRuleType::Style,
            )))
        });
        let Some(slot) = self.slots.get_mut(id.index() as usize) else {
            return;
        };
        slot.id = node
            .attr("id")
            .filter(|v| !v.is_empty())
            .map(style::Atom::from);
        slot.classes = node
            .attr("class")
            .map(|c| c.split_ascii_whitespace().map(style::Atom::from).collect())
            .unwrap_or_default();
        slot.style_attribute = style_attribute;
        slot.state = state(dom, id, &local);
        let shown = &*local == "option"
            && dom
                .ancestors(id)
                .find(|&a| dom.get(a).is_some_and(|n| n.is_html("select")))
                .is_some_and(|sel| shown_options(dom, sel).contains(&id));
        slot.markers = node::Markers {
            shown,
            popover_open: dom.popover_open(id),
            scripting: &*local == "noscript" && dom.scripting(),
            top_layer: self.top_layer.contains(&id),
            defined: self.defined.contains(&id),
        };
    }

    /// Every element's matching data refreshed (the document's, its shadow trees' and any
    /// other live element's); the elements.
    fn refresh(&mut self, dom: &Dom) -> Vec<NodeId> {
        let mut elements = Vec::new();
        let bound = dom.index_bound();
        if self.slots.len() < bound {
            self.slots.resize_with(bound, Slot::default);
        }
        for i in 0..bound {
            let Some(id) = dom.node_at(i) else { continue };
            self.ensure_slot(id);
            if dom.get(id).is_some_and(|n| n.local_name().is_some()) {
                self.refresh_one(dom, id);
                elements.push(id);
            }
        }
        elements
    }

    /// Brings each shadow root's styles up to date (its `<style>` and loaded `<link>`
    /// sheets, then its adopted ones). Whether any changed.
    fn sync_shadows(&mut self, dom: &Dom, linked: &[(NodeId, &str)]) -> bool {
        let mut changed = false;
        let roots: Vec<NodeId> = dom.shadow_hosts().map(|(_, r)| r).collect();
        let before = self.shadows.len();
        self.shadows.retain(|r, _| roots.contains(r));
        changed |= self.shadows.len() != before;
        for root in roots {
            let mut texts: Vec<String> = Vec::new();
            for e in dom.traverse(root) {
                let Edge::Enter(id) = e else { continue };
                let Some(n) = dom.get(id) else { continue };
                if n.is_html("style") {
                    texts.push(dom.cssom_text(id).map_or_else(
                        || {
                            dom.children(id)
                                .filter_map(|c| match dom.get(c).map(|n| &n.data) {
                                    Some(NodeData::Text(t)) => Some(t.as_str()),
                                    _ => None,
                                })
                                .collect()
                        },
                        str::to_string,
                    ));
                } else if n.is_html("link")
                    && let Some(css) = dom
                        .cssom_text(id)
                        .or_else(|| linked.iter().find(|(l, _)| *l == id).map(|(_, c)| *c))
                {
                    texts.push(css.to_string());
                }
            }
            texts.extend(dom.adopted(root).iter().cloned());
            let hashes: Vec<u64> = texts.iter().map(|t| hash(t)).collect();
            if self.shadows.get(&root).is_some_and(|s| s.hashes == hashes) {
                continue;
            }
            changed = true;
            let mut styles = style::author_styles::AuthorStyles::new();
            {
                let guard = self.guard.read();
                let media = style::stylesheets::CustomMediaMap::default();
                for t in &texts {
                    let sheet = parsed(t, &self.url, Origin::Author);
                    styles
                        .stylesheets
                        .append_stylesheet(None, &media, sheet, &guard);
                }
                styles.flush(&mut self.stylist, &guard);
            }
            self.shadows.insert(root, ShadowStyles { styles, hashes });
        }
        changed
    }
}

/// The element states pages match on that the document knows (links, enabled and disabled
/// controls, checked boxes).
fn state(dom: &Dom, id: NodeId, local: &str) -> ElementState {
    let Some(node) = dom.get(id) else {
        return ElementState::empty();
    };
    let mut s = ElementState::empty();
    if matches!(local, "a" | "area") && node.attr("href").is_some() {
        s |= ElementState::UNVISITED;
    }
    if matches!(
        local,
        "input" | "button" | "select" | "textarea" | "option" | "optgroup" | "fieldset"
    ) {
        if node.attr("disabled").is_some() {
            s |= ElementState::DISABLED;
        } else {
            s |= ElementState::ENABLED;
        }
    }
    let checked = dom
        .control(id)
        .and_then(|c| c.checked)
        .unwrap_or_else(|| local == "input" && node.attr("checked").is_some());
    if checked {
        s |= ElementState::CHECKED;
    }
    s
}

/// The `<option>`s a `<select>` shows as chosen: its selected ones; for a drop-down with none,
/// the first enabled option (as browsers show it).
fn shown_options(dom: &Dom, select: NodeId) -> Vec<NodeId> {
    let options: Vec<NodeId> = dom
        .traverse(select)
        .filter_map(|e| match e {
            Edge::Enter(id) if dom.get(id).is_some_and(|n| n.is_html("option")) => Some(id),
            _ => None,
        })
        .collect();
    let chosen: Vec<NodeId> = options
        .iter()
        .copied()
        .filter(|&o| {
            dom.control(o)
                .and_then(|c| c.selected)
                .unwrap_or_else(|| dom.get(o).is_some_and(|n| n.attr("selected").is_some()))
        })
        .collect();
    if dom
        .get(select)
        .is_some_and(|n| n.attr("multiple").is_some())
    {
        return chosen;
    }
    if let Some(&last) = chosen.last() {
        return vec![last];
    }
    options
        .into_iter()
        .find(|&o| dom.get(o).is_some_and(|n| n.attr("disabled").is_none()))
        .into_iter()
        .collect()
}

/// Which style an element inherited (its flat-tree parent's, by address; 0: none).
fn parent_style(el: N<'_>) -> usize {
    TElement::traversal_parent(&el)
        .and_then(|p| {
            p.borrow_data()
                .and_then(|d| d.styles.get_primary().map(|s| std::ptr::from_ref::<ComputedValues>(s) as usize))
        })
        .unwrap_or(0)
}

fn root_element<'a>(tree: &'a Tree<'a>) -> Option<N<'a>> {
    let handles = tree.handles.get()?;
    let doc = N(&handles[tree.dom.root().index() as usize]);
    let mut at = doc.first_child();
    while let Some(n) = at {
        if n.as_element().is_some() {
            return n.as_element();
        }
        at = n.next_sibling();
    }
    None
}

/// A fast hash of a text (a word at a time, as FxHash), for telling sheets apart. Not keyed:
/// texts are the page's own.
pub fn text_hash(s: &str) -> u64 {
    const K: u64 = 0x517c_c1b7_2722_0a95;
    let b = s.as_bytes();
    let mut h: u64 = b.len() as u64;
    let (words, rest) = b.as_chunks::<8>();
    for w in words {
        h = (h.rotate_left(5) ^ u64::from_le_bytes(*w)).wrapping_mul(K);
    }
    for &x in rest {
        h = (h.rotate_left(5) ^ u64::from(x)).wrapping_mul(K);
    }
    h
}

fn hash(s: &str) -> u64 {
    text_hash(s)
}

struct Recalc<'a> {
    context: SharedStyleContext<'a>,
    count: &'a Cell<usize>,
}

// Stylo's traversal is driven through its trait; data is reached through the slots' safe API.
impl<'dom> DomTraversal<N<'dom>> for Recalc<'_> {
    fn process_preorder<F: FnMut(N<'dom>)>(
        &self,
        context: &mut StyleContext<N<'dom>>,
        node: N<'dom>,
        note_child: F,
    ) {
        if let Some(el) = node.as_element() {
            #[allow(unsafe_code)]
            // SAFETY: the traversal is sequential and owns the tree's style data while it runs.
            let mut data = unsafe { el.ensure_data() };
            recalc_style_at(self, context, el, &mut data, note_child);
            el.slot().styled_under.set(el.t.dom.parent(el.id));
            el.slot().inherited_from.set(parent_style(el));
            self.count.set(self.count.get() + 1);
            #[allow(unsafe_code)]
            // SAFETY: as above.
            unsafe {
                el.unset_dirty_descendants();
            }
        }
    }

    fn needs_postorder_traversal() -> bool {
        false
    }

    fn process_postorder(&self, _context: &mut StyleContext<N<'dom>>, _node: N<'dom>) {}

    fn shared_context(&self) -> &SharedStyleContext<'_> {
        &self.context
    }
}

// The traversal runs on one thread: the slots' cells are never shared across threads.
#[allow(unsafe_code)]
unsafe impl Sync for Recalc<'_> {}
