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
mod boxes;
mod caret;
mod fonts;
mod images;
mod media;
mod metrics;
mod paint;

use boxes::tree::{BoxId, Special, Tree};
use pageon_dom::{Dom, NodeId};

pub use actionability::Visibility;
pub use images::{Images, MAX_BYTES as MAX_IMAGE_BYTES, MAX_SIDE as MAX_IMAGE_SIDE};
pub use media::{css_supports, media_query};
pub use metrics::font_metrics;
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
        let hash = pageon_style::text_hash(&css);
        Self { link, css, hash }
    }

    /// Its CSS text.
    pub fn css(&self) -> &str {
        &self.css
    }

    /// A hash of its text (computed once).
    pub fn hash(&self) -> u64 {
        self.hash
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
    /// Styles computed on the document and kept up to date from its changes. Without them
    /// the layout styles the document itself (and keeps its styler for the next layout).
    pub styles: Option<&'a pageon_style::Styler>,
    /// Compute styles only, no boxes: enough for computed style values that do not depend on
    /// layout ([`Layout::style`]); boxes need a full layout ([`Layout::has_boxes`]).
    pub styles_only: bool,
}

/// The document's author sheets in cascade order (`<style>`, loaded `<link>`, adopted), each
/// with its [`pageon_style::text_hash`].
pub fn author_sheets(dom: &Dom, loaded: &[Stylesheet]) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    for e in dom.traverse(dom.root()) {
        let pageon_dom::Edge::Enter(id) = e else {
            continue;
        };
        let Some(n) = dom.get(id) else { continue };
        if n.is_html("style") {
            let text: String = dom.cssom_text(id).map_or_else(
                || {
                    dom.children(id)
                        .filter_map(|c| match dom.get(c).map(|n| &n.data) {
                            Some(pageon_dom::NodeData::Text(t)) => Some(t.as_str()),
                            _ => None,
                        })
                        .collect()
                },
                str::to_string,
            );
            out.push((pageon_style::text_hash(&text), text));
        } else if n.is_html("link") {
            if let Some(css) = dom.cssom_text(id) {
                out.push((pageon_style::text_hash(css), css.to_string()));
            } else if let Some(s) = loaded.iter().find(|s| s.link == id) {
                out.push((s.hash(), s.css().to_string()));
            }
        }
    }
    for css in dom.adopted(dom.root()) {
        out.push((pageon_style::text_hash(css), css.clone()));
    }
    out
}

/// A styler a layout made itself (no styles given), kept for the next layout of the page.
struct Own {
    styler: pageon_style::Styler,
    viewport: Viewport,
    base: String,
}

/// A laid-out document.
pub struct Layout {
    tree: Tree,
    own: Option<Own>,
    viewport: Viewport,
    /// The viewport's scroll offset (CSS px). Boxes are laid out at 0,0; the offset applies
    /// to client coordinates, fixed boxes and painting.
    scroll: std::cell::Cell<(f32, f32)>,
    /// Boxes with `position: fixed`: they follow the viewport.
    fixed: std::collections::HashSet<BoxId>,
    /// Paint children, stacking contexts and transforms built (on first paint or hit test:
    /// most layouts only answer geometry).
    painted: std::cell::Cell<bool>,
    /// Boxes that clip their content (found on first use).
    clippers: std::cell::OnceCell<std::collections::HashMap<BoxId, actionability::Clip>>,
    error: Option<String>,
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
    /// Never panics: a failure inside layout yields an empty layout (every node reports *not
    /// rendered*) and [`Layout::error`] says why.
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

    /// [`Layout::with_options`], reusing `prev` (a layout of the same page): without
    /// [`LayoutOptions::styles`], its styler (parsed sheets) styles the document again.
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
            .unwrap_or("about:blank")
            .to_string();
        let mut own = None;
        let (recycle, prev_own) = match prev {
            Some(p) => (Some(p.tree), p.own),
            None => (None, None),
        };
        if opts.styles.is_none() {
            let mut kept = prev_own
                .filter(|o| o.viewport == viewport && o.base == base)
                .unwrap_or_else(|| Own {
                    styler: pageon_style::Styler::with_metrics(
                        viewport.width as f32,
                        viewport.height as f32,
                        &base,
                        font_metrics(),
                    ),
                    viewport,
                    base: base.clone(),
                });
            let sheets = author_sheets(dom, stylesheets);
            kept.styler.set_sheets(
                &sheets
                    .iter()
                    .map(|(h, s)| (*h, s.as_str()))
                    .collect::<Vec<_>>(),
            );
            let linked: Vec<(NodeId, &str)> =
                stylesheets.iter().map(|s| (s.link, s.css())).collect();
            kept.styler.restyle_all_with(
                dom,
                pageon_style::Page {
                    top_layer: opts.modals,
                    defined: opts.defined,
                    linked: &linked,
                },
            );
            own = Some(kept);
        }
        let styler = opts
            .styles
            .or(own.as_ref().map(|o| &o.styler))
            .unwrap_or_else(|| unreachable!("a styler was made above"));
        let build = boxes::tree::Build {
            recycle: std::cell::RefCell::new(recycle),
            styler,
            images: opts.images,
            redact: opts.redact,
            base: &base,
            viewport,
        };
        let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if opts.styles_only {
                return Tree::build(dom, &build);
            }
            let mut tree = boxes::lay_out(dom, &build);
            // Decoded images now: the URLs a page wants are known right after layout.
            boxes::paint::resolve_images(&mut tree);
            tree
        }));
        let (tree, error) = match built {
            Ok(t) => (t, None),
            Err(e) => {
                let why = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "unknown".into());
                (Tree::empty(), Some(format!("layout failed: {why}")))
            }
        };
        let fixed = tree
            .nodes
            .iter()
            .filter(|n| {
                n.is_element()
                    && n.primary_styles().is_some_and(|s| {
                        s.clone_position() == style::computed_values::position::T::Fixed
                    })
            })
            .map(|n| n.id)
            .collect();
        let mut l = Self {
            tree,
            own,
            viewport,
            scroll: std::cell::Cell::default(),
            fixed,
            painted: std::cell::Cell::new(false),
            clippers: std::cell::OnceCell::new(),
            error,
            boxes: !opts.styles_only,
        };
        if l.boxes {
            l.apply_scroll(opts.scroll);
        }
        l
    }

    /// The tree, ready to paint and hit test.
    fn paint_tree(&self) -> &Tree {
        if self.boxes && !self.painted.replace(true) {
            boxes::paint::prepare(&self.tree);
        }
        &self.tree
    }

    /// Applies element scroll offsets, each clamped to its element's range.
    fn apply_scroll(&mut self, offsets: &[(NodeId, f64, f64)]) {
        for &(id, x, y) in offsets {
            let Some((mx, my)) = self.scroll_range(id) else {
                continue;
            };
            let Some(&b) = self.tree.by_dom.get(&id) else {
                continue;
            };
            let (x, y) = ((x as f32).clamp(0.0, mx), (y as f32).clamp(0.0, my));
            let node = &mut self.tree.nodes[b];
            match &mut node.special {
                // A textarea scrolls its text.
                Special::TextInput(t) if t.multiline => t.scroll = y,
                _ => node.layout.scroll = kurbo::Vec2::new(f64::from(x), f64::from(y)),
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

    /// Why layout is unavailable, if it failed on this document.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Image URLs the document uses that the [`Images`] in [`LayoutOptions`] did not know
    /// (document order; empty without images).
    pub fn wanted_images(&self) -> &[String] {
        &self.tree.wanted
    }

    /// How many known images this layout applied.
    pub fn applied_images(&self) -> usize {
        self.tree.applied
    }

    /// The viewport.
    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    /// The box of a document node.
    fn bx(&self, id: NodeId) -> Option<BoxId> {
        self.tree.by_dom.get(&id).copied()
    }

    /// The border box of a rendered element in document coordinates (`None` for text and
    /// nodes that are not rendered; an element with `display: none` has an empty box). A box
    /// fixed to the viewport moves with the viewport's scroll.
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
        self.bx(id).is_some_and(|b| self.box_fixed(b))
    }

    fn box_fixed(&self, b: BoxId) -> bool {
        if self.fixed.is_empty() {
            return false;
        }
        std::iter::successors(Some(b), |&c| self.tree[c].parent).any(|c| self.fixed.contains(&c))
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
        let node = &self.tree[self.bx(id)?];
        let style = node.primary_styles()?;
        let scrolls = |o: style::values::computed::Overflow| {
            o.is_scrollable() || o == style::values::computed::Overflow::Hidden
        };
        let b = style.get_box();
        if let Special::TextInput(t) = &node.special
            && t.multiline
        {
            let text = t.layout.height();
            return Some((
                0.0,
                (text - node.final_layout().content_box_height()).max(0.0),
            ));
        }
        if !scrolls(b.overflow_x) && !scrolls(b.overflow_y) {
            return None;
        }
        let l = node.final_layout();
        Some((l.scroll_width(), l.scroll_height()))
    }

    /// The scroll offset applied to an element (0,0 for elements that do not scroll).
    pub fn scroll_of(&self, id: NodeId) -> (f32, f32) {
        let Some(b) = self.bx(id) else {
            return (0.0, 0.0);
        };
        let n = &self.tree[b];
        if let Special::TextInput(t) = &n.special
            && t.multiline
        {
            return (0.0, t.scroll);
        }
        (n.layout.scroll.x as f32, n.layout.scroll.y as f32)
    }

    fn raw_box(&self, id: NodeId) -> Option<Rect> {
        let b = self.bx(id)?;
        let node = &self.tree[b];
        if !node.is_element() {
            return None;
        }
        // Not rendered (`display: none`): an empty box, as in Chromium.
        if node.display_style().is_none_or(|d| d.is_none()) {
            return Some(Rect::default());
        }
        let rect = boxes::border_box(&self.tree, b)?;
        // A scrolled element's own box does not move; its content does.
        let own = node.scroll_offset();
        let rect = Rect {
            x: rect.x + own.x as f32,
            y: rect.y + own.y as f32,
            ..rect
        };
        if rect.area() > 0.0 {
            return Some(rect);
        }
        // Table rows and row groups are laid out through their cells: their box is the union
        // of their children's, as in Chromium.
        let display = self.style(id, "display").unwrap_or_default();
        if matches!(
            display.as_str(),
            "table-row" | "table-row-group" | "table-header-group" | "table-footer-group"
        ) {
            let union = node
                .children
                .iter()
                .filter_map(|&c| self.tree[c].dom)
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

    /// The resolved value of a CSS property (`getComputedStyle`: computed, or used for sizes
    /// and positions), e.g. `display`, `opacity`, `width`.
    pub fn style(&self, id: NodeId, property: &str) -> Option<String> {
        let b = self.bx(id)?;
        if !self.tree[b].is_element() {
            return None;
        }
        boxes::resolved::value(&self.tree, b, property).filter(|v| !v.is_empty())
    }
}
