//! `<iframe sandbox>`: opaque origins without `allow-same-origin`, no script without
//! `allow-scripts`, and forms, popups and dialogs blocked unless allowed — in-process and caged.

mod common;

use common::*;
use pageon::{DialogPolicy, LoadOptions, Sandbox, SandboxOptions, Session};

const CHECKS: &str = include_str!("fixtures/sandbox/checks.html");
const INNER: &str = include_str!("fixtures/sandbox/inner.html");

fn checks_site() -> pageon_env::Log {
    site(vec![
        page(
            "https://a.test/",
            200,
            &[
                ("content-type", "text/html"),
                ("set-cookie", "sid=1; Path=/; SameSite=Lax"),
            ],
            CHECKS,
        ),
        html("https://a.test/inner.html", INNER),
        ex(
            "POST",
            "https://a.test/api",
            200,
            &[("content-type", "text/plain")],
            "ok",
        ),
    ])
}

fn options() -> LoadOptions {
    LoadOptions {
        dialogs: DialogPolicy::Accept,
        ..LoadOptions::default()
    }
}

fn failed(results: &[(String, bool)]) -> Vec<&String> {
    results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n)
        .collect()
}

#[test]
fn the_sandbox_checks_pass() {
    let env = Capture::new(checks_site());
    let doc = pageon::load("https://a.test/", &env, options()).unwrap_or_else(|e| panic!("{e}"));
    let out = text_of(&doc, "out");
    let results: Vec<(String, bool)> =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out:?}\n{:#?}", doc.errors()));
    assert!(results.len() >= 13, "{}", results.len());
    assert!(
        failed(&results).is_empty(),
        "failed: {:#?}",
        failed(&results)
    );
    // A frame nested in a sandboxed frame inherits its flags.
    assert_eq!(text_of(&doc, "nested"), "nested null");
    // With `allow-modals`, the dialog policy answers.
    assert_eq!(text_of(&doc, "modals"), "modals true");
    let opaque = doc
        .frames()
        .iter()
        .find(|f| f.url().ends_with("/inner.html"))
        .unwrap_or_else(|| panic!("no frame"));
    assert_eq!(opaque.origin(), "null");
    // Requests from an opaque origin say so, and carry no SameSite cookie.
    assert_eq!(
        env.header("POST", "https://a.test/api", "origin")
            .as_deref(),
        Some("null")
    );
    assert_eq!(env.header("POST", "https://a.test/api", "cookie"), None);
}

#[test]
fn the_sandbox_checks_pass_caged() {
    let env = pageon::Replay::new(checks_site());
    let mut sb = Sandbox::new(
        &env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut tab = sb
        .open("https://a.test/", options())
        .unwrap_or_else(|e| panic!("{e}"));
    let text = tab
        .view(
            pageon::Dump::Text,
            pageon::Viewport::default(),
            pageon::Budget::o200k(100_000),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let json = text
        .find("[[")
        .map(|i| text[i..].lines().next().unwrap_or_default())
        .unwrap_or_else(|| panic!("{text}"));
    let results: Vec<(String, bool)> = serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        results.len() >= 13 && failed(&results).is_empty(),
        "{:#?}",
        failed(&results)
    );
    assert!(
        text.contains("nested null") && text.contains("modals true"),
        "{text}"
    );
}

/// A sandboxed frame's form and new-window link, clicked by the agent.
fn actions_site(flags: &str) -> pageon_env::Log {
    let frame = "<form action=https://a.test/sent><button>Send</button></form>\
        <a href=https://a.test/new target=_blank>New window</a>";
    site(vec![
        html(
            "https://a.test/",
            &format!("<iframe sandbox=\"{flags}\" src=/frame.html></iframe>"),
        ),
        html("https://a.test/frame.html", frame),
        html("https://a.test/new", "<p>new</p>"),
    ])
}

fn click<E: pageon_env::Env>(tab: &mut pageon::Tab<'_, E>, role: &str, name: &str) -> String {
    let obs = tab.observe_full();
    let r = find(&obs, role, name);
    tab.act(
        format!("click {r}")
            .parse()
            .unwrap_or_else(|e| panic!("{e}")),
    )
    .map_or_else(|e| format!("error {e}"), |o| o.to_string())
}

#[test]
fn forms_and_popups_need_their_flags() {
    for allowed in [false, true] {
        let flags = if allowed {
            "allow-forms allow-popups"
        } else {
            ""
        };
        let env = Capture::new(actions_site(flags));
        let s = Session::new(&env);
        let mut tab = s
            .open("https://a.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let link = click(&mut tab, "link", "New window");
        assert_eq!(link.contains("https://a.test/new"), allowed, "{link}");
        // Allowed, the submission asks the frame to navigate (which tabs do not do yet);
        // blocked, nothing happens.
        let send = click(&mut tab, "button", "Send");
        assert_eq!(send.contains("navigating a frame"), allowed, "{send}");
        assert!(!env.urls().iter().any(|u| u.contains("/sent")));
    }
}

#[test]
fn handlers_and_javascript_urls_need_allow_scripts() {
    for scripts in [false, true] {
        let flags = if scripts {
            "allow-same-origin allow-scripts"
        } else {
            "allow-same-origin"
        };
        let frame = "<button onclick=\"parent.document.getElementById('out').textContent += 'h'\">Handler</button>\
            <a href=\"javascript:parent.document.getElementById('out').textContent += 'j'\">Url</a>";
        let env = pageon::Replay::new(site(vec![
            html(
                "https://a.test/",
                &format!("<p id=out></p><iframe sandbox=\"{flags}\" src=/frame.html></iframe>"),
            ),
            html("https://a.test/frame.html", frame),
        ]));
        let s = Session::new(&env);
        let mut tab = s
            .open("https://a.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        click(&mut tab, "button", "Handler");
        click(&mut tab, "link", "Url");
        let out = text_of(tab.document(), "out");
        assert_eq!(out, if scripts { "hj" } else { "" }, "scripts={scripts}");
    }
}
