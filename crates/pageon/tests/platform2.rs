//! Platform basics 2: Web Crypto, streams, DOMParser and the globals sites call at startup;
//! fixture apps written as OAuth, DPoP, streaming-chat and sanitizing libraries do it.

mod common;

use base64::Engine as _;
use common::*;
use pageon::{Budget, Dump, LoadOptions, Replay, Sandbox, SandboxOptions, Session, Viewport};

const CHECKS: &str = include_str!("fixtures/platform2/checks.html");

fn js(url: &str, body: &str) -> pageon_env::Exchange {
    page(url, 200, &[("content-type", "text/javascript")], body)
}

fn sandbox<E: pageon_env::Env>(env: E) -> Sandbox<E> {
    Sandbox::new(
        env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

fn results_of(text: &str) -> Vec<(String, bool)> {
    let json = text
        .find("[[")
        .map(|i| text[i..].lines().next().unwrap_or_default())
        .unwrap_or_else(|| panic!("no results in {text}"));
    serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {json}"))
}

fn assert_all(results: &[(String, bool)]) {
    let failed: Vec<&String> = results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n)
        .collect();
    assert!(results.len() >= 59, "{} checks: {failed:?}", results.len());
    assert!(failed.is_empty(), "failed: {failed:#?}");
}

#[test]
fn the_platform_checks_pass() {
    let env = Replay::new(site(vec![html("https://checks.test/", CHECKS)]));
    let doc = pageon::load("https://checks.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = text_of(&doc, "out");
    assert!(
        out.starts_with("[["),
        "{out:?}\nerrors: {:#?}",
        doc.errors()
    );
    assert_all(&results_of(&out));
    // reportError is reported as an uncaught exception is; nothing else went wrong.
    let errors: Vec<String> = doc.errors().iter().map(|e| e.message.clone()).collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].contains("reported"), "{errors:#?}");
    // The parsed document loaded nothing.
    assert!(
        !doc.requests().iter().any(|r| r.url.contains("never.png")),
        "a DOMParser document fetched its image"
    );
}

#[test]
fn the_platform_checks_pass_caged() {
    let env = Replay::new(site(vec![html("https://checks.test/", CHECKS)]));
    let mut sb = sandbox(&env);
    let mut tab = sb
        .open("https://checks.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let text = tab
        .view(Dump::Text, Viewport::default(), Budget::o200k(100_000))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_all(&results_of(&text));
}

#[test]
fn subtle_is_for_secure_contexts_only() {
    let page_html = "<p id=out></p><script>out.textContent = typeof crypto.subtle + ' ' + typeof crypto.getRandomValues</script>";
    let env = Replay::new(site(vec![html("http://plain.test/", page_html)]));
    let doc = pageon::load("http://plain.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(&doc, "out"), "undefined function");
}

#[test]
fn an_oauth_login_starts_with_pkce() {
    let env = Replay::new(site(vec![html(
        "https://app.test/",
        include_str!("fixtures/platform2/pkce.html"),
    )]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://app.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let go = find(&obs, "button", "Sign in with Example");
    // The authorization server is not in the recording: the navigation it starts names the
    // URL the page built.
    let e = tab
        .act(
            format!("click {go}")
                .parse()
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    let url = e
        .split_whitespace()
        .find(|w| w.starts_with("https://auth.test/authorize?"))
        .unwrap_or_else(|| panic!("no authorization request: {e}"))
        .to_string();
    let q: std::collections::HashMap<String, String> = url::Url::parse(&url)
        .unwrap_or_else(|e| panic!("{e}"))
        .query_pairs()
        .into_owned()
        .collect();
    let verifier = &q["state"];
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.as_ref());
    assert_eq!(q["code_challenge"], expected, "{url}");
    assert_eq!(q["code_challenge_method"], "S256");
}

#[test]
fn a_streaming_chat_answer_is_read() {
    for caged in [false, true] {
        let env = Replay::new(site(vec![
            html(
                "https://chat.test/",
                include_str!("fixtures/platform2/chat.html"),
            ),
            ex(
                "POST",
                "https://chat.test/api/chat",
                200,
                &[("content-type", "text/event-stream")],
                "data: Hel\n\ndata: lo, \n\ndata: agent\n\n",
            ),
        ]));
        let text = if caged {
            let mut sb = sandbox(&env);
            let mut tab = sb
                .open("https://chat.test/", LoadOptions::default())
                .unwrap_or_else(|e| panic!("{e}"));
            let obs = tab.observe_full().unwrap_or_else(|e| panic!("{e}"));
            let ask = find(&obs, "button", "Ask");
            tab.act(
                format!("click {ask}")
                    .parse()
                    .unwrap_or_else(|e| panic!("{e}")),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            tab.view(Dump::Text, Viewport::default(), Budget::o200k(4000))
                .unwrap_or_else(|e| panic!("{e}"))
        } else {
            let s = Session::new(&env);
            let mut tab = s
                .open("https://chat.test/", LoadOptions::default())
                .unwrap_or_else(|e| panic!("{e}"));
            let obs = tab.observe_full();
            let ask = find(&obs, "button", "Ask");
            tab.act(
                format!("click {ask}")
                    .parse()
                    .unwrap_or_else(|e| panic!("{e}")),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            tab.view(Dump::Text, Viewport::default(), Budget::o200k(4000))
        };
        assert!(text.contains("Hello, agent"), "caged={caged}: {text}");
    }
}

#[test]
fn dpop_proofs_sign_and_replay_identically() {
    let log = site(vec![
        html(
            "https://app.test/",
            include_str!("fixtures/platform2/dpop.html"),
        ),
        // A cross-origin POST with a DPoP header: preflighted, as in browsers.
        ex(
            "OPTIONS",
            "https://api.test/token",
            204,
            &[
                ("access-control-allow-origin", "https://app.test"),
                ("access-control-allow-methods", "POST"),
                ("access-control-allow-headers", "dpop"),
            ],
            "",
        ),
        ex(
            "POST",
            "https://api.test/token",
            200,
            &[
                ("content-type", "application/json"),
                ("access-control-allow-origin", "https://app.test"),
            ],
            "{}",
        ),
    ]);
    let run = || {
        let env = Replay::new(log.clone());
        let doc = pageon::load("https://app.test/", &env, LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let text = text_of(&doc, "out");
        let out: serde_json::Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("{e}: {text:?}\nerrors: {:#?}", doc.errors()));
        assert_eq!(out["ok"], true, "{out}");
        out["proof"].as_str().unwrap_or_default().to_string()
    };
    let (a, b) = (run(), run());
    assert_eq!(a.split('.').count(), 3, "{a}");
    // Keys from the crypto stream, signatures by RFC 6979: the same session, the same proof.
    assert_eq!(a, b);
}

#[test]
fn dompurify_sanitizes_through_domparser() {
    let env = Replay::new(site(vec![
        html(
            "https://blog.test/",
            include_str!("fixtures/platform2/purify.html"),
        ),
        js(
            "https://blog.test/purify.min.js",
            include_str!("fixtures/platform2/purify.min.js"),
        ),
    ]));
    let doc = pageon::load("https://blog.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let title = doc.dump(Dump::Text);
    assert!(
        title.contains("Comments sanitized")
            || doc
                .dump(Dump::Html)
                .contains("<title>Comments sanitized</title>"),
        "{:#?}",
        doc.errors()
    );
    let html = doc.dump(Dump::Html);
    let start = html.find("id=\"comment\"").unwrap_or(0);
    let comment = &html[start..start + html[start..].find("</div>").unwrap_or(0)];
    assert!(comment.contains("<b>bold</b>"), "{comment}");
    for bad in ["onerror", "<script", "javascript:"] {
        assert!(!comment.contains(bad), "{bad} survived: {comment}");
    }
}

#[test]
fn typing_dispatches_input_events() {
    let page_html = "<input id=f aria-label=Field><p id=log></p><script>\
        f.addEventListener('beforeinput', e => log.textContent += 'b:' + e.inputType + ':' + e.data + ' ');\
        f.addEventListener('input', e => log.textContent += (e instanceof InputEvent ? 'i:' : 'e:') + e.inputType + ' ');\
        </script>";
    let env = Replay::new(site(vec![html("https://in.test/", page_html)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://in.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let f = find(&obs, "textbox", "Field");
    tab.act(
        format!("type {f} ab")
            .parse()
            .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    tab.act("press BackSpace".parse().unwrap_or_else(|e| panic!("{e}")))
        .unwrap_or_else(|e| panic!("{e}"));
    let log = text_of(tab.document(), "log");
    assert_eq!(
        log.trim(),
        "b:insertText:a i:insertText b:insertText:b i:insertText i:deleteContentBackward"
    );
}

#[test]
fn streams_load_only_when_used() {
    // A page that never touches a stream global leaves them unloaded (still lazy getters).
    let page_html = "<p id=out></p><script>\
        const lazy = () => typeof Object.getOwnPropertyDescriptor(globalThis, 'ReadableStream').get;\
        const before = lazy(); new Response('x').text(); const still = lazy();\
        typeof ReadableStream; out.textContent = [before, still, lazy()].join();</script>";
    let env = Replay::new(site(vec![html("https://lazy.test/", page_html)]));
    let doc = pageon::load("https://lazy.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(&doc, "out"), "function,function,undefined");
}
