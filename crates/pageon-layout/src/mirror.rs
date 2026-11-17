//! The mirror: pageon's document model copied into a Blitz document (Stylo + Taffy + Parley),
//! which is a derived layout tree. pageon-dom stays the source of truth.

use std::collections::HashMap;

use blitz_dom::{
    BaseDocument, DocumentConfig, DocumentMutator, NodeId as BId, StyleThreading,
    UserAgentStylesheet,
};
use pageon_dom::{Dom, Edge, NodeData, NodeId};
use style::stylesheets::Origin;

use crate::images::{Decoded, Recorder};
use crate::{Stylesheet, Viewport, fonts};

const UA_CSS: &str = include_str!("ua.css");
/// The mirror-only attribute marking an element in the top layer.
pub(crate) const TOP_LAYER: &str = "data-pageon-top-layer";
/// The mirror-only attribute marking the `<option>` a drop-down shows (or the selected ones).
const SHOWN: &str = "data-pageon-shown";

/// pageon `NodeId` ↔ Blitz node id.
#[derive(Debug, Default)]
pub(crate) struct Map {
    to_blitz: Vec<Option<(u32, BId)>>,
    to_pageon: HashMap<BId, NodeId>,
    /// Mirrored nodes in tree order.
    order: Vec<NodeId>,
    /// Nodes given a scroll offset: a kept tree resets them before the next layout's.
    scrolled: Vec<BId>,
}

impl Map {
    fn insert(&mut self, id: NodeId, b: BId) {
        let i = id.index() as usize;
        if self.to_blitz.len() <= i {
            self.to_blitz.resize(i + 1, None);
        }
        self.to_blitz[i] = Some((id.generation(), b));
        self.to_pageon.insert(b, id);
        self.order.push(id);
    }

    /// Every mirrored pageon node, in tree order.
    pub(crate) fn pageon_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.order.iter().copied()
    }

    /// The Blitz node of a pageon node (generation-checked).
    pub(crate) fn blitz(&self, id: NodeId) -> Option<BId> {
        match self.to_blitz.get(id.index() as usize)? {
            Some((g, b)) if *g == id.generation() => Some(*b),
            _ => None,
        }
    }

    /// Every mirrored Blitz node.
    pub(crate) fn blitz_ids(&self) -> impl Iterator<Item = BId> + '_ {
        self.to_pageon.keys().copied()
    }

    /// The pageon node of a Blitz node, if it was mirrored.
    pub(crate) fn pageon(&self, b: BId) -> Option<NodeId> {
        self.to_pageon.get(&b).copied()
    }
}

fn config(viewport: Viewport, base: &str, net: Option<std::sync::Arc<Recorder>>) -> DocumentConfig {
    DocumentConfig {
        net_provider: net.map(|n| n as std::sync::Arc<dyn blitz_traits::net::NetProvider>),
        base_url: Some(base.to_string()),
        viewport: Some(blitz_traits::shell::Viewport::new(
            viewport.width,
            viewport.height,
            1.0,
            blitz_traits::shell::ColorScheme::Light,
        )),
        ua_stylesheets: vec![
            UserAgentStylesheet::default_sheet(),
            UserAgentStylesheet::parse(UA_CSS),
        ],
        font_ctx: Some(fonts::context()),
        style_threading: StyleThreading::Sequential,
        ..DocumentConfig::default()
    }
}

/// An empty document (used when layout failed).
pub(crate) fn empty(viewport: Viewport) -> BaseDocument {
    BaseDocument::new(config(viewport, "about:blank", None))
}

/// What a build with images learned: the image URLs the document asked for (without
/// stylesheet and frame URLs), and how many cached images it applied.
#[derive(Debug, Default)]
pub(crate) struct ImageUse {
    pub(crate) wanted: Vec<String>,
    pub(crate) applied: usize,
}

/// A sheet as the mirror attaches it, with its hash: rules that cannot match left out
/// (`tokens`), pseudo-classes rewritten to markers (`pseudo`), and selectors scoped to their
/// tree (pages with shadow trees). Remembered by the text's hash (given, when known) and the
/// rewrites, so a sheet is neither rewritten, copied nor hashed again at the next layout.
fn sheet_css(
    css: &str,
    hash: Option<u64>,
    pseudo: bool,
    scope: Option<crate::scope::Scope>,
    tokens: Option<&crate::scope::Tokens>,
) -> Sheet {
    type Memo = (HashMap<(u64, usize, u64), Sheet>, usize);
    thread_local! {
        static SHEETS: std::cell::RefCell<Memo> = std::cell::RefCell::default();
    }
    let hash = hash.unwrap_or_else(|| crate::scope::fast_hash(css));
    let kind = {
        let scope = match scope {
            None => 0,
            Some(crate::scope::Scope::Document) => 1,
            Some(crate::scope::Scope::Shadow(n)) => 2 + u64::from(n),
        };
        let t = tokens.map_or(0, crate::scope::Tokens::fingerprint_of);
        (t ^ scope.wrapping_mul(0x9e37_79b9_7f4a_7c15)).rotate_left(1) ^ u64::from(pseudo)
    };
    let key = (hash, css.len(), kind);
    if let Some(s) = SHEETS.with(|m| m.borrow().0.get(&key).cloned()) {
        return s;
    }
    let pruned = tokens.and_then(|t| crate::scope::prune_memo(css, t));
    let text: &str = pruned.as_deref().unwrap_or(css);
    let marked = pseudo.then(|| crate::scope::pseudo_memo(text)).flatten();
    let text: &str = marked.as_deref().unwrap_or(text);
    let scoped = scope.map(|s| crate::scope::rewrite_memo(text, s));
    let text: std::rc::Rc<str> = match (scoped, marked, pruned) {
        (Some(s), ..) => s,
        (None, Some(m), _) => m,
        (None, None, Some(p)) => p,
        (None, None, None) => css.into(),
    };
    let sheet = Sheet {
        hash: crate::scope::fast_hash(&text),
        text,
    };
    SHEETS.with(|m| {
        let mut m = m.borrow_mut();
        // Past 64 MiB of sheets the memo starts over.
        if m.1 + sheet.text.len() > 64 << 20 {
            m.0.clear();
            m.1 = 0;
        }
        m.1 += sheet.text.len();
        m.0.insert(key, sheet.clone());
    });
    sheet
}

/// A sheet's text as attached, and its hash.
#[derive(Clone)]
struct Sheet {
    text: std::rc::Rc<str>,
    hash: u64,
}

/// Whether any stylesheet of the document uses a pseudo-class the mirror marks.
fn uses_pseudo(dom: &Dom, stylesheets: &[Stylesheet]) -> bool {
    stylesheets
        .iter()
        .any(|s| crate::scope::uses_pseudo_hashed(s.css(), s.hash))
        || dom.traverse(dom.root()).any(|e| match e {
            Edge::Enter(id) => match dom.get(id).map(|n| &n.data) {
                Some(NodeData::Text(t)) => {
                    dom.parent(id)
                        .and_then(|p| dom.get(p))
                        .is_some_and(|p| p.is_html("style"))
                        && crate::scope::uses_pseudo_memo(t)
                }
                _ => false,
            },
            Edge::Leave(_) => false,
        })
        || adopted(dom, None)
            .iter()
            .any(|(_, css)| crate::scope::uses_pseudo_memo(css))
        || dom.cssom_texts().any(crate::scope::uses_pseudo_memo)
}

/// The adopted stylesheets to apply, in cascade order: the document's, then each connected
/// shadow tree's, with their scope.
fn adopted(
    dom: &Dom,
    composed: Option<&pageon_dom::Composed>,
) -> Vec<(Option<crate::scope::Scope>, String)> {
    if !dom.has_adopted() {
        return Vec::new();
    }
    let mut out: Vec<(Option<crate::scope::Scope>, String)> = dom
        .adopted(dom.root())
        .iter()
        .map(|c| (composed.map(|_| crate::scope::Scope::Document), c.clone()))
        .collect();
    if let Some(c) = composed {
        // Connected shadow trees only (a removed host's tree has no ordinal).
        let mut roots: Vec<(u32, NodeId)> = dom
            .adopted_roots()
            .filter_map(|r| c.roots.get(&r).map(|&n| (n, r)))
            .collect();
        roots.sort_unstable();
        for (n, root) in roots {
            for css in dom.adopted(root) {
                out.push((Some(crate::scope::Scope::Shadow(n)), css.clone()));
            }
        }
    }
    out
}

/// Mirror-only markers for the pseudo-classes rewritten by [`crate::scope::pseudo`].
fn markers(dom: &Dom, id: NodeId, attrs: &mut Vec<blitz_dom::Attribute>) {
    use pageon_dom::forms;
    let Some(n) = dom.get(id) else { return };
    let invalid = if n.is_html("form") || n.is_html("fieldset") {
        Some(forms::has_invalid(dom, id))
    } else if forms::barred(dom, id) {
        None
    } else {
        Some(!forms::validity(dom, id).valid())
    };
    if let Some(invalid) = invalid {
        attrs.push(attr(
            if invalid {
                "pageon-invalid"
            } else {
                "pageon-valid"
            },
            String::new(),
        ));
        if dom.control(id).is_some_and(|c| c.user_validity) && !n.is_html("form") {
            attrs.push(attr(
                if invalid {
                    "pageon-user-invalid"
                } else {
                    "pageon-user-valid"
                },
                String::new(),
            ));
        }
    }
    if let Some(i) = dom.internals(id).filter(|i| !i.states.is_empty()) {
        attrs.push(attr("pageon-state", i.states.join(" ")));
    }
}

fn attr(name: &str, value: String) -> blitz_dom::Attribute {
    blitz_dom::Attribute {
        name: html5ever::QualName::new(None, html5ever::ns!(), html5ever::LocalName::from(name)),
        value,
    }
}

fn set_attr(attrs: &mut Vec<blitz_dom::Attribute>, name: &str, value: Option<String>) {
    attrs.retain(|a| &*a.name.local != name);
    if let Some(v) = value {
        attrs.push(attr(name, v));
    }
}

/// `text` with every secret replaced by as many bullets as it has characters.
fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), &"•".repeat(s.chars().count()));
        }
    }
    out
}

/// The URL of a `srcset`'s `1x` candidate, else its first.
fn srcset_1x(srcset: &str) -> Option<String> {
    let candidates: Vec<(&str, Option<&str>)> = srcset
        .split(',')
        .filter_map(|c| {
            let mut parts = c.split_whitespace();
            Some((parts.next()?, parts.next()))
        })
        .collect();
    candidates
        .iter()
        .find(|(_, d)| matches!(d, None | Some("1x")))
        .or(candidates.first())
        .map(|(u, _)| (*u).to_string())
}

/// The `<option>`s a `<select>` shows as chosen: its selected ones; for a drop-down with none,
/// the first enabled option (as browsers show it).
fn shown_options(dom: &Dom, select: NodeId) -> Vec<NodeId> {
    let el = dom.get(select);
    let options: Vec<NodeId> = dom
        .traverse(select)
        .filter_map(|e| match e {
            Edge::Enter(id) if dom.get(id).is_some_and(|n| n.is_html("option")) => Some(id),
            _ => None,
        })
        .collect();
    let chosen: Vec<NodeId> = options
        .iter()
        .copied()
        .filter(|&o| {
            dom.control(o)
                .and_then(|c| c.selected)
                .unwrap_or_else(|| dom.get(o).is_some_and(|n| n.attr("selected").is_some()))
        })
        .collect();
    if el.is_some_and(|n| n.attr("multiple").is_some()) {
        return chosen;
    }
    if let Some(&last) = chosen.last() {
        return vec![last];
    }
    options
        .into_iter()
        .find(|&o| dom.get(o).is_some_and(|n| n.attr("disabled").is_none()))
        .into_iter()
        .collect()
}

/// The deepest nesting laid out as written (deeper nodes are flattened onto that level). Layout
/// recurses per level; debug builds use far larger frames, so they flatten earlier to survive
/// a 2 MiB thread stack (Rust's default for spawned threads).
const MAX_DEPTH: usize = if cfg!(debug_assertions) { 96 } else { 200 };

pub(crate) fn build(
    dom: &Dom,
    viewport: Viewport,
    stylesheets: &[Stylesheet],
    base: &str,
    opts: &crate::LayoutOptions<'_>,
    composed: Option<&pageon_dom::Composed>,
    prev: Option<(BaseDocument, Sheets, Map)>,
) -> (BaseDocument, Map, Option<ImageUse>, Sheets) {
    let (modals, images, secrets) = (opts.modals, opts.images, opts.redact);
    // Pseudo-classes Blitz does not match (`:invalid`, `:state()`) are rewritten to mirror-only
    // markers, only when a sheet uses them.
    let pseudo = uses_pseudo(dom, stylesheets);
    let mut shown: Vec<NodeId> = Vec::new();
    // Subtrees replaced by the control's state (a textarea's value) or hidden (text in a
    // closed `<details>`): their nodes are not mirrored.
    let mut skip_inside: Option<NodeId> = None;
    let recorder = images.map(|_| std::sync::Arc::new(Recorder::default()));
    // The previous layout's document, when it can be laid out again: its sheets stay parsed,
    // and its tree is updated in place (nodes that stay keep their styles and boxes), so
    // Blitz restyles and lays out only what changed.
    let (mut doc, mut sheets, old) = match prev {
        Some((d, s, m)) => (d, s, Some(m)),
        None => {
            let mut d = BaseDocument::new(config(viewport, base, recorder.clone()));
            let s = Sheets::new(&mut d);
            (d, s, None)
        }
    };
    let old = old.as_ref();
    // With a previous tree: each mirrored node's wanted children, placed after the walk.
    let mut kids = Kids::default();
    // Every class and id the page has had: rules that need others are left out. The set only
    // grows, so the sheets' texts (and their parsed form) stay the same from layout to layout.
    let tokens = crate::scope::Tokens::of(dom).map(|t| sheets.seen(t));
    // The document's sheets in cascade order (style elements, links, then adopted sheets).
    let mut ordered: Vec<Sheet> = Vec::new();
    let linked: std::collections::HashMap<NodeId, &Stylesheet> =
        stylesheets.iter().map(|s| (s.link, s)).collect();
    // URLs the engine fetches that are not images: linked stylesheets (pageon loads those
    // itself) and frames.
    let mut not_images: Vec<String> = Vec::new();
    let base_url = url::Url::parse(base).ok();
    let mut map = Map::default();
    let root = doc.root_node().id;
    map.insert(dom.root(), root);
    {
        let mut m = DocumentMutator::new(&mut doc);
        let mut stack: Vec<BId> = vec![root];
        for edge in dom.traverse(dom.root()) {
            match edge {
                Edge::Enter(id) if id == dom.root() => {}
                Edge::Enter(id) => {
                    let Some(node) = dom.get(id) else { continue };
                    let parent_is = |tag: &str| {
                        dom.parent(id)
                            .and_then(|p| dom.get(p))
                            .filter(|p| p.is_html(tag))
                    };
                    // Text directly in a closed <details> is not rendered (only the summary
                    // is); CSS cannot select text, so the mirror leaves it out.
                    let hidden_text = matches!(node.data, NodeData::Text(_))
                        && parent_is("details").is_some_and(|d| d.attr("open").is_none());
                    let created = if skip_inside.is_some() || hidden_text {
                        None
                    } else {
                        match &node.data {
                            NodeData::Element { name, attrs, .. } => {
                                if recorder.is_some()
                                    && matches!(&*name.local, "link" | "iframe" | "frame")
                                {
                                    for a in attrs {
                                        if matches!(&*a.qual_name().local, "href" | "src")
                                            && let Some(u) = base_url
                                                .as_ref()
                                                .and_then(|b| b.join(&a.value).ok())
                                        {
                                            not_images.push(u.to_string());
                                        }
                                    }
                                }
                                let mut attrs: Vec<blitz_dom::Attribute> = attrs
                                    .iter()
                                    .map(|a| blitz_dom::Attribute {
                                        name: a.qual_name().clone(),
                                        value: redact(&a.value, secrets),
                                    })
                                    .collect();
                                // Control state (what was typed, checked or chosen) is shown as
                                // mirror-only attributes, as a browser paints it.
                                let local = &*name.local;
                                let state = dom.control(id);
                                if local == "input" {
                                    if let Some(v) = state.and_then(|c| c.value.as_ref()) {
                                        set_attr(&mut attrs, "value", Some(redact(v, secrets)));
                                    }
                                    if let Some(c) = state.and_then(|c| c.checked) {
                                        set_attr(&mut attrs, "checked", c.then(String::new));
                                    }
                                }
                                if local == "select" {
                                    shown.extend(shown_options(dom, id));
                                }
                                // An image given only by `srcset`: its 1x candidate (or the first).
                                if local == "img"
                                    && !attrs.iter().any(|a| &*a.name.local == "src")
                                    && let Some(src) = node.attr("srcset").and_then(srcset_1x)
                                {
                                    attrs.push(attr("src", src));
                                }
                                if local == "option" && shown.contains(&id) {
                                    attrs.push(attr(SHOWN, String::new()));
                                }
                                // Shadow scopes, for the rewritten selectors.
                                if let Some(c) = composed {
                                    for (table, name) in [
                                        (&c.scope_of, "pageon-scope"),
                                        (&c.host_of, "pageon-host"),
                                        (&c.slotted, "pageon-slot-in"),
                                    ] {
                                        if let Some(n) = table.get(&id) {
                                            attrs.push(attr(name, n.to_string()));
                                        }
                                    }
                                }
                                if pseudo {
                                    markers(dom, id, &mut attrs);
                                }
                                // Modal dialogs live in the top layer: a mirror-only marker the
                                // UA sheet positions (never visible to the page).
                                if modals.contains(&id) {
                                    attrs.push(blitz_dom::Attribute {
                                        name: html5ever::QualName::new(
                                            None,
                                            html5ever::ns!(),
                                            html5ever::LocalName::from(TOP_LAYER),
                                        ),
                                        value: String::new(),
                                    });
                                }
                                // Open popovers: shown and positioned by the UA sheet (closed
                                // ones are `display: none`), and matched by `:popover-open`.
                                if dom.popover_open(id) {
                                    attrs.push(attr("pageon-popover-open", String::new()));
                                }
                                // With scripting on, `<noscript>` is not rendered (UA sheet).
                                if &*name.local == "noscript" && dom.scripting() {
                                    attrs.push(attr("pageon-scripting", String::new()));
                                }
                                // A stylesheet's element: its rules go to the document's sheets
                                // in cascade order, and the element is a placeholder (Blitz would
                                // make a sheet of a `<style>` of its own at every layout).
                                // The text, and its hash when the loader has it.
                                let sheet: Option<(std::borrow::Cow<'_, str>, Option<u64>)> =
                                    match local {
                                        "style" if node.is_html("style") => Some((
                                            dom.cssom_text(id).map_or_else(
                                                || style_text(dom, id).into(),
                                                std::borrow::Cow::Borrowed,
                                            ),
                                            None,
                                        )),
                                        "link" => match dom.cssom_text(id) {
                                            Some(t) => Some((t.into(), None)),
                                            None => linked
                                                .get(&id)
                                                .map(|s| (s.css().into(), Some(s.hash))),
                                        },
                                        _ => None,
                                    };
                                let el = match &sheet {
                                    Some(css) => {
                                        let scope = composed.map(|c| match c.scope_of.get(&id) {
                                            Some(&n) => crate::scope::Scope::Shadow(n),
                                            None => crate::scope::Scope::Document,
                                        });
                                        ordered.push(sheet_css(
                                            &css.0,
                                            css.1,
                                            pseudo,
                                            scope,
                                            tokens.as_ref(),
                                        ));
                                        skip_inside = Some(id);
                                        let sheet = html5ever::QualName::new(
                                            None,
                                            html5ever::ns!(html),
                                            html5ever::LocalName::from(SHEET),
                                        );
                                        reuse_element(&mut m, old, id, &sheet, &[])
                                            .unwrap_or_else(|| m.create_element(sheet, Vec::new()))
                                    }
                                    None => reuse_element(&mut m, old, id, name, &attrs)
                                        .unwrap_or_else(|| m.create_element(name.clone(), attrs)),
                                };
                                // An input's children (a script can add some) are not rendered.
                                if local == "input" {
                                    skip_inside = Some(id);
                                }
                                if local == "textarea"
                                    && let Some(v) = state.and_then(|c| c.value.as_ref())
                                {
                                    let v = redact(v, secrets);
                                    // The value's text node is not a page node: the one already
                                    // there is kept when it is the only child.
                                    let only = m
                                        .doc
                                        .get_node(el)
                                        .filter(|n| n.children.len() == 1)
                                        .map(|n| n.children[0])
                                        .filter(|&c| {
                                            m.doc.get_node(c).is_some_and(|c| c.is_text_node())
                                        });
                                    let t = match only {
                                        Some(t) if old.is_some() => {
                                            m.set_node_text(t, &v);
                                            t
                                        }
                                        _ => m.create_text_node(&v),
                                    };
                                    if old.is_some() {
                                        kids.wanted(t, el);
                                    } else {
                                        m.append_children(el, &[t]);
                                    }
                                    skip_inside = Some(id);
                                }
                                Some(el)
                            }
                            NodeData::Text(t) => {
                                let t = redact(t, secrets);
                                Some(
                                    reuse_text(&mut m, old, id, &t)
                                        .unwrap_or_else(|| m.create_text_node(&t)),
                                )
                            }
                            NodeData::Comment(t) => Some(
                                reuse_comment(&m, old, id)
                                    .unwrap_or_else(|| m.create_comment_node(t)),
                            ),
                            _ => None,
                        }
                    };
                    match created {
                        Some(b) => {
                            // Elements deeper than MAX_DEPTH are laid out as children of their
                            // ancestor at that depth, so a pathologically deep page cannot
                            // overflow the layout's stack. Text and comments stay in their
                            // element (one level more), which keeps its box and its text.
                            let leaf = !matches!(
                                dom.get(id).map(|n| &n.data),
                                Some(NodeData::Element { .. })
                            );
                            let at = if leaf {
                                stack.len()
                            } else {
                                stack.len().min(MAX_DEPTH)
                            };
                            if let Some(&parent) = stack.get(at - 1) {
                                if old.is_some() {
                                    kids.wanted(b, parent);
                                } else {
                                    m.append_children(parent, &[b]);
                                }
                            }
                            // Blitz gives button-like inputs a text child of its own (their label).
                            if old.is_some() && !node.is_html("input") {
                                kids.node(b);
                            }
                            map.insert(id, b);
                            stack.push(b);
                        }
                        // Doctypes, PIs and fragments are not part of the layout tree; keep
                        // the stack balanced with the parent.
                        None => stack.push(*stack.last().unwrap_or(&root)),
                    }
                }
                Edge::Leave(id) if id == dom.root() => {}
                Edge::Leave(id) => {
                    if skip_inside == Some(id) {
                        skip_inside = None;
                    }
                    stack.pop();
                }
            }
        }
        if old.is_some() {
            kids.node(root);
            kids.place(&mut m);
        }
    }
    // Adopted stylesheets: after every sheet of the document, scoped to their tree.
    for (scope, css) in adopted(dom, composed) {
        ordered.push(sheet_css(&css, None, pseudo, scope, tokens.as_ref()));
    }
    let sheets_changed = sheets.attach(&mut doc, ordered);
    if old.is_some() {
        if sheets_changed {
            // Stylo invalidates a sheet change only partly on a kept tree: the whole document
            // is restyled, as in a fresh layout.
            let root = doc.root_element().id;
            if let Some(n) = doc.get_node_mut(root) {
                n.set_restyle_hint(
                    style::invalidation::element::restyle_hints::RestyleHint::restyle_subtree(),
                );
                n.mark_ancestors_dirty();
            }
        }
        // Styles are kept and only what changed is restyled; boxes are built and laid out
        // again in full (Blitz's incremental box layout keeps stale sizes in places).
        doc.set_incremental_layout(false);
    }
    if opts.styles_only {
        doc.resolve_stylist(0.0);
    } else {
        doc.resolve(0.0);
        if old.is_some() {
            stale_inline_roots(&mut doc, &map);
        }
    }
    let used = recorder.zip(images).map(|(rec, images)| {
        let mut used = ImageUse::default();
        for url in rec.urls() {
            if not_images.contains(&url) {
                continue;
            }
            match images.get(&url) {
                Some(Decoded::Raster {
                    width,
                    height,
                    rgba,
                }) => {
                    doc.load_resource(blitz_dom::net::ResourceLoadResponse {
                        request_id: 0,
                        node_id: None,
                        resolved_url: Some(url.clone()),
                        result: Ok(blitz_dom::net::Resource::Image(
                            blitz_dom::util::ImageType::Image,
                            *width,
                            *height,
                            rgba.clone(),
                        )),
                    });
                    used.applied += 1;
                }
                Some(Decoded::Failed(_)) => {}
                None => used.wanted.push(url),
            }
        }
        if used.applied > 0 {
            doc.resolve(0.0);
        }
        used
    });
    if let Some(old) = old {
        // A kept node scrolled before and not now (the page scrolled it back) is at 0.
        let reset: Vec<(NodeId, f64, f64)> = old
            .scrolled
            .iter()
            .filter_map(|&b| map.pageon(b))
            .filter(|id| !opts.scroll.iter().any(|(s, ..)| s == id))
            .map(|id| (id, 0.0, 0.0))
            .collect();
        scroll(&mut doc, &mut map, &reset);
        map.scrolled.clear();
    }
    scroll(&mut doc, &mut map, opts.scroll);
    (doc, map, used, sheets)
}

/// The children each mirrored node should have, in order (laying out a previous tree again).
#[derive(Default)]
struct Kids {
    of: Vec<(BId, Vec<BId>)>,
    slot: HashMap<BId, usize>,
}

impl Kids {
    /// A mirrored node: its children are exactly those [`Kids::wanted`] gives it.
    fn node(&mut self, b: BId) {
        if !self.slot.contains_key(&b) {
            self.slot.insert(b, self.of.len());
            self.of.push((b, Vec::new()));
        }
    }

    fn wanted(&mut self, child: BId, parent: BId) {
        self.node(parent);
        let i = self.slot[&parent];
        self.of[i].1.push(child);
    }

    /// Gives every node its wanted children: the part that differs from what it has (between a
    /// common prefix and suffix) is replaced, and nodes no longer wanted anywhere are dropped.
    fn place(self, m: &mut DocumentMutator<'_>) {
        // Nodes wanted somewhere are moved, not removed and added again (which would run
        // Blitz's insertion steps twice, such as a button input's label).
        let wanted: std::collections::HashSet<BId> = self
            .of
            .iter()
            .flat_map(|(_, w)| w.iter().copied())
            .collect();
        let mut removed: Vec<BId> = Vec::new();
        for (parent, want) in &self.of {
            let have = m.child_ids(*parent);
            if have.as_slice() == want.as_slice() {
                continue;
            }
            let pre = have.iter().zip(want).take_while(|(a, b)| a == b).count();
            let room = have.len().min(want.len()) - pre;
            let suf = have
                .iter()
                .rev()
                .zip(want.iter().rev())
                .take(room)
                .take_while(|(a, b)| a == b)
                .count();
            for &c in &have[pre..have.len() - suf] {
                if !wanted.contains(&c) {
                    m.remove_node(c);
                    removed.push(c);
                }
            }
            let mid = &want[pre..want.len() - suf];
            if mid.is_empty() {
                continue;
            }
            // A moved node's layout parent is from its old place (an anonymous block that may
            // be dropped); its new parent's layout sets it again, if it lays out children.
            for &c in mid {
                if let Some(n) = m.doc.get_node(c) {
                    n.layout_parent.set(None);
                }
            }
            match have
                .len()
                .checked_sub(suf)
                .and_then(|i| have.get(i))
                .filter(|_| suf > 0)
            {
                Some(&anchor) => m.insert_nodes_before(anchor, mid),
                None => m.append_children(*parent, mid),
            }
        }
        // What is no longer wanted is dropped, with what it still holds.
        for b in removed {
            m.remove_node_if_unparented(b);
        }
    }
}

/// Blitz clears an element's inline-root flag only when it builds that element's box: one that
/// became plain inline on a kept tree (an `<a>` that was a flex item) keeps it, and with it its
/// old box. Cleared after layout.
fn stale_inline_roots(doc: &mut BaseDocument, map: &Map) {
    let ids: Vec<BId> = map.blitz_ids().collect();
    for b in ids {
        let Some(n) = doc.get_node_mut(b) else {
            continue;
        };
        if n.flags.is_inline_root() {
            n.flags.reset_construction_flags();
            if !n.is_non_atomic_inline() {
                n.flags.insert(blitz_dom::node::NodeFlags::IS_INLINE_ROOT);
            }
        }
    }
}

/// The previous layout's element for `id`, when it has this name, with its attributes made
/// `attrs` (set and cleared as a page's script would, so only what changed is restyled).
fn reuse_element(
    m: &mut DocumentMutator<'_>,
    old: Option<&Map>,
    id: NodeId,
    name: &html5ever::QualName,
    attrs: &[blitz_dom::Attribute],
) -> Option<BId> {
    let b = old?.blitz(id)?;
    let el = m.doc.get_node(b)?.element_data()?;
    if el.name != *name {
        return None;
    }
    let have = el.attrs();
    let stale: Vec<html5ever::QualName> = have
        .iter()
        .filter(|a| !attrs.iter().any(|w| w.name == a.name))
        .map(|a| a.name.clone())
        .collect();
    let changed: Vec<&blitz_dom::Attribute> = attrs
        .iter()
        .filter(|w| !have.iter().any(|a| a.name == w.name && a.value == w.value))
        .collect();
    for n in stale {
        m.clear_attribute(b, n);
    }
    for w in changed {
        m.set_attribute(b, w.name.clone(), &w.value);
    }
    Some(b)
}

/// The previous layout's text node for `id`, with its text made `text`.
fn reuse_text(
    m: &mut DocumentMutator<'_>,
    old: Option<&Map>,
    id: NodeId,
    text: &str,
) -> Option<BId> {
    let b = old?.blitz(id)?;
    m.doc.get_node(b).filter(|n| n.is_text_node())?;
    m.set_node_text(b, text);
    Some(b)
}

/// The previous layout's comment node for `id` (a comment's text does not affect layout).
fn reuse_comment(m: &DocumentMutator<'_>, old: Option<&Map>, id: NodeId) -> Option<BId> {
    let b = old?.blitz(id)?;
    m.doc
        .get_node(b)
        .filter(|n| matches!(n.data, blitz_dom::node::NodeData::Comment { .. }))
        .map(|_| b)
}

/// The element a stylesheet's element is mirrored as (`display: none` in the UA sheet).
const SHEET: &str = "pageon-sheet";

/// The text of a `<style>`.
fn style_text(dom: &Dom, id: NodeId) -> String {
    dom.children(id)
        .filter_map(|c| match dom.get(c).map(|n| &n.data) {
            Some(NodeData::Text(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

/// A layout document's stylesheets: attached, in cascade order, to holder elements made before
/// anything else (Blitz orders sheets by their element's id), and parsed once per text. A
/// document laid out again keeps them: sheets that did not change are not parsed again and the
/// cascade is not rebuilt, as browsers keep a page's style data between layouts.
pub(crate) struct Sheets {
    holders: Vec<BId>,
    /// The classes and ids of every layout so far.
    tokens: Option<crate::scope::Tokens>,
    /// The text hash each holder has (0: none).
    texts: Vec<u64>,
    parsed: std::collections::HashMap<u64, style::stylesheets::DocumentStyleSheet>,
}

/// Holders per document: past it, the last holds the rest of the sheets together.
const HOLDERS: usize = 256;

impl Sheets {
    fn new(doc: &mut BaseDocument) -> Self {
        let mut m = DocumentMutator::new(doc);
        let holders = (0..HOLDERS)
            .map(|_| {
                m.create_element(
                    html5ever::QualName::new(
                        None,
                        html5ever::ns!(html),
                        html5ever::LocalName::from(SHEET),
                    ),
                    Vec::new(),
                )
            })
            .collect();
        drop(m);
        Self {
            holders,
            tokens: None,
            texts: vec![0; HOLDERS],
            parsed: std::collections::HashMap::new(),
        }
    }

    /// The classes and ids of this layout added to those of the earlier ones.
    fn seen(&mut self, now: crate::scope::Tokens) -> crate::scope::Tokens {
        let all = match self.tokens.take() {
            Some(t) => t.union(now),
            None => now,
        };
        self.tokens = Some(all.clone());
        all
    }

    /// Gives each holder its sheet; holders whose text is unchanged are left alone. Whether any
    /// changed.
    fn attach(&mut self, doc: &mut BaseDocument, mut ordered: Vec<Sheet>) -> bool {
        let mut changed = false;
        if ordered.len() > HOLDERS {
            let rest: Vec<&str> = ordered[HOLDERS - 1..].iter().map(|s| &*s.text).collect();
            let text: std::rc::Rc<str> = rest.join("\n").into();
            ordered.truncate(HOLDERS - 1);
            ordered.push(Sheet {
                hash: crate::scope::fast_hash(&text),
                text,
            });
        }
        if self.parsed.len() > 4 * HOLDERS {
            self.parsed.clear();
        }
        for i in 0..HOLDERS {
            let (css, hash) = ordered
                .get(i)
                .filter(|s| !s.text.is_empty())
                .map_or(("", 0), |s| (&*s.text, s.hash | 1));
            if self.texts[i] == hash {
                continue;
            }
            let sheet = self
                .parsed
                .entry(hash)
                .or_insert_with(|| doc.make_stylesheet(css, Origin::Author))
                .clone();
            doc.add_stylesheet_for_node(sheet, self.holders[i]);
            self.texts[i] = hash;
            changed = true;
        }
        changed
    }
}

/// Applies element scroll offsets, each clamped to its element's range.
fn scroll(doc: &mut BaseDocument, map: &mut Map, offsets: &[(NodeId, f64, f64)]) {
    for &(id, x, y) in offsets {
        let Some(b) = map.blitz(id) else { continue };
        let Some(node) = doc.get_node_mut(b) else {
            continue;
        };
        map.scrolled.push(b);
        // A textarea scrolls its text, inside the editor.
        if let Some(max) = text_range(node) {
            if let Some(t) = node
                .element_data_mut()
                .and_then(|e| e.text_input_data_mut())
            {
                t.scroll_offset = (y as f32).clamp(0.0, max);
            }
            continue;
        }
        let l = node.final_layout();
        let (mx, my) = (f64::from(l.scroll_width()), f64::from(l.scroll_height()));
        *node.scroll_offset_mut() = blitz_dom::Point {
            x: x.clamp(0.0, mx),
            y: y.clamp(0.0, my),
        };
    }
}

/// The vertical scroll range of a multi-line text input (its text height less its content box).
pub(crate) fn text_range(node: &blitz_dom::Node) -> Option<f32> {
    let t = node.element_data()?.text_input_data()?;
    if !t.is_multiline {
        return None;
    }
    let layout = t.editor.try_layout()?;
    let text = layout.height() / layout.scale();
    Some((text - node.final_layout().content_box_height()).max(0.0))
}

/// The mirrored elements with `position: fixed`.
pub(crate) fn fixed(doc: &BaseDocument, map: &Map) -> std::collections::HashSet<BId> {
    map.blitz_ids()
        .filter(|&b| {
            doc.get_node(b).is_some_and(|n| {
                n.element_data().is_some() && n.taffy_position() == taffy::Position::Fixed
            })
        })
        .collect()
}
