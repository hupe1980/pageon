//! Presentational hints: legacy attributes that style elements (`hidden`, `width`/`height` on
//! embedded content, tables and `<svg>`, `align`, `bgcolor`, `border`, `hspace`/`vspace`,
//! `<body>` margins, `lang`). Adapted from Blitz (blitz-dom `stylo.rs`, MIT OR Apache-2.0).

use markup5ever::{local_name, ns};
use style::color::AbsoluteColor;
use style::properties::PropertyDeclaration;
use style::shared_lock::SharedRwLock;
use style::values::computed::text::TextAlign as Keyword;
use style::values::generics::NonNegative;
use style::values::generics::length::{GenericMargin, Size};
use style::values::specified::{LengthPercentage, NoCalcLength, NoCalcPercentage};

use crate::node::{N, element};

fn parse_color(value: &str) -> Option<(u8, u8, u8)> {
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

/// The HTML "rules for parsing dimension values" as a specified length or percentage;
/// `ignoring_zero` drops a zero ("maps to the dimension property (ignoring zero)").
fn parse_size(value: &str, ignoring_zero: bool) -> Option<LengthPercentage> {
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

/// An SVG `width`/`height`: any CSS length or percentage; a bare number is user units (px).
fn parse_svg_size(value: &str) -> Option<LengthPercentage> {
    use style_traits::ParsingMode;
    let value = value.trim();
    if let Some(number) = value.strip_suffix('%') {
        let v: f32 = number.trim().parse().ok()?;
        return (v >= 0.0).then(|| LengthPercentage::Percentage(NoCalcPercentage::new(v / 100.0)));
    }
    let number_len = value
        .trim_end_matches(|c: char| c.is_ascii_alphabetic())
        .len();
    let (number, unit) = value.split_at(number_len);
    let v: f32 = number.trim().parse().ok().filter(|v| *v >= 0.0)?;
    let length = if unit.is_empty() {
        NoCalcLength::from_px(v)
    } else {
        NoCalcLength::parse_dimension_with_flags(ParsingMode::DEFAULT, false, v, unit).ok()?
    };
    Some(LengthPercentage::Length(length))
}

pub(crate) fn push(n: &N<'_>, _guard: &SharedRwLock, push: &mut dyn FnMut(PropertyDeclaration)) {
    let Some((q, attrs)) = element(n) else {
        return;
    };
    let html = q.ns == ns!(html);
    let tag = &q.local;
    let is = |t: markup5ever::LocalName| html && *tag == t;
    let is_image_input = is(local_name!("input"))
        && attrs.iter().any(|a| {
            a.qual_name().local == local_name!("type") && a.value.eq_ignore_ascii_case("image")
        });
    let accepts_lang = html || q.ns == ns!(svg);
    let mut lang = None;
    for a in attrs {
        let name = &a.qual_name().local;
        let value = a.value.as_str();
        if *name == local_name!("lang") {
            if a.qual_name().ns == ns!(xml) {
                lang = Some(value);
            } else if a.qual_name().ns == ns!() && accepts_lang {
                lang = lang.or(Some(value));
            }
        }
        if !html && !(q.ns == ns!(svg) && *tag == local_name!("svg")) {
            continue;
        }
        if *name == local_name!("align") && html {
            let k = match value.to_ascii_lowercase().as_str() {
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
        let (is_width, is_height) = (
            *name == local_name!("width"),
            *name == local_name!("height"),
        );
        let embedded = ["iframe", "embed", "video", "object", "img", "marquee"]
            .iter()
            .any(|t| is(markup5ever::LocalName::from(*t)))
            || is_image_input;
        let maps = if is_width {
            embedded
                || ["table", "col", "colgroup", "tr", "td", "th", "hr"]
                    .iter()
                    .any(|t| is(markup5ever::LocalName::from(*t)))
        } else if is_height {
            embedded
                || ["table", "thead", "tbody", "tfoot", "tr", "td", "th"]
                    .iter()
                    .any(|t| is(markup5ever::LocalName::from(*t)))
        } else {
            false
        };
        if maps {
            let cell = is(local_name!("td")) || is(local_name!("th"));
            let ignoring_zero = cell || (is_width && is(local_name!("table")));
            if let Some(size) = parse_size(value, ignoring_zero) {
                let size = Size::LengthPercentage(NonNegative(size));
                push(if is_width {
                    PropertyDeclaration::Width(size)
                } else {
                    PropertyDeclaration::Height(size)
                });
            }
        }
        // `<svg>` width and height are presentation attributes (`1em` resolves as CSS does).
        if q.ns == ns!(svg)
            && *tag == local_name!("svg")
            && (is_width || is_height)
            && let Some(size) = parse_svg_size(value)
        {
            let size = Size::LengthPercentage(NonNegative(size));
            push(if is_width {
                PropertyDeclaration::Width(size)
            } else {
                PropertyDeclaration::Height(size)
            });
        }
        // hspace/vspace: a smaller set than width/height takes (not iframe or video).
        let spacing = ["embed", "img", "object", "marquee"]
            .iter()
            .any(|t| is(markup5ever::LocalName::from(*t)))
            || is_image_input;
        if spacing
            && (*name == local_name!("hspace") || *name == local_name!("vspace"))
            && let Some(size) = parse_size(value, false)
        {
            let m = GenericMargin::LengthPercentage(size);
            if *name == local_name!("hspace") {
                push(PropertyDeclaration::MarginLeft(m.clone()));
                push(PropertyDeclaration::MarginRight(m));
            } else {
                push(PropertyDeclaration::MarginTop(m.clone()));
                push(PropertyDeclaration::MarginBottom(m));
            }
        }
        // `border` on images and objects: widths in px, solid.
        if *name == local_name!("border")
            && (is(local_name!("img")) || is(local_name!("object")) || is_image_input)
            && let Ok(px) = style::servo::attr::parse_unsigned_integer(value.chars())
        {
            use style::values::specified::{BorderSideWidth, BorderStyle};
            let w = BorderSideWidth::from_px(px as f32);
            push(PropertyDeclaration::BorderTopWidth(w.clone()));
            push(PropertyDeclaration::BorderRightWidth(w.clone()));
            push(PropertyDeclaration::BorderBottomWidth(w.clone()));
            push(PropertyDeclaration::BorderLeftWidth(w));
            push(PropertyDeclaration::BorderTopStyle(BorderStyle::Solid));
            push(PropertyDeclaration::BorderRightStyle(BorderStyle::Solid));
            push(PropertyDeclaration::BorderBottomStyle(BorderStyle::Solid));
            push(PropertyDeclaration::BorderLeftStyle(BorderStyle::Solid));
        }
        // `<body>` margins: marginwidth/marginheight (both sides), leftmargin/topmargin.
        if is(local_name!("body")) {
            let sides: &[u8] = match &**name {
                "marginwidth" => b"lr",
                "marginheight" => b"tb",
                "leftmargin" => b"l",
                "topmargin" => b"t",
                _ => b"",
            };
            if !sides.is_empty()
                && let Ok(px) = style::servo::attr::parse_unsigned_integer(value.chars())
            {
                let m = GenericMargin::LengthPercentage(LengthPercentage::Length(
                    NoCalcLength::from_px(px as f32),
                ));
                for side in sides {
                    push(match side {
                        b'l' => PropertyDeclaration::MarginLeft(m.clone()),
                        b'r' => PropertyDeclaration::MarginRight(m.clone()),
                        b't' => PropertyDeclaration::MarginTop(m.clone()),
                        _ => PropertyDeclaration::MarginBottom(m.clone()),
                    });
                }
            }
        }
        if *name == local_name!("bgcolor")
            && html
            && let Some((r, g, b)) = parse_color(value)
        {
            push(PropertyDeclaration::BackgroundColor(
                style::values::specified::Color::from_absolute_color(AbsoluteColor::srgb_legacy(
                    r, g, b, 1.0,
                )),
            ));
        }
        if *name == local_name!("hidden") && html {
            push(PropertyDeclaration::Display(
                style::values::specified::Display::None,
            ));
        }
    }
    if let Some(lang) = lang {
        push(PropertyDeclaration::XLang(
            style::values::computed::font::XLang(style::Atom::from(lang)),
        ));
    }
}
