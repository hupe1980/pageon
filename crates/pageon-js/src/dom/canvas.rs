//! Canvas 2D: the drawing subset pages use (rects, paths, arcs, fill, stroke, transforms,
//! styles), rasterized for real with tiny-skia. Text, images, gradients and pixel readback are
//! absent, and screenshots do not show canvases yet (their notes say so).

use std::collections::HashMap;

use pageon_dom::NodeId;
use tiny_skia::{
    BlendMode, Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Shader, Stroke, Transform,
};

use super::gone;
use crate::cv::{self, Cx, Error, Obj, R, Value};
use crate::generated::Iface;
use crate::realm::Slot;

/// The drawing state (`save`/`restore` stack entry).
#[derive(Debug, Clone)]
pub struct DrawState {
    fill: (Color, String),
    stroke: (Color, String),
    line_width: f32,
    alpha: f32,
    composite: (BlendMode, String),
    transform: Transform,
    font: pageon_layout::CanvasFont,
    text_align: &'static str,
    text_baseline: &'static str,
}

impl Default for DrawState {
    fn default() -> Self {
        Self {
            fill: (Color::BLACK, "#000000".into()),
            stroke: (Color::BLACK, "#000000".into()),
            line_width: 1.0,
            alpha: 1.0,
            composite: (BlendMode::SourceOver, "source-over".into()),
            transform: Transform::identity(),
            font: pageon_layout::CanvasFont::default(),
            text_align: "start",
            text_baseline: "alphabetic",
        }
    }
}

/// A canvas element's bitmap and context state.
#[derive(Debug)]
pub struct Canvas {
    pixmap: Option<Pixmap>,
    path: PathBuilder,
    state: DrawState,
    stack: Vec<DrawState>,
}

impl Canvas {
    /// The bitmap: width, height and premultiplied RGBA bytes.
    pub fn bitmap(&self) -> Option<(u32, u32, &[u8])> {
        self.pixmap
            .as_ref()
            .map(|p| (p.width(), p.height(), p.data()))
    }

    fn new(w: u32, h: u32) -> Self {
        Self {
            pixmap: Pixmap::new(w.max(1), h.max(1)),
            path: PathBuilder::new(),
            state: DrawState::default(),
            stack: Vec::new(),
        }
    }
}

/// Canvases of a realm, by element.
pub type Canvases = HashMap<NodeId, Canvas>;

fn size(cx: &Cx<'_, '_, '_>, el: NodeId) -> (u32, u32) {
    let dom = cx.dom();
    let n = dom.get(el);
    let dim = |a: &str, d: u32| {
        n.and_then(|x| x.attr(a))
            .and_then(cv::parse_int)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(d)
    };
    (dim("width", 300), dim("height", 150))
}

fn element(cx: &Cx<'_, '_, '_>, this: Obj) -> R<NodeId> {
    match cx.realm.st.borrow().slot(this.slab()?) {
        Some(Slot::Canvas(n)) => Ok(*n),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn with<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&mut Canvas) -> T) -> R<T> {
    let el = element(cx, this)?;
    let (w, h) = size(cx, el);
    let mut st = cx.realm.st.borrow_mut();
    let c = st.canvases.entry(el).or_insert_with(|| Canvas::new(w, h));
    Ok(f(c))
}

fn paint(color: Color, alpha: f32, blend: BlendMode) -> Paint<'static> {
    let mut c = color;
    c.apply_opacity(alpha);
    Paint {
        shader: Shader::SolidColor(c),
        blend_mode: blend,
        anti_alias: true,
        ..Paint::default()
    }
}

fn finite(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

/// `HTMLCanvasElement`.
pub mod html_canvas_element {
    use super::*;

    pub fn get_width(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(size(cx, this.node()?).0)
    }

    pub fn set_width(cx: &mut Cx<'_, '_, '_>, this: Obj, v: u32) -> R<()> {
        let el = this.node()?;
        cx.dom_mut()
            .set_attr(el, "width", &v.to_string())
            .map_err(|_| gone())?;
        cx.st().canvases.remove(&el); // resizing clears the bitmap and state
        Ok(())
    }

    pub fn get_height(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(size(cx, this.node()?).1)
    }

    pub fn set_height(cx: &mut Cx<'_, '_, '_>, this: Obj, v: u32) -> R<()> {
        let el = this.node()?;
        cx.dom_mut()
            .set_attr(el, "height", &v.to_string())
            .map_err(|_| gone())?;
        cx.st().canvases.remove(&el);
        Ok(())
    }

    /// `"2d"` gives the context; other context types are not supported (`null`, as the spec
    /// allows for unsupported types).
    pub fn get_context(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        id: String,
        _options: Value,
    ) -> R<Option<Obj>> {
        let el = this.node()?;
        if id != "2d" {
            return Ok(None);
        }
        Ok(Some(super::super::same(
            cx,
            el,
            "context2d",
            Iface::CanvasRenderingContext2D,
            Slot::Canvas(el),
        )))
    }
}

/// `CanvasRenderingContext2D`.
pub mod canvas_rendering_context2_d {
    use super::*;

    pub fn get_canvas(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(element(cx, this)?))
    }
}

/// `CanvasState`.
pub mod canvas_state {
    use super::*;

    pub fn save(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        with(cx, this, |c| c.stack.push(c.state.clone()))
    }

    pub fn restore(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        with(cx, this, |c| {
            if let Some(s) = c.stack.pop() {
                c.state = s;
            }
        })
    }
}

/// `CanvasTransform`.
pub mod canvas_transform {
    use super::*;

    fn apply(cx: &Cx<'_, '_, '_>, this: Obj, t: Transform) -> R<()> {
        with(cx, this, |c| {
            c.state.transform = c.state.transform.pre_concat(t)
        })
    }

    pub fn scale(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64) -> R<()> {
        if !finite(&[x, y]) {
            return Ok(());
        }
        apply(cx, this, Transform::from_scale(x as f32, y as f32))
    }

    pub fn rotate(cx: &mut Cx<'_, '_, '_>, this: Obj, angle: f64) -> R<()> {
        if !angle.is_finite() {
            return Ok(());
        }
        apply(cx, this, Transform::from_rotate(angle.to_degrees() as f32))
    }

    pub fn translate(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64) -> R<()> {
        if !finite(&[x, y]) {
            return Ok(());
        }
        apply(cx, this, Transform::from_translate(x as f32, y as f32))
    }

    // The arity is the IDL's (`transform(a, b, c, d, e, f)`).
    #[allow(clippy::many_single_char_names, clippy::too_many_arguments)]
    pub fn transform(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        a: f64,
        b: f64,
        c: f64,
        d: f64,
        e: f64,
        f: f64,
    ) -> R<()> {
        if !finite(&[a, b, c, d, e, f]) {
            return Ok(());
        }
        apply(
            cx,
            this,
            Transform::from_row(a as f32, b as f32, c as f32, d as f32, e as f32, f as f32),
        )
    }

    pub fn reset_transform(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        with(cx, this, |c| c.state.transform = Transform::identity())
    }
}

/// `CanvasCompositing`.
pub mod canvas_compositing {
    use super::*;

    pub fn get_global_alpha(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        with(cx, this, |c| f64::from(c.state.alpha))
    }

    pub fn set_global_alpha(cx: &mut Cx<'_, '_, '_>, this: Obj, v: f64) -> R<()> {
        if !(0.0..=1.0).contains(&v) {
            return Ok(()); // out-of-range values are ignored
        }
        with(cx, this, |c| c.state.alpha = v as f32)
    }

    pub fn get_global_composite_operation(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |c| c.state.composite.1.clone())
    }

    pub fn set_global_composite_operation(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let mode = match v.as_str() {
            "source-over" => BlendMode::SourceOver,
            "source-in" => BlendMode::SourceIn,
            "source-out" => BlendMode::SourceOut,
            "source-atop" => BlendMode::SourceAtop,
            "destination-over" => BlendMode::DestinationOver,
            "destination-in" => BlendMode::DestinationIn,
            "destination-out" => BlendMode::DestinationOut,
            "destination-atop" => BlendMode::DestinationAtop,
            "lighter" => BlendMode::Plus,
            "copy" => BlendMode::Source,
            "xor" => BlendMode::Xor,
            "multiply" => BlendMode::Multiply,
            "screen" => BlendMode::Screen,
            "overlay" => BlendMode::Overlay,
            "darken" => BlendMode::Darken,
            "lighten" => BlendMode::Lighten,
            "difference" => BlendMode::Difference,
            "exclusion" => BlendMode::Exclusion,
            _ => return Ok(()), // unknown values are ignored
        };
        with(cx, this, |c| c.state.composite = (mode, v))
    }
}

/// `CanvasFillStrokeStyles` (colors; gradients and patterns are absent).
pub mod canvas_fill_stroke_styles {
    use super::*;

    fn style(cx: &mut Cx<'_, '_, '_>, v: &Value) -> Option<(Color, String)> {
        let l = cx.local(v);
        if !l.is_string() {
            return None;
        }
        let s = cv::string(cx, l).ok()?;
        parse_color(&s).map(|c| (c, s))
    }

    pub fn get_fill_style(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |c| c.state.fill.1.clone())
    }

    pub fn set_fill_style(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Value) -> R<()> {
        if let Some(st) = style(cx, &v) {
            with(cx, this, |c| c.state.fill = st)?;
        }
        Ok(())
    }

    pub fn get_stroke_style(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |c| c.state.stroke.1.clone())
    }

    pub fn set_stroke_style(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Value) -> R<()> {
        if let Some(st) = style(cx, &v) {
            with(cx, this, |c| c.state.stroke = st)?;
        }
        Ok(())
    }
}

/// `CanvasRect`.
pub mod canvas_rect {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Option<Rect> {
        if !finite(&[x, y, w, h]) {
            return None;
        }
        let (x0, x1) = if w < 0.0 { (x + w, x) } else { (x, x + w) };
        let (y0, y1) = if h < 0.0 { (y + h, y) } else { (y, y + h) };
        Rect::from_ltrb(x0 as f32, y0 as f32, x1 as f32, y1 as f32)
    }

    pub fn clear_rect(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64, w: f64, h: f64) -> R<()> {
        let Some(r) = rect(x, y, w, h) else {
            return Ok(());
        };
        with(cx, this, |c| {
            let p = paint(Color::TRANSPARENT, 1.0, BlendMode::Clear);
            if let Some(pm) = c.pixmap.as_mut() {
                pm.fill_rect(r, &p, c.state.transform, None);
            }
        })
    }

    pub fn fill_rect(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64, w: f64, h: f64) -> R<()> {
        let Some(r) = rect(x, y, w, h) else {
            return Ok(());
        };
        with(cx, this, |c| {
            let p = paint(c.state.fill.0, c.state.alpha, c.state.composite.0);
            if let Some(pm) = c.pixmap.as_mut() {
                pm.fill_rect(r, &p, c.state.transform, None);
            }
        })
    }

    pub fn stroke_rect(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    ) -> R<()> {
        let Some(r) = rect(x, y, w, h) else {
            return Ok(());
        };
        with(cx, this, |c| {
            let p = paint(c.state.stroke.0, c.state.alpha, c.state.composite.0);
            let path = PathBuilder::from_rect(r);
            let stroke = Stroke {
                width: c.state.line_width,
                ..Stroke::default()
            };
            if let Some(pm) = c.pixmap.as_mut() {
                pm.stroke_path(&path, &p, &stroke, c.state.transform, None);
            }
        })
    }
}

/// `CanvasDrawPath`.
pub mod canvas_draw_path {
    use super::*;

    pub fn begin_path(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        with(cx, this, |c| c.path = PathBuilder::new())
    }

    pub fn fill(cx: &mut Cx<'_, '_, '_>, this: Obj, rule: String) -> R<()> {
        let rule = if rule == "evenodd" {
            FillRule::EvenOdd
        } else {
            FillRule::Winding
        };
        with(cx, this, |c| {
            let p = paint(c.state.fill.0, c.state.alpha, c.state.composite.0);
            if let (Some(path), Some(pm)) = (c.path.clone().finish(), c.pixmap.as_mut()) {
                pm.fill_path(&path, &p, rule, Transform::identity(), None);
            }
        })
    }

    pub fn stroke(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        with(cx, this, |c| {
            let p = paint(c.state.stroke.0, c.state.alpha, c.state.composite.0);
            let stroke = Stroke {
                width: c.state.line_width,
                ..Stroke::default()
            };
            if let (Some(path), Some(pm)) = (c.path.clone().finish(), c.pixmap.as_mut()) {
                pm.stroke_path(&path, &p, &stroke, Transform::identity(), None);
            }
        })
    }
}

/// `CanvasPathDrawingStyles`.
pub mod canvas_path_drawing_styles {
    use super::*;

    pub fn get_line_width(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        with(cx, this, |c| f64::from(c.state.line_width))
    }

    pub fn set_line_width(cx: &mut Cx<'_, '_, '_>, this: Obj, v: f64) -> R<()> {
        if !v.is_finite() || v <= 0.0 {
            return Ok(());
        }
        with(cx, this, |c| c.state.line_width = v as f32)
    }
}

/// `CanvasPath`: points are transformed when added, as the spec requires.
pub mod canvas_path {
    use super::*;

    fn pt(t: Transform, x: f64, y: f64) -> (f32, f32) {
        let mut p = [tiny_skia::Point::from_xy(x as f32, y as f32)];
        t.map_points(&mut p);
        (p[0].x, p[0].y)
    }

    pub fn close_path(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        with(cx, this, |c| c.path.close())
    }

    pub fn move_to(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64) -> R<()> {
        if !finite(&[x, y]) {
            return Ok(());
        }
        with(cx, this, |c| {
            let (x, y) = pt(c.state.transform, x, y);
            c.path.move_to(x, y);
        })
    }

    pub fn line_to(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64) -> R<()> {
        if !finite(&[x, y]) {
            return Ok(());
        }
        with(cx, this, |c| {
            let (x, y) = pt(c.state.transform, x, y);
            if c.path.is_empty() {
                c.path.move_to(x, y);
            } else {
                c.path.line_to(x, y);
            }
        })
    }

    pub fn quadratic_curve_to(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        cpx: f64,
        cpy: f64,
        x: f64,
        y: f64,
    ) -> R<()> {
        if !finite(&[cpx, cpy, x, y]) {
            return Ok(());
        }
        with(cx, this, |c| {
            let (a, b) = pt(c.state.transform, cpx, cpy);
            let (x, y) = pt(c.state.transform, x, y);
            if c.path.is_empty() {
                c.path.move_to(a, b);
            }
            c.path.quad_to(a, b, x, y);
        })
    }

    // The arity is the IDL's.
    #[allow(clippy::many_single_char_names, clippy::too_many_arguments)]
    pub fn bezier_curve_to(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        a: f64,
        b: f64,
        c2: f64,
        d: f64,
        x: f64,
        y: f64,
    ) -> R<()> {
        if !finite(&[a, b, c2, d, x, y]) {
            return Ok(());
        }
        with(cx, this, |c| {
            let t = c.state.transform;
            let (p1x, p1y) = pt(t, a, b);
            let (p2x, p2y) = pt(t, c2, d);
            let (x, y) = pt(t, x, y);
            if c.path.is_empty() {
                c.path.move_to(p1x, p1y);
            }
            c.path.cubic_to(p1x, p1y, p2x, p2y, x, y);
        })
    }

    pub fn rect(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64, w: f64, h: f64) -> R<()> {
        if !finite(&[x, y, w, h]) {
            return Ok(());
        }
        with(cx, this, |c| {
            let t = c.state.transform;
            let corners = [
                pt(t, x, y),
                pt(t, x + w, y),
                pt(t, x + w, y + h),
                pt(t, x, y + h),
            ];
            c.path.move_to(corners[0].0, corners[0].1);
            for p in &corners[1..] {
                c.path.line_to(p.0, p.1);
            }
            c.path.close();
        })
    }

    /// An arc as cubic Bézier segments (at most a quarter turn each).
    #[allow(clippy::too_many_arguments)]
    pub fn arc(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        x: f64,
        y: f64,
        r: f64,
        start: f64,
        end: f64,
        ccw: bool,
    ) -> R<()> {
        if !finite(&[x, y, r, start, end]) {
            return Ok(());
        }
        if r < 0.0 {
            return Err(Error::dom(
                "IndexSizeError",
                format!("The radius provided ({r}) is negative."),
            ));
        }
        let tau = std::f64::consts::TAU;
        let mut sweep = end - start;
        if ccw {
            if sweep > 0.0 {
                sweep = (sweep % tau) - tau
            } else if sweep <= -tau {
                sweep = -tau
            }
        } else if sweep < 0.0 {
            sweep = (sweep % tau) + tau;
        } else if sweep >= tau {
            sweep = tau;
        }
        with(cx, this, |c| {
            let t = c.state.transform;
            let segments = (sweep.abs() / (tau / 4.0)).ceil().max(1.0) as usize;
            let step = sweep / segments as f64;
            let k = 4.0 / 3.0 * (step / 4.0).tan();
            let at = |a: f64| (x + r * a.cos(), y + r * a.sin());
            let (sx, sy) = at(start);
            let (px, py) = pt(t, sx, sy);
            if c.path.is_empty() {
                c.path.move_to(px, py);
            } else {
                c.path.line_to(px, py);
            }
            let mut a0 = start;
            for _ in 0..segments {
                let a1 = a0 + step;
                let (x0, y0) = at(a0);
                let (x1, y1) = at(a1);
                let c1 = pt(t, x0 - k * r * a0.sin(), y0 + k * r * a0.cos());
                let c2 = pt(t, x1 + k * r * a1.sin(), y1 - k * r * a1.cos());
                let e = pt(t, x1, y1);
                c.path.cubic_to(c1.0, c1.1, c2.0, c2.1, e.0, e.1);
                a0 = a1;
            }
        })
    }
}

/// A CSS color (hex, `rgb()`/`rgba()`, `hsl()`/`hsla()`, named, `transparent`).
pub(crate) fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim().to_ascii_lowercase();
    if let Some(hex) = s.strip_prefix('#') {
        let d = |i: usize, n: usize| u8::from_str_radix(&hex[i..i + n], 16).ok();
        let dup = |v: u8| v * 17;
        return match hex.len() {
            3 => Some(Color::from_rgba8(
                dup(d(0, 1)?),
                dup(d(1, 1)?),
                dup(d(2, 1)?),
                255,
            )),
            4 => Some(Color::from_rgba8(
                dup(d(0, 1)?),
                dup(d(1, 1)?),
                dup(d(2, 1)?),
                dup(d(3, 1)?),
            )),
            6 => Some(Color::from_rgba8(d(0, 2)?, d(2, 2)?, d(4, 2)?, 255)),
            8 => Some(Color::from_rgba8(d(0, 2)?, d(2, 2)?, d(4, 2)?, d(6, 2)?)),
            _ => None,
        };
    }
    if let Some((f, rest)) = s.split_once('(') {
        let args: Vec<&str> = rest
            .trim_end_matches(')')
            .split([',', ' ', '/'])
            .filter(|a| !a.is_empty())
            .collect();
        let num = |a: &str, scale: f32| -> Option<f32> {
            match a.strip_suffix('%') {
                Some(p) => p.parse::<f32>().ok().map(|v| v / 100.0 * scale),
                None => a.parse::<f32>().ok(),
            }
        };
        let alpha = args
            .get(3)
            .and_then(|a| num(a, 1.0))
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        return match f.trim() {
            "rgb" | "rgba" if args.len() >= 3 => {
                let c = |i: usize| num(args[i], 255.0).map(|v| v.clamp(0.0, 255.0) / 255.0);
                Color::from_rgba(c(0)?, c(1)?, c(2)?, alpha)
            }
            "hsl" | "hsla" if args.len() >= 3 => {
                let h = args[0]
                    .trim_end_matches("deg")
                    .parse::<f32>()
                    .ok()?
                    .rem_euclid(360.0)
                    / 360.0;
                let sat = num(args[1], 1.0)?.clamp(0.0, 1.0);
                let l = num(args[2], 1.0)?.clamp(0.0, 1.0);
                let q = if l < 0.5 {
                    l * (1.0 + sat)
                } else {
                    l + sat - l * sat
                };
                let p = 2.0 * l - q;
                let ch = |t: f32| {
                    let t = t.rem_euclid(1.0);
                    if t < 1.0 / 6.0 {
                        p + (q - p) * 6.0 * t
                    } else if t < 0.5 {
                        q
                    } else if t < 2.0 / 3.0 {
                        p + (q - p) * (2.0 / 3.0 - t) * 6.0
                    } else {
                        p
                    }
                };
                Color::from_rgba(ch(h + 1.0 / 3.0), ch(h), ch(h - 1.0 / 3.0), alpha)
            }
            _ => None,
        };
    }
    if s == "transparent" {
        return Some(Color::TRANSPARENT);
    }
    NAMED
        .iter()
        .find(|(n, _)| *n == s)
        .map(|(_, v)| Color::from_rgba8((v >> 16) as u8, (v >> 8) as u8, *v as u8, 255))
}

/// CSS named colors.
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

/// `CanvasTextDrawingStyles`: the font and how text sits on its point.
pub mod canvas_text_drawing_styles {
    use super::*;

    pub fn get_font(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |c| c.state.font.css())
    }

    /// An invalid font is ignored, as in browsers.
    pub fn set_font(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let Some(f) = pageon_layout::CanvasFont::parse(&v) else {
            return Ok(());
        };
        with(cx, this, |c| c.state.font = f)
    }

    pub fn get_text_align(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |c| c.state.text_align.to_string())
    }

    pub fn set_text_align(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        const ALL: &[&str] = &["start", "end", "left", "right", "center"];
        let Some(a) = ALL.iter().find(|a| **a == v) else {
            return Ok(());
        };
        with(cx, this, |c| c.state.text_align = a)
    }

    pub fn get_text_baseline(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |c| c.state.text_baseline.to_string())
    }

    pub fn set_text_baseline(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        const ALL: &[&str] = &[
            "top",
            "hanging",
            "middle",
            "alphabetic",
            "ideographic",
            "bottom",
        ];
        let Some(b) = ALL.iter().find(|b| **b == v) else {
            return Ok(());
        };
        with(cx, this, |c| c.state.text_baseline = b)
    }
}

/// `CanvasText`: measuring and drawing text with the bundled fonts.
pub mod canvas_text {
    use super::*;
    use pageon_layout::Segment;

    pub fn measure_text(cx: &mut Cx<'_, '_, '_>, this: Obj, text: String) -> R<Obj> {
        let font = with(cx, this, |c| c.state.font.clone())?;
        let (m, _) = pageon_layout::canvas_text(&font, &text, false);
        Ok(cx.alloc(Iface::TextMetrics, Slot::TextMetrics(m)))
    }

    /// Where the text's origin goes for its alignment and baseline: x and y shifts.
    fn anchor(state: &DrawState, m: &pageon_layout::CanvasTextMetrics) -> (f32, f32) {
        let dx = match state.text_align {
            "center" => -m.width / 2.0,
            "end" | "right" => -m.width,
            _ => 0.0,
        };
        let dy = match state.text_baseline {
            "top" | "hanging" => m.font_ascent,
            "middle" => (m.font_ascent - m.font_descent) / 2.0,
            "bottom" | "ideographic" => -m.font_descent,
            _ => 0.0,
        };
        (dx, dy)
    }

    fn draw(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        text: &str,
        x: f64,
        y: f64,
        max: Option<f64>,
        fill: bool,
    ) -> R<()> {
        if !finite(&[x, y]) || max.is_some_and(|m| !m.is_finite() || m <= 0.0) {
            return Ok(());
        }
        let font = with(cx, this, |c| c.state.font.clone())?;
        let (m, segments) = pageon_layout::canvas_text(&font, text, true);
        with(cx, this, |c| {
            let (dx, dy) = anchor(&c.state, &m);
            // `maxWidth`: squeeze horizontally to fit.
            let sx = max.map_or(1.0, |mw| (mw as f32 / m.width).min(1.0));
            let t = c
                .state
                .transform
                .pre_translate(x as f32, y as f32)
                .pre_scale(sx, 1.0);
            let pt = |px: f32, py: f32| {
                let mut p = [tiny_skia::Point::from_xy(px + dx, py + dy)];
                t.map_points(&mut p);
                (p[0].x, p[0].y)
            };
            let mut b = PathBuilder::new();
            for s in &segments {
                match *s {
                    Segment::Move(a, bb) => {
                        let (x, y) = pt(a, bb);
                        b.move_to(x, y);
                    }
                    Segment::Line(a, bb) => {
                        let (x, y) = pt(a, bb);
                        b.line_to(x, y);
                    }
                    Segment::Quad(a, bb, c2, d) => {
                        let (x1, y1) = pt(a, bb);
                        let (x, y) = pt(c2, d);
                        b.quad_to(x1, y1, x, y);
                    }
                    Segment::Cubic(a, bb, c2, d, e, f) => {
                        let (x1, y1) = pt(a, bb);
                        let (x2, y2) = pt(c2, d);
                        let (x, y) = pt(e, f);
                        b.cubic_to(x1, y1, x2, y2, x, y);
                    }
                    Segment::Close => b.close(),
                }
            }
            let (Some(path), Some(pm)) = (b.finish(), c.pixmap.as_mut()) else {
                return;
            };
            if fill {
                let p = paint(c.state.fill.0, c.state.alpha, c.state.composite.0);
                pm.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
            } else {
                let p = paint(c.state.stroke.0, c.state.alpha, c.state.composite.0);
                let stroke = Stroke {
                    width: c.state.line_width,
                    ..Stroke::default()
                };
                pm.stroke_path(&path, &p, &stroke, Transform::identity(), None);
            }
        })
    }

    pub fn fill_text(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        text: String,
        x: f64,
        y: f64,
        max: Option<f64>,
    ) -> R<()> {
        draw(cx, this, &text, x, y, max, true)
    }

    pub fn stroke_text(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        text: String,
        x: f64,
        y: f64,
        max: Option<f64>,
    ) -> R<()> {
        draw(cx, this, &text, x, y, max, false)
    }
}

/// `TextMetrics`.
pub mod text_metrics {
    use super::*;

    fn m(cx: &Cx<'_, '_, '_>, this: Obj) -> R<pageon_layout::CanvasTextMetrics> {
        match crate::dom::collections::slot(cx, this)? {
            Slot::TextMetrics(m) => Ok(m),
            _ => Err(crate::cv::Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_width(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(f64::from(m(cx, this)?.width))
    }
    pub fn get_actual_bounding_box_left(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(f64::from(m(cx, this)?.actual_left))
    }
    pub fn get_actual_bounding_box_right(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(f64::from(m(cx, this)?.actual_right))
    }
    pub fn get_font_bounding_box_ascent(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(f64::from(m(cx, this)?.font_ascent))
    }
    pub fn get_font_bounding_box_descent(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(f64::from(m(cx, this)?.font_descent))
    }
    pub fn get_actual_bounding_box_ascent(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(f64::from(m(cx, this)?.actual_ascent))
    }
    pub fn get_actual_bounding_box_descent(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(f64::from(m(cx, this)?.actual_descent))
    }
    pub fn get_em_height_ascent(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(f64::from(m(cx, this)?.font_ascent))
    }
    pub fn get_em_height_descent(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(f64::from(m(cx, this)?.font_descent))
    }
    /// Text is measured from its alphabetic baseline.
    pub fn get_alphabetic_baseline(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        m(cx, this).map(|_| 0.0)
    }
}
