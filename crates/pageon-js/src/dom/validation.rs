//! Constraint validation (HTML §4.10.20): the API on form controls and forms, `ValidityState`,
//! `invalid` events and interactive validation. The rules themselves are
//! [`pageon_dom::forms`]'s, shared with selectors, layout and the accessibility tree.

use pageon_dom::NodeId;
use pageon_dom::forms;

use crate::cv::{Cx, Obj, R};
use crate::dom::event_target::fire;
use crate::generated::{FocusOptions, Iface};
use crate::realm::{Slot, Target};

/// A control whose validation blocked a submission (or failed `reportValidity()`), as the
/// action outcome reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// The control.
    pub node: NodeId,
    /// Its validation message.
    pub message: String,
    /// The page set the message (`setCustomValidity`, `setValidity`): page data.
    pub page_set: bool,
    /// What takes focus for it: a form-associated element's anchor (its inner field), else the
    /// control.
    pub focus: NodeId,
}

/// The `ValidityState` of an element (`[SameObject]`).
pub(crate) fn validity_obj(cx: &mut Cx<'_, '_, '_>, id: NodeId) -> Obj {
    super::same(cx, id, "validity", Iface::ValidityState, Slot::Validity(id))
}

pub(crate) fn will_validate(cx: &Cx<'_, '_, '_>, id: NodeId) -> bool {
    !forms::barred(&cx.dom(), id)
}

pub(crate) fn message(cx: &Cx<'_, '_, '_>, id: NodeId) -> String {
    let dom = cx.dom();
    if forms::barred(&dom, id) {
        return String::new();
    }
    let v = forms::validity(&dom, id);
    forms::message(&dom, id, &v)
}

fn invalid(cx: &Cx<'_, '_, '_>, id: NodeId) -> bool {
    forms::invalid(&cx.dom(), id)
}

/// Fires `invalid` (cancelable) at `id`: whether it was not cancelled.
fn fire_invalid(cx: &mut Cx<'_, '_, '_>, id: NodeId) -> R<bool> {
    fire(cx, Target::Node(id), "invalid", false, true)
}

/// `checkValidity()` of a control: fires `invalid` if it is an invalid candidate.
pub(crate) fn check(cx: &mut Cx<'_, '_, '_>, id: NodeId) -> R<bool> {
    if !invalid(cx, id) {
        return Ok(true);
    }
    fire_invalid(cx, id)?;
    Ok(false)
}

fn blocked(cx: &Cx<'_, '_, '_>, id: NodeId) -> Blocked {
    let dom = cx.dom();
    let v = forms::validity(&dom, id);
    let page_set = v.custom_error || forms::is_form_associated(&dom, id);
    Blocked {
        node: id,
        message: forms::message(&dom, id, &v),
        page_set,
        focus: dom.internals(id).and_then(|i| i.anchor).unwrap_or(id),
    }
}

/// Focuses an invalid control, or its anchor (form-associated custom elements).
fn focus_invalid(cx: &mut Cx<'_, '_, '_>, id: NodeId) -> R<()> {
    let target = cx.dom().internals(id).and_then(|i| i.anchor).unwrap_or(id);
    crate::dom::html_or_svg_or_math_ml_element::focus(
        cx,
        Obj::Node(target),
        FocusOptions::default(),
    )
}

/// Reports invalid `controls` (tree order): fires `invalid` at each, sets user validity, records
/// them for the action outcome, and focuses the first whose event was not cancelled. Whether
/// all were valid.
fn report(cx: &mut Cx<'_, '_, '_>, controls: &[NodeId]) -> R<bool> {
    let bad: Vec<NodeId> = controls
        .iter()
        .copied()
        .filter(|&c| invalid(cx, c))
        .collect();
    if bad.is_empty() {
        return Ok(true);
    }
    let mut first = None;
    for &c in &bad {
        let entry = blocked(cx, c);
        cx.st().blocked.push(entry);
        if fire_invalid(cx, c)? && first.is_none() {
            first = Some(c);
        }
    }
    for &c in controls {
        cx.dom_mut().control_mut(c).user_validity = true;
    }
    if let Some(f) = first {
        focus_invalid(cx, f)?;
    }
    Ok(false)
}

/// The submittable controls of a form, tree order.
fn controls(cx: &Cx<'_, '_, '_>, form: NodeId) -> Vec<NodeId> {
    let dom = cx.dom();
    forms::listed(&dom, form)
}

/// Interactive validation before a submission: whether the form may submit.
pub(crate) fn interactive(cx: &mut Cx<'_, '_, '_>, form: NodeId) -> R<bool> {
    let list = controls(cx, form);
    report(cx, &list)
}

/// `form.checkValidity()`: fires `invalid` at each invalid control.
pub(crate) fn check_form(cx: &mut Cx<'_, '_, '_>, form: NodeId) -> R<bool> {
    let mut ok = true;
    for c in controls(cx, form) {
        ok &= check(cx, c)?;
    }
    Ok(ok)
}

/// `form.reportValidity()`.
pub(crate) fn report_form(cx: &mut Cx<'_, '_, '_>, form: NodeId) -> R<bool> {
    let list = controls(cx, form);
    report(cx, &list)
}

/// `reportValidity()` of a control.
pub(crate) fn report_one(cx: &mut Cx<'_, '_, '_>, id: NodeId) -> R<bool> {
    report(cx, &[id])
}

/// The constraint validation API shared by `input`, `select`, `textarea`, `button`,
/// `fieldset` and `output` (re-exported into each interface's module).
pub mod api {
    use super::*;

    pub fn get_will_validate(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(will_validate(cx, this.node()?))
    }

    pub fn get_validity(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(validity_obj(cx, id))
    }

    pub fn get_validation_message(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(message(cx, this.node()?))
    }

    pub fn check_validity(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        check(cx, this.node()?)
    }

    pub fn report_validity(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        report_one(cx, this.node()?)
    }

    pub fn set_custom_validity(cx: &mut Cx<'_, '_, '_>, this: Obj, error: String) -> R<()> {
        let id = this.node()?;
        cx.dom_mut().control_mut(id).custom_error = (!error.is_empty()).then_some(error);
        Ok(())
    }

    pub fn get_form(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let id = this.node()?;
        Ok(forms::owner(&cx.dom(), id).map(Obj::Node))
    }

    /// `type` of `fieldset` and `output`: their local name.
    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        Ok(cx
            .dom()
            .get(id)
            .and_then(|n| n.local_name().map(str::to_string))
            .unwrap_or_default())
    }
}

/// `HTMLFieldSetElement`.
pub mod html_field_set_element {
    pub use super::api::*;
}

/// `HTMLOutputElement`.
pub mod html_output_element {
    pub use super::api::*;
}

/// `ValidityState`: a live view of an element's flags.
pub mod validity_state {
    use super::*;

    fn flags(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<forms::Validity> {
        let id = match cx.st().slot(this.slab()?) {
            Some(Slot::Validity(id)) => *id,
            _ => return Err(crate::cv::Error::Type("illegal invocation".into())),
        };
        Ok(forms::validity(&cx.dom(), id))
    }

    macro_rules! flag {
        ($($get:ident => $field:ident),* $(,)?) => {$(
            pub fn $get(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
                Ok(flags(cx, this)?.$field)
            }
        )*};
    }

    flag!(
        get_value_missing => value_missing,
        get_type_mismatch => type_mismatch,
        get_pattern_mismatch => pattern_mismatch,
        get_too_long => too_long,
        get_too_short => too_short,
        get_range_underflow => range_underflow,
        get_range_overflow => range_overflow,
        get_step_mismatch => step_mismatch,
        get_bad_input => bad_input,
        get_custom_error => custom_error,
    );

    pub fn get_valid(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(flags(cx, this)?.valid())
    }
}
