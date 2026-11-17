//! Stylo's view of pageon's document: a node handle over the tree and the style data kept
//! beside it. Adapted from Blitz's binding (blitz-dom `stylo.rs`, MIT OR Apache-2.0).

use std::cell::Cell;
use std::ptr::NonNull;

use markup5ever::{LocalName, LocalNameStaticSet, Namespace, NamespaceStaticSet, local_name, ns};
use pageon_dom::{Dom, NodeData, NodeId};
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::{BLOOM_HASH_MASK, BloomFilter};
use selectors::matching::{ElementSelectorFlags, MatchingContext, VisitedHandlingMode};
use selectors::sink::Push;
use selectors::{Element, OpaqueElement};
use style::Atom;
use style::CaseSensitivityExt;
use style::applicable_declarations::ApplicableDeclarationBlock;
use style::bloom::each_relevant_element_hash;
use style::context::SharedStyleContext;
use style::data::{ElementData, ElementDataMut, ElementDataRef, ElementDataWrapper};
use style::dom::{LayoutIterator, NodeInfo, OpaqueNode, TDocument, TElement, TNode, TShadowRoot};
use style::properties::{
    ComputedValues, Importance, PropertyDeclaration, PropertyDeclarationBlock,
};
use style::rule_tree::{CascadeLevel, CascadeOrigin};
use style::selector_parser::{NonTSPseudoClass, PseudoElement, RestyleDamage, SelectorImpl};
use style::servo_arc::{Arc, ArcBorrow};
use style::shared_lock::{Locked, SharedRwLock};
use style::stylesheets::layer_rule::LayerOrder;
use style::stylesheets::scope_rule::ImplicitScopeRoot;
use style::values::{AtomIdent, AtomString, GenericAtomIdent};
use style_dom::ElementState;

/// The name of a node that is not an element (Stylo asks only elements).
static NONE: std::sync::LazyLock<(LocalName, Namespace)> =
    std::sync::LazyLock::new(|| (local_name!(""), ns!()));

/// What styling keeps for one node, beside the tree.
pub(crate) struct Slot {
    /// The generation of the node this slot belongs to.
    pub(crate) generation: Cell<Option<u32>>,
    pub(crate) data: ElementDataWrapper,
    pub(crate) has_data: Cell<bool>,
    pub(crate) dirty_descendants: Cell<bool>,
    pub(crate) has_snapshot: Cell<bool>,
    pub(crate) handled_snapshot: Cell<bool>,
    pub(crate) selector_flags: Cell<ElementSelectorFlags>,
    /// The `id` and class atoms, interned once per change of the attribute.
    pub(crate) id: Option<Atom>,
    pub(crate) classes: Vec<Atom>,
    pub(crate) style_attribute: Option<Arc<Locked<PropertyDeclarationBlock>>>,
    pub(crate) state: ElementState,
    /// The attributes pageon's user-agent sheet matches that the document holds as state
    /// (an `<option>` shown, an open popover, `<noscript>` with scripting on, the top layer).
    pub(crate) markers: Markers,
    /// The parent the element was last styled under (a moved element is styled again).
    pub(crate) styled_under: Cell<Option<NodeId>>,
}

/// State pageon's user-agent sheet matches as attributes.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Markers {
    pub(crate) shown: bool,
    pub(crate) popover_open: bool,
    pub(crate) scripting: bool,
    pub(crate) top_layer: bool,
    /// A custom element whose definition ran (`:defined`).
    pub(crate) defined: bool,
}

impl Markers {
    fn names(self) -> impl Iterator<Item = &'static str> {
        [
            (self.shown, "data-pageon-shown"),
            (self.popover_open, "pageon-popover-open"),
            (self.scripting, "pageon-scripting"),
            (self.top_layer, "data-pageon-top-layer"),
        ]
        .into_iter()
        .filter_map(|(on, n)| on.then_some(n))
    }

    fn has(&self, name: &str) -> bool {
        match name {
            "data-pageon-shown" => self.shown,
            "pageon-popover-open" => self.popover_open,
            "pageon-scripting" => self.scripting,
            "data-pageon-top-layer" => self.top_layer,
            _ => false,
        }
    }
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            generation: Cell::new(None),
            data: ElementDataWrapper::default(),
            has_data: Cell::new(false),
            dirty_descendants: Cell::new(false),
            has_snapshot: Cell::new(false),
            handled_snapshot: Cell::new(false),
            selector_flags: Cell::new(ElementSelectorFlags::empty()),
            id: None,
            classes: Vec::new(),
            style_attribute: None,
            state: ElementState::empty(),
            markers: Markers::default(),
            styled_under: Cell::new(None),
        }
    }
}

/// The tree as styling sees it: the document, the slots, the lock of the style data, and a
/// handle per node (Stylo's element type must be one pointer wide).
pub(crate) struct Tree<'a> {
    pub(crate) dom: &'a Dom,
    pub(crate) slots: &'a [Slot],
    pub(crate) guard: &'a SharedRwLock,
    pub(crate) handles: std::cell::OnceCell<&'a [Handle<'a>]>,
}

/// One node's handle: its tree and id.
pub(crate) struct Handle<'a> {
    pub(crate) t: &'a Tree<'a>,
    pub(crate) id: NodeId,
}

/// A node of the document, for Stylo.
#[derive(Clone, Copy)]
pub(crate) struct N<'a>(pub(crate) &'a Handle<'a>);

impl<'a> std::ops::Deref for N<'a> {
    type Target = Handle<'a>;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl std::fmt::Debug for N<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.id)
    }
}

impl PartialEq for N<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for N<'_> {}

impl std::hash::Hash for N<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl<'a> N<'a> {
    pub(crate) fn with(&self, id: NodeId) -> Self {
        N(&self.0.t.handles.get().map_or(&[][..], |h| h)[id.index() as usize])
    }

    fn key(&self) -> u64 {
        (u64::from(self.id.generation()) << 32) | u64::from(self.id.index())
    }

    pub(crate) fn slot(&self) -> &'a Slot {
        &self.t.slots[self.id.index() as usize]
    }

    fn data(&self) -> Option<&'a NodeData> {
        self.t.dom.get(self.id).map(|n| &n.data)
    }

    fn element(&self) -> Option<(&'a markup5ever::QualName, &'a [pageon_dom::Attribute])> {
        match self.data()? {
            NodeData::Element { name, attrs, .. } => Some((name, attrs)),
            _ => None,
        }
    }

    fn attr(&self, local: &str) -> Option<&'a str> {
        self.element()?
            .1
            .iter()
            .find(|a| &*a.qual_name().local == local && a.qual_name().ns == ns!())
            .map(|a| a.value.as_str())
    }

    fn is_tag(&self, tag: &LocalName) -> bool {
        self.element()
            .is_some_and(|(n, _)| n.local == *tag && n.ns == ns!(html))
    }

    fn element_siblings(&self, forward: bool) -> Option<Self> {
        let dom = self.t.dom;
        let mut at = if forward {
            dom.next_sibling(self.id)
        } else {
            dom.prev_sibling(self.id)
        };
        while let Some(id) = at {
            let n = self.with(id);
            if n.element().is_some() {
                return Some(n);
            }
            at = if forward {
                dom.next_sibling(id)
            } else {
                dom.prev_sibling(id)
            };
        }
        None
    }

    /// Marks this node's ancestors as having dirty descendants (stops at one already marked).
    pub(crate) fn mark_ancestors_dirty(&self) {
        let mut at = self.t.dom.parent(self.id);
        while let Some(p) = at {
            let s = &self.t.slots[p.index() as usize];
            if s.dirty_descendants.replace(true) {
                break;
            }
            at = self.t.dom.parent(p);
        }
    }
}

impl NodeInfo for N<'_> {
    fn is_element(&self) -> bool {
        self.element().is_some()
    }

    fn is_text_node(&self) -> bool {
        matches!(self.data(), Some(NodeData::Text(_)))
    }
}

impl<'a> TDocument for N<'a> {
    type ConcreteNode = N<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn is_html_document(&self) -> bool {
        true
    }

    fn quirks_mode(&self) -> style::context::QuirksMode {
        style::context::QuirksMode::NoQuirks
    }

    fn shared_lock(&self) -> &SharedRwLock {
        self.t.guard
    }
}

impl<'a> TShadowRoot for N<'a> {
    type ConcreteNode = N<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn host(&self) -> <Self::ConcreteNode as TNode>::ConcreteElement {
        *self
    }

    fn style_data<'b>(&self) -> Option<&'b style::stylist::CascadeData>
    where
        Self: 'b,
    {
        None
    }
}

impl<'a> TNode for N<'a> {
    type ConcreteElement = N<'a>;
    type ConcreteDocument = N<'a>;
    type ConcreteShadowRoot = N<'a>;

    fn parent_node(&self) -> Option<Self> {
        self.t.dom.parent(self.id).map(|p| self.with(p))
    }

    fn first_child(&self) -> Option<Self> {
        self.t.dom.first_child(self.id).map(|c| self.with(c))
    }

    fn last_child(&self) -> Option<Self> {
        self.t.dom.last_child(self.id).map(|c| self.with(c))
    }

    fn prev_sibling(&self) -> Option<Self> {
        self.t.dom.prev_sibling(self.id).map(|c| self.with(c))
    }

    fn next_sibling(&self) -> Option<Self> {
        self.t.dom.next_sibling(self.id).map(|c| self.with(c))
    }

    fn owner_doc(&self) -> Self::ConcreteDocument {
        self.with(self.t.dom.root())
    }

    fn is_in_document(&self) -> bool {
        true
    }

    fn traversal_parent(&self) -> Option<Self::ConcreteElement> {
        self.parent_node().and_then(|p| p.as_element())
    }

    fn opaque(&self) -> OpaqueNode {
        OpaqueNode(self.key() as usize)
    }

    fn debug_id(self) -> usize {
        self.key() as usize
    }

    fn as_element(&self) -> Option<Self::ConcreteElement> {
        self.element().map(|_| *self)
    }

    fn as_document(&self) -> Option<Self::ConcreteDocument> {
        matches!(self.data(), Some(NodeData::Document)).then_some(*self)
    }

    fn as_shadow_root(&self) -> Option<Self::ConcreteShadowRoot> {
        None
    }
}

impl Element for N<'_> {
    type Impl = SelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        let p = NonNull::new((self.key() as usize).wrapping_add(1) as *mut ())
            .unwrap_or(NonNull::dangling());
        OpaqueElement::from_non_null_ptr(p)
    }

    fn parent_element(&self) -> Option<Self> {
        TElement::traversal_parent(self)
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        self.element_siblings(false)
    }

    fn next_sibling_element(&self) -> Option<Self> {
        self.element_siblings(true)
    }

    fn first_element_child(&self) -> Option<Self> {
        let first = self.first_child()?;
        if first.element().is_some() {
            Some(first)
        } else {
            first.element_siblings(true)
        }
    }

    fn is_html_element_in_html_document(&self) -> bool {
        self.element().is_some_and(|(n, _)| n.ns == ns!(html))
    }

    fn has_local_name(&self, local_name: &LocalName) -> bool {
        self.element().is_some_and(|(n, _)| n.local == *local_name)
    }

    fn has_namespace(&self, ns: &Namespace) -> bool {
        self.element().is_some_and(|(n, _)| n.ns == *ns)
    }

    fn is_same_type(&self, other: &Self) -> bool {
        match (self.element(), other.element()) {
            (Some((a, _)), Some((b, _))) => a.local == b.local && a.ns == b.ns,
            _ => false,
        }
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&GenericAtomIdent<NamespaceStaticSet>>,
        local_name: &GenericAtomIdent<LocalNameStaticSet>,
        operation: &AttrSelectorOperation<&AtomString>,
    ) -> bool {
        let Some((_, attrs)) = self.element() else {
            return false;
        };
        if self.slot().markers.has(&local_name.0)
            && matches!(
                ns,
                NamespaceConstraint::Any | NamespaceConstraint::Specific(_)
            )
        {
            return operation.eval_str("");
        }
        attrs.iter().any(|a| {
            a.qual_name().local == local_name.0
                && match ns {
                    NamespaceConstraint::Any => true,
                    NamespaceConstraint::Specific(ns) => a.qual_name().ns == ns.0,
                }
                && operation.eval_str(&a.value)
        })
    }

    fn match_non_ts_pseudo_class(
        &self,
        pseudo_class: &NonTSPseudoClass,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        let state = self.slot().state;
        let dom = self.t.dom;
        let id = self.id;
        let form_control = || {
            ["input", "select", "textarea"]
                .iter()
                .any(|t| crate::node::is(self, t))
        };
        // Validity as constraint validation sees it: forms and fieldsets by their controls;
        // elements barred from validation are neither valid nor invalid.
        let invalid = || -> Option<bool> {
            if crate::node::is(self, "form") || crate::node::is(self, "fieldset") {
                Some(pageon_dom::forms::has_invalid(dom, id))
            } else if pageon_dom::forms::barred(dom, id) {
                None
            } else {
                Some(!pageon_dom::forms::validity(dom, id).valid())
            }
        };
        let user =
            || dom.control(id).is_some_and(|c| c.user_validity) && !crate::node::is(self, "form");
        let text_value = || {
            dom.control(id)
                .and_then(|c| c.value.clone())
                .or_else(|| self.attr("value").map(str::to_string))
                .unwrap_or_default()
        };
        match pseudo_class {
            NonTSPseudoClass::Active => state.contains(ElementState::ACTIVE),
            NonTSPseudoClass::AnyLink => state.intersects(ElementState::VISITED_OR_UNVISITED),
            NonTSPseudoClass::Checked => state.contains(ElementState::CHECKED),
            NonTSPseudoClass::Disabled => state.contains(ElementState::DISABLED),
            NonTSPseudoClass::Enabled => state.contains(ElementState::ENABLED),
            NonTSPseudoClass::Focus => state.contains(ElementState::FOCUS),
            NonTSPseudoClass::Hover => state.contains(ElementState::HOVER),
            NonTSPseudoClass::Link => state.contains(ElementState::UNVISITED),
            NonTSPseudoClass::PopoverOpen => dom.popover_open(id),
            NonTSPseudoClass::Modal => self.slot().markers.top_layer,
            NonTSPseudoClass::Open => {
                (crate::node::is(self, "details") || crate::node::is(self, "dialog"))
                    && self.attr("open").is_some()
            }
            NonTSPseudoClass::Valid => invalid() == Some(false),
            NonTSPseudoClass::Invalid => invalid() == Some(true),
            NonTSPseudoClass::UserValid => user() && invalid() == Some(false),
            NonTSPseudoClass::UserInvalid => user() && invalid() == Some(true),
            NonTSPseudoClass::Required => form_control() && self.attr("required").is_some(),
            NonTSPseudoClass::Optional => form_control() && self.attr("required").is_none(),
            NonTSPseudoClass::PlaceholderShown => {
                (crate::node::is(self, "input") || crate::node::is(self, "textarea"))
                    && self.attr("placeholder").is_some()
                    && text_value().is_empty()
            }
            NonTSPseudoClass::ReadWrite | NonTSPseudoClass::ReadOnly => {
                let editable = ((crate::node::is(self, "input")
                    || crate::node::is(self, "textarea"))
                    && self.attr("readonly").is_none()
                    && !state.contains(ElementState::DISABLED))
                    || self
                        .attr("contenteditable")
                        .is_some_and(|v| v.is_empty() || v.eq_ignore_ascii_case("true"));
                editable == matches!(pseudo_class, NonTSPseudoClass::ReadWrite)
            }
            NonTSPseudoClass::CustomState(name) => dom
                .internals(id)
                .is_some_and(|i| i.states.iter().any(|s| s.as_str() == &*name.0)),
            NonTSPseudoClass::Defined => {
                self.slot().markers.defined
                    || self
                        .element()
                        .is_none_or(|(n, _)| !pageon_dom::is_custom_name(&n.local))
            }
            NonTSPseudoClass::FocusWithin
            | NonTSPseudoClass::FocusVisible
            | NonTSPseudoClass::Fullscreen
            | NonTSPseudoClass::Indeterminate
            | NonTSPseudoClass::Lang(_)
            | NonTSPseudoClass::ServoNonZeroBorder
            | NonTSPseudoClass::Target
            | NonTSPseudoClass::Visited
            | NonTSPseudoClass::Autofill
            | NonTSPseudoClass::Default
            | NonTSPseudoClass::InRange
            | NonTSPseudoClass::OutOfRange
            | NonTSPseudoClass::MozMeterOptimum
            | NonTSPseudoClass::MozMeterSubOptimum
            | NonTSPseudoClass::MozMeterSubSubOptimum => false,
        }
    }

    fn match_pseudo_element(
        &self,
        _pe: &PseudoElement,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        false
    }

    fn apply_selector_flags(&self, flags: ElementSelectorFlags) {
        let own = flags.for_self();
        if !own.is_empty() {
            let s = self.slot();
            s.selector_flags.set(s.selector_flags.get() | own);
        }
        let parent = flags.for_parent();
        if !parent.is_empty()
            && let Some(p) = self.parent_node()
        {
            let s = p.slot();
            s.selector_flags.set(s.selector_flags.get() | parent);
        }
    }

    fn is_link(&self) -> bool {
        (self.is_tag(&local_name!("a")) || self.is_tag(&local_name!("area")))
            && self.attr("href").is_some()
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &AtomIdent, case_sensitivity: CaseSensitivity) -> bool {
        self.slot()
            .id
            .as_ref()
            .is_some_and(|a| case_sensitivity.eq_atom(a, id))
    }

    fn has_class(&self, name: &AtomIdent, case_sensitivity: CaseSensitivity) -> bool {
        self.slot()
            .classes
            .iter()
            .any(|a| case_sensitivity.eq_atom(a, name))
    }

    fn imported_part(&self, _name: &AtomIdent) -> Option<AtomIdent> {
        None
    }

    fn is_part(&self, _name: &AtomIdent) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        let mut at = self.t.dom.first_child(self.id);
        while let Some(c) = at {
            match self.t.dom.get(c).map(|n| &n.data) {
                Some(NodeData::Element { .. }) => return false,
                Some(NodeData::Text(t)) if !t.is_empty() => return false,
                _ => {}
            }
            at = self.t.dom.next_sibling(c);
        }
        true
    }

    fn is_root(&self) -> bool {
        self.parent_node()
            .is_some_and(|p| p.as_document().is_some())
    }

    fn has_custom_state(&self, _name: &AtomIdent) -> bool {
        false
    }

    fn add_element_unique_hashes(&self, filter: &mut BloomFilter) -> bool {
        each_relevant_element_hash(*self, |hash| filter.insert_hash(hash & BLOOM_HASH_MASK));
        true
    }
}

/// The children styling walks (elements and text).
pub(crate) struct Children<'a> {
    at: Option<N<'a>>,
}

impl<'a> Iterator for Children<'a> {
    type Item = N<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let n = self.at?;
        self.at = n.next_sibling();
        Some(n)
    }
}

// Stylo's trait declares some methods `unsafe`; their bodies here are safe code.
#[allow(unsafe_code)]
impl<'a> TElement for N<'a> {
    type ConcreteNode = N<'a>;
    type TraversalChildrenIterator = Children<'a>;

    fn as_node(&self) -> Self::ConcreteNode {
        *self
    }

    fn implicit_scope_for_sheet_in_shadow_root(
        _opaque_host: OpaqueElement,
        _sheet_index: usize,
    ) -> Option<ImplicitScopeRoot> {
        None
    }

    fn traversal_children(&self) -> LayoutIterator<Self::TraversalChildrenIterator> {
        LayoutIterator(Children {
            at: self.first_child(),
        })
    }

    fn is_html_element(&self) -> bool {
        self.element().is_some_and(|(n, _)| n.ns == ns!(html))
    }

    fn is_mathml_element(&self) -> bool {
        self.element().is_some_and(|(n, _)| n.ns == ns!(mathml))
    }

    fn is_svg_element(&self) -> bool {
        self.element().is_some_and(|(n, _)| n.ns == ns!(svg))
    }

    fn style_attribute(&self) -> Option<ArcBorrow<'_, Locked<PropertyDeclarationBlock>>> {
        self.slot().style_attribute.as_ref().map(Arc::borrow_arc)
    }

    fn state(&self) -> ElementState {
        self.slot().state
    }

    fn has_part_attr(&self) -> bool {
        false
    }

    fn exports_any_part(&self) -> bool {
        false
    }

    fn id(&self) -> Option<&Atom> {
        self.slot().id.as_ref()
    }

    fn each_class<F>(&self, mut callback: F)
    where
        F: FnMut(&AtomIdent),
    {
        for c in &self.slot().classes {
            callback(AtomIdent::cast(c));
        }
    }

    fn each_attr_name<F>(&self, mut callback: F)
    where
        F: FnMut(&style::LocalName),
    {
        if let Some((_, attrs)) = self.element() {
            for a in attrs {
                callback(&GenericAtomIdent(a.qual_name().local.clone()));
            }
        }
        for name in self.slot().markers.names() {
            callback(&GenericAtomIdent(LocalName::from(name)));
        }
    }

    fn has_dirty_descendants(&self) -> bool {
        self.slot().dirty_descendants.get()
    }

    fn has_snapshot(&self) -> bool {
        self.slot().has_snapshot.get()
    }

    fn handled_snapshot(&self) -> bool {
        self.slot().handled_snapshot.get()
    }

    unsafe fn set_handled_snapshot(&self) {
        self.slot().handled_snapshot.set(true);
    }

    unsafe fn set_dirty_descendants(&self) {
        self.slot().dirty_descendants.set(true);
    }

    unsafe fn unset_dirty_descendants(&self) {
        self.slot().dirty_descendants.set(false);
    }

    fn store_children_to_process(&self, _n: isize) {}

    fn did_process_child(&self) -> isize {
        0
    }

    unsafe fn ensure_data(&self) -> ElementDataMut<'_> {
        let s = self.slot();
        if !s.has_data.replace(true) {
            *s.data.borrow_mut() = ElementData::default();
        }
        s.data.borrow_mut()
    }

    unsafe fn clear_data(&self) {
        let s = self.slot();
        s.has_data.set(false);
        *s.data.borrow_mut() = ElementData::default();
    }

    fn has_data(&self) -> bool {
        self.slot().has_data.get()
    }

    fn borrow_data(&self) -> Option<ElementDataRef<'_>> {
        let s = self.slot();
        s.has_data.get().then(|| s.data.borrow())
    }

    fn mutate_data(&self) -> Option<ElementDataMut<'_>> {
        let s = self.slot();
        s.has_data.get().then(|| s.data.borrow_mut())
    }

    fn skip_item_display_fixup(&self) -> bool {
        false
    }

    fn may_have_animations(&self) -> bool {
        false
    }

    fn has_animations(&self, _context: &SharedStyleContext) -> bool {
        false
    }

    fn has_css_animations(
        &self,
        _context: &SharedStyleContext,
        _pseudo_element: Option<PseudoElement>,
    ) -> bool {
        false
    }

    fn has_css_transitions(
        &self,
        _context: &SharedStyleContext,
        _pseudo_element: Option<PseudoElement>,
    ) -> bool {
        false
    }

    fn animation_rule(
        &self,
        _context: &SharedStyleContext,
    ) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
        None
    }

    fn transition_rule(
        &self,
        _context: &SharedStyleContext,
    ) -> Option<Arc<Locked<PropertyDeclarationBlock>>> {
        None
    }

    fn shadow_root(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        None
    }

    fn containing_shadow(&self) -> Option<<Self::ConcreteNode as TNode>::ConcreteShadowRoot> {
        None
    }

    fn get_attr(&self, attr: &style::LocalName, ns: &style::Namespace) -> Option<String> {
        self.element()?
            .1
            .iter()
            .find(|a| a.qual_name().local == attr.0 && a.qual_name().ns == ns.0)
            .map(|a| a.value.clone())
    }

    fn lang_attr(&self) -> Option<style::selector_parser::AttrValue> {
        None
    }

    fn match_element_lang(
        &self,
        _override_lang: Option<Option<style::selector_parser::AttrValue>>,
        _value: &style::selector_parser::Lang,
    ) -> bool {
        false
    }

    fn is_html_document_body_element(&self) -> bool {
        self.is_tag(&local_name!("body"))
            && self.parent_node().is_some_and(|p| {
                p.is_tag(&local_name!("html"))
                    && p.parent_node().and_then(|d| d.as_document()).is_some()
            })
    }

    fn synthesize_presentational_hints_for_legacy_attributes<V>(
        &self,
        _visited_handling: VisitedHandlingMode,
        hints: &mut V,
    ) where
        V: Push<ApplicableDeclarationBlock>,
    {
        crate::hints::push(self, self.t.guard, &mut |decl: PropertyDeclaration| {
            hints.push(ApplicableDeclarationBlock::from_declarations(
                Arc::new(
                    self.t
                        .guard
                        .wrap(PropertyDeclarationBlock::with_one(decl, Importance::Normal)),
                ),
                CascadeLevel::new(CascadeOrigin::PresHints),
                LayerOrder::root(),
            ));
        });
    }

    fn local_name(&self) -> &LocalName {
        self.element().map_or(&NONE.0, |(n, _)| &n.local)
    }

    fn namespace(&self) -> &Namespace {
        self.element().map_or(&NONE.1, |(n, _)| &n.ns)
    }

    fn query_container_size(
        &self,
        _display: &style::values::specified::Display,
    ) -> euclid::default::Size2D<Option<app_units::Au>> {
        Default::default()
    }

    fn each_custom_state<F>(&self, _callback: F)
    where
        F: FnMut(&AtomIdent),
    {
    }

    fn has_selector_flags(&self, flags: ElementSelectorFlags) -> bool {
        self.slot().selector_flags.get().contains(flags)
    }

    fn relative_selector_search_direction(&self) -> ElementSelectorFlags {
        let flags = self.slot().selector_flags.get();
        for f in [
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR_SIBLING,
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_ANCESTOR,
            ElementSelectorFlags::RELATIVE_SELECTOR_SEARCH_DIRECTION_SIBLING,
        ] {
            if flags.contains(f) {
                return f;
            }
        }
        ElementSelectorFlags::empty()
    }

    fn compute_layout_damage(old: &ComputedValues, new: &ComputedValues) -> RestyleDamage {
        if std::ptr::eq(old, new) {
            RestyleDamage::empty()
        } else {
            RestyleDamage::RELAYOUT
        }
    }
}

/// The element's attribute named `local` (no namespace), for presentational hints.
pub(crate) fn attr<'a>(n: &N<'a>, local: &str) -> Option<&'a str> {
    n.attr(local)
}

/// Whether the element is the HTML element `tag`.
pub(crate) fn is(n: &N<'_>, tag: &str) -> bool {
    n.element()
        .is_some_and(|(q, _)| &*q.local == tag && q.ns == ns!(html))
}
