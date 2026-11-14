//! Shadow DOM and custom elements: WPT-derived checks of the platform, a Shoelace-like form
//! an agent signs in with (caged and in-process), and the real Lit 3.

mod common;

use common::*;
use pageon::{Action, Budget, LoadOptions, Mode, Replay, Sandbox, SandboxOptions, Session};

const CHECKS: &str = include_str!("fixtures/components/checks.html");
const FORM: &str = include_str!("fixtures/components/form.html");
const LIT: &str = include_str!("fixtures/components/lit.html");

fn js(url: &str, body: &str) -> pageon_env::Exchange {
    page(url, 200, &[("content-type", "text/javascript")], body)
}

fn site_log() -> pageon_env::Log {
    site(vec![
        html("https://wc.test/checks", CHECKS),
        html("https://wc.test/form", FORM),
        html("https://wc.test/lit", LIT),
        js(
            "https://wc.test/lit/lit.js",
            include_str!("fixtures/lit/lit.js"),
        ),
        js(
            "https://wc.test/lit/lit-element.js",
            include_str!("fixtures/lit/lit-element.js"),
        ),
        js(
            "https://wc.test/lit/lit-html.js",
            include_str!("fixtures/lit/lit-html.js"),
        ),
        js(
            "https://wc.test/lit/is-server.js",
            include_str!("fixtures/lit/is-server.js"),
        ),
        js(
            "https://wc.test/lit/reactive-element.js",
            include_str!("fixtures/lit/reactive-element.js"),
        ),
        js(
            "https://wc.test/lit/css-tag.js",
            include_str!("fixtures/lit/css-tag.js"),
        ),
    ])
}

#[test]
fn the_platform_checks_pass() {
    let env = Replay::new(site_log());
    let doc = pageon::load("https://wc.test/checks", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = text_of(&doc, "out");
    let results: Vec<(String, bool)> = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("{e}: {out:?}\nerrors: {:#?}", doc.errors()));
    let failed: Vec<&String> = results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n)
        .collect();
    assert!(results.len() >= 39, "{results:?}");
    assert!(
        failed.is_empty(),
        "failed: {failed:#?}\nerrors: {:#?}",
        doc.errors()
    );
    assert!(doc.errors().is_empty(), "{:#?}", doc.errors());
}

#[test]
fn components_render_in_views() {
    let env = Replay::new(site_log());
    let doc = pageon::load("https://wc.test/checks", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let text = doc.dump(pageon::Dump::Text);
    for t in [
        "Clicked 5 times",
        "Card title",
        "card body",
        "Hidden from scripts",
    ] {
        assert!(text.contains(t), "{t:?} not in {text}");
    }
    assert!(
        !text.contains("fallback"),
        "the default slot has text: no fallback"
    );
    let ax = doc.ax(pageon::Viewport::default(), Budget::o200k(4000));
    assert!(
        ax.contains("button") && ax.contains("\"Clicked 5 times\""),
        "{ax}"
    );
    assert!(
        ax.contains("\"Hidden from scripts\""),
        "closed roots are visible: {ax}"
    );
}

/// Signs in through the components, caged or in-process; returns the outcomes and the view.
fn sign_in(caged: bool) -> (Vec<String>, String) {
    let env = Replay::new(site_log());
    let steps = |obs: &str| -> Vec<String> {
        vec![
            format!("type {} ada@example.com", find(obs, "textbox", "Email")),
            format!("type {} hunter2", find(obs, "textbox", "Password")),
            format!("click {}", find(obs, "button", "Sign in")),
        ]
    };
    let mut out = Vec::new();
    if caged {
        let mut sb = Sandbox::new(
            &env,
            SandboxOptions {
                program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let mut tab = sb
            .open("https://wc.test/form", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let obs = tab.observe_full().unwrap_or_else(|e| panic!("{e}"));
        for s in steps(&obs) {
            let a: Action = s.parse().unwrap_or_else(|e| panic!("{e}"));
            out.push(
                tab.act(a)
                    .map_or_else(|e| format!("error {e}"), |o| o.to_string()),
            );
        }
        (out, tab.observe_full().unwrap_or_else(|e| panic!("{e}")))
    } else {
        let session = Session::new(&env);
        let mut tab = session
            .open("https://wc.test/form", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        tab.set_view(Budget::o200k(4000), Mode::Full);
        let obs = tab.observe_full();
        for s in steps(&obs) {
            let a: Action = s.parse().unwrap_or_else(|e| panic!("{e}"));
            out.push(
                tab.act(a)
                    .map_or_else(|e| format!("error {e}"), |o| o.to_string()),
            );
        }
        (out, tab.observe_full())
    }
}

#[test]
fn agents_sign_in_through_components() {
    let inproc = sign_in(false);
    let caged = sign_in(true);
    assert_eq!(inproc.0, caged.0, "caged and in-process differ");
    for o in &inproc.0 {
        assert!(o.starts_with("ok "), "{:?}", inproc.0);
    }
    assert!(
        inproc.1.contains("signed in as ada@example.com"),
        "{}",
        inproc.1
    );
}

#[test]
fn shadow_labels_name_their_own_inputs() {
    let env = Replay::new(site_log());
    let doc = pageon::load("https://wc.test/form", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let ax = doc.ax(pageon::Viewport::default(), Budget::o200k(4000));
    // Both inputs have `id=input` in their own shadow trees: each label names its own.
    assert!(ax.contains("textbox e") && ax.contains("\"Email\""), "{ax}");
    assert!(ax.contains("\"Password\""), "{ax}");
    let names: Vec<&str> = ax
        .lines()
        .filter(|l| l.trim_start().starts_with("textbox"))
        .collect();
    assert_eq!(names.len(), 2, "{ax}");
    assert!(
        names[0].contains("Email") && names[1].contains("Password"),
        "{names:?}"
    );
    assert!(ax.contains("button") && ax.contains("\"Sign in\""), "{ax}");
}

#[test]
fn lit_renders_and_updates() {
    let env = Replay::new(site_log());
    let session = Session::new(&env);
    let mut tab = session
        .open("https://wc.test/lit", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    assert!(
        obs.contains("\"count: 0\""),
        "{obs}\nerrors: {:#?}",
        tab.document().errors()
    );
    let b = find(&obs, "button", "count: 0");
    let out = tab
        .act(
            format!("click {b}")
                .parse()
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(out.errors.is_empty(), "{out}");
    let obs = tab.observe_full();
    assert!(obs.contains("\"count: 1\""), "{obs}");
    // Its static styles are scoped to it.
    let doc = tab.document();
    let l = doc.layout(pageon::Viewport::default());
    let dom = doc.dom();
    let host = dom
        .traverse(dom.root())
        .find_map(|e| match e {
            pageon_dom::Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("lit-counter")) => {
                Some(n)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("host"));
    let root = dom.shadow_root(host).unwrap_or_else(|| panic!("root"));
    let button = dom
        .traverse(root)
        .find_map(|e| match e {
            pageon_dom::Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("button")) => {
                Some(n)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("button"));
    assert_eq!(l.style(button, "color").as_deref(), Some("rgb(255, 0, 0)"));
}

#[test]
fn screenshots_paint_and_mark_shadow_content() {
    let env = Replay::new(site_log());
    let session = Session::new(&env);
    let mut tab = session
        .open("https://wc.test/form", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let go = find(&obs, "button", "Sign in");
    let img = tab
        .screenshot(&pageon::ScreenshotOptions {
            marks: true,
            ..pageon::ScreenshotOptions::default()
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let legend = img.legend.unwrap_or_default();
    assert!(
        legend.contains(&format!("button {go} \"Sign in\"")),
        "{legend}"
    );
    assert!(
        legend.contains("textbox") && legend.contains("\"Email\""),
        "{legend}"
    );
}

#[test]
fn runaway_reactions_are_bounded() {
    let page = r#"<div id=out>waiting</div><script>
customElements.define('x-loop', class extends HTMLElement {
  static observedAttributes = ['n'];
  attributeChangedCallback(_, __, v) { this.setAttribute('n', String(Number(v) + 1)); }
});
const el = document.createElement('x-loop');
el.setAttribute('n', '0');
document.getElementById('out').textContent = 'returned ' + (Number(el.getAttribute('n')) > 0);
</script>"#;
    let env = Replay::new(site(vec![html("https://wc.test/loop", page)]));
    let doc = pageon::load("https://wc.test/loop", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    // The call returned (bounded), and the session is usable.
    assert_eq!(text_of(&doc, "out"), "returned true");
}

#[test]
fn shadow_elements_get_the_same_actionability_checks() {
    let page = r#"<body style="margin:0"><x-pay></x-pay>
<div style="position:fixed;inset:0;background:white">Overlay</div>
<script>customElements.define('x-pay', class extends HTMLElement {
  constructor() { super(); this.attachShadow({mode: 'closed'}).innerHTML = '<button onclick="document.title = 1">Pay now</button>'; }
});</script></body>"#;
    let env = Replay::new(site(vec![html("https://wc.test/cover", page)]));
    let session = Session::new(&env);
    let mut tab = session
        .open("https://wc.test/cover", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let pay = find(&obs, "button", "Pay now");
    let e = tab
        .act(
            format!("click {pay}")
                .parse()
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .map(|o| o.to_string())
        .unwrap_err();
    assert!(e.to_string().contains("covered"), "{e}");
}
