//! Form-associated custom elements (HTML §4.13.7): `attachInternals()`, `ElementInternals`,
//! `CustomStateSet` and default ARIA semantics. The state lives in the DOM
//! ([`pageon_dom::forms::Internals`]) so selectors, layout, the accessibility tree and form
//! submission read it without a channel.

use pageon_dom::NodeId;
use pageon_dom::forms;

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{Iface, ValidityStateFlags};
use crate::realm::Slot;

/// Most custom states one element keeps (SR-003).
pub const MAX_STATES: usize = 1024;
/// Most bytes of form values and adopted stylesheet text one document keeps (SR-003).
pub const MAX_SIDE_BYTES: usize = 16 << 20;

/// Bytes the document's side tables hold (form values and adopted sheets).
pub(crate) fn side_bytes(cx: &Cx<'_, '_, '_>) -> usize {
    let dom = cx.dom();
    let sheets: usize = cx.st().sheets.bytes();
    let values: usize = dom
        .internals_values()
        .map(|(k, v)| k.as_ref().map_or(0, String::len) + v.len())
        .sum();
    sheets + values
}

/// Reports a refused write once per kind (`kind` names it in the message).
pub(crate) fn refuse(cx: &mut Cx<'_, '_, '_>, kind: &'static str, msg: String) {
    if cx.st().refused.insert(kind) {
        cx.realm
            .shared
            .borrow_mut()
            .error(crate::PageErrorKind::Exception, msg, kind);
    }
}

/// The element behind an `ElementInternals` or `CustomStateSet`.
fn element(cx: &Cx<'_, '_, '_>, this: Obj) -> R<NodeId> {
    match cx.st().slot(this.slab()?) {
        Some(Slot::Internals(el) | Slot::States(el)) => Ok(*el),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// The element of a form-associated `ElementInternals`, or `NotSupportedError`.
fn form_element(cx: &Cx<'_, '_, '_>, this: Obj) -> R<NodeId> {
    let el = element(cx, this)?;
    if forms::is_form_associated(&cx.dom(), el) {
        Ok(el)
    } else {
        Err(Error::dom(
            "NotSupportedError",
            "The target element is not a form-associated custom element.",
        ))
    }
}

/// `HTMLElement.attachInternals()`.
pub(crate) fn attach(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<Obj> {
    let fail = |m: &str| {
        Err(Error::dom(
            "NotSupportedError",
            format!("Failed to execute 'attachInternals' on 'HTMLElement': {m}"),
        ))
    };
    match crate::dom::custom_elements::internals_allowed(cx, el) {
        None => {
            return fail(
                "Unable to attach ElementInternals to non-custom elements or to elements not yet upgraded.",
            );
        }
        Some(false) => {
            return fail("ElementInternals is disabled by disabledFeature static field.");
        }
        Some(true) => {}
    }
    if cx.dom().internals(el).is_some_and(|i| i.attached) {
        return fail("ElementInternals for the specified element was already attached.");
    }
    cx.dom_mut().internals_mut(el).attached = true;
    Ok(super::same(
        cx,
        el,
        "internals",
        Iface::ElementInternals,
        Slot::Internals(el),
    ))
}

/// `ElementInternals`.
pub mod element_internals {
    use super::*;

    pub fn get_shadow_root(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let el = element(cx, this)?;
        Ok(cx.dom().shadow_root(el).map(Obj::Node))
    }

    pub fn set_form_value(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        value: Option<Value>,
        _state: Option<Option<Value>>,
    ) -> R<()> {
        let el = form_element(cx, this)?;
        let entries: Vec<(Option<String>, String)> = match value {
            None => Vec::new(),
            Some(v) => {
                let local = cx.local(&v);
                if local.is_string() {
                    vec![(None, local.to_rust_string_lossy(cx.scope))]
                } else if let Ok(Obj::Slab(id)) = crate::cv::obj(cx, local, Iface::FormData) {
                    crate::dom::form_data::pairs(cx, Obj::Slab(id))?
                        .into_iter()
                        .map(|(k, v)| (Some(k), v))
                        .collect()
                } else if local.is_object() {
                    return Err(Error::dom(
                        "NotSupportedError",
                        "Failed to execute 'setFormValue' on 'ElementInternals': file values are not supported.",
                    ));
                } else {
                    vec![(None, crate::cv::string(cx, local)?)]
                }
            }
        };
        let old: usize = cx.dom().internals(el).map_or(0, |i| {
            i.value
                .iter()
                .map(|(k, v)| k.as_ref().map_or(0, String::len) + v.len())
                .sum()
        });
        let new: usize = entries
            .iter()
            .map(|(k, v)| k.as_ref().map_or(0, String::len) + v.len())
            .sum();
        if side_bytes(cx) - old + new > MAX_SIDE_BYTES {
            refuse(
                cx,
                "values",
                "setFormValue: the document's form values and stylesheets exceed 16 MiB; the value is not kept".into(),
            );
            return Ok(());
        }
        cx.dom_mut().internals_mut(el).value = entries;
        Ok(())
    }

    pub fn get_form(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let el = form_element(cx, this)?;
        Ok(forms::owner(&cx.dom(), el).map(Obj::Node))
    }

    pub fn set_validity(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        flags: ValidityStateFlags,
        message: Option<String>,
        anchor: Option<Obj>,
    ) -> R<()> {
        let el = form_element(cx, this)?;
        let v = forms::Validity {
            value_missing: flags.value_missing,
            type_mismatch: flags.type_mismatch,
            pattern_mismatch: flags.pattern_mismatch,
            too_long: flags.too_long,
            too_short: flags.too_short,
            range_underflow: flags.range_underflow,
            range_overflow: flags.range_overflow,
            step_mismatch: flags.step_mismatch,
            bad_input: flags.bad_input,
            custom_error: flags.custom_error,
        };
        let message = message.unwrap_or_default();
        if !v.valid() && message.is_empty() {
            return Err(Error::Type(
                "Failed to execute 'setValidity' on 'ElementInternals': The second argument should not be empty if one or more flags in the first argument are true.".into(),
            ));
        }
        let anchor = anchor.map(Obj::node).transpose()?;
        if let Some(a) = anchor {
            let inside = {
                let dom = cx.dom();
                let mut cur = Some(a);
                let mut found = false;
                while let Some(c) = cur {
                    if c == el {
                        found = c != a;
                        break;
                    }
                    cur = dom.parent(c).or_else(|| dom.host(c));
                }
                found
            };
            if !inside {
                return Err(Error::dom(
                    "NotFoundError",
                    "Failed to execute 'setValidity' on 'ElementInternals': The Element argument should be a shadow-including descendant of the target element.",
                ));
            }
        }
        let mut dom = cx.dom_mut();
        let i = dom.internals_mut(el);
        i.validity = v;
        i.message = if v.valid() { String::new() } else { message };
        i.anchor = anchor;
        Ok(())
    }

    pub fn get_will_validate(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        let el = form_element(cx, this)?;
        Ok(crate::dom::validation::will_validate(cx, el))
    }

    pub fn get_validity(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let el = form_element(cx, this)?;
        Ok(crate::dom::validation::validity_obj(cx, el))
    }

    pub fn get_validation_message(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let el = form_element(cx, this)?;
        Ok(crate::dom::validation::message(cx, el))
    }

    pub fn check_validity(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        let el = form_element(cx, this)?;
        crate::dom::validation::check(cx, el)
    }

    pub fn report_validity(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        let el = form_element(cx, this)?;
        crate::dom::validation::report_one(cx, el)
    }

    pub fn get_labels(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let el = form_element(cx, this)?;
        let labels = crate::dom::html_input_element::labels_of(cx, el);
        Ok(cx.alloc(Iface::NodeList, Slot::Static(labels)))
    }

    pub fn get_states(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let el = element(cx, this)?;
        Ok(super::super::same(
            cx,
            el,
            "states",
            Iface::CustomStateSet,
            Slot::States(el),
        ))
    }
}

/// `CustomStateSet` (`setlike<DOMString>`).
pub mod custom_state_set {
    use super::*;

    fn states(cx: &Cx<'_, '_, '_>, el: NodeId) -> Vec<String> {
        cx.dom()
            .internals(el)
            .map(|i| i.states.clone())
            .unwrap_or_default()
    }

    pub fn pairs(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<(String, String)>> {
        let el = element(cx, this)?;
        Ok(states(cx, el).into_iter().map(|s| (s.clone(), s)).collect())
    }

    pub fn set_size(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        let el = element(cx, this)?;
        Ok(u32::try_from(states(cx, el).len()).unwrap_or(u32::MAX))
    }

    pub fn set_has(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<bool> {
        let el = element(cx, this)?;
        Ok(states(cx, el).contains(&v))
    }

    pub fn set_add(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let el = element(cx, this)?;
        let full = {
            let mut dom = cx.dom_mut();
            let i = dom.internals_mut(el);
            if i.states.contains(&v) {
                return Ok(());
            }
            if i.states.len() >= MAX_STATES {
                true
            } else {
                i.states.push(v);
                false
            }
        };
        if full {
            refuse(
                cx,
                "states",
                format!(
                    "CustomStateSet: an element keeps at most {MAX_STATES} states; further states are dropped"
                ),
            );
        }
        Ok(())
    }

    pub fn set_delete(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<bool> {
        let el = element(cx, this)?;
        let mut dom = cx.dom_mut();
        let i = dom.internals_mut(el);
        let before = i.states.len();
        i.states.retain(|s| *s != v);
        Ok(i.states.len() != before)
    }

    pub fn set_clear(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let el = element(cx, this)?;
        cx.dom_mut().internals_mut(el).states.clear();
        Ok(())
    }
}

/// `ARIAMixin` on `ElementInternals`: default ARIA semantics (on elements the same properties
/// reflect attributes and are generated).
pub mod aria_mixin {
    use super::*;

    fn get(cx: &Cx<'_, '_, '_>, this: Obj, name: &str) -> R<Option<String>> {
        let el = element(cx, this)?;
        Ok(cx.dom().default_aria(el, name).map(str::to_string))
    }

    fn set(cx: &mut Cx<'_, '_, '_>, this: Obj, name: &str, v: Option<String>) -> R<()> {
        let el = element(cx, this)?;
        let mut dom = cx.dom_mut();
        let aria = &mut dom.internals_mut(el).aria;
        aria.retain(|(k, _)| k != name);
        if let Some(v) = v {
            aria.push((name.to_string(), v));
        }
        Ok(())
    }

    macro_rules! aria {
        ($($get:ident, $set:ident => $attr:literal;)*) => {$(
            pub fn $get(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
                get(cx, this, $attr)
            }
            pub fn $set(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<String>) -> R<()> {
                set(cx, this, $attr, v)
            }
        )*};
    }

    aria! {
        get_role, set_role => "role";
        get_aria_atomic, set_aria_atomic => "aria-atomic";
        get_aria_auto_complete, set_aria_auto_complete => "aria-autocomplete";
        get_aria_braille_label, set_aria_braille_label => "aria-braillelabel";
        get_aria_braille_role_description, set_aria_braille_role_description => "aria-brailleroledescription";
        get_aria_busy, set_aria_busy => "aria-busy";
        get_aria_checked, set_aria_checked => "aria-checked";
        get_aria_col_count, set_aria_col_count => "aria-colcount";
        get_aria_col_index, set_aria_col_index => "aria-colindex";
        get_aria_col_index_text, set_aria_col_index_text => "aria-colindextext";
        get_aria_col_span, set_aria_col_span => "aria-colspan";
        get_aria_current, set_aria_current => "aria-current";
        get_aria_description, set_aria_description => "aria-description";
        get_aria_disabled, set_aria_disabled => "aria-disabled";
        get_aria_expanded, set_aria_expanded => "aria-expanded";
        get_aria_has_popup, set_aria_has_popup => "aria-haspopup";
        get_aria_hidden, set_aria_hidden => "aria-hidden";
        get_aria_invalid, set_aria_invalid => "aria-invalid";
        get_aria_key_shortcuts, set_aria_key_shortcuts => "aria-keyshortcuts";
        get_aria_label, set_aria_label => "aria-label";
        get_aria_level, set_aria_level => "aria-level";
        get_aria_live, set_aria_live => "aria-live";
        get_aria_modal, set_aria_modal => "aria-modal";
        get_aria_multi_line, set_aria_multi_line => "aria-multiline";
        get_aria_multi_selectable, set_aria_multi_selectable => "aria-multiselectable";
        get_aria_orientation, set_aria_orientation => "aria-orientation";
        get_aria_placeholder, set_aria_placeholder => "aria-placeholder";
        get_aria_pos_in_set, set_aria_pos_in_set => "aria-posinset";
        get_aria_pressed, set_aria_pressed => "aria-pressed";
        get_aria_read_only, set_aria_read_only => "aria-readonly";
        get_aria_relevant, set_aria_relevant => "aria-relevant";
        get_aria_required, set_aria_required => "aria-required";
        get_aria_role_description, set_aria_role_description => "aria-roledescription";
        get_aria_row_count, set_aria_row_count => "aria-rowcount";
        get_aria_row_index, set_aria_row_index => "aria-rowindex";
        get_aria_row_index_text, set_aria_row_index_text => "aria-rowindextext";
        get_aria_row_span, set_aria_row_span => "aria-rowspan";
        get_aria_selected, set_aria_selected => "aria-selected";
        get_aria_set_size, set_aria_set_size => "aria-setsize";
        get_aria_sort, set_aria_sort => "aria-sort";
        get_aria_value_max, set_aria_value_max => "aria-valuemax";
        get_aria_value_min, set_aria_value_min => "aria-valuemin";
        get_aria_value_now, set_aria_value_now => "aria-valuenow";
        get_aria_value_text, set_aria_value_text => "aria-valuetext";
    }
}
