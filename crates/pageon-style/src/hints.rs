//! Presentational hints: legacy attributes that style elements (`hidden`, `width`/`height` on
//! embedded content and tables, `align`, `bgcolor`). Adapted from Blitz (blitz-dom `stylo.rs`,
//! MIT OR Apache-2.0).

use style::color::AbsoluteColor;
use style::properties::PropertyDeclaration;
use style::shared_lock::SharedRwLock;
use style::values::specified::{LengthPercentage, NoCalcLength, NoCalcPercentage};

use crate::node::{N, attr, is};

/// The HTML "rules for parsing dimension values" as a specified length or percentage.
fn size(value: &str, ignoring_zero: bool) -> Option<LengthPercentage> {
    use style::servo::attr::{LengthOrPercentageOrAuto, parse_length, parse_nonzero_length};
    let parsed = if ignoring_zero {
        parse_nonzero_length(value)
    } else {
        parse_length(value)
    };
    match parsed {
        LengthOrPercentageOrAuto::Length(l) => Some(LengthPercentage::Length(
            NoCalcLength::from_px(l.to_f32_px()),
        )),
        LengthOrPercentageOrAuto::Percentage(p) => {
            Some(LengthPercentage::Percentage(NoCalcPercentage::new(p)))
        }
        LengthOrPercentageOrAuto::Auto => None,
    }
}

fn hex(value: &str) -> Option<(u8, u8, u8)> {
    let v = value.strip_prefix('#')?;
    let byte = |s: &str| u8::from_str_radix(s, 16).ok();
    match v.len() {
        3 => Some((
            byte(&v[0..1])? * 17,
            byte(&v[1..2])? * 17,
            byte(&v[2..3])? * 17,
        )),
        6 => Some((byte(&v[0..2])?, byte(&v[2..4])?, byte(&v[4..6])?)),
        _ => None,
    }
}

pub(crate) fn push(n: &N<'_>, _guard: &SharedRwLock, push: &mut dyn FnMut(PropertyDeclaration)) {
    use style::values::generics::{NonNegative, length::Size};
    if attr(n, "hidden").is_some() {
        push(PropertyDeclaration::Display(
            style::values::specified::Display::None,
        ));
    }
    let embedded = ["img", "iframe", "embed", "video", "object", "canvas"]
        .iter()
        .any(|t| is(n, t));
    let cell = is(n, "td") || is(n, "th");
    let table_part = is(n, "table") || is(n, "tr") || cell;
    if (embedded || table_part || is(n, "col") || is(n, "hr"))
        && let Some(w) = attr(n, "width").and_then(|v| size(v, cell || is(n, "table")))
    {
        push(PropertyDeclaration::Width(Size::LengthPercentage(
            NonNegative(w),
        )));
    }
    if (embedded || table_part || ["thead", "tbody", "tfoot"].iter().any(|t| is(n, t)))
        && let Some(h) = attr(n, "height").and_then(|v| size(v, cell))
    {
        push(PropertyDeclaration::Height(Size::LengthPercentage(
            NonNegative(h),
        )));
    }
    if let Some((r, g, b)) = attr(n, "bgcolor").and_then(hex) {
        push(PropertyDeclaration::BackgroundColor(
            style::values::specified::Color::from_absolute_color(AbsoluteColor::srgb_legacy(
                r, g, b, 1.0,
            )),
        ));
    }
    if let Some(a) = attr(n, "align") {
        use style::values::computed::text::TextAlign as Keyword;
        let k = match a.to_ascii_lowercase().as_str() {
            "left" => Some(Keyword::MozLeft),
            "right" => Some(Keyword::MozRight),
            "center" => Some(Keyword::MozCenter),
            _ => None,
        };
        if let Some(k) = k {
            push(PropertyDeclaration::TextAlign(
                style::values::specified::TextAlign::Keyword(k),
            ));
        }
    }
}
