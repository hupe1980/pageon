//! Adapted from Blitz (blitz-dom `src/layout/paint_tree.rs`, MIT OR Apache-2.0).
//!
//! Construction of the paint tree (`Node::paint_children` and `Node::stacking_context`) from
//! the laid-out box tree.
//!
//! The pass builds *topology* only: which boxes each box paints itself and which z-indexed
//! descendants are hoisted to the nearest stacking-context root. Geometry (a hoisted box's
//! offset from its stacking-context root) is derived at use time from `final_layout()`.
//! (Blitz rebuilds only damaged subtrees and memoises geometry per generation; pageon builds
//! the whole tree once per layout, so neither is needed.)

use style::computed_values::position::T as Position;
use style::values::computed::Float;
use style::values::specified::box_::DisplayInside;
use taffy::{Point, Rect};

use super::super::tree::{BoxId, Node, Tree};

/// Offset of the hoisted child `child_id` from the border box of its stacking-context root
/// `sc_root_id`, excluding the child's own `final_layout().location`: the sum of
/// `location - scroll_offset` of every box strictly between the two on the child's
/// positioning chain.
///
/// Walks `Node::containing_block` (the out-of-flow containing block for boxes hoisted by
/// Taffy, otherwise `layout_parent`, which is set for every layout child, anonymous blocks
/// included).
pub(crate) fn hoisted_child_position(
    tree: &Tree,
    sc_root_id: BoxId,
    child_id: BoxId,
) -> Point<f32> {
    let child = &tree[child_id];
    let mut position = Point::ZERO;
    let mut current = child.containing_block();

    // A fixed-position box whose containing block is the root element is positioned against
    // the viewport and does not scroll with it: cancel the scroll the walk below (or the
    // stacking-context root itself) applies. A fixed box contained by a transformed ancestor
    // scrolls with it.
    if child.taffy_position() == taffy::Position::Fixed
        && let Some(cb) = current.and_then(|id| tree.nodes.get(id))
        && cb.containing_block().is_none()
    {
        let scroll_offset = cb.scroll_offset();
        let viewport_scroll = super::viewport_scroll(tree);
        position.x += scroll_offset.x as f32 + viewport_scroll.x as f32;
        position.y += scroll_offset.y as f32 + viewport_scroll.y as f32;
    }

    while let Some(id) = current {
        if id == sc_root_id {
            break;
        }
        let Some(node) = tree.nodes.get(id) else {
            break;
        };
        let location = node.final_layout().location;
        let scroll_offset = node.scroll_offset();
        position.x += location.x - scroll_offset.x as f32;
        position.y += location.y - scroll_offset.y as f32;
        current = node.containing_block();
    }
    position
}

/// A child with a z_index that is hoisted up to its containing stacking context for paint
/// purposes.
#[derive(Debug, Clone)]
pub(crate) struct HoistedPaintChild {
    pub(crate) node_id: BoxId,
    pub(crate) z_index: i32,
}

impl HoistedPaintChild {
    fn new(node_id: BoxId, z_index: i32) -> Self {
        Self { node_id, z_index }
    }

    /// The child's offset from the border box of its stacking-context root (`sc_root_id`),
    /// excluding the child's own `final_layout().location`.
    pub(crate) fn position(&self, tree: &Tree, sc_root_id: BoxId) -> Point<f32> {
        hoisted_child_position(tree, sc_root_id, self.node_id)
    }
}

#[derive(Debug)]
pub(crate) struct StackingContext {
    pub(crate) children: Vec<HoistedPaintChild>,
    /// The number of hoisted children with negative z_index
    pub(crate) negative_z_count: u32,
}

impl StackingContext {
    /// Build a sorted stacking context from the hoisted entries collected for it.
    fn from_children(children: Vec<HoistedPaintChild>) -> Self {
        let mut sc = Self {
            children,
            negative_z_count: 0,
        };
        sc.sort();
        sc
    }

    /// Bounding box (in CSS pixels relative to the stacking-context root `sc_root_id`,
    /// unscrolled) of the scrollable overflow of all hoisted children, including their
    /// transforms. `scale` is the device pixel ratio `scrollable_overflow` is stored in.
    pub(crate) fn hoisted_content_bbox(
        &self,
        tree: &Tree,
        sc_root_id: BoxId,
        scale: f64,
    ) -> Rect<f32> {
        let child_rect = |child: &HoistedPaintChild| -> Rect<f32> {
            let node = &tree[child.node_id];
            let position = child.position(tree, sc_root_id);
            let location = node.final_layout().location;
            let mut overflow = node.scrollable_overflow;
            if let Some(transform) = node.transform {
                overflow = transform.transform_rect_bbox(overflow);
            }
            let left = position.x + location.x;
            let top = position.y + location.y;
            Rect {
                left: left + (overflow.x0 / scale) as f32,
                top: top + (overflow.y0 / scale) as f32,
                right: left + (overflow.x1 / scale) as f32,
                bottom: top + (overflow.y1 / scale) as f32,
            }
        };

        let mut area = Rect::ZERO;
        if let Some((first, rest)) = self.children.split_first() {
            area = child_rect(first);
            for child in rest {
                let pos = child_rect(child);
                area.left = area.left.min(pos.left);
                area.top = area.top.min(pos.top);
                area.right = area.right.max(pos.right);
                area.bottom = area.bottom.max(pos.bottom);
            }
        }
        area
    }

    pub(crate) fn sort(&mut self) {
        self.children.sort_by_key(|c| c.z_index);
        self.negative_z_count = self.children.iter().take_while(|c| c.z_index < 0).count() as u32;
    }

    pub(crate) fn neg_z_range(&self) -> std::ops::Range<usize> {
        0..(self.negative_z_count as usize)
    }

    pub(crate) fn pos_z_range(&self) -> std::ops::Range<usize> {
        (self.negative_z_count as usize)..self.children.len()
    }

    pub(crate) fn neg_z_hoisted_children(
        &self,
    ) -> impl ExactSizeIterator<Item = &HoistedPaintChild> + DoubleEndedIterator {
        self.children[self.neg_z_range()].iter()
    }

    pub(crate) fn pos_z_hoisted_children(
        &self,
    ) -> impl ExactSizeIterator<Item = &HoistedPaintChild> + DoubleEndedIterator {
        self.children[self.pos_z_range()].iter()
    }
}

/// Rebuild `paint_children` and `stacking_context` for the tree rooted at `root_id` from the
/// laid-out box tree.
///
/// Each box's layout children are placed either in its `paint_children` (sorted by paint
/// order) or, when they have a non-zero `z-index` and are positioned or flex/grid items, in
/// the nearest ancestor stacking context.
pub(crate) fn build_paint_tree(tree: &mut Tree, root_id: BoxId) {
    build_paint_tree_impl(tree, root_id, None);
}

/// `stacking_context` is the entry list of the nearest ancestor stacking context, or `None`
/// when `node_id` is itself a stacking-context root.
fn build_paint_tree_impl(
    tree: &mut Tree,
    node_id: BoxId,
    stacking_context: Option<&mut Vec<HoistedPaintChild>>,
) {
    let Some(display) = tree[node_id].used_display() else {
        return;
    };

    // Entries hoisted out of this subtree.
    let mut entries: Vec<HoistedPaintChild> = Vec::new();

    let children = tree[node_id].layout_children.borrow_mut().take();
    if let Some(children) = children {
        let is_flex_or_grid = matches!(display.inside(), DisplayInside::Flex | DisplayInside::Grid);

        // Out-of-flow boxes are laid out (and painted) relative to their containing block, as
        // recorded by Taffy's out-of-flow pass in the containing block's `hoisted_children`.
        // A box whose containing block is not its layout parent is owned by the containing
        // block; its layout parent neither visits nor lists it.
        let owned_here = |tree: &Tree, child_id: BoxId| -> bool {
            tree[child_id]
                .oof_containing_block
                .get()
                .is_none_or(|cb| cb == node_id)
        };
        // Boxes hoisted to this box from further down the tree, keyed by the index of the
        // layout child they descend from: they are visited and listed in tree order, directly
        // after that child (CSS 2.1 Appendix E). Boxes whose layout-parent chain does not lead
        // through a layout child of this box come after all of them.
        let mut hoisted_past_parent: Vec<(usize, BoxId)> = tree[node_id]
            .hoisted_children
            .borrow()
            .iter()
            .copied()
            .filter(|&id| {
                tree.nodes
                    .get(id)
                    .is_some_and(|n| n.layout_parent.get() != Some(node_id))
            })
            .map(|id| {
                let anchor = layout_child_containing(tree, node_id, id)
                    .and_then(|anchor| children.iter().position(|&c| c == anchor))
                    .unwrap_or(children.len());
                (anchor, id)
            })
            .collect();
        hoisted_past_parent.sort_by_key(|&(anchor, _)| anchor);
        let mut next_hoisted = 0;

        let mut paint_children: Vec<BoxId> =
            Vec::with_capacity(children.len() + hoisted_past_parent.len());

        for (index, &child_id) in children.iter().enumerate() {
            if owned_here(tree, child_id) {
                let is_sc_root = tree[child_id].is_stacking_context_root(is_flex_or_grid);
                build_paint_tree_impl(
                    tree,
                    child_id,
                    match is_sc_root {
                        true => None,
                        false => Some(&mut entries),
                    },
                );

                // Only stacking contexts with a non-zero z-index are lifted out of tree order
                // into the enclosing stacking context's z-sorted list (CSS 2.1 Appendix E).
                let z_index = tree[child_id].z_index();
                if is_sc_root && z_index != 0 {
                    entries.push(HoistedPaintChild::new(child_id, z_index))
                } else {
                    paint_children.push(child_id);
                }
            }

            while let Some(&(anchor, hoisted_id)) = hoisted_past_parent.get(next_hoisted)
                && anchor == index
            {
                attach_hoisted_past_parent(tree, hoisted_id, &mut paint_children, &mut entries);
                next_hoisted += 1;
            }
        }
        for &(_, hoisted_id) in &hoisted_past_parent[next_hoisted..] {
            attach_hoisted_past_parent(tree, hoisted_id, &mut paint_children, &mut entries);
        }

        paint_children.sort_by_cached_key(|id| node_to_paint_order(&tree[*id], is_flex_or_grid));

        let node = &tree[node_id];
        *node.paint_children.borrow_mut() = Some(paint_children);
        *node.layout_children.borrow_mut() = Some(children);
    }

    let node = &mut tree[node_id];
    if let Some(stacking_context) = stacking_context {
        node.stacking_context = None;
        stacking_context.extend(entries);
    } else {
        node.stacking_context = Some(Box::new(StackingContext::from_children(entries)));
    }
}

/// The layout child of `node_id` whose subtree contains `id`, following `layout_parent`.
fn layout_child_containing(tree: &Tree, node_id: BoxId, id: BoxId) -> Option<BoxId> {
    let mut current = id;
    loop {
        let parent = tree.nodes.get(current)?.layout_parent.get()?;
        if parent == node_id {
            return Some(current);
        }
        current = parent;
    }
}

/// Visit an out-of-flow box hoisted to its containing block past its layout parent and place
/// it in the paint lists: z-indexed boxes go to the enclosing stacking context like any other
/// z-indexed positioned child, the rest join `paint_children` at the same paint level as other
/// positioned boxes (the caller's stable sort keeps tree order).
fn attach_hoisted_past_parent(
    tree: &mut Tree,
    child_id: BoxId,
    paint_children: &mut Vec<BoxId>,
    entries: &mut Vec<HoistedPaintChild>,
) {
    let is_sc_root = tree[child_id].is_stacking_context_root(false);
    build_paint_tree_impl(
        tree,
        child_id,
        match is_sc_root {
            true => None,
            false => Some(entries),
        },
    );

    let z_index = tree[child_id].z_index();
    if z_index != 0 {
        entries.push(HoistedPaintChild::new(child_id, z_index));
    } else {
        paint_children.push(child_id);
    }
}

#[inline(always)]
fn position_to_order(pos: Position) -> i32 {
    match pos {
        Position::Static => 0,
        // All positioned descendants with z-index: auto share one paint level (CSS 2.1
        // Appendix E step 8); the stable sort keeps them in tree order among themselves,
        // above in-flow content and floats.
        Position::Relative | Position::Sticky | Position::Absolute | Position::Fixed => 2,
    }
}
#[inline(always)]
fn float_to_order(pos: Float) -> i32 {
    match pos {
        Float::None => 0,
        _ => 1,
    }
}

/// Paint sort key: (paint level, order-modified position). Positioned (z-index: auto)
/// descendants paint above in-flow content (CSS 2.1 Appendix E step 8); within a level the
/// stable sort preserves (order-modified) document order.
#[inline(always)]
fn node_to_paint_order(node: &Node, is_flex_or_grid: bool) -> (i32, i32) {
    let Some(style) = node.primary_styles() else {
        return (0, 0);
    };
    let position = style.clone_position();
    if is_flex_or_grid {
        match position {
            Position::Static => (0, style.clone_order()),
            Position::Relative | Position::Sticky => (2, style.clone_order()),
            // Out-of-flow children are not flex/grid items: `order` does not apply; tree
            // order does.
            Position::Absolute | Position::Fixed => (2, 0),
        }
    } else {
        (
            position_to_order(position) + float_to_order(style.clone_float()),
            0,
        )
    }
}
