//! Roles: explicit ARIA roles with fallback, implicit roles per HTML-AAM, and presentational
//! conflict resolution.

use pageon_dom::{Dom, Namespace, NodeId};

use crate::Index;

/// Every concrete ARIA 1.2 role (plus DPUB-ARIA and Graphics-ARIA), lowercase.
const ROLES: &[&str] = &[
    "alert",
    "alertdialog",
    "application",
    "article",
    "banner",
    "blockquote",
    "button",
    "caption",
    "cell",
    "checkbox",
    "code",
    "columnheader",
    "combobox",
    "comment",
    "complementary",
    "contentinfo",
    "definition",
    "deletion",
    "dialog",
    "document",
    "emphasis",
    "feed",
    "figure",
    "form",
    "generic",
    "grid",
    "gridcell",
    "group",
    "heading",
    "image",
    "insertion",
    "link",
    "list",
    "listbox",
    "listitem",
    "log",
    "main",
    "mark",
    "marquee",
    "math",
    "menu",
    "menubar",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "meter",
    "navigation",
    "none",
    "note",
    "option",
    "paragraph",
    "progressbar",
    "radio",
    "radiogroup",
    "region",
    "row",
    "rowgroup",
    "rowheader",
    "scrollbar",
    "search",
    "searchbox",
    "sectionfooter",
    "sectionheader",
    "separator",
    "slider",
    "spinbutton",
    "status",
    "strong",
    "subscript",
    "suggestion",
    "superscript",
    "switch",
    "tab",
    "table",
    "tablist",
    "tabpanel",
    "term",
    "textbox",
    "time",
    "timer",
    "toolbar",
    "tooltip",
    "tree",
    "treegrid",
    "treeitem",
    "graphics-document",
    "graphics-object",
    "graphics-symbol",
    "doc-abstract",
    "doc-acknowledgments",
    "doc-afterword",
    "doc-appendix",
    "doc-backlink",
    "doc-biblioentry",
    "doc-bibliography",
    "doc-biblioref",
    "doc-chapter",
    "doc-colophon",
    "doc-conclusion",
    "doc-cover",
    "doc-credit",
    "doc-credits",
    "doc-dedication",
    "doc-endnote",
    "doc-endnotes",
    "doc-epigraph",
    "doc-epilogue",
    "doc-errata",
    "doc-example",
    "doc-footnote",
    "doc-foreword",
    "doc-glossary",
    "doc-glossref",
    "doc-index",
    "doc-introduction",
    "doc-noteref",
    "doc-notice",
    "doc-pagebreak",
    "doc-pagefooter",
    "doc-pageheader",
    "doc-pagelist",
    "doc-part",
    "doc-preface",
    "doc-prologue",
    "doc-pullquote",
    "doc-qna",
    "doc-subtitle",
    "doc-tip",
    "doc-toc",
];

/// Global ARIA attributes: any of them on an element makes `role="none"` resolve to the
/// implicit role (ARIA 1.2 §9.3 presentational role conflict resolution).
const GLOBAL_ATTRS: &[&str] = &[
    "aria-atomic",
    "aria-braillelabel",
    "aria-brailleroledescription",
    "aria-busy",
    "aria-controls",
    "aria-current",
    "aria-describedby",
    "aria-description",
    "aria-details",
    "aria-disabled",
    "aria-dropeffect",
    "aria-errormessage",
    "aria-flowto",
    "aria-grabbed",
    "aria-haspopup",
    "aria-invalid",
    "aria-keyshortcuts",
    "aria-label",
    "aria-labelledby",
    "aria-live",
    "aria-owns",
    "aria-relevant",
    "aria-roledescription",
];

/// The first valid, non-abstract role token of a `role` attribute, with synonyms mapped.
pub(crate) fn explicit(dom: &Dom, ix: &Index<'_>, id: NodeId) -> Option<&'static str> {
    let v = dom.aria(id, "role")?;
    v.split_ascii_whitespace().find_map(|t| {
        let t = t.to_ascii_lowercase();
        let t = match t.as_str() {
            "presentation" => "none",
            "img" => "image",
            "directory" => "list",
            other => other,
        };
        // Landmarks that need a name fall back to the next token without one.
        if matches!(t, "region" | "form") && !author_named(dom, ix, id, true) {
            return None;
        }
        ROLES.iter().copied().find(|r| *r == t)
    })
}

fn tag(dom: &Dom, id: NodeId) -> Option<&str> {
    let n = dom.get(id)?;
    (n.namespace() == Some(Namespace::Html))
        .then(|| n.local_name())
        .flatten()
}

fn ancestors(dom: &Dom, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    std::iter::successors(dom.parent(id), move |&p| dom.parent(p))
}

/// Whether the element is focusable without script (it can take part in conflict resolution).
pub(crate) fn focusable(dom: &Dom, id: NodeId) -> bool {
    let Some(n) = dom.get(id) else { return false };
    if n.attr("tabindex")
        .is_some_and(|t| t.trim().parse::<i32>().is_ok())
    {
        return true;
    }
    match tag(dom, id) {
        Some("a" | "area") => n.attr("href").is_some(),
        Some("button" | "select" | "textarea" | "iframe" | "summary") => {
            n.attr("disabled").is_none()
        }
        Some("input") => {
            n.attr("disabled").is_none()
                && !n
                    .attr("type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("hidden"))
        }
        _ => n
            .attr("contenteditable")
            .is_some_and(|v| v.is_empty() || v.eq_ignore_ascii_case("true")),
    }
}

fn has_global(dom: &Dom, id: NodeId) -> bool {
    dom.get(id).is_some_and(|n| {
        n.attrs()
            .iter()
            .any(|a| GLOBAL_ATTRS.contains(&a.local_name()))
    })
}

/// Whether `aria-label`, `aria-labelledby` (resolving to something) or `title` gives the element
/// an author name; used where HTML-AAM makes the role depend on having a name.
fn author_named(dom: &Dom, ix: &Index<'_>, id: NodeId, title_counts: bool) -> bool {
    let Some(n) = dom.get(id) else { return false };
    if dom
        .aria(id, "aria-label")
        .is_some_and(|v| !v.trim().is_empty())
    {
        return true;
    }
    if let Some(ids) = n.attr("aria-labelledby")
        && ids.split_ascii_whitespace().any(|i| {
            ix.by_id(id, i)
                .is_some_and(|t| !dom.text(t).trim().is_empty())
        })
    {
        return true;
    }
    title_counts && n.attr("title").is_some_and(|v| !v.trim().is_empty())
}

/// The implicit role of an element per HTML-AAM (`generic` when HTML gives none).
pub(crate) fn implicit(dom: &Dom, ix: &Index<'_>, id: NodeId) -> &'static str {
    let Some(n) = dom.get(id) else {
        return "generic";
    };
    match n.namespace() {
        Some(Namespace::Svg) => {
            return if n.local_name() == Some("svg") {
                "graphics-document"
            } else {
                "none"
            };
        }
        Some(Namespace::MathMl) => {
            return if n.local_name() == Some("math") {
                "math"
            } else {
                "none"
            };
        }
        _ => {}
    }
    let attr = |k: &str| n.attr(k);
    let scoped =
        |set: &[&str]| ancestors(dom, id).any(|a| tag(dom, a).is_some_and(|t| set.contains(&t)));
    match n.local_name().unwrap_or_default() {
        "a" | "area" => {
            if attr("href").is_some() {
                "link"
            } else {
                "generic"
            }
        }
        "address" | "details" | "fieldset" | "hgroup" | "optgroup" => "group",
        "article" => "article",
        "aside" => {
            if scoped(&["article", "aside", "nav", "section"]) && !author_named(dom, ix, id, true) {
                "generic"
            } else {
                "complementary"
            }
        }
        "blockquote" => "blockquote",
        "button" => "button",
        "caption" => "caption",
        "code" => "code",
        "datalist" => "listbox",
        "dd" => "definition",
        "del" | "s" => "deletion",
        "dfn" | "dt" => "term",
        "dialog" => "dialog",
        "em" => "emphasis",
        "figure" => "figure",
        "footer" => {
            if scoped(&["article", "aside", "main", "nav", "section"]) {
                "generic"
            } else {
                "contentinfo"
            }
        }
        "header" => {
            if scoped(&["article", "aside", "main", "nav", "section"]) {
                "generic"
            } else {
                "banner"
            }
        }
        "form" => "form",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "hr" => "separator",
        "img" => {
            if attr("alt") == Some("") && !author_named(dom, ix, id, false) {
                "none"
            } else {
                "image"
            }
        }
        "input" => input_role(dom, id),
        "ins" => "insertion",
        "li" => "listitem",
        "main" => "main",
        "mark" => "mark",
        "math" => "math",
        "menu" | "ol" | "ul" => "list",
        "meter" => "meter",
        "nav" => "navigation",
        "option" => "option",
        "output" => "status",
        "p" => "paragraph",
        "progress" => "progressbar",
        "search" => "search",
        "section" => {
            if author_named(dom, ix, id, true) {
                "region"
            } else {
                "generic"
            }
        }
        "select" => {
            let size = attr("size")
                .and_then(|s| s.trim().parse::<u32>().ok())
                .unwrap_or(0);
            if attr("multiple").is_some() || size > 1 {
                "listbox"
            } else {
                "combobox"
            }
        }
        "strong" => "strong",
        "sub" => "subscript",
        "sup" => "superscript",
        "summary" => "button",
        "table" => "table",
        "tbody" | "thead" | "tfoot" => "rowgroup",
        "tr" => "row",
        "td" => {
            if table_role(dom, ix, id).is_some_and(|r| r == "grid" || r == "treegrid") {
                "gridcell"
            } else {
                "cell"
            }
        }
        "th" => th_role(dom, id),
        "textarea" => "textbox",
        "time" => "time",
        "br" | "wbr" => "none",
        _ => "generic",
    }
}

fn input_role(dom: &Dom, id: NodeId) -> &'static str {
    let Some(n) = dom.get(id) else {
        return "textbox";
    };
    let ty = n
        .attr("type")
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let list = n.attr("list").is_some();
    match ty.as_str() {
        // A file input is its "Choose file" button, as in Chrome.
        "button" | "file" | "image" | "reset" | "submit" => "button",
        "checkbox" => {
            if n.attr("switch").is_some() {
                "switch"
            } else {
                "checkbox"
            }
        }
        "radio" => "radio",
        "range" => "slider",
        "number" => "spinbutton",
        "hidden" => "none",
        "search" if !list => "searchbox",
        "" | "text" | "email" | "tel" | "url" | "search" if list => "combobox",
        _ => "textbox",
    }
}

fn table_role(dom: &Dom, ix: &Index<'_>, id: NodeId) -> Option<&'static str> {
    let t = ancestors(dom, id).find(|&a| tag(dom, a) == Some("table"))?;
    Some(explicit(dom, ix, t).unwrap_or("table"))
}

fn th_role(dom: &Dom, id: NodeId) -> &'static str {
    let n = dom.get(id);
    match n
        .and_then(|n| n.attr("scope"))
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("row" | "rowgroup") => return "rowheader",
        Some("col" | "colgroup") => return "columnheader",
        _ => {}
    }
    if ancestors(dom, id).any(|a| tag(dom, a) == Some("thead")) {
        return "columnheader";
    }
    // A header cell followed by data cells in its row heads that row.
    let row = dom.parent(id);
    let has_td = row.is_some_and(|r| dom.children(r).any(|c| tag(dom, c) == Some("td")));
    if has_td { "rowheader" } else { "columnheader" }
}

/// Elements whose implicit role is presentational when their owning container is (`li` in a
/// presentational list, table parts in a presentational table).
fn inherits_presentation(dom: &Dom, ix: &Index<'_>, id: NodeId) -> bool {
    let owner = match tag(dom, id) {
        Some("li") => dom
            .parent(id)
            .filter(|&p| matches!(tag(dom, p), Some("ul" | "ol" | "menu"))),
        Some("tbody" | "thead" | "tfoot" | "tr" | "td" | "th" | "caption") => {
            ancestors(dom, id).find(|&a| tag(dom, a) == Some("table"))
        }
        Some("dt" | "dd") => dom.parent(id).filter(|&p| tag(dom, p) == Some("dl")),
        _ => None,
    };
    owner.is_some_and(|o| explicit(dom, ix, o) == Some("none") && explicit(dom, ix, id).is_none())
}

/// The computed role of an element.
pub(crate) fn role(dom: &Dom, ix: &Index<'_>, id: NodeId) -> &'static str {
    let implicit_role = || implicit(dom, ix, id);
    match explicit(dom, ix, id) {
        Some("none") => {
            if focusable(dom, id) || has_global(dom, id) {
                implicit_role()
            } else {
                "none"
            }
        }
        Some(r) => r,
        None if inherits_presentation(dom, ix, id) => "none",
        None => implicit_role(),
    }
}

#[cfg(test)]
mod tests {
    use pageon_dom::{Edge, ParseOptions, parse_document};

    use super::*;

    fn roles(html: &str) -> Vec<(String, &'static str)> {
        let dom = parse_document(html, ParseOptions::default());
        let ix = Index::new(&dom);
        dom.traverse(dom.root())
            .filter_map(|e| match e {
                Edge::Enter(id) => {
                    let n = dom.get(id)?;
                    let i = n.attr("id")?.to_string();
                    Some((i, role(&dom, &ix, id)))
                }
                Edge::Leave(_) => None,
            })
            .collect()
    }

    #[test]
    fn fallback_conflicts_and_context() {
        let r = roles(
            r#"<div id="a" role="foo button link"></div><div id="b" role="widget"></div>
            <button id="c" role="none">x</button><span id="d" role="presentation" aria-describedby="a">x</span>
            <ul role="none"><li id="e">x</li></ul><section id="f">x</section><section id="g" aria-label="s">x</section>
            <img id="h" alt=""><input id="i" type="search" list="l"><table><tr><th id="j">h</th><td>d</td></tr></table>"#,
        );
        let want = [
            ("a", "button"),
            ("b", "generic"),
            ("c", "button"),
            ("d", "generic"),
            ("e", "none"),
            ("f", "generic"),
            ("g", "region"),
            ("h", "none"),
            ("i", "combobox"),
            ("j", "rowheader"),
        ];
        for (id, role) in want {
            assert_eq!(
                r.iter().find(|(i, _)| i == id).map(|(_, r)| *r),
                Some(role),
                "#{id}"
            );
        }
    }
}
