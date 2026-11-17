//! pageon's geometry.
//!
//! A document from `pageon-dom` is laid out at a viewport: CSS cascade (user agent, `<style>`,
//! `style` attributes and supplied stylesheets), block/inline/flex/grid/float/table layout and
//! text measured with **bundled** fonts — never host fonts — so boxes are identical on every
//! machine and close to Chromium's. Results are keyed by the document model's own handles.
//!
//! ```
//! use pageon_dom::{ParseOptions, parse_document};
//! use pageon_layout::{Layout, Viewport, Visibility};
//!
//! let dom = parse_document(r#"<p id="p">Hello</p><p style="display:none">x</p>"#, ParseOptions::default());
//! let layout = Layout::new(&dom, Viewport::default(), &[], None);
//! let p = dom.traverse(dom.root()).find_map(|e| match e {
//!     pageon_dom::Edge::Enter(id) if dom.get(id).and_then(|n| n.attr("id")) == Some("p") => Some(id),
//!     _ => None,
//! }).unwrap();
//! assert_eq!(layout.visibility(p), Visibility::Visible);
//! assert!(layout.box_of(p).unwrap().height > 0.0);
//! ```

mod canvas_text;
pub use canvas_text::{CanvasFont, CanvasTextMetrics, Segment, canvas_text};
mod actionability;
mod caret;
mod fonts;
mod images;
mod media;
mod mirror;
mod paint;
pub mod scope;

use blitz_dom::BaseDocument;
use pageon_dom::{Dom, NodeId};

pub use actionability::Visibility;
pub use images::{Images, MAX_BYTES as MAX_IMAGE_BYTES, MAX_SIDE as MAX_IMAGE_SIDE};
pub use media::{css_supports, media_query};
pub use paint::Pixmap;

/// The viewport in CSS pixels (scroll position 0,0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
        }
    }
}

/// A box in CSS pixels, document coordinates.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// Area in square CSS px.
    pub fn area(&self) -> f32 {
        self.width.max(0.0) * self.height.max(0.0)
    }

    /// Intersection, if non-empty.
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.width).min(o.x + o.width);
        let y1 = (self.y + self.height).min(o.y + o.height);
        (x1 > x0 && y1 > y0).then_some(Rect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
}

/// An author stylesheet supplied by the loader: the `<link>` node it belongs to and its text
/// (shared, and hashed once: layouts look a sheet up by it many times).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stylesheet {
    /// The `<link rel=stylesheet>` element.
    pub link: NodeId,
    css: std::sync::Arc<str>,
    hash: u64,
}

impl Stylesheet {
    /// The sheet of a `<link>`.
    pub fn new(link: NodeId, css: impl Into<std::sync::Arc<str>>) -> Self {
        let css = css.into();
        let hash = scope::fast_hash(&css);
        Self { link, css, hash }
    }

    /// Its CSS text.
    pub fn css(&self) -> &str {
        &self.css
    }
}

/// What a layout takes besides the document: the top layer, images and redactions.
#[derive(Debug, Clone, Copy, Default)]
pub struct LayoutOptions<'a> {
    /// Element scroll offsets (CSS px) to apply; each is clamped to the element's range.
    pub scroll: &'a [(NodeId, f64, f64)],
    /// Modal dialogs (opened by script) in the top layer.
    pub modals: &'a [NodeId],
    /// Images: known ones take part in layout with their natural sizes, and
    /// [`Layout::wanted_images`] lists the image URLs the document uses that are not known
    /// yet. `None`: no image is asked for.
    pub images: Option<&'a Images>,
    /// Strings drawn as bullets wherever they appear in text or values (secrets typed into
    /// the page), so they never reach pixels.
    pub redact: &'a [String],
    /// Custom elements whose definition ran (`:defined`); other custom elements are undefined.
    pub defined: &'a [NodeId],
    /// Compute styles only, no boxes: enough for computed style values that do not depend on
    /// layout ([`Layout::style`]); boxes need a full layout ([`Layout::has_boxes`]).
    pub styles_only: bool,
}

/// A laid-out document.
pub struct Layout {
    doc: BaseDocument,
    map: mirror::Map,
    /// The document's stylesheets, kept for a later layout of the same page.
    sheets: Option<mirror::Sheets>,
    /// The base URL the document was laid out with.
    base: String,
    viewport: Viewport,
    /// The viewport's scroll offset (CSS px). Kept by pageon rather than blitz: blitz's
    /// geometry and hit-testing of fixed boxes under a viewport scroll are wrong, so blitz
    /// stays at 0,0 and pageon applies the offset (painting sets blitz's own while it runs).
    scroll: std::cell::Cell<(f32, f32)>,
    /// Elements with `position: fixed` (blitz ids): their boxes are relative to the viewport.
    fixed: std::collections::HashSet<blitz_dom::NodeId>,
    /// Elements that clip their content (found on first use): blitz id → what they clip.
    clippers:
        std::cell::OnceCell<std::collections::HashMap<blitz_dom::NodeId, actionability::Clip>>,
    error: Option<String>,
    images: Option<mirror::ImageUse>,
    /// Laid out (not only styled).
    boxes: bool,
}

impl std::fmt::Debug for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layout")
            .field("viewport", &self.viewport)
            .finish_non_exhaustive()
    }
}

impl Layout {
    /// Styles and lays out `dom` at `viewport`, with extra author `stylesheets`; relative URLs in
    /// CSS resolve against `base_url` (`None`: `about:blank`).
    ///
    /// Never panics: a failure inside the layout engine yields an empty layout (every node
    /// reports *not rendered*) and [`Layout::error`] says why.
    pub fn new(
        dom: &Dom,
        viewport: Viewport,
        stylesheets: &[Stylesheet],
        base_url: Option<&str>,
    ) -> Self {
        Self::new_with(dom, viewport, stylesheets, base_url, &[])
    }

    /// [`Layout::new`] with modal dialogs (opened by script) in the top layer.
    pub fn new_with(
        dom: &Dom,
        viewport: Viewport,
        stylesheets: &[Stylesheet],
        base_url: Option<&str>,
        modals: &[NodeId],
    ) -> Self {
        let opts = LayoutOptions {
            modals,
            ..LayoutOptions::default()
        };
        Self::with_options(dom, viewport, stylesheets, base_url, &opts)
    }

    /// [`Layout::new`] with [`LayoutOptions`]: the top layer, images and redactions.
    pub fn with_options(
        dom: &Dom,
        viewport: Viewport,
        stylesheets: &[Stylesheet],
        base_url: Option<&str>,
        opts: &LayoutOptions<'_>,
    ) -> Self {
        Self::again(dom, viewport, stylesheets, base_url, opts, None)
    }

    /// [`Layout::with_options`], reusing `prev` (a layout of the same page) when it can: its
    /// tree is updated in place and its stylesheets stay parsed, so only what changed is
    /// restyled, as browsers keep a page's style data between layouts. The boxes are laid out
    /// in full; the result equals a fresh layout.
    pub fn again(
        dom: &Dom,
        viewport: Viewport,
        stylesheets: &[Stylesheet],
        base_url: Option<&str>,
        opts: &LayoutOptions<'_>,
        prev: Option<Layout>,
    ) -> Self {
        let base = base_url
            .filter(|u| {
                u.starts_with("http://") || u.starts_with("https://") || u.starts_with("file://")
            })
            .unwrap_or("about:blank");
        // Images are applied through the document's network hooks, made with it: a document
        // laid out with images is not reused.
        let reuse = prev
            .filter(|p| {
                p.error.is_none()
                    && p.viewport == viewport
                    && p.base == base
                    && p.images.is_none()
                    && opts.images.is_none()
            })
            .and_then(|p| p.sheets.map(|s| (p.doc, s, p.map)));
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Shadow trees render as the composed (flat) tree, same ids, styles scoped.
            if dom.has_shadow() {
                let composed = dom.composed();
                mirror::build(
                    &composed.dom,
                    viewport,
                    stylesheets,
                    base,
                    opts,
                    Some(&composed),
                    reuse,
                )
            } else {
                mirror::build(dom, viewport, stylesheets, base, opts, None, reuse)
            }
        }));
        match built {
            Ok((doc, map, images, sheets)) => Self {
                fixed: mirror::fixed(&doc, &map),
                clippers: std::cell::OnceCell::new(),
                doc,
                map,
                sheets: Some(sheets),
                base: base.to_string(),
                viewport,
                scroll: std::cell::Cell::default(),
                error: None,
                images,
                boxes: !opts.styles_only,
            },
            Err(e) => {
                let why = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "unknown".into());
                Self {
                    doc: mirror::empty(viewport),
                    map: mirror::Map::default(),
                    sheets: None,
                    base: base.to_string(),
                    viewport,
                    scroll: std::cell::Cell::default(),
                    fixed: std::collections::HashSet::new(),
                    clippers: std::cell::OnceCell::new(),
                    error: Some(format!("layout engine failed: {why}")),
                    images: None,
                    boxes: true,
                }
            }
        }
    }

    /// Whether this layout has boxes (it was not made with [`LayoutOptions::styles_only`]).
    pub fn has_boxes(&self) -> bool {
        self.boxes
    }

    /// Whether a computed style value of `property` depends on layout (a used value: sizes,
    /// margins, padding, insets, grid tracks, origins), so a styles-only layout cannot give it.
    pub fn needs_boxes(property: &str) -> bool {
        const USED: [&str; 12] = [
            "width",
            "height",
            "margin",
            "padding",
            "inset",
            "top",
            "right",
            "bottom",
            "left",
            "grid-template",
            "transform-origin",
            "perspective-origin",
        ];
        USED.iter().any(|p| property.starts_with(p))
            || property.ends_with("-size")
            || property.contains("width")
            || property.contains("height")
    }

    /// Why layout is unavailable, if the layout engine failed on this document.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Image URLs the document uses that the [`Images`] in [`LayoutOptions`] did not know (document order; empty without images).
    pub fn wanted_images(&self) -> &[String] {
        self.images.as_ref().map_or(&[], |u| u.wanted.as_slice())
    }

    /// How many known images this layout applied.
    pub fn applied_images(&self) -> usize {
        self.images.as_ref().map_or(0, |u| u.applied)
    }

    /// The viewport.
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// The border box of a rendered element in document coordinates (`None` for text and
    /// comments: text is measured through its element). A box fixed to the viewport moves with
    /// the viewport's scroll.
    pub fn box_of(&self, id: NodeId) -> Option<Rect> {
        let mut r = self.raw_box(id)?;
        if self.is_fixed(id) {
            let (sx, sy) = self.scroll.get();
            r.x += sx;
            r.y += sy;
        }
        Some(r)
    }

    /// The border box relative to the viewport, as scripts see it (`getBoundingClientRect`).
    pub fn client_rect(&self, id: NodeId) -> Option<Rect> {
        let mut r = self.box_of(id)?;
        let (sx, sy) = self.scroll.get();
        r.x -= sx;
        r.y -= sy;
        Some(r)
    }

    /// Whether the element or an ancestor has `position: fixed` (its box follows the viewport).
    pub fn is_fixed(&self, id: NodeId) -> bool {
        if self.fixed.is_empty() {
            return false;
        }
        let mut cur = self.map.blitz(id);
        while let Some(c) = cur {
            if self.fixed.contains(&c) {
                return true;
            }
            cur = self.doc.get_node(c).and_then(|n| n.parent);
        }
        false
    }

    /// The viewport's scroll offset (CSS px).
    pub fn viewport_scroll(&self) -> (f32, f32) {
        self.scroll.get()
    }

    /// The largest viewport scroll offset: the document size less the viewport.
    pub fn viewport_range(&self) -> (f32, f32) {
        let (w, h) = self.document_size();
        (
            (w - self.viewport.width as f32).max(0.0),
            (h - self.viewport.height as f32).max(0.0),
        )
    }

    /// Scrolls the viewport to `x, y` (clamped to its range) and returns the applied offset.
    /// Costs no layout.
    pub fn set_viewport_scroll(&self, x: f32, y: f32) -> (f32, f32) {
        let (mx, my) = self.viewport_range();
        let v = (x.clamp(0.0, mx), y.clamp(0.0, my));
        self.scroll.set(v);
        v
    }

    /// The largest scroll offset of a scroll container (`overflow` `auto`, `scroll` or `hidden`
    /// on either axis, or a `<textarea>`); `None` for other elements.
    pub fn scroll_range(&self, id: NodeId) -> Option<(f32, f32)> {
        let b = self.map.blitz(id)?;
        let node = self.doc.get_node(b)?;
        let el = node.element_data()?;
        let scrolls = |p: &str| {
            self.style(id, p)
                .is_some_and(|v| matches!(v.as_str(), "auto" | "scroll" | "hidden"))
        };
        if &*el.name.local != "textarea" && !scrolls("overflow-x") && !scrolls("overflow-y") {
            return None;
        }
        if let Some(max) = mirror::text_range(node) {
            return Some((0.0, max));
        }
        let l = node.final_layout();
        Some((l.scroll_width(), l.scroll_height()))
    }

    /// The scroll offset applied to an element (0,0 for elements that do not scroll).
    pub fn scroll_of(&self, id: NodeId) -> (f32, f32) {
        self.map
            .blitz(id)
            .and_then(|b| self.doc.get_node(b))
            .map_or((0.0, 0.0), |n| {
                if let Some(t) = n.element_data().and_then(|e| e.text_input_data())
                    && t.is_multiline
                {
                    return (0.0, t.scroll_offset);
                }
                let o = n.scroll_offset();
                (o.x as f32, o.y as f32)
            })
    }

    /// Whether an ancestor element is not rendered (`display: none`, or never styled).
    fn undisplayed_above(&self, node: &blitz_dom::Node) -> bool {
        std::iter::successors(node.parent.and_then(|p| self.doc.get_node(p)), |n| {
            n.parent.and_then(|p| self.doc.get_node(p))
        })
        .filter(|n| n.element_data().is_some())
        .any(undisplayed)
    }

    fn raw_box(&self, id: NodeId) -> Option<Rect> {
        let b = self.map.blitz(id)?;
        // Blitz keeps layout data on elements only (and panics when asked for anything else).
        let node = self.doc.get_node(b)?;
        node.element_data()?;
        // Not rendered (`display: none` on it or above): no box, as in Chromium.
        if undisplayed(node) || self.undisplayed_above(node) {
            return Some(Rect::default());
        }
        let r = self.doc.get_client_bounding_rect(b)?;
        // Blitz moves a scrolled element's own box by its scroll offset; only its content moves.
        let own = node.scroll_offset();
        let rect = Rect {
            x: r.x as f32 + own.x as f32,
            y: r.y as f32 + own.y as f32,
            width: r.width as f32,
            height: r.height as f32,
        };
        if rect.area() > 0.0 {
            return Some(rect);
        }
        // Blitz lays out table rows and row groups through their cells and leaves them without a
        // box: theirs is the union of their children's boxes, as in Chromium.
        let display = self.style(id, "display").unwrap_or_default();
        if matches!(
            display.as_str(),
            "table-row" | "table-row-group" | "table-header-group" | "table-footer-group"
        ) {
            let children: Vec<NodeId> = self
                .doc
                .get_node(b)?
                .children
                .iter()
                .filter_map(|&c| self.map.pageon(c))
                .collect();
            let union = children
                .into_iter()
                .filter_map(|c| self.raw_box(c))
                .filter(|c| c.area() > 0.0)
                .reduce(|a, c| {
                    let (x0, y0) = (a.x.min(c.x), a.y.min(c.y));
                    let (x1, y1) = (
                        (a.x + a.width).max(c.x + c.width),
                        (a.y + a.height).max(c.y + c.height),
                    );
                    Rect {
                        x: x0,
                        y: y0,
                        width: x1 - x0,
                        height: y1 - y0,
                    }
                });
            if union.is_some() {
                return union;
            }
        }
        Some(rect)
    }

    /// The computed value of a CSS property (e.g. `display`, `opacity`, `cursor`).
    pub fn style(&self, id: NodeId, property: &str) -> Option<String> {
        let b = self.map.blitz(id)?;
        let node = self.doc.get_node(b)?;
        node.element_data()?;
        // Inside an undisplayed subtree there is no computed style (a kept layout may still hold
        // one from before): never ask Blitz to resolve those (its fallback path is not supported
        // with sequential styling).
        node.primary_styles()?;
        if self.undisplayed_above(node) {
            return None;
        }
        let v = self.doc.resolved_style_value(b, property);
        (!v.is_empty()).then_some(v)
    }
}

/// Whether an element is not rendered by its own style (`display: none`, or never styled).
fn undisplayed(n: &blitz_dom::Node) -> bool {
    n.primary_styles()
        .is_none_or(|s| s.clone_display().is_none())
}
