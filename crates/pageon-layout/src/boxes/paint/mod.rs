//! Paint on the box tree: after layout, [`prepare`] resolves CSS images, transforms,
//! scrollable overflow, paint children and stacking contexts; [`paint_scene`] then draws the
//! tree into an [`anyrender::PaintScene`] and [`hit`] finds the box at a point. Ported from
//! Blitz's painter (blitz-paint) and paint tree (blitz-dom), so the same boxes paint the same
//! pixels.

#![allow(clippy::collapsible_if)]

mod color;
mod filters;
mod gradient;
pub(crate) mod hit;
mod kurbo_css;
mod layers;
pub(crate) mod paint_tree;
mod render;
mod sizing;
mod text;
pub(crate) mod transform;

#[cfg(test)]
mod tests;

use style::properties::ComputedValues;
use style::url::ComputedUrl;
use style::values::computed::Display;
use style::values::generics::image::Image as StyloImage;

pub(crate) use paint_tree::StackingContext;
use render::Painter;

use super::tree::{BoxId, Kind, Node, Special, Tree};

/// Synthetic emboldening of glyphs (a Blitz build option pageon leaves off).
const FONT_EMBOLDEN_ENABLED: bool = false;

/// The device pixels per CSS px that transforms and scrollable overflow are resolved in:
/// pageon lays out and paints at 1 and scales the recorded scene.
pub(crate) const LAYOUT_SCALE: f64 = 1.0;

/// Builds paint children, stacking contexts and transforms: once per tree, before the
/// first paint or hit test.
pub(crate) fn prepare(tree: &Tree) {
    let Some(root) = root_element(tree) else {
        return;
    };
    transform::resolve_transforms(tree, root, LAYOUT_SCALE);
    paint_tree::build_paint_tree(tree, root);
}

/// Paints the tree into `scene` (same meaning of arguments as `blitz_paint::paint_scene`).
pub(crate) fn paint_scene(
    scene: &mut impl anyrender::PaintScene,
    tree: &Tree,
    scale: f64,
    width: u32,
    height: u32,
    x_offset: u32,
    y_offset: u32,
) {
    let generator = Painter::new(tree, scale, width, height, x_offset as f64, y_offset as f64);
    generator.paint_scene(scene);
}

/// The root element: the document's first element child.
pub(crate) fn root_element(tree: &Tree) -> Option<BoxId> {
    tree.nodes
        .first()?
        .children
        .iter()
        .copied()
        .find(|&c| tree[c].kind == Kind::Element)
}

/// The viewport's scroll offset (CSS px). The root element scrolls the viewport, so its offset
/// is kept on the document box rather than on the root element.
pub(crate) fn viewport_scroll(tree: &Tree) -> kurbo::Vec2 {
    tree.nodes
        .first()
        .map(|d| d.layout.scroll)
        .unwrap_or_default()
}

/// Fills each box's `background_images` and `mask_images` with the decoded images of its
/// `background-image` and `mask-image` `url()` layers (`None` for other layers and for images
/// not known yet).
pub(crate) fn resolve_images(tree: &mut Tree) {
    for id in 0..tree.nodes.len() {
        let Some(style) = tree[id].style.clone() else {
            continue;
        };
        let background = layer_images(tree, &style.get_background().background_image.0);
        let mask = layer_images(tree, &style.get_svg().mask_image.0);
        tree[id].background_images = background;
        tree[id].mask_images = mask;
    }
}

fn layer_images(
    tree: &mut Tree,
    layers: &[style::values::computed::Image],
) -> Vec<Option<style::servo_arc::Arc<crate::images::Decoded>>> {
    // Most boxes have no image layer: no allocation for them.
    if !layers.iter().any(|l| matches!(l, StyloImage::Url(_))) {
        return Vec::new();
    }
    layers
        .iter()
        .map(|layer| match layer {
            StyloImage::Url(ComputedUrl::Valid(url)) => tree.image(url.as_str()),
            _ => None,
        })
        .collect()
}

// Node helpers the painter uses (blitz-dom `node/node.rs`, `node/element.rs`).
impl Node {
    /// `display` as layout uses it (a button input's display is blockified).
    pub(crate) fn used_display(&self) -> Option<Display> {
        let display = self.style.as_ref()?.clone_display();
        Some(if self.is_button_input() {
            stylo_taffy::convert::button_input_display(display)
        } else {
            display
        })
    }

    /// The box's `display` as a [`taffy::Display`] (block for boxes without style).
    pub(crate) fn taffy_display(&self) -> taffy::Display {
        self.style
            .as_ref()
            .map(|style| {
                let display = style.clone_display();
                if self.is_button_input()
                    && !matches!(
                        display.inside(),
                        style::values::specified::box_::DisplayInside::None
                            | style::values::specified::box_::DisplayInside::Contents
                    )
                {
                    taffy::Display::FlowRoot
                } else {
                    stylo_taffy::convert::display(display)
                }
            })
            .unwrap_or(taffy::Display::Block)
    }

    /// The box's `position` as a [`taffy::Position`] (static for boxes without style).
    pub(crate) fn taffy_position(&self) -> taffy::Position {
        self.style
            .as_ref()
            .map(|s| stylo_taffy::convert::position(s.get_box().position))
            .unwrap_or(taffy::Position::Static)
    }

    /// Out of flow as Taffy places it (absolutely or fixed positioned, and displayed).
    pub(crate) fn is_hoisted_out_of_flow(&self) -> bool {
        self.style.as_ref().is_some_and(|s| {
            let box_style = s.get_box();
            stylo_taffy::convert::position(box_style.position).is_out_of_flow()
                && stylo_taffy::convert::display(box_style.display) != taffy::Display::None
        })
    }

    pub(crate) fn z_index(&self) -> i32 {
        self.style
            .as_ref()
            .map(|s| s.clone_z_index().integer_or(0))
            .unwrap_or(0)
    }

    /// Whether the box establishes a stacking context.
    pub(crate) fn is_stacking_context_root(&self, is_flex_or_grid_item: bool) -> bool {
        use style::computed_values::isolation::T as Isolation;
        use style::computed_values::position::T as Position;
        use style::values::computed::Rotate;
        use style::values::generics::transform::{Scale, Translate};

        let Some(style) = self.style.as_ref() else {
            return false;
        };

        let position = style.clone_position();
        let has_z_index = !style.clone_z_index().is_auto();

        if style.clone_opacity() != 1.0 {
            return true;
        }

        let position_based = match position {
            Position::Fixed | Position::Sticky => true,
            Position::Relative | Position::Absolute => has_z_index,
            Position::Static => has_z_index && is_flex_or_grid_item,
        };
        if position_based {
            return true;
        }

        let box_style = style.get_box();
        if !box_style.transform.0.is_empty()
            || !matches!(box_style.rotate, Rotate::None)
            || !matches!(box_style.scale, Scale::None)
            || !matches!(box_style.translate, Translate::None)
        {
            return true;
        }

        if self.applies_atomic_paint_effect() {
            return true;
        }

        if style.get_box().isolation == Isolation::Isolate {
            return true;
        }

        // TODO: mix-blend-mode
        // TODO: contain

        false
    }

    /// Whether the box's style applies an atomic paint effect (opacity, filter, clip-path,
    /// mask) to its subtree.
    pub(crate) fn applies_atomic_paint_effect(&self) -> bool {
        use style::values::computed::basic_shape::ClipPath;
        use style::values::generics::image::GenericImage;

        let Some(style) = self.style.as_ref() else {
            return false;
        };

        if style.clone_opacity() != 1.0 {
            return true;
        }

        let effects = style.get_effects();
        if !effects.filter.0.is_empty() {
            return true;
        }

        if !matches!(style.clone_clip_path(), ClipPath::None) {
            return true;
        }

        style
            .get_svg()
            .mask_image
            .0
            .iter()
            .any(|image| !matches!(image, GenericImage::None))
    }

    /// The box this box's `Layout.location` is relative to: the `oof_containing_block` for a
    /// hoisted out-of-flow box, otherwise the `layout_parent`.
    pub(crate) fn containing_block(&self) -> Option<BoxId> {
        self.oof_containing_block
            .get()
            .or_else(|| self.layout_parent.get())
    }

    /// The vertical offset (CSS px) that centers a single-line text control's text in its
    /// content box.
    pub(crate) fn text_input_v_centering_offset(&self, scale: f64) -> f64 {
        if let Special::TextInput(input_data) = &self.special {
            if !input_data.multiline {
                let content_box_height = self.final_layout().content_box_height();
                let input_height = input_data.layout.height() / scale as f32;
                let y_offset = ((content_box_height - input_height) / 2.0).max(0.0);

                return y_offset as f64;
            }
        }

        0.0
    }

    /// The decoded image of an `<img>` (or `<input type=image>`), when known.
    pub(crate) fn raster_image_data(&self) -> Option<&crate::images::Decoded> {
        match &self.special {
            Special::Image(Some(image)) => match &**image {
                d @ crate::images::Decoded::Raster { .. } => Some(d),
                crate::images::Decoded::Failed(_) => None,
            },
            _ => None,
        }
    }

    /// A checkbox's or radio button's checked state.
    pub(crate) fn checkbox_input_checked(&self) -> Option<bool> {
        match self.special {
            Special::Checkbox(checked) => Some(checked),
            _ => None,
        }
    }

    pub(crate) fn primary_styles_or_initial(&self) -> style::servo_arc::Arc<ComputedValues> {
        self.style.clone().unwrap_or_else(|| {
            ComputedValues::initial_values_with_font_override(
                style::properties::style_structs::Font::initial_values(),
            )
            .to_arc()
        })
    }
}
