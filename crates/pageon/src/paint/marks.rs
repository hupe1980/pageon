//! Set-of-marks: each visible element with a ref gets an outline and a label with
//! its ref, drawn into the pixels only. Labels use a built-in 5×7 pixel font (`e`
//! and digits), so they are identical on every machine and need no font machinery.

use pageon_layout::{Pixmap, Rect};

/// One mark: a ref, its `ax` line and its box (top-level document CSS px).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mark {
    pub(crate) reference: String,
    pub(crate) line: String,
    pub(crate) rect: Rect,
}

/// High-contrast colours that carry white text, cycled in document order.
const PALETTE: [[u8; 3]; 12] = [
    [230, 25, 75],
    [0, 110, 200],
    [40, 140, 60],
    [210, 100, 0],
    [145, 30, 180],
    [0, 128, 128],
    [160, 100, 30],
    [220, 40, 200],
    [0, 0, 140],
    [128, 0, 0],
    [110, 110, 0],
    [70, 70, 70],
];

/// 5×7 glyphs, one row per byte (high 5 bits used, left to right).
fn glyph(c: char) -> [u8; 7] {
    match c {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'e' => [0x00, 0x00, 0x0E, 0x11, 0x1F, 0x10, 0x0E],
        _ => [0; 7],
    }
}

fn fill(px: &mut Pixmap, x0: i64, y0: i64, x1: i64, y1: i64, rgb: [u8; 3]) {
    let (w, h) = (i64::from(px.width), i64::from(px.height));
    for y in y0.max(0)..y1.min(h) {
        for x in x0.max(0)..x1.min(w) {
            let i = ((y * w + x) * 4) as usize;
            px.data[i..i + 3].copy_from_slice(&rgb);
            px.data[i + 3] = 255;
        }
    }
}

fn frame(px: &mut Pixmap, r: [i64; 4], t: i64, rgb: [u8; 3]) {
    let [x0, y0, x1, y1] = r;
    fill(px, x0, y0, x1, y0 + t, rgb);
    fill(px, x0, y1 - t, x1, y1, rgb);
    fill(px, x0, y0, x0 + t, y1, rgb);
    fill(px, x1 - t, y0, x1, y1, rgb);
}

fn overlaps(a: [i64; 4], b: [i64; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// Draws the marks onto a painting of `area` at `scale`.
pub(crate) fn draw(px: &mut Pixmap, marks: &[Mark], area: Rect, scale: f32) {
    let k = (2.0 * scale).round().max(1.0) as i64; // device px per font pixel
    let t = (2.0 * scale).round().max(1.0) as i64; // outline thickness
    let dev = |v: f32, o: f32| ((v - o) * scale).round() as i64;
    let (w, h) = (i64::from(px.width), i64::from(px.height));
    let mut placed: Vec<[i64; 4]> = Vec::new();
    for (n, m) in marks.iter().enumerate() {
        let rgb = PALETTE[n % PALETTE.len()];
        let r = [
            dev(m.rect.x, area.x),
            dev(m.rect.y, area.y),
            dev(m.rect.x + m.rect.width, area.x),
            dev(m.rect.y + m.rect.height, area.y),
        ];
        frame(
            px,
            [r[0] - 1, r[1] - 1, r[2] + 1, r[3] + 1],
            1,
            [255, 255, 255],
        );
        frame(px, r, t, rgb);

        let chars: Vec<char> = m.reference.chars().collect();
        let (lw, lh) = (
            (chars.len() as i64 * 6 + 1) * k,
            9 * k, // 7 rows plus a pixel of padding above and below
        );
        let inside = r[2] - r[0] >= lw && r[3] - r[1] >= lh * 2;
        let corners = if inside {
            [
                [r[0], r[1]],
                [r[2] - lw, r[1]],
                [r[0], r[3] - lh],
                [r[2] - lw, r[3] - lh],
            ]
        } else {
            [
                [r[0], r[1] - lh],
                [r[2] - lw, r[1] - lh],
                [r[0], r[3]],
                [r[2] - lw, r[3]],
            ]
        };
        let clamp = |[x, y]: [i64; 2]| {
            let x = x.clamp(0, (w - lw).max(0));
            let y = y.clamp(0, (h - lh).max(0));
            [x, y, x + lw, y + lh]
        };
        let spot = corners
            .iter()
            .map(|&c| clamp(c))
            .find(|s| !placed.iter().any(|p| overlaps(*p, *s)))
            .unwrap_or_else(|| clamp(corners[0]));
        placed.push(spot);
        fill(px, spot[0], spot[1], spot[2], spot[3], rgb);
        for (i, c) in chars.iter().enumerate() {
            let g = glyph(*c);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) != 0 {
                        let x = spot[0] + (1 + i as i64 * 6 + col) * k;
                        let y = spot[1] + (1 + row as i64) * k;
                        fill(px, x, y, x + k, y + k, [255, 255, 255]);
                    }
                }
            }
        }
    }
}

/// Paints a mask over `b` (top-level CSS px): neutral grey with a 1 px border.
pub(crate) fn cover(px: &mut Pixmap, b: Rect, area: Rect, scale: f32) {
    let to = |v: f32, o: f32| ((v - o) * scale).round() as i64;
    let r = [
        to(b.x, area.x),
        to(b.y, area.y),
        to(b.x + b.width, area.x),
        to(b.y + b.height, area.y),
    ];
    fill(px, r[0], r[1], r[2], r[3], [0x9e, 0x9e, 0x9e]);
    frame(px, r, 1, [0x61, 0x61, 0x61]);
}
