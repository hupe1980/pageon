//! The `ax` view: structured lines from the joined page, fitting to a budget and rendering
//!.

use std::collections::{BTreeMap, HashSet};

use crate::budget::{Budget, Candidate, fit};
use crate::capture::{AxNode, Capture, FrameState};
use crate::page::{NodeKey, Page};
use crate::quote::{clip, normalize, quote};
use crate::refs::{RefTable, Signature, ref_name};
use crate::visibility::{
    HitIndex, Visibility, covered_by, in_viewport, is_editable, label_of, visibility,
};

/// Maximum characters of a name or value.
const MAX_NAME: usize = 120;
/// Maximum characters of a text run.
const MAX_TEXT: usize = 1000;
/// Options shown for a combobox before `+N`.
const MAX_OPTIONS: usize = 3;

/// Roles rendered as actionable element lines without descending.
const LEAF: &[&str] = &[
    "button",
    "link",
    "textbox",
    "searchbox",
    "checkbox",
    "radio",
    "switch",
    "slider",
    "spinbutton",
    "combobox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "tab",
    "treeitem",
    "DisclosureTriangle",
    "ColorWell",
    "Date",
    "DateTime",
    "InputTime",
];
/// Roles rendered as element lines whose children nest below them.
const CONTAINER: &[&str] = &[
    "dialog",
    "alertdialog",
    "listbox",
    "menu",
    "menubar",
    "tablist",
    "radiogroup",
    "tree",
    "grid",
];
/// Landmark roles rendered as `## <role>`.
const LANDMARK: &[&str] = &[
    "banner",
    "navigation",
    "main",
    "complementary",
    "contentinfo",
    "search",
    "form",
    "region",
];
/// Live-region roles rendered as quoted text with their role.
const ALERTISH: &[&str] = &["alert", "status"];
/// Ancestors that form a relocation path.
const PATH_ROLES: &[&str] = &[
    "banner",
    "navigation",
    "main",
    "complementary",
    "contentinfo",
    "search",
    "form",
    "region",
    "dialog",
    "alertdialog",
    "listbox",
    "menu",
    "menubar",
    "tablist",
    "radiogroup",
    "tree",
    "grid",
    "list",
    "table",
    "group",
    "heading",
];
/// Generic roles that become `clickable` lines when the DOM node has a click handler.
const GENERIC: &[&str] = &[
    "generic",
    "paragraph",
    "listitem",
    "cell",
    "gridcell",
    "image",
    "none",
];

/// The kind of an observation line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// `## main`.
    Landmark,
    /// `# "Title"`.
    Heading,
    /// An actionable element with a ref.
    Element,
    /// A container element with a ref (dialog, listbox, …).
    Container,
    /// A visible text run.
    Text,
    /// Text present in the layout but invisible.
    HiddenText,
    /// An invisible editable field with no visible label (excluded from acting).
    Flagged,
    /// `== frame …`.
    FrameOpen,
    /// `== /frame`.
    FrameClose,
    /// The omission summary.
    Omission,
}

/// One observation line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Stable key: the ref for elements, a hidden key otherwise.
    pub key: String,
    /// Nesting depth (frames and dialogs).
    pub depth: u16,
    /// Kind.
    pub kind: LineKind,
    /// Role label used in the omission summary.
    pub role: String,
    /// Fit priority (lower kept first).
    pub priority: u8,
    /// Rendered text without indentation.
    pub text: String,
    pub(crate) block: Option<(usize, usize)>,
}

impl Line {
    /// The line with indentation.
    pub fn render(&self) -> String {
        format!("{}{}", "  ".repeat(self.depth as usize), self.text)
    }
}

/// Signatures of every element that can carry a ref, for [`RefTable::reconcile`].
pub fn collect(page: &Page) -> Vec<(NodeKey, Signature)> {
    let mut w = Walker::new(page, None);
    w.run();
    w.collected
}

/// Builds the unfitted lines of the page, handing out refs from `table`.
pub fn build(page: &Page, table: &mut RefTable) -> Vec<Line> {
    let mut names = HashSet::new();
    {
        let mut w = Walker::new(page, None);
        w.run();
        for (_, s) in &w.collected {
            if !s.name.is_empty() {
                names.insert(s.name.clone());
            }
        }
    }
    let mut w = Walker::new(page, Some(table));
    w.control_names = names;
    w.run();
    let mut lines = std::mem::take(&mut w.modal);
    lines.extend(w.lines);
    lines
}

/// The header line.
pub fn header(capture: &Capture, version: u64, budget: Budget, fallback: bool) -> String {
    let url = capture.url.replace(' ', "%20").replace('"', "%22");
    let v = &capture.viewport;
    // Where the viewport is on a page larger than it: top / height (x,y / w,h once sideways).
    let scroll = if v.content_width > v.width + 0.5 && v.x > 0.0 {
        format!(
            " scroll:{},{}/{},{}",
            v.x.round(),
            v.y.round(),
            v.content_width.round(),
            v.content_height.round()
        )
    } else if v.content_height > v.height + 0.5 {
        format!(" scroll:{}/{}", v.y.round(), v.content_height.round())
    } else {
        String::new()
    };
    let mut h = format!(
        "@page v{version} {} {url} load:{}{scroll} budget:{budget}",
        quote(&clip(&capture.title, MAX_NAME)),
        capture.load_state
    );
    if fallback {
        h.push_str(" full:fallback");
    }
    // A capture built without geometry: no visibility, cover or hidden marks are possible.
    if capture
        .extras
        .get("layout")
        .is_some_and(|v| v == "unavailable")
    {
        h.push_str(" layout:unavailable");
    }
    h
}

/// Fits lines into the budget (the header counts against it) and appends the omission line.
pub fn fit_lines(lines: &[Line], header: &str, budget: Budget) -> Vec<Line> {
    let header_cost = budget.count(header) + 1;
    let reserve = 40; // omission line
    let available = budget.max_tokens.saturating_sub(header_cost + reserve);
    let mut counts: BTreeMap<u8, usize> = BTreeMap::new();
    for l in lines {
        *counts.entry(l.priority).or_default() += 1;
    }
    let mut seen: BTreeMap<u8, usize> = BTreeMap::new();
    let candidates: Vec<Candidate> = lines
        .iter()
        .map(|l| {
            let pos = seen.entry(l.priority).or_default();
            let n = counts[&l.priority];
            let edge = (*pos).min(n - 1 - *pos);
            *pos += 1;
            Candidate {
                priority: l.priority,
                edge_distance: edge,
                tokens: budget.count(&l.render()) + 1,
            }
        })
        .collect();
    let mut keep = fit(&candidates, available);
    fix_frames(lines, &mut keep);
    loop {
        let out = assemble(lines, &keep);
        let text = render_lines(header, &out);
        if budget.count(&text) <= budget.max_tokens {
            return out;
        }
        // Drop the least important kept line and retry.
        let Some(victim) = (0..lines.len())
            .filter(|&i| keep[i] && lines[i].kind != LineKind::FrameClose)
            .max_by_key(|&i| (candidates[i].priority, candidates[i].edge_distance, i))
        else {
            return out;
        };
        keep[victim] = false;
        fix_frames(lines, &mut keep);
    }
}

/// Renders the header and lines.
pub fn render_lines(header: &str, lines: &[Line]) -> String {
    let mut out = String::from(header);
    for l in lines {
        out.push('\n');
        out.push_str(&l.render());
    }
    out
}

fn assemble(lines: &[Line], keep: &[bool]) -> Vec<Line> {
    let mut out: Vec<Line> = lines
        .iter()
        .zip(keep)
        .filter(|(_, k)| **k)
        .map(|(l, _)| l.clone())
        .collect();
    let mut omitted: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0;
    for (l, k) in lines.iter().zip(keep) {
        if !*k
            && !matches!(
                l.kind,
                LineKind::FrameClose | LineKind::FrameOpen | LineKind::Landmark
            )
        {
            *omitted.entry(l.role.clone()).or_default() += 1;
            total += 1;
        }
    }
    if total > 0 {
        let mut by_count: Vec<(String, usize)> = omitted.into_iter().collect();
        by_count.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let parts: Vec<String> = by_count
            .iter()
            .take(6)
            .map(|(r, c)| format!("{r}×{c}"))
            .collect();
        out.push(Line {
            key: "omission".into(),
            depth: 0,
            kind: LineKind::Omission,
            role: "omission".into(),
            priority: 0,
            text: format!(
                "… {total} more: {} (scroll or act on visible elements)",
                parts.join(" ")
            ),
            block: None,
        });
    }
    out
}

fn fix_frames(lines: &[Line], keep: &mut [bool]) {
    for i in 0..lines.len() {
        if lines[i].kind == LineKind::FrameClose {
            let open = format!("fo:{}", &lines[i].key[3..]);
            keep[i] = lines
                .iter()
                .zip(keep.iter())
                .any(|(l, k)| *k && l.key == open);
        }
    }
}

struct Ctx {
    depth: u16,
    suppress_text: bool,
    in_label: bool,
    in_modal: bool,
    block: Option<(usize, usize)>,
}

struct Walker<'a, 'b> {
    page: &'a Page,
    table: Option<&'b mut RefTable>,
    hits: Vec<HitIndex>,
    lines: Vec<Line>,
    modal: Vec<Line>,
    collected: Vec<(NodeKey, Signature)>,
    control_names: HashSet<String>,
    path: Vec<(String, String)>,
}

fn role_of(n: &AxNode) -> String {
    n.role
        .as_ref()
        .and_then(|r| r.as_text())
        .unwrap_or_default()
}

fn name_of(n: &AxNode) -> String {
    n.name
        .as_ref()
        .and_then(|v| v.as_text())
        .map(|s| normalize(&s))
        .unwrap_or_default()
}

fn prop<'n>(n: &'n AxNode, name: &str) -> Option<&'n serde_json::Value> {
    n.properties
        .iter()
        .find(|p| p.name == name)
        .and_then(|p| p.value.value.as_ref())
}

fn prop_true(n: &AxNode, name: &str) -> bool {
    match prop(n, name) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s == "true",
        _ => false,
    }
}

impl<'a, 'b> Walker<'a, 'b> {
    fn new(page: &'a Page, table: Option<&'b mut RefTable>) -> Self {
        let hits = page.frames.iter().map(HitIndex::build).collect();
        Self {
            page,
            table,
            hits,
            lines: Vec::new(),
            modal: Vec::new(),
            collected: Vec::new(),
            control_names: HashSet::new(),
            path: Vec::new(),
        }
    }

    fn run(&mut self) {
        if let Some(root) = self.page.frames.first().and_then(|f| f.ax_root) {
            let ctx = Ctx {
                depth: 0,
                suppress_text: false,
                in_label: false,
                in_modal: false,
                block: None,
            };
            self.walk(0, root, &ctx, 0);
        }
    }

    fn reference(&mut self, key: &NodeKey, sig: &Signature) -> String {
        match self.table.as_deref_mut() {
            Some(t) => ref_name(t.ref_for(key, Some(sig))),
            None => {
                self.collected.push((key.clone(), sig.clone()));
                String::new()
            }
        }
    }

    fn push(&mut self, ctx: &Ctx, mut line: Line, in_view: bool) {
        line.priority = match line.kind {
            _ if ctx.in_modal => 1,
            LineKind::HiddenText => 6,
            LineKind::Element
            | LineKind::Container
            | LineKind::FrameOpen
            | LineKind::FrameClose
                if in_view =>
            {
                2
            }
            LineKind::Flagged => 3,
            _ if in_view => 3,
            LineKind::Element | LineKind::Container => 4,
            _ => 5,
        };
        let target = if ctx.in_modal {
            &mut self.modal
        } else {
            &mut self.lines
        };
        // Merge consecutive text runs of the same block.
        if line.kind == LineKind::Text
            && let Some(last) = target.last_mut()
            && last.kind == LineKind::Text
            && last.block.is_some()
            && last.block == line.block
            && last.depth == line.depth
        {
            let prev = last.text.trim_start_matches("text ").to_string();
            let a: String = serde_json::from_str(&prev).unwrap_or_default();
            let b: String =
                serde_json::from_str(line.text.trim_start_matches("text ")).unwrap_or_default();
            last.text = format!("text {}", quote(&clip(&format!("{a} {b}"), MAX_TEXT)));
            last.priority = last.priority.min(line.priority);
            return;
        }
        target.push(line);
    }

    fn line(
        &self,
        key: String,
        ctx: &Ctx,
        kind: LineKind,
        role: &str,
        text: String,
        block: Option<(usize, usize)>,
    ) -> Line {
        Line {
            key,
            depth: ctx.depth,
            kind,
            role: role.to_string(),
            priority: 5,
            text,
            block,
        }
    }

    fn walk(&mut self, f: usize, ax: usize, ctx: &Ctx, guard: u32) {
        if guard > 400 {
            return;
        }
        let page = self.page;
        let frame = &page.frames[f];
        let node = &frame.ax[ax];
        let role = role_of(node);
        let name = name_of(node);
        let dom = node
            .backend_dom_node_id
            .and_then(|b| frame.by_backend.get(&b).copied());
        let key = node.backend_dom_node_id.map(|b| page.key(f, b));
        let in_view = dom.is_some_and(|d| in_viewport(page, frame, d));
        let children: Vec<usize> = node
            .child_ids
            .iter()
            .filter_map(|c| frame.ax_by_id.get(c).copied())
            .collect();
        let walk_children = |w: &mut Self, ctx: &Ctx| {
            for &c in &children {
                w.walk(f, c, ctx, guard + 1);
            }
        };

        if node.ignored {
            let aria_hidden = node
                .ignored_reasons
                .iter()
                .any(|r| r.name.starts_with("ariaHidden"));
            if role == "StaticText"
                && aria_hidden
                && !name.is_empty()
                && !ctx.suppress_text
                && let (Some(k), Some(d)) = (key.as_ref(), dom)
                && frame.dom[d].layout.is_some()
            {
                let l = self.line(
                    format!("t:{}", key_str(k)),
                    ctx,
                    LineKind::HiddenText,
                    "hidden-text",
                    format!("hidden-text {}", quote(&clip(&name, MAX_TEXT))),
                    None,
                );
                self.push(ctx, l, in_view);
            }
            walk_children(self, ctx);
            return;
        }

        let block = if dom.is_some_and(|d| {
            frame.dom[d]
                .layout
                .as_ref()
                .is_some_and(|l| !l.display.starts_with("inline"))
        }) || ctx.block.is_none()
        {
            Some((f, ax))
        } else {
            ctx.block
        };
        let sub = |depth: u16, suppress: bool, label: bool, modal: bool| Ctx {
            depth,
            suppress_text: suppress,
            in_label: label,
            in_modal: modal,
            block,
        };

        match role.as_str() {
            "RootWebArea" => walk_children(
                self,
                &sub(ctx.depth, ctx.suppress_text, false, ctx.in_modal),
            ),
            "Iframe" => self.frame_marker(f, node, key.as_ref(), ctx, in_view, guard),
            r if LANDMARK.contains(&r) => {
                if (r == "form" || r == "region") && name.is_empty() {
                    self.drop_target(f, node, &name, &children, key.as_ref(), dom, ctx, in_view);
                    walk_children(
                        self,
                        &sub(ctx.depth, ctx.suppress_text, false, ctx.in_modal),
                    );
                    return;
                }
                if let Some(k) = key.as_ref() {
                    let text = if name.is_empty() {
                        format!("## {r}")
                    } else {
                        format!("## {r} {}", quote(&clip(&name, MAX_NAME)))
                    };
                    let l = self.line(
                        format!("l:{}", key_str(k)),
                        ctx,
                        LineKind::Landmark,
                        r,
                        text,
                        None,
                    );
                    self.push(ctx, l, in_view);
                }
                self.path.push((r.to_string(), name.clone()));
                self.drop_target(f, node, &name, &children, key.as_ref(), dom, ctx, in_view);
                walk_children(
                    self,
                    &sub(ctx.depth, ctx.suppress_text, false, ctx.in_modal),
                );
                self.path.pop();
            }
            "heading" => {
                if let Some(k) = key.as_ref() {
                    let level = prop(node, "level").and_then(|v| v.as_i64()).unwrap_or(2);
                    let mark = if level <= 2 { "#" } else { "###" };
                    let l = self.line(
                        format!("h:{}", key_str(k)),
                        ctx,
                        LineKind::Heading,
                        "heading",
                        format!("{mark} {}", quote(&clip(&name, MAX_NAME))),
                        None,
                    );
                    self.push(ctx, l, in_view);
                }
                self.path.push(("heading".into(), name.clone()));
                walk_children(self, &sub(ctx.depth, true, false, ctx.in_modal));
                self.path.pop();
            }
            r if ALERTISH.contains(&r) => {
                let name = if name.is_empty() {
                    descendant_text(frame, &children, 0)
                } else {
                    name.clone()
                };
                if let Some(k) = key.as_ref()
                    && !name.is_empty()
                {
                    let l = self.line(
                        format!("t:{}", key_str(k)),
                        ctx,
                        LineKind::Text,
                        r,
                        format!("{r} {}", quote(&clip(&name, MAX_TEXT))),
                        None,
                    );
                    self.push(ctx, l, in_view);
                    walk_children(self, &sub(ctx.depth, true, false, ctx.in_modal));
                    return;
                }
                walk_children(
                    self,
                    &sub(ctx.depth, ctx.suppress_text, false, ctx.in_modal),
                );
            }
            r if LEAF.contains(&r) => {
                self.element(
                    f,
                    node,
                    &role,
                    &name,
                    key.as_ref(),
                    dom,
                    ctx,
                    in_view,
                    false,
                );
            }
            r if CONTAINER.contains(&r) => {
                let modal = r.contains("dialog") && prop_true(node, "modal");
                let ctx2 = sub(ctx.depth, ctx.suppress_text, false, ctx.in_modal || modal);
                let shown = self.element(
                    f,
                    node,
                    &role,
                    &name,
                    key.as_ref(),
                    dom,
                    &ctx2,
                    in_view,
                    modal,
                );
                self.path.push((r.to_string(), name.clone()));
                if !shown {
                    self.drop_target(f, node, &name, &children, key.as_ref(), dom, ctx, in_view);
                }
                let depth = if shown { ctx.depth + 1 } else { ctx.depth };
                walk_children(
                    self,
                    &sub(depth, ctx.suppress_text, false, ctx.in_modal || modal),
                );
                self.path.pop();
            }
            "StaticText" => {
                if ctx.suppress_text || name.is_empty() {
                    return;
                }
                if ctx.in_label && self.control_names.contains(&name) {
                    return;
                }
                let Some(k) = key.as_ref() else { return };
                let vis = dom
                    .map(|d| visibility(frame, d))
                    .unwrap_or(Visibility::Visible);
                let (kind, word) = match vis {
                    Visibility::Visible => (LineKind::Text, "text"),
                    Visibility::Hidden => (LineKind::HiddenText, "hidden-text"),
                    Visibility::NotRendered => return,
                };
                let l = self.line(
                    format!("t:{}", key_str(k)),
                    ctx,
                    kind,
                    word,
                    format!("{word} {}", quote(&clip(&name, MAX_TEXT))),
                    ctx.block,
                );
                self.push(ctx, l, in_view);
            }
            r => {
                let clickable = GENERIC.contains(&r)
                    && dom.is_some_and(|d| {
                        let n = &frame.dom[d];
                        n.clickable
                            && n.node_type == 1
                            && !matches!(n.name.as_str(), "body" | "html" | "label")
                    });
                // Only small widgets: a click-listening wrapper around content is not one control.
                if clickable && !ctx.suppress_text && !has_structure(frame, &children, 0) {
                    let text = if name.is_empty() {
                        descendant_text(frame, &children, 0)
                    } else {
                        name.clone()
                    };
                    // Unnamed clickables (colour swatches, icons, shapes) are listed too: an
                    // agent cannot act on what it is not shown.
                    if text.chars().count() <= MAX_NAME {
                        self.element(
                            f,
                            node,
                            "clickable",
                            &normalize(&text),
                            key.as_ref(),
                            dom,
                            ctx,
                            in_view,
                            false,
                        );
                        return;
                    }
                }
                let label = r == "LabelText" || ctx.in_label;
                let on_path = PATH_ROLES.contains(&r);
                if on_path {
                    self.path.push((r.to_string(), name.clone()));
                }
                self.drop_target(f, node, &name, &children, key.as_ref(), dom, ctx, in_view);
                walk_children(
                    self,
                    &sub(ctx.depth, ctx.suppress_text, label, ctx.in_modal),
                );
                if on_path {
                    self.path.pop();
                }
            }
        }
    }

    /// A `droptarget` line for an element that listens for `drop` (a board's column, a file
    /// drop zone): a ref to drag or drop onto. Its content follows as usual.
    #[allow(clippy::too_many_arguments)]
    fn drop_target(
        &mut self,
        f: usize,
        node: &AxNode,
        name: &str,
        children: &[usize],
        key: Option<&NodeKey>,
        dom: Option<usize>,
        ctx: &Ctx,
        in_view: bool,
    ) {
        let frame = &self.page.frames[f];
        let Some(d) = dom else { return };
        if !frame.dom[d].drop_target || ctx.suppress_text {
            return;
        }
        let text = if name.is_empty() {
            descendant_text(frame, children, 0)
        } else {
            name.to_string()
        };
        self.element(
            f,
            node,
            "droptarget",
            &normalize(&clip(&text, MAX_NAME)),
            key,
            dom,
            ctx,
            in_view,
            false,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn element(
        &mut self,
        f: usize,
        node: &AxNode,
        role: &str,
        name: &str,
        key: Option<&NodeKey>,
        dom: Option<usize>,
        ctx: &Ctx,
        in_view: bool,
        modal: bool,
    ) -> bool {
        let page = self.page;
        let frame = &page.frames[f];
        let Some(key) = key else { return false };
        let label = role_label(role);
        let mut covered: Option<Option<String>> = None;
        if let Some(d) = dom {
            match visibility(frame, d) {
                Visibility::Visible => {}
                Visibility::Hidden => {
                    let labelled = label_of(frame, d)
                        .is_some_and(|l| visibility(frame, l) == Visibility::Visible);
                    if !labelled {
                        if is_editable(frame, d) && self.table.is_some() {
                            let n = &frame.dom[d];
                            let ident = n
                                .attr("name")
                                .map(|v| format!(" name:{}", quote(v)))
                                .or_else(|| n.attr("id").map(|v| format!(" id:{}", quote(v))))
                                .unwrap_or_default();
                            let l = self.line(
                                format!("x:{}", key_str(key)),
                                ctx,
                                LineKind::Flagged,
                                "flagged",
                                format!("flagged hidden-field {label}{ident}"),
                                None,
                            );
                            self.push(ctx, l, in_view);
                        }
                        return false;
                    }
                }
                Visibility::NotRendered => return false,
            }
            if let Some(top) = covered_by(page, frame, &self.hits[f], d) {
                covered = Some(self.covering_ref(f, top));
            }
        }
        let sig = Signature {
            role: role.to_string(),
            name: name.to_string(),
            path: self.path.clone(),
        };
        let r = self.reference(key, &sig);
        if self.table.is_none() {
            return true;
        }
        let mut text = format!("{label} {r}");
        if !name.is_empty() {
            let prefix = dom.map(|d| name_prefix(&frame.dom[d], name)).unwrap_or("");
            text.push(' ');
            text.push_str(prefix);
            text.push_str(&quote(&clip(name, MAX_NAME)));
        }
        if let Some(v) = node.value.as_ref().and_then(|v| v.as_text()) {
            let v = normalize(&v);
            if !v.is_empty()
                && v != name
                && !matches!(role, "checkbox" | "radio" | "switch" | "link")
            {
                text.push_str(" =");
                text.push_str(&quote(&clip(&v, MAX_NAME)));
            }
        }
        if role == "combobox" {
            let opts = options(frame, node);
            if !opts.is_empty() {
                let shown: Vec<String> = opts
                    .iter()
                    .take(MAX_OPTIONS)
                    .map(|o| quote(&clip(o, 40)))
                    .collect();
                let mut s = format!(" {{{}", shown.join("|"));
                if opts.len() > MAX_OPTIONS {
                    s.push_str(&format!("|+{}", opts.len() - MAX_OPTIONS));
                }
                s.push('}');
                text.push_str(&s);
            }
        }
        for state in states(node)
            .into_iter()
            .filter(|s| !(role == "combobox" && *s == "collapsed"))
        {
            text.push(' ');
            text.push_str(state);
        }
        if modal {
            text.push_str(" modal");
        }
        match covered {
            Some(Some(by)) if by != r => text.push_str(&format!(" covered-by:{by}")),
            Some(None) => text.push_str(" covered"),
            _ => {}
        }
        // A scroll container: where it is scrolled to, of how much.
        if let Some([x, y, w, h]) = dom.and_then(|d| frame.dom[d].layout.as_ref()?.scroll)
            && let Some(l) = dom.and_then(|d| frame.dom[d].layout.as_ref())
        {
            if w > l.bounds.w + 0.5 && x > 0.0 {
                text.push_str(&format!(
                    " scroll:{},{}/{},{}",
                    x.round(),
                    y.round(),
                    w.round(),
                    h.round()
                ));
            } else if h > l.bounds.h + 0.5 {
                text.push_str(&format!(" scroll:{}/{}", y.round(), h.round()));
            }
        }
        let kind = if CONTAINER.contains(&role) {
            LineKind::Container
        } else {
            LineKind::Element
        };
        let l = self.line(r, ctx, kind, label, text, None);
        self.push(ctx, l, in_view);
        true
    }

    fn covering_ref(&mut self, f: usize, top: usize) -> Option<String> {
        let page = self.page;
        let frame = &page.frames[f];
        let mut i = Some(top);
        let mut guard = 0;
        while let Some(d) = i {
            guard += 1;
            if guard > 128 {
                break;
            }
            let b = frame.dom[d].backend_id;
            if let Some(&ax) = frame.ax_by_backend.get(&b) {
                let n = &frame.ax[ax];
                let role = role_of(n);
                if !n.ignored
                    && (LEAF.contains(&role.as_str()) || CONTAINER.contains(&role.as_str()))
                {
                    let sig = Signature {
                        role: role.clone(),
                        name: name_of(n),
                        path: Vec::new(),
                    };
                    let key = page.key(f, b);
                    return self.table.as_deref_mut().map(|t| {
                        ref_name(t.get(&key).unwrap_or_else(|| t.ref_for(&key, Some(&sig))))
                    });
                }
            }
            i = frame.dom[d].parent;
        }
        None
    }

    fn frame_marker(
        &mut self,
        f: usize,
        node: &AxNode,
        key: Option<&NodeKey>,
        ctx: &Ctx,
        in_view: bool,
        guard: u32,
    ) {
        let page = self.page;
        let Some(key) = key else { return };
        let child = page.child_frame(f, key.backend_id);
        let (origin, third, state) = match child {
            Some(c) => (
                page.frames[c].origin.clone(),
                page.frames[c].third_party,
                page.frames[c].state,
            ),
            None => (String::from("about:blank"), false, FrameState::Loading),
        };
        let sig = Signature {
            role: "Iframe".into(),
            name: name_of(node),
            path: self.path.clone(),
        };
        let r = self.reference(key, &sig);
        if self.table.is_some() {
            let mut text = format!("== frame {r} {origin}");
            if third {
                text.push_str(" third-party");
            }
            if state != FrameState::Loaded {
                text.push_str(&format!(" state:{state}"));
            }
            let fid = child
                .map(|c| page.frames[c].frame_id.clone())
                .unwrap_or_else(|| key_str(key));
            let l = self.line(
                format!("fo:{fid}"),
                ctx,
                LineKind::FrameOpen,
                "frame",
                text,
                None,
            );
            self.push(ctx, l, in_view);
            if let Some(c) = child
                && let Some(root) = page.frames[c].ax_root
            {
                let inner = Ctx {
                    depth: ctx.depth + 1,
                    suppress_text: false,
                    in_label: false,
                    in_modal: ctx.in_modal,
                    block: None,
                };
                self.walk(c, root, &inner, guard + 1);
            }
            let l = self.line(
                format!("fc:{fid}"),
                ctx,
                LineKind::FrameClose,
                "frame",
                "== /frame".into(),
                None,
            );
            self.push(ctx, l, in_view);
        } else if let Some(c) = child
            && let Some(root) = page.frames[c].ax_root
        {
            let inner = Ctx {
                depth: ctx.depth + 1,
                suppress_text: false,
                in_label: false,
                in_modal: ctx.in_modal,
                block: None,
            };
            self.walk(c, root, &inner, guard + 1);
        }
    }
}

fn key_str(k: &NodeKey) -> String {
    format!("{}:{}:{}", k.frame_id, k.nav, k.backend_id)
}

fn role_label(role: &str) -> &str {
    match role {
        "DisclosureTriangle" => "disclosure",
        "ColorWell" => "color",
        "Date" | "DateTime" | "InputTime" => "date",
        r => r,
    }
}

fn name_prefix(n: &crate::page::DomNode, name: &str) -> &'static str {
    let labelled = n.attr("aria-label").is_some() || n.attr("aria-labelledby").is_some();
    if labelled {
        return "";
    }
    if n.attr("placeholder").is_some_and(|p| normalize(p) == name) {
        "ph:"
    } else if n.attr("title").is_some_and(|t| normalize(t) == name) {
        "title:"
    } else if n.attr("alt").is_some_and(|t| normalize(t) == name) {
        "alt:"
    } else {
        ""
    }
}

fn states(n: &AxNode) -> Vec<&'static str> {
    let mut out = Vec::new();
    match prop(n, "checked") {
        Some(serde_json::Value::String(s)) if s == "true" => out.push("checked"),
        Some(serde_json::Value::String(s)) if s == "mixed" => out.push("mixed"),
        Some(serde_json::Value::Bool(true)) => out.push("checked"),
        _ => {}
    }
    if prop_true(n, "disabled") {
        out.push("disabled");
    }
    match prop(n, "expanded") {
        Some(serde_json::Value::Bool(true)) => out.push("expanded"),
        Some(serde_json::Value::Bool(false)) => out.push("collapsed"),
        _ => {}
    }
    if prop_true(n, "selected") {
        out.push("selected");
    }
    if prop_true(n, "required") {
        out.push("required");
    }
    match prop(n, "invalid") {
        Some(serde_json::Value::String(s)) if s != "false" => out.push("invalid"),
        _ => {}
    }
    if prop_true(n, "focused") {
        out.push("focused");
    }
    out
}

fn options(frame: &crate::page::FramePage, node: &AxNode) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<&str> = node.child_ids.iter().rev().map(String::as_str).collect();
    let mut guard = 0;
    while let Some(id) = stack.pop() {
        guard += 1;
        if guard > 2000 {
            break;
        }
        let Some(&i) = frame.ax_by_id.get(id) else {
            continue;
        };
        let n = &frame.ax[i];
        if role_of(n) == "option" {
            let name = name_of(n);
            if !name.is_empty() {
                out.push(name);
            }
            continue;
        }
        stack.extend(n.child_ids.iter().rev().map(String::as_str));
    }
    out
}

/// Whether any descendant is interactive, a heading, a landmark or a container.
fn has_structure(frame: &crate::page::FramePage, children: &[usize], depth: u32) -> bool {
    if depth > 16 {
        return true;
    }
    children.iter().any(|&c| {
        let n = &frame.ax[c];
        let r = role_of(n);
        (!n.ignored
            && (LEAF.contains(&r.as_str())
                || CONTAINER.contains(&r.as_str())
                || LANDMARK.contains(&r.as_str())
                || r == "heading"
                || r == "Iframe"))
            || {
                let kids: Vec<usize> = n
                    .child_ids
                    .iter()
                    .filter_map(|k| frame.ax_by_id.get(k).copied())
                    .collect();
                has_structure(frame, &kids, depth + 1)
            }
    })
}

fn descendant_text(frame: &crate::page::FramePage, children: &[usize], depth: u32) -> String {
    if depth > 8 {
        return String::new();
    }
    let mut parts = Vec::new();
    for &c in children {
        let n = &frame.ax[c];
        if role_of(n) == "StaticText" {
            parts.push(name_of(n));
        } else {
            let kids: Vec<usize> = n
                .child_ids
                .iter()
                .filter_map(|k| frame.ax_by_id.get(k).copied())
                .collect();
            parts.push(descendant_text(frame, &kids, depth + 1));
        }
        if parts.iter().map(String::len).sum::<usize>() > MAX_NAME {
            break;
        }
    }
    normalize(&parts.join(" "))
}

#[cfg(test)]
impl Line {
    /// A blank line for tests.
    pub(crate) fn test_default() -> Self {
        Self {
            key: String::new(),
            depth: 0,
            kind: LineKind::Text,
            role: String::new(),
            priority: 5,
            text: String::new(),
            block: None,
        }
    }
}
