//! The caged content process: the cage holds, and caged runs equal in-process runs.

mod common;

use common::*;
use pageon::{
    Action, Budget, Dump, LoadOptions, Mode, Replay, Sandbox, SandboxOptions, Session, Viewport,
};
use pageon_env::Log;

fn options() -> SandboxOptions {
    SandboxOptions {
        program: Some(env!("CARGO_BIN_EXE_pageon").into()),
        ..SandboxOptions::default()
    }
}

#[test]
fn the_cage_holds() {
    let mut sb = Sandbox::new(Replay::new(Log::default()), options())
        .unwrap_or_else(|e| panic!("{e}: {}", e.guidance()));
    let probe = sb.probe().unwrap_or_else(|e| panic!("{e}"));
    println!("{}\nprobe: {probe}", sb.receipt());
    assert_eq!(
        probe, "js=ok read-home=denied write-tmp=denied connect=denied exec=denied",
        "the cage allows scripts and nothing else"
    );
}

/// One run's transcript: every observation and outcome, then the final page.
fn transcript_in_process(log: Log, url: &str, actions: &[String]) -> Vec<String> {
    let session = Session::new(Replay::new(log));
    let mut tab = session
        .open(url, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    tab.set_view(Budget::o200k(4000), Mode::Diff);
    let mut out = vec![tab.observe()];
    for a in actions {
        let action: Action = a.parse().unwrap_or_else(|e| panic!("{e}"));
        out.push(match tab.act(action) {
            Ok(o) => o.to_string(),
            Err(e) => format!("error: {e}"),
        });
        out.push(tab.observe());
    }
    out.push(tab.view(Dump::Html, Viewport::default(), Budget::o200k(4000)));
    out
}

fn transcript_caged(log: Log, url: &str, actions: &[String]) -> Vec<String> {
    let mut sb = Sandbox::new(Replay::new(log), options()).unwrap_or_else(|e| panic!("{e}"));
    let mut tab = sb
        .open(url, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let mut out = vec![tab.observe().unwrap_or_else(|e| panic!("{e}"))];
    for a in actions {
        let action: Action = a.parse().unwrap_or_else(|e| panic!("{e}"));
        out.push(match tab.act(action) {
            Ok(o) => o.to_string(),
            Err(e) => format!("error: {e}"),
        });
        out.push(tab.observe().unwrap_or_else(|e| panic!("{e}")));
    }
    out.push(
        tab.view(Dump::Html, Viewport::default(), Budget::o200k(4000))
            .unwrap_or_else(|e| panic!("{e}")),
    );
    out
}

#[test]
fn caged_runs_equal_in_process_runs() {
    // A fixture flow: navigation, a POST form with a cookie, history.
    let ex = vec![
        html(
            "https://shop.test/login",
            "<form method=post action=/session><input name=user aria-label=User>\
             <button>Sign in</button></form><a href=/about>About</a>",
        ),
        ex(
            "POST",
            "https://shop.test/session",
            303,
            &[("location", "/account"), ("set-cookie", "sid=1; Path=/")],
            "",
        ),
        html(
            "https://shop.test/account",
            "<h1>Account</h1><div id=out></div><script>out.textContent = document.cookie</script>",
        ),
        html("https://shop.test/login", "<h1>Login again</h1>"),
    ];
    let actions: Vec<String> = ["type e1 ada", "press Enter", "wait 200", "back"]
        .iter()
        .map(ToString::to_string)
        .collect();
    let a = transcript_in_process(site(ex.clone()), "https://shop.test/login", &actions);
    let b = transcript_caged(site(ex), "https://shop.test/login", &actions);
    assert_eq!(a, b, "(fixtures)");
    assert!(
        a.iter()
            .any(|l| l.contains("navigated to \"https://shop.test/account\"")),
        "{a:#?}"
    );

    // MiniWoB tasks: the in-process run picks the actions, the caged run repeats them.
    let Some(rec) = miniwob_log() else {
        println!("skipped MiniWoB: run `uv run pageon-bench setup` in bench/");
        return;
    };
    for task in [
        "click-button",
        "enter-text",
        "login-user",
        "click-checkboxes",
        "choose-list",
    ] {
        let url = format!("{MINIWOB_ORIGIN}miniwob/{task}.html");
        let first = transcript_in_process(rec.clone(), &url, &[]);
        let obs = &first[0];
        let q = obs
            .lines()
            .find_map(|l| {
                l.trim_start()
                    .strip_prefix("text ")
                    .map(|t| quoted(t).concat())
            })
            .unwrap_or_default();
        let args: Vec<String> = q
            .split('"')
            .skip(1)
            .step_by(2)
            .map(|s| s.trim().to_string())
            .collect();
        let actions: Vec<String> = match task {
            "click-button" => vec![format!("click {}", find(obs, "button", &args[0]))],
            "enter-text" => vec![
                format!("type {} {}", refs(obs, "textbox")[0].0, args[0]),
                format!("click {}", find(obs, "button", "Submit")),
            ],
            "login-user" => vec![
                format!("type {} {}", refs(obs, "textbox")[0].0, args[0]),
                format!("type {} {}", refs(obs, "textbox")[1].0, args[1]),
                format!("click {}", find(obs, "button", "Login")),
            ],
            "click-checkboxes" => vec![format!("click {}", refs(obs, "checkbox")[0].0)],
            _ => vec![format!("click {}", refs(obs, "combobox")[0].0)],
        };
        let a = transcript_in_process(rec.clone(), &url, &actions);
        let b = transcript_caged(rec.clone(), &url, &actions);
        assert_eq!(a, b, "({task})");
    }
}

#[test]
fn state_crosses_the_cage() {
    let ex = vec![html(
        "https://a.test/",
        "<div id=out></div><script>out.textContent = localStorage.getItem('k') + ' ' + document.cookie; localStorage.setItem('n', '1');</script>",
    )];
    let mut sb = Sandbox::new(Replay::new(site(ex)), options()).unwrap_or_else(|e| panic!("{e}"));
    let state = pageon::StorageState::from_json(
        r#"{"cookies":[{"name":"c","value":"1","domain":"a.test","path":"/","expires":-1,"httpOnly":false,"secure":true,"sameSite":"Lax"}],
            "origins":[{"origin":"https://a.test","localStorage":[{"name":"k","value":"v"}]}]}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    sb.set_storage_state(&state)
        .unwrap_or_else(|e| panic!("{e}"));
    {
        let mut tab = sb
            .open("https://a.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let text = tab
            .view(Dump::Text, Viewport::default(), Budget::o200k(100))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(text.trim(), "v c=1");
    }
    let back = sb.storage_state().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(back.cookies.len(), 1);
    assert_eq!(back.origins[0].local_storage.len(), 2, "{back:?}");
}

#[test]
#[ignore = "timing; run in release"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn sandbox_overhead() {
    let Some(rec) = miniwob_log() else { return };
    let url = format!("{MINIWOB_ORIGIN}miniwob/click-button.html");
    // The first caged start of a freshly built binary includes the OS's one-time check of it
    // (≈ 1.7 s on macOS, once per binary, not per process): warm up first.
    drop(Sandbox::new(Replay::new(rec.clone()), options()).unwrap_or_else(|e| panic!("{e}")));
    let t = std::time::Instant::now();
    let mut sb =
        Sandbox::new(Replay::new(rec.clone()), options()).unwrap_or_else(|e| panic!("{e}"));
    let spawn_ms = t.elapsed().as_secs_f64() * 1000.0;
    let t = std::time::Instant::now();
    let mut tab = sb
        .open(&url, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let open_ms = t.elapsed().as_secs_f64() * 1000.0;
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    let r = refs(&obs, "button")[0].0.clone();
    let t = std::time::Instant::now();
    let _ = tab.act(Action::Click(r));
    let click_ms = t.elapsed().as_secs_f64() * 1000.0;
    let session = Session::new(Replay::new(rec));
    let t = std::time::Instant::now();
    let _tab = session
        .open(&url, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let local_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!(
        "sandbox: spawn {spawn_ms:.1} ms, open {open_ms:.1} ms (in-process {local_ms:.1} ms), click {click_ms:.1} ms"
    );
    assert!(spawn_ms <= 100.0, "spawn");
    assert!(click_ms <= 10.0, "click");
    assert!(open_ms <= 2.0 * local_ms + 5.0, "open");
}
