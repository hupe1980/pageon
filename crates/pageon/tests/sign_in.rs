//! Sign-ins that renew: a local portal whose sessions expire on demand, an agent over MCP
//! (caged and in-process) and a CLI session.

use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

const SECRET: &str = "s3cret-portal-pw-91";

#[derive(Default)]
struct Portal {
    token: u32,
    logins: u32,
    cancels: u32,
    /// Sign-ins never hold: every page sends the agent back to the sign-in page.
    stuck: bool,
}

/// The portal: `/login` (a form posting to itself), `/orders` (needs the session cookie),
/// `/orders/cancel` (a POST; 401 without the session).
fn serve() -> (String, Arc<Mutex<Portal>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let origin = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().map(|a| a.port()).unwrap_or(0)
    );
    let portal = Arc::new(Mutex::new(Portal {
        token: 1,
        ..Portal::default()
    }));
    let state = portal.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut s = stream;
            let mut buf = vec![0u8; 16384];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let mut p = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let signed_in = !p.stuck && req.contains(&format!("sid=t{}", p.token));
            let line = req.lines().next().unwrap_or_default().to_string();
            let (status, head, body): (&str, String, String) = if line.starts_with("GET /login") {
                ("200 OK", String::new(), "<title>Sign in</title><form method=post action=/login><label>User <input name=user></label><label>Password <input type=password name=pw></label><button>Sign in</button></form>".into())
            } else if line.starts_with("POST /login") {
                p.logins += 1;
                if req.contains(&format!("pw={SECRET}")) {
                    (
                        "302 Found",
                        format!(
                            "Set-Cookie: sid=t{}; Path=/\r\nLocation: /orders\r\n",
                            p.token
                        ),
                        String::new(),
                    )
                } else {
                    ("200 OK", String::new(), "<p>Wrong password".into())
                }
            } else if line.starts_with("GET /orders") {
                if signed_in {
                    ("200 OK", String::new(), "<title>Orders</title><h1>Orders</h1><form method=post action=/orders/cancel><button>Cancel order</button></form>".into())
                } else {
                    (
                        "302 Found",
                        "Location: /login?next=/orders\r\n".into(),
                        String::new(),
                    )
                }
            } else if line.starts_with("POST /orders/cancel") {
                p.cancels += 1;
                if signed_in {
                    (
                        "200 OK",
                        String::new(),
                        "<title>Cancelled</title><p>Cancelled".into(),
                    )
                } else {
                    (
                        "401 Unauthorized",
                        String::new(),
                        "<title>Signed out</title><p>Please sign in".into(),
                    )
                }
            } else {
                ("404 Not Found", String::new(), String::new())
            };
            drop(p);
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\n{head}content-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (origin, portal)
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(origin: &str, caged: bool, extra: &[&str]) -> Self {
        let secret = format!("shop@{origin}");
        let mut args = vec![
            "mcp",
            "--origin",
            origin,
            "--secret",
            &secret,
            "--private-network",
            "allow",
            "--robots",
            "ignore",
        ];
        args.extend_from_slice(extra);
        if !caged {
            args.push("--no-sandbox");
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_pageon"))
            .args(&args)
            .env("PAGEON_SECRET_SHOP", SECRET)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("{e}"));
        let input = child.stdin.take().unwrap_or_else(|| panic!("stdin"));
        let output = BufReader::new(child.stdout.take().unwrap_or_else(|| panic!("stdout")));
        Self {
            child,
            input,
            output,
            next: 1,
        }
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
            "name": tool, "arguments": args,
            "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                      "io.modelcontextprotocol/clientCapabilities": {}}}});
        let _ = writeln!(self.input, "{msg}");
        let _ = self.input.flush();
        let mut line = String::new();
        loop {
            line.clear();
            if self.output.read_line(&mut line).unwrap_or(0) == 0 {
                panic!("pageon mcp ended");
            }
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && v["id"] == id
            {
                return v["result"].clone();
            }
        }
    }
}

fn text(r: &Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Records the sign-in as a user would: sign in over MCP, save the session as a workflow.
fn record_sign_in(origin: &str, dir: &std::path::Path) -> std::path::PathBuf {
    let mut m = Mcp::start(origin, false, &[]);
    m.call("browser_open", json!({"url": format!("{origin}/login")}));
    m.call("browser_type", json!({"ref": "e1", "text": "ada"}));
    m.call(
        "browser_type",
        json!({"ref": "e2", "text": "secret://shop"}),
    );
    let r = m.call("browser_click", json!({"ref": "e3"}));
    assert!(text(&r).contains("Orders"), "{}", text(&r));
    let wf = m.call("browser_workflow_save", json!({}));
    let wf = wf["structuredContent"]["workflow"].clone();
    let wf = if wf.is_null() {
        serde_json::from_str::<Value>(&text(&m.call("browser_workflow_save", json!({}))))
            .unwrap_or(Value::Null)
    } else {
        wf
    };
    assert_eq!(wf["start"], format!("{origin}/login"), "{wf}");
    assert!(!wf.to_string().contains(SECRET));
    let path = dir.join("login.flow.json");
    std::fs::write(&path, wf.to_string()).unwrap_or_else(|e| panic!("{e}"));
    path
}

fn renew(caged: bool) {
    let (origin, portal) = serve();
    let dir = std::env::temp_dir().join(format!("pageon-signin-{}-{caged}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let flow = record_sign_in(&origin, &dir);
    let flow_arg = flow.display().to_string();
    let before = portal.lock().map(|p| p.logins).unwrap_or(0);
    // A new session: not signed in, so opening the orders lands on the sign-in page.
    let mut m = Mcp::start(&origin, caged, &["--sign-in", &flow_arg]);
    let r = m.call("browser_open", json!({"url": format!("{origin}/orders")}));
    let t = text(&r);
    assert!(
        t.starts_with("signed out of 127.0.0.1:"),
        "no renewal line: {t}"
    );
    assert!(t.contains("(redirected to the sign-in page); signed in again with login.flow.json (3 steps), reopened /orders"), "{t}");
    assert!(t.contains("# \"Orders\""), "{t}");
    assert_eq!(
        r["structuredContent"]["signed_in"]["workflow"],
        "login.flow.json"
    );
    assert_eq!(portal.lock().map(|p| p.logins).unwrap_or(0), before + 1);

    // The site expires the session; a POST meets 401: signed in again, the POST not resent.
    portal.lock().map(|mut p| p.token += 1).unwrap_or_default();
    let cancels = portal.lock().map(|p| p.cancels).unwrap_or(0);
    let r = m.call("browser_click", json!({"ref": "e1"}));
    let t = text(&r);
    assert!(t.contains("signed out of 127.0.0.1:"), "{t}");
    assert!(t.contains("(401); signed in again"), "{t}");
    assert!(
        t.contains("the POST was not sent again: do it again"),
        "{t}"
    );
    assert_eq!(
        portal.lock().map(|p| p.cancels).unwrap_or(0),
        cancels + 1,
        "the POST was sent again"
    );
    assert_eq!(r["structuredContent"]["signed_in"]["not_sent"], "POST");

    // Opening the sign-in page itself is not a sign-out.
    let r = m.call("browser_open", json!({"url": format!("{origin}/login")}));
    assert!(!text(&r).starts_with("signed out"), "{}", text(&r));
    // A signed-in page resets the count of renewals in a row.
    let r = m.call("browser_open", json!({"url": format!("{origin}/orders")}));
    assert!(text(&r).contains("Orders"), "{}", text(&r));

    // A site where sign-ins never hold: at most three renewals in a row, then pageon stops.
    portal
        .lock()
        .map(|mut p| p.stuck = true)
        .unwrap_or_default();
    let logins = portal.lock().map(|p| p.logins).unwrap_or(0);
    let mut last = String::new();
    for _ in 0..4 {
        last = text(&m.call("browser_open", json!({"url": format!("{origin}/orders")})));
    }
    assert_eq!(
        portal.lock().map(|p| p.logins).unwrap_or(0),
        logins + 3,
        "renewals in a row were not capped"
    );
    assert!(
        last.contains("not signing in again: 3 sign-ins in a row did not hold"),
        "{last}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn expired_sign_ins_renew_caged() {
    renew(true);
}

#[test]
fn expired_sign_ins_renew_in_process() {
    renew(false);
}

#[cfg(unix)]
#[test]
fn cli_sessions_renew_too() {
    let (origin, portal) = serve();
    let dir = std::env::temp_dir().join(format!("pgsi-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let flow = record_sign_in(&origin, &dir);
    let sessions = dir.join("s");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_pageon"))
            .args(args)
            .env("PAGEON_SESSIONS", &sessions)
            .env("PAGEON_SECRET_SHOP", SECRET)
            .env_remove("PAGEON_SESSION")
            .output()
            .unwrap_or_else(|e| panic!("{e}"))
    };
    let secret = format!("shop@{origin}");
    let o = run(&[
        "open",
        &format!("{origin}/orders"),
        "--origin",
        &origin,
        "--secret",
        &secret,
        "--private-network",
        "allow",
        "--robots",
        "ignore",
        "--no-sandbox",
        "--sign-in",
        &flow.display().to_string(),
    ]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(
        out.contains("signed in again with login.flow.json"),
        "{out}"
    );
    assert!(out.contains("# \"Orders\""), "{out}");
    assert!(!out.contains(SECRET));
    assert_eq!(portal.lock().map(|p| p.logins).unwrap_or(0), 2);
    run(&["close"]);
    let _ = std::fs::remove_dir_all(&dir);
}
