//! The joined page model: DOM snapshot + layout + accessibility tree, per frame.

use std::collections::HashMap;

use crate::capture::{AxNode, Capture, DocumentSnapshot, FrameState};

/// Identity of a DOM node across steps: frame, top-level navigation, backend node id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeKey {
    /// CDP frame id.
    pub frame_id: String,
    /// The capture's navigation counter.
    pub nav: u32,
    /// Backend node id.
    pub backend_id: i64,
}

/// A rectangle in CSS pixels, top-level document coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Left.
    pub x: f64,
    /// Top.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// Area in square CSS pixels.
    pub fn area(&self) -> f64 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    /// Intersection, if non-empty.
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        (x1 > x0 && y1 > y0).then_some(Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }

    /// Whether the point lies inside.
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// The computed styles pageon captures (in capture order).
pub const STYLE_NAMES: [&str; 8] = [
    "display",
    "visibility",
    "opacity",
    "pointer-events",
    "cursor",
    "position",
    "z-index",
    "overflow",
];

/// Layout of one DOM node.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// Border box in top-level coordinates.
    pub bounds: Rect,
    /// `display`.
    pub display: String,
    /// `visibility`.
    pub visibility: String,
    /// `opacity`.
    pub opacity: f64,
    /// `pointer-events`.
    pub pointer_events: String,
    /// `cursor`.
    pub cursor: String,
    /// Paint order (later = on top).
    pub paint_order: i64,
    /// A scroll container's `[scrollLeft, scrollTop, scrollWidth, scrollHeight]`, when its
    /// `overflow` lets a person scroll it (`auto` or `scroll`).
    pub scroll: Option<[f64; 4]>,
}

/// One DOM node.
#[derive(Debug, Clone, Default)]
pub struct DomNode {
    /// Parent index within the frame's DOM.
    pub parent: Option<usize>,
    /// DOM node type.
    pub node_type: i64,
    /// Lowercase node name (`input`, `#text`, …).
    pub name: String,
    /// Backend node id.
    pub backend_id: i64,
    /// Attributes in source order.
    pub attrs: Vec<(String, String)>,
    /// Layout, if rendered.
    pub layout: Option<Layout>,
    /// Current value of a form control.
    pub input_value: Option<String>,
    /// Responds to clicks (CDP `isClickable`, includes JS listeners).
    pub clickable: bool,
}

impl DomNode {
    /// Attribute value.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// One frame's joined data.
#[derive(Debug, Clone)]
pub struct FramePage {
    /// CDP frame id.
    pub frame_id: String,
    /// Origin.
    pub origin: String,
    /// Whether the origin differs from the top-level origin.
    pub third_party: bool,
    /// Frame state.
    pub state: FrameState,
    /// Backend id of the owner `<iframe>` in the parent frame.
    pub owner_backend_id: Option<i64>,
    /// DOM nodes (empty if the frame has no document).
    pub dom: Vec<DomNode>,
    /// Backend id → DOM index.
    pub by_backend: HashMap<i64, usize>,
    /// AX nodes.
    pub ax: Vec<AxNode>,
    /// AX id → index.
    pub ax_by_id: HashMap<String, usize>,
    /// Backend id → AX index.
    pub ax_by_backend: HashMap<i64, usize>,
    /// Index of the AX root.
    pub ax_root: Option<usize>,
    /// AX nodes dropped because their DOM node was missing.
    pub dropped_ax: usize,
}

/// The joined page.
#[derive(Debug, Clone)]
pub struct Page {
    /// Frames; the first is the top-level frame.
    pub frames: Vec<FramePage>,
    /// Viewport in top-level coordinates.
    pub viewport: Rect,
    /// Navigation counter of the capture.
    pub nav: u32,
}

impl Page {
    /// Builds the joined model from a capture.
    pub fn build(capture: &Capture) -> Self {
        let top_origin = capture
            .frames
            .first()
            .map(|f| f.origin.clone())
            .unwrap_or_default();
        let mut frames: Vec<FramePage> = capture
            .frames
            .iter()
            .map(|f| {
                let dom = f
                    .document
                    .map(|d| {
                        let snap = &capture.snapshots[d.snapshot];
                        decode_document(&snap.documents[d.document], &snap.strings)
                    })
                    .unwrap_or_default();
                let by_backend = dom
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (n.backend_id, i))
                    .collect::<HashMap<_, _>>();
                let mut ax_by_id = HashMap::new();
                let mut ax_by_backend = HashMap::new();
                let mut dropped_ax = 0;
                for (i, n) in f.ax_nodes.iter().enumerate() {
                    ax_by_id.insert(n.node_id.clone(), i);
                    if let Some(b) = n.backend_dom_node_id {
                        if by_backend.contains_key(&b) || dom.is_empty() {
                            ax_by_backend.insert(b, i);
                        } else {
                            dropped_ax += 1;
                        }
                    }
                }
                let ax_root = f.ax_nodes.iter().position(|n| n.parent_id.is_none());
                FramePage {
                    frame_id: f.frame_id.clone(),
                    origin: f.origin.clone(),
                    third_party: f.origin != top_origin,
                    state: f.state,
                    owner_backend_id: f.owner_backend_node_id,
                    dom,
                    by_backend,
                    ax: f.ax_nodes.clone(),
                    ax_by_id,
                    ax_by_backend,
                    ax_root,
                    dropped_ax,
                }
            })
            .collect();
        // Translate child-frame boxes into top-level coordinates (owner iframe offset).
        for i in 1..frames.len() {
            let offset = frame_offset(capture, &frames, i);
            if offset != (0.0, 0.0) {
                for n in &mut frames[i].dom {
                    if let Some(l) = n.layout.as_mut() {
                        l.bounds.x += offset.0;
                        l.bounds.y += offset.1;
                    }
                }
            }
        }
        let v = capture.viewport;
        Self {
            frames,
            viewport: Rect {
                x: v.x,
                y: v.y,
                w: v.width,
                h: v.height,
            },
            nav: capture.meta.nav_counter,
        }
    }

    /// The node key of a DOM node.
    pub fn key(&self, frame: usize, backend_id: i64) -> NodeKey {
        NodeKey {
            frame_id: self.frames[frame].frame_id.clone(),
            nav: self.nav,
            backend_id,
        }
    }

    /// Index of the frame owned by `<iframe>` with `backend_id` in frame `parent`.
    pub fn child_frame(&self, parent: usize, backend_id: i64) -> Option<usize> {
        let parent_id = &self.frames[parent].frame_id;
        self.frames
            .iter()
            .position(|f| f.owner_backend_id == Some(backend_id) && f.frame_id != *parent_id)
    }
}

/// Offset of frame `i` relative to the top-level document: the content box of its owner
/// `<iframe>`, recursively. Same-process child documents report their own coordinates.
fn frame_offset(capture: &Capture, frames: &[FramePage], i: usize) -> (f64, f64) {
    let mut x = 0.0;
    let mut y = 0.0;
    let mut current = i;
    let mut guard = 0;
    while let Some(parent_id) = capture.frames[current].parent_frame_id.as_ref() {
        guard += 1;
        if guard > 32 {
            break;
        }
        let Some(p) = frames.iter().position(|f| &f.frame_id == parent_id) else {
            break;
        };
        if let Some(owner) = frames[current].owner_backend_id
            && let Some(&idx) = frames[p].by_backend.get(&owner)
            && let Some(l) = frames[p].dom[idx].layout.as_ref()
        {
            // Parent boxes are still in the parent's own coordinates here.
            x += l.bounds.x;
            y += l.bounds.y;
        }
        current = p;
    }
    (x, y)
}

fn decode_document(doc: &DocumentSnapshot, strings: &[String]) -> Vec<DomNode> {
    let s = |i: i64| -> String {
        usize::try_from(i)
            .ok()
            .and_then(|i| strings.get(i))
            .cloned()
            .unwrap_or_default()
    };
    let n = doc.nodes.backend_node_id.len();
    let mut nodes: Vec<DomNode> = (0..n)
        .map(|i| {
            let attrs = doc
                .nodes
                .attributes
                .get(i)
                .map(|a| {
                    a.chunks(2)
                        .filter(|c| c.len() == 2)
                        .map(|c| (s(c[0]).to_ascii_lowercase(), s(c[1])))
                        .collect()
                })
                .unwrap_or_default();
            DomNode {
                parent: doc
                    .nodes
                    .parent_index
                    .get(i)
                    .and_then(|&p| usize::try_from(p).ok()),
                node_type: doc.nodes.node_type.get(i).copied().unwrap_or(0),
                name: doc
                    .nodes
                    .node_name
                    .get(i)
                    .map(|&x| s(x).to_ascii_lowercase())
                    .unwrap_or_default(),
                backend_id: doc.nodes.backend_node_id[i],
                attrs,
                layout: None,
                input_value: None,
                clickable: false,
            }
        })
        .collect();
    for (k, &idx) in doc.nodes.input_value.index.iter().enumerate() {
        if let (Ok(idx), Some(&v)) = (usize::try_from(idx), doc.nodes.input_value.value.get(k))
            && let Some(node) = nodes.get_mut(idx)
        {
            node.input_value = Some(s(v));
        }
    }
    for &idx in &doc.nodes.is_clickable.index {
        if let Some(node) = usize::try_from(idx).ok().and_then(|i| nodes.get_mut(i)) {
            node.clickable = true;
        }
    }
    let l = &doc.layout;
    for (k, &idx) in l.node_index.iter().enumerate() {
        let Ok(idx) = usize::try_from(idx) else {
            continue;
        };
        let Some(node) = nodes.get_mut(idx) else {
            continue;
        };
        if node.layout.is_some() {
            continue; // first layout object (the element box) wins over continuations
        }
        let b = l.bounds.get(k).cloned().unwrap_or_default();
        let st = |j: usize| {
            l.styles
                .get(k)
                .and_then(|v| v.get(j))
                .map(|&x| s(x))
                .unwrap_or_default()
        };
        node.layout = Some(Layout {
            bounds: Rect {
                x: b.first().copied().unwrap_or(0.0),
                y: b.get(1).copied().unwrap_or(0.0),
                w: b.get(2).copied().unwrap_or(0.0),
                h: b.get(3).copied().unwrap_or(0.0),
            },
            display: st(0),
            visibility: st(1),
            opacity: st(2).parse().unwrap_or(1.0),
            pointer_events: st(3),
            cursor: st(4),
            paint_order: l.paint_orders.get(k).copied().unwrap_or(0),
            scroll: l
                .scroll_rects
                .get(k)
                .filter(|r| r.len() == 4)
                .filter(|_| {
                    st(7)
                        .split_whitespace()
                        .any(|v| v == "auto" || v == "scroll")
                })
                .map(|r| [r[0], r[1], r[2], r[3]]),
        });
    }
    nodes
}
