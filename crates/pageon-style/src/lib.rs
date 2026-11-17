//! CSS styling of pageon's document with Stylo, directly on its tree: style data is kept beside
//! the nodes (by node id), so nothing is copied into a second tree.

mod hints;
mod node;

use std::cell::Cell;

use pageon_dom::{Dom, Edge, NodeData, NodeId};
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

const PAGEON_UA: &str = include_str!("../../pageon-layout/src/ua.css");

/// Font metrics for `ex`, `ch` and friends: the CSS defaults (a later stage measures the
/// bundled fonts).
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

/// An attribute that changed since the last restyle, with its value before.
#[derive(Debug, Clone)]
pub struct AttrChange {
    /// The element.
    pub element: NodeId,
    /// The attribute's local name.
    pub name: String,
    /// Its value before (`None`: it was absent).
    pub old: Option<String>,
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
}

impl Styler {
    /// A styler for a viewport of `width`×`height` CSS px; URLs in sheets resolve against
    /// `base`.
    pub fn new(width: f32, height: f32, base: &str) -> Self {
        let device = Device::new(
            MediaType::screen(),
            QuirksMode::NoQuirks,
            euclid::Size2D::new(width, height),
            euclid::Size2D::new(width, height),
            euclid::Scale::new(1.0),
            Box::new(Metrics),
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
            guard: SharedRwLock::new(),
            snapshots: SnapshotMap::new(),
            slots: Vec::new(),
            url,
            authors: Vec::new(),
        };
        for css in [blitz_dom::DEFAULT_CSS, PAGEON_UA] {
            let sheet = s.sheet(css, Origin::UserAgent);
            s.stylist.append_stylesheet(sheet, &s.guard.read());
        }
        s
    }

    fn sheet(&self, css: &str, origin: Origin) -> DocumentStyleSheet {
        DocumentStyleSheet(Arc::new(Stylesheet::from_str(
            css,
            self.url.clone(),
            origin,
            Arc::new(self.guard.wrap(MediaList::empty())),
            self.guard.clone(),
            None,
            None,
            QuirksMode::NoQuirks,
            AllowImportRules::Yes,
        )))
    }

    /// The document's author sheets, in cascade order. Unchanged texts keep their parsed sheet.
    pub fn set_sheets(&mut self, sheets: &[&str]) {
        let hashes: Vec<u64> = sheets.iter().map(|s| hash(s)).collect();
        if hashes.len() == self.authors.len()
            && hashes.iter().zip(&self.authors).all(|(h, (a, _))| h == a)
        {
            return;
        }
        let old = std::mem::take(&mut self.authors);
        for (_, sheet) in old {
            self.stylist.remove_stylesheet(sheet, &self.guard.read());
        }
        for (css, h) in sheets.iter().zip(hashes) {
            let sheet = self.sheet(css, Origin::Author);
            self.stylist
                .append_stylesheet(sheet.clone(), &self.guard.read());
            self.authors.push((h, sheet));
        }
    }

    /// Styles every element of `dom` (all of them restyled).
    pub fn restyle_all(&mut self, dom: &Dom) -> Restyled {
        self.restyle_all_with(dom, &[])
    }

    /// [`Styler::restyle_all`] with the elements in the top layer (open modal dialogs).
    pub fn restyle_all_with(&mut self, dom: &Dom, top_layer: &[NodeId]) -> Restyled {
        let elements = self.refresh(dom, top_layer);
        self.traverse(dom, &elements, true)
    }

    /// Runs Stylo's traversal: over everything (`all`), or down to `elements` (their
    /// snapshots say what to restyle).
    fn traverse(&mut self, dom: &Dom, elements: &[NodeId], all: bool) -> Restyled {
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
        };
        let handles: Vec<Handle<'_>> = (0..self.slots.len())
            .map(|i| Handle {
                t: &tree,
                id: self.slots[i]
                    .generation
                    .get()
                    .map_or_else(|| dom.root(), |g| NodeId::from_parts(i as u32, g)),
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
            // Every element is walked: mark each element's parent.
            for &id in elements {
                if let Some(p) = dom.parent(id) {
                    self.slots[p.index() as usize].dirty_descendants.set(true);
                }
            }
        } else {
            for &id in elements {
                N(&handles[id.index() as usize]).mark_ancestors_dirty();
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

    /// Restyles after attribute changes: each changed element gets a snapshot of its
    /// attributes before, and Stylo restyles only what the change can affect.
    pub fn restyle(&mut self, dom: &Dom, changes: &[AttrChange]) -> Restyled {
        use style::attr::AttrValue;
        use style::selector_parser::ServoElementSnapshot;
        let mut changed: Vec<NodeId> = Vec::new();
        for c in changes {
            if !changed.contains(&c.element) {
                changed.push(c.element);
            }
        }
        for &id in &changed {
            let Some(node) = dom.get(id) else { continue };
            // The attributes as they were: the current ones with each change undone (the
            // earliest old value of a name wins).
            let mut attrs: Vec<(String, Option<String>)> = node
                .attrs()
                .iter()
                .map(|a| (a.qual_name().local.to_string(), Some(a.value.clone())))
                .collect();
            let mut seen: Vec<&str> = Vec::new();
            for c in changes.iter().filter(|c| c.element == id) {
                if seen.contains(&c.name.as_str()) {
                    // A later change of the same name: the earliest old value is kept.
                    continue;
                }
                seen.push(&c.name);
                match attrs.iter_mut().find(|(n, _)| *n == c.name) {
                    Some(a) => a.1 = c.old.clone(),
                    None => attrs.push((c.name.clone(), c.old.clone())),
                }
            }
            let mut snap = ServoElementSnapshot::new();
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
            for c in changes.iter().filter(|c| c.element == id) {
                match c.name.as_str() {
                    "class" => snap.class_changed = true,
                    "id" => snap.id_changed = true,
                    _ => snap.other_attributes_changed = true,
                }
                let name = style::LocalName::from(c.name.as_str());
                if !snap.changed_attrs.contains(&name) {
                    snap.changed_attrs.push(name);
                }
            }
            let key = (u64::from(id.generation()) << 32) | u64::from(id.index());
            self.snapshots
                .insert(style::dom::OpaqueNode(key as usize), snap);
            self.refresh_one(dom, id);
            let slot = &self.slots[id.index() as usize];
            slot.has_snapshot.set(true);
            slot.handled_snapshot.set(false);
        }
        let r = self.traverse(dom, &changed, false);
        for &id in &changed {
            if let Some(s) = self.slots.get(id.index() as usize) {
                s.has_snapshot.set(false);
            }
        }
        self.snapshots.clear();
        r
    }

    /// The computed style of an element, once styled.
    pub fn style(&self, dom: &Dom, id: NodeId) -> Option<Arc<ComputedValues>> {
        let slot = self.slots.get(id.index() as usize)?;
        if slot.generation.get() != Some(id.generation()) || !slot.has_data.get() {
            return None;
        }
        dom.get(id)?;
        slot.data.borrow().styles.get_primary().cloned()
    }

    /// The computed value of a longhand property, serialized (`display`, `color`, …).
    pub fn value(&self, dom: &Dom, id: NodeId, property: &str) -> Option<String> {
        let style = self.style(dom, id)?;
        let p = style::properties::PropertyId::parse_enabled_for_all_content(property).ok()?;
        let longhand = p.longhand_id()?;
        Some(
            style.computed_value_to_string(style::properties::PropertyDeclarationId::Longhand(
                longhand,
            )),
        )
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
    }

    /// Sizes the slots to the document and refreshes what each element's matching reads
    /// (id, classes, inline style, state). The elements, in tree order.
    fn refresh(&mut self, dom: &Dom, top_layer: &[NodeId]) -> Vec<NodeId> {
        let mut elements = Vec::new();
        let mut shown: Vec<NodeId> = Vec::new();
        for e in dom.traverse(dom.root()) {
            let Edge::Enter(id) = e else { continue };
            let i = id.index() as usize;
            if self.slots.len() <= i {
                self.slots.resize_with(i + 1, Slot::default);
            }
            let slot = &mut self.slots[i];
            if slot.generation.get() != Some(id.generation()) {
                *slot = Slot::default();
                slot.generation.set(Some(id.generation()));
            }
            let Some(node) = dom.get(id) else { continue };
            let NodeData::Element { name, .. } = &node.data else {
                continue;
            };
            elements.push(id);
            slot.id = node
                .attr("id")
                .filter(|v| !v.is_empty())
                .map(style::Atom::from);
            slot.classes = node
                .attr("class")
                .map(|c| c.split_ascii_whitespace().map(style::Atom::from).collect())
                .unwrap_or_default();
            slot.style_attribute = node.attr("style").map(|css| {
                Arc::new(self.guard.wrap(style::properties::parse_style_attribute(
                    css,
                    &self.url,
                    None,
                    QuirksMode::NoQuirks,
                    style::stylesheets::CssRuleType::Style,
                )))
            });
            slot.state = state(dom, id, &name.local);
            if &*name.local == "select" {
                shown.extend(shown_options(dom, id));
            }
            slot.markers = node::Markers {
                shown: &*name.local == "option" && shown.contains(&id),
                popover_open: dom.popover_open(id),
                scripting: &*name.local == "noscript" && dom.scripting(),
                top_layer: top_layer.contains(&id),
            };
        }
        elements
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

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
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
