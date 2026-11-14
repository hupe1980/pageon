//! Constructable stylesheets and `adoptedStyleSheets` (CSSOM §6.1).
//!
//! A constructed sheet is a list of rule texts in the realm. A document or shadow root keeps
//! the array the page set or read (`ObservableArray` semantics: `push` works, the array keeps
//! its identity); after each task the realm writes each tree's adopted sheet texts into the DOM
//! ([`pageon_dom::Dom::set_adopted`]), where layout applies them after the tree's own sheets.

use pageon_dom::NodeId;

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{CSSStyleSheetInit, Iface};
use crate::realm::Slot;

/// The realm's constructed sheets and the trees adopting them.
#[derive(Debug, Default)]
pub struct Sheets {
    sheets: Vec<Vec<String>>,
    adopters: Vec<(NodeId, v8::Global<v8::Array>)>,
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

fn sheet_of(cx: &Cx<'_, '_, '_>, this: Obj) -> R<usize> {
    match cx.st().slot(this.slab()?) {
        Some(Slot::Sheet(i)) => Ok(*i as usize),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
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
    sync(cx);
    Ok(())
}

/// Writes each adopting tree's sheet texts into the DOM (after each task and on change).
pub(crate) fn sync(cx: &mut Cx<'_, '_, '_>) {
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

    pub fn replace_sync(cx: &mut Cx<'_, '_, '_>, this: Obj, text: String) -> R<()> {
        let sheet = sheet_of(cx, this)?;
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
