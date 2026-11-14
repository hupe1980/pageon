//! MiniWoB++: every task page of the pinned BrowserGym copy loads, runs
//! jQuery/d3/MiniWoB core, starts its episode (as BrowserGym does, after `load`) and fills its
//! query, with no uncaught script error.
//!
//! Needs `uv run pageon-bench setup` (the pages live in `bench/.cache/miniwob`); without them
//! the test prints how to get them and passes.

mod common;

use common::*;
use pageon::{LoadOptions, Replay};

fn query(doc: &pageon::Document) -> String {
    let dom = doc.dom();
    dom.traverse(dom.root())
        .find_map(|e| match e {
            pageon_dom::Edge::Enter(n)
                if dom.get(n).and_then(|x| x.attr("id")) == Some("query") =>
            {
                Some(dom.text(n))
            }
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn miniwob_pages_load_and_start() {
    let Some(rec) = miniwob_log() else {
        println!("skipped: run `uv run pageon-bench setup` in bench/ to fetch MiniWoB++");
        return;
    };
    let all = miniwob_tasks();
    let mut ok = 0;
    let mut failures = Vec::new();
    for t in &all {
        let url = format!("{MINIWOB_ORIGIN}miniwob/{t}.html");
        let doc = match pageon::load(&url, &Replay::new(rec.clone()), LoadOptions::default()) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{t}: load failed: {e}"));
                continue;
            }
        };
        let q = query(&doc);
        match (doc.errors().first(), q.trim().is_empty()) {
            (None, false) => ok += 1,
            (Some(e), _) => failures.push(format!("{t}: {e}")),
            (None, true) => failures.push(format!("{t}: empty #query")),
        }
    }
    let pct = 100.0 * ok as f64 / all.len().max(1) as f64;
    println!(
        "MiniWoB++: {ok}/{} pages load with no error and a query ({pct:.1} %)",
        all.len()
    );
    for f in &failures {
        println!("  {f}");
    }
    assert!(pct >= 90.0, "needs ≥ 90 %");
}

#[test]
fn replayed_loads_are_identical() {
    let Some(rec) = miniwob_log() else { return };
    let url = format!("{MINIWOB_ORIGIN}miniwob/click-button.html");
    let a = pageon::load(&url, &Replay::new(rec.clone()), LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let b = pageon::load(&url, &Replay::new(rec), LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(a.dump(pageon::Dump::Html), b.dump(pageon::Dump::Html));
    assert_eq!(a.dump(pageon::Dump::Ax), b.dump(pageon::Dump::Ax));
    assert!(query(&a).starts_with("Click on the"), "{}", query(&a));
}

#[test]
#[ignore = "timing; run in release"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn click_button_is_fast() {
    let Some(rec) = miniwob_log() else { return };
    let url = format!("{MINIWOB_ORIGIN}miniwob/click-button.html");
    let _ = pageon::load(&url, &Replay::new(rec.clone()), LoadOptions::default());
    let t = std::time::Instant::now();
    for _ in 0..5 {
        let _ = pageon::load(&url, &Replay::new(rec.clone()), LoadOptions::default());
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / 5.0;
    println!("click-button: load, scripts and settle in {ms:.1} ms");
    assert!(ms < 150.0, "needs < 150 ms");
}
