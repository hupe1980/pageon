//! Hostile scripts end the script, never the engine.
// Timing a load is the point of these tests.
#![allow(clippy::disallowed_methods)]

mod common;

use common::*;
use pageon::{Document, LoadOptions, PageErrorKind, Replay};

fn load(html: &str) -> (Document, f64) {
    let mut l = log(vec![page(
        "https://a.test/",
        200,
        &[("content-type", "text/html")],
        html,
    )]);
    l.clock = (0..16).collect();
    l.random = vec![7; 20]; // a seed and a 256-bit crypto key per document
    let t = std::time::Instant::now();
    let doc = pageon::load("https://a.test/", &Replay::new(l), LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    (doc, t.elapsed().as_secs_f64())
}

fn kinds(doc: &Document) -> Vec<PageErrorKind> {
    doc.errors().iter().map(|e| e.kind).collect()
}

#[test]
fn endless_loop_hits_the_time_limit() {
    let (doc, secs) = load("<script>while (true) {}</script><p id=after>after</p>");
    assert!(secs < 3.0, "{secs:.1} s");
    assert_eq!(kinds(&doc), [PageErrorKind::TimeLimit]);
    assert!(
        doc.dump(pageon::Dump::Html).contains("after"),
        "parsing continued after the script"
    );
}

#[test]
fn allocation_bomb_hits_the_memory_limit() {
    let (doc, secs) = load(
        "<script>var a = []; while (true) a.push(new Array(1e6).fill(1.5));</script><p>after</p>",
    );
    assert!(secs < 3.0, "{secs:.1} s");
    assert!(
        kinds(&doc).contains(&PageErrorKind::MemoryLimit),
        "{:?}",
        doc.errors()
    );
}

#[test]
fn infinite_recursion_is_a_range_error() {
    let (doc, secs) = load(
        "<script>function f() { return f() + 1 } f()</script><script>document.title = 'still running'</script>",
    );
    assert!(secs < 3.0, "{secs:.1} s");
    assert_eq!(kinds(&doc), [PageErrorKind::Exception]);
    assert!(
        doc.errors()[0].message.contains("call stack"),
        "{:?}",
        doc.errors()
    );
    assert!(doc.dump(pageon::Dump::Html).contains("still running"));
}

#[test]
fn timer_storms_stop_at_the_settle_window() {
    let (doc, secs) = load(
        "<script>setInterval(() => {}, 0); (function again() { setTimeout(again, 0) })();</script>",
    );
    assert!(secs < 3.0, "{secs:.1} s");
    assert!(doc.summary().pending_timers >= 1);
}

#[test]
fn a_load_after_a_terminated_one_works() {
    let _ = load("<script>while (true) {}</script>");
    let (doc, _) = load("<script>document.title = 'fine'</script>");
    assert!(doc.errors().is_empty(), "{:?}", doc.errors());
    assert!(doc.dump(pageon::Dump::Html).contains("<title>fine</title>"));
}
