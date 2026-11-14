//! Loading through recordings: redirects, errors, encodings, content types, determinism, and a
//! live recording against a loopback stub (gzip) that replays identically.

mod common;

use std::io::{Read, Write};

use common::*;
use pageon::{Dump, LoadError, LoadOptions, Recording, Replay, load};
use pageon_env::Live;

fn replay(exchanges: Vec<pageon_env::Exchange>) -> Replay {
    Replay::new(log(exchanges))
}

#[test]
fn follows_redirects_and_reports_the_final_url() {
    let env = replay(vec![
        redirect("https://a.test/", "https://b.test/x"),
        page("https://b.test/x", 302, &[("location", "/y")], ""),
        page(
            "https://b.test/y",
            200,
            &[("content-type", "text/html; charset=utf-8")],
            ARTICLE,
        ),
    ]);
    let doc =
        load("https://a.test/", &env, LoadOptions::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(doc.final_url(), "https://b.test/y");
    assert_eq!(doc.summary().redirects, 2);
    assert_eq!(doc.summary().ms, 42);
    assert_eq!(doc.encoding().name, "UTF-8");
    assert_eq!(doc.encoding().source, "http");
    assert!(doc.dump(Dump::Text).contains("First bold paragraph."));
}

#[test]
fn redirect_loops_and_chains_are_errors() {
    let env = replay(vec![
        redirect("https://a.test/", "https://b.test/"),
        redirect("https://b.test/", "https://a.test/"),
    ]);
    assert!(matches!(
        load("https://a.test/", &env, LoadOptions::default()),
        Err(LoadError::RedirectLoop(_))
    ));
    let hops: Vec<_> = (0..25)
        .map(|i| {
            redirect(
                &format!("https://a.test/{i}"),
                &format!("https://a.test/{}", i + 1),
            )
        })
        .collect();
    assert!(matches!(
        load("https://a.test/0", &replay(hops), LoadOptions::default()),
        Err(LoadError::TooManyRedirects(_))
    ));
}

#[test]
fn error_pages_load_with_their_status() {
    let env = replay(vec![page(
        "https://a.test/",
        404,
        &[("content-type", "text/html")],
        "<h1>Not found</h1>",
    )]);
    let doc =
        load("https://a.test/", &env, LoadOptions::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(doc.status(), 404);
    assert_eq!(doc.dump(Dump::Text), "Not found");
}

#[test]
fn encodings_and_content_types() {
    let (sj, _, _) = encoding_rs::SHIFT_JIS.encode("<p>日本語のページ</p>");
    let env = replay(vec![bytes(
        "https://a.test/",
        "text/html; charset=Shift_JIS",
        &sj,
    )]);
    let doc =
        load("https://a.test/", &env, LoadOptions::default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        (doc.encoding().name, doc.dump(Dump::Text).as_str()),
        ("Shift_JIS", "日本語のページ")
    );

    let env = replay(vec![page(
        "https://a.test/t.txt",
        200,
        &[("content-type", "text/plain")],
        "line 1\n  line 2",
    )]);
    let doc = load("https://a.test/t.txt", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(doc.dump(Dump::Text), "line 1\n  line 2");

    let env = replay(vec![bytes(
        "https://a.test/f.pdf",
        "application/pdf",
        b"%PDF-1.7",
    )]);
    assert_eq!(
        load("https://a.test/f.pdf", &env, LoadOptions::default()).err(),
        Some(LoadError::UnsupportedContentType("application/pdf".into()))
    );
}

#[test]
fn bad_inputs_are_typed_errors() {
    let env = replay(vec![]);
    assert!(
        matches!(load("ftp://a.test/", &env, LoadOptions::default()), Err(LoadError::UnsupportedScheme(s)) if s == "ftp")
    );
    assert!(matches!(
        load("data:text/html,hi", &env, LoadOptions::default()),
        Err(LoadError::UnsupportedScheme(_))
    ));
    assert!(matches!(
        load("https://a.test/missing", &env, LoadOptions::default()),
        Err(LoadError::ReplayMiss(_))
    ));
    let e = load(
        "https://a.test/missing",
        &replay(vec![]),
        LoadOptions::default(),
    )
    .err()
    .unwrap_or_else(|| panic!());
    assert!(e.guidance().contains("--record"), "{}", e.guidance());
}

#[test]
fn replays_are_byte_identical() {
    let l = log(vec![page(
        "https://a.test/",
        200,
        &[("content-type", "text/html")],
        ARTICLE,
    )]);
    let first = load(
        "https://a.test/",
        &Replay::new(l.clone()),
        LoadOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    for _ in 0..100 {
        let d = load(
            "https://a.test/",
            &Replay::new(l.clone()),
            LoadOptions::default(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        for kind in [Dump::Text, Dump::Html, Dump::Tree] {
            assert_eq!(d.dump(kind), first.dump(kind));
        }
        assert_eq!(d.summary(), first.summary());
    }
}

/// One loopback HTTP/1.1 exchange serving `body` gzip-encoded.
fn serve_once(body: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = listener
        .local_addr()
        .unwrap_or_else(|e| panic!("{e}"))
        .port();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap_or_else(|e| panic!("{e}"));
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = gz.write_all(body.as_bytes());
        let gz = gz.finish().unwrap_or_default();
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-encoding: gzip\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            gz.len()
        );
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(&gz);
    });
    format!("http://127.0.0.1:{port}/")
}

#[test]
fn recording_a_live_load_replays_identically() {
    let url = serve_once(ARTICLE);
    let rec = Recording::new(Live::default());
    // A local test server is on the private network, which pageon refuses by default. It
    // answers one request, so robots.txt is not asked for.
    let local = LoadOptions {
        allow_private_network: true,
        robots: false,
        ..LoadOptions::default()
    };
    let live = load(&url, &rec, local).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        live.dump(Dump::Text).contains("First bold paragraph."),
        "gzip was decompressed"
    );
    let replayed = load(&url, &Replay::new(rec.log()), local).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(replayed.dump(Dump::Html), live.dump(Dump::Html));
    assert_eq!(replayed.summary(), live.summary());
}
