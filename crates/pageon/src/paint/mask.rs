//! Masks: what `mask: true` paints over in a screenshot. These are third-party frames
//! (the provenance tier the text views mark), and text a person cannot read but a vision
//! model would (contrast below 1.5:1 against its background, or a font size below 4 px): the
//! injection vectors in pixels.

use pageon_dom::{Edge, NodeData, NodeId};
use pageon_layout::{Layout, Rect, Visibility};

use crate::Document;

/// Below this contrast ratio, text is unreadable to a person (white on #f5f5f5 is 1.09:1;
/// light-grey placeholders are about 2–3:1 and stay).
const MIN_CONTRAST: f64 = 1.5;
/// Below this computed font size (px), text is unreadable.
const MIN_FONT_PX: f64 = 4.0;

/// The boxes to mask (top-level CSS px), and how many frames and texts they are.
pub(crate) struct Masks {
    pub(crate) boxes: Vec<Rect>,
    pub(crate) frames: usize,
    pub(crate) texts: usize,
}

impl Masks {
    /// The note for the image, if anything was masked.
    pub(crate) fn note(&self) -> Option<String> {
        if self.frames + self.texts == 0 {
            return None;
        }
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        Some(format!(
            "masked: {}, {}",
            plural(self.frames, "third-party frame", "third-party frames"),
            plural(self.texts, "unreadable text", "unreadable texts")
        ))
    }
}

pub(crate) fn find(doc: &Document, l: &Layout) -> Masks {
    let mut m = Masks {
        boxes: Vec::new(),
        frames: 0,
        texts: 0,
    };
    let top = crate::frames::origin_of(&doc.final_url);
    for f in &doc.frames {
        if f.origin != top
            && let Some(b) = l.box_of(f.owner)
            && b.area() > 0.0
        {
            m.boxes.push(b);
            m.frames += 1;
        }
    }
    let mut seen = std::collections::HashSet::new();
    for e in doc.dom.traverse(doc.dom.root()) {
        let Edge::Enter(id) = e else { continue };
        let Some(NodeData::Text(t)) = doc.dom.get(id).map(|n| &n.data) else {
            continue;
        };
        if t.trim().is_empty() {
            continue;
        }
        let Some(p) = doc.dom.parent(id) else {
            continue;
        };
        if !seen.insert(p) || l.visibility(p) != Visibility::Visible {
            continue;
        }
        if unreadable(doc, l, p)
            && let Some(b) = l.box_of(p)
            && b.area() > 0.0
        {
            m.boxes.push(b);
            m.texts += 1;
        }
    }
    m
}

fn unreadable(doc: &Document, l: &Layout, el: NodeId) -> bool {
    let size = l
        .style(el, "font-size")
        .and_then(|s| s.trim_end_matches("px").trim().parse::<f64>().ok());
    if size.is_some_and(|s| s < MIN_FONT_PX) {
        return true;
    }
    let Some(fg) = l.style(el, "color").and_then(|c| rgba(&c)) else {
        return false;
    };
    let bg = background(doc, l, el);
    contrast(blend(fg, bg), bg) < MIN_CONTRAST
}

/// The colour behind `el`: the first ancestor (or itself) with an opaque background; white.
fn background(doc: &Document, l: &Layout, el: NodeId) -> [f64; 3] {
    let mut at = Some(el);
    while let Some(id) = at {
        if let Some([r, g, b, a]) = l.style(id, "background-color").and_then(|c| rgba(&c))
            && a >= 0.99
        {
            return [r, g, b];
        }
        at = doc.dom.parent(id);
    }
    [255.0, 255.0, 255.0]
}

/// `rgb(…)` / `rgba(…)` as `[r, g, b, a]` (0–255, alpha 0–1).
fn rgba(css: &str) -> Option<[f64; 4]> {
    let inner = css.split_once('(')?.1.trim_end_matches(')');
    let parts: Vec<f64> = inner
        .split([',', ' ', '/'])
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    match parts.as_slice() {
        [r, g, b] => Some([*r, *g, *b, 1.0]),
        [r, g, b, a, ..] => Some([*r, *g, *b, *a]),
        _ => None,
    }
}

fn blend([r, g, b, a]: [f64; 4], bg: [f64; 3]) -> [f64; 3] {
    [
        r * a + bg[0] * (1.0 - a),
        g * a + bg[1] * (1.0 - a),
        b * a + bg[2] * (1.0 - a),
    ]
}

/// WCAG 2 contrast ratio.
fn contrast(a: [f64; 3], b: [f64; 3]) -> f64 {
    let lum = |c: [f64; 3]| {
        let ch = |v: f64| {
            let v = v / 255.0;
            if v <= 0.039_28 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c[0]) + 0.7152 * ch(c[1]) + 0.0722 * ch(c[2])
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_matches_wcag() {
        assert!((contrast([0.0; 3], [255.0; 3]) - 21.0).abs() < 0.01);
        assert!(contrast([255.0; 3], [245.0; 3]) < MIN_CONTRAST);
        assert!(
            contrast([170.0; 3], [255.0; 3]) > MIN_CONTRAST,
            "light grey stays"
        );
        assert_eq!(rgba("rgba(1, 2, 3, 0.5)"), Some([1.0, 2.0, 3.0, 0.5]));
        assert_eq!(rgba("rgb(10 20 30)"), Some([10.0, 20.0, 30.0, 1.0]));
    }
}
