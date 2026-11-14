//! Speed (release): a typical page under 15 ms, 3,000 elements under 50 ms.
//! Run with `cargo test --release -p pageon-layout --test perf -- --ignored --nocapture`.
// A timing harness measures wall time on purpose; it is not engine code.
#![allow(clippy::disallowed_methods)]

use pageon_dom::{ParseOptions, parse_document};
use pageon_layout::{Layout, Viewport};

fn time(html: &str) -> f64 {
    let dom = parse_document(html, ParseOptions::default());
    let _ = Layout::new(&dom, Viewport::default(), &[], None);
    let t = std::time::Instant::now();
    for _ in 0..5 {
        let _ = Layout::new(&dom, Viewport::default(), &[], None);
    }
    t.elapsed().as_secs_f64() * 1000.0 / 5.0
}

#[test]
#[ignore = "timing; run in release"]
fn layout_is_fast() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/geometry");
    let typical = std::fs::read_to_string(dir.join("checkout.html")).unwrap_or_default();
    let big = std::fs::read_to_string(dir.join("long-list.html")).unwrap_or_default();
    let (t, b) = (time(&typical), time(&big));
    println!("typical page {t:.1} ms, 3,000 elements {b:.1} ms");
    assert!(t < 15.0 && b < 50.0);
}
