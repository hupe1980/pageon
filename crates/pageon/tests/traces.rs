//! Playwright traces of replayed recordings: the zip and its events, caged and in-process,
//! a diverging replay, and the cost per step. (`conformance/playwright/test/trace.test.mjs`
//! opens one in Playwright's own viewer.)

mod common;

use common::*;
use pageon::replay::{Caged, InProcess};
use pageon::{LoadOptions, Recording, Replay, Sandbox, SandboxOptions, Session};
use serde_json::Value;

const SHOP: &str = "<!doctype html><title>Shop</title><h1>Shop</h1><input aria-label=Name>\
    <button onclick=\"fetch('/api').then(r => r.text()).then(t => { out.textContent = t; console.warn('got', t) })\">Load</button>\
    <a href=/next>Next</a><p id=out></p>";

fn site_log() -> pageon_env::Log {
    site(vec![
        html("https://s.test/", SHOP),
        page(
            "https://s.test/api",
            200,
            &[("content-type", "text/plain")],
            "fresh",
        ),
        html(
            "https://s.test/next",
            "<title>Next</title><p>Second page</p>",
        ),
    ])
}

/// Records an episode: open, type, click (a fetch and a console message), follow a link.
fn episode() -> pageon_env::Log {
    let rec = Recording::new(Replay::new(site_log()));
    {
        let s = Session::new(&rec);
        let mut tab = s
            .open("https://s.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        for a in ["type e1 \"Ada\"", "click e2", "click e3"] {
            let _ = tab.observe();
            tab.act(a.parse().unwrap_or_else(|e| panic!("{e}")))
                .unwrap_or_else(|e| panic!("{a}: {e}"));
        }
    }
    rec.log()
}

/// The entries of a zip of stored entries, by name.
fn unzip(bytes: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]) as usize;
    let u32_at = |i: usize| {
        u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize
    };
    let mut out = std::collections::BTreeMap::new();
    let mut i = 0;
    while i + 30 <= bytes.len() && u32_at(i) == 0x0403_4b50 {
        let size = u32_at(i + 18);
        let n = u16_at(i + 26);
        let extra = u16_at(i + 28);
        let name = String::from_utf8_lossy(&bytes[i + 30..i + 30 + n]).into_owned();
        let start = i + 30 + n + extra;
        out.insert(name, bytes[start..start + size].to_vec());
        i = start + size;
    }
    out
}

fn trace_of(caged: bool, log: &pageon_env::Log) -> (Vec<u8>, Option<pageon_env::Divergence>) {
    let dir = std::env::temp_dir().join(format!("pageon-trace-{}-{caged}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let path = dir.join("trace.zip");
    let last = log.steps.len() - 1;
    let again = Recording::resume(log.clone(), last, Replay::new(log.clone()), true)
        .unwrap_or_else(|e| panic!("{e}"));
    let div = if caged {
        let mut sb = Sandbox::new(
            &again,
            SandboxOptions {
                program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                ..SandboxOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        pageon::trace::write(&mut Caged::new(&mut sb), log, last, &path, &mut |_, _| {})
    } else {
        let s = Session::new(&again);
        pageon::trace::write(&mut InProcess::new(&s), log, last, &path, &mut |_, _| {})
    }
    .unwrap_or_else(|e| panic!("{e}"));
    (std::fs::read(&path).unwrap_or_else(|e| panic!("{e}")), div)
}

fn lines(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

#[test]
fn a_replay_writes_a_playwright_trace() {
    let log = episode();
    let (zip, div) = trace_of(false, &log);
    assert!(div.is_none(), "{div:?}");
    let files = unzip(&zip);
    let trace = lines(&files["trace.trace"]);
    assert_eq!(trace[0]["type"], "context-options");
    assert_eq!(trace[0]["version"], 10);
    let of = |t: &str| trace.iter().filter(|e| e["type"] == t).collect::<Vec<_>>();
    let titles: Vec<&str> = of("before")
        .iter()
        .map(|e| e["title"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        titles,
        [
            "open https://s.test/",
            "type e1 \"Ada\"",
            "click e2",
            "click e3"
        ]
    );
    assert_eq!(of("after").len(), 4);
    // Each action has the page after it, and a screencast frame that is in the zip.
    let snaps = of("frame-snapshot");
    assert_eq!(snaps.len(), 4);
    assert!(
        snaps
            .iter()
            .all(|s| s["snapshot"]["isMainFrame"] == true && s["snapshot"]["phase"] == "after")
    );
    assert!(snaps[2]["snapshot"]["html"].to_string().contains("fresh"));
    assert!(
        snaps[3]["snapshot"]["html"]
            .to_string()
            .contains("Second page")
    );
    for f in of("screencast-frame") {
        assert!(
            files.contains_key(f["file"].as_str().unwrap_or_default()),
            "{f}"
        );
    }
    // The console message of the click, and every recorded exchange with its body.
    assert!(
        of("console")
            .iter()
            .any(|c| c["text"] == "got fresh" && c["messageType"] == "warning")
    );
    let network = lines(&files["trace.network"]);
    assert_eq!(network.len(), log.exchanges.len());
    let api = network
        .iter()
        .find(|n| n["snapshot"]["request"]["url"] == "https://s.test/api")
        .unwrap_or_else(|| panic!("{network:#?}"));
    let body = api["snapshot"]["response"]["content"]["_file"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(files[body], b"fresh");
    // Times move forward.
    let starts: Vec<f64> = of("before")
        .iter()
        .map(|e| e["startTime"].as_f64().unwrap_or(-1.0))
        .collect();
    assert!(starts.windows(2).all(|w| w[0] < w[1]), "{starts:?}");
}

#[test]
fn caged_and_in_process_traces_are_the_same() {
    let log = episode();
    let (a, _) = trace_of(false, &log);
    let (b, _) = trace_of(true, &log);
    let (a, b) = (unzip(&a), unzip(&b));
    assert_eq!(
        String::from_utf8_lossy(&a["trace.trace"]),
        String::from_utf8_lossy(&b["trace.trace"])
    );
    assert_eq!(a["trace.network"], b["trace.network"]);
}

#[test]
fn a_diverging_replay_ends_with_the_divergence() {
    let mut log = episode();
    log.steps[2].hash = "0000000000000000".into();
    let (zip, div) = trace_of(false, &log);
    let div = div.unwrap_or_else(|| panic!("no divergence"));
    assert_eq!(div.step, 2);
    let trace = lines(&unzip(&zip)["trace.trace"]);
    let afters: Vec<&Value> = trace.iter().filter(|e| e["type"] == "after").collect();
    assert_eq!(afters.len(), 3);
    assert!(
        afters[2]["error"]["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "{:?}",
        afters[2]
    );
}

#[test]
#[ignore = "timing: run in release (cargo test --release -- --ignored)"]
fn a_trace_costs_little_per_step() {
    let log = episode();
    let replay_only = || {
        let last = log.steps.len() - 1;
        let again = Recording::resume(log.clone(), last, Replay::new(log.clone()), true)
            .unwrap_or_else(|e| panic!("{e}"));
        let s = Session::new(&again);
        #[allow(clippy::disallowed_methods)]
        let t = std::time::Instant::now();
        pageon::replay::rerun(&mut InProcess::new(&s), &log.steps, last, &mut |_, _| {})
            .unwrap_or_else(|d| panic!("{}", pageon::replay::report(&d)));
        t.elapsed()
    };
    let traced = || {
        #[allow(clippy::disallowed_methods)]
        let t = std::time::Instant::now();
        let _ = trace_of(false, &log);
        t.elapsed()
    };
    let a = (0..3).map(|_| replay_only()).min().unwrap_or_default();
    let b = (0..3).map(|_| traced()).min().unwrap_or_default();
    let per = b.saturating_sub(a) / log.steps.len() as u32;
    eprintln!("replay {a:?}; with trace {b:?}; per step {per:?}");
    assert!(per.as_millis() < 20, "{per:?}");
}

#[test]
fn a_trace_carries_no_secret() {
    const SECRET: &str = "s3cret-value-9271";
    let page_html = "<input aria-label=Key><button onclick=\"out.textContent = 'key ' + document.querySelector('input').value; \
        fetch('/save', {method: 'POST', body: document.querySelector('input').value})\">Save</button><p id=out></p>";
    let log = site(vec![
        html("https://k.test/", page_html),
        ex(
            "POST",
            "https://k.test/save",
            200,
            &[("content-type", "text/plain")],
            "saved",
        ),
    ]);
    let vault = |env| {
        pageon::Secrets::new(env).with(
            "key",
            pageon::Secret::value(SECRET).origins(["https://k.test"]),
        )
    };
    let rec = Recording::new(Replay::new(log));
    {
        let v = vault(&rec);
        let s = Session::new(&v);
        let mut tab = s
            .open("https://k.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        for a in ["type e1 secret://key", "click e2"] {
            let _ = tab.observe();
            tab.act(a.parse().unwrap_or_else(|e| panic!("{e}")))
                .unwrap_or_else(|e| panic!("{a}: {e}"));
        }
        // As a saved recording is: every value the vault handed out redacted.
        drop(tab);
        rec.redact(v.values());
    }
    let log = rec.log();
    let last = log.steps.len() - 1;
    let again = Recording::resume(log.clone(), last, Replay::new(log.clone()), true)
        .unwrap_or_else(|e| panic!("{e}"));
    let v = vault(&again);
    let s = Session::new(&v);
    let path = std::env::temp_dir().join(format!("pageon-trace-secret-{}.zip", std::process::id()));
    let div = pageon::trace::write(&mut InProcess::new(&s), &log, last, &path, &mut |_, _| {})
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(div.is_none(), "{div:?}");
    let zip = std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"));
    let files = unzip(&zip);
    for (name, bytes) in &files {
        assert!(
            !String::from_utf8_lossy(bytes).contains(SECRET),
            "{name} carries the secret"
        );
    }
    // The page showed it: the snapshot shows the handle instead.
    assert!(String::from_utf8_lossy(&files["trace.trace"]).contains("secret://key"));
}
