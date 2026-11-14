//! Replay, reset-to-step and divergence: recorded sessions re-run to the same page at
//! every step, caged and in-process; resets send nothing; divergences say where and why.

mod common;

use common::*;
use pageon::replay::{Caged, InProcess, rerun};
use pageon::{Action, LoadOptions, Recording, Replay, Sandbox, SandboxOptions, Session};
use pageon_env::{Kind, Log, Recorder};

/// Opens `url`, observes, and runs `plan` (which sees each observation and returns the next
/// action, or `None` to stop), recording everything over `site`.
fn record(
    site: Log,
    url: &str,
    options: LoadOptions,
    mut plan: impl FnMut(&str, usize) -> Option<String>,
) -> Log {
    let rec = Recording::new(Replay::new(site));
    let session = Session::new(&rec);
    let mut tab = session.open(url, options).unwrap_or_else(|e| panic!("{e}"));
    tab.set_view(pageon::Budget::o200k(100_000), pageon::Mode::Full);
    let mut i = 0;
    loop {
        let obs = tab.observe();
        let Some(a) = plan(&obs, i) else { break };
        let a: Action = a.parse().unwrap_or_else(|e| panic!("{e}"));
        tab.act(a).unwrap_or_else(|e| panic!("{e}"));
        i += 1;
    }
    drop(tab);
    rec.log()
}

/// Re-runs every step of `log` offline, in-process or caged.
fn replay(log: &Log, caged: bool) -> Result<usize, pageon_env::Divergence> {
    let last = log.steps.len() - 1;
    let rec = Recording::resume(log.clone(), last, Replay::new(log.clone()), true)
        .unwrap_or_else(|e| panic!("{e}"));
    let mut n = 0;
    let mut count = |_: usize, _: &pageon_env::Step| n += 1;
    if caged {
        let mut sb = Sandbox::new(
            &rec,
            SandboxOptions {
                program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        rerun(&mut Caged::new(&mut sb), &log.steps, last, &mut count)?;
    } else {
        let session = Session::new(&rec);
        rerun(&mut InProcess::new(&session), &log.steps, last, &mut count)?;
    }
    Ok(n)
}

fn identical(name: &str, log: &Log) {
    assert!(log.steps.len() >= 2, "{name}: steps {:?}", log.steps.len());
    for caged in [false, true] {
        match replay(log, caged) {
            Ok(n) => assert_eq!(n, log.steps.len(), "{name}"),
            Err(d) => panic!(
                "{name} ({}): {d}\n{}",
                if caged { "caged" } else { "in-process" },
                d.diff().join("\n")
            ),
        }
    }
}

fn login_site() -> Log {
    site(vec![
        html(
            "https://shop.test/login",
            r#"<form action=/session method=post><input name=user aria-label=User>
            <button>Sign in</button></form>"#,
        ),
        ex(
            "POST",
            "https://shop.test/session",
            200,
            &[("content-type", "text/html")],
            "<h1>Welcome</h1><a href=/orders>Orders</a>",
        ),
        html("https://shop.test/orders", "<h1>Orders</h1>"),
    ])
}

fn login(obs: &str, i: usize) -> Option<String> {
    match i {
        0 => Some(format!("type {} ada", find(obs, "textbox", "User"))),
        1 => Some(format!("click {}", find(obs, "button", "Sign in"))),
        2 => Some(format!("click {}", find(obs, "link", "Orders"))),
        _ => None,
    }
}

/// A recorded session replays to an identical page at every step.
#[test]
fn recorded_sessions_replay_identically() {
    let log = record(
        login_site(),
        "https://shop.test/login",
        LoadOptions::default(),
        login,
    );
    assert_eq!(log.steps.len(), 4);
    assert_eq!(log.steps[1].action, r#"type e1 "ada""#);
    assert!(
        log.steps[1].view.contains(r#"="ada""#),
        "{}",
        log.steps[1].view
    );
    identical("login", &log);

    // A corpus page with a cross-origin frame.
    let log = record(
        fixtures(),
        &format!("{ORIGIN}xorigin-iframe.html"),
        corpus_options(),
        |obs, i| (i == 0).then(|| format!("click {}", find(obs, "button", "Back"))),
    );
    identical("xorigin-iframe", &log);

    // MiniWoB++ episodes (scripts, timers, randomness).
    let Some(mw) = miniwob_log() else { return };
    for task in [
        "click-button",
        "enter-text",
        "login-user",
        "click-checkboxes",
        "choose-list",
    ] {
        let mut query = String::new();
        let log = record(
            mw.clone(),
            &format!("{MINIWOB_ORIGIN}miniwob/{task}.html"),
            LoadOptions::default(),
            |obs, i| {
                if i == 0 {
                    query = obs
                        .lines()
                        .find_map(|l| {
                            l.trim_start()
                                .strip_prefix("text ")
                                .map(|t| quoted(t).concat())
                        })
                        .unwrap_or_default();
                }
                let args: Vec<String> = query
                    .split('"')
                    .skip(1)
                    .step_by(2)
                    .map(|s| s.trim().to_string())
                    .collect();
                let boxes = refs(obs, "textbox");
                match (task, i) {
                    ("click-button", 0) => Some(format!("click {}", find(obs, "button", &args[0]))),
                    ("enter-text", 0) => Some(format!("type {} {}", boxes[0].0, args[0])),
                    ("login-user", 0) => Some(format!("type {} {}", boxes[0].0, args[0])),
                    ("login-user", 1) => Some(format!("type {} {}", boxes[1].0, args[1])),
                    ("login-user", 2) | ("enter-text", 1) => {
                        let name = if task == "login-user" {
                            "Login"
                        } else {
                            "Submit"
                        };
                        Some(format!("click {}", find(obs, "button", name)))
                    }
                    ("click-checkboxes", 0) | ("choose-list", 0) => Some("wait 100".into()),
                    _ => None,
                }
            },
        );
        identical(task, &log);
    }
}

/// recording steps changes nothing the agent sees.
#[test]
fn step_views_leave_the_agent_view_alone() {
    let mut plain = Vec::new();
    let session = Session::new(Replay::new(login_site()));
    let mut tab = session
        .open("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    for i in 0.. {
        let obs = tab.observe();
        plain.push(obs.clone());
        let Some(a) = login(&obs, i) else { break };
        tab.act(a.parse().unwrap_or_else(|e: String| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    drop(tab);
    let rec = Recording::new(Replay::new(login_site()));
    let session = Session::new(&rec);
    let mut tab = session
        .open("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    for (i, want) in plain.iter().enumerate() {
        let obs = tab.observe();
        assert_eq!(&obs, want, "observation {i}");
        if let Some(a) = login(&obs, i) {
            tab.act(a.parse().unwrap_or_else(|e: String| panic!("{e}")))
                .unwrap_or_else(|e| panic!("{e}"));
        }
    }
}

/// divergences name the step and the kind, with the view diff.
#[test]
fn divergences_are_explained() {
    let log = record(
        login_site(),
        "https://shop.test/login",
        LoadOptions::default(),
        login,
    );
    // An edited response: the page differs at step 2 (after the POST).
    let mut edited = log.clone();
    let post = edited
        .exchanges
        .iter_mut()
        .find(|e| e.method == "POST")
        .unwrap_or_else(|| panic!("POST"));
    post.body = Some("<h1>Goodbye</h1><a href=/orders>Orders</a>".into());
    let d = replay(&edited, false).unwrap_err();
    assert_eq!((d.step, d.kind), (2, Kind::Page), "{d}");
    let diff = d.diff().join("\n");
    assert!(
        diff.contains(r#"- # "Welcome""#) && diff.contains(r#"+ # "Goodbye""#),
        "{diff}"
    );
    // A missing exchange: the request is named at the step that needs it.
    let mut missing = log.clone();
    missing.exchanges.retain(|e| !e.url.ends_with("/orders"));
    let d = replay(&missing, true).unwrap_err();
    assert_eq!((d.step, d.kind), (3, Kind::Request), "{d}");
    assert!(d.detail.contains("GET https://shop.test/orders"), "{d}");
    // An action that now fails: the button is gone.
    let mut gone = log.clone();
    gone.exchanges
        .iter_mut()
        .find(|e| e.url.ends_with("/login"))
        .unwrap_or_else(|| panic!("login"))
        .body =
        Some("<form action=/session method=post><input name=user aria-label=User></form>".into());
    let d = replay(&gone, false).unwrap_err();
    assert!(matches!(d.kind, Kind::Action | Kind::Page), "{d}");
    assert_eq!(d.step, 0, "the page differs from the start: {d}");
}

/// a reset rebuilds step k exactly and sends nothing; the POST reaches the server once.
#[test]
fn resets_are_exact_and_send_nothing() {
    let server = Capture::new(login_site());
    let rec = Recording::new(&server);
    let session = Session::new(&rec);
    let mut tab = session
        .open("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    for i in 0.. {
        let obs = tab.observe();
        let Some(a) = login(&obs, i) else { break };
        tab.act(a.parse().unwrap_or_else(|e: String| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    drop(tab);
    let sent = server.urls().len();
    let posts = || {
        server
            .urls()
            .iter()
            .filter(|u| u.ends_with("/session"))
            .count()
    };
    assert_eq!(posts(), 1);
    for k in [3, 2, 1, 2] {
        let steps_now = rec.steps();
        rec.rewind(k).unwrap_or_else(|e| panic!("{e}"));
        session.clear();
        let mut d = InProcess::new(&session);
        rerun(&mut d, &steps_now, k, &mut |_, _| {})
            .unwrap_or_else(|e| panic!("reset to {k}: {e}"));
        assert_eq!(server.urls().len(), sent, "reset to {k} sent a request");
        let tab = d.tab().unwrap_or_else(|| panic!("tab"));
        assert_eq!(tab.state_hash(), steps_now[k].hash, "reset to {k}");
        assert_eq!(rec.len(), k + 1, "later steps are gone");
        if k == 1 {
            // Live again: the next action is step 2.
            tab.act(r#"type e1 bob"#.parse().unwrap_or_else(|e: String| panic!("{e}")))
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(rec.len(), 3);
            assert_eq!(rec.steps()[2].action, r#"type e1 "bob""#);
            assert_eq!(posts(), 1, "resets never post again");
        }
    }
}

fn pageon(args: &[&str]) -> (i32, String, String) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_pageon"))
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// The CLI: record with `fetch --act --record`, prove with `replay`, inspect with `--to`.
#[test]
fn the_replay_command() {
    let dir = std::env::temp_dir().join(format!("pageon-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let page = root().join("bench/fixtures/pages/checkout.html");
    let page = page.canonicalize().unwrap_or_else(|e| panic!("{e}"));
    let ep = dir.join("ep.json");
    let (code, _, err) = pageon(&[
        "fetch",
        page.to_str().unwrap_or_default(),
        "--no-sandbox",
        "--act",
        "type e9 SAVE10",
        "--act",
        "click e10",
        "--record",
        ep.to_str().unwrap_or_default(),
    ]);
    assert_eq!(code, 0, "{err}");
    let ep_s = ep.to_str().unwrap_or_default();
    for extra in [&[][..], &["--no-sandbox"][..]] {
        let (code, out, err) = pageon(&[&["replay", ep_s][..], extra].concat());
        assert_eq!(code, 0, "{out}{err}");
        assert!(
            out.contains("✓ 1 type e9 \"SAVE10\"") && out.ends_with("identical: 3 steps\n"),
            "{out}"
        );
    }
    let (code, out, _) = pageon(&["replay", ep_s, "--to", "1"]);
    assert_eq!(code, 0);
    assert!(
        out.contains(r#"textbox e9 ph:"Coupon code" ="SAVE10""#),
        "{out}"
    );
    let (code, _, err) = pageon(&["replay", ep_s, "--to", "9"]);
    assert_eq!(code, 2);
    assert!(err.contains("steps 0 to 2"), "{err}");
    // An edited page diverges at step 0, with the diff.
    let text = std::fs::read_to_string(&ep).unwrap_or_default();
    let edited = dir.join("edited.json");
    let mut log: Log = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
    log.exchanges[0].body = log.exchanges[0]
        .body
        .as_ref()
        .map(|b| b.replace("Pay now", "Pay later"));
    std::fs::write(&edited, serde_json::to_string(&log).unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));
    let (code, out, _) = pageon(&["replay", edited.to_str().unwrap_or_default()]);
    assert_eq!(code, 5, "{out}");
    assert!(
        out.contains("✗ 0 open")
            && out.contains("- button e11 \"Pay now\"")
            && out.contains("diverged at step 0 of 3"),
        "{out}"
    );
    // A recording without steps.
    let mut log: Log = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
    log.steps.clear();
    let old = dir.join("old.json");
    std::fs::write(&old, serde_json::to_string(&log).unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));
    let (code, _, err) = pageon(&["replay", old.to_str().unwrap_or_default()]);
    assert_eq!(code, 2);
    assert!(
        err.contains("has no steps") && err.contains("--record"),
        "{err}"
    );
    // `--live`: the same steps against the site as it is now. A changed page is reported.
    let site = dir.join("site.html");
    std::fs::copy(&page, &site).unwrap_or_else(|e| panic!("{e}"));
    let live = dir.join("live.json");
    let (code, _, err) = pageon(&[
        "fetch",
        site.to_str().unwrap_or_default(),
        "--no-sandbox",
        "--act",
        "click e10",
        "--record",
        live.to_str().unwrap_or_default(),
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = pageon(&["replay", live.to_str().unwrap_or_default(), "--live"]);
    assert_eq!(code, 0, "unchanged site: {out}{err}");
    let html = std::fs::read_to_string(&site).unwrap_or_default();
    std::fs::write(&site, html.replace("Pay now", "Sold out")).unwrap_or_else(|e| panic!("{e}"));
    let (code, out, _) = pageon(&["replay", live.to_str().unwrap_or_default(), "--live"]);
    assert_eq!(code, 5, "{out}");
    assert!(out.contains("+ button e11 \"Sold out\""), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// (release): resetting to step 3 of a MiniWoB++ episode, caged, ≤ 100 ms.
#[test]
#[ignore = "timing; run in release"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn reset_is_fast() {
    let Some(mw) = miniwob_log() else { return };
    let rec = Recording::new(Replay::new(mw));
    let mut sb = Sandbox::new(
        &rec,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let (id, _) = sb
        .open_tab(
            &format!("{MINIWOB_ORIGIN}miniwob/login-user.html"),
            LoadOptions::default(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = sb.observe_tab(id, true).unwrap_or_else(|e| panic!("{e}"));
    let boxes = refs(&obs, "textbox");
    for a in [
        format!("type {} ada", boxes[0].0),
        format!("type {} pw", boxes[1].0),
        "wait 100".into(),
    ] {
        sb.act_tab(id, a.parse().unwrap_or_else(|e: String| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    let steps = rec.steps();
    assert_eq!(steps.len(), 4);
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = std::time::Instant::now();
        rec.rewind(3).unwrap_or_else(|e| panic!("{e}"));
        sb.clear().unwrap_or_else(|e| panic!("{e}"));
        rerun(&mut Caged::new(&mut sb), &steps, 3, &mut |_, _| {})
            .unwrap_or_else(|e| panic!("{e}"));
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    println!("reset to step 3 (caged): {best:.1} ms");
    assert!(best <= 100.0, "{best:.1} ms");
}
