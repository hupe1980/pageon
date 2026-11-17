//! Adapted from Blitz (blitz-paint `src/color.rs`, MIT OR Apache-2.0).

use color::{AlphaColor, DynamicColor, Srgb};
use style::color::AbsoluteColor;

pub type Color = AlphaColor<Srgb>;

pub trait ToColorColor {
    /// Converts a color into the `AlphaColor<Srgb>` type from the `color` crate
    fn as_srgb_color(&self) -> Color;

    /// Converts a color into the `DynamicColor` type from the `color` crate
    fn as_dynamic_color(&self) -> DynamicColor;
}
impl ToColorColor for AbsoluteColor {
    fn as_srgb_color(&self) -> Color {
        Color::new(
            *self
                .to_color_space(style::color::ColorSpace::Srgb)
                .raw_components(),
        )
    }

    fn as_dynamic_color(&self) -> DynamicColor {
        DynamicColor::from_alpha_color(self.as_srgb_color())
    }
}

/// The WCAG contrast ratio (>= 1) between two colours, matching Chrome's
/// `color_utils::GetContrastRatio`.
pub(crate) fn contrast_ratio(a: Color, b: Color) -> f32 {
    // `relative_luminance` is defined on `OpaqueColor`; discard the alpha (which
    // it ignores anyway) with `split`.
    let la = a.split().0.relative_luminance() + 0.05;
    let lb = b.split().0.relative_luminance() + 0.05;
    if la > lb { la / lb } else { lb / la }
}
