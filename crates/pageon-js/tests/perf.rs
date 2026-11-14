//! Engine startup (release): isolate, every interface template and a realm in < 20 ms.
//! Run with `cargo test --release -p pageon-js --test perf -- --ignored --nocapture`.
// A timing harness measures wall time on purpose.
#![allow(clippy::disallowed_methods)]

use std::cell::RefCell;
use std::rc::Rc;

use pageon_js::{Engine, Limits, RealmConfig};

#[test]
#[ignore = "timing; run in release"]
fn startup_is_fast() {
    let make = || {
        let mut e = Engine::new(0, 1, Limits::default());
        let dom = pageon_dom::parse_document("<p>x</p>", pageon_dom::ParseOptions::default());
        e.add_realm(
            Rc::new(RefCell::new(dom)),
            RealmConfig {
                url: "https://a.test/".into(),
                origin: "https://a.test".into(),
                navigator: pageon_env::Profile::Agent.navigator(),
                viewport: pageon_layout::Viewport::default(),
                stylesheets: Vec::new(),
                client: pageon_net::Client::top(
                    &url::Url::parse("https://a.test/").unwrap_or_else(|e| panic!("{e}")),
                ),
                net: Rc::default(),
                accept_dialogs: false,
                crypto_key: [7; 32],
            },
        );
        e
    };
    drop(make());
    let t = std::time::Instant::now();
    for _ in 0..10 {
        drop(make());
    }
    let ms = t.elapsed().as_secs_f64() * 100.0;
    println!("engine startup {ms:.1} ms");
    assert!(ms < 20.0);
}
