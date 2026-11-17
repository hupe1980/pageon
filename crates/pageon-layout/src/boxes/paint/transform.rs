//! Adapted from Blitz (blitz-dom `src/resolve.rs` `BaseDocument::resolve_transforms`,
//! `src/node/node.rs` `Node::set_transform` and `src/stylo_to_kurbo.rs`, MIT OR Apache-2.0).
//!
//! CSS transforms and scrollable overflow, resolved after layout.

use euclid::default::{Point2D, Rect, Size2D};
use kurbo::{Affine, Vec2};
use style::{
    properties::generated::style_structs::Box as BoxStyleStruct,
    values::{
        computed::{CSSPixelLength, Rotate},
        generics::transform::{Scale, Translate},
    },
};

use super::super::tree::{BoxId, Node, Tree};

/// Resolves the transform and scrollable overflow of `node_id` and its descendants, and
/// returns the box's overflow in its parent's coordinates (device px).
pub(crate) fn resolve_transforms(tree: &mut Tree, node_id: BoxId, scale: f64) -> kurbo::Rect {
    if node_id >= tree.nodes.len() {
        return kurbo::Rect::ZERO;
    }

    let transform = set_transform(&mut tree[node_id], scale as f32);

    let w = tree[node_id].final_layout().size.width as f64 * scale;
    let h = tree[node_id].final_layout().size.height as f64 * scale;
    let mut overflow = kurbo::Rect::new(0.0, 0.0, w, h);

    let layout_children = std::mem::take(tree[node_id].layout_children.get_mut());

    if let Some(ref children) = layout_children {
        for &child_id in children {
            // Out-of-flow children are laid out relative to their containing block, not their
            // parent: they are visited (and their overflow accounted for) via the containing
            // block's hoisted list below.
            if tree[child_id].is_hoisted_out_of_flow() {
                continue;
            }
            let child_rect_in_self = resolve_transforms(tree, child_id, scale);
            overflow = overflow.union(child_rect_in_self);
        }
    }
    let hoisted_children = std::mem::take(&mut *tree[node_id].hoisted_children.borrow_mut());
    for &child_id in &hoisted_children {
        if child_id >= tree.nodes.len() {
            continue;
        }
        let child_rect_in_self = resolve_transforms(tree, child_id, scale);
        overflow = overflow.union(child_rect_in_self);
    }
    *tree[node_id].hoisted_children.borrow_mut() = hoisted_children;
    for pseudo in [tree[node_id].before, tree[node_id].after] {
        let Some(pseudo) = pseudo else { continue };
        if tree[pseudo].is_hoisted_out_of_flow() {
            continue;
        }
        let child_rect_in_self = resolve_transforms(tree, pseudo, scale);
        overflow = overflow.union(child_rect_in_self);
    }

    tree[node_id].scrollable_overflow = overflow;
    *tree[node_id].layout_children.get_mut() = layout_children;

    let scaled_x = tree[node_id].final_layout().location.x as f64 * scale;
    let scaled_y = tree[node_id].final_layout().location.y as f64 * scale;

    let full = if let Some(t) = transform {
        Affine::translate((scaled_x, scaled_y)) * t
    } else {
        Affine::translate((scaled_x, scaled_y))
    };

    full.transform_rect_bbox(overflow)
}

/// Resolves the box's transform (in device px) into `Node::transform`.
fn set_transform(node: &mut Node, scale: f32) -> Option<Affine> {
    let transform = node.style.as_ref().and_then(|s| {
        let size = node.final_layout().size;
        let reference_box = Rect::new(
            Point2D::new(CSSPixelLength::new(0.0), CSSPixelLength::new(0.0)),
            Size2D::new(
                CSSPixelLength::new(size.width),
                CSSPixelLength::new(size.height),
            ),
        );
        // Resolve the transform in CSS pixels, then convert it to device-pixel space
        // (S * T * S^-1): translation components are scaled, linear components are not.
        resolve_2d_transform(s.get_box(), reference_box).map(|t| {
            let scale = scale as f64;
            let [m11, m12, m21, m22, m41, m42] = t.as_coeffs();
            Affine::new([m11, m12, m21, m22, m41 * scale, m42 * scale])
        })
    });
    node.transform = transform;
    transform
}

// 6. Current Transformation Matrix
//
// The transformation matrix is computed from the transform, transform-origin, translate,
// rotate, scale, and offset properties as follows:
//
//   - Start with the identity matrix.
//   - Translate by the computed X, Y, and Z values of transform-origin.
//   - Translate by the computed X, Y, and Z values of translate.
//   - Rotate by the computed <angle> about the specified axis of rotate.
//   - Scale by the computed X, Y, and Z values of scale.
//   - Translate and rotate by the transform specified by offset.
//   - Multiply by each of the transform functions in transform from left to right.
//   - Translate by the negated computed X, Y and Z values of transform-origin.
//
// <https://drafts.csswg.org/css-transforms-2/#ctm>
pub(crate) fn resolve_2d_transform(
    box_styles: &BoxStyleStruct,
    reference_box: Rect<CSSPixelLength>,
) -> Option<Affine> {
    let translate = match &box_styles.translate {
        Translate::None => None,
        Translate::Translate(x, y, _z) => Some(Vec2 {
            x: x.resolve(reference_box.width()).px() as f64,
            y: y.resolve(reference_box.height()).px() as f64,
        }),
    };

    let rotate = match &box_styles.rotate {
        Rotate::None => None,
        Rotate::Rotate(angle) => Some(angle.radians64()),
        // A rotation about the z axis stays in the plane; the sign of the axis component
        // decides the direction, its magnitude cancels out.
        Rotate::Rotate3D(x, y, z, angle) if *x == 0.0 && *y == 0.0 && *z != 0.0 => {
            Some(angle.radians64() * z.signum() as f64)
        }
        // TODO: support 3D transforms
        Rotate::Rotate3D(_, _, _, _) => None,
    };

    let scale_transform = match &box_styles.scale {
        Scale::None => None,
        Scale::Scale(x, y, _z) => Some(Vec2 {
            x: *x as f64,
            y: *y as f64,
        }),
    };

    let transform = if box_styles.transform.0.is_empty() {
        None
    } else {
        box_styles
            .transform
            .to_transform_3d_matrix(Some(&reference_box))
            .ok()
            // The `has_3d` flag means a 3d function appeared in the list, not that the matrix
            // needs 3d: `translate3d(x, y, 0)` sets it while resolving to a plain 2d
            // translation.
            //
            // TODO: support 3D transforms. Outside a 3d rendering context the spec still
            // applies the transform as a painting effect -- its own example paints
            // `rotateY(50deg)` narrower -- so dropping one is wrong. `is_2d()` is closer than
            // the flag, not the rule.
            // See: https://drafts.csswg.org/css-transforms-2/#3d-transform-rendering
            .filter(|(t, _has_3d)| t.is_2d())
            .map(|(t, _)| {
                // See: https://drafts.csswg.org/css-transforms-2/#two-dimensional-subset
                // And https://docs.rs/kurbo/latest/kurbo/struct.Affine.html#method.new
                Affine::new([t.m11, t.m12, t.m21, t.m22, t.m41, t.m42].map(|v| v as f64))
            })
    };

    // TODO: support the "offset" property
    // <https://developer.mozilla.org/en-US/docs/Web/CSS/Reference/Properties/offset>

    if translate.is_none() && rotate.is_none() && scale_transform.is_none() && transform.is_none() {
        return None;
    }

    // Apply the transform origin by:
    //   - Translating by the origin offset
    //   - Applying our transform
    //   - Translating by the inverse of the origin offset
    let transform_origin = &box_styles.transform_origin;
    let origin_translation = Affine::translate(Vec2 {
        x: transform_origin
            .horizontal
            .resolve(reference_box.width())
            .px() as f64,
        y: transform_origin
            .vertical
            .resolve(reference_box.height())
            .px() as f64,
    });

    let mut resolved = Affine::IDENTITY;

    if let Some(translation) = translate {
        resolved *= Affine::translate(translation)
    }

    if let Some(rotation) = rotate {
        resolved *= Affine::rotate(rotation)
    }

    if let Some(scale_transform) = scale_transform {
        resolved *= Affine::scale_non_uniform(scale_transform.x, scale_transform.y)
    }

    if let Some(transform) = transform {
        resolved *= transform;
    }

    resolved = origin_translation * resolved * origin_translation.inverse();

    if resolved != Affine::IDENTITY {
        Some(resolved)
    } else {
        None
    }
}
