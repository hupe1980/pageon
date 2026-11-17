//! Pixels on demand: screenshots, set-of-marks and crops with provenance, painted
//! from the same layout the views and actions use. Nothing is painted, and no image is
//! fetched, until a client asks.

mod encode;
pub(crate) mod marks;
mod mask;

use std::fmt;
use std::rc::Rc;

use pageon_dom::NodeId;
use pageon_env::Env;
use pageon_layout::{Images, Layout, LayoutOptions, Pixmap, Rect, Viewport, Visibility};
use serde::{Deserialize, Serialize};

use crate::tab::{ActionError, Reason};
use crate::{Budget, Document, Tab};

pub(crate) use encode::data_url;
pub use encode::image_tokens;

/// Largest image side, in device pixels.
pub const MAX_SIDE: u32 = 16_384;
/// Largest image, in device pixels (64 megapixels, 256 MB of RGBA): taller areas are cut.
pub const MAX_PIXELS: u64 = 64 << 20;
/// Images fetched per paint, at most.
const MAX_FETCHES: usize = 256;

/// Cuts `area` so the image stays within [`MAX_PIXELS`] at `scale` (the height goes), with a
/// note when it did.
fn within_pixels(mut area: Rect, scale: f32) -> (Rect, Option<String>) {
    let scale2 = f64::from(scale) * f64::from(scale);
    let pixels = f64::from(area.width) * f64::from(area.height) * scale2;
    if pixels <= MAX_PIXELS as f64 || area.width <= 0.0 {
        return (area, None);
    }
    let height = (MAX_PIXELS as f64 / (f64::from(area.width) * scale2)).floor() as f32;
    let note = format!(
        "capped: {}×{} px is over 64 megapixels; painted the top {}×{height}",
        area.width, area.height, area.width
    );
    area.height = height;
    (area, Some(note))
}

/// What part of the page to paint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Area {
    /// The viewport (scroll position 0, 0).
    #[default]
    Viewport,
    /// The whole document, capped at 16,384 device pixels a side.
    FullPage,
    /// A rectangle in document CSS px (may extend below the fold).
    Clip(Rect),
    /// A rectangle in viewport CSS px (where the viewport is scrolled to).
    ViewportClip(Rect),
}

/// Image format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    /// PNG (lossless).
    #[default]
    Png,
    /// JPEG with a quality from 0 to 100.
    Jpeg {
        /// Quality.
        quality: u8,
    },
}

impl Format {
    /// The MIME type.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg { .. } => "image/jpeg",
        }
    }

    /// From a file name's extension (`.png`, `.jpg`, `.jpeg`).
    pub fn from_path(path: &str) -> Option<Self> {
        let ext = path.rsplit('.').next()?.to_ascii_lowercase();
        match ext.as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg { quality: 80 }),
            _ => None,
        }
    }
}

/// Screenshot options.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenshotOptions {
    /// What to paint.
    pub area: Area,
    /// Device pixels per CSS px, 0.25 to 2.
    pub scale: f32,
    /// Format.
    pub format: Format,
    /// Set-of-marks: outline and label every visible element that has a ref.
    pub marks: bool,
    /// A token budget: the image (and legend) is downscaled to fit, never below scale 0.25.
    pub budget: Option<Budget>,
    /// Paint over third-party frames and text a person cannot read: what a vision
    /// model should not take instructions from. Refs and marks stay.
    #[serde(default)]
    pub mask: bool,
}

impl Default for ScreenshotOptions {
    fn default() -> Self {
        Self {
            area: Area::Viewport,
            scale: 1.0,
            format: Format::Png,
            marks: false,
            budget: None,
            mask: false,
        }
    }
}

/// Where a crop's pixels come from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// The ref.
    pub reference: String,
    /// Its role (from its `ax` line).
    pub role: String,
    /// Its accessible name.
    pub name: String,
    /// Its box (top-level document CSS px).
    pub rect: Rect,
    /// The URL of the frame it is in.
    pub frame_url: String,
    /// That frame's origin.
    pub origin: String,
    /// Whether that origin differs from the top-level origin.
    pub cross_origin: bool,
    /// `first-party`, or `third-party` for a cross-origin frame.
    pub tier: String,
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {:?} at {},{} {}×{}, frame {} ({})",
            self.role,
            self.reference,
            self.name,
            self.rect.x.round(),
            self.rect.y.round(),
            self.rect.width.round(),
            self.rect.height.round(),
            self.origin,
            self.tier
        )
    }
}

/// A painted image.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Image {
    /// The encoded bytes.
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Format.
    pub format: Format,
    /// The scale used (lower than asked when a budget required it).
    pub scale: f32,
    /// Cost in `o200k` image tokens.
    pub tokens: usize,
    /// What the image does not show, or how it was made: images that did not load, elements
    /// pageon does not paint, capping.
    pub notes: Vec<String>,
    /// With marks: one `ax` line per mark, in label order.
    pub legend: Option<String>,
    /// For a crop: where the pixels come from.
    pub provenance: Option<Provenance>,
}

impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Image")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("tokens", &self.tokens)
            .field("notes", &self.notes)
            .finish_non_exhaustive()
    }
}

impl Image {
    /// `1280×720 png, 1105 tokens`.
    pub fn summary(&self) -> String {
        let fmt = match self.format {
            Format::Png => "png",
            Format::Jpeg { .. } => "jpeg",
        };
        format!(
            "{}×{} {fmt}, {} tokens",
            self.width, self.height, self.tokens
        )
    }
}

mod b64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(serde::de::Error::custom)
    }
}

/// Why nothing was painted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PaintError {
    /// Layout failed on this page.
    Layout(String),
    /// The clip lies outside the document or is empty.
    EmptyClip {
        /// Document width (CSS px).
        width: f32,
        /// Document height (CSS px).
        height: f32,
    },
    /// The image would exceed 16,384 pixels a side.
    TooLarge {
        /// Width asked for.
        width: u32,
        /// Height asked for.
        height: u32,
    },
    /// The scale is outside 0.25–2.
    BadScale(f32),
    /// The image does not fit the token budget even at scale 0.25.
    Budget {
        /// Its cost at scale 0.25.
        tokens: usize,
        /// The budget.
        budget: usize,
    },
    /// A ref that cannot be cropped.
    Ref(ActionError),
    /// The content process failed.
    Engine(String),
}

impl fmt::Display for PaintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout(e) => write!(f, "cannot paint this page: {e}"),
            Self::EmptyClip { width, height } => write!(
                f,
                "the clip is empty or outside the document ({width}×{height} px)"
            ),
            Self::TooLarge { width, height } => {
                write!(f, "{width}×{height} px exceeds {MAX_SIDE} px a side")
            }
            Self::BadScale(s) => write!(f, "scale {s} is outside 0.25–2"),
            Self::Budget { tokens, budget } => write!(
                f,
                "the image costs {tokens} tokens even at scale 0.25, over the budget of {budget}"
            ),
            Self::Ref(e) => e.fmt(f),
            Self::Engine(e) => write!(f, "the engine failed: {e}"),
        }
    }
}

impl std::error::Error for PaintError {}

impl PaintError {
    /// What to do next.
    pub fn guidance(&self) -> String {
        match self {
            Self::Layout(_) => "read the page with the ax or markdown view instead".into(),
            Self::EmptyClip { .. } => "pass a clip inside the document".into(),
            Self::TooLarge { .. } => {
                "take a clip, or a crop of one element, instead of the full page".into()
            }
            Self::BadScale(_) => "use a scale from 0.25 to 2".into(),
            Self::Budget { .. } => "raise the budget, or take a crop of one element".into(),
            Self::Ref(e) => e.guidance(),
            Self::Engine(_) => "open the page again".into(),
        }
    }

    /// A short kind for protocol errors.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Layout(_) => "layout",
            Self::EmptyClip { .. } => "empty_clip",
            Self::TooLarge { .. } => "too_large",
            Self::BadScale(_) => "bad_scale",
            Self::Budget { .. } => "budget",
            Self::Ref(e) => crate::mcp::kind_of(e),
            Self::Engine(_) => "engine",
        }
    }
}

/// The viewport painting uses (pageon lays pages out at one viewport).
fn viewport() -> Viewport {
    Viewport::default()
}

impl Document {
    /// Shares a tab's image cache with this document and its frames.
    pub(crate) fn set_images(&mut self, images: Option<Rc<Images>>) {
        for f in &mut self.frames {
            if let Some(d) = f.document.as_deref_mut() {
                d.set_images(images.clone());
            }
        }
        self.images = images;
    }

    /// Image URLs this document and its frames use that are not loaded, with the network
    /// client of the document that uses them.
    fn wanted_images(&self, out: &mut Vec<(String, Option<pageon_net::Client>)>) {
        let l = self.layout(viewport());
        for u in l.wanted_images() {
            if !out.iter().any(|(o, _)| o == u) {
                out.push((u.clone(), self.client.clone()));
            }
        }
        for f in &self.frames {
            if let Some(d) = f.document() {
                d.wanted_images(out);
            }
        }
    }

    /// The layout to paint: images, redactions, and frames embedded in their iframes.
    fn paint_layout(&self, vp: Viewport, redact: &[String]) -> Layout {
        let opts = LayoutOptions {
            modals: &self.js.modals,
            defined: &self.js.defined,
            images: self.images.as_deref(),
            redact,
            scroll: &self.js.scroll.elements,
            styles_only: false,
            styles: None,
        };
        let mut l = Layout::with_options(
            &self.dom,
            vp,
            &self.stylesheets,
            Some(&self.final_url),
            &opts,
        );
        let (x, y) = self.js.scroll.viewport;
        l.set_viewport_scroll(x as f32, y as f32);
        for f in &self.frames {
            let (Some(d), Some((w, h))) = (f.document(), l.content_size(f.owner)) else {
                continue;
            };
            let child = d.paint_layout(
                Viewport {
                    width: w.round().max(1.0) as u32,
                    height: h.round().max(1.0) as u32,
                },
                redact,
            );
            l.embed(f.owner, child);
        }
        l
    }

    /// Draws the canvases' bitmaps into their boxes, where nothing covers them; returns the
    /// canvases painted.
    fn paint_canvases(
        &self,
        px: &mut pageon_layout::Pixmap,
        l: &Layout,
        area: Rect,
        scale: f32,
    ) -> Vec<NodeId> {
        let Some(live) = &self.live else {
            return Vec::new();
        };
        let bitmaps = live.engine.borrow().canvas_bitmaps(live.realm);
        let (sx, sy) = l.viewport_scroll();
        let mut painted = Vec::new();
        for (id, bw, bh, data) in bitmaps {
            if l.visibility(id) != Visibility::Visible || bw == 0 || bh == 0 {
                continue;
            }
            let Some(b) = l.box_of(id) else { continue };
            // Something on top of it (a dialog, a popover): leave it to the page's paint.
            let (cx, cy) = (b.x + b.width / 2.0 - sx, b.y + b.height / 2.0 - sy);
            if l.element_at(cx, cy).is_some_and(|top| top != id) {
                continue;
            }
            let x0 = ((b.x - area.x) * scale).round() as i64;
            let y0 = ((b.y - area.y) * scale).round() as i64;
            let w = (b.width * scale).round() as i64;
            let h = (b.height * scale).round() as i64;
            for y in y0.max(0)..(y0 + h).min(i64::from(px.height)) {
                let syi = (((y - y0) as f32 + 0.5) / h as f32 * bh as f32) as u32;
                for x in x0.max(0)..(x0 + w).min(i64::from(px.width)) {
                    let sxi = (((x - x0) as f32 + 0.5) / w as f32 * bw as f32) as u32;
                    let s = ((syi.min(bh - 1) * bw + sxi.min(bw - 1)) * 4) as usize;
                    let d = ((y as u32 * px.width + x as u32) * 4) as usize;
                    let a = u32::from(data[s + 3]);
                    for k in 0..3 {
                        let dst = u32::from(px.data[d + k]);
                        px.data[d + k] =
                            (u32::from(data[s + k]) + dst * (255 - a) / 255).min(255) as u8;
                    }
                }
            }
            painted.push(id);
        }
        painted
    }

    /// Elements painted empty because pageon does not implement them.
    fn not_painted(&self, l: &Layout, painted: &[NodeId], notes: &mut Vec<String>) {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for e in self.dom.traverse(self.dom.root()) {
            let pageon_dom::Edge::Enter(id) = e else {
                continue;
            };
            let Some(tag) = self.dom.get(id).and_then(|n| n.local_name()) else {
                continue;
            };
            let tag = match tag {
                "canvas" if painted.contains(&id) => continue,
                "canvas" => "canvas",
                "video" => "video",
                "object" => "object",
                "embed" => "embed",
                _ => continue,
            };
            if l.visibility(id) == Visibility::Visible {
                match counts.iter_mut().find(|(t, _)| *t == tag) {
                    Some((_, n)) => *n += 1,
                    None => counts.push((tag, 1)),
                }
            }
        }
        for (tag, n) in counts {
            notes.push(format!("not painted: {n} <{tag}> (not implemented)"));
        }
    }
}

impl<E: Env> Tab<'_, E> {
    /// Paints the page. The first paint of a tab loads its images (through the session's
    /// network, with policy, recording and replay); from then on they take part in layout.
    ///
    /// ```no_run
    /// use pageon::{Live, LoadOptions, ScreenshotOptions, Session};
    ///
    /// let session = Session::new(Live::default());
    /// let mut tab = session.open("https://example.com", LoadOptions::default())?;
    /// let shot = tab.screenshot(&ScreenshotOptions { marks: true, ..Default::default() })?;
    /// std::fs::write("page.png", &shot.bytes)?;
    /// println!("{}", shot.legend.unwrap_or_default()); // button e3 "Sign in", …
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn screenshot(&mut self, o: &ScreenshotOptions) -> Result<Image, PaintError> {
        if !(0.25..=2.0).contains(&o.scale) {
            return Err(PaintError::BadScale(o.scale));
        }
        let mut notes = Vec::new();
        self.load_images(&mut notes);
        let redact: Vec<String> = self
            .session
            .secrets()
            .iter()
            .map(|(v, _)| v.clone())
            .collect();
        let vp = viewport();
        let mut layout = self.doc.paint_layout(vp, &redact);
        if let Some(e) = layout.error() {
            return Err(PaintError::Layout(e.to_string()));
        }
        let (dw, dh) = layout.document_size();
        let area = match o.area {
            Area::ViewportClip(r) => {
                let (x, y) = layout.viewport_scroll();
                Rect {
                    x: r.x + x,
                    y: r.y + y,
                    ..r
                }
            }
            // The viewport where it is scrolled to.
            Area::Viewport => {
                let (x, y) = layout.viewport_scroll();
                Rect {
                    x,
                    y,
                    width: vp.width as f32,
                    height: vp.height as f32,
                }
            }
            Area::FullPage => {
                let cap = MAX_SIDE as f32 / o.scale;
                if dh > cap || dw > cap {
                    notes.push(format!(
                        "capped: the document is {dw}×{dh} px; painted {}×{}",
                        dw.min(cap),
                        dh.min(cap)
                    ));
                }
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: dw.min(cap),
                    height: dh.min(cap),
                }
            }
            Area::Clip(c) => {
                let doc = Rect {
                    x: 0.0,
                    y: 0.0,
                    width: dw,
                    height: dh,
                };
                if c.width < 1.0 || c.height < 1.0 {
                    return Err(PaintError::EmptyClip {
                        width: dw,
                        height: dh,
                    });
                }
                c.intersect(&doc).ok_or(PaintError::EmptyClip {
                    width: dw,
                    height: dh,
                })?
            }
        };
        let (area, capped) = within_pixels(area, o.scale);
        notes.extend(capped);
        let (w, h) = (
            (area.width * o.scale).round() as u32,
            (area.height * o.scale).round() as u32,
        );
        if w > MAX_SIDE || h > MAX_SIDE {
            return Err(PaintError::TooLarge {
                width: w,
                height: h,
            });
        }
        let marks = if o.marks {
            self.marks(area)
        } else {
            Vec::new()
        };
        let legend = o.marks.then(|| {
            self.redact(
                marks
                    .iter()
                    .map(|m| m.line.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        });
        let scale = fit_budget(area, o.scale, o.budget, legend.as_deref())?;
        let mut px = layout.paint(area, scale).map_err(PaintError::Layout)?;
        let canvases = self.doc.paint_canvases(&mut px, &layout, area, scale);
        if o.mask {
            let m = mask::find(&self.doc, &layout);
            for b in &m.boxes {
                marks::cover(&mut px, *b, area, scale);
            }
            notes.extend(m.note());
        }
        marks::draw(&mut px, &marks, area, scale);
        self.doc.not_painted(&layout, &canvases, &mut notes);
        let missing = layout.missing_glyphs();
        if missing > 0 {
            notes.push(format!(
                "not painted: {missing} characters with no bundled glyph (CJK, emoji)"
            ));
        }
        finish(px, o.format, scale, notes, legend, None)
    }

    /// The pixels of one element (its box plus `margin` CSS px), with where they come from.
    pub fn crop(
        &mut self,
        reference: &str,
        margin: f32,
        format: Format,
        scale: f32,
    ) -> Result<Image, PaintError> {
        let capture = self.doc.capture(viewport());
        let elements = self.episode.elements(&capture);
        let el = elements
            .iter()
            .find(|e| e.reference == reference)
            .ok_or_else(|| PaintError::Ref(ActionError::UnknownRef(reference.to_string())))?;
        let (frame, node) = self
            .doc
            .node_for(&el.target)
            .ok_or_else(|| PaintError::Ref(ActionError::StaleRef(reference.to_string())))?;
        let doc = self
            .frame_document(frame)
            .ok_or_else(|| PaintError::Ref(ActionError::StaleRef(reference.to_string())))?;
        let l = doc.layout(viewport());
        let hidden = || {
            PaintError::Ref(ActionError::NotActionable {
                target: reference.to_string(),
                reason: Reason::Hidden,
            })
        };
        if l.visibility(node) != Visibility::Visible {
            return Err(hidden());
        }
        let b = l.client_rect(node).ok_or_else(hidden)?;
        let (ox, oy) = self.frame_document_offset(frame);
        let rect = Rect {
            x: b.x + ox as f32,
            y: b.y + oy as f32,
            width: b.width,
            height: b.height,
        };
        let metas = self.frames_meta();
        let frame_url = metas.get(frame).map(|m| m.url.clone()).unwrap_or_default();
        let origin = crate::frames::origin_of(&frame_url);
        let top = crate::frames::origin_of(&self.doc.final_url);
        let cross_origin = origin != top;
        let (role, name) = role_and_name(&el.line);
        let provenance = Provenance {
            reference: reference.to_string(),
            role,
            name: self.redact(name),
            rect,
            frame_url: self.redact(frame_url),
            origin,
            cross_origin,
            tier: if cross_origin {
                "third-party"
            } else {
                "first-party"
            }
            .into(),
        };
        // Whole CSS pixels, so a crop is exactly a region of the screenshot.
        let (x0, y0) = (
            (rect.x - margin).max(0.0).floor(),
            (rect.y - margin).max(0.0).floor(),
        );
        let clip = Rect {
            x: x0,
            y: y0,
            width: (rect.x + rect.width + margin).ceil() - x0,
            height: (rect.y + rect.height + margin).ceil() - y0,
        };
        let mut img = self.screenshot(&ScreenshotOptions {
            area: Area::Clip(clip),
            scale,
            format,
            marks: false,
            budget: None,
            // A crop is one element the agent chose; its provenance says whose pixels they are.
            mask: false,
        })?;
        img.provenance = Some(provenance);
        Ok(img)
    }

    /// Fetches and decodes the images the page uses that are not loaded yet (first paint:
    /// all of them), then shares the cache with the documents.
    fn load_images(&mut self, notes: &mut Vec<String>) {
        let mut images = self.images.as_deref().cloned().unwrap_or_else(Images::new);
        if self.images.is_none() {
            self.doc.set_images(Some(Rc::new(images.clone())));
        }
        let mut wanted: Vec<(String, Option<pageon_net::Client>)> = Vec::new();
        self.doc.wanted_images(&mut wanted);
        let (mut loaded, mut failed) = (0, Vec::new());
        let net = self.session.net().clone();
        let now = self.session.now_ms();
        for (url, client) in wanted.into_iter().take(MAX_FETCHES) {
            let bytes = if url.starts_with("data:") {
                data_url(&url).ok_or_else(|| "malformed data: URL".to_string())
            } else {
                match (client, url::Url::parse(&url)) {
                    (Some(client), Ok(u)) => {
                        let req = pageon_net::FetchRequest::subresource(
                            u,
                            pageon_net::Dest::Image,
                            client,
                        );
                        match net.borrow_mut().fetch(self.session.env(), req, now) {
                            Ok(r) if (200..300).contains(&r.status) => Ok(r.body),
                            Ok(r) => Err(format!("HTTP {}", r.status)),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    _ => Err("no network for this document".into()),
                }
            };
            match bytes.and_then(|b| images.insert(&url, &b).map(|_| ())) {
                Ok(()) => loaded += 1,
                Err(why) => {
                    images.fail(&url, &why);
                    failed.push(format!(
                        "image not loaded: {} ({why})",
                        pageon_agent::quote::quote(&pageon_agent::quote::clip(
                            &self.redact(url),
                            200
                        ))
                    ));
                }
            }
        }
        if loaded > 0 || !failed.is_empty() {
            self.images = Some(Rc::new(images));
            self.doc.set_images(self.images.clone());
        } else if self.images.is_none() {
            self.images = Some(Rc::new(images));
        }
        if loaded > 0 {
            notes.push(format!("{loaded} images loaded; layout updated"));
        }
        notes.extend(failed);
    }

    /// The marks of an area: elements with refs that are visible, not covered, and inside.
    fn marks(&mut self, area: Rect) -> Vec<marks::Mark> {
        let capture = self.doc.capture(viewport());
        let elements = self.episode.elements(&capture);
        let mut layouts: Vec<(usize, Layout)> = Vec::new();
        let mut out = Vec::new();
        for el in elements {
            let Some((frame, node)) = self.doc.node_for(&el.target) else {
                continue;
            };
            if !layouts.iter().any(|(f, _)| *f == frame) {
                let Some(d) = self.frame_document(frame) else {
                    continue;
                };
                layouts.push((frame, d.layout(viewport())));
            }
            let Some((_, l)) = layouts.iter().find(|(f, _)| *f == frame) else {
                continue;
            };
            if l.visibility(node) != Visibility::Visible || l.covered_by(node).is_some() {
                continue;
            }
            let Some(b) = l.client_rect(node) else {
                continue;
            };
            let (ox, oy) = self.frame_document_offset(frame);
            let rect = Rect {
                x: b.x + ox as f32,
                y: b.y + oy as f32,
                width: b.width,
                height: b.height,
            };
            if rect.intersect(&area).is_none() {
                continue;
            }
            out.push(marks::Mark {
                reference: el.reference,
                line: el.line,
                rect,
            });
        }
        out
    }
}

impl<E: Env> crate::takeover::Driver for Tab<'_, E> {
    fn frame(&mut self) -> Result<Vec<u8>, String> {
        self.screenshot(&ScreenshotOptions::default())
            .map(|i| i.bytes)
            .map_err(|e| e.to_string())
    }

    fn input(&mut self, action: crate::Action) -> Result<(), String> {
        self.act_as_person(action)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

impl<E: Env> Tab<'_, E> {
    /// Hands the tab to a person (a CAPTCHA, a login, a second factor): serves a live view at a
    /// loopback URL, passed to `ready`, and returns when they press Done or `timeout` passes.
    pub fn takeover(
        &mut self,
        reason: &str,
        timeout: std::time::Duration,
        ready: &mut dyn FnMut(&str),
    ) -> Result<crate::takeover::Takeover, String> {
        crate::takeover::serve(reason, timeout, self, ready)
    }
}

/// `button e3 "Pay now" disabled` → (`button`, `Pay now`).
fn role_and_name(line: &str) -> (String, String) {
    let role = line
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string();
    let name = match (line.find('"'), line.rfind('"')) {
        (Some(a), Some(b)) if b > a => line[a + 1..b].to_string(),
        _ => String::new(),
    };
    (role, name)
}

/// The largest scale (from `scale` down in steps of 0.05, not below 0.25) whose image and
/// legend fit the budget.
fn fit_budget(
    area: Rect,
    scale: f32,
    budget: Option<Budget>,
    legend: Option<&str>,
) -> Result<f32, PaintError> {
    let Some(budget) = budget else {
        return Ok(scale);
    };
    let text = legend.map_or(0, |l| budget.count(l));
    let cost = |s: f32| {
        image_tokens(
            (area.width * s).round().max(1.0) as u32,
            (area.height * s).round().max(1.0) as u32,
        ) + text
    };
    let mut s = scale;
    while cost(s) > budget.max_tokens && s > 0.25 {
        s = (s - 0.05).max(0.25);
    }
    if cost(s) > budget.max_tokens {
        return Err(PaintError::Budget {
            tokens: cost(s),
            budget: budget.max_tokens,
        });
    }
    Ok(s)
}

fn finish(
    px: Pixmap,
    format: Format,
    scale: f32,
    notes: Vec<String>,
    legend: Option<String>,
    provenance: Option<Provenance>,
) -> Result<Image, PaintError> {
    let bytes = encode::encode(&px, format).map_err(PaintError::Engine)?;
    Ok(Image {
        bytes,
        width: px.width,
        height: px.height,
        format,
        scale,
        tokens: image_tokens(px.width, px.height),
        notes,
        legend,
        provenance,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P-20: a full-page paint stays within 64 megapixels.
    #[test]
    fn paints_stay_within_the_pixel_budget() {
        let r = |w: f32, h: f32| Rect {
            x: 0.0,
            y: 0.0,
            width: w,
            height: h,
        };
        let (a, note) = within_pixels(r(16_384.0, 16_384.0), 1.0);
        assert_eq!(a.height, 4096.0);
        assert!(note.unwrap_or_default().contains("over 64 megapixels"));
        let (a, _) = within_pixels(r(4096.0, 16_384.0), 2.0);
        assert!(f64::from(a.width) * f64::from(a.height) * 4.0 <= MAX_PIXELS as f64);
        let (a, note) = within_pixels(r(1280.0, 3000.0), 1.0);
        assert_eq!((a.height, note), (3000.0, None));
    }
}
