//! Resolved values (`getComputedStyle`): the computed value, or for layout-dependent
//! properties (sizes, margins, padding, insets, origins, grid tracks, transforms) the used
//! value from the last layout. Adapted from Blitz (blitz-dom `resolved_style.rs`, MIT OR
//! Apache-2.0).

use html5ever::local_name;
use style::computed_values::box_sizing::T as BoxSizing;
use style::computed_values::position::T as Position;
use style::properties::{
    ComputedValues, PropertyDeclaration, PropertyDeclarationId, PropertyId, ShorthandId,
};
use style::values::computed::length::CSSPixelLength;
use style::values::computed::{GridTemplateAreas, GridTemplateComponent, LengthPercentage};
use style::values::generics::position::{Inset as GenericInset, PreferredRatio};
use style::values::resolved;
use style::values::specified::box_::{DisplayInside, DisplayOutside};
use style_traits::{CssStringWriter, ToCss};

use super::replaced::is_replaced_element;
use super::tree::{BoxId, Tree};

/// A used length (CSS px) serialized as Stylo serializes computed lengths.
fn format_px(px: f32) -> String {
    CSSPixelLength::new(px).to_css_string()
}

/// A computed inset in CSS px against `basis`; `None` for `auto`.
fn resolve_inset<P>(val: &GenericInset<P, LengthPercentage>, basis: f32) -> Option<f32> {
    match val {
        GenericInset::LengthPercentage(lp) => Some(lp.resolve(CSSPixelLength::new(basis)).px()),
        _ => None,
    }
}

/// A transform matrix component rounded to 6 decimals (as browsers serialize them).
fn format_matrix_component(v: f64) -> String {
    let rounded = (v * 1e6).round() / 1e6;
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    format!("{rounded}")
}

/// A used transform matrix as `getComputedStyle` serializes it.
fn format_matrix(m: &euclid::default::Transform3D<f64>, is_3d: bool) -> String {
    let c = format_matrix_component;
    if is_3d {
        let v = [
            m.m11, m.m12, m.m13, m.m14, m.m21, m.m22, m.m23, m.m24, m.m31, m.m32, m.m33, m.m34,
            m.m41, m.m42, m.m43, m.m44,
        ];
        format!(
            "matrix3d({})",
            v.iter().map(|&x| c(x)).collect::<Vec<_>>().join(", ")
        )
    } else {
        format!(
            "matrix({}, {}, {}, {}, {}, {})",
            c(m.m11),
            c(m.m12),
            c(m.m21),
            c(m.m22),
            c(m.m41),
            c(m.m42)
        )
    }
}

/// The containing block of a box: its out-of-flow containing block, else its box parent.
fn containing_block(tree: &Tree, id: BoxId) -> Option<BoxId> {
    let n = &tree[id];
    n.oof_containing_block
        .get()
        .or_else(|| n.layout_parent.get())
}

/// Whether the box is a flex or grid item: its nearest ancestor that generates a box (past
/// `display: contents`) is a flex or grid container.
fn is_flex_or_grid_item(tree: &Tree, id: BoxId) -> bool {
    let mut parent = tree[id].parent;
    while let Some(p) = parent {
        let Some(style) = tree[p].primary_styles() else {
            return false;
        };
        let display = style.clone_display();
        if display.is_contents() {
            parent = tree[p].parent;
            continue;
        }
        return matches!(display.inside(), DisplayInside::Flex | DisplayInside::Grid);
    }
    false
}

/// The resolved value of `property` for a box with a style; `None` for unknown properties.
pub(crate) fn value(tree: &Tree, id: BoxId, property: &str) -> Option<String> {
    let node = &tree[id];
    let styles: &ComputedValues = node.primary_styles()?;
    let display = styles.clone_display();
    // Non-atomic inline elements are style spans in their inline root's layout, not boxes:
    // their layout-dependent properties resolve to computed values.
    let is_non_atomic_inline = display.outside() == DisplayOutside::Inline
        && display.inside() == DisplayInside::Flow
        && !node.flags.inline_root
        && !(is_replaced_element(&node.tag)
            || node.tag == local_name!("input")
            || node.tag == local_name!("textarea")
            || node.tag == local_name!("button"));
    let generates_box = !display.is_none();
    let has_layout_box = generates_box && !is_non_atomic_inline;

    let property_id = PropertyId::parse_enabled_for_all_content(property).ok()?;
    // Logical longhands resolve as the physical longhand they map to.
    let property = match property_id.as_shorthand() {
        Err(PropertyDeclarationId::Longhand(longhand)) => {
            longhand.to_physical(styles.writing_mode).name()
        }
        _ => property,
    };
    // Used lengths are serialized unzoomed, like computed lengths.
    let zoom = styles.effective_zoom.value();
    let used = |px: f32| format_px(px / zoom);
    let layout = node.final_layout();
    let unrounded = node.unrounded_layout();
    let grid = match &node.detailed_layout_info {
        taffy::DetailedLayoutInfo::Grid(info) => Some(info),
        _ => None,
    };

    match property {
        "margin" | "margin-block" | "margin-inline" | "padding" | "padding-block"
        | "padding-inline" | "inset" | "inset-block" | "inset-inline"
            if has_layout_box =>
        {
            if let Ok(shorthand) = property_id.as_shorthand() {
                let sides: Vec<String> = shorthand
                    .longhands()
                    .map(|l| value(tree, id, l.name()).unwrap_or_default())
                    .collect();
                return Some(serialize_sides(&sides));
            }
        }
        "grid-template-columns" | "grid-template-rows"
            if display.inside() == DisplayInside::Grid =>
        {
            if let Some(info) = grid {
                return Some(if property == "grid-template-columns" {
                    info.grid_template_columns(zoom)
                } else {
                    info.grid_template_rows(zoom)
                });
            }
        }
        // `grid-template` from the computed track lists, a `none` list on a grid container
        // replaced by the used (implicit) tracks.
        "grid-template" if display.inside() == DisplayInside::Grid => {
            let pos = styles.get_position();
            let rows_none = matches!(pos.grid_template_rows, GridTemplateComponent::None);
            let cols_none = matches!(pos.grid_template_columns, GridTemplateComponent::None);
            if let Some(info) = grid
                && (rows_none || cols_none)
                && matches!(pos.grid_template_areas, GridTemplateAreas::None)
            {
                let rows = if rows_none {
                    info.grid_template_rows(zoom)
                } else {
                    pos.grid_template_rows.to_css_string()
                };
                let columns = if cols_none {
                    info.grid_template_columns(zoom)
                } else {
                    pos.grid_template_columns.to_css_string()
                };
                return Some(format!("{rows} / {columns}"));
            }
        }
        "width" | "height" if has_layout_box => {
            // The layout size per `box-sizing`.
            let border_box = styles.get_position().box_sizing == BoxSizing::BorderBox;
            let size = if property == "width" {
                if border_box {
                    layout.size.width
                } else {
                    layout.size.width
                        - layout.border.left
                        - layout.border.right
                        - layout.padding.left
                        - layout.padding.right
                }
            } else if border_box {
                layout.size.height
            } else {
                layout.size.height
                    - layout.border.top
                    - layout.border.bottom
                    - layout.padding.top
                    - layout.padding.bottom
            };
            return Some(used(size.max(0.0)));
        }
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" if has_layout_box => {
            let m = match property {
                "margin-top" => layout.margin.top,
                "margin-right" => layout.margin.right,
                "margin-bottom" => layout.margin.bottom,
                _ => layout.margin.left,
            };
            return Some(used(m));
        }
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" if has_layout_box => {
            let p = match property {
                "padding-top" => unrounded.padding.top,
                "padding-right" => unrounded.padding.right,
                "padding-bottom" => unrounded.padding.bottom,
                _ => unrounded.padding.left,
            };
            return Some(used(p));
        }
        "transform-origin" | "perspective-origin" if has_layout_box => {
            let resolve =
                |v: &LengthPercentage, basis: f32| used(v.resolve(CSSPixelLength::new(basis)).px());
            let b = styles.get_box();
            return Some(if property == "transform-origin" {
                let o = &b.transform_origin;
                let x = resolve(&o.horizontal, unrounded.size.width);
                let y = resolve(&o.vertical, unrounded.size.height);
                if o.depth.px() == 0.0 {
                    format!("{x} {y}")
                } else {
                    format!("{x} {y} {}", used(o.depth.px()))
                }
            } else {
                let o = &b.perspective_origin;
                let x = resolve(&o.horizontal, unrounded.size.width);
                let y = resolve(&o.vertical, unrounded.size.height);
                format!("{x} {y}")
            });
        }
        "top" | "right" | "bottom" | "left" if has_layout_box => {
            let parent = containing_block(tree, id).map(|p| *tree[p].final_layout());
            let pos = styles.get_position();
            match styles.clone_position() {
                // The relative offset: an `auto` side is the negation of the opposite one;
                // both set (overconstrained), each its computed value.
                Position::Relative => {
                    let (w, h) = parent.map_or((0.0, 0.0), |pl| {
                        (
                            pl.size.width
                                - pl.border.left
                                - pl.border.right
                                - pl.padding.left
                                - pl.padding.right,
                            pl.size.height
                                - pl.border.top
                                - pl.border.bottom
                                - pl.padding.top
                                - pl.padding.bottom,
                        )
                    });
                    let vertical = matches!(property, "top" | "bottom");
                    let basis = if vertical { h } else { w };
                    let (start, end) = if vertical {
                        (
                            resolve_inset(&pos.top, basis),
                            resolve_inset(&pos.bottom, basis),
                        )
                    } else {
                        (
                            resolve_inset(&pos.left, basis),
                            resolve_inset(&pos.right, basis),
                        )
                    };
                    let is_start = matches!(property, "top" | "left");
                    let v = match (start, end) {
                        (Some(s), Some(e)) => {
                            if is_start {
                                s
                            } else {
                                e
                            }
                        }
                        (Some(s), None) => {
                            if is_start {
                                s
                            } else {
                                -s
                            }
                        }
                        (None, Some(e)) => {
                            if is_start {
                                -e
                            } else {
                                e
                            }
                        }
                        (None, None) => 0.0,
                    };
                    return Some(used(v));
                }
                // A set inset resolves as is; `auto` to the distance between the margin edge
                // and the containing block's padding edge.
                Position::Absolute | Position::Fixed => {
                    if let Some(pl) = parent {
                        let cw = pl.size.width - pl.border.left - pl.border.right;
                        let ch = pl.size.height - pl.border.top - pl.border.bottom;
                        let (inset, basis) = match property {
                            "top" => (&pos.top, ch),
                            "bottom" => (&pos.bottom, ch),
                            "left" => (&pos.left, cw),
                            _ => (&pos.right, cw),
                        };
                        if let Some(v) = resolve_inset(inset, basis) {
                            return Some(used(v));
                        }
                        let top = layout.location.y - layout.margin.top - pl.border.top;
                        let left = layout.location.x - layout.margin.left - pl.border.left;
                        let mw = layout.margin.left + layout.size.width + layout.margin.right;
                        let mh = layout.margin.top + layout.size.height + layout.margin.bottom;
                        let v = match property {
                            "top" => top,
                            "bottom" => ch - top - mh,
                            "left" => left,
                            _ => cw - left - mw,
                        };
                        return Some(used(v));
                    }
                }
                _ => {}
            }
        }
        // `min-width: auto` stays `auto` only for boxes with an aspect ratio and flex and grid
        // items; otherwise `0px`.
        "min-width" | "min-height" => {
            let pos = styles.get_position();
            let is_auto = if property == "min-width" {
                pos.min_width.is_auto()
            } else {
                pos.min_height.is_auto()
            };
            let ratio = !matches!(pos.aspect_ratio.ratio, PreferredRatio::None);
            let keeps_auto = generates_box && (ratio || is_flex_or_grid_item(tree, id));
            if is_auto && !keeps_auto {
                return Some(format_px(0.0));
            }
        }
        "transform" if has_layout_box => {
            let transform = &styles.get_box().transform;
            if !transform.0.is_empty() {
                let reference = euclid::Rect::new(
                    euclid::Point2D::new(CSSPixelLength::new(0.0), CSSPixelLength::new(0.0)),
                    euclid::Size2D::new(
                        CSSPixelLength::new(layout.size.width),
                        CSSPixelLength::new(layout.size.height),
                    ),
                );
                if let Ok((m, is_3d)) = transform.to_transform_3d_matrix_f64(Some(&reference)) {
                    return Some(format_matrix(&m, is_3d));
                }
            }
        }
        _ => {}
    }

    let css = match property_id.as_shorthand() {
        Ok(shorthand) => serialize_resolved_shorthand(styles, shorthand),
        Err(declaration) => styles.computed_value_to_string(declaration),
    };
    Some(canonicalize_alignment(property, css))
}

/// A two-sided or four-sided shorthand's values, omitting sides that repeat.
fn serialize_sides(sides: &[String]) -> String {
    let len = match sides {
        [start, end] if start == end => 1,
        [top, right, bottom, left] if right == left => {
            if top != bottom {
                3
            } else if top != right {
                2
            } else {
                1
            }
        }
        _ => sides.len(),
    };
    sides[..len].join(" ")
}

/// A shorthand from its longhands' resolved values (empty when they cannot form it).
fn serialize_resolved_shorthand(styles: &ComputedValues, shorthand: ShorthandId) -> String {
    if shorthand == ShorthandId::All {
        return String::new();
    }
    let declarations: Vec<PropertyDeclaration> = shorthand
        .longhands()
        .map(|longhand| {
            let mut context = resolved::Context {
                style: styles,
                for_property: PropertyId::NonCustom(longhand.into()),
                current_longhand: Some(longhand),
            };
            styles.computed_or_resolved_declaration(longhand, Some(&mut context))
        })
        .collect();
    let refs: Vec<&PropertyDeclaration> = declarations.iter().collect();
    let mut css = CssStringWriter::new();
    let _ = shorthand.longhands_to_css(&refs, &mut css);
    css
}

/// `flex-start`/`flex-end` compute to `flow-start`/`flow-end` in alignment properties
/// (css-align-3), which Stylo does not do yet.
fn canonicalize_alignment(property: &str, value: String) -> String {
    const ALIGNMENT: [&str; 9] = [
        "align-content",
        "align-items",
        "align-self",
        "justify-content",
        "justify-items",
        "justify-self",
        "place-content",
        "place-items",
        "place-self",
    ];
    if !ALIGNMENT.contains(&property) || !value.contains("flex-") {
        return value;
    }
    value
        .replace("flex-start", "flow-start")
        .replace("flex-end", "flow-end")
}
