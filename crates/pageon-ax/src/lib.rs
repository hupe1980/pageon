//! pageon's accessibility tree.
//!
//! Computes what an assistive technology would get for each node of a `pageon-dom` document:
//! roles (HTML-AAM implicit roles, ARIA explicit roles with fallback and presentational conflict
//! resolution), accessible names (AccName 1.2), states and values. With a [`Layout`], nodes that
//! are not rendered or not visible are ignored by computed style; without one, by the `hidden`
//! attribute and never-rendered elements.
//!
//! ```
//! use pageon_dom::{ParseOptions, parse_document};
//!
//! let dom = parse_document(r#"<label for="e">Email</label><input id="e" type="email" required>"#, ParseOptions::default());
//! let tree = pageon_ax::Tree::build(&dom, None);
//! let input = tree.nodes().iter().find(|n| n.role == "textbox").unwrap();
//! assert_eq!(input.name, "Email");
//! assert!(input.states.required);
//! ```

mod name;
mod role;
mod state;

use std::collections::HashMap;

use pageon_dom::{Dom, Edge, NodeData, NodeId};
use pageon_layout::Layout;

pub use state::{Checked, States};

/// Why a node is not exposed (it stays in the tree so its children and text can be reached).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ignored {
    /// `display:none`, `hidden`, or never rendered (`head`, `script`, …), or inside such a node.
    NotRendered,
    /// `visibility: hidden` or `collapse`.
    NotVisible,
    /// `aria-hidden="true"` on the node or an ancestor.
    AriaHidden,
    /// `role="none"`/`presentation` without a conflict.
    Presentational,
    /// Outside the open modal dialog (inert, as the top layer makes it).
    Inert,
}

/// Script-made state the tree reflects: open modal dialogs (top layer) and focus.
#[derive(Debug, Clone, Copy, Default)]
pub struct Interaction<'a> {
    /// Dialogs opened with `showModal()`, oldest first.
    pub modals: &'a [NodeId],
    /// The focused element.
    pub focused: Option<NodeId>,
}

/// One accessible node.
#[derive(Debug, Clone, PartialEq)]
pub struct AxNode {
    /// The element, text node or document it describes.
    pub dom: NodeId,
    /// Role: an ARIA role (`button`, `generic`, `none`, …), `text` for text, `document` for the root.
    pub role: &'static str,
    /// Accessible name (whitespace-normalised; text content for text nodes).
    pub name: String,
    /// Value of a form control or range widget.
    pub value: Option<String>,
    /// States.
    pub states: States,
    /// Why the node is ignored, if it is.
    pub ignored: Option<Ignored>,
    /// Child indexes into [`Tree::nodes`], in document order.
    pub children: Vec<usize>,
    /// Parent index.
    pub parent: Option<usize>,
}

/// The accessibility tree of one document.
#[derive(Debug, Clone)]
pub struct Tree {
    nodes: Vec<AxNode>,
    by_dom: HashMap<NodeId, usize>,
}

/// Lookups built once per tree: elements by `id`, and `<label for>` elements by target id.
#[derive(Default)]
pub(crate) struct Index<'d> {
    /// By tree scope (the shadow root, `None` for the document) and id: ids are per tree.
    ids: HashMap<Option<NodeId>, HashMap<&'d str, NodeId>>,
    labels: HashMap<Option<NodeId>, HashMap<&'d str, Vec<NodeId>>>,
    dom: Option<&'d Dom>,
}

impl<'d> Index<'d> {
    pub(crate) fn new(dom: &'d Dom) -> Self {
        let mut ix = Self {
            dom: Some(dom),
            ..Self::default()
        };
        for e in dom.traverse(dom.root()) {
            let Edge::Enter(n) = e else { continue };
            let Some(x) = dom.get(n) else { continue };
            let scope = dom.tree_scope(n);
            if let Some(i) = x.attr("id") {
                // The first element with an id wins.
                ix.ids.entry(scope).or_default().entry(i).or_insert(n);
            }
            if x.is_html("label")
                && let Some(f) = x.attr("for")
            {
                ix.labels
                    .entry(scope)
                    .or_default()
                    .entry(f)
                    .or_default()
                    .push(n);
            }
        }
        ix
    }

    fn scope(&self, of: NodeId) -> Option<NodeId> {
        self.dom.and_then(|d| d.tree_scope(of))
    }

    /// The first element whose `id` attribute is `id`, in the tree of `from`.
    pub(crate) fn by_id(&self, from: NodeId, id: &str) -> Option<NodeId> {
        self.ids.get(&self.scope(from))?.get(id).copied()
    }

    /// `<label for=id>` elements in tree order, in the tree of `from`.
    pub(crate) fn labels_for(&self, from: NodeId, id: &str) -> &[NodeId] {
        self.labels
            .get(&self.scope(from))
            .and_then(|m| m.get(id))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// Concatenated descendant text.
pub(crate) fn text_content(dom: &Dom, id: NodeId) -> String {
    let mut s = String::new();
    for e in dom.traverse(id) {
        if let Edge::Enter(n) = e
            && let Some(NodeData::Text(t)) = dom.get(n).map(|x| &x.data)
        {
            s.push_str(t);
        }
    }
    s
}

const NEVER_RENDERED: &[&str] = &[
    "head", "script", "style", "template", "title", "meta", "link", "base", "noscript",
];

impl Tree {
    /// Builds the tree of `dom`. Pass the document's layout to judge visibility by computed
    /// style; without it (or when layout failed), only markup is used.
    pub fn build(dom: &Dom, layout: Option<&Layout>) -> Self {
        Self::build_with(dom, layout, Interaction::default())
    }

    /// [`Tree::build`] with script-made state: content outside the top modal dialog is inert,
    /// and the focused element is marked.
    pub fn build_with(dom: &Dom, layout: Option<&Layout>, interaction: Interaction<'_>) -> Self {
        let top_modal = interaction.modals.last().copied();
        let layout = layout.filter(|l| l.error().is_none());
        let ix = Index::new(dom);
        let mut nodes: Vec<AxNode> = Vec::new();
        let mut by_dom = HashMap::new();
        let mut stack: Vec<Option<usize>> = Vec::new();
        // Inherited ignore reasons, parallel to `stack`.
        let mut inherited: Vec<Option<Ignored>> = Vec::new();
        for edge in dom.traverse(dom.root()) {
            match edge {
                Edge::Enter(id) => {
                    let Some(n) = dom.get(id) else { continue };
                    let parent = stack.last().copied().flatten();
                    let parent_ignored = inherited.last().copied().flatten();
                    let entry = match &n.data {
                        NodeData::Document => Some(("document", None)),
                        NodeData::Text(t) if !t.trim().is_empty() => {
                            let own = parent.and_then(|p| nodes[p].ignored);
                            // Text takes its parent's status (presentational parents do not hide text).
                            let own = own
                                .filter(|i| *i != Ignored::Presentational)
                                .or(parent_ignored);
                            Some(("text", own))
                        }
                        NodeData::Element { .. } => {
                            let role = role::role(dom, &ix, id);
                            let inert = top_modal.is_some_and(|m| {
                                !dom.is_inclusive_ancestor(m, id)
                                    && !dom.is_inclusive_ancestor(id, m)
                            });
                            let ignored = if inert && parent_ignored != Some(Ignored::NotRendered) {
                                Some(Ignored::Inert)
                            } else {
                                element_ignored(dom, layout, id, role, parent_ignored)
                            };
                            Some((role, ignored))
                        }
                        _ => None,
                    };
                    match entry {
                        Some((role, ignored)) => {
                            let idx = nodes.len();
                            nodes.push(AxNode {
                                dom: id,
                                role,
                                name: String::new(),
                                value: None,
                                states: States::default(),
                                ignored,
                                children: Vec::new(),
                                parent,
                            });
                            if let Some(p) = parent {
                                nodes[p].children.push(idx);
                            }
                            by_dom.insert(id, idx);
                            stack.push(Some(idx));
                            // Only these reasons pass down to descendants.
                            let passes = ignored.filter(|i| {
                                matches!(
                                    i,
                                    Ignored::NotRendered | Ignored::AriaHidden | Ignored::Inert
                                )
                            });
                            inherited.push(passes.or(parent_ignored));
                        }
                        None => {
                            // Not part of the tree; keep the stacks balanced.
                            stack.push(parent);
                            inherited.push(parent_ignored);
                        }
                    }
                }
                Edge::Leave(_) => {
                    stack.pop();
                    inherited.pop();
                }
            }
        }
        let roles: HashMap<NodeId, &'static str> = nodes.iter().map(|n| (n.dom, n.role)).collect();
        let hidden: HashMap<NodeId, bool> = nodes
            .iter()
            .map(|n| {
                (
                    n.dom,
                    matches!(
                        n.ignored,
                        Some(Ignored::NotRendered | Ignored::NotVisible | Ignored::AriaHidden)
                    ),
                )
            })
            .collect();
        let role_fn = |id: NodeId| {
            roles
                .get(&id)
                .copied()
                .unwrap_or_else(|| role::role(dom, &ix, id))
        };
        let hidden_fn = |id: NodeId| hidden.get(&id).copied().unwrap_or(false);
        let namer = name::Namer {
            dom,
            ix: &ix,
            layout,
            role: &role_fn,
            hidden: &hidden_fn,
        };
        for node in &mut nodes {
            match node.role {
                "text" => {
                    if let Some(NodeData::Text(t)) = dom.get(node.dom).map(|x| &x.data) {
                        node.name = name::normalize(t);
                    }
                }
                _ if node.parent.is_none() => {
                    let title = dom.traverse(dom.root()).find_map(|e| match e {
                        Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("title")) => Some(n),
                        _ => None,
                    });
                    node.name = title
                        .map(|t| name::normalize(&text_content(dom, t)))
                        .unwrap_or_default();
                }
                role => {
                    node.name = namer.name(node.dom);
                    node.states = state::states(dom, node.dom, role);
                    node.states.modal |= interaction.modals.contains(&node.dom);
                    node.states.focused = interaction.focused == Some(node.dom);
                    node.value = state::value(dom, node.dom, role);
                }
            }
        }
        Self { nodes, by_dom }
    }

    /// All nodes; index 0 is the document.
    pub fn nodes(&self) -> &[AxNode] {
        &self.nodes
    }

    /// The node describing a DOM node (elements, non-whitespace text and the document).
    pub fn node(&self, id: NodeId) -> Option<&AxNode> {
        self.by_dom.get(&id).map(|&i| &self.nodes[i])
    }

    /// Index of the node describing a DOM node.
    pub fn index(&self, id: NodeId) -> Option<usize> {
        self.by_dom.get(&id).copied()
    }

    /// The computed role of a node.
    pub fn role(&self, id: NodeId) -> Option<&'static str> {
        self.node(id).map(|n| n.role)
    }

    /// The accessible name of a node.
    pub fn name(&self, id: NodeId) -> Option<&str> {
        self.node(id).map(|n| n.name.as_str())
    }
}

fn element_ignored(
    dom: &Dom,
    layout: Option<&Layout>,
    id: NodeId,
    role: &str,
    inherited: Option<Ignored>,
) -> Option<Ignored> {
    let n = dom.get(id)?;
    if inherited == Some(Ignored::NotRendered) {
        return inherited;
    }
    // Options belong to their select's popup: never judged by the select's box.
    let in_select = std::iter::successors(dom.parent(id), |&p| dom.parent(p)).any(|p| {
        dom.get(p)
            .is_some_and(|x| x.is_html("select") || x.is_html("datalist"))
    });
    let not_rendered = match layout {
        Some(l) if !in_select => l.visibility(id) == pageon_layout::Visibility::NotRendered,
        _ => {
            n.attr("hidden").is_some()
                || n.local_name().is_some_and(|t| {
                    NEVER_RENDERED.contains(&t)
                        && n.namespace() == Some(pageon_dom::Namespace::Html)
                })
        }
    };
    if not_rendered {
        return Some(Ignored::NotRendered);
    }
    if let Some(l) = layout.filter(|_| !in_select)
        && matches!(
            l.style(id, "visibility").as_deref(),
            Some("hidden" | "collapse")
        )
    {
        return Some(Ignored::NotVisible);
    }
    if n.attr("aria-hidden")
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
        || inherited == Some(Ignored::AriaHidden)
    {
        return Some(Ignored::AriaHidden);
    }
    if role == "none" {
        return Some(Ignored::Presentational);
    }
    None
}
