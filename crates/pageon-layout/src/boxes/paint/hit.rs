//! Adapted from Blitz (blitz-dom `src/node/node.rs` `Node::hit_inner` and `src/document.rs`
//! `BaseDocument::hit`, MIT OR Apache-2.0). Overlay scrollbar thumbs (a Blitz build option
//! pageon leaves off) are not hit.
//!
//! Hit testing on the paint tree: needs [`super::prepare`] to have run after layout.

use parley::layout::Cluster;
use style::computed_values::pointer_events::T as PointerEvents;
use style::computed_values::visibility::T as Visibility;

use super::super::tree::{BoxId, Tree};
use super::LAYOUT_SCALE;

/// A box under a point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct HitResult {
    /// The box hit (for text, the box whose style the text run has).
    pub(crate) node_id: BoxId,
    /// The point relative to the box (to its content box for text).
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// Whether text was hit.
    pub(crate) is_text: bool,
}

/// The box hit at document coordinates (CSS px), as Blitz's `BaseDocument::hit`.
pub(crate) fn hit(tree: &Tree, x: f32, y: f32) -> Option<BoxId> {
    hit_result(tree, x, y).map(|h| h.node_id)
}

/// The hit at document coordinates (CSS px), with the point relative to the box hit.
pub(crate) fn hit_result(tree: &Tree, x: f32, y: f32) -> Option<HitResult> {
    let root = super::root_element(tree)?;
    hit_inner(tree, root, x, y, LAYOUT_SCALE)
}

/// Takes an (x, y) position (relative to the *parent's* top-left corner) and returns:
///    - None if the position is outside of this box's bounds
///    - Some(HitResult) if the position is within the box but doesn't match any children
///    - The result of recursively calling `hit_inner` on the child positioned at that
///      position if there is one.
pub(crate) fn hit_inner(tree: &Tree, id: BoxId, x: f32, y: f32, scale: f64) -> Option<HitResult> {
    let node = &tree[id];

    // Don't hit on visbility:hidden elements
    if let Some(style) = node.primary_styles() {
        if matches!(
            style.clone_visibility(),
            Visibility::Hidden | Visibility::Collapse
        ) {
            return None;
        }
    }

    // pointer-events:none makes this element transparent to hits, but its descendants are
    // still tested (one may restore pointer-events:auto).
    let pointer_events_none = node
        .primary_styles()
        .is_some_and(|style| style.clone_pointer_events() == PointerEvents::None);

    let scroll_offset = node.scroll_offset();
    let mut x = x - node.final_layout().location.x + scroll_offset.x as f32;
    let mut y = y - node.final_layout().location.y + scroll_offset.y as f32;

    if let Some(t) = node.transform {
        let p = t.inverse() * kurbo::Point::new(x as f64 * scale, y as f64 * scale);
        x = (p.x / scale) as f32;
        y = (p.y / scale) as f32;
    }

    let size = node.final_layout().size;
    let matches_self = !(x < 0.0
        || x > size.width + scroll_offset.x as f32
        || y < 0.0
        || y > size.height + scroll_offset.y as f32);

    let overflow_rect = node.final_layout().scrollable_overflow_rect;
    let matches_content = !(x < 0.0
        || x > overflow_rect.right + scroll_offset.x as f32
        || y < 0.0
        || y > overflow_rect.bottom + scroll_offset.y as f32);

    let matches_hoisted_content = match &node.stacking_context {
        Some(sc) => {
            // Both the point (after the scroll adjustment above) and the bbox are in this
            // box's unscrolled content coordinates.
            let bbox = sc.hoisted_content_bbox(tree, id, scale);
            x >= bbox.left && x <= bbox.right && y >= bbox.top && y <= bbox.bottom
        }
        None => false,
    };

    // `scrollable_overflow` is stored in device (scaled) pixels, whereas the coordinates here
    // are in CSS pixels, so unscale it before comparing.
    let overflow = node.scrollable_overflow;

    let matches_overflow = x >= (overflow.x0 / scale) as f32
        && x <= (overflow.x1 / scale) as f32
        && y >= (overflow.y0 / scale) as f32
        && y <= (overflow.y1 / scale) as f32;

    if !matches_self && !matches_content && !matches_hoisted_content && !matches_overflow {
        return None;
    }

    let content_box_offset = taffy::Point {
        x: node.final_layout().padding.left + node.final_layout().border.left,
        y: node.final_layout().padding.top + node.final_layout().border.top,
    };
    if node.is_inline_root() {
        x -= content_box_offset.x;
        y -= content_box_offset.y;
    }

    // Positive z_index hoisted children
    if matches_hoisted_content {
        if let Some(hoisted) = &node.stacking_context {
            for hoisted_child in hoisted.pos_z_hoisted_children().rev() {
                let pos = hoisted_child.position(tree, id);
                let x = x - pos.x;
                let y = y - pos.y;
                if let Some(hit) = hit_inner(tree, hoisted_child.node_id, x, y, scale) {
                    return Some(hit);
                }
            }
        }
    }

    // Call `hit_inner` on each child in turn. If any return `Some` then return that value.
    // Else return `Some(self.id)`.
    for child_id in node.paint_children.borrow().iter().flatten().rev() {
        let child = &tree[*child_id];
        let child_position = child.taffy_position();
        let mut child_x = x;
        let mut child_y = y;
        if child_position.is_out_of_flow() {
            // Out-of-flow children's layout location is relative to this box's border box,
            // so undo the inline-root content-box offset applied above
            if node.is_inline_root() {
                child_x += content_box_offset.x;
                child_y += content_box_offset.y;
            }
            // Fixed-position children of the root element are positioned against the
            // viewport and do not scroll with it. (A fixed box whose containing block is a
            // transformed ancestor scrolls with that ancestor like any other out-of-flow box.)
            if child_position == taffy::Position::Fixed && node.containing_block().is_none() {
                child_x -= scroll_offset.x as f32;
                child_y -= scroll_offset.y as f32;
                let viewport_scroll = super::viewport_scroll(tree);
                child_x -= viewport_scroll.x as f32;
                child_y -= viewport_scroll.y as f32;
            }
        }
        if let Some(hit) = hit_inner(tree, *child_id, child_x, child_y, scale) {
            return Some(hit);
        }
    }

    // Negative z_index hoisted children
    if matches_hoisted_content {
        if let Some(hoisted) = &node.stacking_context {
            for hoisted_child in hoisted.neg_z_hoisted_children().rev() {
                let pos = hoisted_child.position(tree, id);
                let x = x - pos.x;
                let y = y - pos.y;
                if let Some(hit) = hit_inner(tree, hoisted_child.node_id, x, y, scale) {
                    return Some(hit);
                }
            }
        }
    }

    // Inline children
    if node.is_inline_root() {
        if let Some(ild) = node.inline_layout.as_ref() {
            let layout = &ild.layout;
            let scale = layout.scale();
            let y = y - ild.block_offset;

            if let Some((cluster, _side)) = Cluster::from_point_exact(layout, x * scale, y * scale)
            {
                let node_id = cluster.style().brush.id;
                let text_pointer_events_none = tree
                    .nodes
                    .get(node_id)
                    .and_then(|n| n.primary_styles())
                    .is_some_and(|style| style.clone_pointer_events() == PointerEvents::None);
                if !text_pointer_events_none {
                    return Some(HitResult {
                        node_id,
                        x,
                        y,
                        is_text: true,
                    });
                }
            }
        }
    }

    // Self (this box)
    if matches_self && !pointer_events_none {
        return Some(HitResult {
            node_id: id,
            x,
            y,
            is_text: false,
        });
    }

    None
}
