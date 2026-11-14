//! Speed (release): layout, accessibility tree and the `ax` view together under 30 ms for a
//! typical page and under 150 ms for 3,000 elements.
//! Run with `cargo test --release -p pageon --test observe_perf -- --ignored --nocapture`.
// A timing harness measures wall time on purpose; it is not engine code.
#![allow(clippy::disallowed_methods)]

use pageon::{Budget, Viewport};

fn time(name: &str) -> f64 {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/fixtures/pages");
    let html = std::fs::read_to_string(dir.join(name)).unwrap_or_else(|e| panic!("{e}"));
    let doc = pageon::parse(&html, "https://a.test/");
    let _ = doc.ax(Viewport::default(), Budget::o200k(4000));
    let t = std::time::Instant::now();
    for _ in 0..5 {
        let _ = doc.ax(Viewport::default(), Budget::o200k(4000));
    }
    t.elapsed().as_secs_f64() * 1000.0 / 5.0
}

#[test]
#[ignore = "timing; run in release"]
fn observation_is_fast() {
    let (typical, big) = (time("checkout.html"), time("long-list.html"));
    println!("typical page {typical:.1} ms, 3,000 elements {big:.1} ms");
    assert!(typical < 30.0 && big < 150.0);
}
