//! The capture file: one page state at one step.
//!
//! A capture holds the raw Chrome DevTools Protocol data the views need: one or more
//! `DOMSnapshot.captureSnapshot` results, the accessibility tree of every frame, the
//! frame tree and the viewport. It is immutable once written.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The only capture schema this crate reads and writes.
pub const SCHEMA: u32 = 1;

/// One page state at one step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capture {
    /// Schema version; always [`SCHEMA`].
    pub schema: u32,
    /// URL of the top-level document.
    pub url: String,
    /// Title of the top-level document.
    pub title: String,
    /// Load state of the top-level document.
    pub load_state: LoadState,
    /// The visual viewport in CSS pixels, in top-level document coordinates.
    pub viewport: Viewport,
    /// Raw `DOMSnapshot.captureSnapshot` results (one per CDP session).
    pub snapshots: Vec<DomSnapshot>,
    /// Frames; the first entry is the top-level frame.
    pub frames: Vec<Frame>,
    /// Baseline outputs computed live at the capture instant, keyed by format name.
    #[serde(default)]
    pub baselines: BTreeMap<String, String>,
    /// Inputs some baselines need that only the live page has (e.g. browser-use's visible text).
    #[serde(default)]
    pub extras: BTreeMap<String, String>,
    /// Provenance; never read by the views.
    pub meta: Meta,
}

/// Load state of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoadState {
    /// Still parsing.
    Loading,
    /// DOM ready, subresources pending.
    Interactive,
    /// Fully loaded.
    Complete,
}

impl fmt::Display for LoadState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Loading => "loading",
            Self::Interactive => "interactive",
            Self::Complete => "complete",
        })
    }
}

/// The visual viewport.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Viewport {
    /// Left edge (scroll x) in CSS pixels.
    pub x: f64,
    /// Top edge (scroll y) in CSS pixels.
    pub y: f64,
    /// Width in CSS pixels.
    pub width: f64,
    /// Height in CSS pixels.
    pub height: f64,
    /// The top-level document's scrollable width (0: unknown).
    #[serde(default)]
    pub content_width: f64,
    /// The top-level document's scrollable height (0: unknown).
    #[serde(default)]
    pub content_height: f64,
}

/// One frame of the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    /// CDP frame id.
    pub frame_id: String,
    /// Parent frame id; `None` for the top-level frame.
    #[serde(default)]
    pub parent_frame_id: Option<String>,
    /// Backend node id of the `<iframe>` element in the parent frame.
    #[serde(default)]
    pub owner_backend_node_id: Option<i64>,
    /// Document URL of the frame.
    #[serde(default)]
    pub url: String,
    /// Origin (scheme, host, port).
    pub origin: String,
    /// Load state of the frame.
    pub state: FrameState,
    /// Where this frame's document lives in [`Capture::snapshots`].
    #[serde(default)]
    pub document: Option<DocRef>,
    /// Raw `Accessibility.getFullAXTree` nodes of this frame.
    #[serde(default)]
    pub ax_nodes: Vec<AxNode>,
}

/// State of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameState {
    /// Loaded.
    Loaded,
    /// Still loading.
    Loading,
    /// Failed to load.
    Failed,
    /// Detached during capture.
    Detached,
}

impl fmt::Display for FrameState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Loaded => "loaded",
            Self::Loading => "loading",
            Self::Failed => "failed",
            Self::Detached => "detached",
        })
    }
}

/// Pointer to one document inside one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRef {
    /// Index into [`Capture::snapshots`].
    pub snapshot: usize,
    /// Index into that snapshot's `documents`.
    pub document: usize,
}

/// Provenance of a capture.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Meta {
    /// When the capture was taken (never read by views).
    #[serde(default)]
    pub captured_at: String,
    /// Version of the capturing tool.
    #[serde(default)]
    pub tool_version: String,
    /// Browser version.
    #[serde(default)]
    pub chromium_version: String,
    /// Step index within an episode.
    #[serde(default)]
    pub step: u32,
    /// Increases whenever the top-level document is replaced (navigation).
    #[serde(default)]
    pub nav_counter: u32,
}

/// A raw `DOMSnapshot.captureSnapshot` result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomSnapshot {
    /// One entry per document (frame) in the session.
    pub documents: Vec<DocumentSnapshot>,
    /// Shared string table.
    pub strings: Vec<String>,
}

/// One document of a snapshot (CDP `DOMSnapshot.DocumentSnapshot`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSnapshot {
    /// String index of the document URL.
    #[serde(rename = "documentURL")]
    pub document_url: i64,
    /// String index of the frame id.
    pub frame_id: i64,
    /// Node tree.
    pub nodes: NodeTreeSnapshot,
    /// Layout tree.
    pub layout: LayoutTreeSnapshot,
}

/// CDP `DOMSnapshot.NodeTreeSnapshot` (only the fields pageon reads).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NodeTreeSnapshot {
    /// Parent node index, `-1` for the root.
    pub parent_index: Vec<i64>,
    /// DOM node type (1 element, 3 text, 9 document, …).
    pub node_type: Vec<i64>,
    /// String index of the node name.
    pub node_name: Vec<i64>,
    /// String index of the node value.
    pub node_value: Vec<i64>,
    /// Backend node id.
    pub backend_node_id: Vec<i64>,
    /// Flat name/value string-index pairs per node.
    pub attributes: Vec<Vec<i64>>,
    /// Current value of form controls.
    pub input_value: RareStringData,
    /// Checked state of checkboxes and radios.
    pub input_checked: RareBooleanData,
    /// Child document of `<iframe>` elements.
    pub content_document_index: RareIntegerData,
    /// Nodes that respond to clicks (including JS click listeners).
    pub is_clickable: RareBooleanData,
}

/// CDP rare string data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RareStringData {
    /// Node indexes.
    pub index: Vec<i64>,
    /// String indexes.
    pub value: Vec<i64>,
}

/// CDP rare boolean data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RareBooleanData {
    /// Node indexes for which the value is true.
    pub index: Vec<i64>,
}

/// CDP rare integer data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RareIntegerData {
    /// Node indexes.
    pub index: Vec<i64>,
    /// Values.
    pub value: Vec<i64>,
}

/// CDP `DOMSnapshot.LayoutTreeSnapshot` (only the fields pageon reads).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LayoutTreeSnapshot {
    /// Node index of each layout object.
    pub node_index: Vec<i64>,
    /// Computed style string indexes, in the order requested at capture.
    pub styles: Vec<Vec<i64>>,
    /// `[x, y, width, height]` per layout object, in document coordinates.
    pub bounds: Vec<Vec<f64>>,
    /// Paint order per layout object (later = on top).
    pub paint_orders: Vec<i64>,
    /// Per layout object: `[scrollLeft, scrollTop, scrollWidth, scrollHeight]` for scroll
    /// containers, empty for others.
    #[serde(default)]
    pub scroll_rects: Vec<Vec<f64>>,
}

/// One node of `Accessibility.getFullAXTree`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxNode {
    /// AX node id (unique within the frame).
    pub node_id: String,
    /// Whether Chromium ignores the node for accessibility.
    #[serde(default)]
    pub ignored: bool,
    /// Why the node is ignored.
    #[serde(default)]
    pub ignored_reasons: Vec<AxProperty>,
    /// Role.
    #[serde(default)]
    pub role: Option<AxValue>,
    /// Accessible name.
    #[serde(default)]
    pub name: Option<AxValue>,
    /// Value.
    #[serde(default)]
    pub value: Option<AxValue>,
    /// States and properties.
    #[serde(default)]
    pub properties: Vec<AxProperty>,
    /// Parent AX node id.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Child AX node ids.
    #[serde(default)]
    pub child_ids: Vec<String>,
    /// Backend id of the DOM node.
    #[serde(default, rename = "backendDOMNodeId")]
    pub backend_dom_node_id: Option<i64>,
}

/// A typed AX value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AxValue {
    /// CDP value type.
    #[serde(rename = "type")]
    pub kind: String,
    /// The value.
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

impl AxValue {
    /// The value as a string, if it is one (or a number/bool rendered as text).
    pub fn as_text(&self) -> Option<String> {
        match self.value.as_ref()? {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }
}

/// A named AX property.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AxProperty {
    /// Property name.
    pub name: String,
    /// Property value.
    pub value: AxValue,
}

/// Errors reading or writing captures.
#[derive(Debug)]
pub enum CaptureError {
    /// The file could not be read or written.
    Io(std::io::Error),
    /// The content is not a valid capture.
    Json(serde_json::Error),
    /// The capture uses another schema version.
    Schema(u32),
    /// The capture is structurally inconsistent.
    Invalid(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read capture: {e}; check the path"),
            Self::Json(e) => write!(
                f,
                "not a valid capture: {e}; re-capture the page with pageon-bench"
            ),
            Self::Schema(v) => write!(
                f,
                "unsupported capture schema {v}; re-capture with pageon-bench >= {}",
                env!("CARGO_PKG_VERSION")
            ),
            Self::Invalid(m) => write!(
                f,
                "inconsistent capture: {m}; re-capture the page with pageon-bench"
            ),
        }
    }
}

impl std::error::Error for CaptureError {}

impl Capture {
    /// Parses a capture from JSON text and validates it.
    pub fn from_json(text: &str) -> Result<Self, CaptureError> {
        let schema = serde_json::from_str::<SchemaProbe>(text)
            .map_err(CaptureError::Json)?
            .schema;
        if schema != SCHEMA {
            return Err(CaptureError::Schema(schema));
        }
        let capture: Self = serde_json::from_str(text).map_err(CaptureError::Json)?;
        capture.validate()?;
        Ok(capture)
    }

    /// Loads a capture from a file; `.gz` files are decompressed.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CaptureError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(CaptureError::Io)?;
        let text = if path.extension().is_some_and(|e| e == "gz") {
            let mut s = String::new();
            flate2::read::GzDecoder::new(&bytes[..])
                .read_to_string(&mut s)
                .map_err(CaptureError::Io)?;
            s
        } else {
            String::from_utf8(bytes).map_err(|e| CaptureError::Invalid(e.to_string()))?
        };
        Self::from_json(&text)
    }

    /// Saves the capture with stable key order; `.gz` paths are compressed.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), CaptureError> {
        let path = path.as_ref();
        let value = serde_json::to_value(self).map_err(CaptureError::Json)?;
        let text = serde_json::to_string(&value).map_err(CaptureError::Json)?;
        if path.extension().is_some_and(|e| e == "gz") {
            let file = std::fs::File::create(path).map_err(CaptureError::Io)?;
            let mut enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            enc.write_all(text.as_bytes()).map_err(CaptureError::Io)?;
            enc.finish().map_err(CaptureError::Io)?;
        } else {
            std::fs::write(path, text).map_err(CaptureError::Io)?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), CaptureError> {
        if self.frames.is_empty() {
            return Err(CaptureError::Invalid("no frames".into()));
        }
        for frame in &self.frames {
            if let Some(doc) = frame.document {
                let snap = self.snapshots.get(doc.snapshot).ok_or_else(|| {
                    CaptureError::Invalid(format!(
                        "frame {} points to missing snapshot {}",
                        frame.frame_id, doc.snapshot
                    ))
                })?;
                if doc.document >= snap.documents.len() {
                    return Err(CaptureError::Invalid(format!(
                        "frame {} points to missing document {}",
                        frame.frame_id, doc.document
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct SchemaProbe {
    schema: u32,
}
