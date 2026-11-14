//! Human-readable dumps: readable text (an innerText approximation without CSS) and an element
//! outline. Both walk iteratively, so deep trees cannot overflow the stack.

use crate::dom::{Dom, Edge, NodeData, NodeId};

/// Elements whose subtree is never readable text.
const SKIP: &[&str] = &["script", "style", "template", "noscript", "head", "title"];
/// Elements followed by a line break in the text dump.
const BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
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
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
    "details",
    "summary",
    "caption",
];
/// Inline elements followed by a word boundary (form controls render as separate words).
const WORD: &[&str] = &[
    "option", "button", "select", "textarea", "label", "input", "img", "meter", "progress",
];
/// Attributes shown in the tree dump.
const KEY_ATTRS: &[&str] = &[
    "id",
    "class",
    "href",
    "src",
    "name",
    "type",
    "role",
    "aria-label",
];

impl Dom {
    /// Readable text in document order: script/style/template/noscript/head excluded,
    /// whitespace collapsed, a line break after block elements, cells separated by tabs.
    pub fn text(&self, id: NodeId) -> String {
        let mut out = String::new();
        let mut skip_depth = 0usize;
        let mut pre_depth = 0usize;
        for edge in self.traverse(id) {
            match edge {
                Edge::Enter(n) => {
                    let Some(node) = self.get(n) else { continue };
                    match &node.data {
                        NodeData::Element { .. } => {
                            let name = node.local_name().unwrap_or_default();
                            if skip_depth > 0 || SKIP.contains(&name) {
                                skip_depth += 1;
                            }
                            if name == "pre" || name == "textarea" {
                                pre_depth += 1;
                            }
                        }
                        NodeData::Text(t) if skip_depth == 0 => {
                            if pre_depth > 0 {
                                out.push_str(t);
                            } else {
                                push_collapsed(&mut out, t);
                            }
                        }
                        _ => {}
                    }
                }
                Edge::Leave(n) => {
                    let Some(node) = self.get(n) else { continue };
                    if let Some(name) = node.local_name() {
                        if skip_depth > 0 {
                            skip_depth -= 1;
                            continue;
                        }
                        if name == "pre" || name == "textarea" {
                            pre_depth = pre_depth.saturating_sub(1);
                        }
                        if WORD.contains(&name)
                            && !out.is_empty()
                            && !out.ends_with([' ', '\n', '\t'])
                        {
                            out.push(' ');
                        }
                        if name == "td" || name == "th" {
                            trim_end_spaces(&mut out);
                            out.push('\t');
                        } else if BLOCK.contains(&name) {
                            trim_end_spaces(&mut out);
                            if name == "br" || (!out.is_empty() && !out.ends_with('\n')) {
                                out.push('\n');
                            }
                        }
                    }
                }
            }
        }
        let lines: Vec<&str> = out.lines().map(str::trim_end).collect();
        let mut text = lines.join("\n");
        while text.contains("\n\n\n") {
            text = text.replace("\n\n\n", "\n\n");
        }
        text.trim().to_string()
    }

    /// One element per line: depth, name, key attributes, up to 80 characters of quoted
    /// direct text. Page text is always quoted.
    pub fn tree(&self, id: NodeId) -> String {
        let mut out = String::new();
        let mut depth = 0usize;
        for edge in self.traverse(id) {
            match edge {
                Edge::Enter(n) => {
                    let Some(node) = self.get(n) else { continue };
                    if let NodeData::Element { .. } = &node.data {
                        out.push_str(&"  ".repeat(depth));
                        out.push_str(node.local_name().unwrap_or_default());
                        for key in KEY_ATTRS {
                            if let Some(v) = node.attr(key) {
                                out.push_str(&format!(" {key}={}", quote(&clip(v, 80))));
                            }
                        }
                        let direct: String = self
                            .children(n)
                            .filter_map(|c| match self.get(c).map(|x| &x.data) {
                                Some(NodeData::Text(t)) => Some(t.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        let direct = collapse(&direct);
                        if !direct.is_empty() {
                            out.push(' ');
                            out.push_str(&quote(&clip(&direct, 80)));
                        }
                        out.push('\n');
                        depth += 1;
                    }
                }
                Edge::Leave(n) => {
                    if matches!(self.get(n).map(|x| &x.data), Some(NodeData::Element { .. })) {
                        depth = depth.saturating_sub(1);
                    }
                }
            }
        }
        out
    }
}

fn push_collapsed(out: &mut String, t: &str) {
    let needs_space = |out: &String| !out.is_empty() && !out.ends_with([' ', '\n', '\t']);
    if t.trim().is_empty() {
        if !t.is_empty() && needs_space(out) {
            out.push(' ');
        }
        return;
    }
    let lead = t.starts_with(char::is_whitespace);
    for (i, word) in t.split_whitespace().enumerate() {
        if (i > 0 || lead) && needs_space(out) {
            out.push(' ');
        }
        out.push_str(word);
    }
    if t.ends_with(char::is_whitespace) && needs_space(out) {
        out.push(' ');
    }
}

fn trim_end_spaces(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut c: String = s.chars().take(max - 1).collect();
    c.push('…');
    c
}

/// JSON-style quoting, the same rule as the observation format.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
