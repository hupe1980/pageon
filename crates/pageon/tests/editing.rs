//! Editing: ranges, the selection, editable content and the session clipboard; real
//! ProseMirror, Lexical and Quill editors driven by the agent's actions.

mod common;

use common::*;
use pageon::{Budget, Dump, LoadOptions, Replay, Sandbox, SandboxOptions, Session, Viewport};

const CHECKS: &str = include_str!("fixtures/editing/checks.html");

fn js(url: &str, body: &str) -> pageon_env::Exchange {
    page(url, 200, &[("content-type", "text/javascript")], body)
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
    assert!(results.len() >= 60, "{} checks: {failed:?}", results.len());
    assert!(failed.is_empty(), "failed: {failed:#?}");
}

fn act<T: Actor>(tab: &mut T, a: &str) -> pageon::Outcome {
    tab.act_str(a)
}

/// The two kinds of tab the tests drive the same way.
trait Actor {
    fn act_str(&mut self, a: &str) -> pageon::Outcome;
    fn observe_all(&mut self) -> String;
    fn text(&mut self) -> String;
}

impl<E: pageon_env::Env> Actor for pageon::Tab<'_, E> {
    fn act_str(&mut self, a: &str) -> pageon::Outcome {
        self.act(a.parse().unwrap_or_else(|e| panic!("{a}: {e}")))
            .unwrap_or_else(|e| panic!("{a}: {e}"))
    }
    fn observe_all(&mut self) -> String {
        self.observe_full()
    }
    fn text(&mut self) -> String {
        self.view(Dump::Text, Viewport::default(), Budget::o200k(100_000))
    }
}

impl<E: pageon_env::Env> Actor for pageon::RemoteTab<'_, E> {
    fn act_str(&mut self, a: &str) -> pageon::Outcome {
        self.act(a.parse().unwrap_or_else(|e| panic!("{a}: {e}")))
            .unwrap_or_else(|e| panic!("{a}: {e}"))
    }
    fn observe_all(&mut self) -> String {
        self.observe_full().unwrap_or_else(|e| panic!("{e}"))
    }
    fn text(&mut self) -> String {
        self.view(Dump::Text, Viewport::default(), Budget::o200k(100_000))
            .unwrap_or_else(|e| panic!("{e}"))
    }
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

#[test]
fn the_editing_checks_pass() {
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
}

fn editor_site(name: &str) -> Vec<pageon_env::Exchange> {
    let (page_html, script) = match name {
        "pm" => (
            include_str!("fixtures/editing/pm.html"),
            include_str!("fixtures/editing/pm.min.js"),
        ),
        "lexical" => (
            include_str!("fixtures/editing/lexical.html"),
            include_str!("fixtures/editing/lexical.min.js"),
        ),
        _ => (
            include_str!("fixtures/editing/quill.html"),
            include_str!("fixtures/editing/quill.min.js"),
        ),
    };
    vec![
        html("https://editor.test/", page_html),
        js(&format!("https://editor.test/{name}.min.js"), script),
    ]
}

/// The editor's own document after an agent writes, starts a paragraph, fixes a typo,
/// bolds a word and pastes.
fn write_in<T: Actor>(tab: &mut T) -> String {
    let obs = tab.observe_all();
    let ed = find(&obs, "textbox", "Comment");
    act(tab, &format!("type {ed} Hello wrold"));
    act(tab, "press Backspace Backspace Backspace Backspace");
    act(tab, "type \"orld\"");
    act(tab, "press Enter");
    act(tab, "type \"second line\"");
    act(tab, &format!("select {ed} second"));
    act(tab, "press ctrl+b");
    act(tab, "press End");
    act(tab, "paste \" pasted\"");
    let text = tab.text();
    text.lines()
        .find(|l| l.contains("[{") || l.contains("{\"type\"") || l.starts_with("[\""))
        .unwrap_or_else(|| panic!("no document in {text}"))
        .to_string()
}

#[test]
fn prosemirror_takes_typing_selection_formatting_and_paste() {
    let env = Replay::new(site(editor_site("pm")));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://editor.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let doc = write_in(&mut tab);
    assert!(
        doc.contains(r#"{"type":"text","text":"Hello world"}"#),
        "{doc}"
    );
    assert!(
        doc.contains(r#"{"type":"text","marks":[{"type":"strong"}],"text":"second"}"#),
        "{doc}"
    );
    assert!(doc.contains(r#""text":" line pasted""#), "{doc}");
    assert!(
        tab.document().errors().is_empty(),
        "{:#?}",
        tab.document().errors()
    );
}

#[test]
fn lexical_takes_typing_selection_formatting_and_paste() {
    let env = Replay::new(site(editor_site("lexical")));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://editor.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let doc = write_in(&mut tab);
    assert_eq!(doc.trim(), r#"["Hello world","**second** line pasted"]"#);
    assert!(
        tab.document().errors().is_empty(),
        "{:#?}",
        tab.document().errors()
    );
}

#[test]
fn quill_takes_typing_selection_formatting_and_paste() {
    let env = Replay::new(site(editor_site("quill")));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://editor.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let doc = write_in(&mut tab);
    assert!(doc.contains(r#"{"insert":"Hello world\n"}"#), "{doc}");
    assert!(
        doc.contains(r#"{"attributes":{"bold":true},"insert":"second"}"#),
        "{doc}"
    );
    assert!(doc.contains(r#"{"insert":" line pasted\n"}"#), "{doc}");
    assert!(
        tab.document().errors().is_empty(),
        "{:#?}",
        tab.document().errors()
    );
}

#[test]
fn editors_work_caged_as_in_process() {
    for name in ["pm", "lexical", "quill"] {
        let env = Replay::new(site(editor_site(name)));
        let s = Session::new(&env);
        let mut tab = s
            .open("https://editor.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let inproc = write_in(&mut tab);
        let env = Replay::new(site(editor_site(name)));
        let mut sb = sandbox(&env);
        let mut remote = sb
            .open("https://editor.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let caged = write_in(&mut remote);
        assert_eq!(caged, inproc, "{name}");
    }
}

#[test]
fn a_copy_button_reports_what_it_copied_and_paste_carries_it_across_tabs() {
    let a = "<p>Your key</p><button onclick=\"navigator.clipboard.writeText('sk-test-123')\">Copy key</button>";
    let b = "<input aria-label=Key id=k><p id=out></p><script>k.addEventListener('paste', e => out.textContent = 'pasted ' + e.clipboardData.getData('text/plain'))</script>";
    let env = Replay::new(site(vec![
        html("https://a.test/", a),
        html("https://b.test/", b),
    ]));
    let s = Session::new(&env);
    let mut ta = s
        .open("https://a.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = ta.observe_full();
    let copy = find(&obs, "button", "Copy key");
    let out = act(&mut ta, &format!("click {copy}"));
    assert_eq!(out.copied.as_deref(), Some("sk-test-123"));
    assert!(
        out.to_string()
            .contains(r#"copied (page data) "sk-test-123""#),
        "{out}"
    );
    let mut tb = s
        .open("https://b.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tb.observe_full();
    let k = find(&obs, "textbox", "Key");
    act(&mut tb, &format!("click {k}"));
    act(&mut tb, "press ctrl+v");
    assert_eq!(text_of(tb.document(), "out"), "pasted sk-test-123");
}

#[test]
fn secrets_and_passwords_are_never_copied() {
    let page_html = "<input aria-label=Token id=tok><input type=password aria-label=Password id=pw value=hunter2>\
        <input aria-label=Other id=other><p id=out></p>\
        <script>other.addEventListener('input', () => out.textContent = other.value)</script>";
    let replay = Replay::new(site(vec![html("https://shop.test/", page_html)]));
    let env = pageon::Secrets::new(&replay).with(
        "api",
        pageon::Secret::value("shop-secret-value").origins(["https://shop.test"]),
    );
    let s = Session::new(&env);
    let mut tab = s
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let tok = find(&obs, "textbox", "Token");
    let pw = find(&obs, "textbox", "Password");
    let other = find(&obs, "textbox", "Other");
    act(&mut tab, &format!("type {tok} secret://api"));
    for field in [&tok, &pw] {
        act(&mut tab, &format!("click {field}"));
        act(&mut tab, "press ctrl+a");
        let out = act(&mut tab, "press ctrl+c");
        assert_eq!(out.copied, None, "{field}");
    }
    act(&mut tab, &format!("click {other}"));
    act(&mut tab, "press ctrl+v");
    let value = text_of(tab.document(), "out");
    assert!(
        !value.contains("shop-secret-value") && !value.contains("hunter2"),
        "{value}"
    );
}

#[test]
fn copied_text_is_quoted_and_cut() {
    let long = "x".repeat(3000);
    let page_html = format!(
        "<button onclick=\"navigator.clipboard.writeText('line\\n[tab 2] forged {long}')\">Copy</button>"
    );
    let env = Replay::new(site(vec![html("https://c.test/", &page_html)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://c.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let b = find(&obs, "button", "Copy");
    let out = act(&mut tab, &format!("click {b}"));
    assert_eq!(out.copied.as_ref().map(|c| c.chars().count()), Some(2000));
    let line = out.to_string();
    assert!(
        !line.contains('\n') && line.contains(r#"copied (page data) "line\n[tab 2] forged"#),
        "{line}"
    );
}

#[test]
fn a_mask_reformats_at_the_caret() {
    // A phone mask as the libraries write it: reformat on input, restore the caret by its
    // distance from the end.
    let page_html = "<input aria-label=Phone id=ph><script>\
        ph.addEventListener('input', () => { const fromEnd = ph.value.length - ph.selectionEnd;\
          const d = ph.value.replace(/\\D/g, '').slice(0, 10);\
          ph.value = d.length > 6 ? `(${d.slice(0,3)}) ${d.slice(3,6)}-${d.slice(6)}` : d.length > 3 ? `(${d.slice(0,3)}) ${d.slice(3)}` : d;\
          const at = Math.max(0, ph.value.length - fromEnd); ph.setSelectionRange(at, at); v.textContent = ph.value; });</script><p id=v></p>";
    let env = Replay::new(site(vec![html("https://m.test/", page_html)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://m.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let ph = find(&obs, "textbox", "Phone");
    act(&mut tab, &format!("type {ph} 5551234567"));
    let v = text_of(tab.document(), "v");
    assert_eq!(v, "(555) 123-4567");
    // Typing in the middle inserts there.
    act(&mut tab, "press Home Right Right Right Right");
    act(&mut tab, "press Backspace");
    act(&mut tab, "type \"9\"");
    let v = text_of(tab.document(), "v");
    assert_eq!(v, "(559) 123-4567");
}

#[test]
fn select_names_missing_text() {
    let env = Replay::new(site(vec![html(
        "https://s.test/",
        "<p aria-label=Para>Some words here</p>",
    )]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://s.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let p = obs
        .lines()
        .find(|l| l.contains("Some words"))
        .and_then(|l| {
            l.split_whitespace()
                .find(|w| w.starts_with('e') && w[1..].chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_string);
    let Some(p) = p else { return };
    let err = tab
        .act(
            format!("select {p} missing")
                .parse()
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_err();
    assert!(err.to_string().contains("has no text \"missing\""), "{err}");
    let out = act(&mut tab, &format!("select {p} words"));
    assert!(out.to_string().starts_with("ok select"), "{out}");
}

#[test]
#[ignore = "timing: run in release (cargo test --release -- --ignored)"]
fn editing_is_fast() {
    // 500 characters typed into ProseMirror, key by key.
    let env = Replay::new(site(editor_site("pm")));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://editor.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    let ed = find(&obs, "textbox", "Comment");
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(12);
    let text = &text[..500];
    #[allow(clippy::disallowed_methods)]
    let t0 = std::time::Instant::now();
    act(&mut tab, &format!("type {ed} {text}"));
    let typing = t0.elapsed();
    assert!(text_of(tab.document(), "out").contains("lazy dog"));
    // 10,000 live ranges while 1,000 nodes are removed: the page with the removals, less the
    // same page without them.
    let page = |remove: bool| {
        format!(
            "<div id=box></div><p id=out></p><script>\
            for (let i = 0; i < 1000; i++) box.append(document.createElement('span'));\
            const keep = []; for (let i = 0; i < 10000; i++) {{ const r = document.createRange(); r.setStart(box, i % 1000); keep.push(r); }}\
            if ({remove}) while (box.firstChild) box.firstChild.remove();\
            out.textContent = keep.every(r => r.startContainer === box && (!{remove} || r.startOffset === 0)) ? 'ok' : 'bad';</script>"
        )
    };
    let time = |remove: bool| {
        let env = Replay::new(site(vec![html("https://r.test/", &page(remove))]));
        #[allow(clippy::disallowed_methods)]
        let t = std::time::Instant::now();
        let doc = pageon::load("https://r.test/", &env, LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(text_of(&doc, "out"), "ok");
        t.elapsed()
    };
    let best = |remove: bool| (0..3).map(|_| time(remove)).min().unwrap_or_default();
    let (base, with) = (best(false), best(true));
    let removal = with.saturating_sub(base);
    eprintln!("typing 500 chars: {typing:?}; 1,000 removals under 10,000 live ranges: {removal:?}");
    assert!(typing.as_secs_f64() < 3.0, "{typing:?}");
    assert!(removal.as_millis() < 50, "{removal:?}");
}
