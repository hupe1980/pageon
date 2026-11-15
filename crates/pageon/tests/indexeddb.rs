//! IndexedDB: the standard's behaviour, libraries built on it, and databases kept per origin
//! for the session across reloads and tabs, caged and in-process.

mod common;

use common::*;
use pageon::{Budget, Dump, LoadOptions, Replay, Sandbox, SandboxOptions, Session, Viewport};

const DIR: &str = "fixtures/indexeddb";

fn js(url: &str, body: &str) -> pageon_env::Exchange {
    page(url, 200, &[("content-type", "text/javascript")], body)
}

fn libs_site() -> Vec<pageon_env::Exchange> {
    let page_html = include_str!("fixtures/indexeddb/libs.html");
    let mut v = Vec::new();
    for _ in 0..3 {
        v.push(html("https://app.test/?write", page_html));
        v.push(html("https://app.test/", page_html));
        v.push(js(
            "https://app.test/idb-keyval.umd.js",
            include_str!("fixtures/indexeddb/idb-keyval.umd.js"),
        ));
        v.push(js(
            "https://app.test/localforage.min.js",
            include_str!("fixtures/indexeddb/localforage.min.js"),
        ));
    }
    v
}

#[test]
fn the_indexeddb_checks_pass() {
    let _ = DIR;
    let env = Replay::new(site(vec![html(
        "https://checks.test/",
        include_str!("fixtures/indexeddb/checks.html"),
    )]));
    let doc = pageon::load("https://checks.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = text_of(&doc, "out");
    let results: Vec<(String, bool)> =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}\n{:#?}", doc.errors()));
    let failed: Vec<&String> = results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n)
        .collect();
    assert!(results.len() >= 20, "{} checks", results.len());
    assert!(failed.is_empty(), "failed: {failed:#?}");
}

#[test]
fn libraries_keep_data_across_reloads_and_tabs() {
    let env = Replay::new(site(libs_site()));
    let s = Session::new(&env);
    let tab = s
        .open("https://app.test/?write", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(tab.document(), "out"), "written asyncStorage");
    // A reload of the page, and the same origin in another tab.
    let mut tab = s
        .open("https://app.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        text_of(tab.document(), "out"),
        "read true true asyncStorage"
    );
    tab.act(
        "goto https://app.test/"
            .parse()
            .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        text_of(tab.document(), "out"),
        "read true true asyncStorage"
    );
}

#[test]
fn a_signed_in_app_stays_signed_in() {
    for caged in [false, true] {
        let auth = include_str!("fixtures/indexeddb/auth.html");
        let env = Replay::new(site(vec![
            html("https://app.test/", auth),
            html("https://app.test/", auth),
            html("https://other.test/", auth),
        ]));
        let text = if caged {
            let mut sb = Sandbox::new(
                &env,
                SandboxOptions {
                    program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                    ..SandboxOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
            {
                let mut tab = sb
                    .open("https://app.test/", LoadOptions::default())
                    .unwrap_or_else(|e| panic!("{e}"));
                let obs = tab.observe_full().unwrap_or_else(|e| panic!("{e}"));
                let b = find(&obs, "button", "Sign in");
                tab.act(
                    format!("click {b}")
                        .parse()
                        .unwrap_or_else(|e| panic!("{e}")),
                )
                .unwrap_or_else(|e| panic!("{e}"));
            }
            let mut view = |url: &str| {
                let mut t = sb
                    .open(url, LoadOptions::default())
                    .unwrap_or_else(|e| panic!("{e}"));
                t.view(Dump::Text, Viewport::default(), Budget::o200k(2000))
                    .unwrap_or_else(|e| panic!("{e}"))
            };
            let again = view("https://app.test/");
            let other = view("https://other.test/");
            format!("{again} | {other}")
        } else {
            let s = Session::new(&env);
            let mut tab = s
                .open("https://app.test/", LoadOptions::default())
                .unwrap_or_else(|e| panic!("{e}"));
            assert!(text_of(tab.document(), "who").contains("signed out"));
            let obs = tab.observe_full();
            let b = find(&obs, "button", "Sign in");
            tab.act(
                format!("click {b}")
                    .parse()
                    .unwrap_or_else(|e| panic!("{e}")),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            let again = s
                .open("https://app.test/", LoadOptions::default())
                .unwrap_or_else(|e| panic!("{e}"));
            let other = s
                .open("https://other.test/", LoadOptions::default())
                .unwrap_or_else(|e| panic!("{e}"));
            format!(
                "{} | {}",
                text_of(again.document(), "who"),
                text_of(other.document(), "who")
            )
        };
        // Kept for the origin; another origin sees nothing of it.
        assert!(
            text.contains("signed in as agent@app.test"),
            "caged={caged}: {text}"
        );
        let (_, other) = text.split_once(" | ").unwrap_or_default();
        assert!(other.contains("signed out"), "caged={caged}: {text}");
    }
}

#[test]
fn quotas_hold() {
    let quota = "<p id=out>pending</p><script>\
        const r = indexedDB.open('big', 1);\
        r.onupgradeneeded = () => r.result.createObjectStore('s');\
        r.onsuccess = () => { const t = r.result.transaction('s', 'readwrite');\
          t.objectStore('s').put(new Uint8Array(60 << 20), 'k'); t.oncomplete = () => setTimeout(() => out.textContent = 'committed', 10); };\
        </script>";
    let env = Replay::new(site(vec![html("https://big.test/", quota)]));
    let s = Session::new(&env);
    let tab = s
        .open("https://big.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(tab.document(), "out"), "committed");
    let errors: Vec<String> = tab
        .document()
        .errors()
        .iter()
        .map(|e| e.message.clone())
        .collect();
    assert!(
        errors
            .iter()
            .any(|e| e.contains("QuotaExceededError") || e.contains("50 MiB")),
        "{errors:#?}"
    );
}

#[test]
fn pages_without_indexeddb_do_not_load_it() {
    let page_html = "<p id=out></p><script>\
        const lazy = () => typeof Object.getOwnPropertyDescriptor(globalThis, 'indexedDB').get;\
        const before = lazy(); typeof indexedDB; out.textContent = [before, lazy()].join();</script>";
    let env = Replay::new(site(vec![html("https://lazy.test/", page_html)]));
    let doc = pageon::load("https://lazy.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(&doc, "out"), "function,undefined");
}

#[test]
#[ignore = "timing: run in release (cargo test --release -- --ignored)"]
fn the_first_use_is_cheap() {
    let with = "<p id=out></p><script>indexedDB.open('x'); out.textContent = 'ok'</script>";
    let without = "<p id=out></p><script>out.textContent = 'ok'</script>";
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
    eprintln!("without {a:?}; with {b:?}; first use {extra:?}");
    assert!(extra.as_millis() < 20, "{extra:?}");
}
