//! Named sessions from the shell: `pageon open`, the session commands, `close`, `sessions`,
//! `skill`, caged and in-process.
#![cfg(unix)]

use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;

const PAGE: &str = r##"<title>Shop</title><h1>Shop</h1>
<form method=post action=/login>
<label>User <input name=user></label>
<label>Password <input type=password name=pw></label>
<button type=button onclick="document.querySelector('h1').textContent='Hi '+document.querySelector('input').value">Greet</button>
<button>Sign in</button>
</form>
<a href="#report">Report</a>"##;

const SECRET: &str = "hunter2-zz9-session";

/// A sessions directory of its own (short enough for a socket path).
struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!("pgs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Self(d)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_pageon"))
            .args(["close", "--all"])
            .env("PAGEON_SESSIONS", &self.0)
            .output();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn private(d: &Path) {
    std::fs::create_dir_all(d).unwrap_or_else(|e| panic!("{e}"));
    std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
        .unwrap_or_else(|e| panic!("{e}"));
}

fn pageon(dir: &Dir, args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_pageon"))
        .args(args)
        .env("PAGEON_SESSIONS", &dir.0)
        .env_remove("PAGEON_SESSION")
        .env("PAGEON_SECRET_SHOP", SECRET)
        .output()
        .unwrap_or_else(|e| panic!("{e}"))
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn ok(o: &Output) -> String {
    assert!(o.status.success(), "{}\n{}", out(o), err(o));
    out(o)
}

/// A local page server: the page for GET, an echo of nothing for POST; it keeps every request.
fn serve() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let origin = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().map(|a| a.port()).unwrap_or(0)
    );
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut s = stream;
            let mut buf = vec![0u8; 16384];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = if req.starts_with("GET / ") {
                PAGE
            } else {
                "<title>Done</title><p>Signed in"
            };
            if let Ok(mut l) = log.lock() {
                l.push(req);
            }
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (origin, seen)
}

fn page_file(dir: &Dir) -> String {
    let p = dir.0.with_extension("html");
    std::fs::write(&p, PAGE).unwrap_or_else(|e| panic!("{e}"));
    p.display().to_string()
}

fn browse(caged: bool) {
    let dir = Dir::new(if caged { "c" } else { "l" });
    let page = page_file(&dir);
    let mut open = vec!["open", page.as_str(), "--allow-files"];
    if !caged {
        open.push("--no-sandbox");
    }
    // No session yet.
    let o = pageon(&dir, &["click", "e1"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(
        err(&o).contains("start one with: pageon open URL"),
        "{}",
        err(&o)
    );

    let v = ok(&pageon(&dir, &open));
    assert!(v.contains("textbox e1 \"User\""), "{v}");
    assert!(v.trim_end().ends_with("step 0"), "{v}");
    let v = ok(&pageon(&dir, &["type", "e1", "ada"]));
    assert!(v.contains("@diff"), "{v}");
    let v = ok(&pageon(&dir, &["click", "e3"]));
    assert!(v.contains("# \"Hi ada\""), "{v}");
    let v = ok(&pageon(&dir, &["find", "report"]));
    assert!(v.contains("link e5 \"Report\""), "{v}");
    let v = ok(&pageon(&dir, &["view", "markdown"]));
    assert!(v.contains("# Hi ada"), "{v}");
    let v = ok(&pageon(&dir, &["view"]));
    assert!(v.starts_with("@page"), "{v}");

    // A tool error: exit 1, the error and its guidance on stderr.
    let o = pageon(&dir, &["click", "e99"]);
    assert_eq!(o.status.code(), Some(3));
    assert!(
        err(&o).starts_with("error: unknown ref e99\n  "),
        "{}",
        err(&o)
    );

    // Screenshots go to the named file.
    let png = dir.0.with_extension("png");
    let v = ok(&pageon(
        &dir,
        &["screenshot", &png.display().to_string(), "--marks"],
    ));
    assert!(v.contains("wrote "), "{v}");
    let bytes = std::fs::read(&png).unwrap_or_default();
    assert!(bytes.starts_with(b"\x89PNG"), "not a PNG");
    let _ = std::fs::remove_file(&png);

    // Reset to step 1 (typed, not yet greeted).
    let v = ok(&pageon(&dir, &["reset", "1"]));
    assert!(v.contains("step 1"), "{v}");

    // A second session is a separate browser.
    let mut open_b = vec!["-s", "b"];
    open_b.extend_from_slice(&open);
    ok(&pageon(&dir, &open_b));
    let v = ok(&pageon(&dir, &["-s", "b", "view"]));
    assert!(v.contains("textbox e1 \"User\"\n"), "b sees a's text: {v}");
    let v = ok(&pageon(&dir, &["sessions"]));
    assert!(v.contains("b\n  t1 \"Shop\""), "{v}");
    assert!(v.contains("default\n  t1 \"Shop\""), "{v}");

    // Close one; it is gone, the other stays.
    let v = ok(&pageon(&dir, &["close"]));
    assert!(v.contains("closed default"), "{v}");
    let o = pageon(&dir, &["click", "e1"]);
    assert_eq!(o.status.code(), Some(2));
    ok(&pageon(&dir, &["-s", "b", "observe"]));
    ok(&pageon(&dir, &["close", "--all"]));
    let left: Vec<_> = std::fs::read_dir(&dir.0)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".log"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    let _ = std::fs::remove_file(&page);
}

#[test]
fn browse_from_the_shell_caged() {
    browse(true);
}

#[test]
fn browse_from_the_shell_in_process() {
    browse(false);
}

#[test]
fn secrets_stay_out_and_close_writes_state_and_recording() {
    let dir = Dir::new("s");
    private(&dir.0);
    let (origin, seen) = serve();
    let state = dir.0.join("state.json");
    let rec = dir.0.join("run.rec.json");
    let secret = format!("shop@{origin}");
    let v = ok(&pageon(
        &dir,
        &[
            "open",
            &format!("{origin}/"),
            "--private-network",
            "allow",
            "--robots",
            "ignore",
            "--origin",
            &origin,
            "--secret",
            &secret,
            "--save-state",
            &state.display().to_string(),
            "--record",
            &rec.display().to_string(),
        ],
    ));
    assert!(v.contains("textbox e2 \"Password\""), "{v}");
    let mut replies = vec![v];
    replies.push(ok(&pageon(&dir, &["type", "e2", "secret://shop"])));
    replies.push(ok(&pageon(&dir, &["click", "e4"])));
    replies.push(ok(&pageon(&dir, &["view", "requests"])));
    // The page got the value; the agent did not.
    let got = seen.lock().map(|l| l.join("\n")).unwrap_or_default();
    assert!(got.contains(SECRET), "the server never got the secret");
    // Options given again to a running session are reported as ignored.
    let o = pageon(&dir, &["open", &format!("{origin}/"), "--no-js"]);
    assert!(err(&o).contains("are ignored"), "{}", err(&o));
    ok(&pageon(&dir, &["close"]));
    assert!(state.exists(), "no state written at close");
    assert!(rec.exists(), "no recording written at close");
    // The session's recording replays offline, step by step.
    let o = pageon(
        &dir,
        &["replay", &rec.display().to_string(), "--secret", &secret],
    );
    assert!(o.status.success(), "{}\n{}", out(&o), err(&o));
    for r in &replies {
        assert!(!r.contains(SECRET), "a reply holds the secret: {r}");
    }
    for entry in std::fs::read_dir(&dir.0).into_iter().flatten().flatten() {
        let text = std::fs::read(entry.path()).unwrap_or_default();
        assert!(
            !String::from_utf8_lossy(&text).contains(SECRET),
            "{} holds the secret",
            entry.path().display()
        );
    }
}

#[test]
fn idle_sessions_close_and_write_their_state() {
    let dir = Dir::new("i");
    let page = page_file(&dir);
    let state = dir.0.join("idle.json");
    ok(&pageon(
        &dir,
        &[
            "open",
            &page,
            "--allow-files",
            "--idle",
            "1s",
            "--save-state",
            &state.display().to_string(),
        ],
    ));
    let mut gone = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if !dir.0.join("default.sock").exists() && state.exists() {
            gone = true;
            break;
        }
    }
    assert!(gone, "the idle session is still running");
    let o = pageon(&dir, &["observe"]);
    assert_eq!(o.status.code(), Some(2));
    let _ = std::fs::remove_file(&page);
}

#[test]
#[allow(clippy::disallowed_methods)] // the stall is measured in wall time
fn a_stalled_client_does_not_hold_the_session() {
    let dir = Dir::new("t");
    let page = page_file(&dir);
    ok(&pageon(
        &dir,
        &["open", &page, "--allow-files", "--no-sandbox"],
    ));
    let _stalled = std::os::unix::net::UnixStream::connect(dir.0.join("default.sock"))
        .unwrap_or_else(|e| panic!("{e}"));
    let t = std::time::Instant::now();
    ok(&pageon(&dir, &["find", "report"]));
    assert!(
        t.elapsed() < std::time::Duration::from_secs(8),
        "{:?}",
        t.elapsed()
    );
    let _ = std::fs::remove_file(&page);
}

#[test]
fn dead_sessions_are_noticed() {
    let dir = Dir::new("d");
    let page = page_file(&dir);
    ok(&pageon(
        &dir,
        &["open", &page, "--allow-files", "--no-sandbox"],
    ));
    let log = std::fs::read_to_string(dir.0.join("default.log")).unwrap_or_default();
    let pid: i32 = log
        .split("pid ")
        .nth(1)
        .and_then(|s| s.split(')').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("no pid in {log}"));
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let o = pageon(&dir, &["observe"]);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let v = ok(&pageon(&dir, &["sessions"]));
    assert!(v.contains("no sessions"), "{v}");
    let v = ok(&pageon(
        &dir,
        &["open", &page, "--allow-files", "--no-sandbox"],
    ));
    assert!(v.contains("step 0"), "{v}");
    let _ = std::fs::remove_file(&page);
}

#[test]
fn private_directories_and_plain_names_only() {
    let dir = Dir::new("p");
    std::fs::create_dir_all(&dir.0).unwrap_or_else(|e| panic!("{e}"));
    std::fs::set_permissions(&dir.0, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("{e}"));
    let o = pageon(&dir, &["open", "https://example.com"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("is open to other users"), "{}", err(&o));
    let o = pageon(&dir, &["-s", "../x", "observe"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("is not a session name"), "{}", err(&o));
    // A fresh directory is created private.
    std::fs::remove_dir_all(&dir.0).unwrap_or_else(|e| panic!("{e}"));
    let page = page_file(&dir);
    ok(&pageon(
        &dir,
        &["open", &page, "--allow-files", "--no-sandbox"],
    ));
    let mode = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0)
    };
    assert_eq!(mode(&dir.0), 0o700);
    assert_eq!(mode(&dir.0.join("default.sock")), 0o600);
    assert_eq!(mode(&dir.0.join("default.args")), 0o600);
    let _ = std::fs::remove_file(&page);
}

#[test]
fn the_skill_names_real_commands() {
    let dir = Dir::new("k");
    let skill = ok(&pageon(&dir, &["skill"]));
    assert!(
        skill.starts_with("---\nname: pageon\ndescription: "),
        "{skill}"
    );
    let help = ok(&pageon(&dir, &["--help"]));
    // Every command in the skill's table exists.
    for line in skill.lines().filter(|l| l.starts_with("| `")) {
        let first = line.split('|').nth(1).unwrap_or_default();
        for cmd in first.split('`').skip(1).step_by(2) {
            let word = cmd.split_whitespace().next().unwrap_or_default();
            assert!(
                help.contains(&format!("  {word} ")),
                "{word} (from {cmd:?}) is not a command"
            );
        }
    }
    private(&dir.0);
    let target = dir.0.join("skills");
    let v = ok(&pageon(
        &dir,
        &["skill", "--install", &target.display().to_string()],
    ));
    assert!(v.contains("SKILL.md"), "{v}");
    assert_eq!(
        std::fs::read_to_string(target.join("pageon/SKILL.md")).unwrap_or_default(),
        skill
    );
}

#[test]
#[ignore = "timing: run with --release -- --ignored"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn commands_are_fast() {
    let dir = Dir::new("f");
    let page = page_file(&dir);
    ok(&pageon(&dir, &["open", &page, "--allow-files"]));
    let mut times = Vec::new();
    for _ in 0..20 {
        let t = std::time::Instant::now();
        ok(&pageon(&dir, &["find", "report"]));
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    let median = times[times.len() / 2];
    println!("find over a session: median {median:.1} ms (process start included)");
    assert!(median < 20.0, "{median:.1} ms");
    let _ = std::fs::remove_file(&page);
}
