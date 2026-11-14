//! Visibility, viewport, cover and hidden-field checks.

use crate::page::{FramePage, Page, Rect};

/// How a DOM node presents to a sighted user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Rendered and perceivable.
    Visible,
    /// Rendered (in layout) but not perceivable: opacity 0, zero size, `visibility:hidden`.
    Hidden,
    /// Not rendered at all.
    NotRendered,
}

/// Visibility of DOM node `idx` in `frame`.
pub fn visibility(frame: &FramePage, idx: usize) -> Visibility {
    let node = &frame.dom[idx];
    let Some(layout) = node.layout.as_ref() else {
        // Elements without a box (e.g. `display: contents`) inherit from their parent;
        // the AX tree already drops `display: none` subtrees.
        return match node.parent {
            Some(p) => visibility(frame, p),
            None => Visibility::Visible,
        };
    };
    if layout.visibility == "hidden" || layout.visibility == "collapse" {
        return Visibility::Hidden;
    }
    if layout.bounds.area() < 1.0 {
        return Visibility::Hidden;
    }
    let mut opacity = layout.opacity;
    let mut p = node.parent;
    let mut guard = 0;
    while let Some(i) = p {
        guard += 1;
        if guard > 512 {
            break;
        }
        if let Some(l) = frame.dom[i].layout.as_ref() {
            opacity *= l.opacity;
        }
        p = frame.dom[i].parent;
    }
    if opacity <= 0.01 {
        return Visibility::Hidden;
    }
    Visibility::Visible
}

/// Whether node `idx`'s box intersects the viewport.
pub fn in_viewport(page: &Page, frame: &FramePage, idx: usize) -> bool {
    let mut i = Some(idx);
    while let Some(k) = i {
        if let Some(l) = frame.dom[k].layout.as_ref() {
            return l.bounds.intersect(&page.viewport).is_some();
        }
        i = frame.dom[k].parent;
    }
    false
}

/// Whether `a` is an ancestor of (or equal to) `b` in the same frame.
pub fn is_ancestor(frame: &FramePage, a: usize, b: usize) -> bool {
    let mut i = Some(b);
    let mut guard = 0;
    while let Some(k) = i {
        if k == a {
            return true;
        }
        guard += 1;
        if guard > 512 {
            return false;
        }
        i = frame.dom[k].parent;
    }
    false
}

/// Hit-test index for one frame: element boxes sorted top-most first.
#[derive(Debug, Default)]
pub struct HitIndex {
    order: Vec<(i64, usize, Rect)>,
}

impl HitIndex {
    /// Builds the index of a frame.
    pub fn build(frame: &FramePage) -> Self {
        let mut order: Vec<(i64, usize, Rect)> = frame
            .dom
            .iter()
            .enumerate()
            .filter(|(_, n)| n.node_type == 1 && n.name != "html" && n.name != "body")
            .filter_map(|(i, n)| {
                let l = n.layout.as_ref()?;
                // Inline boxes are unions of line boxes and overlap their neighbours; they never
                // count as covering anything.
                let blocks = l.pointer_events != "none"
                    && l.visibility != "hidden"
                    && l.display != "inline"
                    && l.bounds.area() >= 1.0;
                blocks.then_some((l.paint_order, i, l.bounds))
            })
            .collect();
        order.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        Self { order }
    }

    /// The top-most element at a point.
    pub fn top_at(&self, x: f64, y: f64) -> Option<usize> {
        self.order
            .iter()
            .find(|(_, _, r)| r.contains(x, y))
            .map(|&(_, i, _)| i)
    }
}

/// The element covering node `idx` at the centre of its visible part, if an unrelated element
/// is painted over it there. Elements outside the viewport are never reported as covered.
pub fn covered_by(page: &Page, frame: &FramePage, hits: &HitIndex, idx: usize) -> Option<usize> {
    let bounds = frame.dom[idx].layout.as_ref()?.bounds;
    let visible = bounds.intersect(&page.viewport)?;
    let (cx, cy) = (visible.x + visible.w / 2.0, visible.y + visible.h / 2.0);
    let top = hits.top_at(cx, cy)?;
    if is_ancestor(frame, idx, top) || is_ancestor(frame, top, idx) {
        return None;
    }
    if let Some(label) = label_of(frame, idx)
        && is_ancestor(frame, label, top)
    {
        return None;
    }
    Some(top)
}

/// The `<label>` associated with form control `idx` (`for=` or wrapping label).
pub fn label_of(frame: &FramePage, idx: usize) -> Option<usize> {
    let node = &frame.dom[idx];
    if let Some(id) = node.attr("id").filter(|s| !s.is_empty())
        && let Some(l) = frame
            .dom
            .iter()
            .position(|n| n.name == "label" && n.attr("for") == Some(id))
    {
        return Some(l);
    }
    let mut p = node.parent;
    let mut guard = 0;
    while let Some(i) = p {
        guard += 1;
        if guard > 64 {
            break;
        }
        if frame.dom[i].name == "label" {
            return Some(i);
        }
        p = frame.dom[i].parent;
    }
    None
}

/// Whether DOM node `idx` is an editable field.
pub fn is_editable(frame: &FramePage, idx: usize) -> bool {
    let n = &frame.dom[idx];
    match n.name.as_str() {
        "textarea" | "select" => true,
        "input" => !matches!(
            n.attr("type")
                .unwrap_or("text")
                .to_ascii_lowercase()
                .as_str(),
            "button" | "submit" | "reset" | "image" | "hidden"
        ),
        _ => n
            .attr("contenteditable")
            .is_some_and(|v| v.is_empty() || v == "true"),
    }
}
