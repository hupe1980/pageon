//! Playwright traces of replayed recordings. [`Tracer`] wraps a replay driver and keeps, for
//! each step, its outcome, the page's HTML and a screenshot; [`Tracer::finish`] writes them,
//! with the recording's network exchanges, as a trace (format 10, what playwright-core 1.65
//! writes) that `npx playwright show-trace` opens. Times are the page's virtual time.

use std::io::Write as _;

use serde_json::{Value, json};
use sha1::Digest as _;

use pageon_env::Divergence;

use crate::replay::Driver;
use crate::{Action, ActionError, Dump, LoadOptions};

/// Playwright's trace format version.
const VERSION: u32 = 10;

/// Bodies larger than this stay out of the trace (the entry is kept).
const MAX_BODY: usize = 10 << 20;

/// What one step left behind.
#[derive(Debug, Default)]
struct Entry {
    /// Opening a page (`Frame.goto`) rather than an action.
    open: bool,
    url: String,
    error: Option<String>,
    /// Virtual time the action settled in.
    virtual_ms: u64,
    console: Vec<(String, String)>,
    errors: Vec<crate::PageError>,
    html: Option<String>,
    screenshot: Option<(Vec<u8>, u32, u32)>,
}

/// A replay driver that keeps what a trace needs.
pub struct Tracer<'a> {
    inner: &'a mut dyn Driver,
    entries: Vec<Entry>,
}

impl<'a> Tracer<'a> {
    /// Wraps `inner`.
    pub fn new(inner: &'a mut dyn Driver) -> Self {
        Self {
            inner,
            entries: Vec::new(),
        }
    }

    fn capture(&mut self, mut e: Entry) {
        e.html = self.inner.view(Dump::Html).ok();
        e.screenshot = self.inner.screenshot().ok();
        self.entries.push(e);
    }

    /// Writes the trace of the steps run so far to `out` (a zip). `divergence` marks the step
    /// a diverging replay stopped at.
    pub fn finish(
        &self,
        log: &pageon_env::Log,
        divergence: Option<&Divergence>,
        out: &mut dyn std::io::Write,
    ) -> std::io::Result<()> {
        let mut zip = Zip::default();
        let (trace, network) = events(log, &self.entries, divergence, &mut zip);
        zip.add("trace.trace", trace.as_bytes());
        zip.add("trace.network", network.as_bytes());
        out.write_all(&zip.finish())
    }
}

impl Driver for Tracer<'_> {
    fn open(&mut self, url: &str, options: LoadOptions) -> Result<(), String> {
        let r = self.inner.open(url, options);
        self.capture(Entry {
            open: true,
            url: url.to_string(),
            error: r.as_ref().err().cloned(),
            ..Entry::default()
        });
        r
    }

    fn act(&mut self, action: Action) -> Result<crate::Outcome, ActionError> {
        let r = self.inner.act(action);
        let mut e = Entry::default();
        match &r {
            Ok(o) => {
                e.virtual_ms = o.settled.virtual_ms;
                e.console = o.console.clone();
                e.errors = o.errors.clone();
            }
            Err(err) => e.error = Some(err.to_string()),
        }
        self.capture(e);
        r
    }

    fn observe(&mut self) -> Result<String, String> {
        self.inner.observe()
    }

    fn view(&mut self, kind: Dump) -> Result<String, String> {
        self.inner.view(kind)
    }

    fn screenshot(&mut self) -> Result<(Vec<u8>, u32, u32), String> {
        self.inner.screenshot()
    }

    fn divergence(&self) -> Option<Divergence> {
        self.inner.divergence()
    }
}

/// The trace's two event files.
fn events(
    log: &pageon_env::Log,
    entries: &[Entry],
    divergence: Option<&Divergence>,
    zip: &mut Zip,
) -> (String, String) {
    // Virtual time: the recording's clock readings, relative to the first one.
    let t0 = log.clock.first().copied().unwrap_or(0);
    let at = |i: usize| -> f64 {
        log.steps
            .get(i)
            .and_then(|s| s.at.clock.checked_sub(1))
            .and_then(|k| log.clock.get(k))
            .map_or(0.0, |&ms| ms.saturating_sub(t0) as f64)
    };
    let viewport = json!({"width": crate::Viewport::default().width, "height": crate::Viewport::default().height});
    let mut lines: Vec<Value> = vec![
        json!({
            "version": VERSION,
            "type": "context-options",
            "origin": "library",
            "browserName": "pageon",
            "playwrightVersion": "1.65.0",
            "platform": std::env::consts::OS,
            "wallTime": t0,
            "monotonicTime": 0,
            "sdkLanguage": "javascript",
            "title": format!("pageon {}", env!("CARGO_PKG_VERSION")),
            "options": {"viewport": viewport, "userAgent": log.user_agent},
        }),
        json!({"type": "event", "time": 0, "class": "BrowserContext", "method": "page", "params": {"pageId": "page@1"}}),
    ];
    let mut network = String::new();
    let mut prev_end = 0.0;
    let mut prev_exchanges = 0;
    for (i, e) in entries.iter().enumerate() {
        let step = log.steps.get(i);
        let start = prev_end;
        // At least the time the action settled in, and 1 ms, so steps stay apart.
        let end = at(i).max(start + e.virtual_ms.max(1) as f64);
        prev_end = end;
        let call = format!("call@{}", i + 1);
        let title = step.map_or_else(String::new, |s| s.action.clone());
        let (class, method, params) = if e.open {
            ("Frame", "goto", json!({"url": e.url}))
        } else {
            let verb = title.split_whitespace().next().unwrap_or("act").to_string();
            ("pageon", "act", json!({"action": title, "verb": verb}))
        };
        lines.push(json!({
            "type": "before", "callId": call, "startTime": start, "title": title,
            "class": class, "method": method, "params": params,
        }));
        // The step's requests, spread over its time.
        let upto = step.map_or(log.exchanges.len(), |s| s.at.exchanges);
        let mine = log.exchanges.get(prev_exchanges..upto).unwrap_or_default();
        for (k, x) in mine.iter().enumerate() {
            let t = start + (end - start) * (k as f64 + 0.5) / mine.len().max(1) as f64;
            network.push_str(&resource(x, t0, t, zip).to_string());
            network.push('\n');
        }
        prev_exchanges = upto.max(prev_exchanges);
        for (level, text) in &e.console {
            lines.push(json!({
                "type": "console", "messageType": console_type(level), "text": text, "args": [],
                "location": {"url": "", "lineNumber": 0, "columnNumber": 0},
                "time": end, "pageId": "page@1",
            }));
        }
        for pe in &e.errors {
            lines.push(json!({
                "type": "event", "time": end, "class": "BrowserContext", "method": "pageError",
                "params": {"error": {"error": {"message": pe.message, "name": "Error", "stack": ""}},
                    "location": {"url": pe.source, "line": pe.line, "column": pe.column}},
                "pageId": "page@1",
            }));
        }
        if let Some(html) = &e.html {
            let url = step_url(e, step).unwrap_or_default();
            lines.push(json!({"type": "frame-snapshot", "snapshot": {
                "callId": call, "phase": "after", "pageId": "page@1", "frameId": "frame@1",
                "frameUrl": url, "doctype": "html", "html": snapshot(html),
                "viewport": viewport, "timestamp": end, "wallTime": t0 as f64 + end,
                "collectionTime": 0, "resourceOverrides": [], "isMainFrame": true,
            }}));
        }
        if let Some((bytes, w, h)) = &e.screenshot {
            let file = format!("screencast/page@1-{}.jpeg", i + 1);
            zip.add(&file, bytes);
            lines.push(json!({
                "type": "screencast-frame", "pageId": "page@1", "file": file,
                "width": w, "height": h, "timestamp": end,
            }));
        }
        let error = e.error.clone().or_else(|| {
            divergence
                .filter(|d| d.step == i)
                .map(crate::replay::report)
        });
        let mut after = json!({"type": "after", "callId": call, "endTime": end});
        if let Some(message) = error {
            after["error"] = json!({"message": message, "name": "Error"});
        }
        lines.push(after);
    }
    let mut trace = String::new();
    for l in lines {
        trace.push_str(&l.to_string());
        trace.push('\n');
    }
    (trace, network)
}

/// The URL of the page after a step: the address line of its agent view.
fn step_url(e: &Entry, step: Option<&pageon_env::Step>) -> Option<String> {
    if e.open && step.is_none() {
        return Some(e.url.clone());
    }
    step?
        .view
        .split_whitespace()
        .find(|w| w.starts_with("http://") || w.starts_with("https://") || w.starts_with("file:"))
        .map(str::to_string)
        .or_else(|| e.open.then(|| e.url.clone()))
}

fn console_type(level: &str) -> &'static str {
    match level {
        "error" => "error",
        "warn" | "warning" => "warning",
        "info" => "info",
        "debug" => "debug",
        _ => "log",
    }
}

/// One recorded exchange as a `resource-snapshot` (HAR entry), its body in `resources/`.
fn resource(x: &pageon_env::Exchange, t0: u64, time: f64, zip: &mut Zip) -> Value {
    use base64::Engine as _;
    let body: Vec<u8> = match (&x.body, &x.body_base64) {
        (Some(t), _) => t.as_bytes().to_vec(),
        (None, Some(b)) => base64::engine::general_purpose::STANDARD
            .decode(b)
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let mime = x
        .headers
        .iter()
        .find(|(k, _)| k == "content-type")
        .map_or("x-unknown", |(_, v)| v.as_str())
        .to_string();
    let mut content = json!({"size": body.len(), "mimeType": mime});
    if !body.is_empty() && body.len() <= MAX_BODY {
        let sha: String = sha1::Sha1::digest(&body)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let file = format!("resources/{sha}.{}", extension(&mime));
        zip.add(&file, &body);
        content["_file"] = json!(file);
    } else if body.len() > MAX_BODY {
        content["comment"] = json!("left out of the trace (over 10 MiB)");
    }
    let headers: Vec<Value> = x
        .headers
        .iter()
        .map(|(n, v)| json!({"name": n, "value": v}))
        .collect();
    let mut request = json!({
        "method": x.method, "url": x.url, "httpVersion": "HTTP/1.1", "cookies": [],
        "headers": [], "queryString": [], "headersSize": -1, "bodySize": 0,
    });
    if let Some(b) = &x.request_body {
        request["bodySize"] = json!(b.len());
        request["postData"] = json!({"mimeType": "", "text": b});
    }
    let status = if x.error.is_some() { 0 } else { x.status };
    json!({"type": "resource-snapshot", "snapshot": {
        "pageref": "page@1",
        "startedDateTime": iso8601(t0 + time as u64),
        "time": 0,
        "request": request,
        "response": {
            "status": status, "statusText": x.error.clone().unwrap_or_default(),
            "httpVersion": "HTTP/1.1", "cookies": [], "headers": headers, "content": content,
            "headersSize": -1, "bodySize": body.len(), "redirectURL": "", "_transferSize": body.len(),
        },
        "cache": {}, "timings": {"send": -1, "wait": -1, "receive": -1},
        "_frameref": "frame@1", "_monotonicTime": time.max(0.001),
    }})
}

fn extension(mime: &str) -> &'static str {
    let m = mime.split(';').next().unwrap_or_default().trim();
    match m {
        "text/html" => "html",
        "text/css" => "css",
        "text/javascript" | "application/javascript" => "js",
        "application/json" => "json",
        "image/png" => "png",
        "image/jpeg" => "jpeg",
        "image/svg+xml" => "svg",
        "image/webp" => "webp",
        "text/plain" => "txt",
        _ => "dat",
    }
}

/// Epoch milliseconds as an ISO 8601 UTC timestamp.
fn iso8601(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

/// The page's HTML as Playwright's snapshot tree: `[TAG, {attrs}, ...children]` and text as
/// strings. Scripts, comments and event handler attributes are left out, as Playwright does.
pub(crate) fn snapshot(html: &str) -> Value {
    let dom = pageon_dom::parse_document(html, pageon_dom::ParseOptions::default());
    let root = dom.root();
    dom.children(root)
        .find_map(|c| element(&dom, c))
        .unwrap_or_else(|| json!(["HTML", {}]))
}

fn element(dom: &pageon_dom::Dom, id: pageon_dom::NodeId) -> Option<Value> {
    let node = dom.get(id)?;
    match &node.data {
        pageon_dom::NodeData::Text(t) => Some(json!(t)),
        pageon_dom::NodeData::Element { .. } => {
            let name = node.local_name()?.to_ascii_uppercase();
            if name == "SCRIPT" {
                return None;
            }
            let name = if name == "NOSCRIPT" {
                "X-NOSCRIPT".to_string()
            } else {
                name
            };
            let mut attrs = serde_json::Map::new();
            for a in node.attrs() {
                let n = a.local_name();
                if !n.to_ascii_lowercase().starts_with("on") {
                    attrs.insert(n.to_string(), json!(a.value));
                }
            }
            let mut out = vec![json!(name), Value::Object(attrs)];
            out.extend(dom.children(id).filter_map(|c| element(dom, c)));
            Some(Value::Array(out))
        }
        _ => None,
    }
}

/// A zip of stored (uncompressed) entries.
#[derive(Default)]
struct Zip {
    data: Vec<u8>,
    central: Vec<u8>,
    count: u16,
    names: std::collections::HashSet<String>,
}

impl Zip {
    fn add(&mut self, name: &str, bytes: &[u8]) {
        if !self.names.insert(name.to_string()) {
            return;
        }
        let crc = crc32(bytes);
        let size = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let offset = u32::try_from(self.data.len()).unwrap_or(u32::MAX);
        let n = u16::try_from(name.len()).unwrap_or(u16::MAX);
        // Local file header: version 2.0, UTF-8 names, stored.
        let mut h = Vec::with_capacity(30 + name.len());
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(&0x0800u16.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes()); // time and date
        h.extend_from_slice(&crc.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&n.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes());
        h.extend_from_slice(name.as_bytes());
        self.data.extend_from_slice(&h);
        self.data.extend_from_slice(bytes);
        let c = &mut self.central;
        c.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&20u16.to_le_bytes());
        c.extend_from_slice(&0x0800u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u32.to_le_bytes());
        c.extend_from_slice(&crc.to_le_bytes());
        c.extend_from_slice(&size.to_le_bytes());
        c.extend_from_slice(&size.to_le_bytes());
        c.extend_from_slice(&n.to_le_bytes());
        c.extend_from_slice(&[0; 12]); // extra, comment, disk, attributes
        c.extend_from_slice(&offset.to_le_bytes());
        c.extend_from_slice(name.as_bytes());
        self.count = self.count.saturating_add(1);
    }

    fn finish(mut self) -> Vec<u8> {
        let offset = u32::try_from(self.data.len()).unwrap_or(u32::MAX);
        let size = u32::try_from(self.central.len()).unwrap_or(u32::MAX);
        let mut out = std::mem::take(&mut self.data);
        out.extend_from_slice(&self.central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend_from_slice(&self.count.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }
}

/// CRC-32 (IEEE), as zip uses.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Writes a trace for a whole replay: re-runs `log` on `driver` and writes the zip to `path`.
/// Returns the divergence, if the replay diverged (the trace then ends there).
pub fn write(
    driver: &mut dyn Driver,
    log: &pageon_env::Log,
    upto: usize,
    path: &std::path::Path,
    progress: &mut dyn FnMut(usize, &pageon_env::Step),
) -> std::io::Result<Option<Divergence>> {
    let mut t = Tracer::new(driver);
    let div = crate::replay::rerun(&mut t, &log.steps, upto, progress).err();
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    t.finish(log, div.as_ref(), &mut f)?;
    f.flush()?;
    Ok(div)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn snapshots_are_playwright_trees() {
        let v =
            snapshot("<!doctype html><p class=a onclick=x()>Hi <b>there</b><script>1</script><br>");
        assert_eq!(
            v,
            json!(["HTML", {}, ["HEAD", {}], ["BODY", {}, ["P", {"class": "a"}, "Hi ", ["B", {}, "there"], ["BR", {}]]]])
        );
    }

    #[test]
    fn timestamps_are_iso8601() {
        assert_eq!(iso8601(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
    }
}
