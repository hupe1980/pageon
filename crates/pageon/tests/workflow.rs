//! Workflows: a recorded session compiles into role-and-name steps with parameters,
//! reruns without a model, explains drift precisely, and is repaired by compiling again.

mod common;

use std::collections::HashMap;

use common::*;
use pageon::replay::{Caged, InProcess};
use pageon::workflow::{self, Do, Locator, Status, Workflow};
use pageon::{LoadOptions, Recording, Replay, Sandbox, SandboxOptions, Secret, Secrets, Session};

const LOGIN: &str = r#"<title>Log in</title><form method=post action=/session>
<label>User <input name=user></label><label>Password <input name=pw type=password></label>
<button>Sign in</button></form>"#;

const ACCOUNT: &str =
    r#"<title>Account</title><nav><a href=/help>Help</a> <a href=/report>Report</a></nav>"#;

/// The shop, with each page's markup replaceable to make a changed site.
fn shop(login: &str, account: &str, after_login: &str) -> pageon_env::Log {
    site(vec![
        html("https://shop.test/login", login),
        ex(
            "POST",
            "https://shop.test/session",
            303,
            &[("location", after_login)],
            "",
        ),
        html("https://shop.test/account", account),
        html("https://shop.test/login?error=1", LOGIN),
        html(
            "https://shop.test/report",
            "<title>Report</title><h1>Sales</h1>",
        ),
        html(
            "https://shop.test/reports",
            "<title>Reports</title><h1>Archive</h1>",
        ),
    ])
}

fn original() -> pageon_env::Log {
    shop(LOGIN, ACCOUNT, "/account")
}

fn vault<E: pageon_env::Env>(env: E) -> Secrets<E> {
    Secrets::new(env).with(
        "shop",
        Secret::value("hunter2").origins(["https://shop.test"]),
    )
}

/// Records the 4-step task once, in-process: type the user, type the secret, sign in, open the
/// report.
fn recorded() -> pageon_env::Log {
    let rec = Recording::new(Replay::new(original()));
    let env = vault(&rec);
    let session = Session::new(&env);
    let mut tab = session
        .open("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    for a in [
        format!("type {} ada", find(&obs, "textbox", "User")),
        format!("type {} secret://shop", find(&obs, "textbox", "Password")),
        format!("click {}", find(&obs, "button", "Sign in")),
    ] {
        tab.act(a.parse().unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{a}: {e}"));
    }
    let obs = tab.observe();
    let report = find(&obs, "link", "Report");
    tab.act(
        format!("click {report}")
            .parse()
            .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    drop(tab);
    rec.log()
}

fn compiled() -> Workflow {
    workflow::compile(&recorded().steps).unwrap_or_else(|e| panic!("{e}"))
}

fn params(user: &str) -> HashMap<String, String> {
    HashMap::from([("user".to_string(), user.to_string())])
}

/// Runs `wf` on `log`'s site, in-process or caged; returns the report and the lines it printed.
fn run_on(
    wf: &Workflow,
    log: pageon_env::Log,
    caged: bool,
) -> (workflow::Report, Vec<String>, Vec<String>) {
    let cap = Capture::new(log);
    let env = vault(&cap);
    let mut lines = Vec::new();
    let report = if caged {
        let mut sb = Sandbox::new(
            &env,
            SandboxOptions {
                program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                ..SandboxOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        workflow::run(
            &mut Caged::new(&mut sb),
            wf,
            &params("bob"),
            LoadOptions::default(),
            &mut |l| lines.push(l.to_string()),
        )
    } else {
        let session = Session::new(&env);
        workflow::run(
            &mut InProcess::new(&session),
            wf,
            &params("bob"),
            LoadOptions::default(),
            &mut |l| lines.push(l.to_string()),
        )
    }
    .unwrap_or_else(|e| panic!("{e}"));
    (report, lines, cap.urls())
}

#[test]
fn a_session_compiles_to_named_steps_with_parameters() {
    let wf = compiled();
    assert_eq!(wf.start, "https://shop.test/login");
    assert_eq!(wf.params.len(), 1);
    assert_eq!(
        (wf.params[0].name.as_str(), wf.params[0].default.as_deref()),
        ("user", Some("ada"))
    );
    let shown: Vec<String> = wf.steps.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            r#"type textbox "User" "{{user}}""#,
            r#"type textbox "Password" "secret://shop""#,
            r#"click button "Sign in""#,
            r#"click link "Report""#,
        ]
    );
    let expects: Vec<Option<String>> = wf
        .steps
        .iter()
        .map(|s| match &s.action {
            Do::Click { expect, .. } | Do::Type { expect, .. } => expect.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        expects,
        [
            None,
            None,
            Some("https://shop.test/account".into()),
            Some("https://shop.test/report".into())
        ]
    );
    // No ref, no secret value, no recorded body in the file.
    let json = serde_json::to_string(&wf).unwrap_or_default();
    assert!(
        !json.contains("hunter2") && !json.contains("\"e1\""),
        "{json}"
    );
    assert!(matches!(
        &wf.steps[0].action,
        Do::Type {
            target: Locator::Element(_),
            ..
        }
    ));
}

#[test]
fn it_reruns_with_new_values_where_refs_moved() {
    let wf = compiled();
    let banner = |page: &str| {
        format!("<a href=/a>Sale</a> <a href=/b>New</a> <button>Dismiss</button>{page}")
    };
    for caged in [false, true] {
        for (name, log) in [
            ("original", original()),
            ("twin", shop(&banner(LOGIN), &banner(ACCOUNT), "/account")),
        ] {
            let (r, lines, urls) = run_on(&wf, log, caged);
            assert_eq!(
                r.status,
                Status::Done,
                "{name} caged={caged}: {r}\n{lines:#?}"
            );
            assert_eq!(r.to_string(), "done: 4 steps");
            assert!(r.drifts.is_empty());
            assert!(
                urls.contains(&"GET https://shop.test/report".to_string()),
                "{name}: {urls:?}"
            );
        }
    }
}

#[test]
fn drift_is_detected_and_explained() {
    let wf = compiled();
    // Renamed: done, the rename reported.
    let (r, lines, _) = run_on(
        &wf,
        shop(
            &LOGIN.replace(">Sign in<", ">Sign in now<"),
            ACCOUNT,
            "/account",
        ),
        false,
    );
    assert_eq!(r.status, Status::Done, "{r}");
    assert_eq!(r.to_string(), "done: 4 steps, 1 drift");
    assert_eq!(
        (
            r.drifts[0].step,
            r.drifts[0].from.as_str(),
            r.drifts[0].to.as_str()
        ),
        (3, "Sign in", "Sign in now")
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("~ 3 click button \"Sign in\"")),
        "{lines:#?}"
    );
    // Reordered fields: names, not positions.
    let reordered = r#"<title>Log in</title><form method=post action=/session>
<label>Password <input name=pw type=password></label><label>User <input name=user></label>
<button>Sign in</button></form>"#;
    let (r, _, _) = run_on(&wf, shop(reordered, ACCOUNT, "/account"), false);
    assert_eq!((r.status, r.drifts.len()), (Status::Done, 0), "{r}");
    // Removed: stops at the step, with what the page offers instead.
    let removed =
        r#"<title>Account</title><nav><a href=/help>Help</a> <a href=/reports>Archive</a></nav>"#;
    let (r, _, urls) = run_on(&wf, shop(LOGIN, removed, "/account"), false);
    assert_eq!(r.status, Status::Drifted);
    let stop = r.stop.clone().unwrap_or_else(|| panic!("no stop"));
    assert_eq!(
        (stop.step, stop.reason.as_str()),
        (4, r#"no link "Report" on the page"#)
    );
    assert_eq!(stop.candidates.len(), 2, "{:?}", stop.candidates);
    assert!(
        stop.candidates.iter().all(|c| c.starts_with("link e")),
        "{:?}",
        stop.candidates
    );
    assert!(stop.view.contains("\"Archive\""));
    assert!(
        !urls
            .iter()
            .any(|u| u.contains("/reports") || u.contains("/help")),
        "no wrong click: {urls:?}"
    );
    // Misdirected: the login lands elsewhere; it stops at the login step.
    let (r, _, urls) = run_on(&wf, shop(LOGIN, ACCOUNT, "/login?error=1"), false);
    let stop = r.stop.clone().unwrap_or_else(|| panic!("no stop: {r}"));
    assert_eq!(
        (stop.step, stop.reason.as_str()),
        (
            3,
            "expected https://shop.test/account, at https://shop.test/login"
        )
    );
    assert!(!urls.iter().any(|u| u.contains("/report")), "{urls:?}");
}

#[test]
fn the_cli_compiles_and_runs() {
    let dir = std::env::temp_dir().join(format!("pageon-workflow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let rec = dir.join("ep.json");
    std::fs::write(&rec, serde_json::to_string(&recorded()).unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));
    let flow = dir.join("report.flow.json");
    let out = assert_cmd::Command::cargo_bin("pageon")
        .unwrap_or_else(|e| panic!("{e}"))
        .args(["workflow", "compile"])
        .arg(&rec)
        .arg("-o")
        .arg(&flow)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("compiled 4 steps from https://shop.test/login; parameters: user"),
        "{err}"
    );
    let run = |site: &pageon_env::Log| {
        let replay = file(&format!("workflow-site-{}", site.exchanges.len()), site);
        assert_cmd::Command::cargo_bin("pageon")
            .unwrap_or_else(|e| panic!("{e}"))
            .args(["workflow", "run"])
            .arg(&flow)
            .args([
                "--param",
                "user=bob",
                "--secret",
                "shop@https://shop.test",
                "--replay",
            ])
            .arg(&replay)
            .env("PAGEON_SECRET_SHOP", "hunter2")
            .output()
            .unwrap_or_else(|e| panic!("{e}"))
    };
    let out = run(&original());
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(
        err.contains("✓ 4 click link \"Report\"") && err.contains("done: 4 steps"),
        "{err}"
    );
    let mut removed = original();
    removed.exchanges.retain(|e| !e.url.ends_with("/report"));
    for e in &mut removed.exchanges {
        if e.url.ends_with("/account") {
            e.body = Some("<title>Account</title><a href=/help>Help</a>".into());
        }
    }
    let out = run(&removed);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(7), "{err}");
    assert!(
        err.contains("drifted at step 4: no link \"Report\" on the page"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "timing: cargo test --release -p pageon --test workflow -- --ignored --nocapture"]
#[allow(clippy::disallowed_methods)]
fn runs_are_fast() {
    let wf = compiled();
    let mut per = Vec::new();
    for _ in 0..20 {
        let t = std::time::Instant::now();
        let (r, _, _) = run_on(&wf, original(), false);
        assert_eq!(r.status, Status::Done);
        per.push(t.elapsed().as_secs_f64() * 1000.0 / 5.0);
    }
    per.sort_by(f64::total_cmp);
    println!(
        "workflow run: {:.2} ms per step (median of 20, in-process)",
        per[10]
    );
    assert!(per[10] <= 20.0);
}
