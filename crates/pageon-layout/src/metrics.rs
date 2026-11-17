//! Font metrics for CSS units that depend on the font (`ex`, `ch`, `cap`, `ic`), from the
//! bundled fonts, for styles computed on pageon's document. Adapted from Blitz (blitz-dom
//! `font_metrics.rs` and `stylo_to_parley.rs`, MIT OR Apache-2.0).

use std::sync::{Arc, Mutex};

use app_units::Au;
use parley::fontique::{Query, QueryFamily, QueryFont, QueryStatus};
use parley::{FontContext, FontVariation};
use skrifa::MetadataProvider as _;
use skrifa::charmap::Charmap;
use skrifa::instance::{LocationRef, Size};
use skrifa::metrics::{GlyphMetrics, Metrics};
use style::device::servo::FontMetricsProvider;
use style::font_metrics::FontMetrics;
use style::properties::style_structs::Font;
use style::values::computed::font::{GenericFontFamily, QueryFontMetricsFlags, SingleFontFamily};
use style::values::computed::{CSSPixelLength, Length};

/// The provider styles use: metrics of the bundled fonts, as layout shapes text with them.
pub fn font_metrics() -> Box<dyn FontMetricsProvider> {
    Box::new(Provider {
        ctx: Arc::new(Mutex::new(crate::fonts::context())),
    })
}

struct Provider {
    ctx: Arc<Mutex<FontContext>>,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Provider")
    }
}

fn generic(input: GenericFontFamily) -> parley::GenericFamily {
    match input {
        GenericFontFamily::None | GenericFontFamily::SansSerif => parley::GenericFamily::SansSerif,
        GenericFontFamily::Serif => parley::GenericFamily::Serif,
        GenericFontFamily::Monospace => parley::GenericFamily::Monospace,
        GenericFontFamily::Cursive => parley::GenericFamily::Cursive,
        GenericFontFamily::Fantasy => parley::GenericFamily::Fantasy,
        GenericFontFamily::SystemUi => parley::GenericFamily::SystemUi,
    }
}

fn family(input: &SingleFontFamily) -> QueryFamily<'_> {
    match input {
        SingleFontFamily::FamilyName(name) => {
            let name = name.name.as_str();
            if cfg!(target_vendor = "apple") && name == "-apple-system"
                || cfg!(target_os = "macos") && name == "BlinkMacSystemFont"
            {
                return QueryFamily::Generic(parley::GenericFamily::SystemUi);
            }
            QueryFamily::Named(name)
        }
        SingleFontFamily::Generic(g) => QueryFamily::Generic(generic(*g)),
    }
}

fn style_of(input: style::values::computed::font::FontStyle) -> parley::FontStyle {
    use style::values::computed::font::FontStyle;
    match input {
        FontStyle::NORMAL => parley::FontStyle::Normal,
        FontStyle::ITALIC => parley::FontStyle::Italic,
        v => parley::FontStyle::Oblique(Some(v.oblique_degrees())),
    }
}

fn find(query: &mut Query<'_>, ch: char) -> Option<QueryFont> {
    let mut font = None;
    query.matches_with(|f: &QueryFont| {
        let Ok(r) = skrifa::FontRef::from_index(f.blob.as_ref(), f.index) else {
            return QueryStatus::Continue;
        };
        if r.charmap().map(ch).is_some() {
            font = Some(f.clone());
            QueryStatus::Stop
        } else {
            QueryStatus::Continue
        }
    });
    font
}

fn location(r: &skrifa::FontRef<'_>, variations: &[FontVariation]) -> skrifa::instance::Location {
    r.axes().location(
        variations
            .iter()
            .map(|v| (skrifa::Tag::from_be_bytes(v.tag.to_bytes()), v.value)),
    )
}

/// A glyph's advance, scaled as layout scales shaped advances (font units × size / upem).
fn advance(query: &mut Query<'_>, ch: char, size: f32, variations: &[FontVariation]) -> Option<f32> {
    let font = find(query, ch)?;
    let r = skrifa::FontRef::from_index(font.blob.as_ref(), font.index).ok()?;
    let loc = location(&r, variations);
    let at = LocationRef::from(&loc);
    let upem = Metrics::new(&r, Size::unscaled(), at).units_per_em;
    if upem == 0 {
        return None;
    }
    let glyph = Charmap::new(&r).map(ch)?;
    let a = GlyphMetrics::new(&r, Size::unscaled(), at).advance_width(glyph)?;
    Some(a * (size / f32::from(upem)))
}

impl FontMetricsProvider for Provider {
    fn query_font_metrics(
        &self,
        _vertical: bool,
        font: &Font,
        size: CSSPixelLength,
        _flags: QueryFontMetricsFlags,
    ) -> FontMetrics {
        let Ok(mut ctx) = self.ctx.lock() else {
            return FontMetrics::default();
        };
        let ctx = &mut *ctx;
        let mut query = ctx.collection.query(&mut ctx.source_cache);
        query.set_families(font.font_family.families.iter().map(family));
        query.set_attributes(parley::fontique::Attributes {
            width: parley::FontWidth::from_percentage(font.font_width.0.to_float()),
            weight: parley::FontWeight::new(font.font_weight.value()),
            style: style_of(font.font_style),
        });
        let variations: Vec<FontVariation> = font
            .font_variation_settings
            .0
            .iter()
            .map(|v| FontVariation {
                tag: parley::setting::Tag::from_bytes(v.tag.0.to_be_bytes()),
                value: v.value,
            })
            .collect();
        let zero = advance(&mut query, '0', size.px(), &variations);
        let ic = advance(&mut query, '\u{6C34}', size.px(), &variations);
        let (ascent, x_height, cap_height) = find(&mut query, ' ')
            .and_then(|f| {
                let r = skrifa::FontRef::from_index(f.blob.as_ref(), f.index).ok()?;
                let loc = location(&r, &variations);
                let m = Metrics::new(&r, Size::new(size.px()), LocationRef::from(&loc));
                Some((m.ascent, m.x_height, m.cap_height))
            })
            .unwrap_or((0.0, None, None));
        FontMetrics {
            ascent: CSSPixelLength::new(ascent),
            x_height: x_height.filter(|x| *x != 0.0).map(CSSPixelLength::new),
            cap_height: cap_height.map(CSSPixelLength::new),
            zero_advance_measure: zero.map(CSSPixelLength::new),
            ic_width: ic.map(CSSPixelLength::new),
            script_percent_scale_down: None,
            script_script_percent_scale_down: None,
        }
    }

    fn base_size_for_generic(&self, generic: GenericFontFamily) -> Length {
        let px = if generic == GenericFontFamily::Monospace {
            13.0
        } else {
            16.0
        };
        Length::from(Au::from_f32_px(px))
    }
}
