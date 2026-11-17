//! Forms: form owners, listed elements, and constraint validation (HTML §4.10.17–4.10.21).
//!
//! Validity is a pure function of the document: attributes, the control state (value,
//! checkedness, the user-edit flag, the custom error) and, for form-associated custom elements,
//! what their `ElementInternals` set. Bindings, selectors, layout, the accessibility tree and
//! action outcomes all read the same answer.

use std::collections::HashMap;

use crate::{Dom, Edge, NodeId};

/// The validity flags of a control (`ValidityState`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(missing_docs)] // HTML's names
pub struct Validity {
    pub value_missing: bool,
    pub type_mismatch: bool,
    pub pattern_mismatch: bool,
    pub too_long: bool,
    pub too_short: bool,
    pub range_underflow: bool,
    pub range_overflow: bool,
    pub step_mismatch: bool,
    pub bad_input: bool,
    pub custom_error: bool,
}

impl Validity {
    /// No flag is set.
    pub fn valid(&self) -> bool {
        *self == Self::default()
    }
}

/// What a form-associated custom element's `ElementInternals` hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Internals {
    /// The element's definition says `static formAssociated = true`.
    pub form_associated: bool,
    /// `attachInternals()` was called.
    pub attached: bool,
    /// Entries `setFormValue` gave: `(None, v)` is submitted under the element's `name`.
    pub value: Vec<(Option<String>, String)>,
    /// Flags from `setValidity`.
    pub validity: Validity,
    /// The message from `setValidity`.
    pub message: String,
    /// The anchor from `setValidity` (focused instead of the element).
    pub anchor: Option<NodeId>,
    /// The custom state set, in insertion order.
    pub states: Vec<String>,
    /// Default ARIA semantics, by attribute name (`role`, `aria-label`, …).
    pub aria: Vec<(String, String)>,
    /// Disabled by its own `disabled` attribute or a disabled fieldset (as last told).
    pub disabled: bool,
}

impl Dom {
    /// The internals of a form-associated or internals-bearing custom element.
    pub fn internals(&self, id: NodeId) -> Option<&Internals> {
        self.get(id)?;
        self.internals.get(&id)
    }

    /// The internals of an element, for changes.
    pub fn internals_mut(&mut self, id: NodeId) -> &mut Internals {
        self.internals.entry(id).or_default()
    }

    /// The form values of every element with internals.
    pub fn internals_values(&self) -> impl Iterator<Item = &(Option<String>, String)> + '_ {
        self.internals.values().flat_map(|i| i.value.iter())
    }

    /// Whether any element has internals (selectors and layout skip the lookups otherwise).
    pub fn has_internals(&self) -> bool {
        !self.internals.is_empty()
    }

    /// The default ARIA value `name` (`role`, `aria-checked`) from an element's internals.
    pub fn default_aria(&self, id: NodeId, name: &str) -> Option<&str> {
        self.internals(id)?
            .aria
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// An ARIA attribute (`role`, `aria-label`, …) of an element, else its default from
    /// `ElementInternals` (default semantics, overridden by the element's own attributes).
    pub fn aria(&self, id: NodeId, name: &str) -> Option<&str> {
        let n = self.get(id)?;
        n.attr(name).or_else(|| {
            if self.internals.is_empty() {
                None
            } else {
                self.default_aria(id, name)
            }
        })
    }

    /// The CSS texts adopted by `root` (the document node or a shadow root), in order.
    pub fn adopted(&self, root: NodeId) -> &[String] {
        self.adopted.get(&root).map_or(&[], Vec::as_slice)
    }

    /// Sets the CSS texts `root` adopts.
    pub fn set_adopted(&mut self, root: NodeId, sheets: Vec<String>) {
        self.log_style(crate::StyleChange::Sheets);
        if sheets.is_empty() {
            self.adopted.remove(&root);
        } else {
            self.adopted.insert(root, sheets);
        }
    }

    /// The trees (document node, shadow roots) that adopt stylesheets.
    pub fn adopted_roots(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.adopted.keys().copied()
    }

    /// Whether any tree adopts stylesheets.
    pub fn has_adopted(&self) -> bool {
        !self.adopted.is_empty()
    }

    /// The rules of a `<style>` as the CSSOM changed them (`None`: its text applies).
    pub fn cssom_text(&self, style: NodeId) -> Option<&str> {
        self.cssom.get(&style).map(String::as_str)
    }

    /// The source line (1-based) of a parsed `<script>`'s start tag: its text's lines count
    /// from there, as browsers report them.
    pub fn script_line(&self, script: NodeId) -> Option<u32> {
        self.script_lines.get(&script).copied()
    }

    /// Whether the document was parsed with scripting on (`<noscript>` is not rendered).
    pub fn scripting(&self) -> bool {
        self.scripting
    }

    /// Every `<style>`'s CSSOM rules.
    pub fn cssom_texts(&self) -> impl Iterator<Item = &str> + '_ {
        self.cssom.values().map(String::as_str)
    }

    /// Sets (or, with `None`, clears) the CSSOM rules of a `<style>`.
    pub fn set_cssom_text(&mut self, style: NodeId, css: Option<String>) {
        self.log_style(crate::StyleChange::Sheets);
        match css {
            Some(c) => {
                self.cssom.insert(style, c);
            }
            None => {
                self.cssom.remove(&style);
            }
        }
    }
}

fn html(dom: &Dom, id: NodeId, local: &str) -> bool {
    dom.get(id).is_some_and(|n| n.is_html(local))
}

fn attr<'a>(dom: &'a Dom, id: NodeId, name: &str) -> Option<&'a str> {
    dom.get(id)?.attr(name)
}

/// A form-associated custom element.
pub fn is_form_associated(dom: &Dom, id: NodeId) -> bool {
    dom.internals(id).is_some_and(|i| i.form_associated)
}

/// A listed element (`form.elements`): `button`, `fieldset`, `input`, `object`, `output`,
/// `select`, `textarea`, and form-associated custom elements.
pub fn is_listed(dom: &Dom, id: NodeId) -> bool {
    dom.get(id).is_some_and(|n| {
        [
            "button", "fieldset", "input", "object", "output", "select", "textarea",
        ]
        .iter()
        .any(|t| n.is_html(t))
    }) || is_form_associated(dom, id)
}

/// The lower-cased `type` of an `<input>`, `text` when absent or unknown.
pub fn input_type(dom: &Dom, id: NodeId) -> String {
    const TYPES: &[&str] = &[
        "hidden",
        "text",
        "search",
        "tel",
        "url",
        "email",
        "password",
        "date",
        "month",
        "week",
        "time",
        "datetime-local",
        "number",
        "range",
        "color",
        "checkbox",
        "radio",
        "file",
        "submit",
        "image",
        "reset",
        "button",
    ];
    let t = attr(dom, id, "type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if TYPES.contains(&t.as_str()) {
        t
    } else {
        "text".into()
    }
}

/// The root of the tree `id` is in (the document node, a shadow root, or a detached subtree's
/// top).
fn tree_root(dom: &Dom, id: NodeId) -> NodeId {
    dom.ancestors(id).last().unwrap_or(id)
}

/// The form owner of a listed element: the form its `form` attribute names in its tree, else
/// its nearest ancestor form.
pub fn owner(dom: &Dom, id: NodeId) -> Option<NodeId> {
    if !is_listed(dom, id) {
        return None;
    }
    if let Some(name) = attr(dom, id, "form") {
        let root = tree_root(dom, id);
        return dom.traverse(root).find_map(|e| match e {
            Edge::Enter(n) if attr(dom, n, "id") == Some(name) => html(dom, n, "form").then_some(n),
            _ => None,
        });
    }
    dom.ancestors(id).find(|&a| html(dom, a, "form"))
}

/// The listed elements whose owner is `form`, in tree order.
pub fn listed(dom: &Dom, form: NodeId) -> Vec<NodeId> {
    // Owners by `form` attribute only exist when the form has an id; otherwise descendants.
    let scope = if attr(dom, form, "id").is_some() {
        tree_root(dom, form)
    } else {
        form
    };
    dom.traverse(scope)
        .filter_map(|e| match e {
            Edge::Enter(n) if is_listed(dom, n) && owner(dom, n) == Some(form) => Some(n),
            _ => None,
        })
        .collect()
}

/// Disabled: its own `disabled` attribute, or inside a disabled `<fieldset>` (not within that
/// fieldset's first `<legend>`).
pub fn disabled(dom: &Dom, id: NodeId) -> bool {
    let Some(n) = dom.get(id) else { return false };
    let control = ["button", "input", "select", "textarea", "fieldset"]
        .iter()
        .any(|t| n.is_html(t))
        || is_form_associated(dom, id);
    if !control {
        return false;
    }
    if n.attr("disabled").is_some() {
        return true;
    }
    let mut child = id;
    for p in dom.ancestors(id) {
        if html(dom, p, "fieldset") && attr(dom, p, "disabled").is_some() {
            let first_legend = dom.children(p).find(|&c| html(dom, c, "legend"));
            if first_legend != Some(child) {
                return true;
            }
        }
        child = p;
    }
    false
}

/// Barred from constraint validation (`willValidate` false).
pub fn barred(dom: &Dom, id: NodeId) -> bool {
    let Some(n) = dom.get(id) else { return true };
    let submittable = ["button", "input", "select", "textarea"]
        .iter()
        .any(|t| n.is_html(t))
        || is_form_associated(dom, id);
    if !submittable || disabled(dom, id) {
        return true;
    }
    if n.is_html("input") {
        let t = input_type(dom, id);
        if matches!(t.as_str(), "hidden" | "reset" | "button") {
            return true;
        }
        if n.attr("readonly").is_some() && readonly_applies(&t) {
            return true;
        }
    }
    if n.is_html("textarea") && n.attr("readonly").is_some() {
        return true;
    }
    if n.is_html("button") {
        let t = n.attr("type").map(str::to_ascii_lowercase);
        if matches!(t.as_deref(), Some("reset" | "button")) {
            return true;
        }
    }
    dom.ancestors(id).any(|a| html(dom, a, "datalist"))
}

fn readonly_applies(t: &str) -> bool {
    matches!(
        t,
        "text"
            | "search"
            | "url"
            | "tel"
            | "email"
            | "password"
            | "date"
            | "month"
            | "week"
            | "time"
            | "datetime-local"
            | "number"
    )
}

/// The current value of an `<input>` or `<textarea>` (control state, else the default).
pub fn value(dom: &Dom, id: NodeId) -> String {
    if let Some(v) = dom.control(id).and_then(|c| c.value.clone()) {
        return v;
    }
    if html(dom, id, "textarea") {
        return dom.text(id);
    }
    attr(dom, id, "value").unwrap_or_default().to_string()
}

/// The current checkedness of a checkbox or radio.
pub fn checked(dom: &Dom, id: NodeId) -> bool {
    dom.control(id)
        .and_then(|c| c.checked)
        .unwrap_or_else(|| attr(dom, id, "checked").is_some())
}

/// The radios of `radio`'s group (same name, same form owner, same tree), itself included.
pub fn radio_group(dom: &Dom, radio: NodeId) -> Vec<NodeId> {
    let Some(name) = attr(dom, radio, "name").filter(|n| !n.is_empty()) else {
        return vec![radio];
    };
    let form = owner(dom, radio);
    let root = tree_root(dom, radio);
    dom.traverse(root)
        .filter_map(|e| match e {
            Edge::Enter(n)
                if html(dom, n, "input")
                    && input_type(dom, n) == "radio"
                    && attr(dom, n, "name") == Some(name)
                    && owner(dom, n) == form =>
            {
                Some(n)
            }
            _ => None,
        })
        .collect()
}

fn selected_options(dom: &Dom, select: NodeId) -> (Vec<NodeId>, Option<NodeId>) {
    let options: Vec<NodeId> = dom
        .traverse(select)
        .filter_map(|e| match e {
            Edge::Enter(n) if html(dom, n, "option") => Some(n),
            _ => None,
        })
        .collect();
    let selected: Vec<NodeId> = options
        .iter()
        .copied()
        .filter(|&o| {
            dom.control(o)
                .and_then(|c| c.selected)
                .unwrap_or_else(|| attr(dom, o, "selected").is_some())
        })
        .collect();
    let multiple = attr(dom, select, "multiple").is_some();
    let size = attr(dom, select, "size")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(1);
    // The placeholder label option: the first option, value "", child of the select.
    let placeholder = (!multiple && size <= 1)
        .then(|| options.first().copied())
        .flatten()
        .filter(|&o| dom.parent(o) == Some(select) && option_value(dom, o).is_empty());
    // A single-select shows its first enabled option when none is selected.
    let shown = if selected.is_empty() && !multiple && size <= 1 {
        options
            .iter()
            .copied()
            .find(|&o| attr(dom, o, "disabled").is_none())
            .into_iter()
            .collect()
    } else if multiple {
        selected
    } else {
        selected.last().copied().into_iter().collect()
    };
    (shown, placeholder)
}

fn option_value(dom: &Dom, o: NodeId) -> String {
    match attr(dom, o, "value") {
        Some(v) => v.to_string(),
        None => dom.text(o).split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

/// The validity of `id`. Barred elements keep only `customError`.
pub fn validity(dom: &Dom, id: NodeId) -> Validity {
    let Some(n) = dom.get(id) else {
        return Validity::default();
    };
    if is_form_associated(dom, id) {
        let mut v = dom.internals(id).map(|i| i.validity).unwrap_or_default();
        if barred(dom, id) {
            v = Validity {
                custom_error: v.custom_error,
                ..Validity::default()
            };
        }
        return v;
    }
    let mut v = Validity {
        custom_error: dom
            .control(id)
            .and_then(|c| c.custom_error.as_ref())
            .is_some_and(|m| !m.is_empty()),
        ..Validity::default()
    };
    if barred(dom, id) {
        return v;
    }
    let required = n.attr("required").is_some();
    if n.is_html("textarea") {
        let val = value(dom, id);
        v.value_missing = required && val.is_empty();
        lengths(dom, id, &val, &mut v);
    } else if n.is_html("select") {
        if required {
            let (shown, placeholder) = selected_options(dom, id);
            v.value_missing =
                shown.is_empty() || (shown.len() == 1 && Some(shown[0]) == placeholder);
        }
    } else if n.is_html("input") {
        let t = input_type(dom, id);
        let val = value(dom, id);
        match t.as_str() {
            "checkbox" => v.value_missing = required && !checked(dom, id),
            "radio" => {
                let group = radio_group(dom, id);
                let any_required = group.iter().any(|&r| attr(dom, r, "required").is_some());
                v.value_missing = any_required && !group.iter().any(|&r| checked(dom, r));
            }
            "file" => v.value_missing = required && val.is_empty(),
            "submit" | "image" | "color" => {}
            _ => {
                v.value_missing = required && val.is_empty();
                if !val.is_empty() {
                    match t.as_str() {
                        "email" => v.type_mismatch = !email_ok(&val, n.attr("multiple").is_some()),
                        "url" => v.type_mismatch = !url_ok(&val),
                        _ => {}
                    }
                    if let Some(p) = n.attr("pattern")
                        && matches!(
                            t.as_str(),
                            "text" | "search" | "url" | "tel" | "email" | "password"
                        )
                    {
                        let values: Vec<&str> = if t == "email" && n.attr("multiple").is_some() {
                            val.split(',').map(str::trim).collect()
                        } else {
                            vec![val.as_str()]
                        };
                        v.pattern_mismatch =
                            values.iter().any(|x| pattern_matches(p, x) == Some(false));
                    }
                    if let Some(kind) = Numeric::of(&t) {
                        range(dom, id, kind, &val, &mut v);
                    }
                }
                if matches!(
                    t.as_str(),
                    "text" | "search" | "url" | "tel" | "email" | "password"
                ) {
                    lengths(dom, id, &val, &mut v);
                }
            }
        }
    }
    v
}

fn lengths(dom: &Dom, id: NodeId, val: &str, v: &mut Validity) {
    // Only for values the user edited (HTML: the dirty value flag set by a user edit).
    if val.is_empty() || !dom.control(id).is_some_and(|c| c.user_edited) {
        return;
    }
    let len = val.encode_utf16().count() as i64;
    if let Some(max) = non_negative(dom, id, "maxlength") {
        v.too_long = len > max;
    }
    if let Some(min) = non_negative(dom, id, "minlength") {
        v.too_short = len < min;
    }
}

fn non_negative(dom: &Dom, id: NodeId, name: &str) -> Option<i64> {
    attr(dom, id, name)?
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)
}

/// HTML's valid email address (one, or a comma-separated list with `multiple`).
fn email_ok(val: &str, multiple: bool) -> bool {
    let one = |s: &str| {
        let Some((local, domain)) = s.split_once('@') else {
            return false;
        };
        !local.is_empty()
            && local
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(c))
            && !domain.is_empty()
            && domain.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            })
    };
    if multiple {
        val.split(',').map(str::trim).all(one)
    } else {
        one(val)
    }
}

fn url_ok(val: &str) -> bool {
    url::Url::parse(val.trim()).is_ok()
}

thread_local! {
    static PATTERNS: std::cell::RefCell<HashMap<String, Option<fancy_regex::Regex>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Whether `value` matches `pattern` as a whole (`^(?:pattern)$`); `None` if the pattern does
/// not compile (then it imposes no constraint, as in HTML).
pub fn pattern_matches(pattern: &str, value: &str) -> Option<bool> {
    PATTERNS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() > 256 {
            cache.clear();
        }
        let re = cache
            .entry(pattern.to_string())
            .or_insert_with(|| {
                fancy_regex::RegexBuilder::new(&format!("^(?:{pattern})$"))
                    .backtrack_limit(100_000)
                    .build()
                    .ok()
            })
            .clone()?;
        Some(re.is_match(value).unwrap_or(true))
    })
}

/// Input types with numeric values: how to parse and print them, their default step and step
/// scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Numeric {
    Number,
    Range,
    Date,
    Month,
    Week,
    Time,
    DateTime,
}

impl Numeric {
    fn of(t: &str) -> Option<Self> {
        Some(match t {
            "number" => Self::Number,
            "range" => Self::Range,
            "date" => Self::Date,
            "month" => Self::Month,
            "week" => Self::Week,
            "time" => Self::Time,
            "datetime-local" => Self::DateTime,
            _ => return None,
        })
    }

    /// Default step, step scale (value units per step unit), default step base.
    fn step(self) -> (f64, f64, f64) {
        match self {
            Self::Number | Self::Range => (1.0, 1.0, 0.0),
            Self::Date => (1.0, 86_400_000.0, 0.0),
            Self::Month => (1.0, 1.0, 0.0),
            Self::Week => (1.0, 604_800_000.0, -259_200_000.0),
            Self::Time | Self::DateTime => (60.0, 1000.0, 0.0),
        }
    }

    fn parse(self, s: &str) -> Option<f64> {
        match self {
            Self::Number | Self::Range => parse_float(s),
            Self::Date => parse_date(s).map(|d| d as f64 * 86_400_000.0),
            Self::Month => {
                let (y, m) = s.split_once('-')?;
                let (y, m): (i64, i64) = (year(y)?, two(m)?);
                (1..=12)
                    .contains(&m)
                    .then_some(((y - 1970) * 12 + m - 1) as f64)
            }
            Self::Week => {
                let (y, w) = s.split_once("-W")?;
                let (y, w) = (year(y)?, two(w)?);
                let weeks = if long_year(y) { 53 } else { 52 };
                if !(1..=weeks).contains(&w) {
                    return None;
                }
                // Monday of week 1: the week with the year's first Thursday.
                let jan1 = days_from_civil(y, 1, 1);
                let dow = (jan1 + 3).rem_euclid(7); // 0 = Monday
                let monday = if dow <= 3 { jan1 - dow } else { jan1 + 7 - dow };
                Some((monday + (w - 1) * 7) as f64 * 86_400_000.0)
            }
            Self::Time => parse_time(s),
            Self::DateTime => {
                let (d, t) = s.split_once(['T', ' '])?;
                Some(parse_date(d)? as f64 * 86_400_000.0 + parse_time(t)?)
            }
        }
    }

    fn print(self, v: f64) -> String {
        match self {
            Self::Number | Self::Range => number(v),
            Self::Date => {
                let (y, m, d) = civil_from_days((v / 86_400_000.0).floor() as i64);
                format!("{y:04}-{m:02}-{d:02}")
            }
            Self::Month => {
                let m = v as i64;
                format!("{:04}-{:02}", 1970 + m.div_euclid(12), m.rem_euclid(12) + 1)
            }
            Self::Week | Self::DateTime => {
                let days = (v / 86_400_000.0).floor() as i64;
                let (y, m, d) = civil_from_days(days);
                if self == Self::Week {
                    format!("{y:04}-{m:02}-{d:02}")
                } else {
                    format!(
                        "{y:04}-{m:02}-{d:02}T{}",
                        time(v - days as f64 * 86_400_000.0)
                    )
                }
            }
            Self::Time => time(v),
        }
    }

    fn dated(self) -> bool {
        !matches!(self, Self::Number | Self::Range)
    }
}

fn range(dom: &Dom, id: NodeId, kind: Numeric, val: &str, v: &mut Validity) {
    let Some(x) = kind.parse(val) else {
        v.bad_input = true;
        return;
    };
    if kind == Numeric::Range {
        // A range's value is clamped and stepped by the browser: never out of range.
        return;
    }
    let min = attr(dom, id, "min").and_then(|s| kind.parse(s));
    let max = attr(dom, id, "max").and_then(|s| kind.parse(s));
    v.range_underflow = min.is_some_and(|m| x < m);
    v.range_overflow = max.is_some_and(|m| x > m);
    if let Some((step, base)) = step_of(dom, id, kind) {
        let n = (x - base) / step;
        v.step_mismatch = (n - n.round()).abs() > 1e-9 * n.abs().max(1.0);
    }
}

/// The allowed value step and the step base, or `None` for `step=any`.
fn step_of(dom: &Dom, id: NodeId, kind: Numeric) -> Option<(f64, f64)> {
    let (default, scale, default_base) = kind.step();
    let step = match attr(dom, id, "step").map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("any") => return None,
        Some(s) => parse_float(s).filter(|n| *n > 0.0).unwrap_or(default),
        None => default,
    } * scale;
    let base = attr(dom, id, "min")
        .and_then(|s| kind.parse(s))
        .or_else(|| attr(dom, id, "value").and_then(|s| kind.parse(s)))
        .unwrap_or(default_base);
    Some((step, base))
}

/// The validation message of `id` for flags `v` (Chrome's English messages; the custom message
/// when there is one). Messages never quote the control's value.
pub fn message(dom: &Dom, id: NodeId, v: &Validity) -> String {
    if v.valid() {
        return String::new();
    }
    if v.custom_error || is_form_associated(dom, id) {
        if let Some(i) = dom.internals(id).filter(|i| i.form_associated) {
            return i.message.clone();
        }
        return dom
            .control(id)
            .and_then(|c| c.custom_error.clone())
            .unwrap_or_default();
    }
    let Some(n) = dom.get(id) else {
        return String::new();
    };
    let t = if n.is_html("input") {
        input_type(dom, id)
    } else {
        n.local_name().unwrap_or_default().to_string()
    };
    let kind = Numeric::of(&t);
    if v.value_missing {
        return match t.as_str() {
            "checkbox" => "Please check this box if you want to proceed.",
            "radio" => "Please select one of these options.",
            "select" => "Please select an item in the list.",
            "file" => "Please select a file.",
            _ => "Please fill out this field.",
        }
        .into();
    }
    if v.bad_input {
        return if kind.is_some_and(Numeric::dated) {
            "Please enter a valid value. The field is incomplete or has an invalid date.".into()
        } else {
            "Please enter a number.".into()
        };
    }
    if v.type_mismatch {
        return if t == "email" {
            "Please enter an email address."
        } else {
            "Please enter a URL."
        }
        .into();
    }
    if v.pattern_mismatch {
        return match n.attr("title").map(str::trim).filter(|s| !s.is_empty()) {
            Some(title) => format!("Please match the requested format.\n{title}"),
            None => "Please match the requested format.".into(),
        };
    }
    let len = value(dom, id).encode_utf16().count();
    if v.too_long {
        let max = non_negative(dom, id, "maxlength").unwrap_or(0);
        return format!(
            "Please shorten this text to {max} characters or less (you are currently using {len} characters)."
        );
    }
    if v.too_short {
        let min = non_negative(dom, id, "minlength").unwrap_or(0);
        return format!(
            "Please lengthen this text to {min} characters or more (you are currently using {len} characters)."
        );
    }
    let Some(kind) = kind else {
        return String::new();
    };
    let bound = |name: &str| {
        attr(dom, id, name)
            .and_then(|s| kind.parse(s))
            .map(|x| kind.print(x))
            .unwrap_or_default()
    };
    if v.range_underflow {
        return if kind.dated() {
            format!("Value must be {} or later.", bound("min"))
        } else {
            format!("Value must be greater than or equal to {}.", bound("min"))
        };
    }
    if v.range_overflow {
        return if kind.dated() {
            format!("Value must be {} or earlier.", bound("max"))
        } else {
            format!("Value must be less than or equal to {}.", bound("max"))
        };
    }
    if v.step_mismatch
        && let (Some(x), Some((step, base))) = (kind.parse(&value(dom, id)), step_of(dom, id, kind))
    {
        let low = base + ((x - base) / step).floor() * step;
        let high = low + step;
        let max = attr(dom, id, "max").and_then(|s| kind.parse(s));
        return if max.is_some_and(|m| high > m) {
            format!(
                "Please enter a valid value. The nearest valid value is {}.",
                kind.print(low)
            )
        } else {
            format!(
                "Please enter a valid value. The two nearest valid values are {} and {}.",
                kind.print(low),
                kind.print(high)
            )
        };
    }
    String::new()
}

/// A candidate for validation that is invalid.
pub fn invalid(dom: &Dom, id: NodeId) -> bool {
    !barred(dom, id) && !validity(dom, id).valid()
}

/// Whether a form or fieldset contains an invalid candidate (`:invalid` on them).
pub fn has_invalid(dom: &Dom, id: NodeId) -> bool {
    if html(dom, id, "form") {
        return listed(dom, id).into_iter().any(|c| invalid(dom, c));
    }
    dom.traverse(id).any(|e| match e {
        Edge::Enter(n) if n != id => invalid(dom, n),
        _ => false,
    })
}

/// User-invalid (`:user-invalid`, shown as `invalid` to agents): invalid after the user
/// changed it or a submission was blocked.
pub fn user_invalid(dom: &Dom, id: NodeId) -> bool {
    dom.control(id).is_some_and(|c| c.user_validity) && invalid(dom, id)
}

// ----- numbers, dates and times -----

/// HTML's rules for parsing floating-point number values (a valid floating-point number).
pub fn parse_float(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > start
    };
    let int = digits(&mut i);
    let mut frac = false;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        frac = digits(&mut i);
        if !frac {
            return None;
        }
    }
    if !int && !frac {
        return None;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        if !digits(&mut i) {
            return None;
        }
    }
    if i != b.len() {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Shortest decimal form of a number (`10`, `0.5`).
fn number(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn year(s: &str) -> Option<i64> {
    (s.len() >= 4 && s.bytes().all(|c| c.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
        .filter(|y| *y > 0)
}

fn two(s: &str) -> Option<i64> {
    (s.len() == 2 && s.bytes().all(|c| c.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn long_year(y: i64) -> bool {
    let dow = |y: i64| (days_from_civil(y, 1, 1) + 3).rem_euclid(7); // 0 = Monday
    dow(y) == 3 || (leap(y) && dow(y) == 2)
}

/// Days since 1970-01-01 (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.splitn(3, '-');
    let (y, m, d) = (year(it.next()?)?, two(it.next()?)?, two(it.next()?)?);
    let days = [
        31,
        if leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    ((1..=12).contains(&m) && d >= 1 && d <= days[(m - 1) as usize])
        .then(|| days_from_civil(y, m, d))
}

/// Milliseconds since midnight.
fn parse_time(s: &str) -> Option<f64> {
    let mut it = s.splitn(3, ':');
    let (h, m) = (two(it.next()?)?, two(it.next()?)?);
    let secs = match it.next() {
        None => 0.0,
        Some(sec) => {
            let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
            let w = two(whole)?;
            if frac.len() > 3 || !frac.bytes().all(|c| c.is_ascii_digit()) || w > 59 {
                return None;
            }
            w as f64
                + if frac.is_empty() {
                    0.0
                } else {
                    format!("0.{frac}").parse::<f64>().ok()?
                }
        }
    };
    (h < 24 && m < 60).then(|| (h * 3_600_000 + m * 60_000) as f64 + secs * 1000.0)
}

fn time(ms: f64) -> String {
    let total = ms.round() as i64;
    let (h, rest) = (total / 3_600_000, total % 3_600_000);
    let (m, rest) = (rest / 60_000, rest % 60_000);
    let (s, milli) = (rest / 1000, rest % 1000);
    match (s, milli) {
        (0, 0) => format!("{h:02}:{m:02}"),
        (_, 0) => format!("{h:02}:{m:02}:{s:02}"),
        _ => format!("{h:02}:{m:02}:{s:02}.{milli:03}"),
    }
}
