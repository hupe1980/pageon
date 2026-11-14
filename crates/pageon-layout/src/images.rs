//! Images on demand: a layout built with [`Images`] records the image URLs the
//! document asks for (`<img>`, CSS backgrounds) and applies the ones already decoded. Decoding
//! happens here, in whatever process lays out (the caged content process), never in the
//! broker. SVG images are rasterized with the bundled fonts only (never host fonts).

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, Mutex};

use blitz_traits::net::{NetHandler, NetProvider, Request};

/// Largest image side accepted, in pixels.
pub const MAX_SIDE: u32 = 16_384;
/// Largest decoded image accepted, in bytes (RGBA).
pub const MAX_BYTES: u64 = 64 << 20;

/// One decoded image (straight RGBA8), or why it could not be used.
#[derive(Clone)]
pub(crate) enum Decoded {
    Raster {
        width: u32,
        height: u32,
        rgba: Arc<Vec<u8>>,
    },
    Failed(String),
}

/// Decoded images by absolute URL, kept per tab and handed to every later layout.
#[derive(Clone, Default)]
pub struct Images {
    map: HashMap<String, Decoded>,
}

impl std::fmt::Debug for Images {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Images")
            .field("count", &self.map.len())
            .finish_non_exhaustive()
    }
}

impl Images {
    /// No images.
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes `bytes` (PNG, JPEG, GIF's first frame, WebP or SVG) for `url`. Returns the
    /// natural size, or why it failed; a failure is remembered so the URL is not fetched again.
    pub fn insert(&mut self, url: &str, bytes: &[u8]) -> Result<(u32, u32), String> {
        let decoded = decode(bytes);
        let out = match &decoded {
            Decoded::Raster { width, height, .. } => Ok((*width, *height)),
            Decoded::Failed(why) => Err(why.clone()),
        };
        self.map.insert(url.to_string(), decoded);
        out
    }

    /// Remembers that `url` could not be loaded.
    pub fn fail(&mut self, url: &str, why: &str) {
        self.map
            .insert(url.to_string(), Decoded::Failed(why.to_string()));
    }

    /// Whether `url` has been loaded or has failed.
    pub fn contains(&self, url: &str) -> bool {
        self.map.contains_key(url)
    }

    /// Number of URLs known (loaded or failed).
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether no URL is known.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Why `url` failed, if it did.
    pub fn failure(&self, url: &str) -> Option<&str> {
        match self.map.get(url)? {
            Decoded::Failed(why) => Some(why),
            Decoded::Raster { .. } => None,
        }
    }

    pub(crate) fn get(&self, url: &str) -> Option<&Decoded> {
        self.map.get(url)
    }
}

fn decode(bytes: &[u8]) -> Decoded {
    let raster = || -> Result<Decoded, String> {
        let mut reader = image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| e.to_string())?;
        if reader.format().is_none() {
            return Err("unknown image format".into());
        }
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_SIDE);
        limits.max_image_height = Some(MAX_SIDE);
        limits.max_alloc = Some(MAX_BYTES);
        reader.limits(limits);
        let img = reader.decode().map_err(|e| e.to_string())?;
        let (width, height) = (img.width(), img.height());
        Ok(Decoded::Raster {
            width,
            height,
            rgba: Arc::new(img.into_rgba8().into_raw()),
        })
    };
    match raster() {
        Ok(d) => d,
        Err(_) if looks_like_svg(bytes) => svg(bytes).unwrap_or_else(Decoded::Failed),
        Err(why) => Decoded::Failed(why),
    }
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(1024)];
    String::from_utf8_lossy(head).contains("<svg")
}

fn svg_options() -> &'static resvg::usvg::Options<'static> {
    static OPTIONS: std::sync::OnceLock<resvg::usvg::Options<'static>> = std::sync::OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        for (_, bytes) in crate::fonts::FONTS {
            db.load_font_data(bytes.to_vec());
        }
        db.set_sans_serif_family("Liberation Sans");
        db.set_serif_family("Liberation Serif");
        db.set_monospace_family("Liberation Mono");
        resvg::usvg::Options {
            font_family: "Liberation Serif".into(),
            fontdb: Arc::new(db),
            // Nested `<image href>`s resolve `data:` URLs only, never paths: an image from the
            // web must not read local files.
            image_href_resolver: resvg::usvg::ImageHrefResolver {
                resolve_string: Box::new(|_, _| None),
                ..resvg::usvg::ImageHrefResolver::default()
            },
            ..resvg::usvg::Options::default()
        }
    })
}

/// Rasterizes an SVG at its natural size (default 300×150, as an `<img>` without sizes).
fn svg(bytes: &[u8]) -> Result<Decoded, String> {
    let tree = resvg::usvg::Tree::from_data(bytes, svg_options()).map_err(|e| e.to_string())?;
    let size = tree.size();
    let (w, h) = (size.width().ceil() as u32, size.height().ceil() as u32);
    let (w, h) = if w == 0 || h == 0 { (300, 150) } else { (w, h) };
    if w > MAX_SIDE || h > MAX_SIDE || u64::from(w) * u64::from(h) * 4 > MAX_BYTES {
        return Err(format!("SVG too large ({w}×{h})"));
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or("SVG has no area")?;
    let t = resvg::tiny_skia::Transform::from_scale(
        w as f32 / size.width().max(1.0),
        h as f32 / size.height().max(1.0),
    );
    resvg::render(&tree, t, &mut pixmap.as_mut());
    let rgba: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Ok(Decoded::Raster {
        width: w,
        height: h,
        rgba: Arc::new(rgba),
    })
}

/// A network provider that only records the URLs the layout engine asks for. It reports
/// itself as a no-op, so nothing waits on it (no "critical" resources block painting).
#[derive(Default)]
pub(crate) struct Recorder(Mutex<Vec<String>>);

impl Recorder {
    pub(crate) fn urls(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for u in self.0.lock().map(|v| v.clone()).unwrap_or_default() {
            if !out.contains(&u) {
                out.push(u);
            }
        }
        out
    }
}

impl NetProvider for Recorder {
    fn fetch(&self, _doc_id: usize, request: Request, _handler: Box<dyn NetHandler>) {
        if let Ok(mut v) = self.0.lock() {
            v.push(request.url.to_string());
        }
    }

    fn is_noop(&self) -> bool {
        true
    }
}
