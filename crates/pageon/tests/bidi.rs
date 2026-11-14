//! The WebDriver BiDi server: raw protocol transcripts through
//! `pageon::bidi::Session`, and the endpoint through the real binary (caged and in-process).

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use common::*;
use pageon::{LoadOptions, Replay, Secret, Secrets, Session};
use pageon_env::Env;
use serde_json::{Value, json};

/// A BiDi session over a replayed site.
struct Bidi<'s, E: Env> {
    bidi: pageon::bidi::Session<'s, E>,
    next: u64,
    events: Vec<Value>,
}

impl<'s, E: Env> Bidi<'s, E> {
    fn new(session: &'s Session<E>) -> Self {
        Self {
            bidi: pageon::bidi::Session::new(session, LoadOptions::default()),
            next: 0,
            events: Vec::new(),
        }
    }

    /// A started session.
    fn started(session: &'s Session<E>) -> Self {
        let mut b = Self::new(session);
        let r = b.call("session.new", json!({"capabilities": {}}));
        assert_eq!(r["type"], "success", "{r}");
        b
    }

    fn raw(&mut self, msg: &str) -> Vec<Value> {
        self.bidi
            .handle(msg)
            .iter()
            .map(|m| serde_json::from_str(m).unwrap_or_else(|e| panic!("{e}: {m}")))
            .collect()
    }

    /// Sends a command; returns its reply, keeping the events.
    fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        let out = self.raw(&json!({"id": id, "method": method, "params": params}).to_string());
        let mut reply = Value::Null;
        for m in out {
            if m["id"] == id {
                reply = m;
            } else {
                self.events.push(m);
            }
        }
        reply
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let r = self.call(method, params);
        assert_eq!(r["type"], "success", "{method}: {r}");
        r["result"].clone()
    }

    fn top(&mut self) -> String {
        let t = self.ok("browsingContext.getTree", json!({}));
        t["contexts"][0]["context"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn eval(&mut self, ctx: &str, expr: &str) -> Value {
        let r = self.ok(
            "script.evaluate",
            json!({"expression": expr, "target": {"context": ctx}, "awaitPromise": true}),
        );
        assert_eq!(r["type"], "success", "{expr}: {r}");
        r["result"].clone()
    }

    fn take_events(&mut self, method: &str) -> Vec<Value> {
        let (hit, rest): (Vec<Value>, Vec<Value>) = std::mem::take(&mut self.events)
            .into_iter()
            .partition(|e| e["method"] == method);
        self.events = rest;
        hit.into_iter().map(|e| e["params"].clone()).collect()
    }
}

fn shop() -> Replay {
    Replay::new(site(vec![
        html(
            "https://shop.test/",
            "<title>Shop</title><input id=q value=x><button id=b onclick=\"this.textContent='clicked'\">Press</button>\
             <form action=/done><input id=f name=f></form>\
             <script>console.log('hello', 1, {a: 2})</script>",
        ),
        html("https://shop.test/done?f=hi", "<title>Done</title>"),
        html("https://shop.test/other", "<title>Other</title>"),
        html("https://shop.test/", "<title>Shop</title><p>again</p>"),
    ]))
}

#[test]
fn session_lifecycle_and_capabilities() {
    let session = Session::new(shop());
    let mut b = Bidi::new(&session);
    let s = b.ok("session.status", json!({}));
    assert_eq!(s["ready"], true);
    let r = b.call("browsingContext.getTree", json!({}));
    assert_eq!(r["error"], "invalid session id", "{r}");
    let r = b.call(
        "session.new",
        json!({"capabilities": {"alwaysMatch": {"browserName": "chrome"}}}),
    );
    assert_eq!(r["error"], "session not created", "{r}");
    assert!(
        r["message"]
            .as_str()
            .unwrap_or_default()
            .contains("browserName"),
        "{r}"
    );
    // A firstMatch entry that fits wins.
    let r = b.ok(
        "session.new",
        json!({"capabilities": {"firstMatch": [{"browserName": "firefox"}, {"browserName": "pageon"}],
               "alwaysMatch": {"webSocketUrl": true, "unhandledPromptBehavior": {"default": "ignore"}}}}),
    );
    let c = &r["capabilities"];
    assert_eq!(c["browserName"], "pageon");
    assert_eq!(c["acceptInsecureCerts"], false);
    assert!(
        c["userAgent"]
            .as_str()
            .unwrap_or_default()
            .contains("pageon"),
        "{c}"
    );
    assert_eq!(c["unhandledPromptBehavior"]["default"], "dismiss");
    assert!(c["webSocketUrl"].is_string());
    let r = b.call("session.new", json!({"capabilities": {}}));
    assert_eq!(r["error"], "session not created");
    assert_eq!(b.ok("session.status", json!({}))["ready"], false);
    assert_eq!(
        b.ok("browser.getUserContexts", json!({}))["userContexts"][0]["userContext"],
        "default"
    );
    b.ok("session.end", json!({}));
    assert!(b.bidi.closed());
}

#[test]
fn subscriptions_and_navigation_events() {
    let session = Session::new(shop());
    let mut b = Bidi::started(&session);
    let a = b.top();
    b.ok(
        "session.subscribe",
        json!({"events": ["browsingContext", "network.beforeRequestSent"], "contexts": [a]}),
    );
    // A new tab: no event, it is outside the subscription.
    let tab = b.ok("browsingContext.create", json!({"type": "tab"}))["context"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(b.take_events("browsingContext.contextCreated").is_empty());
    let nav = b.ok(
        "browsingContext.navigate",
        json!({"context": a, "url": "https://shop.test/", "wait": "complete"}),
    );
    assert_eq!(nav["url"], "https://shop.test/");
    let id = nav["navigation"].clone();
    let started = b.take_events("browsingContext.navigationStarted");
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0]["navigation"], id);
    assert_eq!(started[0]["url"], "https://shop.test/");
    let req = b.take_events("network.beforeRequestSent");
    assert_eq!(req[0]["navigation"], id, "{req:?}");
    assert_eq!(req[0]["request"]["method"], "GET");
    let dom = b.take_events("browsingContext.domContentLoaded");
    let load = b.take_events("browsingContext.load");
    assert_eq!((dom.len(), load.len()), (1, 1));
    assert_eq!(load[0]["navigation"], id);
    // The other tab's navigation sends nothing.
    b.ok(
        "browsingContext.navigate",
        json!({"context": tab, "url": "https://shop.test/other", "wait": "complete"}),
    );
    assert!(
        b.events.iter().all(|e| e["params"]["context"] != tab),
        "{:?}",
        b.events
    );
    // getTree has both tabs; close one.
    assert_eq!(
        b.ok("browsingContext.getTree", json!({}))["contexts"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    b.ok("browsingContext.close", json!({"context": tab}));
    assert_eq!(
        b.ok("browsingContext.getTree", json!({}))["contexts"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    // Unsubscribe by id stops events.
    let sub = b.ok("session.subscribe", json!({"events": ["log.entryAdded"]}))["subscription"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    b.ok("session.unsubscribe", json!({"subscriptions": [sub]}));
    let r = b.call("session.unsubscribe", json!({"subscriptions": ["nope"]}));
    assert_eq!(r["error"], "invalid argument");
}

#[test]
fn script_values_handles_and_nodes() {
    let session = Session::new(shop());
    let mut b = Bidi::started(&session);
    let a = b.top();
    b.ok(
        "session.subscribe",
        json!({"events": ["log.entryAdded", "script"]}),
    );
    b.ok(
        "browsingContext.navigate",
        json!({"context": a, "url": "https://shop.test/", "wait": "complete"}),
    );
    // The console entry, with its arguments as RemoteValues.
    let logs = b.take_events("log.entryAdded");
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!(logs[0]["method"], "log");
    assert_eq!(logs[0]["level"], "info");
    // pageon's console reports one text argument (as over CDP).
    assert!(
        logs[0]["text"]
            .as_str()
            .unwrap_or_default()
            .starts_with("hello 1"),
        "{logs:?}"
    );
    assert_eq!(logs[0]["args"][0]["type"], "string");
    assert!(!b.take_events("script.realmCreated").is_empty());
    // Every RemoteValue type the page can make.
    let types = [
        ("undefined", "undefined"),
        ("null", "null"),
        ("'s'", "string"),
        ("NaN", "number"),
        ("true", "boolean"),
        ("1n", "bigint"),
        ("Symbol()", "symbol"),
        ("[1]", "array"),
        ("({})", "object"),
        ("() => 1", "function"),
        ("/x/", "regexp"),
        ("new Date(0)", "date"),
        ("new Map()", "map"),
        ("new Set()", "set"),
        ("new WeakMap()", "weakmap"),
        ("new WeakSet()", "weakset"),
        ("(function*(){})()", "generator"),
        ("new Error('e')", "error"),
        ("new Proxy({}, {})", "proxy"),
        ("new Promise(() => {})", "promise"),
        ("new Uint8Array(1)", "typedarray"),
        ("new ArrayBuffer(1)", "arraybuffer"),
        ("document.querySelectorAll('input')", "nodelist"),
        ("document.getElementsByTagName('input')", "htmlcollection"),
        ("document.body", "node"),
        ("window", "window"),
    ];
    for (expr, ty) in types {
        let r = b.ok(
            "script.evaluate",
            json!({"expression": expr, "target": {"context": a}, "awaitPromise": false}),
        );
        assert_eq!(r["result"]["type"], ty, "{expr}: {r}");
    }
    assert_eq!(b.eval(&a, "window")["value"]["context"], a);
    // The spec's example: a map holding a node with a sharedId, which comes back as itself.
    let m = b.eval(&a, "new Map([[1, document.getElementById('q')]])");
    assert_eq!(m["type"], "map");
    let node = m["value"][0][1].clone();
    assert_eq!(node["type"], "node");
    assert_eq!(node["value"]["localName"], "input");
    let shared = node["sharedId"].as_str().unwrap_or_default().to_string();
    assert!(!shared.is_empty(), "{node}");
    let f = b.ok(
        "script.callFunction",
        json!({
            "functionDeclaration": "(n, m, d, r, a, o) => [n.id, m.get('k'), d.getTime(), r.test('AAA'), a.length, o.x, typeof o.y]",
            "awaitPromise": false, "target": {"context": a},
            "arguments": [
                {"sharedId": shared},
                {"type": "map", "value": [["k", {"type": "number", "value": "-0"}]]},
                {"type": "date", "value": "1970-01-01T00:00:01.000Z"},
                {"type": "regexp", "value": {"pattern": "a+", "flags": "i"}},
                {"type": "array", "value": [{"type": "bigint", "value": "12345678901234567890"}, {"sharedId": shared}]},
                {"type": "object", "value": [["x", {"type": "string", "value": "y"}], ["y", {"type": "undefined"}]]},
            ],
        }),
    );
    assert_eq!(
        f["result"]["value"],
        json!([
            {"type": "string", "value": "q"},
            {"type": "number", "value": "-0"},
            {"type": "number", "value": 1000},
            {"type": "boolean", "value": true},
            {"type": "number", "value": 2},
            {"type": "string", "value": "y"},
            {"type": "string", "value": "undefined"},
        ]),
        "{f}"
    );
    // Handles: owned with `root`, usable as `this` and arguments, gone after disown.
    let h = b.ok(
        "script.evaluate",
        json!({"expression": "({n: 5})", "target": {"context": a}, "awaitPromise": false, "resultOwnership": "root"}),
    );
    let handle = h["result"]["handle"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!handle.is_empty(), "{h}");
    let r = b.ok(
        "script.callFunction",
        json!({"functionDeclaration": "function (k) { return this[k] * 2 }", "awaitPromise": false,
               "target": {"context": a}, "this": {"handle": handle}, "arguments": [{"type": "string", "value": "n"}]}),
    );
    assert_eq!(r["result"], json!({"type": "number", "value": 10}));
    b.ok(
        "script.disown",
        json!({"handles": [handle], "target": {"context": a}}),
    );
    let r = b.call(
        "script.callFunction",
        json!({"functionDeclaration": "(o) => o", "awaitPromise": false, "target": {"context": a},
               "arguments": [{"handle": handle}]}),
    );
    assert_eq!(r["error"], "no such handle", "{r}");
    // Exceptions, and a promise the virtual clock settles.
    let r = b.ok(
        "script.evaluate",
        json!({"expression": "throw new TypeError('boom')", "target": {"context": a}, "awaitPromise": false}),
    );
    assert_eq!(r["type"], "exception");
    assert_eq!(r["exceptionDetails"]["text"], "TypeError: boom");
    assert_eq!(r["exceptionDetails"]["exception"]["type"], "error");
    let r = b.ok(
        "script.evaluate",
        json!({"expression": "new Promise(r => setTimeout(() => r('later'), 3000))", "target": {"context": a}, "awaitPromise": true}),
    );
    assert_eq!(r["result"], json!({"type": "string", "value": "later"}));
    let r = b.ok(
        "script.evaluate",
        json!({"expression": "Promise.reject(new Error('nope'))", "target": {"context": a}, "awaitPromise": true}),
    );
    assert_eq!(r["exceptionDetails"]["text"], "Error: nope");
    // Sandboxes are their own realms on the same document.
    let s = b.ok(
        "script.evaluate",
        json!({"expression": "document.title", "target": {"context": a, "sandbox": "util"}, "awaitPromise": false}),
    );
    assert_eq!(s["result"]["value"], "Shop");
    assert_ne!(s["realm"], h["realm"]);
    let realms = b.ok("script.getRealms", json!({"context": a}))["realms"].clone();
    assert!(
        realms
            .as_array()
            .is_some_and(|r| r.iter().any(|x| x["sandbox"] == "util")),
        "{realms}"
    );
    // locateNodes.
    let n = b.ok(
        "browsingContext.locateNodes",
        json!({"context": a, "locator": {"type": "css", "value": "input"}}),
    );
    assert_eq!(n["nodes"].as_array().map(Vec::len), Some(2), "{n}");
    assert!(n["nodes"][0]["sharedId"].is_string());
    // A new document: old sharedIds are stale, never another element.
    b.ok(
        "browsingContext.navigate",
        json!({"context": a, "url": "https://shop.test/other", "wait": "complete"}),
    );
    let r = b.call(
        "script.callFunction",
        json!({"functionDeclaration": "(n) => n", "awaitPromise": false, "target": {"context": a},
               "arguments": [{"sharedId": shared}]}),
    );
    assert_eq!(r["error"], "no such node", "{r}");
}

#[test]
fn input_actions_click_and_type() {
    let session = Session::new(shop());
    let mut b = Bidi::started(&session);
    let a = b.top();
    b.ok(
        "browsingContext.navigate",
        json!({"context": a, "url": "https://shop.test/", "wait": "complete"}),
    );
    let button = b.eval(&a, "document.getElementById('b')");
    b.ok(
        "input.performActions",
        json!({"context": a, "actions": [{"type": "pointer", "id": "mouse", "actions": [
            {"type": "pointerMove", "x": 0, "y": 0, "origin": {"type": "element", "element": {"sharedId": button["sharedId"]}}},
            {"type": "pointerDown", "button": 0},
            {"type": "pointerUp", "button": 0},
        ]}]}),
    );
    assert_eq!(b.eval(&a, "b.textContent")["value"], "clicked");
    b.eval(&a, "document.getElementById('f').focus()");
    let keys: Vec<Value> = ["h", "i", "\u{E007}"]
        .iter()
        .flat_map(|k| {
            [
                json!({"type": "keyDown", "value": k}),
                json!({"type": "keyUp", "value": k}),
            ]
        })
        .collect();
    b.ok(
        "input.performActions",
        json!({"context": a, "actions": [{"type": "key", "id": "kb", "actions": keys}]}),
    );
    let tree = b.ok("browsingContext.getTree", json!({}));
    assert_eq!(
        tree["contexts"][0]["url"], "https://shop.test/done?f=hi",
        "{tree}"
    );
    b.ok("input.releaseActions", json!({"context": a}));
    let r = b.call(
        "input.performActions",
        json!({"context": a, "actions": [{"type": "key", "id": "kb", "actions": [{"type": "keyDown", "value": "\u{E0FF}"}]}]}),
    );
    assert_eq!(r["error"], "invalid argument", "{r}");
}

#[test]
fn errors_say_what_happened() {
    let session = Session::new(shop());
    let mut b = Bidi::started(&session);
    let a = b.top();
    let r = b.call("nope.nothing", json!({}));
    assert_eq!(r["error"], "unknown command", "{r}");
    let r = b.call(
        "network.addIntercept",
        json!({"phases": ["beforeRequestSent"]}),
    );
    assert_eq!(r["error"], "unsupported operation");
    assert!(
        r["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--guard"),
        "{r}"
    );
    let r = b.call(
        "script.addPreloadScript",
        json!({"functionDeclaration": "() => {}"}),
    );
    assert_eq!(r["error"], "unsupported operation");
    let r = b.call("browsingContext.navigate", json!({"context": a}));
    assert_eq!(r["error"], "invalid argument");
    let r = b.call(
        "browsingContext.navigate",
        json!({"context": "nope", "url": "https://shop.test/"}),
    );
    assert_eq!(r["error"], "no such frame");
    let r = b.call("browsingContext.handleUserPrompt", json!({"context": a}));
    assert_eq!(r["error"], "no such alert");
    let r = b.call(
        "browsingContext.locateNodes",
        json!({"context": a, "locator": {"type": "accessibility", "value": {"role": "button"}}}),
    );
    assert_eq!(r["error"], "unsupported operation");
    assert!(
        r["message"]
            .as_str()
            .unwrap_or_default()
            .contains("pageon:observe")
    );
    let r = b.call("session.subscribe", json!({"events": ["nope.event"]}));
    assert_eq!(r["error"], "invalid argument");
    let out = b.raw("not json");
    assert_eq!(out[0]["type"], "error");
    assert_eq!(out[0]["id"], Value::Null);
    assert_eq!(out[0]["error"], "invalid argument");
    let out = b.raw(r#"{"method": "session.status"}"#);
    assert_eq!(out[0]["error"], "invalid argument");
}

#[test]
fn pageon_extension_observes_and_acts() {
    let session = Session::new(shop());
    let mut b = Bidi::started(&session);
    let a = b.top();
    b.ok(
        "browsingContext.navigate",
        json!({"context": a, "url": "https://shop.test/", "wait": "complete"}),
    );
    let o = b.ok("pageon:observe", json!({"context": a}));
    let view = o["observation"].as_str().unwrap_or_default().to_string();
    let line = view
        .lines()
        .find(|l| l.contains("button") && l.contains("Press"))
        .unwrap_or_else(|| panic!("{view}"));
    let r = line
        .split_whitespace()
        .find(|w| w.starts_with('e') && w[1..].chars().all(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("{line}"))
        .to_string();
    let act = b.ok(
        "pageon:act",
        json!({"context": a, "action": format!("click {r}")}),
    );
    assert!(act["outcome"].is_string(), "{act}");
    assert_eq!(b.eval(&a, "b.textContent")["value"], "clicked");
    let r = b.call("pageon:act", json!({"context": a, "action": "fly e1"}));
    assert_eq!(r["type"], "error");
}

/// The binary: both protocols on one port, CDP's refusals, and caged = in-process.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start(rec: &std::path::Path, extra: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pageon"))
            .args(["bidi", "--port", "0", "--replay"])
            .arg(rec)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{e}"));
        let stderr = child.stderr.take().unwrap_or_else(|| panic!("stderr"));
        let mut lines = BufReader::new(stderr).lines();
        let port = loop {
            let line = lines
                .next()
                .and_then(Result::ok)
                .unwrap_or_else(|| panic!("no BiDi line"));
            if let Some(url) = line.strip_prefix("WebDriver BiDi listening on ws://127.0.0.1:") {
                assert!(url.ends_with("/session"), "{line}");
                break url
                    .split('/')
                    .next()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0);
            }
        };
        std::thread::spawn(move || for _ in lines {});
        Self { child, port }
    }

    fn http(&self, head: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap_or_else(|e| panic!("{e}"));
        s.write_all(head.as_bytes())
            .unwrap_or_else(|e| panic!("{e}"));
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    fn connect(&self) -> tungstenite::WebSocket<TcpStream> {
        let s = TcpStream::connect(("127.0.0.1", self.port)).unwrap_or_else(|e| panic!("{e}"));
        let (ws, _) = tungstenite::client(format!("ws://127.0.0.1:{}/session", self.port), s)
            .unwrap_or_else(|e| panic!("{e}"));
        ws
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Sends `commands` in order; returns every message received (replies and events).
fn transcript(ws: &mut tungstenite::WebSocket<TcpStream>, commands: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, c) in commands.iter().enumerate() {
        let id = i as u64 + 1;
        let mut c = c.clone();
        c["id"] = json!(id);
        ws.send(tungstenite::Message::text(c.to_string()))
            .unwrap_or_else(|e| panic!("{e}"));
        loop {
            let m = ws.read().unwrap_or_else(|e| panic!("{e}"));
            let Ok(t) = m.to_text() else { continue };
            let v: Value = serde_json::from_str(t).unwrap_or_else(|e| panic!("{e}: {t}"));
            let done = v["id"] == id;
            out.push(v);
            if done {
                break;
            }
        }
    }
    out
}

#[test]
fn the_endpoint_is_caged_and_refuses_like_cdp() {
    let rec = file(
        "bidi-endpoint",
        &site(vec![html(
            "https://a.test/",
            "<title>A</title><input id=pw><script>console.log('hi')</script>",
        )]),
    );
    let commands = [
        json!({"method": "session.new", "params": {"capabilities": {}}}),
        json!({"method": "session.subscribe", "params": {"events": ["browsingContext", "log", "script"]}}),
        json!({"method": "browsingContext.getTree", "params": {}}),
    ];
    let mut runs = Vec::new();
    for extra in [&[][..], &["--no-sandbox"][..]] {
        let s = Server::start(&rec, extra);
        let mut ws = s.connect();
        // Refusals while one client is connected: a second client, foreign Host and Origin.
        let upgrade = |host: &str, origin: &str| {
            s.http(&format!(
                "GET /session HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{origin}\r\n"
            ))
        };
        let foreign = upgrade("127.0.0.1", "Origin: https://evil.test\r\n");
        assert!(foreign.starts_with("HTTP/1.1 403"), "{foreign}");
        let rebind = upgrade("evil.test", "");
        assert!(rebind.starts_with("HTTP/1.1 400"), "{rebind}");
        let second = upgrade("127.0.0.1", "");
        assert!(second.starts_with("HTTP/1.1 409"), "{second}");
        let mut t = transcript(&mut ws, &commands);
        let ctx = t
            .last()
            .and_then(|r| r["result"]["contexts"][0]["context"].as_str())
            .unwrap_or_default()
            .to_string();
        let more = [
            json!({"method": "browsingContext.navigate", "params": {"context": ctx, "url": "https://a.test/", "wait": "complete"}}),
            json!({"method": "input.performActions", "params": {"context": ctx, "actions": [{"type": "key", "id": "k", "actions": []}]}}),
            json!({"method": "script.evaluate", "params": {"expression": "document.title", "target": {"context": ctx}, "awaitPromise": false}}),
            json!({"method": "browser.close", "params": {}}),
        ];
        let rest = transcript(&mut ws, &more);
        assert!(
            rest.iter().any(|m| m["result"]["result"]["value"] == "A"),
            "{rest:?}"
        );
        t.extend(rest);
        // `browser.close` ends the server.
        let mut child = std::mem::replace(&mut ws, s.connect_dead());
        let _ = child.close(None);
        runs.push(t);
        drop(s);
    }
    assert_eq!(runs[0], runs[1], "caged and in-process transcripts differ");
}

impl Server {
    /// A placeholder socket after the server ended (never used).
    fn connect_dead(&self) -> tungstenite::WebSocket<TcpStream> {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
        let a = l.local_addr().unwrap_or_else(|e| panic!("{e}"));
        let s = TcpStream::connect(a).unwrap_or_else(|e| panic!("{e}"));
        tungstenite::WebSocket::from_raw_socket(s, tungstenite::protocol::Role::Client, None)
    }
}

/// A secret typed through BiDi appears in no BiDi message, even when the page
/// echoes it to the console.
#[test]
fn secrets_never_reach_bidi_messages() {
    let site = Replay::new(site(vec![html(
        "https://bank.test/",
        "<input id=pw type=password aria-label=Password>\
         <script>document.getElementById('pw').addEventListener('input', e => console.log('pw', e.target.value))</script>",
    )]));
    let env = Secrets::new(site).with(
        "pw",
        Secret::value("s3cret-value").origins(["https://bank.test"]),
    );
    let session = Session::new(&env);
    let mut b = Bidi::started(&session);
    let a = b.top();
    b.ok(
        "session.subscribe",
        json!({"events": ["log", "network", "browsingContext"]}),
    );
    b.ok(
        "browsingContext.navigate",
        json!({"context": a, "url": "https://bank.test/", "wait": "complete"}),
    );
    let o = b.ok("pageon:observe", json!({"context": a}));
    let r = o["observation"]
        .as_str()
        .unwrap_or_default()
        .lines()
        .find(|l| l.contains("Password"))
        .and_then(|l| {
            l.split_whitespace()
                .find(|w| w.starts_with('e') && w[1..].chars().all(|c| c.is_ascii_digit()))
                .map(str::to_string)
        })
        .unwrap_or_else(|| panic!("{o}"));
    let act = b.ok(
        "pageon:act",
        json!({"context": a, "action": format!("type {r} secret://pw")}),
    );
    let v = b.eval(&a, "document.getElementById('pw').value");
    let mut seen = serde_json::to_string(&b.events).unwrap_or_default();
    seen.push_str(&act.to_string());
    seen.push_str(&v.to_string());
    assert!(
        seen.contains("pw secret://pw"),
        "the echoed value is replaced: {seen}"
    );
    assert!(!seen.contains("s3cret-value"), "{seen}");
}
