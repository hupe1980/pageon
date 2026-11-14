//! `--dump boxes` over a recording: stylesheets through the environment, flags, determinism.

mod common;

use common::*;
use pageon::{LoadOptions, Replay, Viewport};

const PAGE: &str = r#"<!DOCTYPE html><html><head><link rel="stylesheet" href="/s.css"><link rel="stylesheet" href="/missing.css"></head>
<body><main><h1>Shop</h1><button id="buy">Buy</button><input name="trap" style="opacity:0">
<div id="overlay" style="position:fixed;inset:0"></div></main><footer style="margin-top:2000px"><a href="/t">Terms</a></footer></body></html>"#;

fn rec() -> pageon_env::Log {
    log(vec![
        page(
            "https://a.test/",
            200,
            &[("content-type", "text/html")],
            PAGE,
        ),
        page(
            "https://a.test/s.css",
            200,
            &[("content-type", "text/css")],
            "h1 { margin: 0 0 30px 40px }",
        ),
        page("https://a.test/missing.css", 404, &[], "nope"),
    ])
}

#[test]
fn boxes_dump_with_stylesheets_and_flags() {
    let doc = pageon::load(
        "https://a.test/",
        &Replay::new(rec()),
        LoadOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        (doc.summary().stylesheets, doc.summary().stylesheets_failed),
        (1, 1)
    );
    let boxes = doc.boxes(Viewport {
        width: 800,
        height: 600,
    });
    assert!(
        boxes.contains("h1 48,"),
        "external stylesheet applied:\n{boxes}"
    );
    assert!(
        boxes.contains("button") && boxes.contains("covered-by:div"),
        "{boxes}"
    );
    assert!(boxes.contains("name=\"trap\" hidden"), "{boxes}");
    assert!(boxes.contains("offscreen"), "{boxes}");
    insta::assert_snapshot!(boxes);
}

#[test]
fn boxes_are_deterministic() {
    let first = pageon::load(
        "https://a.test/",
        &Replay::new(rec()),
        LoadOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"))
    .boxes(Viewport::default());
    for _ in 0..100 {
        let d = pageon::load(
            "https://a.test/",
            &Replay::new(rec()),
            LoadOptions::default(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(d.boxes(Viewport::default()), first);
    }
}
