//! Expectations: checked after actions over MCP (caged and in-process), the CLI, `fetch` and
//! workflow runs, against a local shop whose orders succeed or fail on demand.

use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

#[derive(Default)]
struct Shop {
    fail: bool,
    orders: u32,
}

const HOME: &str = r#"<title>Shop</title><h1>Shop</h1><p hidden>Order confirmed</p>
<form method=post action="/orders?token=abcdef123456"><button>Place order</button></form>
<a href="/report.pdf">Report</a>"#;

fn serve() -> (String, Arc<Mutex<Shop>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let origin = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().map(|a| a.port()).unwrap_or(0)
    );
    let shop = Arc::new(Mutex::new(Shop::default()));
    let state = shop.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut s = stream;
            let mut buf = vec![0u8; 16384];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let line = req.lines().next().unwrap_or_default().to_string();
            let mut shop = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (status, head, body): (&str, &str, String) = if line.starts_with("GET / ") {
                ("200 OK", "content-type: text/html\r\n", HOME.into())
            } else if line.starts_with("POST /orders") {
                shop.orders += 1;
                if shop.fail {
                    ("500 Internal Server Error", "content-type: text/html\r\n", "<title>Error</title><h1>Payment declined</h1><p hidden>Order confirmed</p>".into())
                } else {
                    ("302 Found", "location: /done\r\n", String::new())
                }
            } else if line.starts_with("GET /done") {
                ("200 OK", "content-type: text/html\r\n", "<title>Thanks</title><h1>Thanks</h1><p>Order confirmed</p><button>Track</button>".into())
            } else if line.starts_with("GET /report.pdf") {
                (
                    "200 OK",
                    "content-type: application/pdf\r\ncontent-disposition: attachment; filename=\"report.pdf\"\r\n",
                    "%PDF-1.4 not really".into(),
                )
            } else {
                ("404 Not Found", "", String::new())
            };
            drop(shop);
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\n{head}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (origin, shop)
}

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(origin: &str, caged: bool) -> Self {
        let mut args = vec![
            "mcp",
            "--origin",
            origin,
            "--private-network",
            "allow",
            "--robots",
            "ignore",
        ];
        if !caged {
            args.push("--no-sandbox");
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_pageon"))
            .args(&args)
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
            assert!(
                self.output.read_line(&mut line).unwrap_or(0) > 0,
                "pageon mcp ended"
            );
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && v["id"] == id
            {
                return v["result"].clone();
            }
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(r: &Value) -> String {
    r["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn set_fail(shop: &Arc<Mutex<Shop>>, fail: bool) {
    if let Ok(mut s) = shop.lock() {
        s.fail = fail;
    }
}

fn orders(shop: &Arc<Mutex<Shop>>) -> u32 {
    shop.lock().map(|s| s.orders).unwrap_or(0)
}

fn verify(caged: bool) {
    let (origin, shop) = serve();
    let mut m = Mcp::start(&origin, caged);
    let home = m.call("browser_open", json!({"url": format!("{origin}/")}));
    assert!(
        text(&home).contains("button e1 \"Place order\""),
        "{}",
        text(&home)
    );

    // Everything as expected: each line with its evidence, no error; the token redacted.
    let r = m.call(
        "browser_click",
        json!({"ref": "e1", "expect": [
            "url /done",
            "see \"Order confirmed\"",
            "see button \"Track\"",
            "no \"Payment declined\"",
            "request POST /orders 2xx"
        ]}),
    );
    let t = text(&r);
    assert_ne!(r["isError"], true, "{t}");
    assert!(t.starts_with("✓ url /done: at http://127.0.0.1:"), "{t}");
    assert!(t.contains("✓ see button \"Track\": button e"), "{t}");
    assert!(
        t.contains("✓ request POST /orders 2xx: POST http://127.0.0.1:"),
        "{t}"
    );
    assert!(
        t.contains("token=[redacted]") && !t.contains("abcdef123456"),
        "{t}"
    );
    assert_eq!(
        r["structuredContent"]["expectations"]
            .as_array()
            .map(Vec::len),
        Some(5)
    );
    // Saved into the workflow, checked on every run.
    let wf = m.call("browser_workflow_save", json!({}));
    let wf = wf["structuredContent"]["workflow"].clone();
    assert_eq!(
        wf["steps"][0]["check"][4], "request POST /orders 2xx",
        "{wf}"
    );

    // The order fails: the hidden "Order confirmed" does not count, the 500 is the evidence.
    set_fail(&shop, true);
    m.call("browser_open", json!({"url": format!("{origin}/")}));
    let r = m.call(
        "browser_click",
        json!({"ref": "e1", "expect": ["request POST /orders 2xx", "see \"Order confirmed\""]}),
    );
    let t = text(&r);
    assert_eq!(r["isError"], true, "{t}");
    assert!(
        t.contains("✗ request POST /orders 2xx: POST http://127.0.0.1:"),
        "{t}"
    );
    assert!(t.contains("→ 500"), "{t}");
    assert!(
        t.contains("✗ see \"Order confirmed\": not on the page (it says \"Payment declined\")"),
        "{t}"
    );

    // Downloads.
    m.call("browser_open", json!({"url": format!("{origin}/")}));
    let r = m.call(
        "browser_click",
        json!({"ref": "e2", "expect": ["download *.pdf"]}),
    );
    assert!(
        text(&r).starts_with("✓ download *.pdf: downloaded \"report.pdf\""),
        "{}",
        text(&r)
    );

    // A malformed expectation is refused before acting.
    let before = orders(&shop);
    let r = m.call(
        "browser_click",
        json!({"ref": "e1", "expect": ["see Order"]}),
    );
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("is not an expectation"), "{}", text(&r));
    assert_eq!(orders(&shop), before, "the action ran");

    // The saved workflow, replayed while orders fail, stops at the failed check.
    let r = m.call("browser_workflow_run", json!({"workflow": wf}));
    let t = text(&r);
    assert!(t.contains("drifted"), "{t}");
    assert!(
        t.contains("check failed: ✗ url /done: at http://127.0.0.1:"),
        "{t}"
    );
    set_fail(&shop, false);
    let r = m.call("browser_workflow_run", json!({"workflow": wf}));
    assert!(text(&r).contains("done"), "{}", text(&r));
}

#[test]
fn expectations_caged() {
    verify(true);
}

#[test]
fn expectations_in_process() {
    verify(false);
}

#[cfg(unix)]
#[test]
fn the_cli_and_fetch_exit_8_on_a_failed_expectation() {
    let (origin, shop) = serve();
    let pageon = |args: &[&str], sessions: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_pageon"))
            .args(args)
            .env("PAGEON_SESSIONS", sessions)
            .env_remove("PAGEON_SESSION")
            .output()
            .unwrap_or_else(|e| panic!("{e}"))
    };
    let dir = std::env::temp_dir().join(format!("pgv8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = format!("{origin}/");
    let net = [
        "--private-network",
        "allow",
        "--robots",
        "ignore",
        "--no-sandbox",
    ];
    let fetch = |expect: &str| {
        let mut args = vec![
            "fetch",
            home.as_str(),
            "--act",
            "click e1",
            "--expect",
            expect,
        ];
        args.extend_from_slice(&net);
        pageon(&args, &dir)
    };
    let o = fetch("see \"Thanks\"");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(String::from_utf8_lossy(&o.stderr).contains("✓ see \"Thanks\""));
    set_fail(&shop, true);
    let o = fetch("see \"Thanks\"");
    assert_eq!(
        o.status.code(),
        Some(8),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );

    let mut open = vec!["open", home.as_str()];
    open.extend_from_slice(&net);
    assert!(pageon(&open, &dir).status.success());
    let o = pageon(&["click", "e1", "--expect", "url /done"], &dir);
    assert_eq!(
        o.status.code(),
        Some(8),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("✗ url /done: at http://127.0.0.1:"));
    pageon(&["close"], &dir);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "timing: run with --release -- --ignored"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn checks_are_cheap() {
    let (origin, _shop) = serve();
    let mut m = Mcp::start(&origin, false);
    m.call("browser_open", json!({"url": format!("{origin}/")}));
    let time = |m: &mut Mcp, expect: Value| {
        let t = std::time::Instant::now();
        for _ in 0..50 {
            m.call("browser_hover", json!({"ref": "e1", "expect": expect}));
        }
        t.elapsed().as_secs_f64() * 1000.0 / 50.0
    };
    let plain = time(&mut m, json!([]));
    let checked = time(
        &mut m,
        json!(["url /", "see \"Shop\"", "see button \"Place order\""]),
    );
    println!("hover {plain:.2} ms, with three checks {checked:.2} ms");
    assert!(
        checked - plain < 2.0,
        "three checks add {:.2} ms",
        checked - plain
    );
}
