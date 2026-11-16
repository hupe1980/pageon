//! The Popover API: `popover` elements shown and hidden by script, by invoker buttons
//! (`popovertarget`), and dismissed by Escape or a click outside. Open popovers are kept on
//! the DOM, so layout shows them (`:popover-open`) and views list exactly the open ones.

use pageon_dom::NodeId;

use crate::cv::{Cx, Error, Obj, R};

/// The popover state of an element: `auto`, `manual`, `hint`, or none.
pub(crate) fn state(cx: &Cx<'_, '_, '_>, id: NodeId) -> Option<&'static str> {
    let v = cx.dom().get(id)?.attr("popover")?.to_ascii_lowercase();
    Some(match v.as_str() {
        "" | "auto" => "auto",
        "hint" => "hint",
        _ => "manual",
    })
}

fn connected(cx: &Cx<'_, '_, '_>, id: NodeId) -> bool {
    let dom = cx.dom();
    dom.is_inclusive_ancestor(dom.root(), id)
}

/// Shows `id`; with `throw`, a state that forbids it is an error (as `showPopover()` is).
pub(crate) fn show(
    cx: &mut Cx<'_, '_, '_>,
    id: NodeId,
    throw: bool,
    source: Option<NodeId>,
) -> R<()> {
    let Some(kind) = state(cx, id) else {
        return if throw {
            Err(Error::dom(
                "NotSupportedError",
                "The element is not a popover.",
            ))
        } else {
            Ok(())
        };
    };
    if cx.dom().popover_open(id) {
        return Ok(());
    }
    if !connected(cx, id) {
        return if throw {
            Err(Error::dom(
                "InvalidStateError",
                "The popover is not connected.",
            ))
        } else {
            Ok(())
        };
    }
    if !crate::dom::toggle_event::fire(cx, id, "beforetoggle", ("closed", "open"), true, source)? {
        return Ok(());
    }
    if kind != "manual" {
        // An auto (or hint) popover closes the open ones it is not inside.
        let others: Vec<NodeId> = cx
            .dom()
            .open_popovers()
            .into_iter()
            .filter(|&p| p != id && !cx.dom().is_inclusive_ancestor(p, id))
            .filter(|&p| state(cx, p) != Some("manual"))
            .collect();
        for p in others.into_iter().rev() {
            hide(cx, p, source)?;
        }
    }
    cx.dom_mut().set_popover_open(id, true);
    crate::dom::toggle_event::fire(cx, id, "toggle", ("closed", "open"), false, source)?;
    Ok(())
}

/// Hides `id` and the popovers opened inside it after it.
pub(crate) fn hide(cx: &mut Cx<'_, '_, '_>, id: NodeId, source: Option<NodeId>) -> R<()> {
    if !cx.dom().popover_open(id) {
        return Ok(());
    }
    let inside: Vec<NodeId> = cx
        .dom()
        .open_popovers()
        .into_iter()
        .filter(|&p| p != id && cx.dom().is_inclusive_ancestor(id, p))
        .collect();
    for p in inside.into_iter().rev() {
        hide(cx, p, source)?;
    }
    crate::dom::toggle_event::fire(cx, id, "beforetoggle", ("open", "closed"), false, source)?;
    cx.dom_mut().set_popover_open(id, false);
    crate::dom::toggle_event::fire(cx, id, "toggle", ("open", "closed"), false, source)?;
    Ok(())
}

/// Toggles `id` (or forces a state); returns whether it is open afterwards.
pub(crate) fn toggle(
    cx: &mut Cx<'_, '_, '_>,
    id: NodeId,
    force: Option<bool>,
    source: Option<NodeId>,
) -> R<bool> {
    if state(cx, id).is_none() {
        return Err(Error::dom(
            "NotSupportedError",
            "The element is not a popover.",
        ));
    }
    let open = cx.dom().popover_open(id);
    match force.unwrap_or(!open) {
        true => show(cx, id, true, source)?,
        false => hide(cx, id, source)?,
    }
    Ok(cx.dom().popover_open(id))
}

/// Light dismiss: a press on `target` hides the open auto popovers that do not contain it
/// (and are not the popover its invoker controls).
pub(crate) fn light_dismiss(cx: &mut Cx<'_, '_, '_>, target: NodeId) -> R<()> {
    let invoked = invoker_target(cx, target).map(|(p, _)| p);
    let close: Vec<NodeId> = cx
        .dom()
        .open_popovers()
        .into_iter()
        .filter(|&p| state(cx, p) != Some("manual"))
        .filter(|&p| !cx.dom().is_inclusive_ancestor(p, target) && Some(p) != invoked)
        .collect();
    for p in close.into_iter().rev() {
        hide(cx, p, None)?;
    }
    Ok(())
}

/// Escape: hides the topmost open auto popover; true when it hid one.
pub(crate) fn escape(cx: &mut Cx<'_, '_, '_>) -> R<bool> {
    let top = cx
        .dom()
        .open_popovers()
        .into_iter()
        .rev()
        .find(|&p| state(cx, p) != Some("manual"));
    match top {
        Some(p) => {
            hide(cx, p, None)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// The popover a button or input controls (`popovertarget`) and its action, if any.
fn invoker_target(cx: &Cx<'_, '_, '_>, el: NodeId) -> Option<(NodeId, String)> {
    let dom = cx.dom();
    let n = dom.get(el)?;
    if !(n.is_html("button") || n.is_html("input")) {
        return None;
    }
    let id = n.attr("popovertarget")?;
    let action = n
        .attr("popovertargetaction")
        .map(str::to_ascii_lowercase)
        .filter(|a| a == "show" || a == "hide")
        .unwrap_or_else(|| "toggle".into());
    let target = dom.traverse(dom.root()).find_map(|e| match e {
        pageon_dom::Edge::Enter(t) if dom.get(t).and_then(|x| x.attr("id")) == Some(id) => Some(t),
        _ => None,
    })?;
    drop(dom);
    state(cx, target).map(|_| (target, action))
}

/// A button's activation with `popovertarget`: shows, hides or toggles its popover; true
/// when it did.
pub(crate) fn invoke(cx: &mut Cx<'_, '_, '_>, invoker: NodeId) -> R<bool> {
    let Some((target, action)) = invoker_target(cx, invoker) else {
        return Ok(false);
    };
    let open = cx.dom().popover_open(target);
    match action.as_str() {
        "show" if !open => show(cx, target, false, Some(invoker))?,
        "hide" if open => hide(cx, target, Some(invoker))?,
        "toggle" if open => hide(cx, target, Some(invoker))?,
        "toggle" => show(cx, target, false, Some(invoker))?,
        _ => {}
    }
    Ok(true)
}

/// `togglePopover(force)` and `togglePopover({force, source})`.
pub(crate) fn toggle_popover_any(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, args| {
        let this = cx.this(args, crate::generated::Iface::HTMLElement)?;
        let a = args.get(0);
        let (force, source) = if a.is_undefined() {
            (None, None)
        } else if a.is_boolean() {
            (Some(a.boolean_value(cx.scope)), None)
        } else if let Some(o) = a.to_object(cx.scope) {
            let k =
                v8::String::new(cx.scope, "force").unwrap_or_else(|| v8::String::empty(cx.scope));
            let f = o.get(cx.scope, k.into()).filter(|v| !v.is_undefined());
            let force = f.map(|v| v.boolean_value(cx.scope));
            let k =
                v8::String::new(cx.scope, "source").unwrap_or_else(|| v8::String::empty(cx.scope));
            let s = o.get(cx.scope, k.into()).filter(|v| v.is_object());
            let source = match s {
                Some(v) => crate::cv::unwrap_any(cx, v).and_then(|(o, _)| o.node().ok()),
                None => None,
            };
            (force, source)
        } else {
            (Some(a.boolean_value(cx.scope)), None)
        };
        let open = toggle(cx, this.node()?, force, source)?;
        cx.ret(open)
    });
}

/// `popoverTargetElement` and `popoverTargetAction` on buttons and inputs.
pub mod popover_target_attributes {
    use super::*;

    pub fn get_popover_target_element(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let id = this.node()?;
        let want = cx
            .dom()
            .get(id)
            .and_then(|n| n.attr("popovertarget").map(str::to_string));
        let Some(want) = want else { return Ok(None) };
        let dom = cx.dom();
        Ok(dom
            .traverse(dom.root())
            .find_map(|e| match e {
                pageon_dom::Edge::Enter(t)
                    if dom.get(t).and_then(|x| x.attr("id")) == Some(want.as_str()) =>
                {
                    Some(t)
                }
                _ => None,
            })
            .map(Obj::Node))
    }

    pub fn set_popover_target_element(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<Obj>) -> R<()> {
        let id = this.node()?;
        match v.map(Obj::node).transpose()? {
            Some(t) => {
                let tid = cx
                    .dom()
                    .get(t)
                    .and_then(|n| n.attr("id").map(str::to_string));
                let tid = tid.unwrap_or_default();
                cx.dom_mut().set_attr(id, "popovertarget", &tid)
            }
            None => cx.dom_mut().remove_attr(id, "popovertarget"),
        }
        .map_err(|_| crate::dom::gone())
    }

    pub fn get_popover_target_action(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let v = cx
            .dom()
            .get(this.node()?)
            .and_then(|n| n.attr("popovertargetaction").map(str::to_ascii_lowercase));
        Ok(match v.as_deref() {
            Some(a @ ("show" | "hide")) => a.to_string(),
            _ => "toggle".into(),
        })
    }

    pub fn set_popover_target_action(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "popovertargetaction", &v)
            .map_err(|_| crate::dom::gone())
    }
}
