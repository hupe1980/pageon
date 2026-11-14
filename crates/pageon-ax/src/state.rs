//! States and values of accessible nodes (ARIA attributes over native HTML state).

use pageon_dom::{Dom, NodeId};

/// A checkbox-like state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checked {
    /// Not checked.
    False,
    /// Checked.
    True,
    /// Partially checked (`aria-checked="mixed"`).
    Mixed,
}

/// States of an accessible node.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct States {
    /// Checked state of checkbox-like roles.
    pub checked: Option<Checked>,
    /// Disabled (natively or by `aria-disabled`).
    pub disabled: bool,
    /// Expanded (`Some(false)` = collapsed).
    pub expanded: Option<bool>,
    /// Selected (options, tabs, rows).
    pub selected: bool,
    /// Required.
    pub required: bool,
    /// Invalid (`aria-invalid` other than `false`).
    pub invalid: bool,
    /// Heading level.
    pub level: Option<u8>,
    /// Modal dialog (`aria-modal="true"`, or opened with `showModal()`).
    pub modal: bool,
    /// Focused.
    pub focused: bool,
}

fn is(dom: &Dom, id: NodeId, local: &str) -> bool {
    dom.get(id).is_some_and(|n| n.is_html(local))
}

fn aria_true(dom: &Dom, id: NodeId, attr: &str) -> bool {
    dom.aria(id, attr)
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
}

/// The selected `<option>`s of a `<select>`: those with `selected`; for a single-select box with
/// none, the first enabled option (as browsers show it).
pub(crate) fn selected_options(dom: &Dom, select: NodeId) -> Vec<NodeId> {
    let options: Vec<NodeId> = dom
        .traverse(select)
        .filter_map(|e| match e {
            pageon_dom::Edge::Enter(id) if is(dom, id, "option") => Some(id),
            _ => None,
        })
        .collect();
    let multiple = dom
        .get(select)
        .is_some_and(|n| n.attr("multiple").is_some());
    let chosen: Vec<NodeId> = options
        .iter()
        .copied()
        .filter(|&o| {
            dom.control(o)
                .and_then(|c| c.selected)
                .unwrap_or_else(|| dom.get(o).is_some_and(|n| n.attr("selected").is_some()))
        })
        .collect();
    if multiple {
        return chosen;
    }
    if let Some(&last) = chosen.last() {
        return vec![last]; // the last `selected` wins in a single-select box
    }
    let size = dom
        .get(select)
        .and_then(|n| n.attr("size"))
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if size > 1 {
        return Vec::new();
    }
    options
        .into_iter()
        .find(|&o| dom.get(o).is_some_and(|n| n.attr("disabled").is_none()))
        .into_iter()
        .collect()
}

fn natively_disabled(dom: &Dom, id: NodeId) -> bool {
    let Some(n) = dom.get(id) else { return false };
    let form_control = [
        "button", "input", "select", "textarea", "optgroup", "option", "fieldset",
    ]
    .iter()
    .any(|t| n.is_html(t));
    if !form_control {
        return false;
    }
    if n.attr("disabled").is_some() {
        return true;
    }
    if n.is_html("option") {
        if let Some(p) = dom.parent(id).filter(|&p| is(dom, p, "optgroup")) {
            return dom.get(p).is_some_and(|g| g.attr("disabled").is_some());
        }
        return false;
    }
    // Inside a disabled fieldset, except within its first legend.
    let mut child = id;
    let mut cur = dom.parent(id);
    while let Some(p) = cur {
        if is(dom, p, "fieldset") && dom.get(p).is_some_and(|f| f.attr("disabled").is_some()) {
            let first_legend = dom.children(p).find(|&c| is(dom, c, "legend"));
            if first_legend != Some(child) {
                return true;
            }
        }
        child = p;
        cur = dom.parent(p);
    }
    false
}

/// States of element `id` with computed `role`.
pub(crate) fn states(dom: &Dom, id: NodeId, role: &str) -> States {
    let Some(n) = dom.get(id) else {
        return States::default();
    };
    let mut s = States::default();
    let checkable = matches!(
        role,
        "checkbox" | "radio" | "switch" | "menuitemcheckbox" | "menuitemradio"
    );
    if checkable {
        s.checked = Some(
            match dom
                .aria(id, "aria-checked")
                .map(|v| v.trim().to_ascii_lowercase())
            {
                Some(v) if v == "true" => Checked::True,
                Some(v) if v == "mixed" && role != "radio" && role != "switch" => Checked::Mixed,
                Some(v) if v == "false" => Checked::False,
                _ if n.is_html("input")
                    && dom
                        .control(id)
                        .and_then(|c| c.checked)
                        .unwrap_or(n.attr("checked").is_some()) =>
                {
                    Checked::True
                }
                _ => Checked::False,
            },
        );
    }
    s.disabled = natively_disabled(dom, id)
        || (pageon_dom::forms::is_form_associated(dom, id) && pageon_dom::forms::disabled(dom, id))
        || aria_true(dom, id, "aria-disabled");
    s.expanded = match dom
        .aria(id, "aria-expanded")
        .map(|v| v.trim().to_ascii_lowercase())
    {
        Some(v) if v == "true" => Some(true),
        Some(v) if v == "false" => Some(false),
        _ if n.is_html("summary") => dom
            .parent(id)
            .filter(|&p| is(dom, p, "details"))
            .map(|p| dom.get(p).is_some_and(|d| d.attr("open").is_some())),
        _ if n.is_html("select") && role == "combobox" => Some(false),
        _ => None,
    };
    s.selected = aria_true(dom, id, "aria-selected")
        || (n.is_html("option")
            && std::iter::successors(dom.parent(id), |&p| dom.parent(p))
                .find(|&p| is(dom, p, "select"))
                .is_some_and(|sel| selected_options(dom, sel).contains(&id)));
    s.required = aria_true(dom, id, "aria-required")
        || (["input", "select", "textarea"].iter().any(|t| n.is_html(t))
            && n.attr("required").is_some());
    s.invalid = dom
        .aria(id, "aria-invalid")
        .map(|v| v.trim().to_ascii_lowercase())
        .is_some_and(|v| !v.is_empty() && v != "false")
        || pageon_dom::forms::user_invalid(dom, id);
    if role == "heading" {
        let native = n
            .local_name()
            .and_then(|l| l.strip_prefix('h'))
            .and_then(|d| d.parse::<u8>().ok());
        let aria = dom
            .aria(id, "aria-level")
            .and_then(|v| v.trim().parse::<u8>().ok())
            .filter(|&l| l > 0);
        s.level = aria.or(native).or(Some(2));
    }
    s.modal = matches!(role, "dialog" | "alertdialog") && aria_true(dom, id, "aria-modal");
    s
}

/// The value of a form control or range widget.
pub(crate) fn value(dom: &Dom, id: NodeId, role: &str) -> Option<String> {
    let n = dom.get(id)?;
    match role {
        "textbox" | "searchbox" | "combobox" | "spinbutton" | "slider" if n.is_html("input") => {
            let ty = n
                .attr("type")
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            if ty == "password" {
                // Never expose a password field's value, nor its length: only that it is filled.
                let v = dom
                    .control(id)
                    .and_then(|c| c.value.clone())
                    .or_else(|| n.attr("value").map(str::to_string));
                return v.filter(|v| !v.is_empty()).map(|_| "•••".to_string());
            }
            if ty == "range" {
                return Some(n.attr("value").unwrap_or("50").to_string());
            }
            dom.control(id)
                .and_then(|c| c.value.clone())
                .or_else(|| n.attr("value").map(str::to_string))
        }
        "textbox" if n.is_html("textarea") => Some(
            dom.control(id)
                .and_then(|c| c.value.clone())
                .unwrap_or_else(|| crate::text_content(dom, id)),
        ),
        "combobox" | "listbox" if n.is_html("select") => selected_options(dom, id)
            .first()
            .map(|&o| crate::name::normalize(&crate::text_content(dom, o))),
        "slider" | "spinbutton" | "progressbar" | "meter" | "scrollbar" => dom
            .aria(id, "aria-valuetext")
            .or_else(|| dom.aria(id, "aria-valuenow"))
            .or_else(|| n.attr("value"))
            .map(str::to_string),
        _ => None,
    }
}
