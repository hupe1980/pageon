//! Accessible names per AccName 1.2 (steps 2A–2I) with the HTML-AAM host-language rules.

use std::collections::HashSet;

use pageon_dom::{Dom, Namespace, NodeData, NodeId};
use pageon_layout::Layout;

/// Longest name kept (bytes); longer names are cut at a character boundary.
const MAX_NAME: usize = 64 * 1024;

/// Roles whose name may come from their content (ARIA 1.2 plus DPUB link roles).
const FROM_CONTENT: &[&str] = &[
    "button",
    "cell",
    "checkbox",
    "columnheader",
    "gridcell",
    "heading",
    "link",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "row",
    "rowheader",
    "sectionheader",
    "sectionfooter",
    "switch",
    "tab",
    "tooltip",
    "treeitem",
    "doc-backlink",
    "doc-biblioref",
    "doc-glossref",
    "doc-noteref",
];

/// Roles that may not be named by authors (ARIA 1.2 "name prohibited").
const PROHIBITED: &[&str] = &[
    "caption",
    "code",
    "deletion",
    "emphasis",
    "generic",
    "insertion",
    "none",
    "paragraph",
    "strong",
    "subscript",
    "superscript",
];

/// Controls whose value stands in for their name when they are embedded in another name (2E).
const EMBEDDED: &[&str] = &[
    "textbox",
    "searchbox",
    "combobox",
    "listbox",
    "slider",
    "spinbutton",
    "progressbar",
    "meter",
    "scrollbar",
];

/// Elements that separate words in name-from-content when no layout is available.
const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];

#[derive(Clone, Copy)]
struct Cx {
    /// Inside an `aria-labelledby` traversal (2B is not applied again).
    in_labelledby: bool,
    /// Called recursively (from content, a label, or a reference).
    recursion: bool,
    /// Hidden nodes are included: the traversal started at a hidden referenced node.
    allow_hidden: bool,
}

pub(crate) struct Namer<'a> {
    pub(crate) dom: &'a Dom,
    pub(crate) ix: &'a crate::Index<'a>,
    pub(crate) layout: Option<&'a Layout>,
    pub(crate) role: &'a dyn Fn(NodeId) -> &'static str,
    pub(crate) hidden: &'a dyn Fn(NodeId) -> bool,
}

impl Namer<'_> {
    /// The accessible name of `id`, whitespace-normalised.
    pub(crate) fn name(&self, id: NodeId) -> String {
        let role = (self.role)(id);
        if PROHIBITED.contains(&role) {
            // Authors may not name these roles, but browsers expose `title` (abbr, iframe).
            return self
                .dom
                .get(id)
                .and_then(|n| n.attr("title"))
                .map(normalize)
                .unwrap_or_default();
        }
        let mut visited = HashSet::new();
        let cx = Cx {
            in_labelledby: false,
            recursion: false,
            allow_hidden: false,
        };
        normalize(&self.compute(id, cx, &mut visited))
    }

    fn el(&self, id: NodeId) -> Option<(&str, &pageon_dom::Node)> {
        let n = self.dom.get(id)?;
        let local = n.local_name()?;
        Some((local, n))
    }

    fn compute(&self, id: NodeId, cx: Cx, visited: &mut HashSet<NodeId>) -> String {
        if !visited.insert(id) {
            return String::new();
        }
        let Some(node) = self.dom.get(id) else {
            return String::new();
        };
        if let NodeData::Text(t) = &node.data {
            // 2A for text: text inside hidden subtrees is skipped unless referenced.
            if !cx.allow_hidden && self.dom.parent(id).is_some_and(|p| (self.hidden)(p)) {
                return String::new();
            }
            // 2G, with the parent's `text-transform` applied as rendered.
            let transform = self.layout.filter(|l| l.error().is_none()).and_then(|l| {
                self.dom
                    .parent(id)
                    .and_then(|p| l.style(p, "text-transform"))
            });
            return match transform.as_deref() {
                Some("uppercase") => t.to_uppercase(),
                Some("lowercase") => t.to_lowercase(),
                Some("capitalize") => capitalize(t),
                _ => t.to_string(),
            };
        }
        let Some((local, n)) = self.el(id) else {
            return String::new();
        };
        // 2A: hidden and not part of a traversal that started at a hidden referenced node.
        if !cx.allow_hidden && (self.hidden)(id) {
            return String::new();
        }
        // 2B: aria-labelledby (not followed again inside a labelledby traversal).
        if !cx.in_labelledby
            && let Some(ids) = n.attr("aria-labelledby")
        {
            let parts: Vec<String> = ids
                .split_ascii_whitespace()
                .filter_map(|i| self.ix.by_id(id, i))
                .map(|t| {
                    let sub = Cx {
                        in_labelledby: true,
                        recursion: true,
                        allow_hidden: (self.hidden)(t),
                    };
                    // Each reference starts its own traversal (the same node may be referenced twice).
                    let mut seen = visited.clone();
                    seen.remove(&t);
                    self.compute(t, sub, &mut seen)
                })
                .collect();
            let joined = parts.join(" ");
            if !joined.trim().is_empty() {
                return joined;
            }
        }
        let role = (self.role)(id);
        let embedded = cx.recursion && EMBEDDED.contains(&role);
        // 2C: aria-label (an embedded control in recursion uses its value instead).
        if !embedded
            && let Some(l) = self
                .dom
                .aria(id, "aria-label")
                .filter(|l| !l.trim().is_empty())
        {
            return l.to_string();
        }
        // 2E: an embedded control inside another name contributes its value.
        if embedded {
            return self.embedded_value(id, role);
        }
        // 2D: host language text alternatives (not for presentational elements).
        if (role != "none" || local == "img")
            && let Some(s) = self.native(id, local, n, cx, visited)
            && (!s.trim().is_empty() || local == "img")
        {
            return s;
        }
        // 2F: name from content.
        if (FROM_CONTENT.contains(&role) || cx.recursion || is_label_like(local))
            && !(PROHIBITED.contains(&role) && !cx.recursion)
        {
            let sub = Cx {
                recursion: true,
                ..cx
            };
            let s = self.content(id, sub, visited);
            // Inside a recursion, whitespace-only content still separates words.
            if !s.trim().is_empty() || (cx.recursion && !s.is_empty()) {
                return s;
            }
        }
        // 2I: tooltip.
        if let Some(t) = n.attr("title").filter(|t| !t.trim().is_empty()) {
            return t.to_string();
        }
        if matches!(local, "input" | "textarea")
            && let Some(p) = n.attr("placeholder").filter(|t| !t.trim().is_empty())
        {
            return p.to_string();
        }
        String::new()
    }

    fn content(&self, id: NodeId, cx: Cx, visited: &mut HashSet<NodeId>) -> String {
        let mut out = String::new();
        for c in self.dom.children(id) {
            let block = self.is_block(c);
            let s = self.compute(c, cx, visited);
            if block {
                out.push(' ');
            }
            out.push_str(&s);
            if block {
                out.push(' ');
            }
            if out.len() > MAX_NAME {
                break;
            }
        }
        out
    }

    fn is_block(&self, id: NodeId) -> bool {
        let Some((local, n)) = self.el(id) else {
            return false;
        };
        if n.namespace() != Some(Namespace::Html) {
            return false;
        }
        if let Some(l) = self.layout.filter(|l| l.error().is_none())
            && let Some(d) = l.style(id, "display")
        {
            // Only inline content runs on; inline-block and other boxes separate words.
            return d != "inline" && d != "contents" && d != "none";
        }
        BLOCK.contains(&local)
    }

    /// 2D: HTML host-language alternatives.
    fn native(
        &self,
        id: NodeId,
        local: &str,
        n: &pageon_dom::Node,
        cx: Cx,
        visited: &mut HashSet<NodeId>,
    ) -> Option<String> {
        if n.namespace() == Some(Namespace::Svg) {
            return self
                .dom
                .children(id)
                .find(|&c| self.dom.get(c).and_then(|x| x.local_name()) == Some("title"))
                .map(|t| self.dom.text(t));
        }
        if n.namespace() != Some(Namespace::Html) {
            return None;
        }
        let ty = n
            .attr("type")
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match local {
            "input" if matches!(ty.as_str(), "button" | "submit" | "reset") => {
                let v = n.attr("value").map(str::to_string);
                if v.as_deref().is_some_and(|v| !v.trim().is_empty()) {
                    return v;
                }
                if let Some(l) = self
                    .labels(id, cx, visited)
                    .filter(|l| !l.trim().is_empty())
                {
                    return Some(l);
                }
                v.or_else(|| match ty.as_str() {
                    "submit" => Some("Submit".into()),
                    "reset" => Some("Reset".into()),
                    _ => None,
                })
            }
            "input" if ty == "image" => n
                .attr("alt")
                .filter(|a| !a.trim().is_empty())
                .or_else(|| n.attr("value").filter(|a| !a.trim().is_empty()))
                .or_else(|| n.attr("title").filter(|a| !a.trim().is_empty()))
                .map(str::to_string)
                .or_else(|| Some("Submit".into())),
            "input" | "select" | "textarea" | "meter" | "progress" | "output" => {
                let l = self.labels(id, cx, visited);
                if l.as_deref().is_some_and(|l| !l.trim().is_empty()) {
                    return l;
                }
                None // title (2I), then placeholder
            }
            "img" | "area" => n.attr("alt").map(str::to_string),
            "fieldset" => self.first_child(id, "legend").map(|c| {
                self.content(
                    c,
                    Cx {
                        recursion: true,
                        ..cx
                    },
                    visited,
                )
            }),
            "table" => self.first_child(id, "caption").map(|c| {
                self.content(
                    c,
                    Cx {
                        recursion: true,
                        ..cx
                    },
                    visited,
                )
            }),
            "figure" => self.first_child(id, "figcaption").map(|c| {
                self.content(
                    c,
                    Cx {
                        recursion: true,
                        ..cx
                    },
                    visited,
                )
            }),
            "optgroup" => n.attr("label").map(str::to_string),
            "option" => n
                .attr("label")
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string),
            _ => None,
        }
    }

    fn first_child(&self, id: NodeId, local: &str) -> Option<NodeId> {
        self.dom
            .children(id)
            .find(|&c| self.dom.get(c).is_some_and(|x| x.is_html(local)))
    }

    /// The text of the `<label>`s of a labelable control: `label[for=id]` in document order,
    /// then the wrapping label.
    fn labels(&self, id: NodeId, cx: Cx, visited: &mut HashSet<NodeId>) -> Option<String> {
        let mut labels: Vec<NodeId> = Vec::new();
        if let Some(own) = self
            .dom
            .get(id)
            .and_then(|n| n.attr("id"))
            .filter(|s| !s.is_empty())
        {
            labels.extend_from_slice(self.ix.labels_for(id, own));
        }
        let wrapping = std::iter::successors(self.dom.parent(id), |&p| self.dom.parent(p))
            .find(|&a| self.dom.get(a).is_some_and(|x| x.is_html("label")));
        if let Some(w) = wrapping {
            let for_other = self
                .dom
                .get(w)
                .and_then(|x| x.attr("for"))
                .is_some_and(|f| self.dom.get(id).and_then(|n| n.attr("id")) != Some(f));
            if !for_other && !labels.contains(&w) {
                labels.push(w);
            }
        }
        if labels.is_empty() {
            return None;
        }
        let parts: Vec<String> = labels
            .into_iter()
            .filter(|&l| cx.allow_hidden || !(self.hidden)(l))
            .map(|l| {
                let mut seen = visited.clone();
                self.content(
                    l,
                    Cx {
                        recursion: true,
                        ..cx
                    },
                    &mut seen,
                )
            })
            .collect();
        Some(parts.join(" "))
    }

    /// 2E: the value of an embedded control.
    fn embedded_value(&self, id: NodeId, role: &str) -> String {
        let Some((local, n)) = self.el(id) else {
            return String::new();
        };
        match role {
            "textbox" | "searchbox" => match local {
                "textarea" => self.dom.text(id),
                "input" => n.attr("value").unwrap_or_default().to_string(),
                _ => crate::text_content(self.dom, id),
            },
            "combobox" | "listbox" => {
                if local == "select" {
                    crate::state::selected_options(self.dom, id)
                        .first()
                        .map(|&o| crate::text_content(self.dom, o))
                        .unwrap_or_default()
                } else {
                    n.attr("value").unwrap_or_default().to_string()
                }
            }
            _ => n
                .attr("aria-valuetext")
                .or_else(|| n.attr("aria-valuenow"))
                .or_else(|| n.attr("value"))
                .unwrap_or_default()
                .to_string(),
        }
    }
}

/// Upper-cases the first letter of each word.
fn capitalize(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut start = true;
    for c in t.chars() {
        if start && c.is_alphabetic() {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
        start = c.is_whitespace();
    }
    out
}

fn is_label_like(local: &str) -> bool {
    matches!(local, "label" | "legend" | "caption" | "figcaption")
}

/// Collapses whitespace runs to single spaces and trims; caps the length.
pub(crate) fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_NAME));
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
        if out.len() > MAX_NAME {
            let mut cut = MAX_NAME;
            while !out.is_char_boundary(cut) {
                cut -= 1;
            }
            out.truncate(cut);
            break;
        }
    }
    out
}
