//! Paint on demand: the box tree is painted onto a CPU rasterizer (vello_cpu),
//! single-threaded and deterministic. Pixels come from the same
//! boxes as hit-testing and the views, so refs and pixels agree by construction.

use anyrender::ImageRenderer as _;
use pageon_dom::NodeId;

use crate::{Layout, Rect};

/// An RGBA8 image (straight alpha; painted pages are opaque).
#[derive(Clone, PartialEq, Eq)]
pub struct Pixmap {
    /// Width in device pixels.
    pub width: u32,
    /// Height in device pixels.
    pub height: u32,
    /// `width × height × 4` bytes, row by row.
    pub data: Vec<u8>,
}

impl std::fmt::Debug for Pixmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pixmap({}×{})", self.width, self.height)
    }
}

impl Pixmap {
    /// The pixel at `(x, y)` as `[r, g, b, a]`.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        self.data.get(i..i + 4).map(|p| [p[0], p[1], p[2], p[3]])
    }
}

/// Largest side of a painted image, in device pixels.
pub const MAX_PAINT_SIDE: u32 = 16_384;

impl Layout {
    /// The document's scrollable size in CSS px (at least the viewport).
    pub fn document_size(&self) -> (f32, f32) {
        let (vw, vh) = (self.viewport.width as f32, self.viewport.height as f32);
        let Some(root) = crate::boxes::paint::root_element(&self.tree) else {
            return (vw, vh);
        };
        let l = self.tree[root].final_layout();
        let w = l.size.width.max(l.scrollable_overflow_rect.right);
        let h = l.size.height.max(l.scrollable_overflow_rect.bottom);
        (w.max(vw).ceil(), h.max(vh).ceil())
    }

    /// Paints `area` (document CSS px) at `scale` device pixels per CSS px, over white.
    ///
    /// Fails if layout failed, if the area is empty, or if a side exceeds
    /// [`MAX_PAINT_SIDE`] device pixels.
    pub fn paint(&mut self, area: Rect, scale: f32) -> Result<Pixmap, String> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let width = (area.width * scale).round() as u32;
        let height = (area.height * scale).round() as u32;
        if width == 0 || height == 0 {
            return Err("the area to paint is empty".into());
        }
        if width > MAX_PAINT_SIDE || height > MAX_PAINT_SIDE {
            return Err(format!(
                "{width}×{height} px exceeds the {MAX_PAINT_SIDE} px limit"
            ));
        }
        self.paint_tree();
        // The painter reads the viewport scroll from the document's box.
        let root = self.tree.root();
        let saved = self.tree[root].layout.scroll;
        self.tree[root].layout.scroll = kurbo::Vec2::new(f64::from(area.x), f64::from(area.y));
        let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(width, height);
        let mut data = Vec::new();
        let tree = &self.tree;
        // The painter culls with unscaled boxes, so the page is painted at scale 1 (in CSS px)
        // and the recorded scene is scaled as a whole.
        let (css_w, css_h) = (area.width.ceil() as u32, area.height.ceil() as u32);
        let painted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            renderer.render_to_vec(
                |scene| {
                    if (scale - 1.0).abs() < f32::EPSILON {
                        crate::boxes::paint::paint_scene(scene, tree, 1.0, css_w, css_h, 0, 0);
                    } else {
                        let mut recorded = anyrender::Scene::new();
                        crate::boxes::paint::paint_scene(
                            &mut recorded,
                            tree,
                            1.0,
                            css_w,
                            css_h,
                            0,
                            0,
                        );
                        anyrender::PaintScene::append_scene(
                            scene,
                            recorded,
                            kurbo::Affine::scale(f64::from(scale)),
                        );
                    }
                },
                &mut data,
            );
        }));
        self.tree[root].layout.scroll = saved;
        if painted.is_err() {
            return Err("the painter failed on this page".into());
        }
        // Premultiplied RGBA over white.
        for p in data.as_chunks_mut::<4>().0 {
            let a = 255 - p[3];
            p[0] = p[0].saturating_add(a);
            p[1] = p[1].saturating_add(a);
            p[2] = p[2].saturating_add(a);
            p[3] = 255;
        }
        Ok(Pixmap {
            width,
            height,
            data,
        })
    }

    /// Embeds `child` (a frame's document, laid out at the iframe's content-box size) into
    /// the `<iframe>` element `iframe`, so it is painted inside it, in stacking order.
    pub fn embed(&mut self, iframe: NodeId, child: Layout) -> bool {
        let Some(b) = self.bx(iframe) else {
            return false;
        };
        if !self.tree[b].is_element() || child.error.is_some() {
            return false;
        }
        self.tree[b].special = crate::boxes::tree::Special::SubDocument(Box::new(child));
        true
    }

    /// Characters laid out with no glyph (the bundled fonts lack them: CJK, emoji): they paint
    /// as nothing.
    pub fn missing_glyphs(&self) -> usize {
        let mut n = 0;
        for text in self
            .tree
            .nodes
            .iter()
            .filter_map(|b| b.inline_layout.as_ref())
        {
            for line in text.layout.lines() {
                for item in line.items() {
                    if let parley::PositionedLayoutItem::GlyphRun(run) = item {
                        n += run.glyphs().filter(|g| g.id == 0).count();
                    }
                }
            }
        }
        n
    }

    /// The size of an element's content box (for laying out the document of an iframe).
    pub fn content_size(&self, id: NodeId) -> Option<(f32, f32)> {
        let node = &self.tree[self.bx(id)?];
        if !node.is_element() {
            return None;
        }
        let l = node.final_layout();
        let w = l.size.width - l.border.left - l.border.right - l.padding.left - l.padding.right;
        let h = l.size.height - l.border.top - l.border.bottom - l.padding.top - l.padding.bottom;
        (w > 0.0 && h > 0.0).then_some((w, h))
    }
}
