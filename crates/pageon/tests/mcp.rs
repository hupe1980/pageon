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
            "browser_scroll",
            "browser_wait",
            "browser_back",
            "browser_forward",
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
