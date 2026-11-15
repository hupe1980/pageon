//! Dedicated workers: the worker global, messages and transfer, timers and `fetch` on the
//! page's virtual clock, `importScripts`, module, `blob:` and `data:` workers, errors and ends —
//! in-process, caged and replayed.

mod common;

use common::*;
use pageon::replay::{Caged, InProcess, rerun};
use pageon::{LoadOptions, Recording, Replay, Sandbox, SandboxOptions, Session};

const DIR: &str = "fixtures/workers";

fn js(url: &str, body: &str) -> pageon_env::Exchange {
    page(url, 200, &[("content-type", "text/javascript")], body)
}

fn checks_site() -> pageon_env::Log {
    site(vec![
        html(
            "https://w.test/",
            include_str!("fixtures/workers/checks.html"),
        ),
        js(
            "https://w.test/worker.js",
            include_str!("fixtures/workers/worker.js"),
        ),
        js(
            "https://w.test/helpers.js",
            include_str!("fixtures/workers/helpers.js"),
        ),
        js(
            "https://w.test/module.js",
            include_str!("fixtures/workers/module.js"),
        ),
        js(
            "https://w.test/lib.js",
            include_str!("fixtures/workers/lib.js"),
        ),
        js(
            "https://w.test/worker.js",
            include_str!("fixtures/workers/worker.js"),
        ),
        page(
            "https://w.test/data.txt",
            200,
            &[("content-type", "text/plain")],
            "hello from data",
        ),
    ])
}

fn results(out: &str) -> Vec<(String, bool)> {
    serde_json::from_str(out).unwrap_or_else(|e| panic!("{e}: {out}"))
}

fn failed(results: &[(String, bool)]) -> Vec<&String> {
    results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n)
        .collect()
}

#[test]
fn the_worker_checks_pass() {
    let _ = DIR;
    let env = Replay::new(checks_site());
    let doc = pageon::load("https://w.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = text_of(&doc, "out");
    let r = results(&out);
    assert!(
        r.len() >= 24,
        "{} checks: {out}\n{:#?}",
        r.len(),
        doc.errors()
    );
    assert!(
        failed(&r).is_empty(),
        "failed: {:#?}\n{:#?}",
        failed(&r),
        doc.errors()
    );
}

fn sandbox<E: pageon_env::Env>(env: E) -> Sandbox<E> {
    Sandbox::new(
        env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn the_worker_checks_pass_caged() {
    let env = Replay::new(checks_site());
    let mut sb = sandbox(&env);
    let mut tab = sb
        .open("https://w.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let text = tab
        .view(
            pageon::Dump::Text,
            pageon::Viewport::default(),
            pageon::Budget::o200k(100_000),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let json = text
        .find("[[")
        .map(|i| text[i..].lines().next().unwrap_or_default())
        .unwrap_or_else(|| panic!("{text}"));
    let r = results(json);
    assert!(r.len() >= 24 && failed(&r).is_empty(), "{:#?}", failed(&r));
}

#[test]
fn comlink_calls_into_a_worker() {
    let lib = include_str!("fixtures/workers/comlink.min.js");
    let env = Replay::new(site(vec![
        html(
            "https://c.test/",
            include_str!("fixtures/workers/comlink.html"),
        ),
        js("https://c.test/comlink.min.js", lib),
        js(
            "https://c.test/comlink-worker.js",
            include_str!("fixtures/workers/comlink-worker.js"),
        ),
        js("https://c.test/comlink.min.js", lib),
    ]));
    let doc = pageon::load("https://c.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(&doc, "out"), "5 DONE nope", "{:#?}", doc.errors());
}

#[test]
fn a_session_with_workers_replays_caged_and_in_process() {
    let rec = Recording::new(Replay::new(checks_site()));
    {
        let session = Session::new(&rec);
        let mut tab = session
            .open("https://w.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = tab.observe();
    }
    let log = rec.log();
    let last = log.steps.len() - 1;
    for caged in [false, true] {
        let again = Recording::resume(log.clone(), last, Replay::new(log.clone()), true)
            .unwrap_or_else(|e| panic!("{e}"));
        let r = if caged {
            let mut sb = sandbox(&again);
            rerun(&mut Caged::new(&mut sb), &log.steps, last, &mut |_, _| {})
        } else {
            let session = Session::new(&again);
            rerun(
                &mut InProcess::new(&session),
                &log.steps,
                last,
                &mut |_, _| {},
            )
        };
        r.unwrap_or_else(|d| panic!("caged={caged}: {}", pageon::replay::report(&d)));
    }
}

#[test]
#[ignore = "timing: run in release (cargo test --release -- --ignored)"]
fn the_first_worker_is_cheap() {
    let with = "<p id=out></p><script>const w = new Worker('data:text/javascript,postMessage(1)'); \
        w.onmessage = () => out.textContent = 'ok'</script>";
    let without = "<p id=out></p><script>setTimeout(() => out.textContent = 'ok')</script>";
    let time = |body: &str| {
        (0..5)
            .map(|_| {
                let env = Replay::new(site(vec![html("https://t.test/", body)]));
                #[allow(clippy::disallowed_methods)]
                let t = std::time::Instant::now();
                let doc = pageon::load("https://t.test/", &env, LoadOptions::default())
                    .unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(text_of(&doc, "out"), "ok");
                t.elapsed()
            })
            .min()
            .unwrap_or_default()
    };
    let (a, b) = (time(without), time(with));
    let extra = b.saturating_sub(a);
    eprintln!("without {a:?}; with {b:?}; a worker {extra:?}");
    assert!(extra.as_millis() < 5, "{extra:?}");
}

#[test]
fn limits_hold() {
    let page_html = "<p id=out></p><script>\
        const ws = []; let error = '';\
        try { for (let i = 0; i < 20; i++) ws.push(new Worker('data:text/javascript,0')); } catch (e) { error = e.name; }\
        for (const w of ws) w.terminate();\
        const flood = new Worker('data:text/javascript,for (let i = 0; i < 5000; i++) postMessage(i)');\
        let n = 0; flood.onmessage = () => n++;\
        setTimeout(() => out.textContent = [ws.length, error, n].join(' '), 100);\
        </script>";
    let env = Replay::new(site(vec![html("https://l.test/", page_html)]));
    let doc = pageon::load("https://l.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    // 16 workers, then QuotaExceededError; terminated ones make room; a flood is cut at 4096.
    assert_eq!(text_of(&doc, "out"), "16 QuotaExceededError 4096");
    assert!(
        doc.errors()
            .iter()
            .any(|e| e.message.contains("4096 worker messages")),
        "{:#?}",
        doc.errors()
    );
}
