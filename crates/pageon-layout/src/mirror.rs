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

/// A stylesheet's text for the mirror: the rules that cannot match the document dropped
/// (`tokens`), pseudo-classes rewritten to markers (`pseudo`), and selectors scoped to their
/// tree (pages with shadow trees).
fn sheet_css<'a>(
    css: &'a str,
    pseudo: bool,
    scope: Option<crate::scope::Scope>,
    tokens: Option<&crate::scope::Tokens>,
) -> std::borrow::Cow<'a, str> {
    let pruned = tokens.and_then(|t| crate::scope::prune_memo(css, t));
    let text: &str = pruned.as_deref().unwrap_or(css);
    let marked = pseudo.then(|| crate::scope::pseudo_memo(text)).flatten();
    let text: &str = marked.as_deref().unwrap_or(text);
    let scoped = scope.map(|s| crate::scope::rewrite_memo(text, s));
    match (scoped, marked, pruned) {
        (Some(s), ..) => String::from(&*s).into(),
        (None, Some(m), _) => String::from(&*m).into(),
        (None, None, Some(p)) => String::from(&*p).into(),
        (None, None, None) => css.into(),
    }
}

/// Whether any stylesheet of the document uses a pseudo-class the mirror marks.
fn uses_pseudo(dom: &Dom, stylesheets: &[Stylesheet]) -> bool {
    stylesheets
        .iter()
        .any(|s| crate::scope::uses_pseudo_memo(&s.css))
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
) -> (BaseDocument, Map, Option<ImageUse>) {
    let (modals, images, secrets) = (opts.modals, opts.images, opts.redact);
    // Pseudo-classes Blitz does not match (`:invalid`, `:state()`) are rewritten to mirror-only
    // markers, only when a sheet uses them.
    let pseudo = uses_pseudo(dom, stylesheets);
    // The document's classes and ids: rules that need others are left out.
    let tokens = crate::scope::Tokens::of(dom);
    let mut shown: Vec<NodeId> = Vec::new();
    // Subtrees replaced by the control's state (a textarea's value) or hidden (text in a
    // closed `<details>`): their nodes are not mirrored.
    let mut skip_inside: Option<NodeId> = None;
    let recorder = images.map(|_| std::sync::Arc::new(Recorder::default()));
    let mut doc = BaseDocument::new(config(viewport, base, recorder.clone()));
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
                                // A `<link>` stylesheet a script inserted: its CSS, loaded by
                                // the engine, applies as a `<style>`.
                                let linked =
                                    (local == "link").then(|| dom.cssom_text(id)).flatten();
                                let el = match linked {
                                    Some(_) => m.create_element(
                                        html5ever::QualName::new(
                                            None,
                                            html5ever::ns!(html),
                                            html5ever::LocalName::from("style"),
                                        ),
                                        Vec::new(),
                                    ),
                                    None => m.create_element(name.clone(), attrs),
                                };
                                if let Some(css) = linked {
                                    let scope = composed.map(|c| match c.scope_of.get(&id) {
                                        Some(&n) => crate::scope::Scope::Shadow(n),
                                        None => crate::scope::Scope::Document,
                                    });
                                    let t = m.create_text_node(&sheet_css(
                                        css,
                                        pseudo,
                                        scope,
                                        tokens.as_ref(),
                                    ));
                                    m.append_children(el, &[t]);
                                }
                                if local == "textarea"
                                    && let Some(v) = state.and_then(|c| c.value.as_ref())
                                {
                                    let t = m.create_text_node(&redact(v, secrets));
                                    m.append_children(el, &[t]);
                                    skip_inside = Some(id);
                                }
                                // A `<style>` whose rules a script changed (`insertRule`):
                                // those rules, not its text.
                                if local == "style"
                                    && let Some(css) = dom.cssom_text(id)
                                {
                                    let scope = composed.map(|c| match c.scope_of.get(&id) {
                                        Some(&n) => crate::scope::Scope::Shadow(n),
                                        None => crate::scope::Scope::Document,
                                    });
                                    let t = m.create_text_node(&sheet_css(
                                        css,
                                        pseudo,
                                        scope,
                                        tokens.as_ref(),
                                    ));
                                    m.append_children(el, &[t]);
                                    skip_inside = Some(id);
                                }
                                Some(el)
                            }
                            NodeData::Text(t) => {
                                // A `<style>`'s rules, scoped to its tree.
                                let style = dom
                                    .parent(id)
                                    .filter(|&p| dom.get(p).is_some_and(|n| n.is_html("style")));
                                Some(match style {
                                    Some(p) => {
                                        let scope = composed.map(|c| match c.scope_of.get(&p) {
                                            Some(&n) => crate::scope::Scope::Shadow(n),
                                            None => crate::scope::Scope::Document,
                                        });
                                        m.create_text_node(&sheet_css(
                                            t,
                                            pseudo,
                                            scope,
                                            tokens.as_ref(),
                                        ))
                                    }
                                    _ => m.create_text_node(&redact(t, secrets)),
                                })
                            }
                            NodeData::Comment(t) => Some(m.create_comment_node(t)),
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
                                m.append_children(parent, &[b]);
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
        // Adopted stylesheets: after every sheet of the document, scoped to their tree.
        if dom.has_adopted() {
            let html = dom
                .children(dom.root())
                .find(|&c| dom.get(c).is_some_and(|n| n.is_html("html")))
                .and_then(|h| map.blitz(h));
            if let Some(html) = html {
                for (scope, css) in adopted(dom, composed) {
                    let el = m.create_element(
                        html5ever::QualName::new(
                            None,
                            html5ever::ns!(html),
                            html5ever::LocalName::from("style"),
                        ),
                        Vec::new(),
                    );
                    let t = m.create_text_node(&sheet_css(&css, pseudo, scope, tokens.as_ref()));
                    m.append_children(el, &[t]);
                    m.append_children(html, &[el]);
                }
            }
        }
    }
    for sheet in stylesheets {
        if let Some(b) = map.blitz(sheet.link) {
            let scope = composed.map(|_| crate::scope::Scope::Document);
            let css = sheet_css(&sheet.css, pseudo, scope, tokens.as_ref());
            let s = doc.make_stylesheet(&css, Origin::Author);
            doc.add_stylesheet_for_node(s, b);
        }
    }
    doc.resolve(0.0);
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
    scroll(&mut doc, &map, opts.scroll);
    (doc, map, used)
}

/// Applies element scroll offsets, each clamped to its element's range.
fn scroll(doc: &mut BaseDocument, map: &Map, offsets: &[(NodeId, f64, f64)]) {
    for &(id, x, y) in offsets {
        let Some(node) = map.blitz(id).and_then(|b| doc.get_node_mut(b)) else {
            continue;
        };
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
