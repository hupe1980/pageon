//! HTML element interfaces with behavior: form controls, dialogs, links, scripts, templates.

use pageon_dom::{NodeData, NodeId};

use super::{collect, gone, text_content};
use crate::cv::{Cx, Error, Obj, R};
use crate::generated::Iface;
use crate::realm::{Coll, Slot, Target};

fn attr(cx: &Cx<'_, '_, '_>, id: NodeId, name: &str) -> Option<String> {
    cx.dom()
        .get(id)
        .and_then(|n| n.attr(name).map(str::to_string))
}

fn form_of(cx: &Cx<'_, '_, '_>, id: NodeId) -> Option<NodeId> {
    pageon_dom::forms::owner(&cx.dom(), id)
}

/// `HTMLInputElement`.
pub mod html_input_element {
    use super::*;

    /// Days since 1970-01-01 of a proleptic Gregorian date.
    fn days(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// `valueAsNumber`: the number of a number or range input, the milliseconds (UTC) of a
    /// date, time or local date-time input, else NaN.
    pub fn get_value_as_number(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let id = this.node()?;
        let kind = pageon_dom::forms::input_type(&cx.dom(), id).to_string();
        let v = get_value(cx, this)?;
        let v = v.trim();
        let date = |s: &str| -> Option<f64> {
            let mut p = s.split('-');
            let (y, m, d) = (
                p.next()?.parse().ok()?,
                p.next()?.parse().ok()?,
                p.next()?.parse().ok()?,
            );
            (p.next().is_none() && (1..=12).contains(&m) && (1..=31).contains(&d))
                .then(|| days(y, m, d) as f64 * 86_400_000.0)
        };
        let time = |s: &str| -> Option<f64> {
            let mut p = s.split(':');
            let h: f64 = p.next()?.parse().ok()?;
            let m: f64 = p.next()?.parse().ok()?;
            let sec: f64 = p.next().map_or(Some(0.0), |x| x.parse().ok())?;
            (h < 24.0 && m < 60.0 && sec < 60.0).then_some(((h * 60.0 + m) * 60.0 + sec) * 1000.0)
        };
        Ok(match kind.as_str() {
            "number" | "range" => v.parse::<f64>().ok().filter(|f| f.is_finite()),
            "date" => date(v),
            "time" => time(v),
            "datetime-local" => v
                .split_once('T')
                .and_then(|(d, t)| Some(date(d)? + time(t)?)),
            _ => None,
        }
        .unwrap_or(f64::NAN))
    }

    /// Sets the value from a number (an empty value for NaN).
    pub fn set_value_as_number(cx: &mut Cx<'_, '_, '_>, this: Obj, v: f64) -> R<()> {
        let id = this.node()?;
        let kind = pageon_dom::forms::input_type(&cx.dom(), id).to_string();
        if !matches!(kind.as_str(), "number" | "range") {
            return Err(Error::Dom(
                "InvalidStateError",
                format!("valueAsNumber cannot be set on a {kind} input here"),
            ));
        }
        let s = if v.is_finite() {
            format!("{v}")
        } else {
            String::new()
        };
        set_value(cx, this, s)
    }
    pub use crate::dom::validation::api::*;

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

    fn ty(cx: &Cx<'_, '_, '_>, id: NodeId) -> String {
        let t = attr(cx, id, "type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if TYPES.contains(&t.as_str()) {
            t
        } else {
            "text".into()
        }
    }

    /// Current checkedness.
    pub(crate) fn checked(cx: &Cx<'_, '_, '_>, id: NodeId) -> bool {
        match cx.dom().control(id).and_then(|c| c.checked) {
            Some(c) => c,
            None => attr(cx, id, "checked").is_some(),
        }
    }

    /// Sets checkedness (unchecking the other radios of the group).
    pub(crate) fn set_checked_state(cx: &Cx<'_, '_, '_>, id: NodeId, on: bool) {
        if on
            && ty(cx, id) == "radio"
            && let Some(name) = attr(cx, id, "name").filter(|n| !n.is_empty())
        {
            let scope = form_of(cx, id).unwrap_or_else(|| cx.dom().root());
            let group: Vec<NodeId> = {
                let dom = cx.dom();
                dom.traverse(scope)
                    .filter_map(|e| match e {
                        pageon_dom::Edge::Enter(n)
                            if n != id
                                && dom.get(n).is_some_and(|x| {
                                    x.is_html("input")
                                        && x.attr("type")
                                            .is_some_and(|t| t.eq_ignore_ascii_case("radio"))
                                        && x.attr("name") == Some(name.as_str())
                                }) =>
                        {
                            Some(n)
                        }
                        _ => None,
                    })
                    .collect()
            };
            for g in group {
                cx.dom_mut().control_mut(g).checked = Some(false);
            }
        }
        cx.dom_mut().control_mut(id).checked = Some(on);
    }

    pub fn get_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        if let Some(v) = cx.dom().control(id).and_then(|c| c.value.clone()) {
            return Ok(v);
        }
        let t = ty(cx, id);
        Ok(attr(cx, id, "value").unwrap_or_else(|| {
            if t == "checkbox" || t == "radio" {
                "on".into()
            } else {
                String::new()
            }
        }))
    }

    pub fn set_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let id = this.node()?;
        let t = ty(cx, id);
        if matches!(
            t.as_str(),
            "checkbox" | "radio" | "hidden" | "submit" | "reset" | "button" | "image"
        ) {
            return cx.dom_mut().set_attr(id, "value", &v).map_err(|_| gone());
        }
        if t == "file" && !v.is_empty() {
            return Err(Error::dom(
                "InvalidStateError",
                "A file input's value can only be set to the empty string.",
            ));
        }
        let v = if matches!(
            t.as_str(),
            "text" | "search" | "tel" | "password" | "url" | "email"
        ) {
            v.replace(['\r', '\n'], "")
        } else {
            v
        };
        {
            let mut dom = cx.dom_mut();
            let c = dom.control_mut(id);
            c.value = Some(v);
            c.selection = None;
            c.from_vault = false;
        }
        Ok(())
    }

    pub fn get_default_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(attr(cx, this.node()?, "value").unwrap_or_default())
    }

    pub fn set_default_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "value", &v)
            .map_err(|_| gone())
    }

    pub fn get_checked(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(checked(cx, this.node()?))
    }

    pub fn set_checked(cx: &mut Cx<'_, '_, '_>, this: Obj, v: bool) -> R<()> {
        set_checked_state(cx, this.node()?, v);
        Ok(())
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(ty(cx, this.node()?))
    }

    pub fn set_type_(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "type", &v)
            .map_err(|_| gone())
    }

    /// `select()`: selects the whole value.
    pub fn select(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        crate::dom::editing::sel::select(cx, this)
    }

    pub fn get_selection_start(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<u32>> {
        crate::dom::editing::sel::start(cx, this)
    }

    pub fn set_selection_start(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<u32>) -> R<()> {
        crate::dom::editing::sel::set_start(cx, this, v.unwrap_or(0))
    }

    pub fn get_selection_end(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<u32>> {
        crate::dom::editing::sel::end(cx, this)
    }

    pub fn set_selection_end(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<u32>) -> R<()> {
        crate::dom::editing::sel::set_end(cx, this, v.unwrap_or(0))
    }

    pub fn get_selection_direction(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        crate::dom::editing::sel::direction(cx, this)
    }

    pub fn set_selection_direction(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<String>) -> R<()> {
        crate::dom::editing::sel::set_direction(cx, this, v)
    }

    pub fn set_selection_range(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        start: u32,
        end: u32,
        direction: Option<String>,
    ) -> R<()> {
        crate::dom::editing::sel::set_range(cx, this, start, end, direction)
    }

    pub fn get_form(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(form_of(cx, this.node()?).map(Obj::Node))
    }

    /// `files`: a file input's `FileList` (`null` for other types).
    pub fn get_files(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let id = this.node()?;
        if ty(cx, id) != "file" {
            return Ok(None);
        }
        let files = cx.st().files.get(&id).cloned().unwrap_or_default();
        Ok(Some(cx.alloc(Iface::FileList, Slot::FileList(files))))
    }

    /// Setting `files` to another input's `FileList` (`null` is ignored, as in Chrome).
    pub fn set_files(cx: &mut Cx<'_, '_, '_>, this: Obj, list: Option<Obj>) -> R<()> {
        let id = this.node()?;
        let Some(list) = list else { return Ok(()) };
        let files = match cx.st().slot(list.slab()?) {
            Some(Slot::FileList(l)) => l.clone(),
            _ => return Err(Error::Type("not a FileList".into())),
        };
        if ty(cx, id) == "file" {
            cx.st().files.insert(id, files);
        }
        Ok(())
    }

    pub fn get_labels(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let labels = labels_of(cx, this.node()?);
        Ok(Some(cx.alloc(Iface::NodeList, Slot::Static(labels))))
    }

    /// The labels of a labelable element: `label for=id`, or wrapping labels without `for`.
    pub(crate) fn labels_of(cx: &Cx<'_, '_, '_>, id: NodeId) -> Vec<NodeId> {
        let own = attr(cx, id, "id");
        let dom = cx.dom();
        dom.traverse(dom.root())
            .filter_map(|e| match e {
                pageon_dom::Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("label")) => {
                    let x = dom.get(n)?;
                    let by_for = own.as_deref().is_some_and(|i| x.attr("for") == Some(i));
                    let wraps = x.attr("for").is_none() && dom.is_inclusive_ancestor(n, id);
                    (by_for || wraps).then_some(n)
                }
                _ => None,
            })
            .collect()
    }
}

/// `HTMLTextAreaElement`.
pub mod html_text_area_element {
    use super::*;

    /// `rows` and `cols`: positive numbers, else their defaults (2 and 20).
    fn positive(cx: &Cx<'_, '_, '_>, id: NodeId, name: &str, default: u32) -> u32 {
        cx.dom()
            .get(id)
            .and_then(|n| n.attr(name))
            .and_then(|v| v.trim().parse::<u32>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default)
    }

    pub fn get_rows(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(positive(cx, this.node()?, "rows", 2))
    }

    pub fn set_rows(cx: &mut Cx<'_, '_, '_>, this: Obj, v: u32) -> R<()> {
        let v = if v == 0 { 2 } else { v };
        cx.dom_mut()
            .set_attr(this.node()?, "rows", &v.to_string())
            .map_err(|_| gone())
    }

    pub fn get_cols(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(positive(cx, this.node()?, "cols", 20))
    }

    pub fn set_cols(cx: &mut Cx<'_, '_, '_>, this: Obj, v: u32) -> R<()> {
        let v = if v == 0 { 20 } else { v };
        cx.dom_mut()
            .set_attr(this.node()?, "cols", &v.to_string())
            .map_err(|_| gone())
    }
    pub use crate::dom::validation::api::*;

    pub fn get_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        Ok(cx
            .dom()
            .control(id)
            .and_then(|c| c.value.clone())
            .unwrap_or_else(|| text_content(&cx.dom(), id)))
    }

    pub fn set_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        {
            let id = this.node()?;
            let mut dom = cx.dom_mut();
            let c = dom.control_mut(id);
            c.value = Some(v.replace("\r\n", "\n"));
            c.selection = None;
            c.from_vault = false;
        }
        Ok(())
    }

    pub fn get_default_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(text_content(&cx.dom(), this.node()?))
    }

    pub fn set_default_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        super::super::node::replace_with_text(cx, this.node()?, &v)
    }

    pub fn get_type_(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("textarea".into())
    }

    /// `select()`: selects the whole value.
    pub fn select(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        crate::dom::editing::sel::select(cx, this)
    }

    pub fn get_selection_start(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(crate::dom::editing::sel::start(cx, this)?.unwrap_or(0))
    }

    pub fn set_selection_start(cx: &mut Cx<'_, '_, '_>, this: Obj, v: u32) -> R<()> {
        crate::dom::editing::sel::set_start(cx, this, v)
    }

    pub fn get_selection_end(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(crate::dom::editing::sel::end(cx, this)?.unwrap_or(0))
    }

    pub fn set_selection_end(cx: &mut Cx<'_, '_, '_>, this: Obj, v: u32) -> R<()> {
        crate::dom::editing::sel::set_end(cx, this, v)
    }

    pub fn get_selection_direction(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(crate::dom::editing::sel::direction(cx, this)?.unwrap_or_default())
    }

    pub fn set_selection_direction(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        crate::dom::editing::sel::set_direction(cx, this, Some(v))
    }

    pub fn set_selection_range(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        start: u32,
        end: u32,
        direction: Option<String>,
    ) -> R<()> {
        crate::dom::editing::sel::set_range(cx, this, start, end, direction)
    }

    pub fn get_text_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(get_value(cx, this)?.encode_utf16().count()).unwrap_or(u32::MAX))
    }
}

/// `HTMLOptionElement`.
pub mod html_option_element {
    use super::*;

    /// `new Option(text, value, defaultSelected, selected)`.
    pub fn legacy_factory(
        cx: &mut Cx<'_, '_, '_>,
        text: String,
        value: Option<String>,
        default_selected: bool,
        selected: bool,
    ) -> R<Obj> {
        crate::dom::platform::new_option(cx, text, value, default_selected, selected)
    }

    pub(crate) fn select_of(cx: &Cx<'_, '_, '_>, opt: NodeId) -> Option<NodeId> {
        let dom = cx.dom();
        std::iter::successors(dom.parent(opt), |&p| dom.parent(p))
            .find(|&p| dom.get(p).is_some_and(|n| n.is_html("select")))
    }

    /// Current selectedness (an unselected single-select box shows its first enabled option).
    pub(crate) fn selected(cx: &Cx<'_, '_, '_>, opt: NodeId) -> bool {
        if let Some(s) = cx.dom().control(opt).and_then(|c| c.selected) {
            return s;
        }
        if attr(cx, opt, "selected").is_some() {
            return true;
        }
        let Some(sel) = select_of(cx, opt) else {
            return false;
        };
        if attr(cx, sel, "multiple").is_some() {
            return false;
        }
        let options = collect(&cx.dom(), &Coll::Options(sel));
        let any = options.iter().any(|&o| {
            cx.dom()
                .control(o)
                .and_then(|c| c.selected)
                .unwrap_or_else(|| attr(cx, o, "selected").is_some())
        });
        !any && options.iter().find(|&&o| attr(cx, o, "disabled").is_none()) == Some(&opt)
    }

    pub(crate) fn set_selected_state(cx: &Cx<'_, '_, '_>, opt: NodeId, on: bool) {
        if let Some(sel) = select_of(cx, opt).filter(|&s| on && attr(cx, s, "multiple").is_none()) {
            let options = collect(&cx.dom(), &Coll::Options(sel));
            for o in options {
                cx.dom_mut().control_mut(o).selected = Some(false);
            }
        }
        cx.dom_mut().control_mut(opt).selected = Some(on);
    }

    pub fn get_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        Ok(attr(cx, id, "value").unwrap_or_else(|| get_text_of(cx, id)))
    }

    fn get_text_of(cx: &Cx<'_, '_, '_>, id: NodeId) -> String {
        text_content(&cx.dom(), id)
            .split_ascii_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn set_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "value", &v)
            .map_err(|_| gone())
    }

    pub fn get_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(get_text_of(cx, this.node()?))
    }

    pub fn set_text(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        super::super::node::replace_with_text(cx, this.node()?, &v)
    }

    pub fn get_label(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        Ok(attr(cx, id, "label").unwrap_or_else(|| get_text_of(cx, id)))
    }

    pub fn set_label(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "label", &v)
            .map_err(|_| gone())
    }

    pub fn get_selected(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(selected(cx, this.node()?))
    }

    pub fn set_selected(cx: &mut Cx<'_, '_, '_>, this: Obj, v: bool) -> R<()> {
        set_selected_state(cx, this.node()?, v);
        Ok(())
    }

    pub fn get_index(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        let id = this.node()?;
        let Some(sel) = select_of(cx, id) else {
            return Ok(0);
        };
        Ok(collect(&cx.dom(), &Coll::Options(sel))
            .iter()
            .position(|&o| o == id)
            .map_or(0, |i| i32::try_from(i).unwrap_or(0)))
    }
}

/// `HTMLSelectElement`.
pub mod html_select_element {
    use super::*;
    pub use crate::dom::validation::api::*;

    fn options(cx: &Cx<'_, '_, '_>, id: NodeId) -> Vec<NodeId> {
        collect(&cx.dom(), &Coll::Options(id))
    }

    pub fn get_options(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "options",
            Iface::HTMLOptionsCollection,
            Slot::Collection(Coll::Options(id)),
        ))
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(options(cx, this.node()?).len()).unwrap_or(0))
    }

    pub fn set_length(_cx: &mut Cx<'_, '_, '_>, _this: Obj, _v: u32) -> R<()> {
        Ok(())
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<Option<Obj>> {
        Ok(options(cx, this.node()?)
            .get(i as usize)
            .copied()
            .map(Obj::Node))
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(if attr(cx, this.node()?, "multiple").is_some() {
            "select-multiple"
        } else {
            "select-one"
        }
        .into())
    }

    pub fn get_selected_index(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        let opts = options(cx, this.node()?);
        Ok(opts
            .iter()
            .position(|&o| super::html_option_element::selected(cx, o))
            .map_or(-1, |i| i32::try_from(i).unwrap_or(-1)))
    }

    pub fn set_selected_index(cx: &mut Cx<'_, '_, '_>, this: Obj, i: i32) -> R<()> {
        let opts = options(cx, this.node()?);
        for o in &opts {
            cx.dom_mut().control_mut(*o).selected = Some(false);
        }
        if let Some(&o) = usize::try_from(i).ok().and_then(|i| opts.get(i)) {
            cx.dom_mut().control_mut(o).selected = Some(true);
        }
        Ok(())
    }

    pub fn get_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let opts = options(cx, this.node()?);
        match opts
            .into_iter()
            .find(|&o| super::html_option_element::selected(cx, o))
        {
            Some(o) => super::html_option_element::get_value(cx, Obj::Node(o)),
            None => Ok(String::new()),
        }
    }

    pub fn set_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let opts = options(cx, this.node()?);
        let mut found = false;
        for o in opts {
            let matches = !found && super::html_option_element::get_value(cx, Obj::Node(o))? == v;
            found |= matches;
            cx.dom_mut().control_mut(o).selected = Some(matches);
        }
        Ok(())
    }
}

/// `HTMLButtonElement`.
pub mod html_button_element {
    use super::*;
    pub use crate::dom::validation::api::*;

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let t = attr(cx, this.node()?, "type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        Ok(if matches!(t.as_str(), "submit" | "reset" | "button") {
            t
        } else {
            "submit".into()
        })
    }

    pub fn set_type_(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "type", &v)
            .map_err(|_| gone())
    }

    pub fn get_form(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(form_of(cx, this.node()?).map(Obj::Node))
    }
}

/// `HTMLFormElement`.
pub mod html_form_element {
    use super::*;

    pub fn get_elements(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "elements",
            Iface::HTMLFormControlsCollection,
            Slot::Collection(Coll::FormElements(id)),
        ))
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(collect(&cx.dom(), &Coll::FormElements(this.node()?)).len()).unwrap_or(0))
    }

    /// `action`: the attribute resolved against the document, or the document's URL.
    pub fn get_action(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        let raw = cx
            .dom()
            .get(id)
            .and_then(|n| n.attr("action").map(str::to_string))
            .unwrap_or_default();
        let base = url::Url::parse(&cx.realm.st.borrow().url).ok();
        Ok(if raw.is_empty() {
            base.as_ref().map(|u| u.to_string()).unwrap_or_default()
        } else {
            base.and_then(|b| b.join(raw.trim()).ok())
                .map_or(raw, |u| u.to_string())
        })
    }

    pub fn set_action(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "action", &v)
            .map_err(|_| gone())
    }

    /// An enumerated attribute's value: one of `known` (case-insensitive), else `default`.
    fn enumerated(
        cx: &Cx<'_, '_, '_>,
        id: NodeId,
        attr: &str,
        known: &[&'static str],
        default: &'static str,
    ) -> String {
        let v = cx
            .dom()
            .get(id)
            .and_then(|n| n.attr(attr).map(str::to_ascii_lowercase));
        known
            .iter()
            .find(|k| v.as_deref() == Some(**k))
            .map_or(default, |k| *k)
            .to_string()
    }

    /// `method`: `get`, `post` or `dialog`.
    pub fn get_method(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(enumerated(
            cx,
            this.node()?,
            "method",
            &["get", "post", "dialog"],
            "get",
        ))
    }

    pub fn set_method(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "method", &v)
            .map_err(|_| gone())
    }

    const ENCTYPES: &[&str] = &[
        "application/x-www-form-urlencoded",
        "multipart/form-data",
        "text/plain",
    ];

    /// `enctype`: one of the three form encodings.
    pub fn get_enctype(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(enumerated(
            cx,
            this.node()?,
            "enctype",
            ENCTYPES,
            ENCTYPES[0],
        ))
    }

    pub fn set_enctype(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "enctype", &v)
            .map_err(|_| gone())
    }

    /// `encoding`: the same as `enctype`.
    pub fn get_encoding(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get_enctype(cx, this)
    }

    pub fn set_encoding(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set_enctype(cx, this, v)
    }

    /// `form[name]` and `form.elements[name]`: the control with that id or name, a
    /// `RadioNodeList` when several share it, `None` when none does.
    pub(crate) fn named(cx: &mut Cx<'_, '_, '_>, form: NodeId, name: &str) -> Option<Obj> {
        if name.is_empty() {
            return None;
        }
        let hits = collect(&cx.dom(), &Coll::Radio(form, name.to_string()));
        match hits.as_slice() {
            [] => None,
            [one] => Some(Obj::Node(*one)),
            _ => Some(cx.alloc(
                Iface::RadioNodeList,
                Slot::Collection(Coll::Radio(form, name.to_string())),
            )),
        }
    }

    /// `submit()`: submits without a `submit` event.
    pub fn submit(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        crate::dom::input::submit_form(cx, this.node()?, None, false)
    }

    /// `checkValidity()`.
    pub fn check_validity(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        crate::dom::validation::check_form(cx, this.node()?)
    }

    /// `reportValidity()`.
    pub fn report_validity(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        crate::dom::validation::report_form(cx, this.node()?)
    }

    /// `reset()`.
    pub fn reset(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        crate::dom::input::reset_form(cx, this.node()?)
    }

    /// `requestSubmit()`: fires `submit` (cancelable), then submits.
    pub fn request_submit(cx: &mut Cx<'_, '_, '_>, this: Obj, submitter: Option<Obj>) -> R<()> {
        let s = submitter.map(Obj::node).transpose()?;
        crate::dom::input::submit_form(cx, this.node()?, s, true)
    }
}

/// `HTMLDialogElement`.
pub mod html_dialog_element {
    use super::*;

    pub fn get_return_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        Ok(cx.st().dialog_return.get(&id).cloned().unwrap_or_default())
    }

    pub fn set_return_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let id = this.node()?;
        cx.st().dialog_return.insert(id, v);
        Ok(())
    }

    pub fn show(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = this.node()?;
        if attr(cx, id, "open").is_some() {
            return Ok(());
        }
        cx.dom_mut().set_attr(id, "open", "").map_err(|_| gone())
    }

    pub fn show_modal(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = this.node()?;
        if attr(cx, id, "open").is_some() {
            if cx.st().modals.contains(&id) {
                return Ok(());
            }
            return Err(Error::dom(
                "InvalidStateError",
                "The dialog is already open as a non-modal dialog.",
            ));
        }
        let connected = {
            let dom = cx.dom();
            dom.is_inclusive_ancestor(dom.root(), id)
        };
        if !connected {
            return Err(Error::dom(
                "InvalidStateError",
                "The dialog is not connected.",
            ));
        }
        cx.dom_mut().set_attr(id, "open", "").map_err(|_| gone())?;
        cx.st().modals.push(id);
        // Dialog focusing steps: the first focusable descendant (or one with `autofocus`).
        let candidates: Vec<NodeId> = {
            let dom = cx.dom();
            dom.traverse(id)
                .filter_map(|e| match e {
                    pageon_dom::Edge::Enter(n) if n != id && super::super::is_element(&dom, n) => {
                        Some(n)
                    }
                    _ => None,
                })
                .collect()
        };
        let auto = candidates
            .iter()
            .copied()
            .find(|&n| attr(cx, n, "autofocus").is_some());
        let first = auto.or_else(|| {
            candidates
                .into_iter()
                .find(|&n| crate::dom::html_or_svg_or_math_ml_element::focusable(cx, n))
        });
        crate::dom::event_target::move_focus(cx, first)?;
        Ok(())
    }

    pub fn close(cx: &mut Cx<'_, '_, '_>, this: Obj, rv: Option<String>) -> R<()> {
        let id = this.node()?;
        if attr(cx, id, "open").is_none() {
            return Ok(());
        }
        cx.dom_mut().remove_attr(id, "open").map_err(|_| gone())?;
        cx.st().modals.retain(|&m| m != id);
        if let Some(v) = rv {
            cx.st().dialog_return.insert(id, v);
        }
        crate::dom::event_target::fire(cx, Target::Node(id), "close", false, false)?;
        Ok(())
    }
}

fn href_url(cx: &Cx<'_, '_, '_>, id: NodeId) -> Option<url::Url> {
    let h = attr(cx, id, "href")?;
    let base = url::Url::parse(&cx.realm.st.borrow().url).ok()?;
    base.join(h.trim()).ok()
}

fn set_url(cx: &Cx<'_, '_, '_>, id: NodeId, f: impl FnOnce(&mut url::Url)) -> R<()> {
    if let Some(mut u) = href_url(cx, id) {
        f(&mut u);
        cx.dom_mut()
            .set_attr(id, "href", u.as_str())
            .map_err(|_| gone())?;
    }
    Ok(())
}

/// URL parts of a URL (shared by links and `location`).
pub(crate) fn url_part(u: Option<&url::Url>, part: &str) -> String {
    let Some(u) = u else { return String::new() };
    match part {
        "href" => u.to_string(),
        "origin" => u.origin().ascii_serialization(),
        "protocol" => format!("{}:", u.scheme()),
        "host" => match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => String::new(),
        },
        "hostname" => u.host_str().unwrap_or_default().to_string(),
        "port" => u.port().map(|p| p.to_string()).unwrap_or_default(),
        "pathname" => u.path().to_string(),
        "search" => u
            .query()
            .filter(|q| !q.is_empty())
            .map(|q| format!("?{q}"))
            .unwrap_or_default(),
        "hash" => u
            .fragment()
            .filter(|f| !f.is_empty())
            .map(|f| format!("#{f}"))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// `HTMLHyperlinkElementUtils`.
pub mod html_hyperlink_element_utils {
    use super::*;

    pub fn get_href(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        Ok(href_url(cx, id)
            .map(|u| u.to_string())
            .or_else(|| attr(cx, id, "href"))
            .unwrap_or_default())
    }

    pub fn set_href(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "href", &v)
            .map_err(|_| gone())
    }
}

/// `HyperlinkElementUtils`.
pub mod hyperlink_element_utils {
    use super::*;

    macro_rules! part {
        ($get:ident, $name:literal) => {
            pub fn $get(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
                Ok(url_part(href_url(cx, this.node()?).as_ref(), $name))
            }
        };
    }
    part!(get_origin, "origin");
    part!(get_protocol, "protocol");
    part!(get_host, "host");
    part!(get_hostname, "hostname");
    part!(get_port, "port");
    part!(get_pathname, "pathname");
    part!(get_search, "search");
    part!(get_hash, "hash");

    pub fn set_protocol(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set_url(cx, this.node()?, |u| {
            let _ = u.set_scheme(v.trim_end_matches(':'));
        })
    }

    pub fn set_host(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set_url(cx, this.node()?, |u| {
            let (h, p) = v
                .split_once(':')
                .map_or((v.as_str(), None), |(h, p)| (h, p.parse().ok()));
            let _ = u.set_host(Some(h));
            let _ = u.set_port(p);
        })
    }

    pub fn set_hostname(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set_url(cx, this.node()?, |u| {
            let _ = u.set_host(Some(&v));
        })
    }

    pub fn set_port(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set_url(cx, this.node()?, |u| {
            let _ = u.set_port(v.parse().ok());
        })
    }

    pub fn set_pathname(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set_url(cx, this.node()?, |u| u.set_path(&v))
    }

    pub fn set_search(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set_url(cx, this.node()?, |u| {
            u.set_query(Some(v.trim_start_matches('?')).filter(|q| !q.is_empty()))
        })
    }

    pub fn set_hash(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set_url(cx, this.node()?, |u| {
            u.set_fragment(Some(v.trim_start_matches('#')).filter(|f| !f.is_empty()))
        })
    }
}

/// `HTMLScriptElement`.
pub mod html_script_element {
    use super::*;

    /// `src`: the attribute resolved against the document URL ("" if absent).
    pub fn get_src(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        let Some(src) = attr(cx, id, "src") else {
            return Ok(String::new());
        };
        let base = url::Url::parse(&cx.realm.st.borrow().url).ok();
        Ok(base
            .and_then(|b| b.join(src.trim()).ok())
            .map_or(src, |u| u.to_string()))
    }

    pub fn set_src(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "src", &v)
            .map_err(|_| gone())
    }

    /// `async`: the attribute (pageon runs inserted scripts in insertion order either way).
    pub fn get_async(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(attr(cx, this.node()?, "async").is_some())
    }

    pub fn set_async(cx: &mut Cx<'_, '_, '_>, this: Obj, v: bool) -> R<()> {
        let id = this.node()?;
        let mut dom = cx.dom_mut();
        if v {
            dom.set_attr(id, "async", "").map_err(|_| gone())
        } else {
            dom.remove_attr(id, "async").map(|_| ()).map_err(|_| gone())
        }
    }

    /// `crossOrigin`: the CORS settings attribute (`anonymous`, `use-credentials`, or null).
    pub fn get_cross_origin(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        Ok(attr(cx, this.node()?, "crossorigin").map(|v| {
            if v.eq_ignore_ascii_case("use-credentials") {
                "use-credentials".into()
            } else {
                "anonymous".into()
            }
        }))
    }

    pub fn set_cross_origin(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<String>) -> R<()> {
        let id = this.node()?;
        let mut dom = cx.dom_mut();
        match v {
            Some(v) => dom.set_attr(id, "crossorigin", &v).map_err(|_| gone()),
            None => dom
                .remove_attr(id, "crossorigin")
                .map(|_| ())
                .map_err(|_| gone()),
        }
    }

    /// The script types pageon runs; `speculationrules` and others are not.
    pub fn supports(_cx: &mut Cx<'_, '_, '_>, ty: String) -> R<bool> {
        Ok(matches!(ty.as_str(), "classic" | "module" | "importmap"))
    }

    pub fn get_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        let dom = cx.dom();
        Ok(dom
            .children(id)
            .filter_map(|c| match dom.get(c).map(|n| &n.data) {
                Some(NodeData::Text(t)) => Some(t.clone()),
                _ => None,
            })
            .collect())
    }

    pub fn set_text(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        super::super::node::replace_with_text(cx, this.node()?, &v)
    }
}

/// `HTMLTemplateElement`.
pub mod html_template_element {
    use super::*;

    pub fn get_content(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        match cx.dom().get(id).map(|n| &n.data) {
            Some(NodeData::Element {
                template_contents: Some(t),
                ..
            }) => Ok(Obj::Node(*t)),
            _ => Err(gone()),
        }
    }
}

/// `HTMLTableElement`.
pub mod html_table_element {
    use super::*;

    pub fn get_rows(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "rows",
            Iface::HTMLCollection,
            Slot::Collection(Coll::TableRows(id)),
        ))
    }

    pub fn get_t_bodies(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "tBodies",
            Iface::HTMLCollection,
            Slot::Collection(Coll::TableBodies(id)),
        ))
    }
}

/// `HTMLAnchorElement`.
pub mod html_anchor_element {
    use super::*;

    /// `text`: the link's text content.
    pub fn get_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(text_content(&cx.dom(), this.node()?))
    }

    pub fn set_text(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        crate::dom::node::replace_with_text(cx, this.node()?, &v)
    }
}
