//! The arena: nodes addressed by generational handles that never alias.

use std::fmt;

use html5ever::QualName;

/// A handle to a node. It resolves only while its node exists; after removal it never
/// resolves again, not even when the storage is reused.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId {
    index: u32,
    generation: u32,
}

impl NodeId {
    /// A handle from its parts, e.g. as stored by a script binding. It is checked like any other
    /// handle: [`Dom::get`] returns `None` unless the slot still holds that generation.
    pub fn from_parts(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// The slot index (stable while the node lives; reused after removal).
    pub fn index(self) -> u32 {
        self.index
    }

    /// The slot generation.
    pub fn generation(self) -> u32 {
        self.generation
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}v{}", self.index, self.generation)
    }
}

/// XML namespaces an element or attribute can be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// HTML.
    Html,
    /// SVG.
    Svg,
    /// MathML.
    MathMl,
    /// XLink.
    Xlink,
    /// XML.
    Xml,
    /// XMLNS.
    Xmlns,
    /// No namespace.
    None,
}

impl Namespace {
    pub(crate) fn of(ns: &html5ever::Namespace) -> Self {
        match &**ns {
            "http://www.w3.org/1999/xhtml" => Self::Html,
            "http://www.w3.org/2000/svg" => Self::Svg,
            "http://www.w3.org/1998/Math/MathML" => Self::MathMl,
            "http://www.w3.org/1999/xlink" => Self::Xlink,
            "http://www.w3.org/XML/1998/namespace" => Self::Xml,
            "http://www.w3.org/2000/xmlns/" => Self::Xmlns,
            _ => Self::None,
        }
    }
}

/// The document's compatibility mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuirksMode {
    /// Standards mode.
    NoQuirks,
    /// Almost-standards mode.
    LimitedQuirks,
    /// Quirks mode.
    Quirks,
}

/// An attribute of an element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub(crate) name: QualName,
    /// The value.
    pub value: String,
}

impl Attribute {
    /// A new attribute with no namespace.
    pub fn new(local: &str, value: impl Into<String>) -> Self {
        Self {
            name: QualName::new(None, html5ever::ns!(), local.into()),
            value: value.into(),
        }
    }

    /// The local name.
    pub fn local_name(&self) -> &str {
        &self.name.local
    }

    /// The namespace.
    pub fn namespace(&self) -> Namespace {
        Namespace::of(&self.name.ns)
    }

    /// The parser's qualified name (for engine crates).
    #[doc(hidden)]
    pub fn qual_name(&self) -> &QualName {
        &self.name
    }

    /// The qualified name as written (`xlink:href`).
    pub fn qualified_name(&self) -> String {
        match &self.name.prefix {
            Some(p) => format!("{p}:{}", self.name.local),
            None => self.name.local.to_string(),
        }
    }
}

/// What a node is.
#[derive(Debug, Clone)]
pub enum NodeData {
    /// The document.
    Document,
    /// A document fragment (template contents, fragment parsing).
    DocumentFragment,
    /// A shadow root: a fragment attached to `host` (it has no parent).
    ShadowRoot {
        /// The element it is attached to.
        host: NodeId,
        /// Open or closed.
        mode: crate::ShadowMode,
        /// `delegatesFocus`.
        delegates_focus: bool,
        /// `clonable`.
        clonable: bool,
        /// `serializable`.
        serializable: bool,
        /// `slotAssignment`.
        slot_assignment: crate::SlotAssignment,
    },
    /// `<!DOCTYPE …>`.
    Doctype {
        /// Name.
        name: String,
        /// Public id.
        public_id: String,
        /// System id.
        system_id: String,
    },
    /// An element.
    Element {
        /// Qualified name.
        name: QualName,
        /// Attributes in source order.
        attrs: Vec<Attribute>,
        /// For `<template>`: the contents fragment.
        template_contents: Option<NodeId>,
        /// MathML `annotation-xml` integration point (parser bookkeeping).
        mathml_annotation_xml_integration_point: bool,
    },
    /// Text.
    Text(String),
    /// A comment.
    Comment(String),
    /// A processing instruction.
    ProcessingInstruction {
        /// Target.
        target: String,
        /// Data.
        data: String,
    },
}

/// A node and its tree links.
#[derive(Debug, Clone)]
pub struct Node {
    pub(crate) parent: Option<NodeId>,
    pub(crate) first_child: Option<NodeId>,
    pub(crate) last_child: Option<NodeId>,
    pub(crate) prev_sibling: Option<NodeId>,
    pub(crate) next_sibling: Option<NodeId>,
    /// What the node is.
    pub data: NodeData,
}

impl Node {
    /// The element's local name, if this is an element.
    pub fn local_name(&self) -> Option<&str> {
        match &self.data {
            NodeData::Element { name, .. } => Some(&name.local),
            _ => None,
        }
    }

    /// The element's namespace, if this is an element.
    pub fn namespace(&self) -> Option<Namespace> {
        match &self.data {
            NodeData::Element { name, .. } => Some(Namespace::of(&name.ns)),
            _ => None,
        }
    }

    /// An attribute value by local name (no namespace).
    pub fn attr(&self, local: &str) -> Option<&str> {
        match &self.data {
            NodeData::Element { attrs, .. } => attrs
                .iter()
                .find(|a| a.name.ns.is_empty() && &*a.name.local == local)
                .map(|a| a.value.as_str()),
            _ => None,
        }
    }

    /// The attributes (empty for non-elements).
    pub fn attrs(&self) -> &[Attribute] {
        match &self.data {
            NodeData::Element { attrs, .. } => attrs,
            _ => &[],
        }
    }

    /// Whether this is an HTML element with the given local name.
    pub fn is_html(&self, local: &str) -> bool {
        matches!(&self.data, NodeData::Element { name, .. }
            if &*name.local == local && name.ns == html5ever::ns!(html))
    }
}

#[derive(Debug, Clone)]
struct Slot {
    generation: u32,
    node: Option<Node>,
}

/// Errors of tree mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomError {
    /// A handle no longer resolves.
    Gone(NodeId),
    /// The operation would make a node its own ancestor.
    Cycle,
    /// The reference node is not a child of the given parent.
    NotAChild,
}

impl fmt::Display for DomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gone(id) => write!(f, "node {id:?} no longer exists; re-read the tree"),
            Self::Cycle => f.write_str("a node cannot be inserted into its own subtree"),
            Self::NotAChild => f.write_str("the reference node is not a child of the parent"),
        }
    }
}

impl std::error::Error for DomError {}

/// The arena DOM.
#[derive(Debug, Clone)]
pub struct Dom {
    slots: Vec<Slot>,
    free: Vec<u32>,
    root: NodeId,
    /// Document mode.
    pub quirks_mode: QuirksMode,
    controls: std::collections::HashMap<NodeId, ControlState>,
    /// Whether changes are logged (while a `MutationObserver` exists).
    pub(crate) observing: bool,
    pub(crate) mutations: Vec<Mutation>,
    /// Hosts and their shadow roots.
    pub(crate) shadow: std::collections::HashMap<NodeId, NodeId>,
    /// In a composed tree: each shadow-tree node's original shadow root.
    pub(crate) scopes: std::collections::HashMap<NodeId, NodeId>,
    /// `ElementInternals` of custom elements (see [`crate::forms`]).
    pub(crate) internals: std::collections::HashMap<NodeId, crate::forms::Internals>,
    /// Adopted stylesheets of the document or a shadow root: CSS texts in order.
    pub(crate) adopted: std::collections::HashMap<NodeId, Vec<String>>,
    /// The rules of `<style>` elements a script changed through the CSSOM (`insertRule`),
    /// which layout applies instead of the element's text.
    pub(crate) cssom: std::collections::HashMap<NodeId, String>,
    /// The document was parsed with scripting on, so `<noscript>` content is not rendered.
    pub(crate) scripting: bool,
    /// Live ranges and the selection (see [`crate::ranges`]).
    pub(crate) ranges: crate::ranges::Ranges,
    /// Mutable accesses so far (see [`Dom::changes`]).
    changes: u64,
    /// Open popovers, in the order they were shown.
    popovers: Vec<NodeId>,
}

/// A change to the tree, logged for `MutationObserver` while [`Dom::set_observing`] is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutation {
    /// A child was added to or removed from `target`.
    ChildList {
        /// The parent.
        target: NodeId,
        /// Added child, if any.
        added: Option<NodeId>,
        /// Removed child, if any.
        removed: Option<NodeId>,
        /// The sibling before the change point.
        previous: Option<NodeId>,
        /// The sibling after the change point.
        next: Option<NodeId>,
    },
    /// An attribute of `target` was set or removed.
    Attribute {
        /// The element.
        target: NodeId,
        /// Local name.
        name: String,
        /// The value before.
        old: Option<String>,
    },
    /// A text or comment node's data changed.
    CharacterData {
        /// The node.
        target: NodeId,
        /// The data before.
        old: String,
    },
}

/// The state of a form control once it differs from its markup (set by script or input): the
/// current value, checkedness and selectedness. Attributes keep the defaults (`value`,
/// `checked`, `selected`), as in HTML.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlState {
    /// The current value (`input`, `textarea`).
    pub value: Option<String>,
    /// The current checkedness (checkboxes and radios).
    pub checked: Option<bool>,
    /// The current selectedness (`option`).
    pub selected: Option<bool>,
    /// The selection in the value: start, end (UTF-16) and direction; `None` is a caret at
    /// the end.
    pub selection: Option<(u32, u32, crate::Direction)>,
    /// The value came from the vault: it is never copied or cut.
    pub from_vault: bool,
    /// The custom validity message (`setCustomValidity`); empty or `None`: no custom error.
    pub custom_error: Option<String>,
    /// The value was last changed by a user action (`tooLong`/`tooShort` apply only then).
    pub user_edited: bool,
    /// HTML's user validity: the user changed the control, or a submission was blocked
    /// (`:user-invalid`).
    pub user_validity: bool,
}

impl Default for Dom {
    fn default() -> Self {
        Self::new()
    }
}

impl Dom {
    /// An empty document.
    pub fn new() -> Self {
        let mut dom = Self {
            slots: Vec::new(),
            free: Vec::new(),
            root: NodeId {
                index: 0,
                generation: 0,
            },
            quirks_mode: QuirksMode::NoQuirks,
            controls: std::collections::HashMap::new(),
            observing: false,
            mutations: Vec::new(),
            changes: 0,
            shadow: std::collections::HashMap::new(),
            scopes: std::collections::HashMap::new(),
            internals: std::collections::HashMap::new(),
            adopted: std::collections::HashMap::new(),
            cssom: std::collections::HashMap::new(),
            scripting: false,
            ranges: crate::ranges::Ranges::default(),
            popovers: Vec::new(),
        };
        dom.root = dom.create(NodeData::Document);
        dom
    }

    /// A form control's state, if it differs from its markup.
    pub fn control(&self, id: NodeId) -> Option<&ControlState> {
        self.get(id)?;
        self.controls.get(&id)
    }

    /// A form control's state, for changes.
    pub fn control_mut(&mut self, id: NodeId) -> &mut ControlState {
        self.changes += 1;
        self.controls.entry(id).or_default()
    }

    /// The open popovers that are still in the document, in the order they were shown.
    pub fn open_popovers(&self) -> Vec<NodeId> {
        let root = self.root;
        self.popovers
            .iter()
            .copied()
            .filter(|&p| self.get(p).is_some() && self.is_inclusive_ancestor(root, p))
            .collect()
    }

    /// Whether `id` is an open popover.
    pub fn popover_open(&self, id: NodeId) -> bool {
        self.popovers.contains(&id) && self.is_inclusive_ancestor(self.root, id)
    }

    /// Opens (at the top) or closes a popover.
    pub fn set_popover_open(&mut self, id: NodeId, open: bool) {
        self.changes += 1;
        self.popovers
            .retain(|&p| p != id && self.slots.len() > p.index as usize);
        if open {
            self.popovers.push(id);
        }
    }

    /// A counter that grows with every change to the tree or a control (every mutable
    /// access counts, so it may grow without a visible change, never the reverse).
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// The document node.
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Creates a detached node.
    pub fn create(&mut self, data: NodeData) -> NodeId {
        let node = Node {
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            data,
        };
        match self.free.pop() {
            Some(i) => {
                let slot = &mut self.slots[i as usize];
                slot.node = Some(node);
                NodeId {
                    index: i,
                    generation: slot.generation,
                }
            }
            None => {
                let index = u32::try_from(self.slots.len())
                    .unwrap_or_else(|_| panic!("more than 4 billion nodes"));
                self.slots.push(Slot {
                    generation: 0,
                    node: Some(node),
                });
                NodeId {
                    index,
                    generation: 0,
                }
            }
        }
    }

    /// The node behind a handle, or `None` if it was removed.
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.slots
            .get(id.index as usize)
            .filter(|s| s.generation == id.generation)
            .and_then(|s| s.node.as_ref())
    }

    /// Mutable access to a node.
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.changes += 1;
        self.slots
            .get_mut(id.index as usize)
            .filter(|s| s.generation == id.generation)
            .and_then(|s| s.node.as_mut())
    }

    fn node(&self, id: NodeId) -> Result<&Node, DomError> {
        self.get(id).ok_or(DomError::Gone(id))
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut Node, DomError> {
        self.get_mut(id).ok_or(DomError::Gone(id))
    }

    /// Parent.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.get(id)?.parent
    }

    /// The ancestors of a node, nearest first (not the node itself).
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.parent(id), move |&p| self.parent(p))
    }

    /// First child.
    pub fn first_child(&self, id: NodeId) -> Option<NodeId> {
        self.get(id)?.first_child
    }

    /// Last child.
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.get(id)?.last_child
    }

    /// Next sibling.
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.get(id)?.next_sibling
    }

    /// Previous sibling.
    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.get(id)?.prev_sibling
    }

    /// Children in order.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            dom: self,
            next: self.first_child(id),
        }
    }

    /// Whether `a` is `b` or an ancestor of `b`.
    pub fn is_inclusive_ancestor(&self, a: NodeId, b: NodeId) -> bool {
        let mut cur = Some(b);
        while let Some(c) = cur {
            if c == a {
                return true;
            }
            cur = self.parent(c);
        }
        false
    }

    /// Unlinks a node from its parent (keeps its subtree).
    pub fn detach(&mut self, id: NodeId) -> Result<(), DomError> {
        let (parent, prev, next) = {
            let n = self.node(id)?;
            (n.parent, n.prev_sibling, n.next_sibling)
        };
        let Some(parent) = parent else { return Ok(()) };
        self.ranges_before_remove(id, parent);
        if self.observing {
            self.mutations.push(Mutation::ChildList {
                target: parent,
                added: None,
                removed: Some(id),
                previous: prev,
                next,
            });
        }
        match prev {
            Some(p) => self.node_mut(p)?.next_sibling = next,
            None => self.node_mut(parent)?.first_child = next,
        }
        match next {
            Some(n) => self.node_mut(n)?.prev_sibling = prev,
            None => self.node_mut(parent)?.last_child = prev,
        }
        let n = self.node_mut(id)?;
        n.parent = None;
        n.prev_sibling = None;
        n.next_sibling = None;
        Ok(())
    }

    /// Appends `child` as the last child of `parent` (moving it if attached).
    pub fn append(&mut self, parent: NodeId, child: NodeId) -> Result<(), DomError> {
        self.node(parent)?;
        if self.is_inclusive_ancestor(child, parent) {
            return Err(DomError::Cycle);
        }
        self.detach(child)?;
        let last = self.node(parent)?.last_child;
        {
            let c = self.node_mut(child)?;
            c.parent = Some(parent);
            c.prev_sibling = last;
        }
        match last {
            Some(l) => self.node_mut(l)?.next_sibling = Some(child),
            None => self.node_mut(parent)?.first_child = Some(child),
        }
        self.node_mut(parent)?.last_child = Some(child);
        self.ranges_after_insert(child);
        if self.observing {
            self.mutations.push(Mutation::ChildList {
                target: parent,
                added: Some(child),
                removed: None,
                previous: last,
                next: None,
            });
        }
        Ok(())
    }

    /// Inserts `child` before `reference` (a child of `parent`).
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        child: NodeId,
        reference: NodeId,
    ) -> Result<(), DomError> {
        if self.node(reference)?.parent != Some(parent) {
            return Err(DomError::NotAChild);
        }
        if self.is_inclusive_ancestor(child, parent) {
            return Err(DomError::Cycle);
        }
        if child == reference {
            return Ok(());
        }
        self.detach(child)?;
        let prev = self.node(reference)?.prev_sibling;
        {
            let c = self.node_mut(child)?;
            c.parent = Some(parent);
            c.prev_sibling = prev;
            c.next_sibling = Some(reference);
        }
        self.node_mut(reference)?.prev_sibling = Some(child);
        match prev {
            Some(p) => self.node_mut(p)?.next_sibling = Some(child),
            None => self.node_mut(parent)?.first_child = Some(child),
        }
        self.ranges_after_insert(child);
        if self.observing {
            self.mutations.push(Mutation::ChildList {
                target: parent,
                added: Some(child),
                removed: None,
                previous: prev,
                next: Some(reference),
            });
        }
        Ok(())
    }

    /// Removes a node and its whole subtree; every handle into it stops resolving.
    pub fn remove(&mut self, id: NodeId) -> Result<(), DomError> {
        self.detach(id)?;
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            let Some(slot) = self.slots.get_mut(n.index as usize) else {
                continue;
            };
            if slot.generation != n.generation {
                continue;
            }
            let Some(node) = slot.node.take() else {
                continue;
            };
            slot.generation = slot.generation.wrapping_add(1);
            self.free.push(n.index);
            let mut c = node.first_child;
            while let Some(k) = c {
                stack.push(k);
                c = self.get(k).and_then(|x| x.next_sibling);
            }
            if let NodeData::Element {
                template_contents: Some(t),
                ..
            } = node.data
            {
                stack.push(t);
            }
        }
        self.ranges_after_free();
        Ok(())
    }

    /// Sets (or replaces) an attribute with no namespace.
    pub fn set_attr(&mut self, id: NodeId, local: &str, value: &str) -> Result<(), DomError> {
        let mut old = None;
        let mut element = false;
        if let NodeData::Element { attrs, .. } = &mut self.node_mut(id)?.data {
            element = true;
            match attrs
                .iter_mut()
                .find(|a| a.name.ns.is_empty() && &*a.name.local == local)
            {
                Some(a) => old = Some(std::mem::replace(&mut a.value, value.to_string())),
                None => attrs.push(Attribute::new(local, value)),
            }
        }
        if self.observing && element {
            self.mutations.push(Mutation::Attribute {
                target: id,
                name: local.to_string(),
                old,
            });
        }
        Ok(())
    }

    /// Removes an attribute with no namespace.
    pub fn remove_attr(&mut self, id: NodeId, local: &str) -> Result<(), DomError> {
        let mut old = None;
        if let NodeData::Element { attrs, .. } = &mut self.node_mut(id)?.data
            && let Some(i) = attrs
                .iter()
                .position(|a| a.name.ns.is_empty() && &*a.name.local == local)
        {
            old = Some(attrs.remove(i).value);
        }
        if self.observing && old.is_some() {
            self.mutations.push(Mutation::Attribute {
                target: id,
                name: local.to_string(),
                old,
            });
        }
        Ok(())
    }

    /// Replaces a text or comment node's data (DOM's replace data over the whole node).
    pub fn set_text(&mut self, id: NodeId, text: &str) -> Result<(), DomError> {
        let len = self.node_length(id);
        self.replace_data(id, 0, len, text)
    }

    /// Starts or stops logging changes (for `MutationObserver`); stopping drops the log.
    pub fn set_observing(&mut self, on: bool) {
        self.observing = on;
        if !on {
            self.mutations.clear();
        }
    }

    /// The changes logged so far (not taken).
    pub fn mutations(&self) -> &[Mutation] {
        &self.mutations
    }

    /// The changes logged since the last call.
    pub fn take_mutations(&mut self) -> Vec<Mutation> {
        std::mem::take(&mut self.mutations)
    }

    /// Number of live nodes.
    pub fn len(&self) -> usize {
        self.slots.len() - self.free.len()
    }

    /// Whether only the document node exists.
    pub fn is_empty(&self) -> bool {
        self.len() <= 1
    }

    /// Pre-order traversal of the subtree at `id` with enter/leave events (iterative).
    pub fn traverse(&self, id: NodeId) -> Traverse<'_> {
        Traverse {
            dom: self,
            stack: if self.get(id).is_some() {
                vec![(id, false)]
            } else {
                Vec::new()
            },
        }
    }
}

/// Iterator over a node's children.
pub struct Children<'a> {
    dom: &'a Dom,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let cur = self.next?;
        self.next = self.dom.next_sibling(cur);
        Some(cur)
    }
}

/// A traversal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Entering a node (pre-order).
    Enter(NodeId),
    /// Leaving a node, after its children.
    Leave(NodeId),
}

/// Iterative pre-order traversal with enter/leave events.
pub struct Traverse<'a> {
    dom: &'a Dom,
    stack: Vec<(NodeId, bool)>,
}

impl Iterator for Traverse<'_> {
    type Item = Edge;

    fn next(&mut self) -> Option<Edge> {
        let (id, entered) = self.stack.pop()?;
        if entered {
            return Some(Edge::Leave(id));
        }
        self.stack.push((id, true));
        let kids: Vec<NodeId> = self.dom.children(id).collect();
        self.stack
            .extend(kids.into_iter().rev().map(|k| (k, false)));
        Some(Edge::Enter(id))
    }
}
