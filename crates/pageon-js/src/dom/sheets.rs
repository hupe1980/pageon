//! Stylesheets in the CSSOM: constructed ones and `adoptedStyleSheets` (CSSOM §6.1), and the
//! sheets of `<style>` and `<link>` elements with `document.styleSheets`. Rules a script
//! inserts into a `<style>`'s sheet (CSS-in-JS libraries do) are written to the DOM
//! ([`pageon_dom::Dom::set_cssom_text`]), where layout applies them instead of its text.
//!
//! A constructed sheet is a list of rule texts in the realm. A document or shadow root keeps
//! the array the page set or read (`ObservableArray` semantics: `push` works, the array keeps
//! its identity); after each task the realm writes each tree's adopted sheet texts into the DOM
//! ([`pageon_dom::Dom::set_adopted`]), where layout applies them after the tree's own sheets.

use pageon_dom::NodeId;

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{CSSStyleSheetInit, Iface};
use crate::realm::Slot;

/// The realm's sheets and the trees adopting them.
#[derive(Debug, Default)]
pub struct Sheets {
    sheets: Vec<Vec<String>>,
    adopters: Vec<(NodeId, v8::Global<v8::Array>)>,
    /// Sheets of `<style>` and `<link>` elements, by sheet index.
    owned: std::collections::HashMap<usize, Owned>,
    by_node: std::collections::HashMap<NodeId, usize>,
    /// Each sheet object's `cssRules` object (slab ids).
    rule_lists: std::collections::HashMap<u32, u32>,
}

/// An element's sheet.
#[derive(Debug)]
struct Owned {
    node: NodeId,
    /// A `<link>`'s sheet: its rules are not readable here.
    link: bool,
    /// The element text the rules come from.
    text: String,
    /// A script changed the rules.
    changed: bool,
    disabled: bool,
}

impl Sheets {
    /// Bytes of rule text held.
    pub fn bytes(&self) -> usize {
        self.sheets.iter().flatten().map(String::len).sum()
    }
}

/// Splits CSS into top-level rules (statements up to `;` or a block), dropping `@import`
/// statements as constructed sheets do.
fn split(css: &str) -> Vec<String> {
    let b = css.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut start, mut depth) = (0, 0, 0usize);
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = css[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                continue;
            }
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    out.push(css[start..=i].trim().to_string());
                    start = i + 1;
                }
            }
            b';' if depth == 0 => {
                out.push(css[start..=i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let rest = css[start.min(css.len())..].trim();
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out.retain(|r| {
        !r.is_empty()
            && !r
                .get(..7)
                .is_some_and(|p| p.eq_ignore_ascii_case("@import"))
    });
    out
}

fn sheet_of(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<usize> {
    let i = match cx.st().slot(this.slab()?) {
        Some(Slot::Sheet(i)) => *i as usize,
        _ => return Err(Error::Type("Illegal invocation".into())),
    };
    refresh(cx, i);
    Ok(i)
}

/// A `<link>`'s rules: not readable (Chrome refuses them across origins; pageon keeps a
/// link's CSS outside the page).
fn readable(cx: &Cx<'_, '_, '_>, sheet: usize, what: &str) -> R<()> {
    if cx.st().sheets.owned.get(&sheet).is_some_and(|o| o.link) {
        return Err(Error::dom(
            "SecurityError",
            format!("Failed to {what}: Cannot access rules"),
        ));
    }
    Ok(())
}

/// The text of a `<style>`.
fn style_text(cx: &Cx<'_, '_, '_>, node: NodeId) -> String {
    let dom = cx.dom();
    dom.children(node)
        .filter_map(|c| match dom.get(c).map(|n| &n.data) {
            Some(pageon_dom::NodeData::Text(t)) => Some(t.to_string()),
            _ => None,
        })
        .collect()
}

/// Brings an element's sheet up to date with its text: rules a script inserted hold until
/// the text changes, which starts the sheet over (as a new sheet in browsers).
fn refresh(cx: &mut Cx<'_, '_, '_>, sheet: usize) {
    let Some((node, link, text, changed)) = cx
        .st()
        .sheets
        .owned
        .get(&sheet)
        .map(|o| (o.node, o.link, o.text.clone(), o.changed))
    else {
        return;
    };
    if link {
        return;
    }
    let now = style_text(cx, node);
    if now == text && !(changed && cx.dom().cssom_text(node).is_none()) {
        return;
    }
    let rules = split(&now);
    {
        let mut st = cx.st();
        st.sheets.sheets[sheet] = rules;
        if let Some(o) = st.sheets.owned.get_mut(&sheet) {
            o.text = now;
            o.changed = false;
        }
    }
    write_owned(cx, sheet);
}

/// Writes an element sheet's rules to the DOM for layout (`None` while its text applies).
fn write_owned(cx: &mut Cx<'_, '_, '_>, sheet: usize) {
    let css = {
        let st = cx.st();
        let Some(o) = st.sheets.owned.get(&sheet) else {
            return;
        };
        if o.link {
            return;
        }
        let css = if o.disabled {
            Some(String::new())
        } else if o.changed {
            Some(st.sheets.sheets[sheet].join("\n"))
        } else {
            None
        };
        (o.node, css)
    };
    if cx.dom().cssom_text(css.0) != css.1.as_deref() {
        cx.dom_mut().set_cssom_text(css.0, css.1);
    }
}

/// The sheet of a connected `<style>`, or of a `<link rel=stylesheet href>`; `None` otherwise.
pub(crate) fn sheet_for(cx: &mut Cx<'_, '_, '_>, node: NodeId) -> R<Option<Obj>> {
    let (style, link) = {
        let dom = cx.dom();
        let Some(n) = dom.get(node) else {
            return Ok(None);
        };
        let link = n.is_html("link")
            && n.attr("href").is_some()
            && n.attr("rel").is_some_and(|r| {
                r.split_ascii_whitespace()
                    .any(|t| t.eq_ignore_ascii_case("stylesheet"))
            });
        (n.is_html("style"), link)
    };
    if !(style || link) || !cx.dom().is_connected(node) {
        return Ok(None);
    }
    let found = cx.st().sheets.by_node.get(&node).copied();
    let i = match found {
        Some(i) => i,
        None => {
            let text = if style {
                style_text(cx, node)
            } else {
                String::new()
            };
            let mut st = cx.st();
            st.sheets.sheets.push(split(&text));
            let i = st.sheets.sheets.len() - 1;
            st.sheets.owned.insert(
                i,
                Owned {
                    node,
                    link,
                    text,
                    changed: false,
                    disabled: false,
                },
            );
            st.sheets.by_node.insert(node, i);
            i
        }
    };
    refresh(cx, i);
    let slot = Slot::Sheet(u32::try_from(i).unwrap_or(u32::MAX));
    Ok(Some(super::same(
        cx,
        node,
        "sheet",
        Iface::CSSStyleSheet,
        slot,
    )))
}

/// The sheets of a tree's `<style>` and `<link rel=stylesheet>` elements, in tree order.
fn tree_sheets(cx: &mut Cx<'_, '_, '_>, root: NodeId) -> R<Vec<Obj>> {
    let nodes: Vec<NodeId> = {
        let dom = cx.dom();
        dom.traverse(root)
            .filter_map(|e| match e {
                pageon_dom::Edge::Enter(id)
                    if dom
                        .get(id)
                        .is_some_and(|n| n.is_html("style") || n.is_html("link")) =>
                {
                    Some(id)
                }
                _ => None,
            })
            .collect()
    };
    let mut out = Vec::new();
    for n in nodes {
        if let Some(s) = sheet_for(cx, n)? {
            out.push(s);
        }
    }
    Ok(out)
}

/// `document.styleSheets` (or a shadow root's).
pub(crate) fn get_style_sheets(cx: &mut Cx<'_, '_, '_>, root: NodeId) -> R<Obj> {
    Ok(super::same(
        cx,
        root,
        "styleSheets",
        Iface::StyleSheetList,
        Slot::SheetList(root),
    ))
}

/// The rules of the sheet object `sheet` (a slab id), brought up to date.
fn rules_of(cx: &mut Cx<'_, '_, '_>, sheet: u32) -> R<(usize, Vec<String>)> {
    let i = sheet_of(cx, Obj::Slab(sheet))?;
    readable(cx, i, "read the 'cssRules' property from 'CSSStyleSheet'")?;
    let rules = cx.st().sheets.sheets[i].clone();
    Ok((i, rules))
}

/// Replaces a sheet's rules, within the per-document cap.
fn set_rules(cx: &mut Cx<'_, '_, '_>, sheet: usize, rules: Vec<String>) -> R<()> {
    let old: usize = cx.st().sheets.sheets[sheet].iter().map(String::len).sum();
    let new: usize = rules.iter().map(String::len).sum();
    if crate::dom::internals::side_bytes(cx) - old + new > crate::dom::internals::MAX_SIDE_BYTES {
        crate::dom::internals::refuse(
            cx,
            "sheets",
            "CSSStyleSheet: the document's stylesheets and form values exceed 16 MiB; the rules are not kept".into(),
        );
        return Ok(());
    }
    cx.st().sheets.sheets[sheet] = rules;
    let text = cx
        .st()
        .sheets
        .owned
        .get(&sheet)
        .map(|o| o.node)
        .map(|n| style_text(cx, n));
    if let Some(text) = text {
        if let Some(o) = cx.st().sheets.owned.get_mut(&sheet) {
            o.text = text;
            o.changed = true;
        }
        write_owned(cx, sheet);
    }
    sync(cx);
    Ok(())
}

/// Writes each adopting tree's sheet texts into the DOM (after each task and on change).
pub(crate) fn sync(cx: &mut Cx<'_, '_, '_>) {
    let changed: Vec<usize> = cx
        .st()
        .sheets
        .owned
        .iter()
        .filter(|(_, o)| o.changed || o.disabled)
        .map(|(i, _)| *i)
        .collect();
    for i in changed {
        refresh(cx, i);
    }
    if cx.st().sheets.adopters.is_empty() {
        return;
    }
    let adopters: Vec<(NodeId, v8::Global<v8::Array>)> = cx
        .st()
        .sheets
        .adopters
        .iter()
        .map(|(n, a)| (*n, a.clone()))
        .collect();
    for (root, arr) in adopters {
        let arr = v8::Local::new(cx.scope, &arr);
        let mut ids: Vec<usize> = Vec::new();
        for i in 0..arr.length() {
            let Some(v) = arr.get_index(cx.scope, i) else {
                continue;
            };
            if let Ok(Obj::Slab(s)) = crate::cv::obj(cx, v, Iface::CSSStyleSheet)
                && let Some(Slot::Sheet(k)) = cx.st().slot(s)
            {
                let k = *k as usize;
                // A sheet adopted twice applies once, at its later position.
                ids.retain(|&x| x != k);
                ids.push(k);
            }
        }
        let texts: Vec<String> = {
            let st = cx.st();
            ids.iter()
                .filter_map(|&k| st.sheets.sheets.get(k))
                .map(|rules| rules.join("\n"))
                .collect()
        };
        if cx.dom().adopted(root) != texts.as_slice() {
            cx.dom_mut().set_adopted(root, texts);
        }
    }
}

/// `DocumentOrShadowRoot.adoptedStyleSheets`.
pub(crate) fn get_adopted(cx: &mut Cx<'_, '_, '_>, root: NodeId) -> R<Value> {
    let found = cx
        .st()
        .sheets
        .adopters
        .iter()
        .find(|(n, _)| *n == root)
        .map(|(_, a)| a.clone());
    let arr = match found {
        Some(a) => a,
        None => {
            let a = v8::Array::new(cx.scope, 0);
            let g = v8::Global::new(cx.scope, a);
            cx.st().sheets.adopters.push((root, g.clone()));
            g
        }
    };
    let local: v8::Local<v8::Value> = v8::Local::new(cx.scope, &arr).into();
    Ok(v8::Global::new(cx.scope, local))
}

pub(crate) fn set_adopted(cx: &mut Cx<'_, '_, '_>, root: NodeId, sheets: Vec<Obj>) -> R<()> {
    let items: Vec<v8::Local<v8::Value>> = sheets.into_iter().map(|s| cx.wrap(s).into()).collect();
    let arr = v8::Array::new_with_elements(cx.scope, &items);
    let g = v8::Global::new(cx.scope, arr);
    {
        let mut st = cx.st();
        st.sheets.adopters.retain(|(n, _)| *n != root);
        st.sheets.adopters.push((root, g));
    }
    sync(cx);
    Ok(())
}

/// `CSSStyleSheet` (constructed sheets only).
pub mod css_style_sheet {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, _options: CSSStyleSheetInit) -> R<Obj> {
        let i = {
            let mut st = cx.st();
            st.sheets.sheets.push(Vec::new());
            u32::try_from(st.sheets.sheets.len() - 1).unwrap_or(u32::MAX)
        };
        Ok(cx.alloc(Iface::CSSStyleSheet, Slot::Sheet(i)))
    }

    pub fn get_css_rules(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let me = this.slab()?;
        let i = sheet_of(cx, this)?;
        readable(cx, i, "read the 'cssRules' property from 'CSSStyleSheet'")?;
        if let Some(&l) = cx.st().sheets.rule_lists.get(&me) {
            return Ok(Obj::Slab(l));
        }
        let l = cx.alloc(Iface::CSSRuleList, Slot::RuleList(me));
        if let Obj::Slab(id) = l {
            cx.st().sheets.rule_lists.insert(me, id);
        }
        Ok(l)
    }

    pub fn get_rules(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        get_css_rules(cx, this)
    }

    pub fn get_owner_rule(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(None)
    }

    pub fn replace_sync(cx: &mut Cx<'_, '_, '_>, this: Obj, text: String) -> R<()> {
        let sheet = sheet_of(cx, this)?;
        if cx.st().sheets.owned.contains_key(&sheet) {
            return Err(Error::dom(
                "NotAllowedError",
                "Failed to execute 'replaceSync' on 'CSSStyleSheet': Can't call replaceSync on non-constructed CSSStyleSheets.",
            ));
        }
        set_rules(cx, sheet, split(&text))
    }

    pub fn replace(cx: &mut Cx<'_, '_, '_>, this: Obj, text: String) -> R<Value> {
        replace_sync(cx, this, text)?;
        let resolver = v8::PromiseResolver::new(cx.scope)
            .ok_or_else(|| Error::Type("replace: no promise".into()))?;
        let w: v8::Local<v8::Value> = cx.wrap(this).into();
        resolver.resolve(cx.scope, w);
        let p: v8::Local<v8::Value> = resolver.get_promise(cx.scope).into();
        Ok(v8::Global::new(cx.scope, p))
    }

    pub fn insert_rule(cx: &mut Cx<'_, '_, '_>, this: Obj, rule: String, index: u32) -> R<u32> {
        let sheet = sheet_of(cx, this)?;
        readable(cx, sheet, "execute 'insertRule' on 'CSSStyleSheet'")?;
        let parsed = split(&rule);
        if rule
            .trim_start()
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("@import"))
        {
            return Err(Error::dom(
                "SyntaxError",
                "Failed to execute 'insertRule' on 'CSSStyleSheet': Can't insert @import rules into a constructed stylesheet.",
            ));
        }
        let [one] = parsed.as_slice() else {
            return Err(Error::dom(
                "SyntaxError",
                format!(
                    "Failed to execute 'insertRule' on 'CSSStyleSheet': Failed to parse the rule '{rule}'."
                ),
            ));
        };
        let mut rules = cx.st().sheets.sheets[sheet].clone();
        let i = index as usize;
        if i > rules.len() {
            return Err(Error::dom(
                "IndexSizeError",
                format!(
                    "Failed to execute 'insertRule' on 'CSSStyleSheet': The index provided ({index}) is larger than the maximum index ({}).",
                    rules.len()
                ),
            ));
        }
        rules.insert(i, one.clone());
        set_rules(cx, sheet, rules)?;
        Ok(index)
    }

    pub fn delete_rule(cx: &mut Cx<'_, '_, '_>, this: Obj, index: u32) -> R<()> {
        let sheet = sheet_of(cx, this)?;
        readable(cx, sheet, "execute 'deleteRule' on 'CSSStyleSheet'")?;
        let mut rules = cx.st().sheets.sheets[sheet].clone();
        if index as usize >= rules.len() {
            return Err(Error::dom(
                "IndexSizeError",
                format!(
                    "Failed to execute 'deleteRule' on 'CSSStyleSheet': The index provided ({index}) is larger than the maximum index ({}).",
                    rules.len().saturating_sub(1)
                ),
            ));
        }
        rules.remove(index as usize);
        set_rules(cx, sheet, rules)
    }
}

/// `StyleSheet`.
pub mod style_sheet {
    use super::*;

    fn owner(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<(NodeId, bool)>> {
        let i = sheet_of(cx, this)?;
        Ok(cx.st().sheets.owned.get(&i).map(|o| (o.node, o.link)))
    }

    pub fn get_type_(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("text/css".into())
    }

    pub fn get_href(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        match owner(cx, this)? {
            Some((n, true)) => crate::dom::html_link_element::get_href(cx, Obj::Node(n)).map(Some),
            _ => Ok(None),
        }
    }

    pub fn get_owner_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(owner(cx, this)?.map(|(n, _)| Obj::Node(n)))
    }

    pub fn get_title(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        Ok(match owner(cx, this)? {
            Some((n, _)) => cx
                .dom()
                .get(n)
                .and_then(|e| e.attr("title"))
                .filter(|t| !t.is_empty())
                .map(str::to_string),
            None => None,
        })
    }

    pub fn get_disabled(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        let i = sheet_of(cx, this)?;
        Ok(cx.st().sheets.owned.get(&i).is_some_and(|o| o.disabled))
    }

    pub fn set_disabled(cx: &mut Cx<'_, '_, '_>, this: Obj, v: bool) -> R<()> {
        let i = sheet_of(cx, this)?;
        if let Some(o) = cx.st().sheets.owned.get_mut(&i) {
            o.disabled = v;
        }
        write_owned(cx, i);
        Ok(())
    }
}

/// `LinkStyle`: `style.sheet`, `link.sheet`.
pub mod link_style {
    use super::*;

    pub fn get_sheet(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        sheet_for(cx, this.node()?)
    }
}

/// `StyleSheetList`.
pub mod style_sheet_list {
    use super::*;

    fn root(cx: &Cx<'_, '_, '_>, this: Obj) -> R<NodeId> {
        match cx.st().slot(this.slab()?) {
            Some(Slot::SheetList(r)) => Ok(*r),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        let r = root(cx, this)?;
        Ok(u32::try_from(tree_sheets(cx, r)?.len()).unwrap_or(u32::MAX))
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<Option<Obj>> {
        let r = root(cx, this)?;
        Ok(tree_sheets(cx, r)?.get(i as usize).copied())
    }
}

/// `CSSRuleList`.
pub mod css_rule_list {
    use super::*;

    fn sheet(cx: &Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        match cx.st().slot(this.slab()?) {
            Some(Slot::RuleList(s)) => Ok(*s),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        let s = sheet(cx, this)?;
        Ok(u32::try_from(rules_of(cx, s)?.1.len()).unwrap_or(u32::MAX))
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<Option<Obj>> {
        let s = sheet(cx, this)?;
        if i as usize >= rules_of(cx, s)?.1.len() {
            return Ok(None);
        }
        Ok(Some(cx.alloc(Iface::CSSRule, Slot::Rule(s, i))))
    }
}

/// `CSSRule`: its text, kind and sheet.
pub mod css_rule {
    use super::*;

    fn at(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(u32, u32)> {
        match cx.st().slot(this.slab()?) {
            Some(Slot::Rule(s, i)) => Ok((*s, *i)),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_css_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let (s, i) = at(cx, this)?;
        Ok(rules_of(cx, s)?
            .1
            .get(i as usize)
            .cloned()
            .unwrap_or_default())
    }

    /// Setting `cssText` does nothing (CSSOM).
    pub fn set_css_text(_cx: &mut Cx<'_, '_, '_>, _this: Obj, _v: String) -> R<()> {
        Ok(())
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        let text = get_css_text(cx, this)?.to_ascii_lowercase();
        Ok(
            match text
                .split(|c: char| c.is_whitespace() || c == '{' || c == '(')
                .next()
            {
                Some("@import") => 3,
                Some("@media") => 4,
                Some("@font-face") => 5,
                Some("@page") => 6,
                Some("@keyframes") | Some("@-webkit-keyframes") => 7,
                Some("@namespace") => 10,
                Some("@supports") => 12,
                Some(w) if w.starts_with('@') => 0,
                _ => 1,
            },
        )
    }

    pub fn get_parent_style_sheet(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(Some(Obj::Slab(at(cx, this)?.0)))
    }

    pub fn get_parent_rule(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn splits_top_level_rules_and_drops_imports() {
        assert_eq!(
            split("@import url(x.css); p { color: red } @media (x) { a { b: c } } /* } */ q{}"),
            vec![
                "p { color: red }",
                "@media (x) { a { b: c } }",
                "/* } */ q{}"
            ]
        );
        assert_eq!(
            split("a { content: '}' } b{}"),
            vec!["a { content: '}' }", "b{}"]
        );
    }
}
