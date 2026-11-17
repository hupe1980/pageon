//! Adapted from Blitz (blitz-dom `layout/mod.rs`, MIT OR Apache-2.0).
//!
//! The layout pass: the box tree as Taffy's tree (block, flex, grid and table containers),
//! with inline formatting contexts, replaced elements and form controls laid out here.

use std::cell::Ref;
use std::rc::Rc;

use html5ever::{LocalName, local_name};
use parley::{FontContext, LayoutContext};
use style::Atom;
use style::properties::ComputedValues;
use style::values::computed::CSSPixelLength;
use style::values::computed::length_percentage::CalcLengthPercentage;
use stylo_taffy::TaffyStyloStyle;
use taffy::{
    AxisStaticEdge, AxisStaticPosition, BlockContext, CoreStyle as _, DetailedLayoutInfo,
    LayoutContainingBlock, LayoutPartialTree, MaybeMath as _, NodeId, OofCandidate, ResolveOrZero,
    RoundTree, RunMode, TraversePartialTree, TraverseTree, compute_block_layout,
    compute_cached_layout, compute_flexbox_layout, compute_grid_layout, compute_leaf_layout,
    compute_oof_layout, prelude::*,
};

use super::replaced::{
    IntrinsicSizes, ReplacedContext, compute_replaced_layout, is_replaced_element,
};
use super::table::TableTreeWrapper;
use super::tree::{BoxId, Kind, Node, Special, TextBrush, Tree};
use crate::images::Decoded;

/// The default object size of replaced elements.
const DEFAULT_OBJECT_SIZE: taffy::Size<f32> = taffy::Size {
    width: 300.0,
    height: 150.0,
};

/// The intrinsic dimensions and default object size of a replaced element given by its tag:
/// an image without a loaded resource has none and a zero default object size; a canvas has
/// the size and ratio of its `width`/`height` (default 300×150); other replaced elements
/// (video, iframe, embed) have none and the 300×150 default object size.
fn tag_intrinsic_sizes(
    tag: &LocalName,
    attr_size: taffy::Size<Option<f32>>,
) -> (IntrinsicSizes, taffy::Size<f32>) {
    if *tag == local_name!("img") || *tag == local_name!("svg") {
        return (IntrinsicSizes::default(), taffy::Size::ZERO);
    }
    if *tag == local_name!("canvas") {
        let width = attr_size.width.unwrap_or(300.0);
        let height = attr_size.height.unwrap_or(150.0);
        return (
            IntrinsicSizes {
                width: Some(width),
                height: Some(height),
                ratio: Some(width / height),
            },
            DEFAULT_OBJECT_SIZE,
        );
    }
    (IntrinsicSizes::default(), DEFAULT_OBJECT_SIZE)
}

/// Resolves a `calc()` length-percentage Taffy holds as a pointer.
pub(crate) fn resolve_calc_value(calc_ptr: *const (), parent_size: f32) -> f32 {
    // SAFETY: Taffy only hands back pointers it was given in a calc `CompactLength`, and
    // every such pointer is to a `CalcLengthPercentage`: Stylo's (inside a computed style
    // that outlives the layout pass) or one a table context owns for as long as its style.
    #[allow(unsafe_code)]
    let calc = unsafe { &*(calc_ptr as *const CalcLengthPercentage) };
    calc.resolve(CSSPixelLength::new(parent_size)).px()
}

/// The layout pass over a box tree.
pub(crate) struct Pass<'t> {
    pub(crate) tree: &'t mut Tree,
    pub(crate) fonts: &'t mut FontContext,
    pub(crate) layouts: &'t mut LayoutContext<TextBrush>,
}

fn id_of(node_id: NodeId) -> BoxId {
    usize::from(node_id)
}

impl Pass<'_> {
    pub(crate) fn node(&self, node_id: NodeId) -> &Node {
        &self.tree.nodes[id_of(node_id)]
    }

    fn node_mut(&mut self, node_id: NodeId) -> &mut Node {
        &mut self.tree.nodes[id_of(node_id)]
    }

    /// The style of a box laid out by the running algorithm.
    pub(crate) fn child_layout_style<'a>(
        &self,
        node: &'a Node,
    ) -> TaffyStyloStyle<&'a ComputedValues> {
        node.layout_style()
    }

    /// Runs the box's algorithm, then lays out the out-of-flow boxes it is the containing
    /// block of. Called inside the cache wrapper, so a cache hit skips both.
    fn compute_child_layout_internal(
        &mut self,
        node_id: NodeId,
        inputs: taffy::tree::LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> taffy::tree::LayoutOutput {
        let mut output = self.dispatch_child_layout(node_id, inputs, block_ctx);
        if inputs.run_mode == RunMode::PerformLayout {
            compute_oof_layout(self, node_id, &mut output);
        }
        output
    }

    fn dispatch_child_layout(
        &mut self,
        node_id: NodeId,
        inputs: taffy::tree::LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> taffy::tree::LayoutOutput {
        let id = id_of(node_id);
        let node = &self.tree.nodes[id];

        let font_styles = node.primary_styles().map(|style| {
            use style::values::computed::font::LineHeight;
            let font_size = style.clone_font_size().used_size().px();
            let line_height = match style.clone_line_height() {
                LineHeight::Normal => font_size * 1.2,
                LineHeight::Number(num) => font_size * num.0,
                LineHeight::Length(value) => value.0.px(),
            };
            (font_size, line_height)
        });
        let font_size = font_styles.map(|s| s.0);
        let resolved_line_height = font_styles.map(|s| s.1);

        match node.kind {
            // Text is laid out in its inline formatting context, never alone.
            Kind::Text => taffy::LayoutOutput::HIDDEN,
            Kind::Document => compute_block_layout(self, node_id, inputs, None),
            Kind::Element | Kind::Anonymous | Kind::Pseudo => {
                let tag = node.tag.clone();
                if &*tag == "textarea" {
                    let rows = node
                        .attr(&local_name!("rows"))
                        .and_then(|v| v.parse::<f32>().ok())
                        .unwrap_or(2.0);
                    let cols = node
                        .attr(&local_name!("cols"))
                        .and_then(|v| v.parse::<f32>().ok());
                    let output = compute_leaf_layout(
                        inputs,
                        &node.layout_style(),
                        resolve_calc_value,
                        |_known, _available| taffy::Size {
                            width: cols
                                .map(|cols| cols * font_size.unwrap_or(16.0) * 0.6)
                                .unwrap_or(300.0),
                            height: resolved_line_height.unwrap_or(16.0) * rows,
                        },
                    );
                    if inputs.run_mode == RunMode::PerformLayout {
                        let pb = {
                            let style = node.layout_style();
                            style
                                .padding()
                                .resolve_or_zero(inputs.parent_size.width, resolve_calc_value)
                                + style
                                    .border()
                                    .resolve_or_zero(inputs.parent_size.width, resolve_calc_value)
                        };
                        let content_width = (output.size.width - pb.horizontal_axis_sum()).max(0.0);
                        if matches!(node.special, Special::TextInput(_)) {
                            let layout = super::construct::control_layout(
                                self.tree,
                                self.fonts,
                                self.layouts,
                                id,
                                Some(content_width),
                            );
                            if let Special::TextInput(c) = &mut self.tree.nodes[id].special {
                                c.layout = layout;
                            }
                        }
                    }
                    return output;
                }

                if &*tag == "input" {
                    match node.attr(&local_name!("type")) {
                        Some("hidden") => return taffy::LayoutOutput::HIDDEN,
                        Some("checkbox") => {
                            return compute_leaf_layout(
                                inputs,
                                &node.layout_style(),
                                resolve_calc_value,
                                |_known, _available| {
                                    let size = node.layout_style().size();
                                    let width = size.width.resolve_or_zero(
                                        inputs.parent_size.width,
                                        resolve_calc_value,
                                    );
                                    let height = size.height.resolve_or_zero(
                                        inputs.parent_size.height,
                                        resolve_calc_value,
                                    );
                                    let min = width.min(height);
                                    taffy::Size {
                                        width: min,
                                        height: min,
                                    }
                                },
                            );
                        }
                        None | Some("text" | "password" | "email" | "tel" | "url" | "search") => {
                            return compute_leaf_layout(
                                inputs,
                                &node.layout_style(),
                                resolve_calc_value,
                                |_known, _available| taffy::Size {
                                    width: match inputs.available_space.width {
                                        AvailableSpace::Definite(limit) => limit.min(300.0),
                                        AvailableSpace::MinContent => 0.0,
                                        AvailableSpace::MaxContent => 300.0,
                                    },
                                    height: resolved_line_height.unwrap_or(16.0),
                                },
                            );
                        }
                        _ => {}
                    }
                }

                if is_replaced_element(&tag) {
                    // `width`/`height` attributes are already in the style (presentational
                    // hints); they are read here only where they give the intrinsic size.
                    let attr_size = taffy::Size {
                        width: node
                            .attr(&local_name!("width"))
                            .and_then(|v| v.parse::<f32>().ok()),
                        height: node
                            .attr(&local_name!("height"))
                            .and_then(|v| v.parse::<f32>().ok()),
                    };
                    let (intrinsic_sizes, default_object_size) = match &node.special {
                        Special::Image(Some(image)) => match &**image {
                            Decoded::Raster { width, height, .. } => {
                                let (width, height) = (*width as f32, *height as f32);
                                (
                                    IntrinsicSizes {
                                        width: Some(width),
                                        height: Some(height),
                                        ratio: Some(width / height),
                                    },
                                    DEFAULT_OBJECT_SIZE,
                                )
                            }
                            Decoded::Failed(_) => (IntrinsicSizes::default(), taffy::Size::ZERO),
                        },
                        Special::Image(None) => (IntrinsicSizes::default(), taffy::Size::ZERO),
                        _ => tag_intrinsic_sizes(&tag, attr_size),
                    };
                    let context = ReplacedContext {
                        intrinsic_sizes,
                        default_object_size,
                    };
                    return compute_replaced_layout(
                        inputs,
                        &node.layout_style(),
                        resolve_calc_value,
                        &context,
                    );
                }

                if node.flags.table_root {
                    // The space for an absolutely positioned table never exceeds its
                    // containing block less its margins.
                    let mut inputs = inputs;
                    if node.is_out_of_flow() {
                        let margin = node
                            .layout_style()
                            .margin()
                            .resolve_or_zero(inputs.parent_size.width, resolve_calc_value);
                        let max_available = inputs
                            .parent_size
                            .maybe_sub(margin.sum_axes())
                            .maybe_max(taffy::Size::ZERO);
                        inputs.available_space = inputs.available_space.maybe_min(max_available);
                    }
                    let Special::Table(context) = &node.special else {
                        panic!("a table root without its table");
                    };
                    let context = Rc::clone(context);
                    let mut wrapper = TableTreeWrapper {
                        pass: self,
                        ctx: context,
                    };
                    let mut output = compute_grid_layout(&mut wrapper, node_id, inputs);

                    // Absolutely positioned children take no part in the grid: their static
                    // position is the top left of the table's content box.
                    let context = wrapper.ctx;
                    if inputs.run_mode == RunMode::PerformLayout && !context.oof_children.is_empty()
                    {
                        let style = &context.style;
                        let parent_width = inputs.parent_size.width;
                        let padding = style
                            .padding
                            .resolve_or_zero(parent_width, resolve_calc_value);
                        let border = style
                            .border
                            .resolve_or_zero(parent_width, resolve_calc_value);
                        let static_position = taffy::Point {
                            x: padding.left + border.left,
                            y: padding.top + border.top,
                        }
                        .map(|pos| AxisStaticPosition::from_edge(pos, AxisStaticEdge::Start));
                        let in_flow = context.cells.len();
                        for (i, (child, position)) in context.oof_children.iter().enumerate() {
                            output.oof_candidates.push(OofCandidate {
                                node: NodeId::from(*child),
                                order: (in_flow + i) as u32,
                                position: *position,
                                static_position,
                            });
                        }
                    }

                    // Scrollable overflow is capped at the table's size (tables do not scroll).
                    output.scrollable_overflow_rect.left = 0.0;
                    output.scrollable_overflow_rect.top = 0.0;
                    output.scrollable_overflow_rect.right =
                        output.scrollable_overflow_rect.right.min(output.size.width);
                    output.scrollable_overflow_rect.bottom = output
                        .scrollable_overflow_rect
                        .bottom
                        .min(output.size.height);
                    return output;
                }

                if node.flags.inline_root {
                    return self.compute_inline_layout(id, inputs, block_ctx);
                }

                match taffy_display(node) {
                    Display::Block => compute_block_layout(self, node_id, inputs, block_ctx),
                    Display::FlowRoot => compute_block_layout(self, node_id, inputs, None),
                    Display::Flex => compute_flexbox_layout(self, node_id, inputs),
                    Display::Grid => compute_grid_layout(self, node_id, inputs),
                    Display::None => taffy::LayoutOutput::HIDDEN,
                }
            }
        }
    }
}

/// The box's `display` for Taffy (block for boxes without style; button inputs are flow
/// roots).
pub(crate) fn taffy_display(node: &Node) -> Display {
    node.primary_styles()
        .map(|style| {
            let display = style.clone_display();
            if node.is_button_input()
                && !matches!(
                    display.inside(),
                    style::values::specified::box_::DisplayInside::None
                        | style::values::specified::box_::DisplayInside::Contents
                )
            {
                Display::FlowRoot
            } else {
                stylo_taffy::convert::display(display)
            }
        })
        .unwrap_or(Display::Block)
}

impl TraversePartialTree for Pass<'_> {
    type ChildIter<'a>
        = RefCellChildIter<'a>
    where
        Self: 'a;

    fn child_ids(&self, node_id: NodeId) -> Self::ChildIter<'_> {
        let children = self.node(node_id).layout_children.borrow();
        RefCellChildIter::new(Ref::map(children, |c| c.as_deref().unwrap_or(&[])))
    }

    fn child_count(&self, node_id: NodeId) -> usize {
        self.node(node_id)
            .layout_children
            .borrow()
            .as_ref()
            .map_or(0, Vec::len)
    }

    fn get_child_id(&self, node_id: NodeId, index: usize) -> NodeId {
        let children = self.node(node_id).layout_children.borrow();
        NodeId::from(children.as_ref().map_or(0, |c| c[index]))
    }
}

impl TraverseTree for Pass<'_> {}

impl LayoutPartialTree for Pass<'_> {
    type CoreContainerStyle<'a>
        = TaffyStyloStyle<&'a ComputedValues>
    where
        Self: 'a;

    type CustomIdent = Atom;

    fn get_core_container_style(&self, node_id: NodeId) -> Self::CoreContainerStyle<'_> {
        self.node(node_id).layout_style()
    }

    fn set_unrounded_layout(&mut self, node_id: NodeId, layout: &Layout) {
        self.node_mut(node_id).layout.unrounded = *layout;
    }

    fn resolve_calc_value(&self, calc_ptr: *const (), parent_size: f32) -> f32 {
        resolve_calc_value(calc_ptr, parent_size)
    }

    #[inline(always)]
    fn compute_child_layout(
        &mut self,
        node_id: NodeId,
        inputs: taffy::LayoutInput,
    ) -> taffy::LayoutOutput {
        compute_cached_layout(self, node_id, inputs, |tree, node_id, inputs| {
            tree.compute_child_layout_internal(node_id, inputs, None)
        })
    }
}

impl LayoutContainingBlock for Pass<'_> {
    type OofItemStyle<'a>
        = TaffyStyloStyle<&'a ComputedValues>
    where
        Self: 'a;

    fn get_oof_item_style(&self, node_id: NodeId) -> Self::OofItemStyle<'_> {
        self.child_layout_style(self.node(node_id))
    }

    fn clear_hoisted_children(&mut self, node_id: NodeId) {
        let containing_block = id_of(node_id);
        let mut hoisted = self.node(node_id).hoisted_children.borrow_mut();
        for &h in hoisted.iter() {
            // A box claimed since by another containing block keeps pointing at that one.
            if let Some(n) = self.tree.nodes.get(h)
                && n.oof_containing_block.get() == Some(containing_block)
            {
                n.oof_containing_block.set(None);
            }
        }
        hoisted.clear();
    }

    fn add_hoisted_children(&mut self, node_id: NodeId, hoisted: &[NodeId]) {
        let containing_block = id_of(node_id);
        self.node(node_id)
            .hoisted_children
            .borrow_mut()
            .extend(hoisted.iter().copied().map(id_of));
        for &h in hoisted {
            self.node(h)
                .oof_containing_block
                .set(Some(containing_block));
        }
    }

    fn get_detailed_layout_info(&self, node_id: NodeId) -> &DetailedLayoutInfo<Atom> {
        &self.node(node_id).detailed_layout_info
    }
}

impl taffy::CacheTree for Pass<'_> {
    #[inline]
    fn cache_get(
        &mut self,
        node_id: NodeId,
        inputs: &taffy::LayoutInput,
    ) -> Option<taffy::LayoutOutput> {
        self.node_mut(node_id).layout.cache.get(inputs)
    }

    #[inline]
    fn cache_store(
        &mut self,
        node_id: NodeId,
        inputs: &taffy::LayoutInput,
        output: taffy::LayoutOutput,
    ) {
        self.node_mut(node_id).layout.cache.store(inputs, output);
    }

    #[inline]
    fn cache_clear(&mut self, node_id: NodeId) {
        self.node_mut(node_id).layout.cache.clear();
    }
}

impl taffy::LayoutBlockContainer for Pass<'_> {
    type BlockContainerStyle<'a>
        = TaffyStyloStyle<&'a ComputedValues>
    where
        Self: 'a;

    type BlockItemStyle<'a>
        = TaffyStyloStyle<&'a ComputedValues>
    where
        Self: 'a;

    fn get_block_container_style(&self, node_id: NodeId) -> Self::BlockContainerStyle<'_> {
        self.get_core_container_style(node_id)
    }

    fn get_block_child_style(&self, child_node_id: NodeId) -> Self::BlockItemStyle<'_> {
        self.child_layout_style(self.node(child_node_id))
    }

    #[inline(always)]
    fn compute_block_child_layout(
        &mut self,
        node_id: NodeId,
        inputs: taffy::LayoutInput,
        block_ctx: Option<&mut BlockContext<'_>>,
    ) -> taffy::LayoutOutput {
        compute_cached_layout(self, node_id, inputs, |tree, node_id, inputs| {
            tree.compute_child_layout_internal(node_id, inputs, block_ctx)
        })
    }
}

impl taffy::LayoutFlexboxContainer for Pass<'_> {
    type FlexboxContainerStyle<'a>
        = TaffyStyloStyle<&'a ComputedValues>
    where
        Self: 'a;

    type FlexboxItemStyle<'a>
        = TaffyStyloStyle<&'a ComputedValues>
    where
        Self: 'a;

    fn get_flexbox_container_style(&self, node_id: NodeId) -> Self::FlexboxContainerStyle<'_> {
        self.get_core_container_style(node_id)
    }

    fn get_flexbox_child_style(&self, child_node_id: NodeId) -> Self::FlexboxItemStyle<'_> {
        self.child_layout_style(self.node(child_node_id))
    }
}

impl taffy::LayoutGridContainer for Pass<'_> {
    type GridContainerStyle<'a>
        = TaffyStyloStyle<&'a ComputedValues>
    where
        Self: 'a;

    type GridItemStyle<'a>
        = TaffyStyloStyle<&'a ComputedValues>
    where
        Self: 'a;

    fn get_grid_container_style(&self, node_id: NodeId) -> Self::GridContainerStyle<'_> {
        self.get_core_container_style(node_id)
    }

    fn get_grid_child_style(&self, child_node_id: NodeId) -> Self::GridItemStyle<'_> {
        self.child_layout_style(self.node(child_node_id))
    }

    fn set_detailed_grid_info(&mut self, node_id: NodeId, info: taffy::DetailedGridInfo<Atom>) {
        self.node_mut(node_id).detailed_layout_info = DetailedLayoutInfo::Grid(Box::new(info));
    }
}

impl RoundTree for Pass<'_> {
    fn get_unrounded_layout(&self, node_id: NodeId) -> Layout {
        self.node(node_id).layout.unrounded
    }

    fn set_final_layout(&mut self, node_id: NodeId, layout: &Layout) {
        self.node_mut(node_id).layout.layout = *layout;
    }

    fn is_out_of_flow(&self, node_id: NodeId) -> bool {
        self.node(node_id).is_out_of_flow()
    }

    fn hoisted_child_count(&self, node_id: NodeId) -> usize {
        self.node(node_id).hoisted_children.borrow().len()
    }

    fn get_hoisted_child_id(&self, node_id: NodeId, index: usize) -> NodeId {
        NodeId::from(self.node(node_id).hoisted_children.borrow()[index])
    }
}

/// Iterates a box's layout children while they are borrowed.
pub(crate) struct RefCellChildIter<'a> {
    items: Ref<'a, [BoxId]>,
    idx: usize,
}

impl<'a> RefCellChildIter<'a> {
    fn new(items: Ref<'a, [BoxId]>) -> Self {
        Self { items, idx: 0 }
    }
}

impl Iterator for RefCellChildIter<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<Self::Item> {
        let id = *self.items.get(self.idx)?;
        self.idx += 1;
        Some(NodeId::from(id))
    }
}
