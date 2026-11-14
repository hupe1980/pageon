//! The `markdown` view: the page's main content as Markdown, for reading.
//!
//! - Main content: the `main` landmark, else the only `<article>`, else the body without
//!   banner, navigation, complementary and footer landmarks.
//! - Visible text only (computed style and boxes), so text hidden from sighted users, which is
//!   where injected instructions hide, never reaches the reader.
//! - Links are absolute and carry the element's `ax` ref in the title slot: `[Docs](https://… "e4")`.
//! - Page text is escaped, so it cannot forge links, emphasis or headings.
//! - Cut to the budget, keeping the head and the tail.

use std::collections::HashMap;
use std::fmt::Write as _;

use pageon_ax::Tree;
use pageon_dom::{Dom, Edge, NodeData, NodeId};
use pageon_layout::{Layout, Viewport, Visibility};
use url::Url;

use crate::{Budget, Document};

/// Landmarks left out when the main content falls back to the body.
const CHROME: &[&str] = &[
    "banner",
    "navigation",
    "complementary",
    "contentinfo",
    "search",
];
/// Elements that never contribute text.
const SKIP: &[&str] = &[
    "head", "script", "style", "template", "noscript", "iframe", "object", "embed", "canvas",
    "svg", "button", "input", "select", "textarea", "option", "datalist", "dialog", "nav", "label",
];
/// Elements that start and end a block.
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "main",
    "header",
    "footer",
    "aside",
    "form",
    "fieldset",
    "figure",
    "figcaption",
    "address",
    "details",
    "summary",
    "legend",
    "caption",
    "center",
    "body",
    "html",
    "hgroup",
    "dl",
    "dd",
    "dt",
    "search",
];

struct Md<'a> {
    dom: &'a Dom,
    layout: Option<&'a Layout>,
    tree: &'a Tree,
    base: Option<Url>,
    refs: HashMap<NodeId, String>,
    skip: Vec<NodeId>,
    out: String,
    line: String,
}

pub(crate) fn render(doc: &Document, viewport: Viewport, budget: Budget) -> String {
    let layout = doc.layout(viewport);
    let usable = layout.error().is_none();
    // Shadow trees: the composed page, as it renders.
    let flat = doc.flat_dom();
    let dom: &pageon_dom::Dom = &flat;
    let tree = Tree::build(dom, Some(&layout));
    let capture = doc.capture(viewport);
    let nav = capture.meta.nav_counter;
    let refs = pageon_agent::ref_map(&capture)
        .into_iter()
        .filter(|(k, _)| k.frame_id == "F0" && k.nav == nav)
        .filter_map(|(k, r)| {
            let (index, generation) = crate::capture::dom_index(k.backend_id)?;
            dom.traverse(dom.root())
                .find_map(|e| match e {
                    Edge::Enter(id)
                        if id.index() == index && id.generation() & 0xffff == generation =>
                    {
                        Some(id)
                    }
                    _ => None,
                })
                .map(|id| (id, r))
        })
        .collect();
    let mut md = Md {
        dom,
        layout: usable.then_some(&layout),
        tree: &tree,
        base: Url::parse(doc.final_url()).ok(),
        refs,
        skip: Vec::new(),
        out: String::new(),
        line: String::new(),
    };
    let (root, skip) = main_content(dom, &tree);
    md.skip = skip;
    if let Some(root) = root {
        md.block(root, 0);
        md.flush();
    }
    let title = tree
        .nodes()
        .first()
        .map(|n| n.name.as_str())
        .unwrap_or_default();
    let mut header = format!(
        "@markdown v1 {} {} budget:{budget}",
        pageon_agent::quote::quote(title),
        doc.final_url().replace(' ', "%20")
    );
    if !usable {
        header.push_str(" layout:unavailable");
    }
    let body = md.out.trim().to_string();
    let full = if body.is_empty() {
        header.clone()
    } else {
        format!("{header}\n\n{body}")
    };
    if budget.count(&full) <= budget.max_tokens {
        return full;
    }
    let room = Budget {
        max_tokens: budget.max_tokens.saturating_sub(budget.count(&header) + 2),
        ..budget
    };
    format!("{header}\n\n{}", pageon_agent::cut_head_tail(&body, room))
}

/// The main-content root, and subtrees to leave out.
fn main_content(dom: &Dom, tree: &Tree) -> (Option<NodeId>, Vec<NodeId>) {
    let rendered = |n: &pageon_ax::AxNode| {
        n.ignored
            .is_none_or(|i| i == pageon_ax::Ignored::Presentational)
    };
    let by_role = |role: &str| {
        tree.nodes()
            .iter()
            .filter(|n| n.role == role && rendered(n))
            .map(|n| n.dom)
            .collect::<Vec<_>>()
    };
    if let Some(&main) = by_role("main").first() {
        return (Some(main), Vec::new());
    }
    let articles = by_role("article");
    if articles.len() == 1 {
        return (Some(articles[0]), Vec::new());
    }
    let body = dom.traverse(dom.root()).find_map(|e| match e {
        Edge::Enter(id) if dom.get(id).is_some_and(|n| n.is_html("body")) => Some(id),
        _ => None,
    });
    let skip = tree
        .nodes()
        .iter()
        .filter(|n| CHROME.contains(&n.role))
        .map(|n| n.dom)
        .collect();
    (body.or(Some(dom.root())), skip)
}

/// Escapes Markdown syntax in page text.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '*' | '_' | '`' | '[' | ']' | '<' | '>' | '#' | '|' | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl Md<'_> {
    fn tag(&self, id: NodeId) -> &str {
        self.dom
            .get(id)
            .filter(|n| n.namespace() == Some(pageon_dom::Namespace::Html))
            .and_then(|n| n.local_name())
            .unwrap_or("")
    }

    fn attr(&self, id: NodeId, k: &str) -> Option<&str> {
        self.dom.get(id).and_then(|n| n.attr(k))
    }

    /// Whether the element's content is shown to a sighted reader.
    fn shown(&self, id: NodeId) -> bool {
        if self.skip.contains(&id) {
            return false;
        }
        match self.layout {
            // Clipped away (`overflow: hidden`, `clip`): no one sees it.
            Some(l) if l.clipped(id) => false,
            Some(l) => match l.visibility(id) {
                Visibility::Visible => true,
                Visibility::NotRendered => false,
                // No box of its own: `display: contents` or an empty inline; its children decide.
                Visibility::Hidden => {
                    l.style(id, "display").as_deref() == Some("contents")
                        || (l
                            .style(id, "opacity")
                            .and_then(|o| o.parse::<f32>().ok())
                            .unwrap_or(1.0)
                            > 0.01
                            && !matches!(
                                l.style(id, "visibility").as_deref(),
                                Some("hidden" | "collapse")
                            )
                            && l.box_of(id).is_none())
                }
            },
            None => self
                .tree
                .node(id)
                .is_none_or(|n| n.ignored != Some(pageon_ax::Ignored::NotRendered)),
        }
    }

    fn flush(&mut self) {
        let text = self.line.split_whitespace().collect::<Vec<_>>().join(" ");
        self.line.clear();
        if !text.is_empty() {
            // A paragraph may not pose as the view's header line.
            if text.starts_with('@') {
                self.out.push('\\');
            }
            self.out.push_str(&text);
            self.out.push_str("\n\n");
        }
    }

    fn block(&mut self, id: NodeId, depth: usize) {
        for c in self.dom.children(id).collect::<Vec<_>>() {
            self.node(c, depth);
        }
    }

    fn node(&mut self, id: NodeId, depth: usize) {
        let Some(n) = self.dom.get(id) else { return };
        match &n.data {
            NodeData::Text(t) => {
                self.line.push_str(&escape(t));
                return;
            }
            NodeData::Element { .. } => {}
            _ => return,
        }
        let tag = self.tag(id).to_string();
        if SKIP.contains(&tag.as_str()) || !self.shown(id) {
            return;
        }
        match tag.as_str() {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                let level = usize::from(tag.as_bytes()[1] - b'0');
                let text = self.inline(id);
                if !text.is_empty() {
                    let _ = write!(self.out, "{} {text}\n\n", "#".repeat(level));
                }
            }
            "ul" | "ol" | "menu" => {
                self.flush();
                self.list(id, tag == "ol", depth);
                if depth == 0 {
                    self.out.push('\n');
                }
            }
            "pre" => {
                self.flush();
                let text = self.dom.text(id);
                let fence = if text.contains("```") { "````" } else { "```" };
                let _ = write!(
                    self.out,
                    "{fence}\n{}\n{fence}\n\n",
                    text.trim_end_matches('\n')
                );
            }
            "blockquote" => {
                self.flush();
                let saved = std::mem::take(&mut self.out);
                self.block(id, depth);
                self.flush();
                let inner = std::mem::replace(&mut self.out, saved);
                for l in inner.trim().lines() {
                    self.out.push_str(if l.is_empty() { ">" } else { "> " });
                    self.out.push_str(l);
                    self.out.push('\n');
                }
                self.out.push('\n');
            }
            "table" => {
                self.flush();
                self.table(id);
            }
            "hr" => {
                self.flush();
                self.out.push_str("---\n\n");
            }
            "br" => self.line.push_str("  \n"),
            _ if BLOCKS.contains(&tag.as_str()) => {
                self.flush();
                if tag == "dt" {
                    let text = self.inline(id);
                    if !text.is_empty() {
                        let _ = write!(self.out, "**{text}**\n\n");
                    }
                } else {
                    self.block(id, depth);
                    self.flush();
                }
            }
            _ => {
                let text = self.inline_element(id);
                self.line.push_str(&text);
            }
        }
    }

    /// Inline Markdown of an element's content.
    fn inline(&mut self, id: NodeId) -> String {
        let mut s = String::new();
        for c in self.dom.children(id).collect::<Vec<_>>() {
            let Some(n) = self.dom.get(c) else { continue };
            match &n.data {
                NodeData::Text(t) => s.push_str(&escape(t)),
                NodeData::Element { .. } => {
                    let t = self.inline_element(c);
                    s.push_str(&t);
                }
                _ => {}
            }
        }
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Inline Markdown of one element (empty when it is not shown).
    fn inline_element(&mut self, c: NodeId) -> String {
        let tag = self.tag(c).to_string();
        if SKIP.contains(&tag.as_str()) || !self.shown(c) {
            return String::new();
        }
        match tag.as_str() {
            "a" => self.link(c),
            "strong" | "b" => wrap(&self.inline(c), "**"),
            "em" | "i" => wrap(&self.inline(c), "_"),
            "code" | "kbd" | "samp" => {
                let t = self.dom.text(c).replace('`', "'");
                if t.trim().is_empty() {
                    String::new()
                } else {
                    format!("`{}`", t.trim())
                }
            }
            "img" => self.image(c),
            "br" => " ".into(),
            _ => {
                let inner = self.inline(c);
                if BLOCKS.contains(&tag.as_str())
                    || matches!(tag.as_str(), "li" | "td" | "th" | "tr")
                {
                    format!(" {inner} ")
                } else {
                    inner
                }
            }
        }
    }

    fn absolute(&self, href: &str) -> Option<String> {
        let href = href.trim();
        if href.to_ascii_lowercase().starts_with("javascript:") {
            return None;
        }
        Some(match &self.base {
            Some(b) => b
                .join(href)
                .map(|u| u.to_string())
                .unwrap_or_else(|_| href.to_string()),
            None => href.to_string(),
        })
    }

    fn link(&mut self, id: NodeId) -> String {
        let text = self.inline(id);
        let Some(url) = self.attr(id, "href").and_then(|h| self.absolute(h)) else {
            return text;
        };
        if text.is_empty() {
            return String::new();
        }
        let url = url
            .replace(' ', "%20")
            .replace('(', "%28")
            .replace(')', "%29");
        match self.refs.get(&id) {
            Some(r) => format!("[{text}]({url} \"{r}\")"),
            None => format!("[{text}]({url})"),
        }
    }

    fn image(&self, id: NodeId) -> String {
        let alt = self
            .attr(id, "alt")
            .map(|a| a.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        if alt.is_empty() {
            return String::new();
        }
        match self.attr(id, "src").and_then(|s| self.absolute(s)) {
            Some(src) if !src.starts_with("data:") => {
                format!("![{}]({})", escape(&alt), src.replace(' ', "%20"))
            }
            _ => escape(&alt),
        }
    }

    fn list(&mut self, id: NodeId, ordered: bool, depth: usize) {
        let mut n = 0;
        for li in self.dom.children(id).collect::<Vec<_>>() {
            if self.tag(li) != "li" || !self.shown(li) {
                continue;
            }
            n += 1;
            let marker = if ordered { format!("{n}.") } else { "-".into() };
            let indent = "  ".repeat(depth);
            // Text first, then nested lists.
            let mut text = String::new();
            let mut nested = Vec::new();
            for c in self.dom.children(li).collect::<Vec<_>>() {
                if matches!(self.tag(c), "ul" | "ol") {
                    nested.push(c);
                } else if let Some(NodeData::Text(t)) = self.dom.get(c).map(|x| &x.data) {
                    text.push_str(&escape(t));
                } else if self
                    .dom
                    .get(c)
                    .is_some_and(|x| matches!(x.data, NodeData::Element { .. }))
                {
                    let t = self.inline_element(c);
                    text.push(' ');
                    text.push_str(&t);
                    text.push(' ');
                }
            }
            let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let _ = writeln!(self.out, "{indent}{marker} {text}");
            for l in nested {
                let o = self.tag(l) == "ol";
                self.list(l, o, depth + 1);
            }
        }
    }

    fn table(&mut self, id: NodeId) {
        let rows: Vec<NodeId> = self
            .dom
            .traverse(id)
            .filter_map(|e| match e {
                Edge::Enter(r) if self.tag(r) == "tr" => Some(r),
                _ => None,
            })
            .filter(|&r| self.shown(r))
            .collect();
        let mut cells: Vec<Vec<String>> = Vec::new();
        for r in rows {
            let shown: Vec<NodeId> = self
                .dom
                .children(r)
                .filter(|&c| matches!(self.tag(c), "td" | "th") && self.shown(c))
                .collect();
            let row: Vec<String> = shown.into_iter().map(|c| self.inline(c)).collect();
            if !row.is_empty() {
                cells.push(row);
            }
        }
        let width = cells.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            return;
        }
        for (i, row) in cells.iter().enumerate() {
            let mut padded = row.clone();
            padded.resize(width, String::new());
            let _ = writeln!(self.out, "| {} |", padded.join(" | "));
            if i == 0 {
                let _ = writeln!(self.out, "|{}", " --- |".repeat(width));
            }
        }
        self.out.push('\n');
    }
}

fn wrap(text: &str, mark: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!("{mark}{text}{mark}")
    }
}
