//! Speed (release only): a 1 MB page parses in under 50 ms; a replayed load in under 100 ms.
//! Run with `cargo test --release -p pageon --test perf -- --ignored --nocapture`.
// A timing harness measures wall time on purpose; it is not engine code.
#![allow(clippy::disallowed_methods)]

mod common;

use pageon::{LoadOptions, Replay};

fn big_page() -> String {
    let mut s = String::from("<!DOCTYPE html><html><head><title>Big</title></head><body><main>");
    let mut i = 0;
    while s.len() < 1_000_000 {
        s.push_str(&format!(
            "<article class=\"card\" id=\"c{i}\"><h2>Item {i}</h2><p>Some <b>text</b> with a <a href=\"/p/{i}\">link</a> &amp; entity.</p><ul><li>a</li><li>b</li></ul></article>"
        ));
        i += 1;
    }
    s + "</main></body></html>"
}

#[test]
#[ignore = "timing; run in release"]
fn one_megabyte_parses_fast() {
    let html = big_page();
    let _ = pageon::parse(&html, "https://a.test/");
    let t = std::time::Instant::now();
    for _ in 0..5 {
        let _ = pageon::parse(&html, "https://a.test/");
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / 5.0;
    println!("parse 1 MB: {ms:.1} ms");
    assert!(ms < 50.0, "{ms:.1} ms");

    let l = common::log(vec![common::page(
        "https://a.test/",
        200,
        &[("content-type", "text/html")],
        common::ARTICLE,
    )]);
    let t = std::time::Instant::now();
    for _ in 0..20 {
        let _ = pageon::load(
            "https://a.test/",
            &Replay::new(l.clone()),
            LoadOptions::default(),
        );
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / 20.0;
    println!("replayed load: {ms:.2} ms");
    assert!(ms < 100.0);
}
