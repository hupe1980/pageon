//! The egress policy: origin sets, effect classes and the approver, in-process
//! and across the cage.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::*;
use pageon::{
    Budget, Dump, LoadOptions, Policy, RequestOutcome, Sandbox, SandboxOptions, Session, Viewport,
};

/// A page that fetches `url` with `method` and shows the outcome.
/// (`no-cors` for simple methods; CORS with a preflight for the others.)
fn page_for(method: &str, url: &str) -> String {
    let mode = if matches!(method, "GET" | "POST") {
        "no-cors"
    } else {
        "cors"
    };
    format!(
        "<div id=out></div><script>fetch('{url}', {{method: '{method}', mode: '{mode}'}})\
         .then(() => out.textContent = 'sent', () => out.textContent = 'refused');</script>"
    )
}

fn exchanges(method: &str, url: &str) -> Vec<pageon_env::Exchange> {
    let cors = [
        ("content-type", "text/plain"),
        ("access-control-allow-origin", "*"),
        ("access-control-allow-methods", "PUT, DELETE"),
    ];
    vec![
        html("https://shop.test/", &page_for(method, url)),
        ex("OPTIONS", url, 204, &cors, ""),
        ex(method, url, 200, &cors, "ok"),
    ]
}

/// The policy used by every case: shop read-write, cdn read-only; the approver allows only
/// PUT.
fn policy<E: pageon_env::Env>(env: E, asked: Rc<RefCell<Vec<String>>>) -> Policy<E> {
    Policy::new(env)
        .read_write(["https://shop.test", "https://*.shop.test"])
        .read_only(["https://cdn.test"])
        .approver(move |e| {
            asked.borrow_mut().push(e.to_string());
            e.method == "PUT"
        })
}

const CASES: &[(&str, &str, &str)] = &[
    ("GET", "https://shop.test/a", "sent"),
    ("POST", "https://shop.test/a", "refused"),
    ("PUT", "https://shop.test/a", "sent"),
    ("DELETE", "https://shop.test/a", "refused"),
    ("GET", "https://api.shop.test/a", "sent"),
    ("PUT", "https://api.shop.test/a", "sent"),
    ("GET", "https://cdn.test/lib.js", "sent"),
    ("POST", "https://cdn.test/beacon", "refused"),
    ("PUT", "https://cdn.test/x", "refused"),
    ("GET", "https://evil.test/?q=secret", "refused"),
    ("POST", "https://evil.test/", "refused"),
    ("GET", "http://shop.test/a", "refused"),
    ("GET", "https://shop.test:8443/a", "refused"),
];

#[test]
fn policy_matrix_in_process_and_caged() {
    assert!(CASES.len() >= 12, "at least 12 cases");
    for caged in [false, true] {
        for (method, url, want) in CASES {
            let asked = Rc::new(RefCell::new(Vec::new()));
            let env = policy(Capture::new(site(exchanges(method, url))), asked.clone());
            let (out, log) = if caged {
                let mut sb = Sandbox::new(
                    &env,
                    SandboxOptions {
                        program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                    },
                )
                .unwrap_or_else(|e| panic!("{e}"));
                let mut tab = sb
                    .open("https://shop.test/", LoadOptions::default())
                    .unwrap_or_else(|e| panic!("{e}"));
                let text = tab
                    .view(Dump::Text, Viewport::default(), Budget::o200k(200))
                    .unwrap_or_else(|e| panic!("{e}"));
                let log = tab
                    .view(Dump::Requests, Viewport::default(), Budget::o200k(2000))
                    .unwrap_or_else(|e| panic!("{e}"));
                (text, log)
            } else {
                let session = Session::new(&env);
                let tab = session
                    .open("https://shop.test/", LoadOptions::default())
                    .unwrap_or_else(|e| panic!("{e}"));
                (
                    tab.document().dump(Dump::Text),
                    tab.document().dump(Dump::Requests),
                )
            };
            assert_eq!(out.trim(), *want, "{method} {url} (caged: {caged})\n{log}");
            let reached = env.inner().urls().contains(&format!("{method} {url}"));
            assert_eq!(
                reached,
                *want == "sent",
                "{method} {url}: reached the network?"
            );
            if *want == "refused" {
                assert!(log.contains(" blocked "), "{log}");
                if url.starts_with("https://") {
                    assert!(log.contains("(policy: "), "{log}");
                }
            }
            // The approver is asked about writes to read-write origins only.
            let write_rw =
                *method != "GET" && url.contains("shop.test/") && url.starts_with("https://");
            assert_eq!(
                !asked.borrow().is_empty(),
                write_rw && !url.contains(":8443"),
                "{method} {url}: {:?}",
                asked.borrow()
            );
        }
    }
}

#[test]
fn a_blocked_top_level_load_says_why() {
    let env = Policy::new(Capture::new(site(vec![]))).read_write(["https://shop.test"]);
    let session = Session::new(&env);
    let e = session
        .load("https://evil.test/", LoadOptions::default())
        .err()
        .unwrap_or_else(|| panic!("must be blocked"));
    assert_eq!(
        e.to_string(),
        "blocked https://evil.test/: policy: https://evil.test is outside the task's origins"
    );
    assert!(e.guidance().contains("--origin"), "{}", e.guidance());
    let _ = RequestOutcome::Ok;
}
