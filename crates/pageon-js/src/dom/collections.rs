//! Collections, token lists, style declarations and rects.

use pageon_dom::NodeId;

use super::{collect, gone};
use crate::cv::{Cx, Error, Obj, R};
use crate::realm::Slot;

pub(crate) fn slot(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Slot> {
    let id = this.slab()?;
    cx.realm
        .st
        .borrow()
        .slot(id)
        .cloned()
        .ok_or_else(|| Error::Type("Illegal invocation".into()))
}

/// The nodes of a list or collection object, now.
pub(crate) fn items(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Vec<NodeId>> {
    Ok(match slot(cx, this)? {
        Slot::Static(v) => v,
        Slot::ChildNodes(p) => cx.dom().children(p).collect(),
        Slot::Collection(c) => collect(&cx.dom(), &c),
        _ => return Err(Error::Type("Illegal invocation".into())),
    })
}

/// `NodeList`.
pub mod node_list {
    use super::*;

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(items(cx, this)?.len()).unwrap_or(u32::MAX))
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<Option<Obj>> {
        Ok(items(cx, this)?.get(i as usize).copied().map(Obj::Node))
    }
}

/// `HTMLCollection`.
pub mod html_collection {
    use super::*;

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(items(cx, this)?.len()).unwrap_or(u32::MAX))
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<Option<Obj>> {
        Ok(items(cx, this)?.get(i as usize).copied().map(Obj::Node))
    }

    pub fn named_item(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Option<Obj>> {
        if name.is_empty() {
            return Ok(None);
        }
        let all = items(cx, this)?;
        let dom = cx.dom();
        Ok(all
            .into_iter()
            .find(|&n| {
                dom.get(n).is_some_and(|x| {
                    x.attr("id") == Some(name.as_str()) || x.attr("name") == Some(name.as_str())
                })
            })
            .map(Obj::Node))
    }
}

/// `RadioNodeList`: the controls of a form sharing a name.
pub mod radio_node_list {
    use super::*;

    fn radios(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Vec<NodeId>> {
        let dom = cx.dom();
        Ok(items(cx, this)?
            .into_iter()
            .filter(|&n| {
                dom.get(n).is_some_and(|x| x.is_html("input"))
                    && pageon_dom::forms::input_type(&dom, n) == "radio"
            })
            .collect())
    }

    /// The checked radio button's value (`on` without a `value`), or empty.
    pub fn get_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        for n in radios(cx, this)? {
            if crate::dom::html_input_element::get_checked(cx, Obj::Node(n))? {
                return Ok(cx
                    .dom()
                    .get(n)
                    .and_then(|x| x.attr("value").map(str::to_string))
                    .unwrap_or_else(|| "on".into()));
            }
        }
        Ok(String::new())
    }

    /// Checks the first radio button with that value.
    pub fn set_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        for n in radios(cx, this)? {
            let value = cx
                .dom()
                .get(n)
                .and_then(|x| x.attr("value").map(str::to_string))
                .unwrap_or_else(|| "on".into());
            if value == v {
                return crate::dom::html_input_element::set_checked(cx, Obj::Node(n), true);
            }
        }
        Ok(())
    }
}

/// `DOMTokenList` (e.g. `classList`), backed by an attribute.
pub mod dom_token_list {
    use super::*;

    fn parts(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(NodeId, &'static str)> {
        match slot(cx, this)? {
            Slot::TokenList(n, a) => Ok((n, a)),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    fn tokens(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Vec<String>> {
        let (n, a) = parts(cx, this)?;
        let v = cx
            .dom()
            .get(n)
            .and_then(|x| x.attr(a))
            .unwrap_or_default()
            .to_string();
        let mut out: Vec<String> = Vec::new();
        for t in v.split_ascii_whitespace() {
            if !out.iter().any(|o| o == t) {
                out.push(t.to_string());
            }
        }
        Ok(out)
    }

    fn write(cx: &Cx<'_, '_, '_>, this: Obj, tokens: &[String]) -> R<()> {
        let (n, a) = parts(cx, this)?;
        cx.dom_mut()
            .set_attr(n, a, &tokens.join(" "))
            .map_err(|_| gone())
    }

    fn check(token: &str) -> R<()> {
        if token.is_empty() {
            return Err(Error::dom(
                "SyntaxError",
                "The token provided must not be empty.",
            ));
        }
        if token.chars().any(|c| c.is_ascii_whitespace()) {
            return Err(Error::dom(
                "InvalidCharacterError",
                format!("The token provided ('{token}') contains HTML space characters."),
            ));
        }
        Ok(())
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(tokens(cx, this)?.len()).unwrap_or(u32::MAX))
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<Option<String>> {
        Ok(tokens(cx, this)?.get(i as usize).cloned())
    }

    pub fn contains(cx: &mut Cx<'_, '_, '_>, this: Obj, t: String) -> R<bool> {
        Ok(tokens(cx, this)?.contains(&t))
    }

    pub fn add(cx: &mut Cx<'_, '_, '_>, this: Obj, add: Vec<String>) -> R<()> {
        for t in &add {
            check(t)?;
        }
        let mut list = tokens(cx, this)?;
        for t in add {
            if !list.contains(&t) {
                list.push(t);
            }
        }
        write(cx, this, &list)
    }

    pub fn remove(cx: &mut Cx<'_, '_, '_>, this: Obj, remove: Vec<String>) -> R<()> {
        for t in &remove {
            check(t)?;
        }
        let mut list = tokens(cx, this)?;
        list.retain(|t| !remove.contains(t));
        write(cx, this, &list)
    }

    pub fn toggle(cx: &mut Cx<'_, '_, '_>, this: Obj, t: String, force: Option<bool>) -> R<bool> {
        check(&t)?;
        let mut list = tokens(cx, this)?;
        let has = list.contains(&t);
        let want = force.unwrap_or(!has);
        if want && !has {
            list.push(t);
            write(cx, this, &list)?;
        } else if !want && has {
            list.retain(|x| *x != t);
            write(cx, this, &list)?;
        }
        Ok(want)
    }

    pub fn replace(cx: &mut Cx<'_, '_, '_>, this: Obj, old: String, new: String) -> R<bool> {
        check(&old)?;
        check(&new)?;
        let mut list = tokens(cx, this)?;
        let Some(i) = list.iter().position(|t| *t == old) else {
            return Ok(false);
        };
        if list.contains(&new) {
            list.remove(i);
        } else {
            list[i] = new;
        }
        write(cx, this, &list)?;
        Ok(true)
    }

    pub fn get_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let (n, a) = parts(cx, this)?;
        Ok(cx
            .dom()
            .get(n)
            .and_then(|x| x.attr(a))
            .unwrap_or_default()
            .to_string())
    }

    pub fn set_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let (n, a) = parts(cx, this)?;
        cx.dom_mut().set_attr(n, a, &v).map_err(|_| gone())
    }
}

/// A style declaration's target.
#[derive(Clone, Copy)]
pub(crate) enum Decl {
    Inline(NodeId),
    Computed(NodeId),
}

pub(crate) fn decl(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Decl> {
    match slot(cx, this)? {
        Slot::Style(n) => Ok(Decl::Inline(n)),
        Slot::Computed(n) => Ok(Decl::Computed(n)),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// Declarations of a `style` attribute: (property, value, important).
pub(crate) fn parse_style(text: &str) -> Vec<(String, String, bool)> {
    let mut out: Vec<(String, String, bool)> = Vec::new();
    for d in split_declarations(text) {
        let Some((name, value)) = d.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let mut value = value.trim().to_string();
        let important = value.to_ascii_lowercase().ends_with("!important");
        if important {
            value.truncate(value.len() - "!important".len());
            value = value.trim().to_string();
        }
        if name.is_empty() || value.is_empty() {
            continue;
        }
        out.retain(|(n, _, _)| *n != name);
        out.push((name, value, important));
    }
    out
}

/// Splits on `;` outside parentheses and quotes.
fn split_declarations(text: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth, mut quote) = (Vec::new(), String::new(), 0i32, None::<char>);
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ';') if depth <= 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out
}

fn serialize_style(decls: &[(String, String, bool)]) -> String {
    decls
        .iter()
        .map(|(n, v, i)| format!("{n}: {v}{};", if *i { " !important" } else { "" }))
        .collect::<Vec<_>>()
        .join(" ")
}

fn inline_decls(cx: &Cx<'_, '_, '_>, n: NodeId) -> Vec<(String, String, bool)> {
    parse_style(
        cx.dom()
            .get(n)
            .and_then(|x| x.attr("style"))
            .unwrap_or_default(),
    )
}

fn write_inline(cx: &Cx<'_, '_, '_>, n: NodeId, decls: &[(String, String, bool)]) -> R<()> {
    let text = serialize_style(decls);
    let mut dom = cx.dom_mut();
    if text.is_empty() {
        dom.remove_attr(n, "style")
    } else {
        dom.set_attr(n, "style", &text)
    }
    .map_err(|_| gone())
}

fn read_only() -> Error {
    Error::dom(
        "NoModificationAllowedError",
        "Computed styles are read-only.",
    )
}

/// A camelCase or dashed property name as CSS spells it, if it is a property.
pub(crate) fn css_name(name: &str) -> Option<String> {
    let dashed = if name.contains('-') {
        name.to_ascii_lowercase()
    } else {
        let mut s = String::new();
        for (i, c) in name.chars().enumerate() {
            if c.is_ascii_uppercase() {
                if i > 0 || name.starts_with("webkit") || name.starts_with("Webkit") {
                    s.push('-');
                }
                s.push(c.to_ascii_lowercase());
            } else {
                s.push(c);
            }
        }
        s
    };
    let dashed = if dashed == "css-float" {
        "float".to_string()
    } else {
        dashed
    };
    style::properties::PropertyId::parse_enabled_for_all_content(&dashed)
        .ok()
        .map(|_| dashed)
}

/// The value of a property of a declaration.
pub(crate) fn property(cx: &Cx<'_, '_, '_>, d: Decl, name: &str) -> String {
    match d {
        Decl::Inline(n) => inline_decls(cx, n)
            .into_iter()
            .find(|(p, _, _)| p == name)
            .map(|(_, v, _)| v)
            .unwrap_or_default(),
        Decl::Computed(n) => cx.realm.layout().style(n, name).unwrap_or_default(),
    }
}

/// Sets a property (an empty value removes it).
pub(crate) fn set_property_value(
    cx: &Cx<'_, '_, '_>,
    d: Decl,
    name: &str,
    value: &str,
    important: bool,
) -> R<()> {
    let Decl::Inline(n) = d else {
        return Err(read_only());
    };
    let mut decls = inline_decls(cx, n);
    decls.retain(|(p, _, _)| p != name);
    let v = value.trim();
    if !v.is_empty() {
        decls.push((name.to_string(), v.to_string(), important));
    }
    write_inline(cx, n, &decls)
}

/// `CSSStyleDeclaration`.
pub mod css_style_declaration {
    use super::*;

    pub fn get_css_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        match decl(cx, this)? {
            Decl::Inline(n) => Ok(serialize_style(&inline_decls(cx, n))),
            Decl::Computed(_) => Ok(String::new()),
        }
    }

    pub fn set_css_text(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let Decl::Inline(n) = decl(cx, this)? else {
            return Err(read_only());
        };
        write_inline(cx, n, &parse_style(&v))
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        match decl(cx, this)? {
            Decl::Inline(n) => Ok(u32::try_from(inline_decls(cx, n).len()).unwrap_or(0)),
            Decl::Computed(_) => Ok(0),
        }
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<String> {
        match decl(cx, this)? {
            Decl::Inline(n) => Ok(inline_decls(cx, n)
                .get(i as usize)
                .map(|(p, _, _)| p.clone())
                .unwrap_or_default()),
            Decl::Computed(_) => Ok(String::new()),
        }
    }

    pub fn get_property_value(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<String> {
        let d = decl(cx, this)?;
        let name = if name.starts_with("--") {
            name
        } else {
            name.to_ascii_lowercase()
        };
        Ok(property(cx, d, &name))
    }

    pub fn get_property_priority(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<String> {
        let Decl::Inline(n) = decl(cx, this)? else {
            return Ok(String::new());
        };
        let name = name.to_ascii_lowercase();
        Ok(
            if inline_decls(cx, n).iter().any(|(p, _, i)| *p == name && *i) {
                "important".into()
            } else {
                String::new()
            },
        )
    }

    pub fn set_property(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        name: String,
        value: String,
        priority: String,
    ) -> R<()> {
        let d = decl(cx, this)?;
        let name = if name.starts_with("--") {
            name
        } else {
            name.to_ascii_lowercase()
        };
        if !name.starts_with("--") && css_name(&name).is_none() {
            return Ok(()); // unknown properties are ignored, as in browsers
        }
        set_property_value(
            cx,
            d,
            &name,
            &value,
            priority.eq_ignore_ascii_case("important"),
        )
    }

    pub fn remove_property(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<String> {
        let d = decl(cx, this)?;
        let name = name.to_ascii_lowercase();
        let old = property(cx, d, &name);
        set_property_value(cx, d, &name, "", false)?;
        Ok(old)
    }
}

/// `CSSStyleProperties`.
pub mod css_style_properties {
    use super::*;

    pub fn get_css_float(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let d = decl(cx, this)?;
        Ok(property(cx, d, "float"))
    }

    pub fn set_css_float(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let d = decl(cx, this)?;
        set_property_value(cx, d, "float", &v, false)
    }
}

fn rect(cx: &Cx<'_, '_, '_>, this: Obj) -> R<[f64; 4]> {
    match slot(cx, this)? {
        Slot::Rect(r) => Ok(r),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// `DOMRectReadOnly`.
pub mod dom_rect_read_only {
    use super::*;

    pub fn get_x(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(rect(cx, this)?[0])
    }

    pub fn get_y(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(rect(cx, this)?[1])
    }

    pub fn get_width(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(rect(cx, this)?[2])
    }

    pub fn get_height(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(rect(cx, this)?[3])
    }

    pub fn get_top(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let r = rect(cx, this)?;
        Ok(r[1].min(r[1] + r[3]))
    }

    pub fn get_bottom(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let r = rect(cx, this)?;
        Ok(r[1].max(r[1] + r[3]))
    }

    pub fn get_left(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let r = rect(cx, this)?;
        Ok(r[0].min(r[0] + r[2]))
    }

    pub fn get_right(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let r = rect(cx, this)?;
        Ok(r[0].max(r[0] + r[2]))
    }
}

/// `DOMRectList`.
pub mod dom_rect_list {
    use super::*;

    fn list(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Vec<[f64; 4]>> {
        match slot(cx, this)? {
            Slot::RectList(v) => Ok(v),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(list(cx, this)?.len()).unwrap_or(0))
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<Option<Obj>> {
        let r = list(cx, this)?.get(i as usize).copied();
        Ok(r.map(|r| cx.alloc(crate::generated::Iface::DOMRect, Slot::Rect(r))))
    }
}

fn element_of(cx: &Cx<'_, '_, '_>, this: Obj) -> R<NodeId> {
    match slot(cx, this)? {
        Slot::NamedNodeMap(n) => Ok(n),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// The attribute names of an element, in order.
pub(crate) fn attr_names(cx: &Cx<'_, '_, '_>, el: NodeId) -> Vec<String> {
    cx.dom()
        .get(el)
        .map(|n| {
            n.attrs()
                .iter()
                .map(pageon_dom::Attribute::qualified_name)
                .collect()
        })
        .unwrap_or_default()
}

/// `NamedNodeMap` (`element.attributes`, live).
pub mod named_node_map {
    use super::*;

    fn attr(cx: &mut Cx<'_, '_, '_>, el: NodeId, name: String) -> Obj {
        cx.alloc(crate::generated::Iface::Attr, Slot::Attr(el, name))
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        let el = element_of(cx, this)?;
        Ok(u32::try_from(attr_names(cx, el).len()).unwrap_or(0))
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<Option<Obj>> {
        let el = element_of(cx, this)?;
        let name = attr_names(cx, el).into_iter().nth(i as usize);
        Ok(name.map(|n| attr(cx, el, n)))
    }

    pub fn get_named_item(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Option<Obj>> {
        let el = element_of(cx, this)?;
        let html = cx
            .dom()
            .get(el)
            .is_some_and(|n| n.namespace() == Some(pageon_dom::Namespace::Html));
        let name = if html {
            name.to_ascii_lowercase()
        } else {
            name
        };
        let found = attr_names(cx, el).into_iter().find(|n| *n == name);
        Ok(found.map(|n| attr(cx, el, n)))
    }

    pub fn get_named_item_ns(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        _ns: Option<String>,
        local: String,
    ) -> R<Option<Obj>> {
        let el = element_of(cx, this)?;
        let found = attr_names(cx, el)
            .into_iter()
            .find(|n| n.rsplit(':').next() == Some(local.as_str()));
        Ok(found.map(|n| attr(cx, el, n)))
    }
}

/// `Attr`.
pub mod attr {
    use super::*;

    fn parts(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(NodeId, String)> {
        match slot(cx, this)? {
            Slot::Attr(n, name) => Ok((n, name)),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_namespace_uri(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<String>> {
        Ok(None)
    }

    pub fn get_prefix(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        let (_, name) = parts(cx, this)?;
        Ok(name.split_once(':').map(|(p, _)| p.to_string()))
    }

    pub fn get_local_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let (_, name) = parts(cx, this)?;
        Ok(name.rsplit(':').next().unwrap_or(&name).to_string())
    }

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(parts(cx, this)?.1)
    }

    pub fn get_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let (el, name) = parts(cx, this)?;
        let dom = cx.dom();
        Ok(dom
            .get(el)
            .and_then(|n| n.attrs().iter().find(|a| a.qualified_name() == name))
            .map(|a| a.value.clone())
            .unwrap_or_default())
    }

    pub fn set_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let (el, name) = parts(cx, this)?;
        cx.dom_mut().set_attr(el, &name, &v).map_err(|_| gone())
    }

    pub fn get_owner_element(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(Some(Obj::Node(parts(cx, this)?.0)))
    }

    pub fn get_specified(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(true)
    }
}
