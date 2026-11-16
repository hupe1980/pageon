//! The engine's page state in the capture model of the agent views, so the proven views (`ax`, refs, diffs,
//! budgets) run on pageon's own DOM, layout and accessibility tree.
//!
//! - Frames are numbered `F0` (the page), then breadth-first; failed frames keep their number.
//! - Backend ids are unique across frames and never alias a removed node:
//!   `frame << 48 | generation << 32 | (node index + 1)`.
//! - Roles use the vocabulary the views key on (Chromium's for document, text, iframe, label,
//!   summary and date/time inputs; ARIA otherwise).

use std::collections::{BTreeMap, HashMap};

use pageon_agent::capture::{
    AxNode as CAxNode, AxProperty, AxValue, DocRef, DocumentSnapshot, DomSnapshot, Frame as CFrame,
    FrameState as CState, LayoutTreeSnapshot, LoadState, Meta, NodeTreeSnapshot, SCHEMA,
};
use pageon_ax::{Checked, Ignored, Tree};
use pageon_dom::{Edge, NodeData, NodeId};
use pageon_layout::{Layout, Viewport};
use serde_json::Value;

use crate::{Document, FrameState};

/// One frame of the flattened page.
pub(crate) struct Entry<'a> {
    pub(crate) doc: Option<&'a Document>,
    pub(crate) parent: Option<usize>,
    pub(crate) owner: Option<NodeId>,
    pub(crate) url: String,
    pub(crate) origin: String,
    pub(crate) failed: bool,
}

/// The page's frames, breadth-first (index = frame number).
pub(crate) fn entries(root: &Document) -> Vec<Entry<'_>> {
    let mut out = vec![Entry {
        doc: Some(root),
        parent: None,
        owner: None,
        url: root.final_url().to_string(),
        origin: crate::frames::origin_of(root.final_url()),
        failed: false,
    }];
    let mut i = 0;
    while i < out.len() {
        if let Some(doc) = out[i].doc {
            for f in doc.frames() {
                out.push(Entry {
                    doc: f.document(),
                    parent: Some(i),
                    owner: Some(f.owner()),
                    url: f.url().to_string(),
                    origin: f.origin().to_string(),
                    failed: matches!(f.state(), FrameState::Failed(_)),
                });
            }
        }
        i += 1;
    }
    out
}

/// Each frame's document (`None` for failed frames), by frame number.
pub(crate) fn frame_documents(root: &Document) -> Vec<Option<&Document>> {
    entries(root).into_iter().map(|e| e.doc).collect()
}

pub(crate) fn backend(frame: usize, id: NodeId) -> i64 {
    ((frame as i64 & 0x7fff) << 48)
        | (i64::from(id.generation() & 0xffff) << 32)
        | (i64::from(id.index()) + 1)
}

/// The node index and generation (low 16 bits) inside its document of a capture backend id.
pub(crate) fn dom_index(backend: i64) -> Option<(u32, u32)> {
    let index = u32::try_from((backend & 0xffff_ffff) - 1).ok()?;
    Some((index, ((backend >> 32) & 0xffff) as u32))
}

#[derive(Default)]
struct Strings {
    list: Vec<String>,
    index: HashMap<String, i64>,
}

impl Strings {
    fn id(&mut self, s: &str) -> i64 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.list.len() as i64;
        self.list.push(s.to_string());
        self.index.insert(s.to_string(), i);
        i
    }
}

/// The layout facts of one element, as the views read them.
struct Facts {
    bounds: [f64; 4],
    styles: [String; 8],
    paint: i64,
    /// A scroll container's `[scrollLeft, scrollTop, scrollWidth, scrollHeight]`.
    scroll: Option<[f64; 4]>,
}

/// The size of an `<iframe>`'s content box in its parent's layout.
fn content_size(layout: &Layout, owner: NodeId) -> Option<Viewport> {
    let b = layout.box_of(owner)?;
    let px = |p: &str| {
        layout
            .style(owner, p)
            .and_then(|v| v.trim_end_matches("px").parse::<f32>().ok())
            .unwrap_or(0.0)
    };
    let w = b.width
        - px("border-left-width")
        - px("border-right-width")
        - px("padding-left")
        - px("padding-right");
    let h = b.height
        - px("border-top-width")
        - px("border-bottom-width")
        - px("padding-top")
        - px("padding-bottom");
    Some(Viewport {
        width: w.max(1.0).round() as u32,
        height: h.max(1.0).round() as u32,
    })
}

/// Builds the capture of `doc` at `viewport`; also returns whether layout failed anywhere.
pub(crate) fn build(doc: &Document, viewport: Viewport) -> (Capture, bool) {
    let frames = entries(doc);
    let mut strings = Strings::default();
    let mut documents = Vec::new();
    let mut out_frames = Vec::new();
    let mut layouts: Vec<Option<Layout>> = Vec::new();
    let mut layout_failed = false;
    for (fi, entry) in frames.iter().enumerate() {
        let frame_id = format!("F{fi}");
        let owner_backend = entry.owner.zip(entry.parent).map(|(o, p)| backend(p, o));
        let Some(fdoc) = entry.doc else {
            layouts.push(None);
            out_frames.push(CFrame {
                frame_id,
                parent_frame_id: entry.parent.map(|p| format!("F{p}")),
                owner_backend_node_id: owner_backend,
                url: entry.url.clone(),
                origin: entry.origin.clone(),
                state: if entry.failed {
                    CState::Failed
                } else {
                    CState::Loading
                },
                document: None,
                ax_nodes: Vec::new(),
            });
            continue;
        };
        let vp = match (entry.parent, entry.owner) {
            (Some(p), Some(o)) => layouts[p]
                .as_ref()
                .and_then(|l| content_size(l, o))
                .unwrap_or(Viewport {
                    width: 300,
                    height: 150,
                }),
            _ => viewport,
        };
        let layout = fdoc.layout(vp);
        let usable = layout.error().is_none();
        layout_failed |= !usable;
        let interaction = pageon_ax::Interaction {
            modals: fdoc.modals(),
            focused: fdoc.focused(),
        };
        // Shadow trees: the composed (flat) tree, as the page renders (same node ids).
        let composed = fdoc.dom().has_shadow().then(|| fdoc.dom().composed().dom);
        let dom = composed.as_ref().unwrap_or(fdoc.dom());
        let tree = Tree::build_with(dom, Some(&layout), interaction);
        let snapshot = document(
            fi,
            fdoc,
            dom,
            usable.then_some(&layout),
            &mut strings,
            &frame_id,
        );
        let ax_nodes = ax_nodes(fi, dom, &tree);
        out_frames.push(CFrame {
            frame_id,
            parent_frame_id: entry.parent.map(|p| format!("F{p}")),
            owner_backend_node_id: owner_backend,
            url: entry.url.clone(),
            origin: entry.origin.clone(),
            state: CState::Loaded,
            document: Some(DocRef {
                snapshot: 0,
                document: documents.len(),
            }),
            ax_nodes,
        });
        documents.push(snapshot);
        layouts.push(Some(layout));
    }
    let title = Tree::build(doc.dom(), None)
        .nodes()
        .first()
        .map(|n| n.name.clone())
        .unwrap_or_default();
    let mut extras = BTreeMap::new();
    if layout_failed {
        extras.insert("layout".to_string(), "unavailable".to_string());
    }
    if let Some(src) = doc.source {
        extras.insert("source".to_string(), src.to_string());
    }
    if let Some(u) = &doc.usage {
        extras.insert("usage".to_string(), u.clone());
    }
    let capture = Capture {
        schema: SCHEMA,
        url: doc.final_url().to_string(),
        title,
        load_state: LoadState::Complete,
        viewport: {
            let top = layouts.first().and_then(Option::as_ref);
            let (x, y) = top.map_or((0.0, 0.0), pageon_layout::Layout::viewport_scroll);
            let (w, h) = top.map_or((0.0, 0.0), pageon_layout::Layout::document_size);
            pageon_agent::capture::Viewport {
                x: f64::from(x),
                y: f64::from(y),
                width: f64::from(viewport.width),
                height: f64::from(viewport.height),
                content_width: f64::from(w),
                content_height: f64::from(h),
            }
        },
        snapshots: vec![DomSnapshot {
            documents,
            strings: strings.list,
        }],
        frames: out_frames,
        baselines: BTreeMap::new(),
        extras,
        meta: Meta {
            tool_version: format!("pageon {}", env!("CARGO_PKG_VERSION")),
            // Another document is another navigation: refs never carry over by position.
            nav_counter: doc.nav,
            ..Meta::default()
        },
    };
    (capture, layout_failed)
}

use pageon_agent::Capture;

const STYLES: [&str; 8] = [
    "display",
    "visibility",
    "opacity",
    "pointer-events",
    "cursor",
    "position",
    "z-index",
    "overflow",
];

fn document(
    fi: usize,
    doc: &Document,
    dom: &pageon_dom::Dom,
    layout: Option<&Layout>,
    s: &mut Strings,
    frame_id: &str,
) -> DocumentSnapshot {
    let mut nodes = NodeTreeSnapshot::default();
    let mut lay = LayoutTreeSnapshot::default();
    let mut index_of: HashMap<NodeId, i64> = HashMap::new();
    let mut facts: HashMap<NodeId, Facts> = HashMap::new();
    let drops: std::collections::HashSet<NodeId> = doc.drop_targets().iter().copied().collect();
    // Paint order: stacking bucket, then z-index, then tree order.
    let mut paint_keys: Vec<(i32, i32, usize, NodeId)> = Vec::new();
    let mut skip_depth: Option<usize> = None;
    let mut depth = 0usize;
    let mut order = 0usize;
    for e in dom.traverse(dom.root()) {
        match e {
            Edge::Enter(id) => {
                depth += 1;
                if skip_depth.is_some() {
                    continue;
                }
                let Some(n) = dom.get(id) else { continue };
                let (ty, name, value) = match &n.data {
                    NodeData::Document => (9, "#document".to_string(), None),
                    NodeData::Doctype { name, .. } => (10, name.to_string(), None),
                    NodeData::Element { .. } => (
                        1,
                        n.local_name().unwrap_or_default().to_ascii_uppercase(),
                        None,
                    ),
                    NodeData::Text(t) => (3, "#text".to_string(), Some(t.to_string())),
                    NodeData::Comment(t) => (8, "#comment".to_string(), Some(t.to_string())),
                    _ => {
                        skip_depth = Some(depth); // template contents are not part of the page
                        continue;
                    }
                };
                let idx = nodes.backend_node_id.len() as i64;
                index_of.insert(id, idx);
                nodes.parent_index.push(
                    dom.parent(id)
                        .and_then(|p| index_of.get(&p).copied())
                        .unwrap_or(-1),
                );
                nodes.node_type.push(ty);
                nodes.node_name.push(s.id(&name));
                nodes.node_value.push(value.map(|v| s.id(&v)).unwrap_or(-1));
                nodes.backend_node_id.push(backend(fi, id));
                let mut attrs = Vec::new();
                for a in n.attrs() {
                    attrs.push(s.id(&a.qualified_name()));
                    attrs.push(s.id(&a.value));
                }
                nodes.attributes.push(attrs);
                if ty == 1 {
                    if let Some(v) = input_value(dom, id) {
                        nodes.input_value.index.push(idx);
                        nodes.input_value.value.push(s.id(&v));
                    }
                    if clickable(n) {
                        nodes.is_clickable.index.push(idx);
                    }
                    if n.attr("ondrop").is_some() || drops.contains(&id) {
                        nodes.is_drop_target.index.push(idx);
                    }
                    if let Some(l) = layout
                        && let Some(f) = element_facts(l, id)
                    {
                        let position = f.styles[5].as_str();
                        let z: i32 = f.styles[6].parse().unwrap_or(0);
                        let bucket = match (position, z) {
                            ("static" | "", _) => 1,
                            (_, z) if z < 0 => 0,
                            (_, 0) => 2,
                            _ => 3,
                        };
                        paint_keys.push((bucket, if bucket == 1 { 0 } else { z }, order, id));
                        facts.insert(id, f);
                    }
                }
                order += 1;
            }
            Edge::Leave(_) => {
                if skip_depth == Some(depth) {
                    skip_depth = None;
                }
                depth -= 1;
            }
        }
    }
    paint_keys.sort();
    for (rank, (_, _, _, id)) in paint_keys.iter().enumerate() {
        if let Some(f) = facts.get_mut(id) {
            f.paint = rank as i64;
        }
    }
    // Layout objects in tree order: elements with facts, and text whose element has them.
    for e in dom.traverse(dom.root()) {
        let Edge::Enter(id) = e else { continue };
        let Some(&idx) = index_of.get(&id) else {
            continue;
        };
        let f = match dom.get(id).map(|n| &n.data) {
            Some(NodeData::Element { .. }) => facts
                .get(&id)
                .map(|f| (f.bounds, f.styles.clone(), f.paint, f.scroll)),
            Some(NodeData::Text(t)) if !t.trim().is_empty() => {
                dom.parent(id).and_then(|p| facts.get(&p)).map(|f| {
                    let mut st = f.styles.clone();
                    st[0] = "inline".into();
                    st[2] = "1".into();
                    (f.bounds, st, f.paint, None)
                })
            }
            _ => None,
        };
        if let Some((bounds, styles, paint, scroll)) = f {
            lay.node_index.push(idx);
            lay.bounds.push(bounds.to_vec());
            lay.styles.push(styles.iter().map(|v| s.id(v)).collect());
            lay.paint_orders.push(paint);
            lay.scroll_rects
                .push(scroll.map(|r| r.to_vec()).unwrap_or_default());
        }
    }
    DocumentSnapshot {
        document_url: s.id(doc.final_url()),
        frame_id: s.id(frame_id),
        nodes,
        layout: lay,
    }
}

fn element_facts(l: &Layout, id: NodeId) -> Option<Facts> {
    if l.visibility(id) == pageon_layout::Visibility::NotRendered {
        return None;
    }
    let mut styles = STYLES.map(|p| l.style(id, p).unwrap_or_default());
    if styles[0] == "contents" {
        return None; // no box of its own; children carry the layout
    }
    // Clipped away (`overflow: hidden`, `clip`): no one sees it, so the views mark its text as
    // hidden. Accessibility (names) is untouched, as in browsers.
    if l.clipped(id) {
        styles[1] = "hidden".into();
    }
    let r = l.box_of(id).unwrap_or(pageon_layout::Rect {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    });
    Some(Facts {
        bounds: [
            f64::from(r.x),
            f64::from(r.y),
            f64::from(r.width),
            f64::from(r.height),
        ],
        styles,
        paint: 0,
        scroll: l
            .scroll_range(id)
            .filter(|&(mx, my)| mx > 0.0 || my > 0.0)
            .map(|(mx, my)| {
                let (x, y) = l.scroll_of(id);
                [
                    f64::from(x),
                    f64::from(y),
                    f64::from(r.width + mx),
                    f64::from(r.height + my),
                ]
            }),
    })
}

/// Whether a node is a password field: its value is never shown.
fn is_password(dom: &pageon_dom::Dom, id: NodeId) -> bool {
    dom.get(id).is_some_and(|n| {
        n.is_html("input")
            && n.attr("type")
                .is_some_and(|t| t.eq_ignore_ascii_case("password"))
    })
}

/// What a password field's value shows (not its length either).
const MASK: &str = "•••";

fn input_value(dom: &pageon_dom::Dom, id: NodeId) -> Option<String> {
    let n = dom.get(id)?;
    if n.is_html("input") {
        let v = n.attr("value").map(str::to_string);
        return if is_password(dom, id) {
            v.filter(|v| !v.is_empty()).map(|_| MASK.into())
        } else {
            v
        };
    }
    if n.is_html("textarea") {
        return Some(dom.text(id));
    }
    None
}

/// Responds to clicks without script: interactive elements and inline `onclick` handlers.
fn clickable(n: &pageon_dom::Node) -> bool {
    n.attr("onclick").is_some()
        || (n.is_html("a") && n.attr("href").is_some())
        || ["button", "select", "textarea", "summary", "label"]
            .iter()
            .any(|t| n.is_html(t))
        || (n.is_html("input")
            && !n
                .attr("type")
                .is_some_and(|t| t.eq_ignore_ascii_case("hidden")))
}

fn val(kind: &str, v: Value) -> AxValue {
    AxValue {
        kind: kind.into(),
        value: Some(v),
    }
}

fn prop(name: &str, kind: &str, v: Value) -> AxProperty {
    AxProperty {
        name: name.into(),
        value: val(kind, v),
    }
}

/// The role string the views key on.
fn view_role(dom: &pageon_dom::Dom, node: &pageon_ax::AxNode, root: bool) -> String {
    if root {
        return "RootWebArea".into();
    }
    if node.role == "text" {
        return "StaticText".into();
    }
    let Some(n) = dom.get(node.dom) else {
        return node.role.into();
    };
    let ty = n
        .attr("type")
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let explicit = n.attr("role").is_some();
    match n.local_name().unwrap_or_default() {
        "iframe" if !explicit => "Iframe".into(),
        "label" if node.role == "generic" => "LabelText".into(),
        "summary" if !explicit => "DisclosureTriangle".into(),
        "input" if !explicit && n.is_html("input") => match ty.as_str() {
            "date" | "month" | "week" => "Date".into(),
            "time" => "InputTime".into(),
            "datetime-local" => "DateTime".into(),
            "color" => "ColorWell".into(),
            _ => node.role.into(),
        },
        _ if node.role == "graphics-document" => "image".into(),
        _ => node.role.into(),
    }
}

fn ax_nodes(fi: usize, dom: &pageon_dom::Dom, tree: &Tree) -> Vec<CAxNode> {
    tree.nodes()
        .iter()
        .enumerate()
        .map(|(i, node)| {
            let mut properties = Vec::new();
            let s = &node.states;
            if let Some(c) = s.checked {
                let v = match c {
                    Checked::True => "true",
                    Checked::False => "false",
                    Checked::Mixed => "mixed",
                };
                properties.push(prop("checked", "tristate", Value::String(v.into())));
            }
            if s.disabled {
                properties.push(prop("disabled", "boolean", Value::Bool(true)));
            }
            if let Some(e) = s.expanded {
                properties.push(prop("expanded", "boolean", Value::Bool(e)));
            }
            if s.selected {
                properties.push(prop("selected", "boolean", Value::Bool(true)));
            }
            if s.required {
                properties.push(prop("required", "boolean", Value::Bool(true)));
            }
            if s.invalid {
                properties.push(prop("invalid", "token", Value::String("true".into())));
            }
            if let Some(l) = s.level {
                properties.push(prop("level", "integer", Value::from(l)));
            }
            if s.modal {
                properties.push(prop("modal", "boolean", Value::Bool(true)));
            }
            if s.focused {
                properties.push(prop("focused", "boolean", Value::Bool(true)));
            }
            let ignored_reasons = match node.ignored {
                Some(Ignored::AriaHidden) => {
                    vec![prop("ariaHiddenElement", "boolean", Value::Bool(true))]
                }
                Some(Ignored::NotRendered) => {
                    vec![prop("notRendered", "boolean", Value::Bool(true))]
                }
                Some(Ignored::NotVisible) => vec![prop("notVisible", "boolean", Value::Bool(true))],
                Some(Ignored::Presentational) => {
                    vec![prop("presentationalRole", "boolean", Value::Bool(true))]
                }
                Some(Ignored::Inert) => vec![prop("inertSubtree", "boolean", Value::Bool(true))],
                None => Vec::new(),
            };
            CAxNode {
                node_id: i.to_string(),
                ignored: node.ignored.is_some(),
                ignored_reasons,
                role: Some(val(
                    "role",
                    Value::String(view_role(dom, node, node.parent.is_none())),
                )),
                name: (!node.name.is_empty())
                    .then(|| val("computedString", Value::String(node.name.clone()))),
                value: node.value.as_ref().map(|v| {
                    let shown = if is_password(dom, node.dom) && !v.is_empty() {
                        MASK.to_string()
                    } else {
                        v.clone()
                    };
                    val("string", Value::String(shown))
                }),
                properties,
                parent_id: node.parent.map(|p| p.to_string()),
                child_ids: node.children.iter().map(ToString::to_string).collect(),
                backend_dom_node_id: Some(backend(fi, node.dom)),
            }
        })
        .collect()
}
