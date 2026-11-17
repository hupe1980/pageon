//! The vendor toolsets over `pageon mcp`: Anthropic's browser and computer toolsets and
//! OpenAI's computer tool, against the real binary over a recording, caged and in-process.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use common::*;
use serde_json::{Value, json};

struct Server {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: i64,
}

impl Server {
    fn start(rec: &std::path::Path, extra: &[&str]) -> Self {
        Self::start_env(rec, extra, &[])
    }

    fn start_env(rec: &std::path::Path, extra: &[&str], env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pageon"))
            .arg("mcp")
            .arg("--replay")
            .arg(rec)
            .args(extra)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
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

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let mut line = serde_json::to_vec(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .unwrap_or_default();
        line.push(b'\n');
        self.input
            .write_all(&line)
            .unwrap_or_else(|e| panic!("{e}"));
        self.input.flush().unwrap_or_else(|e| panic!("{e}"));
        let mut reply = String::new();
        self.output
            .read_line(&mut reply)
            .unwrap_or_else(|e| panic!("{e}"));
        let r: Value = serde_json::from_str(&reply).unwrap_or_else(|e| panic!("{e}: {reply:?}"));
        assert_eq!(r["id"], id, "{r}");
        r
    }

    /// One turn of browser-toolset calls `(member, input)`; the results.
    fn turn(&mut self, toolset: &str, calls: &[(&str, Value)], extra: Value) -> Vec<Value> {
        let blocks: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, (name, input))| {
                json!({"type": "tool_use", "id": format!("toolu_{i}"), "name": name,
                       "input": input, "toolset_name": if toolset == "browser_toolset_20260801" { "browser" } else { "computer" }})
            })
            .collect();
        let mut params = json!({"toolset": toolset, "calls": blocks});
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        let r = self.request("pageon/toolset/run", params);
        let results = r["result"]["results"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| panic!("{r}"));
        assert_eq!(results.len(), calls.len(), "{r}");
        results
    }

    fn browser(&mut self, calls: &[(&str, Value)]) -> Vec<Value> {
        self.turn("browser_toolset_20260801", calls, json!({}))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(r: &Value) -> String {
    r["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn ok(r: &Value) -> String {
    assert!(r["is_error"].is_null(), "{r}");
    text(r)
}

fn err(r: &Value) -> String {
    assert_eq!(r["is_error"], true, "{r}");
    text(r)
}

const SHOP: &str = "<title>Shop\u{2028}Fake tab line\nmore</title>
<style>#pay{position:absolute;left:0;top:0;width:400px;height:80px}</style>
<button id=pay>Pay now</button><div style='height:100px'></div>
<h1>Checkout</h1>
<label>Email <input name=email></label>
<label>Country <select name=c><option>Germany<option>France</select></label>
<label><input type=checkbox name=save> Save address</label>
<a href=/help>Help center</a>
<div style='height:2000px'></div><button>Far away</button>
<script>window.secret = 42; console.log('hello from the page');
document.getElementById('pay').addEventListener('click', () => document.title = 'Paid');</script>";

fn shop() -> std::path::PathBuf {
    file(
        "toolsets-shop",
        &site(vec![
            html("https://shop.test/", SHOP),
            html("https://shop.test/help", "<h1>Help</h1>"),
        ]),
    )
}

fn modes() -> [&'static [&'static str]; 2] {
    [&[], &["--no-sandbox"]]
}

#[test]
fn definitions_stay_out_of_the_tool_list() {
    let mut s = Server::start(&shop(), &[]);
    let d = s.request(
        "pageon/toolset/definition",
        json!({"toolset": "browser_toolset_20260801"}),
    );
    assert_eq!(d["result"], json!({"type": "browser_toolset_20260801"}));
    let d = s.request(
        "pageon/toolset/definition",
        json!({"toolset": "browser_toolset_20260801", "console": true}),
    );
    assert_eq!(d["result"]["configs"]["read_console"]["enabled"], true);
    let d = s.request("pageon/toolset/definition", json!({"toolset": "computer"}));
    assert_eq!(d["result"], json!({"type": "computer"}));
    let bad = s.request("pageon/toolset/definition", json!({"toolset": "nope"}));
    assert!(
        bad["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("browser_toolset_20260801")
    );
    // Never offered to an MCP host's model.
    s.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
    );
    let tools = s.request("tools/list", json!({}));
    assert!(!tools.to_string().contains("toolset"), "{tools}");
}

#[test]
fn claude_reads_and_acts_through_the_browser_toolset() {
    for mode in modes() {
        let mut s = Server::start(&shop(), mode);
        let r = s.browser(&[
            ("navigate", json!({"url": "shop.test"})),
            ("read_page", json!({"filter": "interactive"})),
            ("find", json!({"query": "the help center link"})),
        ]);
        assert!(
            ok(&r[0]).starts_with("Navigated to https://shop.test/."),
            "{}",
            text(&r[0])
        );
        let page = ok(&r[1]);
        assert!(
            page.contains("button") && page.contains("\"Pay now\""),
            "{page}"
        );
        // Interactive and in view only: the far button and the heading are not there.
        assert!(
            !page.contains("Far away") && !page.contains("Checkout"),
            "{page}"
        );
        let found = ok(&r[2]);
        assert!(found.contains("by the words"), "{found}");
        let help = found.lines().nth(1).unwrap_or_default().to_string();
        assert!(
            help.starts_with("link") && help.contains("Help center"),
            "{found}"
        );
        let all = ok(&s.browser(&[("read_page", json!({"filter": "all"}))])[0]);
        assert!(
            all.contains("Far away") && all.contains("Checkout"),
            "{all}"
        );
        let email = find(&all, "textbox", "Email");
        let country = find(&all, "combobox", "Country");
        let save = find(&all, "checkbox", "Save address");
        let r = s.browser(&[
            (
                "form_input",
                json!({"target": {"type": "ref", "ref": email}, "value": "ada@example.com"}),
            ),
            (
                "form_input",
                json!({"target": {"type": "ref", "ref": country}, "value": "France"}),
            ),
            (
                "form_input",
                json!({"target": {"type": "ref", "ref": save}, "value": true}),
            ),
            (
                "form_input",
                json!({"target": {"type": "ref", "ref": save}, "value": true}),
            ),
            ("read_page", json!({"ref": email})),
        ]);
        for x in &r[..3] {
            ok(x);
        }
        assert!(ok(&r[3]).contains("already checked"));
        let scoped = ok(&r[4]);
        assert!(
            scoped.contains("ada@example.com") && !scoped.contains("Pay now"),
            "{scoped}"
        );
        // Focused typing, select all, chords.
        let r = s.browser(&[
            (
                "left_click",
                json!({"target": {"type": "ref", "ref": email}}),
            ),
            ("key", json!({"text": "ctrl+a"})),
            ("type", json!({"text": "bob@example.com"})),
            ("read_page", json!({"ref": email})),
        ]);
        assert!(ok(&r[3]).contains("bob@example.com"), "{}", text(&r[3]));
        // A coordinate click names what it hit.
        let r = s.browser(&[(
            "left_click",
            json!({"target": {"type": "coordinate", "x": 100, "y": 40}}),
        )]);
        let clicked = ok(&r[0]);
        assert!(
            clicked.contains("button")
                && clicked.contains("Pay now")
                && clicked.contains("at 100,40"),
            "{clicked}"
        );
        // Screenshots and zooms are PNG images.
        let r = s.browser(&[
            ("screenshot", json!({})),
            ("zoom", json!({"region": [0, 0, 400, 80]})),
            ("get_page_text", json!({})),
        ]);
        for x in &r[..2] {
            let data = x["content"][0]["source"]["data"]
                .as_str()
                .unwrap_or_default();
            assert!(data.starts_with("iVBOR"), "{x}");
        }
        assert!(ok(&r[2]).contains("Checkout"));
    }
}

#[test]
fn a_turn_stops_at_its_first_failure() {
    for mode in modes() {
        let mut s = Server::start(&shop(), mode);
        s.browser(&[("navigate", json!({"url": "https://shop.test/"}))]);
        let r = s.browser(&[
            (
                "left_click",
                json!({"target": {"type": "ref", "ref": "e999"}}),
            ),
            (
                "left_click",
                json!({"target": {"type": "coordinate", "x": 100, "y": 40}}),
            ),
            ("javascript_exec", json!({"text": "1"})),
        ]);
        assert_eq!(
            err(&r[0]),
            "Error: e999 is stale or not found on the current page. Re-read the page to get fresh references."
        );
        for x in &r[1..] {
            assert_eq!(
                err(x),
                "Not executed: an earlier action in this turn failed."
            );
            assert_eq!(x["toolset_name"], "browser");
        }
        // Nothing ran: the title is unchanged.
        let tabs = s.browser(&[("list_tabs", json!({}))]);
        assert!(!tabs[0].to_string().contains("Paid"), "{}", tabs[0]);
    }
}

#[test]
fn members_the_session_did_not_offer_are_refused() {
    let mut s = Server::start(&shop(), &[]);
    s.browser(&[("navigate", json!({"url": "https://shop.test/"}))]);
    let r = s.browser(&[("javascript_exec", json!({"text": "document.cookie"}))]);
    assert_eq!(
        err(&r[0]),
        "Error: javascript_exec is not enabled in this environment."
    );
    let r = s.browser(&[(
        "file_upload",
        json!({"target": {"type": "ref", "ref": "e1"}, "paths": ["/etc/passwd"]}),
    )]);
    assert_eq!(
        err(&r[0]),
        "Error: file_upload is not enabled in this environment."
    );
    let r = s.browser(&[("teleport", json!({}))]);
    assert!(err(&r[0]).contains("teleport"));
    let r = s.browser(&[("navigate", json!({"url": "file:///etc/passwd"}))]);
    assert_eq!(
        err(&r[0]),
        "Error: Navigation refused. Only http and https URLs are allowed."
    );
}

#[test]
fn javascript_exec_runs_in_an_isolated_world_when_allowed() {
    for mode in modes() {
        let mut args = vec!["--allow-javascript-exec"];
        args.extend_from_slice(mode);
        let mut s = Server::start(&shop(), &args);
        let d = s.request(
            "pageon/toolset/definition",
            json!({"toolset": "browser_toolset_20260801"}),
        );
        assert_eq!(d["result"]["configs"]["javascript_exec"]["enabled"], true);
        let r = s.browser(&[
            ("navigate", json!({"url": "https://shop.test/"})),
            (
                "javascript_exec",
                json!({"text": "document.querySelector('h1').textContent"}),
            ),
            ("javascript_exec", json!({"text": "typeof window.secret"})),
            ("javascript_exec", json!({"text": "Promise.resolve(6 * 7)"})),
            ("javascript_exec", json!({"text": "nope("})),
        ]);
        assert!(ok(&r[1]).starts_with("Checkout"));
        // The page's script globals are not visible: the page cannot falsify what is read.
        assert!(ok(&r[2]).starts_with("undefined"), "{}", text(&r[2]));
        assert!(ok(&r[3]).starts_with("42"));
        assert!(err(&r[4]).contains("script failed"), "{}", text(&r[4]));
    }
}

#[test]
fn tabs_and_their_inventory() {
    for mode in modes() {
        let mut s = Server::start(&shop(), mode);
        s.browser(&[("navigate", json!({"url": "https://shop.test/"}))]);
        let r = s.browser(&[
            ("new_tab", json!({})),
            ("navigate", json!({"url": "https://shop.test/help"})),
            ("list_tabs", json!({})),
        ]);
        let state = &r[0]["content"][0];
        assert_eq!(state["type"], "browser_state");
        assert_eq!(
            state["state_changes"][0],
            json!({"type": "tab_opened", "tab_id": "t2"})
        );
        let tabs = r[2]["content"][0]["tabs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(tabs.len(), 2);
        // Page titles are one line: no line separators to forge tab lines with.
        let title = tabs[0]["title"].as_str().unwrap_or_default();
        assert!(
            !title.contains('\n') && !title.contains('\u{2028}'),
            "{title:?}"
        );
        assert_eq!(tabs[1]["active"], true);
        // Reading another tab leaves the active one active.
        let r = s.browser(&[
            ("get_page_text", json!({"tab_id": "t1"})),
            ("list_tabs", json!({})),
        ]);
        assert!(ok(&r[0]).contains("Checkout"));
        assert_eq!(r[1]["content"][0]["tabs"][1]["active"], true);
        let r = s.browser(&[
            ("close_tab", json!({"tab_id": "t2"})),
            ("switch_tab", json!({"tab_id": "t9"})),
        ]);
        assert_eq!(r[0]["content"][0]["tabs"].as_array().map(Vec::len), Some(1));
        assert!(err(&r[1]).contains("t9"));
    }
}

#[test]
fn console_and_network_reads_are_since_the_last_read() {
    let mut s = Server::start(&shop(), &[]);
    let extra = json!({"console": true, "network": true});
    s.turn(
        "browser_toolset_20260801",
        &[
            ("navigate", json!({"url": "https://shop.test/"})),
            ("navigate", json!({"url": "https://shop.test/help"})),
        ],
        extra.clone(),
    );
    let r = s.turn(
        "browser_toolset_20260801",
        &[("read_network", json!({})), ("read_network", json!({}))],
        extra,
    );
    assert!(
        ok(&r[0]).contains("https://shop.test/help"),
        "{}",
        text(&r[0])
    );
    assert_eq!(ok(&r[1]), "No requests since the last read.");
}

#[test]
fn computer_use_by_coordinates() {
    for mode in modes() {
        let mut s = Server::start(&shop(), mode);
        s.browser(&[("navigate", json!({"url": "https://shop.test/"}))]);
        let r = s.turn(
            "computer_toolset_20260801",
            &[
                ("screenshot", json!({})),
                ("left_click", json!({"coordinate": [100, 40]})),
                ("cursor_position", json!({})),
                ("key", json!({"text": "Return"})),
                ("zoom", json!({"region": [0, 0, 0, 10]})),
                ("wait", json!({"duration": 1})),
            ],
            json!({}),
        );
        assert_eq!(r[0]["toolset_name"], "computer");
        assert!(
            r[0]["content"][0]["source"]["data"]
                .as_str()
                .unwrap_or_default()
                .starts_with("iVBOR")
        );
        assert_eq!(ok(&r[1]), "OK");
        assert_eq!(ok(&r[2]), "X=100, Y=40");
        assert_eq!(ok(&r[3]), "OK");
        assert!(err(&r[4]).contains("x1 > x0"));
        assert_eq!(
            err(&r[5]),
            "Not executed: an earlier computer action in this turn failed."
        );
        let tabs = s.browser(&[("list_tabs", json!({}))]);
        assert!(tabs[0].to_string().contains("Paid"), "{}", tabs[0]);
    }
}

#[test]
fn openai_computer_calls_answer_with_a_screenshot() {
    let mut s = Server::start(&shop(), &[]);
    s.browser(&[("navigate", json!({"url": "https://shop.test/"}))]);
    let call = |id: &str, checks: Value| {
        json!({"type": "computer_call", "call_id": id, "status": "completed",
               "actions": [{"type": "click", "button": "left", "x": 100, "y": 40}, {"type": "wait"}],
               "pending_safety_checks": checks})
    };
    let r = s.request(
        "pageon/toolset/run",
        json!({"toolset": "computer", "calls": [call("c1", json!([{"id": "sc1", "code": "malicious_instructions", "message": "?"}]))]}),
    );
    let out = &r["result"]["results"][0];
    assert_eq!(out["type"], "computer_call_output");
    assert!(
        out["pageon_error"]
            .as_str()
            .unwrap_or_default()
            .contains("safety checks")
    );
    let r = s.request(
        "pageon/toolset/run",
        json!({"toolset": "computer", "confirmed": ["c2"], "calls": [call("c2", json!([{"id": "sc1"}]))]}),
    );
    let out = &r["result"]["results"][0];
    assert!(
        out["output"]["image_url"]
            .as_str()
            .unwrap_or_default()
            .starts_with("data:image/png;base64,")
    );
    assert_eq!(out["acknowledged_safety_checks"][0]["id"], "sc1");
    assert!(out.get("pageon_error").is_none(), "{out}");
    let tabs = s.browser(&[("list_tabs", json!({}))]);
    assert!(tabs[0].to_string().contains("Paid"), "{}", tabs[0]);
}

#[test]
fn toolset_calls_are_steps_that_replay() {
    let mut s = Server::start(&shop(), &[]);
    s.browser(&[("navigate", json!({"url": "https://shop.test/"}))]);
    s.browser(&[(
        "left_click",
        json!({"target": {"type": "coordinate", "x": 100, "y": 40}}),
    )]);
    s.browser(&[("read_page", json!({}))]);
    s.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
    );
    // Reads are not steps: the session has the open and the click.
    let reset = s.request(
        "tools/call",
        json!({"name": "browser_reset", "arguments": {"step": 1}}),
    );
    let t = reset.to_string();
    assert!(!t.contains("\"isError\":true"), "{t}");
}

#[test]
fn secrets_stay_out_of_every_result() {
    let page = "<label>Password <input type=password id=pw></label><p id=echo></p>
<script>pw.addEventListener('input', () => {
  console.log('typed ' + pw.value);
  document.getElementById('echo').textContent = pw.value;
});</script>";
    let rec = file(
        "toolsets-secret",
        &site(vec![
            html("https://shop.test/", page),
            html("https://other.test/", "<input id=q aria-label=Search>"),
        ]),
    );
    for mode in modes() {
        let mut args = vec!["--secret", "shop@https://shop.test"];
        args.extend_from_slice(mode);
        let mut s = Server::start_env(
            &rec,
            &args,
            &[("PAGEON_SECRET_SHOP", "hunter2-very-secret")],
        );
        let extra = json!({"console": true});
        let all = s.turn(
            "browser_toolset_20260801",
            &[
                ("navigate", json!({"url": "https://shop.test/"})),
                ("read_page", json!({"filter": "all"})),
            ],
            extra.clone(),
        );
        let pw = find(&ok(&all[1]), "textbox", "Password");
        let r = s.turn(
            "browser_toolset_20260801",
            &[
                ("left_click", json!({"target": {"type": "ref", "ref": pw}})),
                ("type", json!({"text": "secret://shop"})),
                ("read_console", json!({})),
                ("read_page", json!({"filter": "all"})),
                ("get_page_text", json!({})),
                ("screenshot", json!({})),
            ],
            extra,
        );
        let everything = serde_json::to_string(&r).unwrap_or_default();
        assert!(!everything.contains("hunter2"), "{everything}");
        assert!(ok(&r[2]).contains("typed secret://shop"), "{}", text(&r[2]));
        // Nothing editable focused: the secret is not released.
        let r = s.browser(&[
            ("navigate", json!({"url": "https://other.test/"})),
            ("type", json!({"text": "secret://shop"})),
        ]);
        assert!(err(&r[1]).contains("secret"), "{}", text(&r[1]));
    }
}

#[test]
fn a_coordinate_click_names_a_third_party_frame() {
    let top =
        "<style>iframe{position:absolute;left:0;top:0;width:400px;height:200px;border:0}</style>
<button>Real button</button><iframe src='https://ads.test/'></iframe>";
    let rec = file(
        "toolsets-frame",
        &site(vec![
            html("https://shop.test/", top),
            html(
                "https://ads.test/",
                "<button style='width:380px;height:180px'>Claim prize</button>",
            ),
        ]),
    );
    for mode in modes() {
        let mut s = Server::start(&rec, mode);
        let r = s.browser(&[
            ("navigate", json!({"url": "https://shop.test/"})),
            (
                "left_click",
                json!({"target": {"type": "coordinate", "x": 50, "y": 20}}),
            ),
        ]);
        let t = ok(&r[1]);
        assert!(
            t.contains("Claim prize") && t.contains("third-party frame https://ads.test"),
            "{t}"
        );
    }
}

/// The query of a MiniWoB++ task (the first quoted text of the page).
fn task_query(page: &str) -> String {
    page.lines()
        .find_map(|l| {
            l.trim_start()
                .strip_prefix("text ")
                .map(|t| quoted(t).concat())
        })
        .unwrap_or_default()
}

/// One MiniWoB++ episode played through the browser toolset, as Claude would: read the page,
/// act on refs; the reward read with javascript_exec.
fn toolset_episode(s: &mut Server, task: &str) -> Result<f64, String> {
    let r = s.browser(&[
        (
            "navigate",
            json!({"url": format!("{MINIWOB_ORIGIN}miniwob/{task}.html")}),
        ),
        ("read_page", json!({"filter": "all"})),
    ]);
    let page = text(&r[1]);
    let q = task_query(&page);
    let args: Vec<String> = q
        .split('"')
        .skip(1)
        .step_by(2)
        .map(|a| a.trim().to_string())
        .collect();
    let click = |r: &str| ("left_click", json!({"target": {"type": "ref", "ref": r}}));
    let set = |r: &str, v: Value| {
        (
            "form_input",
            json!({"target": {"type": "ref", "ref": r}, "value": v}),
        )
    };
    let calls: Vec<(&str, Value)> = match task {
        "click-button" => vec![click(&find(&page, "button", &args[0]))],
        "focus-text" => vec![click(&refs(&page, "textbox")[0].0)],
        "click-checkboxes" => {
            let list = q
                .trim_start_matches("Select ")
                .trim_end_matches(" and click Submit.");
            let mut c: Vec<(&str, Value)> = list
                .split([',', ' '])
                .filter(|w| !w.is_empty() && *w != "and" && list != "nothing")
                .map(|n| set(&find(&page, "checkbox", n), json!(true)))
                .collect();
            c.push(click(&find(&page, "button", "Submit")));
            c
        }
        "click-option" => {
            let name = q
                .trim_start_matches("Select ")
                .trim_end_matches(" and click Submit.");
            vec![
                set(&find(&page, "radio", name), json!(true)),
                click(&find(&page, "button", "Submit")),
            ]
        }
        "enter-text" => vec![
            set(&refs(&page, "textbox")[0].0, json!(args[0])),
            click(&find(&page, "button", "Submit")),
        ],
        "login-user" => {
            let f = refs(&page, "textbox");
            vec![
                set(&f[0].0, json!(args[0])),
                set(&f[1].0, json!(args[1])),
                click(&find(&page, "button", "Login")),
            ]
        }
        "choose-list" => {
            let name = q
                .trim_start_matches("Select ")
                .trim_end_matches(" from the list and click Submit.");
            vec![
                set(&refs(&page, "combobox")[0].0, json!(name)),
                click(&find(&page, "button", "Submit")),
            ]
        }
        other => return Err(format!("no episode for {other}")),
    };
    for r in s.browser(&calls) {
        if r["is_error"] == true {
            return Err(format!("{task}: {}", text(&r)));
        }
    }
    let r = s.browser(&[(
        "javascript_exec",
        json!({"text": "document.getElementById('reward-last').textContent"}),
    )]);
    let t = text(&r[0]);
    t.lines()
        .next()
        .and_then(|l| l.trim().parse::<f64>().ok())
        .ok_or_else(|| format!("{task}: no reward ({t:?}); query {q:?}"))
}

#[test]
fn miniwob_through_the_browser_toolset() {
    let Some(log) = miniwob_log() else {
        println!("skipped: run `uv run pageon-bench setup` in bench/ to fetch MiniWoB++");
        return;
    };
    let tasks = [
        "click-button",
        "focus-text",
        "click-checkboxes",
        "click-option",
        "enter-text",
        "login-user",
        "choose-list",
    ];
    for mode in modes() {
        let mut ok_n = 0;
        let mut failures = Vec::new();
        for seed in 0..3u64 {
            let mut log = log.clone();
            log.random = log
                .random
                .iter()
                .map(|v| v.wrapping_mul(seed * 2 + 1).wrapping_add(seed))
                .collect();
            let rec = file(&format!("toolsets-miniwob-{seed}"), &log);
            let mut args = vec!["--allow-javascript-exec"];
            args.extend_from_slice(mode);
            for task in tasks {
                // One session per episode, as the oracle plays them: a recording serves each
                // uncacheable script once.
                let mut s = Server::start(&rec, &args);
                match toolset_episode(&mut s, task) {
                    Ok(r) if r > 0.0 => ok_n += 1,
                    Ok(r) => failures.push(format!("{task} #{seed}: reward {r}")),
                    Err(e) => failures.push(format!("{task} #{seed}: {e}")),
                }
            }
        }
        println!(
            "MiniWoB++ through the browser toolset {mode:?}: {ok_n}/{} rewarded",
            tasks.len() * 3
        );
        assert!(failures.is_empty(), "{failures:#?}");
    }
}

/// A toolset call costs what its action costs over MCP (release build:
/// `cargo test --release -p pageon --test toolsets -- --ignored`).
#[test]
#[ignore = "timing; run in release"]
#[allow(clippy::disallowed_methods)] // wall time is what is measured
fn toolset_calls_cost_what_their_actions_cost() {
    let mut s = Server::start(&shop(), &[]);
    s.browser(&[("navigate", json!({"url": "https://shop.test/"}))]);
    let all = ok(&s.browser(&[("read_page", json!({"filter": "all"}))])[0]);
    let save = find(&all, "checkbox", "Save address");
    s.request(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
    );
    let n = 60;
    let t = std::time::Instant::now();
    for _ in 0..n {
        s.request(
            "tools/call",
            json!({"name": "browser_click", "arguments": {"ref": save}}),
        );
    }
    let mcp = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
    let t = std::time::Instant::now();
    for _ in 0..n {
        s.browser(&[(
            "left_click",
            json!({"target": {"type": "ref", "ref": save}}),
        )]);
    }
    let toolset = t.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
    // A batch of 10 is one round trip.
    let batch: Vec<(&str, Value)> = (0..10)
        .map(|_| {
            (
                "left_click",
                json!({"target": {"type": "ref", "ref": save}}),
            )
        })
        .collect();
    let t = std::time::Instant::now();
    s.browser(&batch);
    let ten = t.elapsed().as_secs_f64() * 1000.0;
    println!(
        "click over MCP {mcp:.2} ms, over the toolset {toolset:.2} ms, 10 in one turn {ten:.2} ms"
    );
    // The MCP tool also renders an observation; the toolset call must not cost more than it
    // plus 0.2 ms.
    assert!(
        toolset <= mcp + 0.2,
        "toolset {toolset:.2} ms vs MCP {mcp:.2} ms"
    );
}
