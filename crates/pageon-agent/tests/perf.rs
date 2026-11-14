//! Serialization speed on real pages (plan: under 20 ms per page in release builds).
//! Run with `cargo test --release -p pageon-agent --test perf -- --ignored`.
// A timing harness measures wall time on purpose; it is not engine code.
#![allow(clippy::disallowed_methods)]

use std::path::Path;
use std::time::Instant;

use pageon_agent::{Budget, Capture, ax};

#[test]
#[ignore = "timing; run in release"]
fn ax_is_fast_on_real_pages() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let budget = Budget::o200k(4000);
    let mut worst = (0.0_f64, String::new());
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if !name.starts_with("real-") {
            continue;
        }
        let capture = Capture::load(&p).unwrap_or_else(|e| panic!("{e}"));
        let _ = ax(&capture, budget); // warm the tokenizer
        let t = Instant::now();
        for _ in 0..5 {
            let _ = ax(&capture, budget);
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / 5.0;
        println!("{name}: {ms:.1} ms");
        if ms > worst.0 {
            worst = (ms, name);
        }
    }
    assert!(
        worst.0 < 20.0,
        "slowest page {} took {:.1} ms",
        worst.1,
        worst.0
    );
}
