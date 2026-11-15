//! OpenTelemetry spans for MCP tool calls, exported over OTLP/HTTP with JSON. One span per
//! `tools/call`, as the OpenTelemetry MCP conventions describe a server span (they are in
//! development; pageon follows the semantic-conventions-genai repository at the 1.45 line).
//! Spans carry no arguments, results or page text. A background thread posts them in
//! batches, so a slow or absent collector costs a tool call nothing.

use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Spans per request to the collector.
const BATCH: usize = 64;
/// How long a span waits for its batch to fill.
const LINGER: Duration = Duration::from_secs(1);
/// How long one export may take.
const TIMEOUT: Duration = Duration::from_secs(2);
/// Spans waiting at most; beyond this they are dropped (the collector is not keeping up).
const QUEUE: usize = 4096;

/// A finished span.
#[derive(Debug, Clone)]
pub struct Span {
    /// 16 bytes, hex.
    pub trace_id: String,
    /// 8 bytes, hex.
    pub span_id: String,
    /// The client's span, from `params._meta.traceparent`.
    pub parent: Option<String>,
    /// `tools/call {tool}`.
    pub name: String,
    /// Unix nanoseconds.
    pub start: u64,
    /// Unix nanoseconds.
    pub end: u64,
    /// Attributes (strings).
    pub attributes: Vec<(&'static str, String)>,
    /// The error's description; the span's status is ERROR when set.
    pub error: Option<String>,
}

/// Sends spans to a collector from a background thread.
pub struct Exporter {
    tx: mpsc::SyncSender<Span>,
}

impl Exporter {
    /// An exporter posting to `endpoint` (`http://localhost:4318`; `/v1/traces` is added
    /// unless the URL already names a path).
    pub fn new(endpoint: &str) -> Result<Self, String> {
        let mut url = url::Url::parse(endpoint)
            .map_err(|e| format!("--otlp {endpoint:?}: {e} (use http://localhost:4318)"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("--otlp {endpoint:?}: use an http or https URL"));
        }
        if url.path() == "/" || url.path().is_empty() {
            url.set_path("/v1/traces");
        }
        let (tx, rx) = mpsc::sync_channel::<Span>(QUEUE);
        let target = url.to_string();
        std::thread::Builder::new()
            .name("pageon-otlp".into())
            .spawn(move || export_loop(&rx, &target))
            .map_err(|e| e.to_string())?;
        Ok(Self { tx })
    }

    /// Queues a span (dropped if the queue is full).
    pub fn send(&self, span: Span) {
        let _ = self.tx.try_send(span);
    }
}

#[allow(clippy::disallowed_methods)] // batching waits in real time, never page-visible
fn export_loop(rx: &mpsc::Receiver<Span>, url: &str) {
    let live = pageon_env::Live::new(pageon_env::Limits {
        timeout: TIMEOUT,
        max_bytes: 1 << 20,
    });
    let mut warned = false;
    loop {
        let Ok(first) = rx.recv() else { return };
        let mut batch = vec![first];
        let until = std::time::Instant::now() + LINGER;
        while batch.len() < BATCH {
            let left = until.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(s) => batch.push(s),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    post(&live, url, &batch, &mut warned);
                    return;
                }
            }
        }
        post(&live, url, &batch, &mut warned);
    }
}

fn post(live: &pageon_env::Live, url: &str, batch: &[Span], warned: &mut bool) {
    use pageon_env::Env as _;
    let req = pageon_env::Request {
        method: "POST".into(),
        url: url.to_string(),
        headers: vec![("content-type".into(), "application/json".into())],
        body: body(batch).to_string().into_bytes(),
        // The operator's collector, often on loopback: not page traffic.
        allow_private: true,
        labels: Vec::new(),
        cause: None,
        shown_url: None,
        cookie_scope: None,
    };
    let failed = match live.fetch(&req) {
        Ok(r) if (200..300).contains(&r.status) => None,
        Ok(r) => Some(format!("the collector answered {}", r.status)),
        Err(e) => Some(e.to_string()),
    };
    if let Some(why) = failed
        && !std::mem::replace(warned, true)
    {
        eprintln!(
            "warning: --otlp: spans not exported to {url}: {why} (further failures are not reported)"
        );
    }
}

/// An OTLP `ExportTraceServiceRequest` in JSON.
pub fn body(batch: &[Span]) -> Value {
    let spans: Vec<Value> = batch
        .iter()
        .map(|s| {
            let mut v = json!({
                "traceId": s.trace_id,
                "spanId": s.span_id,
                "name": s.name,
                "kind": 2, // SERVER
                "startTimeUnixNano": s.start.to_string(),
                "endTimeUnixNano": s.end.to_string(),
                "attributes": s.attributes.iter().map(|(k, v)| json!({"key": k, "value": {"stringValue": v}})).collect::<Vec<_>>(),
                "status": match &s.error {
                    Some(m) => json!({"code": 2, "message": m}),
                    None => json!({"code": 0}),
                },
            });
            if let Some(p) = &s.parent {
                v["parentSpanId"] = json!(p);
            }
            v
        })
        .collect();
    json!({"resourceSpans": [{
        "resource": {"attributes": [
            {"key": "service.name", "value": {"stringValue": "pageon"}},
            {"key": "service.version", "value": {"stringValue": env!("CARGO_PKG_VERSION")}},
        ]},
        "scopeSpans": [{
            "scope": {"name": "pageon.mcp", "version": env!("CARGO_PKG_VERSION")},
            "spans": spans,
        }],
    }]})
}

/// The trace and parent span of a W3C `traceparent` (`00-<trace>-<span>-<flags>`).
pub fn parent(traceparent: &str) -> Option<(String, String)> {
    let mut parts = traceparent.trim().split('-');
    let (version, trace, span, flags) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let hex = |s: &str, n: usize| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    };
    let ok = version == "00"
        && parts.next().is_none()
        && hex(trace, 32)
        && hex(span, 16)
        && hex(flags, 2)
        && trace.bytes().any(|b| b != b'0')
        && span.bytes().any(|b| b != b'0');
    ok.then(|| (trace.to_string(), span.to_string()))
}

/// Fresh random hex of `n` bytes.
pub fn random_id(n: usize) -> String {
    let mut b = vec![0u8; n];
    if getrandom::fill(&mut b).is_err() || b.iter().all(|&x| x == 0) {
        b[0] = 1;
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Now, in Unix nanoseconds: spans time the operator's tool calls, never page-visible.
#[allow(clippy::disallowed_methods)]
pub fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traceparents_are_read_strictly() {
        let p = parent("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
        assert_eq!(
            p,
            Some((
                "4bf92f3577b34da6a3ce929d0e0e4736".into(),
                "00f067aa0ba902b7".into()
            ))
        );
        for bad in [
            "",
            "01-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
        ] {
            assert_eq!(parent(bad), None, "{bad}");
        }
    }

    #[test]
    fn ids_have_their_length() {
        assert_eq!(random_id(16).len(), 32);
        assert_eq!(random_id(8).len(), 16);
    }
}
