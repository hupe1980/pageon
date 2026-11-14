//! Shared helpers: recordings built in code, so each test reads as a specification.
#![allow(dead_code)]

use std::cell::RefCell;

use pageon::Replay;
use pageon_env::{Env, Exchange, Log, NetError, Request, Response};

pub fn page(url: &str, status: u16, headers: &[(&str, &str)], body: &str) -> Exchange {
    Exchange {
        method: "GET".into(),
        url: url.into(),
        status,
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
        body: Some(body.into()),
        ..Exchange::default()
    }
}

pub fn bytes(url: &str, content_type: &str, body: &[u8]) -> Exchange {
    use base64::Engine as _;
    Exchange {
        method: "GET".into(),
        url: url.into(),
        status: 200,
        headers: vec![("content-type".into(), content_type.into())],
        body: None,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(body)),
        ..Exchange::default()
    }
}

pub fn redirect(from: &str, to: &str) -> Exchange {
    page(from, 301, &[("location", to)], "")
}

pub fn log(exchanges: Vec<Exchange>) -> Log {
    Log {
        exchanges,
        clock: vec![1_700_000_000_000, 1_700_000_000_042],
        ..Log::default()
    }
}

/// Writes a recording to a unique temp file and returns its path.
pub fn file(name: &str, log: &Log) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pageon-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let p = dir.join(format!("{name}.rec.json"));
    std::fs::write(&p, serde_json::to_string_pretty(log).unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));
    p
}

pub const ARTICLE: &str = "<!DOCTYPE html><html><head><title>Article</title></head><body>\
<header><a href=\"/\">Home</a></header><main><h1>Title</h1><p>First <b>bold</b> paragraph.</p>\
<ul><li>one</li><li>two</li></ul></main></body></html>";

/// Load options for the corpus: its pages were captured from `*.localhost`, which pageon
/// refuses by default (private network).
pub fn corpus_options() -> pageon::LoadOptions {
    pageon::LoadOptions {
        allow_private_network: true,
        ..pageon::LoadOptions::default()
    }
}

/// Where the corpus pages were captured from.
pub const ORIGIN: &str = "http://a.localhost:47811/pages/";
/// The second corpus origin.
pub const ORIGIN_B: &str = "http://b.localhost:47811/pages/";

/// The repository root.
pub fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every corpus fixture page (`bench/fixtures/pages/`), served at its capture URL; other URLs
/// fail like unreachable hosts.
pub fn fixtures() -> Log {
    let dir = root().join("bench/fixtures/pages");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(Result::ok)
        .collect();
    names.sort_by_key(|e| e.file_name());
    // Served at both corpus origins (`b.localhost` is the cross-origin frame's host).
    let mut ex = Vec::new();
    for e in names {
        let name = e.file_name().to_string_lossy().to_string();
        let body = std::fs::read_to_string(e.path()).unwrap_or_else(|e| panic!("{e}"));
        for origin in [ORIGIN, ORIGIN_B] {
            ex.push(page(
                &format!("{origin}{name}"),
                200,
                &[("content-type", "text/html; charset=utf-8")],
                &body,
            ));
        }
    }
    let mut log = log(ex);
    log.clock = (0..256).map(|i| 1_700_000_000_000 + i).collect();
    log.random = (1..=64)
        .map(|i: u64| i.wrapping_mul(0x9e37_79b9_7f4a_7c15))
        .collect();
    log
}

/// Replays a log and keeps every request it was asked for.
pub struct Capture {
    pub replay: Replay,
    pub seen: RefCell<Vec<Request>>,
}

impl Capture {
    pub fn new(log: Log) -> Self {
        Self {
            replay: Replay::new(log),
            seen: RefCell::new(Vec::new()),
        }
    }

    /// The headers of the first request for `url` (with `method`).
    pub fn headers(&self, method: &str, url: &str) -> Vec<(String, String)> {
        self.seen
            .borrow()
            .iter()
            .find(|r| r.method == method && r.url == url)
            .map(|r| r.headers.clone())
            .unwrap_or_else(|| panic!("no {method} {url} in {:#?}", self.urls()))
    }

    pub fn header(&self, method: &str, url: &str, name: &str) -> Option<String> {
        self.headers(method, url)
            .into_iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v)
    }

    pub fn urls(&self) -> Vec<String> {
        self.seen
            .borrow()
            .iter()
            .map(|r| format!("{} {}", r.method, r.url))
            .collect()
    }
}

impl Env for Capture {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        self.seen.borrow_mut().push(req.clone());
        self.replay.fetch(req)
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        self.replay.now_ms()
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        self.replay.random_u64()
    }
}

pub fn ex(method: &str, url: &str, status: u16, headers: &[(&str, &str)], body: &str) -> Exchange {
    Exchange {
        method: method.into(),
        ..page(url, status, headers, body)
    }
}

pub fn html(url: &str, body: &str) -> Exchange {
    page(url, 200, &[("content-type", "text/html")], body)
}

pub fn json(url: &str, headers: &[(&str, &str)], body: &str) -> Exchange {
    let mut h = vec![("content-type", "application/json")];
    h.extend_from_slice(headers);
    page(url, 200, &h, body)
}

pub fn site(exchanges: Vec<Exchange>) -> Log {
    let mut l = log(exchanges);
    l.clock = (0..256).map(|i| 1_800_000_000_000 + i * 1000).collect();
    l.random = (1..=64).collect();
    l
}

pub const MINIWOB_ORIGIN: &str = "http://miniwob.test/";
pub const MINIWOB_START: &str =
    "<script>addEventListener('load', function () { core.startEpisodeReal(); });</script>";

pub fn miniwob_dir() -> std::path::PathBuf {
    root().join("bench/.cache/miniwob/miniwob/html")
}

fn miniwob_files(
    base: &std::path::Path,
    rel: &std::path::Path,
    out: &mut Vec<pageon_env::Exchange>,
) {
    let Ok(entries) = std::fs::read_dir(base.join(rel)) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = rel.join(e.file_name());
        if path.is_dir() {
            miniwob_files(base, &name, out);
            continue;
        }
        let url = format!(
            "{MINIWOB_ORIGIN}{}",
            name.to_string_lossy().replace('\\', "/")
        );
        let ext = path
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or_default();
        let ct = match ext {
            "html" => "text/html; charset=utf-8",
            "js" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "json" => "application/json",
            _ => "application/octet-stream",
        };
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        if ext == "html" && name.starts_with("miniwob") {
            let text = String::from_utf8_lossy(&data)
                .replace("</body>", &format!("{MINIWOB_START}</body>"));
            out.push(page(&url, 200, &[("content-type", ct)], &text));
        } else {
            out.push(bytes(&url, ct, &data));
        }
    }
}

/// The MiniWoB++ pages of the BrowserGym copy (`uv run pageon-bench setup`), served at
/// [`MINIWOB_ORIGIN`]; `None` without them.
pub fn miniwob_log() -> Option<pageon_env::Log> {
    let base = miniwob_dir();
    if !base.join("miniwob").is_dir() {
        return None;
    }
    let mut ex = Vec::new();
    miniwob_files(&base, std::path::Path::new(""), &mut ex);
    let mut log = log(ex);
    log.clock = (0..64).map(|i| 1_700_000_000_000 + i).collect();
    log.random = (0..64)
        .map(|i| 0x9e37_79b9_7f4a_7c15_u64.wrapping_mul(i + 1))
        .collect();
    Some(log)
}

/// The task names, sorted.
pub fn miniwob_tasks() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(miniwob_dir().join("miniwob"))
        .map(|d| {
            d.flatten()
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".html"))
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The quoted strings of a line, unescaped.
pub fn quoted(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (&mut cur, c) {
            (Some(s), '\\') => {
                if let Some(n) = chars.next() {
                    s.push(n);
                }
            }
            (Some(_), '"') => out.extend(cur.take()),
            (Some(s), c) => s.push(c),
            (None, '"') => cur = Some(String::new()),
            (None, _) => {}
        }
    }
    out
}

/// `(ref, name)` of every element with this role, in order.
pub fn refs(obs: &str, role: &str) -> Vec<(String, String)> {
    obs.lines()
        .filter_map(|l| {
            let rest = l.trim_start().strip_prefix(role)?.strip_prefix(' ')?;
            let r = rest.split_whitespace().next()?;
            r.strip_prefix('e')?.parse::<u32>().ok()?;
            Some((
                r.to_string(),
                quoted(rest).into_iter().next().unwrap_or_default(),
            ))
        })
        .collect()
}

/// The ref of the element with this role and name.
pub fn find(obs: &str, role: &str, name: &str) -> String {
    refs(obs, role)
        .into_iter()
        .find(|(_, n)| n == name)
        .map(|(r, _)| r)
        .unwrap_or_else(|| panic!("no {role} {name:?} in\n{obs}"))
}

pub fn text_of(doc: &pageon::Document, id: &str) -> String {
    let dom = doc.dom();
    dom.traverse(dom.root())
        .find_map(|e| match e {
            pageon_dom::Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some(id) => {
                Some(dom.text(n))
            }
            _ => None,
        })
        .unwrap_or_default()
}
