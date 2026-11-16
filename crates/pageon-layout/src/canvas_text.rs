//! Text on a canvas: the `font` shorthand, shaping with the bundled fonts (so metrics are the
//! same on every machine and say nothing about the host), metrics, and glyph outlines to fill
//! or stroke.

use std::cell::RefCell;

use parley::{FontContext, LayoutContext, PositionedLayoutItem, StyleProperty};
use skrifa::MetadataProvider as _;
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};

/// A canvas `font`: the parts that change shaping.
#[derive(Debug, Clone, PartialEq)]
pub struct CanvasFont {
    /// `italic`, `oblique` or empty.
    pub style: &'static str,
    /// 1 to 1000.
    pub weight: u16,
    /// CSS px.
    pub size: f32,
    /// The family list, as written.
    pub family: String,
}

impl Default for CanvasFont {
    fn default() -> Self {
        Self {
            style: "",
            weight: 400,
            size: 10.0,
            family: "sans-serif".into(),
        }
    }
}

impl CanvasFont {
    /// Parses the CSS `font` shorthand (`bold 12px/1.5 "Helvetica Neue", Arial, sans-serif`);
    /// `None` when it is not a font, as the canvas then keeps its font.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let mut font = Self::default();
        let mut rest = s;
        loop {
            let (word, after) = rest.split_once(char::is_whitespace)?;
            let w = word.to_ascii_lowercase();
            match w.as_str() {
                "normal" | "small-caps" | "condensed" | "expanded" | "semi-condensed"
                | "semi-expanded" => {}
                "italic" => font.style = "italic",
                "oblique" => font.style = "oblique",
                "bold" | "bolder" => font.weight = 700,
                "lighter" => font.weight = 300,
                _ => match w.parse::<u16>() {
                    Ok(n) if (1..=1000).contains(&n) && !w.ends_with("px") => font.weight = n,
                    _ => {
                        font.size = size(word.split('/').next()?)?;
                        font.family = after.trim().to_string();
                        return (!font.family.is_empty()).then_some(font);
                    }
                },
            }
            rest = after.trim_start();
        }
    }

    /// The `font` attribute as Chrome serializes it.
    pub fn css(&self) -> String {
        let mut out = String::new();
        if !self.style.is_empty() {
            out.push_str(self.style);
            out.push(' ');
        }
        match self.weight {
            400 => {}
            700 => out.push_str("bold "),
            w => out.push_str(&format!("{w} ")),
        }
        let size = if self.size.fract() == 0.0 {
            format!("{}", self.size as i64)
        } else {
            format!("{}", self.size)
        };
        out.push_str(&format!("{size}px {}", self.family));
        out
    }
}

/// A font size in px: `12px`, `9pt`, `1.5em` (of the canvas default, 10px), `150%`.
fn size(s: &str) -> Option<f32> {
    let s = s.to_ascii_lowercase();
    let units: [(&str, f32); 5] = [
        ("px", 1.0),
        ("pt", 4.0 / 3.0),
        ("rem", 16.0),
        ("em", 10.0),
        ("%", 0.1),
    ];
    let (n, k) = units
        .iter()
        .find_map(|(u, k)| s.strip_suffix(u).map(|n| (n, *k)))?;
    let v = n.parse::<f32>().ok()? * k;
    (v.is_finite() && v >= 0.0).then_some(v)
}

/// The measurements of a run of text, in CSS px, from the start of the text on its
/// alphabetic baseline (y down).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CanvasTextMetrics {
    /// The advance width.
    pub width: f32,
    /// How far the ink reaches left of the start (positive: left).
    pub actual_left: f32,
    /// How far the ink reaches right of the start.
    pub actual_right: f32,
    /// How far the ink reaches above the baseline.
    pub actual_ascent: f32,
    /// How far the ink reaches below the baseline.
    pub actual_descent: f32,
    /// The font's ascent.
    pub font_ascent: f32,
    /// The font's descent.
    pub font_descent: f32,
}

/// One segment of a text outline, in the same coordinates as the metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    /// Starts a contour.
    Move(f32, f32),
    /// A line.
    Line(f32, f32),
    /// A quadratic curve: control, end.
    Quad(f32, f32, f32, f32),
    /// A cubic curve: two controls, end.
    Cubic(f32, f32, f32, f32, f32, f32),
    /// Closes the contour.
    Close,
}

thread_local! {
    static CONTEXTS: RefCell<(FontContext, LayoutContext<()>)> =
        RefCell::new((crate::fonts::context(), LayoutContext::new()));
}

struct Pen {
    x: f32,
    y: f32,
    out: Vec<Segment>,
    bounds: Option<(f32, f32, f32, f32)>,
}

impl Pen {
    fn at(&mut self, px: f32, py: f32) -> (f32, f32) {
        let (x, y) = (self.x + px, self.y - py);
        let b = self.bounds.get_or_insert((x, y, x, y));
        *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
        (x, y)
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.out.push(Segment::Move(x, y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.at(x, y);
        self.out.push(Segment::Line(x, y));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (a, b) = self.at(cx0, cy0);
        let (x, y) = self.at(x, y);
        self.out.push(Segment::Quad(a, b, x, y));
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (a, b) = self.at(cx0, cy0);
        let (c, d) = self.at(cx1, cy1);
        let (x, y) = self.at(x, y);
        self.out.push(Segment::Cubic(a, b, c, d, x, y));
    }
    fn close(&mut self) {
        self.out.push(Segment::Close);
    }
}

/// Shapes `text` (on one line; line breaks are spaces, as on a canvas) in `font`: its metrics,
/// and its outline when `outline` is set.
pub fn canvas_text(
    font: &CanvasFont,
    text: &str,
    outline: bool,
) -> (CanvasTextMetrics, Vec<Segment>) {
    let text: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    CONTEXTS.with(|c| {
        let (fcx, lcx) = &mut *c.borrow_mut();
        let mut builder = lcx.ranged_builder(fcx, &text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(parley::FontFamily::Source(
            std::borrow::Cow::Borrowed(&font.family),
        )));
        builder.push_default(StyleProperty::FontSize(font.size));
        builder.push_default(StyleProperty::FontWeight(parley::FontWeight::new(
            f32::from(font.weight),
        )));
        builder.push_default(StyleProperty::FontStyle(match font.style {
            "italic" => parley::FontStyle::Italic,
            "oblique" => parley::FontStyle::Oblique(None),
            _ => parley::FontStyle::Normal,
        }));
        let mut layout = builder.build(&text);
        layout.break_all_lines(None);
        let mut m = CanvasTextMetrics {
            width: layout.full_width(),
            ..CanvasTextMetrics::default()
        };
        let mut pen = Pen {
            x: 0.0,
            y: 0.0,
            out: Vec::new(),
            bounds: None,
        };
        let mut baseline = None;
        for line in layout.lines() {
            for item in line.items() {
                let PositionedLayoutItem::GlyphRun(run) = item else {
                    continue;
                };
                let base = *baseline.get_or_insert(run.baseline());
                let data = run.run().font();
                let Ok(face) = skrifa::FontRef::from_index(data.data.as_ref(), data.index) else {
                    continue;
                };
                let size = Size::new(run.run().font_size());
                let fm = face.metrics(size, LocationRef::default());
                m.font_ascent = m.font_ascent.max(fm.ascent);
                m.font_descent = m.font_descent.max(-fm.descent);
                let glyphs = face.outline_glyphs();
                for g in run.positioned_glyphs() {
                    let Some(glyph) = glyphs.get(skrifa::GlyphId::new(g.id)) else {
                        continue;
                    };
                    pen.x = g.x;
                    pen.y = g.y - base;
                    let settings = DrawSettings::unhinted(size, LocationRef::default());
                    let _ = glyph.draw(settings, &mut pen);
                }
            }
        }
        if let Some((x0, y0, x1, y1)) = pen.bounds {
            m.actual_left = -x0;
            m.actual_right = x1;
            m.actual_ascent = -y0;
            m.actual_descent = y1;
        }
        let segments = if outline { pen.out } else { Vec::new() };
        (m, segments)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_font_shorthand() {
        let f = CanvasFont::parse("bold 12px Arial").unwrap_or_default();
        assert_eq!((f.weight, f.size, f.family.as_str()), (700, 12.0, "Arial"));
        assert_eq!(f.css(), "bold 12px Arial");
        let f = CanvasFont::parse("italic 600 9pt/1.5 \"Helvetica Neue\", sans-serif")
            .unwrap_or_default();
        assert_eq!((f.style, f.weight, f.size), ("italic", 600, 12.0));
        assert_eq!(f.css(), "italic 600 12px \"Helvetica Neue\", sans-serif");
        assert_eq!(CanvasFont::default().css(), "10px sans-serif");
        for bad in ["", "12px", "bold", "big Arial", "-3px Arial"] {
            assert_eq!(CanvasFont::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn metrics_come_from_the_bundled_fonts() {
        let arial = CanvasFont::parse("20px Arial").unwrap_or_default();
        let sans = CanvasFont::parse("20px sans-serif").unwrap_or_default();
        let host_only =
            CanvasFont::parse("20px \"Some Host Font\", sans-serif").unwrap_or_default();
        let (a, _) = canvas_text(&arial, "Hello, world", false);
        let (b, _) = canvas_text(&sans, "Hello, world", false);
        let (c, _) = canvas_text(&host_only, "Hello, world", false);
        assert!(a.width > 90.0 && a.width < 130.0, "{a:?}");
        assert_eq!(a.width, b.width);
        assert_eq!(a.width, c.width);
        assert!(a.font_ascent > 15.0 && a.font_descent > 3.0, "{a:?}");
        assert!(
            a.actual_ascent > 10.0 && a.actual_ascent <= a.font_ascent,
            "{a:?}"
        );
        let (_, path) = canvas_text(&arial, "Hi", true);
        assert!(path.iter().any(|s| matches!(s, Segment::Close)));
    }
}
