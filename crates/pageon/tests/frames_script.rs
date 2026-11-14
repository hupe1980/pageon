//! Scripting across frames (spec 028): `contentWindow`, `contentDocument`, `parent`/`top`,
//! cross-origin windows, `postMessage`, and nodes between documents — in-process, caged and
//! replayed.

mod common;

use common::*;
use pageon::replay::{Caged, InProcess, rerun};
use pageon::{LoadOptions, Recording, Replay, Sandbox, SandboxOptions, Session};

const SHOP: &str = include_str!("fixtures/frames/shop.html");
const FIELD: &str = include_str!("fixtures/frames/field.html");
const CHECKS: &str = include_str!("fixtures/frames/checks.html");

fn checks_site() -> Replay {
    Replay::new(site(vec![
        html("https://a.test/", CHECKS),
        html(
            "https://a.test/same.html",
            include_str!("fixtures/frames/same.html"),
        ),
        html(
            "https://b.test/remote.html",
            include_str!("fixtures/frames/remote.html"),
        ),
        html(
            "https://a.test/inner.html",
            include_str!("fixtures/frames/inner.html"),
        ),
    ]))
}

fn shop() -> pageon_env::Log {
    site(vec![
        html("https://shop.test/", SHOP),
        html("https://pay.test/field.html", FIELD),
    ])
}

fn act<E: pageon_env::Env>(tab: &mut pageon::Tab<'_, E>, a: &str) -> String {
    tab.act(a.parse().unwrap_or_else(|e| panic!("{e}")))
        .map_or_else(|e| format!("error {e}"), |o| o.to_string())
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

fn failed(results: &[(String, bool)]) -> Vec<&String> {
    results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n)
        .collect()
}

#[test]
fn the_cross_frame_checks_pass() {
    let doc = pageon::load("https://a.test/", &checks_site(), LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = text_of(&doc, "out");
    let results: Vec<(String, bool)> = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("{e}: {out:?}\nerrors: {:#?}", doc.errors()));
    assert!(results.len() >= 40, "{}", results.len());
    assert!(
        failed(&results).is_empty(),
        "failed: {:#?}\nerrors: {:#?}",
        failed(&results),
        doc.errors()
    );
    assert!(doc.errors().is_empty(), "{:#?}", doc.errors());
}

#[test]
fn the_cross_frame_checks_pass_caged() {
    let env = checks_site();
    let mut sb = sandbox(&env);
    let mut tab = sb
        .open("https://a.test/", LoadOptions::default())
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
        results.len() >= 40 && failed(&results).is_empty(),
        "{:#?}",
        failed(&results)
    );
}

/// The agent pays in a cross-origin card field; the host hears about it by message.
fn pay<E: pageon_env::Env>(tab: &mut pageon::Tab<'_, E>) -> (String, String) {
    let obs = tab.observe();
    assert!(obs.contains("Amount 42"), "the field got the amount: {obs}");
    let typed = act(
        tab,
        &format!(
            "type {} \"4242424242424242\"",
            find(&obs, "textbox", "Card number")
        ),
    );
    let clicked = act(tab, &format!("click {}", find(&obs, "button", "Pay")));
    (typed, clicked)
}

#[test]
fn an_embedded_payment_field_talks_to_its_host() {
    let env = Replay::new(shop());
    let session = Session::new(&env);
    let mut tab = session
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(tab.opened().errors.is_empty(), "{:#?}", tab.opened().errors);
    let (typed, clicked) = pay(&mut tab);
    assert!(!typed.starts_with("error"), "{typed}");
    assert!(!clicked.contains("error"), "{clicked}");
    let obs = tab.observe();
    assert!(obs.contains("Paid with tok_4242"), "{obs}");
}

#[test]
fn an_embedded_payment_field_works_caged() {
    let env = Replay::new(shop());
    let mut sb = sandbox(&env);
    let mut tab = sb
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    assert!(obs.contains("Amount 42"), "{obs}");
    let field = find(&obs, "textbox", "Card number");
    let button = find(&obs, "button", "Pay");
    for a in [
        format!("type {field} \"4242424242424242\""),
        format!("click {button}"),
    ] {
        tab.act(a.parse().unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    assert!(obs.contains("Paid with tok_4242"), "{obs}");
}

#[test]
fn a_payment_session_replays_caged_and_in_process() {
    let rec = Recording::new(Replay::new(shop()));
    {
        let session = Session::new(&rec);
        let mut tab = session
            .open("https://shop.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = pay(&mut tab);
        let _ = tab.observe();
    }
    let log = rec.log();
    let last = log.steps.len() - 1;
    for caged in [false, true] {
        let again = Recording::resume(log.clone(), last, Replay::new(log.clone()), true)
            .unwrap_or_else(|e| panic!("{e}"));
        let r = if caged {
            let mut sb = sandbox(&again);
            rerun(&mut Caged::new(&mut sb), &log.steps, last, &mut |_, _| {})
        } else {
            let session = Session::new(&again);
            rerun(
                &mut InProcess::new(&session),
                &log.steps,
                last,
                &mut |_, _| {},
            )
        };
        r.unwrap_or_else(|d| panic!("caged={caged}: {}", pageon::replay::report(&d)));
    }
}

const EDITOR: &str = r#"<!doctype html><title>Editor</title>
<p id=saved>Not saved</p>
<iframe id=area src="/area.html" title="Editor"></iframe>
<script>
window.saveDraft = (text) => { saved.textContent = 'Saved: ' + text; };
area.addEventListener('load', () => {
  area.contentDocument.body.innerHTML = '<p id=draft>Draft one</p><button id=save>Save</button><button id=boom>Break</button>';
  area.contentDocument.getElementById('save').addEventListener('click', () => {
    parent.saveDraft(area.contentDocument.getElementById('draft').textContent);
  });
  area.contentWindow.document.getElementById('boom').addEventListener('click', () => { area.contentWindow.explode(); });
});
</script>"#;

const AREA: &str = r#"<!doctype html><title>Area</title><body></body>
<script>window.explode = () => { throw new Error('the frame broke'); };</script>"#;

#[test]
fn a_same_origin_editor_scripts_its_frame() {
    let env = Replay::new(site(vec![
        html("https://edit.test/", EDITOR),
        html("https://edit.test/area.html", AREA),
    ]));
    let session = Session::new(&env);
    let mut tab = session
        .open("https://edit.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(tab.opened().errors.is_empty(), "{:#?}", tab.opened().errors);
    let obs = tab.observe();
    assert!(obs.contains("Draft one"), "the host wrote the frame: {obs}");
    let out = act(&mut tab, &format!("click {}", find(&obs, "button", "Save")));
    assert!(!out.contains("error"), "{out}");
    let obs = tab.observe();
    assert!(obs.contains("Saved: Draft one"), "{obs}");
    // An error in the frame's script is reported by the action and attributed to the frame.
    let out = act(
        &mut tab,
        &format!("click {}", find(&obs, "button", "Break")),
    );
    assert!(out.contains("the frame broke"), "{out}");
    let doc = tab.document();
    let frame = doc.frames()[0]
        .document()
        .unwrap_or_else(|| panic!("frame"));
    assert!(
        frame
            .errors()
            .iter()
            .any(|e| e.message.contains("the frame broke")),
        "{:#?}",
        frame.errors()
    );
    assert!(doc.errors().is_empty(), "{:#?}", doc.errors());
}

#[test]
fn a_message_flood_is_bounded() {
    let page = r#"<!doctype html><p id=out></p><script>
let n = 0;
addEventListener('message', () => { n++; });
for (let i = 0; i < 5000; i++) postMessage(i, '*');
setTimeout(() => { out.textContent = String(n); }, 100);
</script>"#;
    let env = Replay::new(site(vec![html("https://flood.test/", page)]));
    let doc = pageon::load("https://flood.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(text_of(&doc, "out"), "1024");
    let drops: Vec<_> = doc
        .errors()
        .iter()
        .filter(|e| e.message.contains("further messages are dropped"))
        .collect();
    assert_eq!(drops.len(), 1, "{:#?}", doc.errors());
}

#[test]
#[allow(clippy::disallowed_methods)] // wall time: the action must return
fn frames_answering_each_other_forever_do_not_hold_an_action() {
    let host = r#"<!doctype html><button id=go>Go</button><p id=n>0</p>
<iframe id=f src="https://echo.test/"></iframe><script>
let n = 0;
addEventListener('message', (e) => { n++; e.source.postMessage(n, '*'); });
go.addEventListener('click', () => { f.contentWindow.postMessage(0, '*'); });
setInterval(() => { document.getElementById('n').textContent = String(n); }, 50);
</script>"#;
    let echo = r#"<!doctype html><script>
addEventListener('message', (e) => { e.source.postMessage(e.data + 1, '*'); });
</script>"#;
    let env = Replay::new(site(vec![
        html("https://loop.test/", host),
        html("https://echo.test/", echo),
    ]));
    let session = Session::new(&env);
    let mut tab = session
        .open("https://loop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    let started = std::time::Instant::now();
    let out = act(&mut tab, &format!("click {}", find(&obs, "button", "Go")));
    assert!(!out.starts_with("error"), "{out}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    // Some rounds ran, then the action returned; the next action carries on.
    let first = text_of(tab.document(), "n");
    assert!(first.parse::<u32>().unwrap_or(0) >= 1, "{first}");
}
