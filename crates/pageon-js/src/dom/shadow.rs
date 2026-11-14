//! Shadow DOM bindings: `attachShadow`, `shadowRoot`, the `ShadowRoot` interface, slots and
//! `assignedSlot`. The trees themselves live in pageon-dom.

use pageon_dom::{NodeData, NodeId, ShadowInit, ShadowMode, SlotAssignment};

use crate::cv::{Cx, Error, Obj, R};
use crate::generated::{AssignedNodesOptions, ShadowRootInit};

fn root_data(cx: &Cx<'_, '_, '_>, this: Obj) -> R<NodeData> {
    let id = this.node()?;
    match cx.dom().get(id).map(|n| n.data.clone()) {
        Some(d @ NodeData::ShadowRoot { .. }) => Ok(d),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// `Element` members.
pub(crate) mod element_shadow {
    use super::*;

    pub fn attach_shadow(cx: &mut Cx<'_, '_, '_>, this: Obj, init: ShadowRootInit) -> R<Obj> {
        let host = this.node()?;
        let mode = match init.mode.as_deref() {
            Some("open") => ShadowMode::Open,
            Some("closed") => ShadowMode::Closed,
            _ => {
                return Err(Error::Type(
                    "Failed to execute 'attachShadow' on 'Element': The provided value is not a valid enum value of type ShadowRootMode.".into(),
                ));
            }
        };
        let init = ShadowInit {
            mode,
            delegates_focus: init.delegates_focus,
            clonable: init.clonable,
            serializable: init.serializable,
            slot_assignment: if init.slot_assignment == "manual" {
                SlotAssignment::Manual
            } else {
                SlotAssignment::Named
            },
        };
        let r = cx.dom_mut().attach_shadow(host, init);
        match r {
            Ok(root) => Ok(Obj::Node(root)),
            Err(pageon_dom::AttachError::NotAHost) => Err(Error::dom(
                "NotSupportedError",
                "Failed to execute 'attachShadow' on 'Element': This element does not support attachShadow",
            )),
            Err(pageon_dom::AttachError::Attached) => Err(Error::dom(
                "NotSupportedError",
                "Failed to execute 'attachShadow' on 'Element': Shadow root cannot be created on a host which already hosts a shadow tree.",
            )),
        }
    }

    /// The open shadow root (closed roots are hidden from scripts).
    pub fn get_shadow_root(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let host = this.node()?;
        let dom = cx.dom();
        Ok(dom
            .shadow_root(host)
            .filter(|&r| {
                matches!(
                    dom.get(r).map(|n| &n.data),
                    Some(NodeData::ShadowRoot {
                        mode: ShadowMode::Open,
                        ..
                    })
                )
            })
            .map(Obj::Node))
    }
}

/// `ShadowRoot`.
pub mod shadow_root {
    use super::*;

    pub fn get_mode(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        match root_data(cx, this)? {
            NodeData::ShadowRoot {
                mode: ShadowMode::Closed,
                ..
            } => Ok("closed".into()),
            _ => Ok("open".into()),
        }
    }

    pub fn get_delegates_focus(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(matches!(
            root_data(cx, this)?,
            NodeData::ShadowRoot {
                delegates_focus: true,
                ..
            }
        ))
    }

    pub fn get_clonable(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(matches!(
            root_data(cx, this)?,
            NodeData::ShadowRoot { clonable: true, .. }
        ))
    }

    pub fn get_serializable(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(matches!(
            root_data(cx, this)?,
            NodeData::ShadowRoot {
                serializable: true,
                ..
            }
        ))
    }

    pub fn get_slot_assignment(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        match root_data(cx, this)? {
            NodeData::ShadowRoot {
                slot_assignment: SlotAssignment::Manual,
                ..
            } => Ok("manual".into()),
            _ => Ok("named".into()),
        }
    }

    pub fn get_host(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        match root_data(cx, this)? {
            NodeData::ShadowRoot { host, .. } => Ok(Obj::Node(host)),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_inner_html(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        let dom = cx.dom();
        Ok(dom.children(id).map(|c| dom.to_html(c)).collect())
    }

    /// Parsed in the context of the host, as HTML specifies.
    pub fn set_inner_html(cx: &mut Cx<'_, '_, '_>, this: Obj, html: String) -> R<()> {
        let root = this.node()?;
        let NodeData::ShadowRoot { host, .. } = root_data(cx, this)? else {
            return Err(Error::Type("Illegal invocation".into()));
        };
        let local = cx
            .dom()
            .get(host)
            .and_then(|n| n.local_name().map(str::to_string))
            .unwrap_or_else(|| "div".into());
        let nodes = crate::dom::parse_fragment_into(cx, &local, false, &html);
        let kids: Vec<NodeId> = cx.dom().children(root).collect();
        for k in kids {
            crate::dom::detach(cx, k)?;
        }
        for n in nodes {
            crate::dom::insert(cx, root, n, None)?;
        }
        Ok(())
    }
}

/// `HTMLSlotElement`.
pub mod html_slot_element {
    use super::*;

    /// `new HTMLSlotElement()` is not a custom element: construct through the registry.
    pub fn assigned_nodes(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        o: AssignedNodesOptions,
    ) -> R<Vec<Obj>> {
        let slot = this.node()?;
        Ok(cx
            .dom()
            .assigned_nodes(slot, o.flatten)
            .into_iter()
            .filter(|&n| {
                // Without `flatten`, an unassigned slot gives nothing (its fallback is not
                // assigned).
                o.flatten || cx.dom().assigned_slot(n) == Some(slot)
            })
            .map(Obj::Node)
            .collect())
    }

    pub fn assigned_elements(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        o: AssignedNodesOptions,
    ) -> R<Vec<Obj>> {
        let nodes = assigned_nodes(cx, this, o)?;
        let dom = cx.dom();
        Ok(nodes
            .into_iter()
            .filter(|o| {
                o.node()
                    .ok()
                    .and_then(|n| dom.get(n))
                    .is_some_and(|n| matches!(n.data, NodeData::Element { .. }))
            })
            .collect())
    }
}

/// The `Slottable` mixin (`Element`, `Text`).
pub mod slottable {
    use super::*;

    /// The slot this node is assigned to, if that slot is in an open shadow tree.
    pub fn get_assigned_slot(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let id = this.node()?;
        let dom = cx.dom();
        let Some(slot) = dom.assigned_slot(id) else {
            return Ok(None);
        };
        let open = matches!(
            dom.get(dom.root_of(slot)).map(|n| &n.data),
            Some(NodeData::ShadowRoot {
                mode: ShadowMode::Open,
                ..
            })
        );
        Ok(open.then_some(Obj::Node(slot)))
    }
}

/// Whether `a` is a shadow-including inclusive ancestor of `b`.
pub(crate) fn includes(dom: &pageon_dom::Dom, a: NodeId, b: NodeId) -> bool {
    let mut cur = Some(b);
    while let Some(c) = cur {
        if c == a {
            return true;
        }
        cur = dom.shadow_parent(c);
    }
    false
}

/// DOM's "retarget A against B": A, or the host of its shadow tree, repeatedly, until A's
/// tree contains B.
pub(crate) fn retarget(dom: &pageon_dom::Dom, a: NodeId, b: NodeId) -> NodeId {
    let mut a = a;
    loop {
        let root = dom.root_of(a);
        match dom.host(root) {
            Some(h) if !includes(dom, root, b) => a = h,
            _ => return a,
        }
    }
}

/// DOM's "closed-shadow-hidden": `a` is in a closed shadow tree that `b` is not inside.
pub(crate) fn closed_hidden(dom: &pageon_dom::Dom, a: NodeId, b: NodeId) -> bool {
    let root = dom.root_of(a);
    let Some(NodeData::ShadowRoot { host, mode, .. }) = dom.get(root).map(|n| n.data.clone())
    else {
        return false;
    };
    !includes(dom, root, b) && (mode == ShadowMode::Closed || closed_hidden(dom, host, b))
}

/// Fires `slotchange` at slots whose assigned nodes changed since the last check (HTML's
/// "signal a slot change", delivered at the end of the task). Returns whether any fired.
pub(crate) fn deliver_slotchange(cx: &mut Cx<'_, '_, '_>) -> R<bool> {
    if !cx.dom().has_shadow() {
        return Ok(false);
    }
    let now: Vec<(NodeId, Vec<NodeId>)> = {
        let dom = cx.dom();
        let mut roots: Vec<NodeId> = Vec::new();
        for e in dom.traverse(dom.root()) {
            if let pageon_dom::Edge::Enter(n) = e
                && let Some(r) = dom.shadow_root(n)
            {
                roots.push(r);
            }
        }
        let mut i = 0;
        while i < roots.len() {
            let r = roots[i];
            for e in dom.traverse(r) {
                if let pageon_dom::Edge::Enter(n) = e
                    && let Some(s) = dom.shadow_root(n)
                {
                    roots.push(s);
                }
            }
            i += 1;
        }
        roots
            .into_iter()
            .flat_map(|r| {
                dom.traverse(r)
                    .filter_map(|e| match e {
                        pageon_dom::Edge::Enter(n)
                            if dom.get(n).is_some_and(|x| x.is_html("slot")) =>
                        {
                            Some(n)
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .map(|s| (s, dom.assigned_nodes(s, false)))
            .collect()
    };
    let changed: Vec<NodeId> = {
        let mut st = cx.st();
        let mut changed = Vec::new();
        for (slot, nodes) in now {
            let before = st.slots.insert(slot, nodes.clone());
            // A slot seen for the first time with nodes counts as a change (they were just
            // assigned), as in browsers.
            if before.unwrap_or_default() != nodes {
                changed.push(slot);
            }
        }
        changed
    };
    for &s in &changed {
        crate::dom::event_target::fire(
            cx,
            crate::realm::Target::Node(s),
            "slotchange",
            true,
            false,
        )?;
    }
    Ok(!changed.is_empty())
}
