//! The `pageon fetch` command over recordings: dumps, summary, every exit code with guidance.

mod common;

use assert_cmd::Command;
use common::*;

fn pageon() -> Command {
    Command::cargo_bin("pageon").unwrap_or_else(|e| panic!("{e}"))
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = pageon()
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn dumps_and_summary() {
    let rec = file(
        "article",
        &log(vec![page(
            "https://a.test/",
            200,
            &[("content-type", "text/html")],
            ARTICLE,
        )]),
    );
    let rec = rec.to_string_lossy();
    let (code, out, err) = run(&["fetch", "https://a.test/", "--replay", &rec]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.starts_with("Home\nTitle\nFirst bold paragraph."),
        "{out}"
    );
    assert_eq!(
        err.trim(),
        format!(
            "https://a.test/ · 200 · text/html · windows-1252 (default) · {} B · 42 ms",
            ARTICLE.len()
        )
    );
    let (_, html, _) = run(&[
        "fetch",
        "https://a.test/",
        "--replay",
        &rec,
        "--dump",
        "html",
    ]);
    assert!(html.starts_with("<!DOCTYPE html><html><head><title>Article</title>"));
    let (_, tree, _) = run(&[
        "fetch",
        "https://a.test/",
        "--replay",
        &rec,
        "--dump",
        "tree",
    ]);
    assert!(tree.contains("\n      a href=\"/\" \"Home\"\n"), "{tree}");
    // Byte-identical across runs.
    assert_eq!(
        run(&[
            "fetch",
            "https://a.test/",
            "--replay",
            &rec,
            "--dump",
            "tree"
        ])
        .1,
        tree
    );
}

#[test]
fn exit_codes_and_guidance() {
    let cases: Vec<(&str, pageon_env::Log, i32, &str)> = vec![
        (
            "http-error",
            log(vec![page("https://a.test/", 500, &[], "<p>oops</p>")]),
            4,
            "server's error page",
        ),
        (
            "pdf",
            log(vec![bytes("https://a.test/", "application/pdf", b"%PDF")]),
            3,
            "dedicated tool",
        ),
        ("miss", log(vec![]), 5, "--record"),
        (
            "loop",
            log(vec![
                redirect("https://a.test/", "https://a.test/b"),
                redirect("https://a.test/b", "https://a.test/"),
            ]),
            3,
            "report the loop",
        ),
    ];
    for (name, l, want, hint) in cases {
        let rec = file(name, &l);
        let (code, _, err) = run(&[
            "fetch",
            "https://a.test/",
            "--replay",
            &rec.to_string_lossy(),
        ]);
        assert_eq!(code, want, "{name}: {err}");
        assert!(
            err.contains("error: ") && err.contains(hint),
            "{name}: {err}"
        );
    }
    let (code, _, err) = run(&["fetch", "ftp://a.test/"]);
    assert_eq!(code, 3);
    assert!(err.contains("http:, https: and file:"), "{err}");
    let (code, _, err) = run(&["fetch", "/definitely/not/here.html"]);
    assert_eq!(code, 3);
    assert!(err.contains("exists and is readable"), "{err}");
    let (code, _, _) = run(&["fetch"]);
    assert_eq!(code, 2, "usage error");
    let (code, _, err) = run(&["fetch", "https://a.test/", "--replay", "/no/such.rec.json"]);
    assert_eq!(code, 3);
    assert!(err.contains("--record"), "{err}");
}

#[test]
fn local_files_load_without_network() {
    let dir = std::env::temp_dir().join(format!("pageon-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let p = dir.join("page.html");
    std::fs::write(&p, ARTICLE).unwrap_or_else(|e| panic!("{e}"));
    let (code, out, err) = run(&["fetch", &p.to_string_lossy()]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("First bold paragraph."));
    assert!(err.starts_with("file://"), "{err}");
}

#[test]
fn network_flags() {
    let mut l = log(vec![
        page(
            "https://a.test/",
            200,
            &[("content-type", "text/html")],
            "<script>fetch('http://10.0.0.1/x').catch(() => {})</script>",
        ),
        page(
            "http://localhost:3000/",
            200,
            &[("content-type", "text/html")],
            "<p>local</p>",
        ),
    ]);
    l.clock = (0..32).collect();
    l.random = vec![1; 8];
    let rec = file("network", &l);
    let rec = rec.to_string_lossy();

    let (code, out, err) = run(&[
        "fetch",
        "https://a.test/",
        "--replay",
        &rec,
        "--dump",
        "requests",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        out,
        "GET 200 document https://a.test/\nGET blocked fetch http://10.0.0.1/x (private network address 10.0.0.1)\n"
    );
    assert!(
        err.contains("2 requests · 1 blocked (--dump requests)"),
        "{err}"
    );

    let (code, _, err) = run(&["fetch", "http://localhost:3000/", "--replay", &rec]);
    assert_eq!(code, 3);
    assert!(
        err.contains("blocked http://localhost:3000/: private network address localhost")
            && err.contains("--private-network allow"),
        "{err}"
    );
    let (code, out, err) = run(&[
        "fetch",
        "http://localhost:3000/",
        "--replay",
        &rec,
        "--private-network",
        "allow",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out, "local\n");

    let (code, _, err) = run(&[
        "fetch",
        "https://a.test/",
        "--replay",
        &rec,
        "--cookies",
        "/nonexistent/state.json",
    ]);
    assert_eq!(code, 3);
    assert!(err.contains("check that the file exists"), "{err}");
}

#[test]
fn actions_from_the_command_line() {
    let mut l = log(vec![
        page(
            "https://a.test/",
            200,
            &[("content-type", "text/html")],
            "<a href=/next>Next</a><button disabled>Off</button>",
        ),
        page(
            "https://a.test/next",
            200,
            &[("content-type", "text/html")],
            "<h1>Next page</h1>",
        ),
    ]);
    l.clock = (0..64).collect();
    l.random = vec![1; 16];
    let rec = file("act", &l);
    let rec = rec.to_string_lossy();
    let (code, out, err) = run(&[
        "fetch",
        "https://a.test/",
        "--replay",
        &rec,
        "--act",
        "click e1",
        "--dump",
        "ax",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("ok click e1 · navigated to \"https://a.test/next\""),
        "{err}"
    );
    assert!(out.contains("# \"Next page\""), "{out}");
    let (code, _, err) = run(&[
        "fetch",
        "https://a.test/",
        "--replay",
        &rec,
        "--act",
        "click e2",
    ]);
    assert_eq!(code, 3);
    assert!(
        err.contains("e2 is not actionable: disabled") && err.contains("wait until it is enabled"),
        "{err}"
    );
    let (code, _, err) = run(&[
        "fetch",
        "https://a.test/",
        "--replay",
        &rec,
        "--act",
        "jump e1",
    ]);
    assert_eq!(code, 2);
    assert!(err.contains("unknown action"), "{err}");
}

#[test]
fn sandbox_policy_secrets_and_state_from_the_command_line() {
    let dir = std::env::temp_dir().join(format!("pageon-cli-008-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let mut l = log(vec![page(
        "https://a.test/",
        200,
        &[("content-type", "text/html")],
        "<input type=password aria-label=Password><div id=out></div>\
             <script>out.textContent = (localStorage.getItem('k') || '-') + ' ' + document.cookie;\
             localStorage.setItem('seen', '1'); fetch('https://evil.test/x').catch(() => {});</script>",
    )]);
    l.clock = (0..64).collect();
    l.random = vec![1; 16];
    let rec = file("cli008", &l);
    let rec = rec.to_string_lossy();
    let state = dir.join("state.json");
    std::fs::write(
        &state,
        r#"{"cookies":[{"name":"c","value":"1","domain":"a.test","path":"/","expires":-1,"httpOnly":false,"secure":true,"sameSite":"Lax"}],
            "origins":[{"origin":"https://a.test","localStorage":[{"name":"k","value":"v"}]}]}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let saved = dir.join("saved.json");
    let out = pageon()
        .args([
            "fetch",
            "https://a.test/",
            "--replay",
            &rec,
            "--origin",
            "https://a.test",
            "--state",
            &state.to_string_lossy(),
            "--save-state",
            &saved.to_string_lossy(),
            "--secret",
            "pw@https://a.test",
            "--act",
            "type e1 secret://pw",
            "--dump",
            "requests",
        ])
        .env("PAGEON_SECRET_PW", "s3cr3t-value")
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("ok type e1 \"secret://pw\""), "{stderr}");
    assert!(stdout.contains("GET blocked fetch https://evil.test/x (policy: https://evil.test is outside the task's origins)"), "{stdout}");
    for text in [&stdout, &stderr] {
        assert!(!text.contains("s3cr3t-value"), "{text}");
    }
    let saved = std::fs::read_to_string(&saved).unwrap_or_default();
    assert!(
        saved.contains("\"seen\"") && saved.contains("\"name\": \"c\""),
        "{saved}"
    );
    // A missing secret value says how to provide it.
    let (code, _, err) = run(&[
        "fetch",
        "https://a.test/",
        "--replay",
        &rec,
        "--secret",
        "nope@https://a.test",
    ]);
    assert_eq!(code, 2);
    assert!(err.contains("PAGEON_SECRET_NOPE"), "{err}");
}

#[test]
fn page_text_cannot_drive_the_terminal() {
    let rec = file(
        "escapes",
        &log(vec![page(
            "https://a.test/",
            200,
            &[("content-type", "text/html; charset=utf-8")],
            "<title>T\u{1b}]0;pwned\u{7}</title><p>before\u{1b}[2Jafter\u{9b}31m red\ttab</p>",
        )]),
    );
    let rec = rec.to_string_lossy();
    for dump in ["text", "html", "ax"] {
        let (code, out, err) = run(&["fetch", "https://a.test/", "--replay", &rec, "--dump", dump]);
        assert_eq!(code, 0, "{err}");
        for s in [&out, &err] {
            assert!(
                !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t'),
                "{dump}: a raw control character: {s:?}"
            );
        }
        assert!(
            out.contains("\\u001b") || out.contains("\\u001B"),
            "{dump}: {out}"
        );
    }
    let (_, out, _) = run(&["fetch", "https://a.test/", "--replay", &rec]);
    assert!(
        out.contains("before\\u001b[2Jafter\\u009b31m red tab"),
        "{out}"
    );
}
