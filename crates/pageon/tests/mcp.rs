//! `pageon mcp`: protocol transcripts against the real binary, over a recording.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use common::*;
use serde_json::{Value, json};

/// A running `pageon mcp`.
struct Server {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: i64,
}

impl Server {
    fn start(rec: &std::path::Path, extra: &[&str], env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pageon"));
        cmd.arg("mcp")
            .arg("--replay")
            .arg(rec)
            .args(extra)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn().unwrap_or_else(|e| panic!("{e}"));
        let input = child.stdin.take().unwrap_or_else(|| panic!("stdin"));
        let output = BufReader::new(child.stdout.take().unwrap_or_else(|| panic!("stdout")));
        Self {
            child,
            input,
            output,
            next: 1,
        }
    }

    fn send(&mut self, msg: &Value) {
        let mut line = serde_json::to_vec(msg).unwrap_or_default();
        line.push(b'\n');
        self.input
            .write_all(&line)
            .unwrap_or_else(|e| panic!("{e}"));
        self.input.flush().unwrap_or_else(|e| panic!("{e}"));
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        self.output
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("{e}"));
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let r = self.recv();
        assert_eq!(r["id"], id, "{r}");
        r
    }

    fn init(&mut self, version: &str) -> Value {
        let r = self.request(
            "initialize",
            json!({"protocolVersion": version, "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        r
    }

    fn call(&mut self, tool: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}));
        r["result"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn shop() -> std::path::PathBuf {
    file(
        "mcp-shop",
        &site(vec![
            html(
                "https://shop.test/",
                "<h1>Shop</h1><a href=/login>Sign in</a><button disabled>Buy</button>\
                 <select aria-label=Size><option>S<option>M</select>",
            ),
            html(
                "https://shop.test/login",
                "<form method=post action=/session><input name=user aria-label=User>\
                 <input name=pw type=password aria-label=Password><button>Sign in</button></form>",
            ),
            ex(
                "POST",
                "https://shop.test/session",
                200,
                &[("content-type", "text/html")],
                "<h1>Welcome</h1><p>hello ada</p>",
            ),
            // `back` loads the login page again.
            html("https://shop.test/login", "<h1>Login again</h1>"),
        ]),
    )
}

#[test]
fn handshake_and_tools() {
    let rec = shop();
    for (asked, answered) in [
        ("2025-11-25", "2025-11-25"),
        ("2025-06-18", "2025-06-18"),
        ("2025-03-26", "2025-03-26"),
        ("2024-11-05", "2024-11-05"),
        ("1999-01-01", "2025-11-25"),
    ] {
        let mut s = Server::start(&rec, &[], &[]);
        let r = s.init(asked);
        assert_eq!(r["result"]["protocolVersion"], answered);
        assert_eq!(r["result"]["serverInfo"]["name"], "pageon");
        assert!(r["result"]["capabilities"]["tools"].is_object());
    }
    let mut s = Server::start(&rec, &[], &[]);
    s.init("2025-06-18");
    assert_eq!(s.request("ping", json!({}))["result"], json!({}));
    let tools = s.request("tools/list", json!({}))["result"]["tools"].clone();
    let names: Vec<&str> = tools
        .as_array()
        .map(|t| t.iter().filter_map(|x| x["name"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(
        names,
        [
            "browser_open",
            "browser_observe",
            "browser_click",
            "browser_type",
            "browser_upload",
            "browser_press",
            "browser_select",
            "browser_hover",
            "browser_drag",
            "browser_scroll",
            "browser_wait",
            "browser_back",
            "browser_forward",
            "browser_tabs",
            "browser_screenshot",
            "browser_reset",
            "browser_lookahead",
            "browser_call_tool",
            "browser_workflow_run",
            "browser_workflow_save",
            "browser_takeover",
            "browser_view"
        ]
    );
    for t in tools.as_array().into_iter().flatten() {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        assert!(t["annotations"]["readOnlyHint"].is_boolean(), "{t}");
    }
    // JSON-RPC errors; notifications get no answer; the server keeps going.
    s.send(&json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {}}));
    s.input
        .write_all(b"{not json\n")
        .unwrap_or_else(|e| panic!("{e}"));
    let r = s.recv();
    assert_eq!(r["error"]["code"], -32700, "{r}");
    assert_eq!(
        s.request("resources/list", json!({}))["error"]["code"],
        -32601
    );
    assert_eq!(
        s.request(
            "tools/call",
            json!({"name": "browser_fly", "arguments": {}})
        )["error"]["code"],
        -32602
    );
    assert_eq!(
        s.request(
            "tools/call",
            json!({"name": "browser_click", "arguments": {}})
        )["error"]["code"],
        -32602
    );
    let r = s.call("browser_click", json!({"ref": "e1"}));
    assert_eq!(r["isError"], true);
    assert_eq!(r["structuredContent"]["error"]["kind"], "no_page");
}

#[test]
fn browsing_through_mcp() {
    let rec = shop();
    for sandbox in [true, false] {
        let extra: &[&str] = if sandbox { &[] } else { &["--no-sandbox"] };
        let mut s = Server::start(
            &rec,
            &[extra, &["--secret", "pw@https://shop.test"]].concat(),
            &[("PAGEON_SECRET_PW", "very-secret-pw")],
        );
        s.init("2025-06-18");
        let mut seen = Vec::new();
        let r = s.call("browser_open", json!({"url": "https://shop.test/"}));
        let t = text(&r);
        assert!(t.starts_with("ok open https://shop.test/"), "{t}");
        assert!(t.contains("link e1 \"Sign in\""), "{t}");
        assert_eq!(
            r["structuredContent"]["outcome"]["navigated"],
            "https://shop.test/"
        );
        seen.push(r.to_string());
        // A tool error with guidance.
        let r = s.call("browser_click", json!({"ref": find(&t, "button", "Buy")}));
        assert_eq!(r["isError"], true, "{r}");
        assert_eq!(r["structuredContent"]["error"]["kind"], "not_actionable");
        assert!(text(&r).contains("wait until it is enabled"), "{r}");
        let r = s.call(
            "browser_select",
            json!({"ref": find(&t, "combobox", "Size"), "values": ["M"]}),
        );
        assert!(text(&r).contains("ok select e3 \"M\""), "{r}");
        assert!(text(&r).contains("=\"M\""), "{r}");
        let r = s.call("browser_view", json!({"kind": "text"}));
        assert!(text(&r).contains("Shop"), "{r}");
        let r = s.call("browser_click", json!({"ref": "e1"}));
        let t = text(&r);
        assert!(
            t.contains("navigated to \"https://shop.test/login\""),
            "{t}"
        );
        assert_eq!(
            r["structuredContent"]["outcome"]["navigated"],
            "https://shop.test/login"
        );
        let r = s.call("browser_observe", json!({"full": true}));
        let t = text(&r);
        let user = find(&t, "textbox", "User");
        let pw = find(&t, "textbox", "Password");
        let r = s.call("browser_hover", json!({"ref": user}));
        assert!(text(&r).contains("ok hover"), "{r}");
        s.call("browser_type", json!({"ref": user, "text": "ada"}));
        let r = s.call(
            "browser_type",
            json!({"ref": pw, "text": "secret://pw", "submit": true}),
        );
        let t = text(&r);
        assert!(
            t.contains("navigated to \"https://shop.test/session\""),
            "{t}"
        );
        assert!(t.contains("\"Welcome\""), "{t}");
        seen.push(r.to_string());
        let r = s.call("browser_back", json!({}));
        assert!(
            text(&r).contains("navigated to \"https://shop.test/login\""),
            "{r}"
        );
        let r = s.call("browser_press", json!({"key": "Tab"}));
        assert!(text(&r).contains("ok press Tab"), "{r}");
        // Forward re-submits the POST, which this recording has once: a typed navigation error.
        let r = s.call("browser_forward", json!({}));
        assert!(
            r["isError"] == true || text(&r).contains("ok forward"),
            "{r}"
        );
        let r = s.call("browser_wait", json!({"ms": 100}));
        assert!(text(&r).contains("ok wait 100"), "{r}");
        seen.push(
            s.call("browser_view", json!({"kind": "requests"}))
                .to_string(),
        );
        for x in &seen {
            assert!(
                !x.contains("very-secret-pw"),
                "secret leaked (sandbox {sandbox}): {x}"
            );
        }
    }
}

#[test]
fn miniwob_through_mcp() {
    let Some(rec) = miniwob_log() else {
        println!("skipped: run `uv run pageon-bench setup` in bench/ to fetch MiniWoB++");
        return;
    };
    let path = file("mcp-miniwob", &rec);
    for task in ["click-button", "enter-text", "login-user"] {
        let mut s = Server::start(&path, &[], &[]);
        s.init("2025-06-18");
        let r = s.call(
            "browser_open",
            json!({"url": format!("{MINIWOB_ORIGIN}miniwob/{task}.html")}),
        );
        let obs = text(&r);
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
        match task {
            "click-button" => {
                s.call(
                    "browser_click",
                    json!({"ref": find(&obs, "button", &args[0])}),
                );
            }
            "enter-text" => {
                s.call(
                    "browser_type",
                    json!({"ref": refs(&obs, "textbox")[0].0, "text": args[0]}),
                );
                s.call(
                    "browser_click",
                    json!({"ref": find(&obs, "button", "Submit")}),
                );
            }
            _ => {
                let f = refs(&obs, "textbox");
                s.call("browser_type", json!({"ref": f[0].0, "text": args[0]}));
                s.call("browser_type", json!({"ref": f[1].0, "text": args[1]}));
                s.call(
                    "browser_click",
                    json!({"ref": find(&obs, "button", "Login")}),
                );
            }
        }
        let page = text(&s.call("browser_view", json!({"kind": "text"})));
        let reward = page
            .split("Last reward:")
            .nth(1)
            .and_then(|r| r.split_whitespace().next())
            .and_then(|r| r.parse::<f64>().ok())
            .unwrap_or(0.0);
        assert!(reward > 0.0, "{task}: no reward\n{page}");
    }
}

#[test]
#[ignore = "timing; run in release"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn round_trip_is_fast() {
    let Some(rec) = miniwob_log() else { return };
    let path = file("mcp-speed", &rec);
    let mut s = Server::start(&path, &[], &[]);
    s.init("2025-06-18");
    let obs = text(&s.call(
        "browser_open",
        json!({"url": format!("{MINIWOB_ORIGIN}miniwob/click-button.html")}),
    ));
    let r = refs(&obs, "textbox")[0].0.clone();
    let t = std::time::Instant::now();
    for _ in 0..10 {
        s.call("browser_click", json!({"ref": r}));
    }
    let ms = t.elapsed().as_secs_f64() * 100.0;
    println!("mcp click round trip: {ms:.1} ms");
    assert!(ms < 15.0, "a caged MCP round trip");
    // A caged screenshot with marks, through MCP (image tokens included).
    s.call("browser_screenshot", json!({"marks": true}));
    let t = std::time::Instant::now();
    for _ in 0..10 {
        s.call("browser_screenshot", json!({"marks": true}));
    }
    let ms = t.elapsed().as_secs_f64() * 100.0;
    println!("mcp screenshot (marks) round trip: {ms:.1} ms");
    assert!(ms <= 40.0, "a caged screenshot with marks");
}

/// `browser_screenshot` returns MCP image content, with marks and crops.
#[test]
fn screenshots_through_mcp() {
    use base64::Engine as _;
    let rec = file(
        "mcp-paint",
        &site(vec![
            html(
                "https://pics.test/",
                r#"<h1>Pics</h1><img src="/red.png" alt="Red"><button>Buy</button><input aria-label=Search>"#,
            ),
            bytes("https://pics.test/red.png", "image/png", &red_png()),
        ]),
    );
    for sandbox in [true, false] {
        let extra: &[&str] = if sandbox { &[] } else { &["--no-sandbox"] };
        let mut s = Server::start(&rec, extra, &[]);
        s.init("2025-06-18");
        let r = s.call("browser_screenshot", json!({}));
        assert_eq!(r["structuredContent"]["error"]["kind"], "no_page", "{r}");
        s.call("browser_open", json!({"url": "https://pics.test/"}));
        let r = s.call("browser_screenshot", json!({"marks": true}));
        assert_eq!(r["content"][0]["type"], "image", "{r}");
        assert_eq!(r["content"][0]["mimeType"], "image/png");
        let png = base64::engine::general_purpose::STANDARD
            .decode(r["content"][0]["data"].as_str().unwrap_or_default())
            .unwrap_or_default();
        assert!(png.starts_with(b"\x89PNG"), "a PNG");
        assert_eq!(r["structuredContent"]["width"], 1280);
        let legend = r["structuredContent"]["legend"]
            .as_str()
            .unwrap_or_default();
        assert!(legend.contains(r#"button e1 "Buy""#), "{legend}");
        let t = r["content"][1]["text"].as_str().unwrap_or_default();
        assert!(t.contains("1 images loaded"), "{t}");
        let r = s.call("browser_screenshot", json!({"ref": "e1", "format": "jpeg"}));
        assert_eq!(r["content"][0]["mimeType"], "image/jpeg", "{r}");
        assert_eq!(r["structuredContent"]["provenance"]["tier"], "first-party");
        let r = s.call("browser_screenshot", json!({"ref": "e99"}));
        assert_eq!(
            r["structuredContent"]["error"]["kind"], "unknown_ref",
            "{r}"
        );
    }
}

fn red_png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(40, 30, image::Rgba([255, 0, 0, 255]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap_or_default();
    out
}

/// Steps are numbered, and `browser_reset` goes back exactly, sending nothing again.
#[test]
fn reset_goes_back_without_sending_again() {
    let rec = file(
        "mcp-reset",
        &site(vec![
            html(
                "https://shop.test/login",
                r#"<form action=/session method=post><input name=user aria-label=User><button>Sign in</button></form>"#,
            ),
            ex(
                "POST",
                "https://shop.test/session",
                200,
                &[("content-type", "text/html")],
                "<h1>Welcome</h1>",
            ),
        ]),
    );
    for sandbox in [true, false] {
        let extra: &[&str] = if sandbox { &[] } else { &["--no-sandbox"] };
        let mut s = Server::start(&rec, extra, &[]);
        s.init("2025-06-18");
        let r = s.call("browser_reset", json!({}));
        assert_eq!(r["structuredContent"]["error"]["kind"], "no_page", "{r}");
        let r = s.call("browser_open", json!({"url": "https://shop.test/login"}));
        assert_eq!(r["structuredContent"]["step"], 0, "{r}");
        assert!(text(&r).ends_with("step 0"), "{}", text(&r));
        let user = find(&text(&r), "textbox", "User");
        let r = s.call("browser_type", json!({"ref": user, "text": "ada"}));
        assert_eq!(r["structuredContent"]["step"], 1);
        let full = text(&s.call("browser_observe", json!({"full": true})));
        let r = s.call(
            "browser_click",
            json!({"ref": find(&full, "button", "Sign in")}),
        );
        assert_eq!(r["structuredContent"]["step"], 2);
        assert!(text(&r).contains("Welcome"), "{r}");
        // Re-run up to the POST: served from the recording (the replayed site has a single
        // POST; sending it again would fail).
        let r = s.call("browser_reset", json!({"step": 2}));
        assert!(r["isError"].is_null(), "the POST was sent again: {r}");
        assert!(text(&r).contains("Welcome"), "{r}");
        // Undo the POST: back on the form with "ada" typed, same refs.
        let r = s.call("browser_reset", json!({}));
        let t = text(&r);
        assert!(
            t.starts_with(r#"ok reset to step 1 (type e1 "ada")"#),
            "{t}"
        );
        assert!(t.contains(r#"textbox e1 "User" ="ada""#), "{t}");
        let r = s.call("browser_reset", json!({"step": 0}));
        assert!(text(&r).contains("ok reset to step 0"), "{r}");
        let r = s.call("browser_reset", json!({"step": 9}));
        assert_eq!(r["structuredContent"]["error"]["kind"], "bad_step", "{r}");
        assert!(text(&r).contains("from 0 to 0"), "{r}");
    }
}

#[test]
fn refusals_reach_the_agent() {
    let rec = file(
        "mcp-refusal",
        &site(vec![
            html("https://shop.test/", "<a href=/cf>Deals</a>"),
            page(
                "https://shop.test/cf",
                403,
                &[("content-type", "text/html"), ("cf-mitigated", "challenge")],
                "<p>Checking your browser",
            ),
        ]),
    );
    let mut s = Server::start(&rec, &[], &[]);
    s.init("2025-06-18");
    let r = s.call("browser_open", json!({"url": "https://shop.test/"}));
    assert!(r["structuredContent"].get("refusal").is_none(), "{r}");
    let deals = find(&text(&r), "link", "Deals");
    let r = s.call("browser_click", json!({"ref": deals}));
    assert_eq!(
        r["structuredContent"]["refusal"]["kind"], "challenge",
        "{r}"
    );
    assert_eq!(
        r["structuredContent"]["refusal"]["vendor"], "cloudflare",
        "{r}"
    );
    assert!(
        text(&r).contains("refused: challenge by cloudflare"),
        "{}",
        text(&r)
    );
    assert!(text(&r).contains("browser_takeover"), "{}", text(&r));
}

#[test]
fn lookahead_shows_without_doing() {
    // Lookahead: a fork tries the actions; the page is unchanged; writes are held.
    let rec = file(
        "mcp-lookahead",
        &site(vec![
            html(
                "https://shop.test/",
                "<p id=n>0</p><button onclick=\"n.textContent = 'clicked'\">Try</button>\
                 <form method=post action=/orders><button>Pay now</button></form>",
            ),
            ex(
                "POST",
                "https://shop.test/orders",
                200,
                &[("content-type", "text/html")],
                "<h1>Paid</h1>",
            ),
        ]),
    );
    let mut s = Server::start(&rec, &[], &[]);
    s.init("2025-06-18");
    let r = s.call("browser_open", json!({"url": "https://shop.test/"}));
    let page = text(&r);
    let (tryit, pay) = (
        find(&page, "button", "Try"),
        find(&page, "button", "Pay now"),
    );
    let r = s.call(
        "browser_lookahead",
        json!({"actions": [format!("click {tryit}")]}),
    );
    assert!(
        text(&r).starts_with("lookahead (the page is unchanged; writes were held"),
        "{}",
        text(&r)
    );
    assert!(text(&r).contains("clicked"), "{}", text(&r));
    let r = s.call("browser_observe", json!({}));
    assert!(
        text(&r).contains("no change"),
        "the page moved: {}",
        text(&r)
    );
    let r = s.call(
        "browser_lookahead",
        json!({"actions": [format!("click {pay}")]}),
    );
    assert_eq!(r["structuredContent"]["pending"]["method"], "POST", "{r}");
    assert!(
        text(&r).contains("pending: POST https://shop.test/orders"),
        "{}",
        text(&r)
    );
    let r = s.call("browser_lookahead", json!({"actions": ["fly e1"]}));
    assert_eq!(
        r["structuredContent"]["error"]["kind"], "invalid_action",
        "{r}"
    );
    drop(s);
    let mut s = Server::start(&rec, &["--no-sandbox"], &[]);
    s.init("2025-06-18");
    s.call("browser_open", json!({"url": "https://shop.test/"}));
    let r = s.call("browser_lookahead", json!({"actions": []}));
    assert_eq!(
        r["structuredContent"]["error"]["kind"], "unsupported",
        "{r}"
    );
    assert!(text(&r).contains("browser_reset"), "{}", text(&r));
}

fn workflow_site(account: &str) -> std::path::PathBuf {
    // Twice over: a replay serves each response once, and the test runs twice.
    let pages = || {
        vec![
            html(
                "https://shop.test/login",
                "<form method=post action=/session><label>User <input name=user></label><button>Sign in</button></form>",
            ),
            ex(
                "POST",
                "https://shop.test/session",
                303,
                &[("location", "/account")],
                "",
            ),
            html("https://shop.test/account", account),
            html("https://shop.test/report", "<h1>Sales</h1>"),
            html("https://shop.test/archive", "<h1>Archive</h1>"),
        ]
    };
    let mut all = pages();
    all.extend(pages());
    file(&format!("mcp-workflow-{}", account.len()), &site(all))
}

#[test]
fn workflows_run_drift_and_are_repaired() {
    // Workflows: save a session, run it on a changed site, finish by hand, save the repair.
    let mut s = Server::start(&workflow_site("<a href=/report>Report</a>"), &[], &[]);
    s.init("2025-06-18");
    let page = text(&s.call("browser_open", json!({"url": "https://shop.test/login"})));
    s.call(
        "browser_type",
        json!({"ref": find(&page, "textbox", "User"), "text": "ada"}),
    );
    s.call(
        "browser_click",
        json!({"ref": find(&page, "button", "Sign in")}),
    );
    let page = text(&s.call("browser_observe", json!({"full": true})));
    s.call(
        "browser_click",
        json!({"ref": find(&page, "link", "Report")}),
    );
    let r = s.call("browser_workflow_save", json!({}));
    let wf = r["structuredContent"]["workflow"].clone();
    assert!(text(&r).contains("parameters: user"), "{}", text(&r));
    assert_eq!(wf["steps"].as_array().map(Vec::len), Some(3), "{wf}");
    drop(s);
    // The site renamed the link: the run drifts at step 3 and offers the new one.
    let mut s = Server::start(&workflow_site("<a href=/archive>Archive</a>"), &[], &[]);
    s.init("2025-06-18");
    let r = s.call(
        "browser_workflow_run",
        json!({"workflow": wf, "params": {"user": "bob"}}),
    );
    assert_eq!(r["structuredContent"]["status"], "drifted", "{r}");
    assert_eq!(r["structuredContent"]["stop"]["step"], 3, "{r}");
    let t = text(&r);
    assert!(
        t.contains("drifted at step 3: no link \"Report\" on the page"),
        "{t}"
    );
    // Finish by hand, then save: the repaired workflow runs clean.
    s.call("browser_click", json!({"ref": find(&t, "link", "Archive")}));
    let repaired =
        s.call("browser_workflow_save", json!({}))["structuredContent"]["workflow"].clone();
    let r = s.call(
        "browser_workflow_run",
        json!({"workflow": repaired, "params": {"user": "bob"}}),
    );
    assert_eq!(r["structuredContent"]["status"], "done", "{r}");
    assert!(text(&r).contains("click link \"Archive\""), "{}", text(&r));
}

#[test]
fn page_tools_are_called_over_mcp() {
    // WebMCP: tools are listed first in the observation and called with browser_call_tool.
    let rec = file(
        "mcp-webmcp",
        &site(vec![html(
            "https://air.test/",
            r#"<p id=out>none</p><script>document.modelContext.registerTool({name: "searchFlights", description: "Find flights",
               inputSchema: {type: "object", properties: {from: {type: "string"}}, required: ["from"]},
               execute: async ({from}) => { out.textContent = from; return {price: 99}; }})</script>"#,
        )]),
    );
    let mut s = Server::start(&rec, &[], &[]);
    s.init("2025-06-18");
    let r = s.call("browser_open", json!({"url": "https://air.test/"}));
    assert!(text(&r).contains("tool \"searchFlights\" \"Find flights\" (from: string) origin https://air.test script inline"), "{}", text(&r));
    let r = s.call(
        "browser_call_tool",
        json!({"name": "searchFlights", "arguments": {"from": "TXL"}}),
    );
    assert_eq!(
        r["structuredContent"]["outcome"]["tool_result"], r#"{"price":99}"#,
        "{r}"
    );
    assert!(text(&r).contains("\"TXL\""), "{}", text(&r));
    let r = s.call("browser_call_tool", json!({"name": "nope"}));
    assert_eq!(r["structuredContent"]["error"]["kind"], "tool", "{r}");
}

#[test]
fn scrolling_through_mcp() {
    let page = "<body style='margin:0'><div style='height:3000px'>Top</div><p>Bottom</p></body>";
    let rec = file("mcp-scroll", &site(vec![html("https://long.test/", page)]));
    let mut s = Server::start(&rec, &[], &[]);
    s.init("2025-06-18");
    let r = s.call("browser_open", json!({"url": "https://long.test/"}));
    assert!(text(&r).contains("scroll:0/"), "{}", text(&r));
    let r = s.call("browser_scroll", json!({"direction": "down"}));
    assert!(
        text(&r).starts_with("ok scroll down · scrolled to 0,630"),
        "{}",
        text(&r)
    );
    let r = s.call(
        "browser_scroll",
        json!({"direction": "down", "pixels": 100000}),
    );
    assert!(text(&r).contains("scrolled to 0,"), "{}", text(&r));
    let r = s.call("browser_scroll", json!({"direction": "down"}));
    assert!(text(&r).contains("already at the end"), "{}", text(&r));
    let r = s.request(
        "tools/call",
        json!({"name": "browser_scroll", "arguments": {}}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
}

/// One HTTP exchange with the live view (status, head).
fn view(url: &str, method: &str, cookie: Option<&str>) -> (u16, String) {
    use std::io::Read as _;
    let rest = url.trim_start_matches("http://");
    let (host, path) = rest
        .split_once('/')
        .map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let mut s = std::net::TcpStream::connect(host).unwrap_or_else(|e| panic!("{e}"));
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\n");
    if let Some(c) = cookie {
        req.push_str(&format!("Cookie: {c}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())
        .unwrap_or_else(|e| panic!("{e}"));
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap_or_else(|e| panic!("{e}"));
    let text = String::from_utf8_lossy(&out).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, text)
}

#[test]
fn the_live_view_url_goes_to_the_person_never_to_the_model() {
    let rec = shop();
    let mut s = Server::start(&rec, &[], &[]);
    let r = s.request(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {"elicitation": {"url": {}}},
               "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25", "{r}");
    s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    s.call("browser_open", json!({"url": "https://shop.test/"}));
    s.send(&json!({"jsonrpc": "2.0", "id": 99, "method": "tools/call", "params": {
        "name": "browser_takeover", "arguments": {"reason": "see http://evil.example/x", "timeout_s": 30},
        "_meta": {"progressToken": "p1"}}}));
    // The client gets the URL in an elicitation request, and nothing else carries it.
    let mut seen = Vec::new();
    let (eid, url) = loop {
        let m = s.recv();
        seen.push(m.clone());
        if m["method"] == "elicitation/create" {
            assert_eq!(m["params"]["mode"], "url", "{m}");
            break (
                m["params"]["elicitationId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                m["params"]["url"].as_str().unwrap_or_default().to_string(),
            );
        }
    };
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    // The client answers (accept): a response, which the server must not answer.
    s.send(&json!({"jsonrpc": "2.0", "id": eid, "result": {"action": "accept"}}));
    // The person opens the view and presses Done.
    let (status, head) = view(&url, "GET", None);
    assert_eq!(status, 200);
    let cookie = head
        .lines()
        .find_map(|l| l.strip_prefix("Set-Cookie: "))
        .and_then(|c| c.split(';').next())
        .unwrap_or_default()
        .to_string();
    assert_eq!(view(&format!("{url}/done"), "POST", Some(&cookie)).0, 204);
    let result = loop {
        let m = s.recv();
        seen.push(m.clone());
        if m["id"] == 99 {
            break m;
        }
    };
    assert!(
        text(&result["result"]).starts_with("ok takeover"),
        "{result}"
    );
    assert!(
        seen.iter()
            .any(|m| m["method"] == "notifications/elicitation/complete"
                && m["params"]["elicitationId"] == eid.as_str()),
        "{seen:?}"
    );
    for m in &seen {
        if m["method"] != "elicitation/create" {
            assert!(!m.to_string().contains(&url), "the URL leaked: {m}");
        }
    }
    // The response was not answered: the next reply is the ping's.
    let pong = s.request("ping", json!({}));
    assert_eq!(pong["result"], json!({}), "{pong}");
}

/// `_meta` of a 2026-07-28 request.
fn modern(caps: Value) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": caps,
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
    })
}

/// A 2026-07-28 tool call (no handshake): the whole reply.
fn modern_call(s: &mut Server, caps: &Value, tool: &str, args: Value, extra: Value) -> Value {
    let mut params = json!({"name": tool, "arguments": args, "_meta": modern(caps.clone())});
    if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            p.insert(k.clone(), v.clone());
        }
    }
    s.request("tools/call", params)
}

#[test]
fn modern_requests_need_no_handshake() {
    let rec = shop();
    let mut s = Server::start(&rec, &[], &[]);
    let d = s.request("server/discover", json!({"_meta": modern(json!({}))}));
    let r = &d["result"];
    assert_eq!(r["resultType"], "complete", "{d}");
    assert_eq!(r["supportedVersions"][0], "2026-07-28");
    assert!(
        r["supportedVersions"]
            .as_array()
            .is_some_and(|v| v.iter().any(|x| x == "2025-11-25"))
    );
    assert_eq!(r["capabilities"], json!({"tools": {}}));
    assert_eq!(r["cacheScope"], "public");
    assert!(r["ttlMs"].as_u64().is_some_and(|t| t > 0));
    assert!(
        r["instructions"]
            .as_str()
            .is_some_and(|i| i.contains("browser_open"))
    );
    assert_eq!(
        r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "pageon"
    );
    let l = s.request("tools/list", json!({"_meta": modern(json!({}))}));
    assert_eq!(l["result"]["resultType"], "complete");
    assert_eq!(l["result"]["cacheScope"], "public");
    assert!(
        l["result"]["tools"]
            .as_array()
            .is_some_and(|t| t.len() > 10)
    );
    let o = modern_call(
        &mut s,
        &json!({}),
        "browser_open",
        json!({"url": "https://shop.test/"}),
        json!({}),
    );
    assert_eq!(o["result"]["resultType"], "complete", "{o}");
    assert!(text(&o["result"]).contains("Shop"), "{o}");
    assert_eq!(o["result"]["structuredContent"]["step"], 0);
    // Refusals: an unknown version names what is served; 2025 revisions need initialize.
    let mut meta = modern(json!({}));
    meta["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
    let e = s.request("tools/list", json!({"_meta": meta}));
    assert_eq!(e["error"]["code"], -32022, "{e}");
    assert_eq!(e["error"]["data"]["requested"], "1900-01-01");
    assert_eq!(e["error"]["data"]["supported"][0], "2026-07-28");
    meta["io.modelcontextprotocol/protocolVersion"] = json!("2025-11-25");
    let e = s.request("tools/list", json!({"_meta": meta}));
    assert_eq!(e["error"]["code"], -32022, "{e}");
    assert!(
        e["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("initialize")),
        "{e}"
    );
    let e = s.request(
        "tools/list",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
    );
    assert_eq!(e["error"]["code"], -32602, "{e}");
    let e = s.request("ping", json!({"_meta": modern(json!({}))}));
    assert_eq!(e["error"]["code"], -32601, "{e}");
    // The handshake era still works in the same process.
    let i = s.init("2025-11-25");
    assert_eq!(i["result"]["protocolVersion"], "2025-11-25", "{i}");
    assert!(i["result"].get("resultType").is_none(), "{i}");
}

/// The person opens the live view and presses Done.
fn person_done(url: &str) {
    let (status, head) = view(url, "GET", None);
    assert_eq!(status, 200);
    let cookie = head
        .lines()
        .find_map(|l| l.strip_prefix("Set-Cookie: "))
        .and_then(|c| c.split(';').next())
        .unwrap_or_default()
        .to_string();
    assert_eq!(view(&format!("{url}/done"), "POST", Some(&cookie)).0, 204);
}

#[test]
fn the_takeover_is_a_multi_round_trip_request() {
    let rec = shop();
    let mut s = Server::start(&rec, &[], &[]);
    let caps = json!({"elicitation": {"url": {}}});
    modern_call(
        &mut s,
        &caps,
        "browser_open",
        json!({"url": "https://shop.test/"}),
        json!({}),
    );
    let args = json!({"reason": "a second factor", "timeout_s": 30});
    let first = modern_call(&mut s, &caps, "browser_takeover", args.clone(), json!({}));
    let r = &first["result"];
    assert_eq!(r["resultType"], "input_required", "{first}");
    assert!(r.get("content").is_none(), "no tool result yet: {first}");
    let ask = &r["inputRequests"]["takeover"];
    assert_eq!(ask["method"], "elicitation/create");
    assert_eq!(ask["params"]["mode"], "url");
    assert!(ask["params"].get("elicitationId").is_none());
    let url = ask["params"]["url"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let state = r["requestState"].clone();
    // A retry without an answer asks again for the same view.
    let again = modern_call(
        &mut s,
        &caps,
        "browser_takeover",
        args.clone(),
        json!({"requestState": state}),
    );
    assert_eq!(again["result"]["requestState"], state, "{again}");
    // Accept: the retry waits for the person.
    let id = 77;
    s.send(
        &json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
        "name": "browser_takeover", "arguments": args, "_meta": modern(caps.clone()),
        "inputResponses": {"takeover": {"action": "accept"}}, "requestState": state}}),
    );
    person_done(&url);
    let done = s.recv();
    assert_eq!(done["id"], id, "{done}");
    assert!(text(&done["result"]).starts_with("ok takeover"), "{done}");
    assert_eq!(done["result"]["resultType"], "complete");
    assert!(!done.to_string().contains(&url), "the URL leaked: {done}");
    // Decline closes the view.
    let second = modern_call(&mut s, &caps, "browser_takeover", args.clone(), json!({}));
    let state = second["result"]["requestState"].clone();
    let declined = modern_call(
        &mut s,
        &caps,
        "browser_takeover",
        args.clone(),
        json!({"inputResponses": {"takeover": {"action": "decline"}}, "requestState": state}),
    );
    assert_eq!(declined["result"]["isError"], true, "{declined}");
    assert_eq!(
        declined["result"]["structuredContent"]["error"]["kind"],
        "declined"
    );
    // Another call closes a pending view; a stale state starts a new one.
    let third = modern_call(&mut s, &caps, "browser_takeover", args.clone(), json!({}));
    let url3 = third["result"]["inputRequests"]["takeover"]["params"]["url"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let stale = third["result"]["requestState"].clone();
    modern_call(&mut s, &caps, "browser_observe", json!({}), json!({}));
    let host = url3
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(
        std::net::TcpStream::connect(&host).is_err(),
        "the view closed"
    );
    let fresh = modern_call(
        &mut s,
        &caps,
        "browser_takeover",
        args,
        json!({"inputResponses": {"takeover": {"action": "accept"}}, "requestState": stale}),
    );
    assert_eq!(fresh["result"]["resultType"], "input_required", "{fresh}");
    assert_ne!(fresh["result"]["requestState"], stale);
}

#[test]
fn modern_clients_without_url_elicitation_get_the_url_out_of_band() {
    let rec = shop();
    let mut s = Server::start(&rec, &[], &[]);
    modern_call(
        &mut s,
        &json!({}),
        "browser_open",
        json!({"url": "https://shop.test/"}),
        json!({}),
    );
    let mut meta = modern(json!({}));
    meta["progressToken"] = json!("p1");
    s.send(&json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
        "name": "browser_takeover", "arguments": {"reason": "captcha", "timeout_s": 30}, "_meta": meta}}));
    let p = s.recv();
    assert_eq!(p["method"], "notifications/progress", "{p}");
    let url = p["params"]["_meta"]["url"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    person_done(&url);
    let done = s.recv();
    assert_eq!(done["id"], 5);
    assert_eq!(done["result"]["resultType"], "complete");
    assert!(!done.to_string().contains(&url), "the URL leaked: {done}");
}

/// `pageon mcp --http 0`: the child and its printed endpoint URL.
fn http_server(rec: &std::path::Path, extra: &[&str]) -> (Child, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pageon"))
        .args(["mcp", "--http", "0", "--replay"])
        .arg(rec)
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("{e}"));
    let mut err = BufReader::new(child.stderr.take().unwrap_or_else(|| panic!("stderr")));
    let url = loop {
        let mut line = String::new();
        if err.read_line(&mut line).unwrap_or(0) == 0 {
            panic!("no endpoint line");
        }
        if let Some(u) = line.trim().strip_prefix("MCP endpoint: ") {
            break u.to_string();
        }
    };
    // Keep draining stderr so the child never blocks on it.
    std::thread::spawn(move || {
        let mut sink = String::new();
        while err.read_line(&mut sink).unwrap_or(0) > 0 {
            sink.clear();
        }
    });
    (child, url)
}

/// One raw HTTP exchange: (status, head, body). `headers` are extra header lines.
fn post(
    url: &str,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (u16, String, String) {
    use std::io::Read as _;
    let rest = url.trim_start_matches("http://");
    let (host, path) = rest
        .split_once('/')
        .map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let mut s = std::net::TcpStream::connect(host).unwrap_or_else(|e| panic!("{e}"));
    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: {host}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            b.len()
        ));
    }
    req.push_str("\r\n");
    req.push_str(body.unwrap_or_default());
    s.write_all(req.as_bytes())
        .unwrap_or_else(|e| panic!("{e}"));
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap_or_else(|e| panic!("{e}"));
    let text = String::from_utf8_lossy(&out).to_string();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .map_or((text.clone(), String::new()), |(h, b)| {
            (h.to_string(), b.to_string())
        });
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, head, body)
}

/// A modern request over HTTP with matching headers.
fn rpc(url: &str, id: i64, method: &str, mut params: Value) -> (u16, String, String) {
    params["_meta"] = modern(json!({}));
    let name = params["name"].as_str().unwrap_or_default().to_string();
    let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    let mut h = vec![
        ("MCP-Protocol-Version", "2026-07-28"),
        ("Mcp-Method", method),
        ("Accept", "application/json, text/event-stream"),
    ];
    if method == "tools/call" {
        h.push(("Mcp-Name", name.as_str()));
    }
    post(url, "POST", &h, Some(&body))
}

#[test]
fn streamable_http_serves_2026_and_refuses_the_rest() {
    let rec = shop();
    let (mut child, url) = http_server(&rec, &[]);
    assert!(
        url.starts_with("http://127.0.0.1:") && url.contains("/mcp/"),
        "{url}"
    );
    let (st, head, body) = rpc(&url, 1, "server/discover", json!({}));
    assert_eq!(st, 200, "{head}\n{body}");
    assert!(head.contains("Content-Type: application/json"), "{head}");
    let d: Value = serde_json::from_str(&body).unwrap_or_else(|e| panic!("{e}: {body}"));
    assert_eq!(
        d["result"]["supportedVersions"],
        json!(["2026-07-28"]),
        "{d}"
    );
    let (st, _, body) = rpc(
        &url,
        2,
        "tools/call",
        json!({"name": "browser_open", "arguments": {"url": "https://shop.test/"}}),
    );
    assert_eq!(st, 200, "{body}");
    assert!(body.contains("Shop"), "{body}");
    // Header and body disagree.
    let call = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list", "params": {"_meta": modern(json!({}))}}).to_string();
    let (st, _, body) = post(
        &url,
        "POST",
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
        ],
        Some(&call),
    );
    assert_eq!(st, 400);
    assert!(body.contains("-32020"), "{body}");
    let (st, _, body) = post(&url, "POST", &[("Mcp-Method", "tools/list")], Some(&call));
    assert_eq!((st, body.contains("-32020")), (400, true), "{body}");
    // A base64-encoded header value matches its decoded body value.
    let obs = json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "browser_observe", "arguments": {}, "_meta": modern(json!({}))}}).to_string();
    let (st, _, body) = post(
        &url,
        "POST",
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "=?base64?YnJvd3Nlcl9vYnNlcnZl?="),
        ],
        Some(&obs),
    );
    assert_eq!(st, 200, "{body}");
    // The handshake era is for stdio.
    let init = json!({"jsonrpc": "2.0", "id": 5, "method": "initialize", "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}).to_string();
    let (st, _, body) = post(&url, "POST", &[("Mcp-Method", "initialize")], Some(&init));
    assert_eq!(st, 400);
    let e: Value = serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(e["error"]["code"], -32022, "{body}");
    assert_eq!(e["error"]["data"]["supported"], json!(["2026-07-28"]));
    let (st, _, body) = rpc(&url, 6, "resources/list", json!({}));
    assert_eq!((st, body.contains("-32601")), (404, true), "{body}");
    // Notifications are accepted without a body.
    let n =
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}})
            .to_string();
    assert_eq!(
        post(
            &url,
            "POST",
            &[("Mcp-Method", "notifications/cancelled")],
            Some(&n)
        )
        .0,
        202
    );
    // Transport refusals.
    let token_less = url
        .rsplit_once('/')
        .map(|(a, _)| format!("{a}/wrong"))
        .unwrap_or_default();
    assert_eq!(rpc(&token_less, 7, "server/discover", json!({})).0, 404);
    assert_eq!(post(&url, "GET", &[], None).0, 405);
    assert_eq!(post(&url, "DELETE", &[], None).0, 405);
    let d = json!({"jsonrpc": "2.0", "id": 8, "method": "server/discover", "params": {"_meta": modern(json!({}))}}).to_string();
    let hdr = [
        ("MCP-Protocol-Version", "2026-07-28"),
        ("Mcp-Method", "server/discover"),
    ];
    assert_eq!(
        post(
            &url,
            "POST",
            &[hdr[0], hdr[1], ("Origin", "https://evil.example")],
            Some(&d)
        )
        .0,
        403
    );
    assert_eq!(
        post(
            &url,
            "POST",
            &[hdr[0], hdr[1], ("Host", "evil.example")],
            Some(&d)
        )
        .0,
        400
    );
    assert_eq!(post(&url, "POST", &hdr, None).0, 411);
    let (st, _, _) = post(
        &url,
        "POST",
        &[hdr[0], hdr[1], ("Content-Length", "99999999")],
        None,
    );
    assert_eq!(st, 413);
    // A client that sends nothing is dropped after the deadline; the next one is served.
    let host = url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let idle = std::net::TcpStream::connect(&host).unwrap_or_else(|e| panic!("{e}"));
    #[allow(clippy::disallowed_methods)] // a network deadline is real time
    let t = std::time::Instant::now();
    assert_eq!(rpc(&url, 9, "server/discover", json!({})).0, 200);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(4),
        "{:?}",
        t.elapsed()
    );
    drop(idle);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn http_streams_progress_for_a_takeover() {
    let rec = shop();
    let (mut child, url) = http_server(&rec, &["--allow-origin", "https://ops.example"]);
    rpc(
        &url,
        1,
        "tools/call",
        json!({"name": "browser_open", "arguments": {"url": "https://shop.test/"}}),
    );
    let mut meta = modern(json!({}));
    meta["progressToken"] = json!("p");
    let body = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "browser_takeover", "arguments": {"reason": "captcha", "timeout_s": 30}, "_meta": meta}}).to_string();
    let rest = url.trim_start_matches("http://");
    let (host, path) = rest
        .split_once('/')
        .map_or((rest, String::new()), |(h, p)| (h, format!("/{p}")));
    let mut s = std::net::TcpStream::connect(host).unwrap_or_else(|e| panic!("{e}"));
    write!(s, "POST {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: https://ops.example\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/call\r\nMcp-Name: browser_takeover\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap_or_else(|e| panic!("{e}"));
    let mut r = BufReader::new(s);
    let mut events = Vec::new();
    let mut line = String::new();
    let mut head = String::new();
    while r.read_line(&mut line).unwrap_or(0) > 0 {
        if line == "\r\n" {
            break;
        }
        head.push_str(&line);
        line.clear();
    }
    assert!(head.contains("text/event-stream"), "{head}");
    let mut person = None;
    loop {
        line.clear();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if let Some(d) = line.trim().strip_prefix("data: ") {
            let m: Value = serde_json::from_str(d).unwrap_or_else(|e| panic!("{e}: {d}"));
            if m["method"] == "notifications/progress" && person.is_none() {
                let u = m["params"]["_meta"]["url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                person = Some(std::thread::spawn(move || person_done(&u)));
            }
            events.push(m);
        }
    }
    if let Some(p) = person {
        p.join().unwrap_or_else(|_| panic!("person"));
    }
    let last = events.last().cloned().unwrap_or_default();
    assert_eq!(last["id"], 2, "{events:?}");
    assert!(text(&last["result"]).starts_with("ok takeover"), "{last}");
    let _ = child.kill();
    let _ = child.wait();
}

fn tabs_site() -> std::path::PathBuf {
    let mut pages = vec![
        html(
            "https://a.test/",
            "<title>Search</title><input aria-label=Query><a href=/r target=_blank>Result</a>",
        ),
        html("https://b.test/", "<title>Mail</title><p>code 1234</p>"),
        html("https://a.test/r", "<title>Result</title><p>detail</p>"),
    ];
    for n in 1..=6 {
        pages.push(html(
            &format!("https://c.test/{n}"),
            &format!("<title>C{n}</title>"),
        ));
    }
    file("mcp-tabs", &site(pages))
}

#[test]
fn tabs_keep_their_pages() {
    let rec = tabs_site();
    for sandbox in [true, false] {
        let extra: &[&str] = if sandbox { &[] } else { &["--no-sandbox"] };
        let mut s = Server::start(&rec, extra, &[]);
        s.init("2025-11-25");
        let tabs = |s: &mut Server, args: Value| s.call("browser_tabs", args);
        // One tab: replies as before.
        let r = s.call("browser_open", json!({"url": "https://a.test/"}));
        let t = text(&r);
        assert!(t.ends_with("step 0"), "{t}");
        assert_eq!(r["structuredContent"]["tab"], "t1", "{r}");
        let query = find(&t, "textbox", "Query");
        let link = find(&t, "link", "Result");
        s.call("browser_type", json!({"ref": query, "text": "hello"}));
        // A popup is reported with how to open it, and not loaded.
        let r = s.call("browser_click", json!({"ref": link}));
        let t = text(&r);
        assert!(t.contains("popup \"https://a.test/r\""), "{t}");
        assert!(
            t.contains("browser_tabs {\"action\": \"new\", \"url\": \"https://a.test/r\"}"),
            "{t}"
        );
        let l = tabs(&mut s, json!({"action": "list"}));
        assert_eq!(
            l["structuredContent"]["tabs"].as_array().map(Vec::len),
            Some(1),
            "{l}"
        );
        let req = s.call("browser_view", json!({"kind": "requests"}));
        assert!(
            !text(&req).contains("https://a.test/r"),
            "the popup was not loaded: {}",
            text(&req)
        );
        // A second tab: replies name it.
        let r = tabs(&mut s, json!({"action": "new", "url": "https://b.test/"}));
        let t = text(&r);
        assert!(t.contains("code 1234"), "{t}");
        assert!(
            t.lines()
                .last()
                .is_some_and(|l| l.starts_with("step ") && l.ends_with(" · tab t2")),
            "{t}"
        );
        assert_eq!(r["structuredContent"]["tab"], "t2");
        let r = s.call("browser_observe", json!({}));
        assert!(text(&r).ends_with("\ntab t2"), "{}", text(&r));
        let l = tabs(&mut s, json!({"action": "list"}));
        let t = text(&l);
        assert!(t.contains("t1 \"Search\" https://a.test/\n"), "{t}");
        assert!(t.contains("t2 \"Mail\" https://b.test/ (current)"), "{t}");
        // Back to the first: its input and refs are intact.
        let r = tabs(&mut s, json!({"action": "select", "tab": "t1"}));
        assert_eq!(r["structuredContent"]["tab"], "t1", "{r}");
        let full = text(&s.call("browser_observe", json!({"full": true})));
        assert!(full.contains("hello"), "{full}");
        let r = s.call("browser_click", json!({"ref": link}));
        assert!(text(&r).starts_with("ok click"), "{}", text(&r));
        // Reset and workflows rebuild one tab: refused.
        for tool in ["browser_reset", "browser_workflow_save"] {
            let r = s.call(tool, json!({}));
            assert_eq!(
                r["structuredContent"]["error"]["kind"], "several_tabs",
                "{tool}: {r}"
            );
        }
        // Close; handles are never reused; unknown handles name the open ones.
        let r = tabs(&mut s, json!({"action": "close", "tab": "t2"}));
        assert!(
            text(&r).starts_with("closed t2\nt1 \"Search\""),
            "{}",
            text(&r)
        );
        let r = tabs(&mut s, json!({"action": "new", "url": "https://a.test/r"}));
        assert_eq!(r["structuredContent"]["tab"], "t3", "{r}");
        let r = tabs(&mut s, json!({"action": "select", "tab": "t2"}));
        assert_eq!(r["structuredContent"]["error"]["kind"], "unknown_tab");
        assert!(text(&r).contains("open tabs: t1, t3"), "{}", text(&r));
        // At most 8.
        for n in 1..=6 {
            let r = tabs(
                &mut s,
                json!({"action": "new", "url": format!("https://c.test/{n}")}),
            );
            assert!(r.get("isError").is_none(), "{r}");
        }
        let r = tabs(&mut s, json!({"action": "new", "url": "https://b.test/"}));
        assert_eq!(
            r["structuredContent"]["error"]["kind"], "too_many_tabs",
            "{r}"
        );
        // Closing the current tab makes the most recently used one current.
        tabs(&mut s, json!({"action": "select", "tab": "t3"}));
        tabs(&mut s, json!({"action": "select", "tab": "t9"}));
        let r = tabs(&mut s, json!({"action": "close"}));
        assert!(
            text(&r).contains("t3 \"Result\" https://a.test/r (current)"),
            "{}",
            text(&r)
        );
    }
}
