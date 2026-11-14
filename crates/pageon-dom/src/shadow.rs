//! Shadow trees (DOM Standard): shadow roots attached to hosts, slots and their assigned
//! nodes, shadow-including tree walks, and the composed (flat) tree that rendering uses.
//!
//! A shadow root is a parentless node whose children are its shadow tree, so ordinary
//! traversal, selectors and mutation work inside it unchanged. [`Dom::composed`] clones the
//! arena with the same [`NodeId`]s and relinks it into the flat tree, so layout, the
//! accessibility tree, refs and actions address the real nodes.

use std::collections::HashMap;

use crate::dom::{Dom, DomError, NodeData, NodeId};

/// `ShadowRootMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowMode {
    /// `host.shadowRoot` returns it.
    Open,
    /// Hidden from scripts outside the tree (not from people).
    Closed,
}

/// `SlotAssignmentMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotAssignment {
    /// By the `slot` attribute.
    Named,
    /// By `slot.assign()` (not implemented: nothing is assigned).
    Manual,
}

/// `ShadowRootInit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowInit {
    /// Mode.
    pub mode: ShadowMode,
    /// `delegatesFocus`.
    pub delegates_focus: bool,
    /// `clonable`.
    pub clonable: bool,
    /// `serializable`.
    pub serializable: bool,
    /// `slotAssignment`.
    pub slot_assignment: SlotAssignment,
}

impl ShadowInit {
    /// An open root with the defaults.
    pub fn open() -> Self {
        Self {
            mode: ShadowMode::Open,
            delegates_focus: false,
            clonable: false,
            serializable: false,
            slot_assignment: SlotAssignment::Named,
        }
    }
}

/// Why a shadow root cannot be attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachError {
    /// The element cannot host one (DOM's list of valid shadow hosts, plus custom names).
    NotAHost,
    /// It has one already.
    Attached,
}

/// Elements that may host a shadow root, besides valid custom element names.
const HOSTS: &[&str] = &[
    "article",
    "aside",
    "blockquote",
    "body",
    "div",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "main",
    "nav",
    "p",
    "section",
    "span",
];

/// Whether `name` is a valid custom element name (HTML): a lowercase ASCII letter first, a
/// hyphen, no uppercase, and not one of the reserved names.
pub fn is_custom_name(name: &str) -> bool {
    const RESERVED: &[&str] = &[
        "annotation-xml",
        "color-profile",
        "font-face",
        "font-face-src",
        "font-face-uri",
        "font-face-format",
        "font-face-name",
        "missing-glyph",
    ];
    name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.contains('-')
        && !name.chars().any(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| {
            !c.is_ascii() || c.is_ascii_lowercase() || c.is_ascii_digit() || "-._".contains(c)
        })
        && !RESERVED.contains(&name)
}

/// The composed (flat) tree and the scopes its nodes come from.
#[derive(Debug, Clone)]
pub struct Composed {
    /// The flat tree: a host's children are its shadow tree's, a slot's children are its
    /// assigned nodes (or its fallback), unassigned light children are unlinked.
    pub dom: Dom,
    /// Element → the shadow tree it belongs to (ordinals from 1; the document is 0).
    pub scope_of: HashMap<NodeId, u32>,
    /// Host → the ordinal of the shadow tree it hosts.
    pub host_of: HashMap<NodeId, u32>,
    /// Node → the ordinal of the shadow tree whose slot it is assigned to.
    pub slotted: HashMap<NodeId, u32>,
}

impl Dom {
    /// Whether any shadow root is attached.
    pub fn has_shadow(&self) -> bool {
        !self.shadow.is_empty()
    }

    /// Attaches a shadow root to `host`.
    pub fn attach_shadow(&mut self, host: NodeId, init: ShadowInit) -> Result<NodeId, AttachError> {
        let ok = self.get(host).is_some_and(|n| {
            n.namespace() == Some(crate::dom::Namespace::Html)
                && n.local_name()
                    .is_some_and(|l| HOSTS.contains(&l) || is_custom_name(l))
        });
        if !ok {
            return Err(AttachError::NotAHost);
        }
        if self.shadow.contains_key(&host) {
            return Err(AttachError::Attached);
        }
        let root = self.create(NodeData::ShadowRoot {
            host,
            mode: init.mode,
            delegates_focus: init.delegates_focus,
            clonable: init.clonable,
            serializable: init.serializable,
            slot_assignment: init.slot_assignment,
        });
        self.shadow.insert(host, root);
        Ok(root)
    }

    /// The shadow root of `host`, open or closed.
    pub fn shadow_root(&self, host: NodeId) -> Option<NodeId> {
        self.shadow
            .get(&host)
            .copied()
            .filter(|&r| self.get(r).is_some())
    }

    /// The host of a shadow root.
    pub fn host(&self, root: NodeId) -> Option<NodeId> {
        match self.get(root)?.data {
            NodeData::ShadowRoot { host, .. } => Some(host),
            _ => None,
        }
    }

    /// The root of `id`'s tree: the document, a shadow root, or a detached subtree's top.
    pub fn root_of(&self, id: NodeId) -> NodeId {
        self.ancestors(id).last().unwrap_or(id)
    }

    /// The shadow-including root: through hosts up to the document (or a detached top).
    pub fn shadow_including_root(&self, id: NodeId) -> NodeId {
        let mut r = self.root_of(id);
        while let Some(h) = self.host(r) {
            r = self.root_of(h);
        }
        r
    }

    /// Whether `id` is connected: its shadow-including root is the document.
    pub fn is_connected(&self, id: NodeId) -> bool {
        self.get(id).is_some() && self.shadow_including_root(id) == self.root()
    }

    /// The parent in the shadow-including tree: a shadow root's parent is its host.
    pub fn shadow_parent(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id).or_else(|| self.host(id))
    }

    /// The parent in the flat tree: an assigned node's slot, a shadow tree's top nodes'
    /// host, otherwise the parent. Unassigned children of a host have none.
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId> {
        if let Some(slot) = self.assigned_slot(id) {
            return Some(slot);
        }
        let p = self.parent(id)?;
        if let Some(h) = self.host(p) {
            return Some(h);
        }
        if self.shadow_root(p).is_some() {
            return None; // a host's light child that no slot takes is not rendered
        }
        Some(p)
    }

    /// Ancestors in the flat tree, nearest first.
    pub fn flat_ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(self.flat_parent(id), move |&p| self.flat_parent(p))
    }

    /// The shadow root of the tree `id` is in (`None` for the document's tree), also in a
    /// composed tree: ids and `label[for]` resolve within it, as in the DOM.
    pub fn tree_scope(&self, id: NodeId) -> Option<NodeId> {
        if let Some(&r) = self.scopes.get(&id) {
            return Some(r);
        }
        let r = self.root_of(id);
        matches!(self.get(r)?.data, NodeData::ShadowRoot { .. }).then_some(r)
    }

    /// Whether `node` is a `<slot>` in a shadow tree.
    fn is_slot(&self, node: NodeId) -> bool {
        self.get(node).is_some_and(|n| n.is_html("slot")) && self.host(self.root_of(node)).is_some()
    }

    fn slot_name(&self, slot: NodeId) -> &str {
        self.get(slot)
            .and_then(|n| n.attr("name"))
            .unwrap_or_default()
    }

    /// A slottable's slot name (`slot` attribute; text has none).
    fn slottable_name(&self, node: NodeId) -> &str {
        self.get(node)
            .and_then(|n| n.attr("slot"))
            .unwrap_or_default()
    }

    fn slottable(&self, node: NodeId) -> bool {
        self.get(node).is_some_and(|n| {
            matches!(n.data, NodeData::Element { .. })
                || matches!(&n.data, NodeData::Text(t) if !t.is_empty())
        })
    }

    /// The slot a host's child is assigned to: the first slot in tree order of the host's
    /// (named-assignment) shadow tree whose name matches.
    pub fn assigned_slot(&self, node: NodeId) -> Option<NodeId> {
        let host = self.parent(node)?;
        let root = self.shadow_root(host)?;
        if !self.slottable(node)
            || !matches!(
                self.get(root)?.data,
                NodeData::ShadowRoot {
                    slot_assignment: SlotAssignment::Named,
                    ..
                }
            )
        {
            return None;
        }
        let name = self.slottable_name(node);
        self.traverse(root).find_map(|e| match e {
            crate::Edge::Enter(s) if self.is_slot(s) && self.slot_name(s) == name => Some(s),
            _ => None,
        })
    }

    /// A slot's assigned nodes (`assignedNodes()`); with `flatten`, slots among them are
    /// replaced by their own assigned nodes, and an unassigned slot by its fallback.
    pub fn assigned_nodes(&self, slot: NodeId, flatten: bool) -> Vec<NodeId> {
        let Some(host) = self.host(self.root_of(slot)).filter(|_| self.is_slot(slot)) else {
            return Vec::new();
        };
        let mine: Vec<NodeId> = self
            .children(host)
            .filter(|&c| self.assigned_slot(c) == Some(slot))
            .collect();
        if !flatten {
            return mine;
        }
        let source = if mine.is_empty() {
            self.children(slot).collect()
        } else {
            mine
        };
        let mut out = Vec::new();
        for n in source {
            if self.is_slot(n) {
                out.extend(self.assigned_nodes(n, true));
            } else {
                out.push(n);
            }
        }
        out
    }

    /// The composed (flat) tree, with the same ids. Pages without shadow trees get a plain
    /// clone.
    pub fn composed(&self) -> Composed {
        let mut out = Composed {
            dom: self.clone(),
            scope_of: HashMap::new(),
            host_of: HashMap::new(),
            slotted: HashMap::new(),
        };
        if !self.has_shadow() {
            return out;
        }
        // Ordinals in shadow-including tree order of the hosts.
        let mut hosts: Vec<(NodeId, NodeId)> = Vec::new();
        let mut stack = vec![self.root()];
        while let Some(n) = stack.pop() {
            let mut kids: Vec<NodeId> = self.children(n).collect();
            if let Some(r) = self.shadow_root(n) {
                hosts.push((n, r));
                kids.insert(0, r);
            }
            stack.extend(kids.into_iter().rev());
        }
        let ordinal: HashMap<NodeId, u32> = hosts
            .iter()
            .enumerate()
            .map(|(i, &(_, r))| (r, u32::try_from(i + 1).unwrap_or(u32::MAX)))
            .collect();
        for &(host, root) in &hosts {
            let n = ordinal[&root];
            out.host_of.insert(host, n);
            for e in self.traverse(root) {
                if let crate::Edge::Enter(x) = e
                    && x != root
                {
                    out.dom.scopes.insert(x, root);
                    if matches!(self.get(x).map(|y| &y.data), Some(NodeData::Element { .. })) {
                        out.scope_of.insert(x, n);
                    }
                }
            }
        }
        // Relink, slots first (they read the original tree), then hosts.
        let d = &mut out.dom;
        for &(host, root) in &hosts {
            let n = ordinal[&root];
            for slot in self.traverse(root).filter_map(|e| match e {
                crate::Edge::Enter(s) if self.is_slot(s) => Some(s),
                _ => None,
            }) {
                let assigned = self.assigned_nodes(slot, false);
                if assigned.is_empty() {
                    continue;
                }
                for c in self.children(slot) {
                    let _ = d.unlink(c);
                }
                for a in assigned {
                    let _ = d.unlink(a);
                    let _ = d.link_last(slot, a);
                    out.slotted.insert(a, n);
                }
            }
            for c in self.children(host) {
                if self.assigned_slot(c).is_none() {
                    let _ = d.unlink(c);
                }
            }
            for c in self.children(root) {
                let _ = d.unlink(c);
                let _ = d.link_last(host, c);
            }
        }
        out.dom.shadow.clear();
        out
    }

    /// Unlinks without logging (composed-tree construction).
    fn unlink(&mut self, id: NodeId) -> Result<(), DomError> {
        let saved = self.observing;
        self.observing = false;
        let r = self.detach(id);
        self.observing = saved;
        r
    }

    /// Appends without logging (composed-tree construction).
    fn link_last(&mut self, parent: NodeId, id: NodeId) -> Result<(), DomError> {
        let saved = self.observing;
        self.observing = false;
        let r = self.append(parent, id);
        self.observing = saved;
        r
    }
}
