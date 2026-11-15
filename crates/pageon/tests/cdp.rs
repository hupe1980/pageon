//! The CDP server: protocol transcripts through `pageon::cdp::Session`, and the
//! endpoint's discovery and refusals through the real binary.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use common::*;
use pageon::{LoadOptions, Replay, Session};
use serde_json::{Value, json};

/// A CDP session over a replayed site, attached to its first page.
struct Cdp<'s> {
    cdp: pageon::cdp::Session<'s, Replay>,
    next: i64,
    sid: String,
    events: Vec<Value>,
}

impl<'s> Cdp<'s> {
    fn new(session: &'s Session<Replay>) -> Self {
        let mut c = Self {
            cdp: pageon::cdp::Session::new(session, LoadOptions::default()),
            next: 0,
            sid: String::new(),
            events: Vec::new(),
        };
        c.browser("Target.setDiscoverTargets", json!({"discover": true}));
        c.browser(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "flatten": true, "waitForDebuggerOnStart": true}),
        );
        c.sid = c
            .events
            .iter()
            .find(|e| e["method"] == "Target.attachedToTarget")
            .map(|e| {
                e["params"]["sessionId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .unwrap_or_default();
        assert!(!c.sid.is_empty(), "{:#?}", c.events);
        c
    }

    fn send(&mut self, msg: Value) -> Vec<Value> {
        self.cdp
            .handle(&msg.to_string())
            .iter()
            .map(|m| serde_json::from_str(m).unwrap_or_else(|e| panic!("{e}: {m}")))
            .collect()
    }

    /// Sends a command; returns its reply, keeping the events.
    fn call(&mut self, method: &str, params: Value, session: Option<String>) -> Value {
        self.next += 1;
        let id = self.next;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let out = self.send(msg);
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

    fn browser(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params, None)
    }

    fn page(&mut self, method: &str, params: Value) -> Value {
        let sid = self.sid.clone();
        self.call(method, params, Some(sid))
    }

    fn eval(&mut self, expression: &str) -> Value {
        self.page(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
        )
    }
}

fn shop() -> Replay {
    Replay::new(site(vec![
        html(
            "https://shop.test/",
            "<title>Shop</title><input id=q value=x><div id=late></div>\
             <script>setTimeout(() => { late.textContent = 'late' }, 5000)</script>",
        ),
        html("https://shop.test/next", "<title>Next</title>"),
    ]))
}

#[test]
fn connect_navigate_and_events() {
    let session = Session::new(shop());
    let mut c = Cdp::new(&session);
    for m in ["Page.enable", "Runtime.enable", "Network.enable"] {
        assert_eq!(c.page(m, json!({}))["result"], json!({}));
    }
    c.page("Page.setLifecycleEventsEnabled", json!({"enabled": true}));
    let tree = c.page("Page.getFrameTree", json!({}));
    let frame = tree["result"]["frameTree"]["frame"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(tree["result"]["frameTree"]["frame"]["url"], "about:blank");
    c.events.clear();
    let r = c.page("Page.navigate", json!({"url": "https://shop.test/"}));
    let loader = r["result"]["loaderId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(r["result"]["frameId"], frame);
    let names: Vec<String> = c
        .events
        .iter()
        .map(|e| {
            let m = e["method"].as_str().unwrap_or_default();
            match m {
                "Page.lifecycleEvent" => format!(
                    "lifecycle:{}",
                    e["params"]["name"].as_str().unwrap_or_default()
                ),
                _ => m.to_string(),
            }
        })
        .collect();
    let want = [
        "Page.frameStartedLoading",
        "Network.requestWillBeSent",
        "Network.responseReceived",
        "Network.loadingFinished",
        "Runtime.executionContextsCleared",
        "lifecycle:init",
        "Page.frameNavigated",
        "Target.targetInfoChanged",
        "Runtime.executionContextCreated",
        "lifecycle:commit",
        "Page.domContentEventFired",
        "lifecycle:DOMContentLoaded",
        "Page.loadEventFired",
        "lifecycle:load",
        "Page.frameStoppedLoading",
        "lifecycle:networkAlmostIdle",
        "lifecycle:networkIdle",
    ];
    assert_eq!(names, want, "Chrome's order");
    let doc = c
        .events
        .iter()
        .find(|e| e["method"] == "Network.requestWillBeSent")
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        doc["params"]["requestId"],
        loader.as_str(),
        "the document request carries the loader id"
    );
    assert!(
        c.events
            .iter()
            .all(|e| e["sessionId"] == c.sid.as_str() || e["method"] == "Target.targetInfoChanged")
    );
    assert_eq!(
        c.eval("document.title")["result"]["result"]["value"],
        "Shop"
    );
}

#[test]
fn runtime_values_exceptions_and_handles() {
    let session = Session::new(shop());
    let mut c = Cdp::new(&session);
    c.page("Runtime.enable", json!({}));
    c.page("Page.navigate", json!({"url": "https://shop.test/"}));
    for (expr, want) in [
        (
            "1 + 1",
            json!({"type": "number", "value": 2, "description": "2"}),
        ),
        ("'a'", json!({"type": "string", "value": "a"})),
        ("undefined", json!({"type": "undefined"})),
        (
            "null",
            json!({"type": "object", "subtype": "null", "value": null}),
        ),
        (
            "NaN",
            json!({"type": "number", "unserializableValue": "NaN", "description": "NaN"}),
        ),
        (
            "-0",
            json!({"type": "number", "unserializableValue": "-0", "description": "-0"}),
        ),
        (
            "2n ** 64n",
            json!({"type": "bigint", "unserializableValue": "18446744073709551616n", "description": "18446744073709551616n"}),
        ),
        (
            "({a: [1, true]})",
            json!({"type": "object", "value": {"a": [1, true]}}),
        ),
    ] {
        assert_eq!(c.eval(expr)["result"]["result"], want, "{expr}");
    }
    let thrown = c.eval("(() => { throw new TypeError('bad') })()");
    let details = &thrown["result"]["exceptionDetails"];
    assert_eq!(details["exception"]["subtype"], "error");
    assert!(
        details["exception"]["description"]
            .as_str()
            .unwrap_or_default()
            .starts_with("TypeError: bad"),
        "{details}"
    );
    // A node by reference, its backend id, and back.
    let node = c.page(
        "Runtime.evaluate",
        json!({"expression": "document.querySelector('#q')"}),
    );
    let r = &node["result"]["result"];
    assert_eq!(r["subtype"], "node");
    assert_eq!(r["className"], "HTMLInputElement");
    assert_eq!(r["description"], "input#q");
    let oid = r["objectId"].as_str().unwrap_or_default().to_string();
    let described = c.page("DOM.describeNode", json!({"objectId": oid}));
    let n = &described["result"]["node"];
    assert_eq!(n["nodeName"], "INPUT");
    assert_eq!(n["attributes"], json!(["id", "q", "value", "x"]));
    let backend = n["backendNodeId"].clone();
    let resolved = c.page("DOM.resolveNode", json!({"backendNodeId": backend}));
    let oid2 = resolved["result"]["object"]["objectId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let value = c.page(
        "Runtime.callFunctionOn",
        json!({"functionDeclaration": "function () { return this.value }", "objectId": oid2, "returnByValue": true}),
    );
    assert_eq!(value["result"]["result"]["value"], "x");
    c.page("Runtime.releaseObject", json!({"objectId": oid2}));
    let gone = c.page(
        "Runtime.callFunctionOn",
        json!({"functionDeclaration": "function () {}", "objectId": oid2}),
    );
    assert_eq!(
        gone["error"]["message"],
        "Could not find object with given id"
    );
    // A navigation destroys the contexts and the objects.
    c.page("Page.navigate", json!({"url": "https://shop.test/next"}));
    let stale = c.page("DOM.describeNode", json!({"objectId": oid}));
    assert_eq!(
        stale["error"]["message"],
        "Could not find object with given id"
    );
}

#[test]
#[allow(clippy::disallowed_methods)] // measures real time on purpose
fn awaited_promises_run_virtual_time() {
    let session = Session::new(shop());
    let mut c = Cdp::new(&session);
    c.page("Page.navigate", json!({"url": "https://shop.test/"}));
    let t = std::time::Instant::now();
    let r = c.eval(
        "new Promise(r => { const t = setInterval(() => { if (late.textContent) { clearInterval(t); r(late.textContent) } }, 100) })",
    );
    assert_eq!(r["result"]["result"]["value"], "late");
    assert!(t.elapsed().as_millis() < 1000, "{:?}", t.elapsed());
    // A promise nothing will settle stays pending: no reply until the client gives up.
    c.next += 1;
    let id = c.next;
    let sid = c.sid.clone();
    let out = c.send(
        json!({"id": id, "method": "Runtime.evaluate", "sessionId": sid,
        "params": {"expression": "new Promise(() => {})", "awaitPromise": true}}),
    );
    assert!(out.iter().all(|m| m["id"] != id), "{out:?}");
}

#[test]
fn errors_say_what_happened() {
    let session = Session::new(shop());
    let mut c = Cdp::new(&session);
    let r = c.page("Animation.enable", json!({}));
    assert_eq!(r["error"]["code"], -32601);
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .starts_with("'Animation.enable' wasn't found")
    );
    let r = c.page("Page.printToPDF", json!({}));
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Page.captureScreenshot"),
        "{r}"
    );
    // Screenshots work; formats pageon does not encode say so.
    let r = c.page("Page.captureScreenshot", json!({}));
    assert!(
        r["result"]["data"]
            .as_str()
            .is_some_and(|d| d.starts_with("iVBOR")),
        "{r}"
    );
    let r = c.page("Page.captureScreenshot", json!({"format": "webp"}));
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("png or jpeg"),
        "{r}"
    );
    let r = c.page("Network.setUserAgentOverride", json!({"userAgent": "x"}));
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("--profile browser-compatible"),
        "{r}"
    );
    let r = c.call(
        "Runtime.evaluate",
        json!({"expression": "1"}),
        Some("nope".into()),
    );
    assert_eq!(r["error"]["code"], -32001);
    let r = c.page(
        "Runtime.evaluate",
        json!({"expression": "1", "contextId": 999}),
    );
    assert_eq!(
        r["error"]["message"],
        "Cannot find context with specified id"
    );
    let r = c.page("Pageon.act", json!({"action": "click e9"}));
    assert_eq!(r["error"]["data"]["kind"], "unknown_ref", "{r}");
    let bad = c.send(json!("not an object"));
    assert!(bad.is_empty() || bad[0]["error"].is_object());
}

#[test]
fn sessions_are_deterministic() {
    let run = || {
        let session = Session::new(shop());
        let mut c = Cdp::new(&session);
        c.page("Runtime.enable", json!({}));
        c.page("Page.navigate", json!({"url": "https://shop.test/"}));
        let mut all = c.events.clone();
        all.push(c.page("Runtime.evaluate", json!({"expression": "document.body"})));
        all
    };
    assert_eq!(run(), run());
}

/// A running `pageon cdp`.
struct Server {
    child: Child,
    port: u16,
    /// The printed CDP WebSocket URL.
    ws: String,
}

impl Server {
    fn start(rec: &std::path::Path, extra: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pageon"))
            .args(["cdp", "--port", "0", "--replay"])
            .arg(rec)
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{e}"));
        let stderr = child.stderr.take().unwrap_or_else(|| panic!("stderr"));
        let mut lines = BufReader::new(stderr).lines();
        let (port, ws) = loop {
            let line = lines
                .next()
                .and_then(Result::ok)
                .unwrap_or_else(|| panic!("no DevTools line"));
            if let Some(url) = line.strip_prefix("DevTools listening on ") {
                let port = url
                    .trim_start_matches("ws://127.0.0.1:")
                    .split('/')
                    .next()
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0);
                break (port, url.to_string());
            }
        };
        std::thread::spawn(move || for _ in lines {});
        Self { child, port, ws }
    }

    fn http(&self, head: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap_or_else(|e| panic!("{e}"));
        s.write_all(head.as_bytes())
            .unwrap_or_else(|e| panic!("{e}"));
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn idle_connections_and_quoted_commands_cannot_stop_the_endpoint() {
    let rec = file("cdp-idle", &site(vec![html("https://a.test/", "<p>a</p>")]));
    let s = Server::start(&rec, &["--no-sandbox", "--discovery"]);
    // A connection that never sends its head does not block the next client.
    let _idle = TcpStream::connect(("127.0.0.1", s.port)).unwrap_or_else(|e| panic!("{e}"));
    let mut c = TcpStream::connect(("127.0.0.1", s.port)).unwrap_or_else(|e| panic!("{e}"));
    c.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap_or_else(|e| panic!("{e}"));
    c.write_all(b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .unwrap_or_else(|e| panic!("{e}"));
    let mut version = String::new();
    let _ = c.read_to_string(&mut version);
    assert!(version.starts_with("HTTP/1.1 200"), "{version:?}");
    let body: Value = serde_json::from_str(version.split("\r\n\r\n").nth(1).unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));
    let url = body["webSocketDebuggerUrl"].as_str().unwrap_or_default();
    let tcp = TcpStream::connect(("127.0.0.1", s.port)).unwrap_or_else(|e| panic!("{e}"));
    let (mut ws, _) = tungstenite::client(url, tcp).unwrap_or_else(|e| panic!("{e}"));
    let mut call = |msg: Value| {
        ws.send(tungstenite::Message::text(msg.to_string()))
            .unwrap_or_else(|e| panic!("{e}"));
        loop {
            match ws.read() {
                Ok(tungstenite::Message::Text(t)) => {
                    let v: Value = serde_json::from_str(&t).unwrap_or_else(|e| panic!("{e}"));
                    if v["id"] == msg["id"] {
                        return Some(v);
                    }
                }
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    };
    // The name of the close command inside a message is not the command.
    let quoted = json!({"id": 1, "method": "Browser.getVersion", "params": {"\"Browser.close\"": "Browser.close"}});
    assert!(quoted.to_string().contains("\"Browser.close\""));
    assert!(call(quoted).is_some());
    let alive = call(json!({"id": 2, "method": "Browser.getVersion"}));
    assert!(
        alive.is_some_and(|v| v["result"]["product"].is_string()),
        "the server ended"
    );
}

#[test]
fn the_endpoint_refuses_foreign_hosts_and_origins() {
    let rec = file(
        "cdp-endpoint",
        &site(vec![html("https://a.test/", "<p>a</p>")]),
    );
    for extra in [&["--discovery"][..], &["--no-sandbox", "--discovery"][..]] {
        let s = Server::start(&rec, extra);
        let version = s.http("GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        assert!(version.starts_with("HTTP/1.1 200"), "{version}");
        let body: Value =
            serde_json::from_str(version.split("\r\n\r\n").nth(1).unwrap_or_default())
                .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            body["Browser"]
                .as_str()
                .unwrap_or_default()
                .starts_with("pageon/")
        );
        let ws = body["webSocketDebuggerUrl"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            ws.starts_with(&format!("ws://127.0.0.1:{}/devtools/browser/", s.port)),
            "{ws}"
        );
        let list = s.http("GET /json/list HTTP/1.1\r\nHost: localhost\r\n\r\n");
        assert!(list.contains("\"type\":\"page\""), "{list}");
        let rebind = s.http("GET /json/version HTTP/1.1\r\nHost: evil.test\r\n\r\n");
        assert!(rebind.starts_with("HTTP/1.1 400"), "{rebind}");
        let path = ws
            .split_once(&format!("{}", s.port))
            .map(|(_, p)| p.to_string())
            .unwrap_or_default();
        let upgrade = |origin: &str| {
            s.http(&format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                 Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{origin}\r\n"
            ))
        };
        let foreign = upgrade("Origin: https://evil.test\r\n");
        assert!(foreign.starts_with("HTTP/1.1 403"), "{foreign}");
        assert!(
            foreign.contains("--allow-origin https://evil.test"),
            "{foreign}"
        );
        assert!(
            s.http("GET /nope HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
                .starts_with("HTTP/1.1 404")
        );
    }
}

#[test]
#[ignore = "timing; run in release"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn cdp_is_fast() {
    let Some(rec) = miniwob_log() else { return };
    let session = Session::new(Replay::new(rec));
    let t = std::time::Instant::now();
    let mut c = Cdp::new(&session);
    for m in ["Page.enable", "Runtime.enable", "Network.enable"] {
        c.page(m, json!({}));
    }
    c.page(
        "Page.navigate",
        json!({"url": format!("{MINIWOB_ORIGIN}miniwob/click-button.html")}),
    );
    let open = t.elapsed().as_secs_f64() * 1000.0;
    let r = c.eval("(() => { const b = document.querySelector('#area button').getBoundingClientRect(); return [b.x + b.width / 2, b.y + b.height / 2] })()");
    let (x, y) = (
        r["result"]["result"]["value"][0].as_f64().unwrap_or(0.0),
        r["result"]["result"]["value"][1].as_f64().unwrap_or(0.0),
    );
    let t = std::time::Instant::now();
    for _ in 0..10 {
        for ty in ["mouseMoved", "mousePressed", "mouseReleased"] {
            c.page(
                "Input.dispatchMouseEvent",
                json!({"type": ty, "x": x, "y": y, "button": "left", "clickCount": 1}),
            );
        }
    }
    let click = t.elapsed().as_secs_f64() * 100.0;
    println!("cdp: open {open:.1} ms, click {click:.1} ms (in-process)");
    assert!(open < 150.0, "open");
    assert!(click < 25.0, "click");
}

/// The web platform pieces Puppeteer needed, checked directly.
#[test]
fn observers_traversal_serializer_and_iterators() {
    let session = Session::new(Replay::new(site(vec![html(
        "https://w.test/",
        "<!DOCTYPE html><ul id=l><li>a</li><li class=x>b</li><!--c--><li>d</li></ul>\
         <p id=p title=t>text</p><div id=gone style=display:none>x</div><br>",
    )])));
    let mut c = Cdp::new(&session);
    c.page("Page.navigate", json!({"url": "https://w.test/"}));
    let check = |c: &mut Cdp<'_>, expr: &str, want: Value| {
        let r = c.eval(expr);
        assert_eq!(r["result"]["result"]["value"], want, "{expr}: {r}");
    };
    // MutationObserver: options, old values, filter, takeRecords, disconnect, errors.
    check(&mut c, "(() => {
        const out = [];
        const mo = new MutationObserver(() => {});
        mo.observe(p, { attributeOldValue: true, attributeFilter: ['title'], characterDataOldValue: true, subtree: true });
        p.title = 'u'; p.lang = 'en'; p.firstChild.data = 'new';
        for (const r of mo.takeRecords()) out.push([r.type, r.attributeName, r.oldValue].join(':'));
        mo.observe(l, { childList: true });
        l.appendChild(document.createElement('li')); l.removeChild(l.firstElementChild);
        for (const r of mo.takeRecords()) out.push(r.type + '+' + r.addedNodes.length + '-' + r.removedNodes.length);
        mo.disconnect(); l.appendChild(document.createElement('li'));
        out.push(mo.takeRecords().length);
        try { mo.observe(p, {}); } catch (e) { out.push(e.constructor.name); }
        return out;
    })()", json!(["attributes:title:t", "characterData::text", "childList+1-0", "childList+0-1", 0, "TypeError"]));
    check(
        &mut c,
        "new Promise(r => { new MutationObserver((rs, o) => r([rs.length, o instanceof MutationObserver])).observe(document.body, { childList: true }); document.body.append('x'); })",
        json!([1, true]),
    );
    // TreeWalker and NodeFilter.
    check(
        &mut c,
        "[typeof NodeFilter, NodeFilter.SHOW_ELEMENT, NodeFilter.SHOW_ALL, NodeFilter.prototype, (() => { try { NodeFilter(); return 'called' } catch (e) { return e.constructor.name } })()]",
        json!(["function", 1, 4_294_967_295_u64, null, "TypeError"]),
    );
    check(
        &mut c,
        "(() => { const w = document.createTreeWalker(l, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_COMMENT); const o = []; let n; while ((n = w.nextNode())) o.push(n.nodeName); return o; })()",
        // The observer check above removed the first item and appended two.
        json!(["LI", "#comment", "LI", "LI", "LI"]),
    );
    check(
        &mut c,
        "(() => { const w = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT, n => n.id === 'l' ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT); const o = []; let n; while ((n = w.nextNode())) o.push(n.id || n.nodeName); return o; })()",
        json!(["p", "gone", "BR"]),
    );
    check(
        &mut c,
        "(() => { const w = document.createTreeWalker(document, NodeFilter.SHOW_ELEMENT, { acceptNode: () => NodeFilter.FILTER_ACCEPT }); w.currentNode = p; return [w.parentNode().nodeName, w.previousNode().nodeName]; })()",
        json!(["BODY", "HEAD"]),
    );
    // XMLSerializer.
    check(
        &mut c,
        "[new XMLSerializer().serializeToString(document.doctype), new XMLSerializer().serializeToString(document.querySelector('br')), new XMLSerializer().serializeToString(p)]",
        json!([
            "<!DOCTYPE html>",
            "<br xmlns=\"http://www.w3.org/1999/xhtml\" />",
            "<p xmlns=\"http://www.w3.org/1999/xhtml\" id=\"p\" title=\"u\" lang=\"en\">new</p>"
        ]),
    );
    // IntersectionObserver against the viewport.
    check(
        &mut c,
        "new Promise(r => { const io = new IntersectionObserver(es => r(es.map(e => [e.target.id, e.isIntersecting, e.intersectionRatio]))); io.observe(p); io.observe(gone); })",
        json!([["p", true, 1], ["gone", false, 0]]),
    );
    check(
        &mut c,
        "(() => { try { new IntersectionObserver(() => {}, { threshold: 2 }); } catch (e) { return e.constructor.name } })()",
        json!("RangeError"),
    );
    // Web IDL iterators.
    check(
        &mut c,
        "[[...document.querySelectorAll('li')].length, [...l.childNodes].length, [...document.links].length, (() => { const o = []; l.children[0].classList.add('a', 'b'); for (const [i, t] of l.children[0].classList.entries()) o.push(i + t); return o; })()]",
        json!([4, 5, 0, ["0x", "1a", "2b"]]),
    );
}

/// Frames: tree, events, contexts, nodes, coordinates and input.
#[test]
fn frames_are_cdp_frames() {
    let session = Session::new(Replay::new(site(vec![
        html(
            "https://shop.test/",
            "<style>body{margin:0}</style><h1 style='margin:0;height:40px'>Shop</h1>\
             <iframe name=pay src=https://pay.test/card style='border:5px solid;padding:3px;width:300px;height:100px'></iframe>\
             <iframe name=gone src=https://nowhere.test/></iframe>",
        ),
        html(
            "https://pay.test/card",
            "<style>body{margin:0}</style><button id=b style='width:100px;height:20px' onclick=\"this.textContent='paid'\">Pay</button>\
             <script>console.log('card ready')</script>",
        ),
        html("https://shop.test/next", "<title>Next</title>"),
    ])));
    let mut c = Cdp::new(&session);
    for m in ["Page.enable", "Runtime.enable"] {
        c.page(m, json!({}));
    }
    c.page("Page.setLifecycleEventsEnabled", json!({"enabled": true}));
    c.events.clear();
    c.page("Page.navigate", json!({"url": "https://shop.test/"}));
    // Tree: main, the card frame, and the frame that failed (no contexts).
    let tree = c.page("Page.getFrameTree", json!({}))["result"]["frameTree"].clone();
    let kids = tree["childFrames"].as_array().cloned().unwrap_or_default();
    assert_eq!(kids.len(), 2, "{tree}");
    let pay = kids[0]["frame"].clone();
    assert_eq!(pay["url"], "https://pay.test/card");
    assert_eq!(pay["name"], "pay");
    assert_eq!(pay["parentId"], tree["frame"]["id"]);
    let pay_id = pay["id"].as_str().unwrap_or_default().to_string();
    // Events: the child frames are attached and navigated, each frame has its contexts and
    // its own lifecycle (with its own loader).
    let attached: Vec<&Value> = c
        .events
        .iter()
        .filter(|e| e["method"] == "Page.frameAttached")
        .collect();
    assert_eq!(attached.len(), 2);
    let contexts: Vec<&Value> = c
        .events
        .iter()
        .filter(|e| e["method"] == "Runtime.executionContextCreated")
        .collect();
    let pay_ctx = contexts
        .iter()
        .find(|e| e["params"]["context"]["auxData"]["frameId"] == pay_id.as_str())
        .map(|e| e["params"]["context"]["id"].clone())
        .unwrap_or_default();
    assert_eq!(
        contexts.len(),
        2,
        "main and card; the failed frame has none"
    );
    let loads: Vec<&Value> = c
        .events
        .iter()
        .filter(|e| e["method"] == "Page.lifecycleEvent" && e["params"]["name"] == "load")
        .collect();
    assert_eq!(loads.len(), 2);
    assert_ne!(
        loads[0]["params"]["loaderId"],
        loads[1]["params"]["loaderId"]
    );
    assert!(
        c.events
            .iter()
            .any(|e| e["method"] == "Runtime.consoleAPICalled"
                && e["params"]["args"][0]["value"] == "card ready"
                && e["params"]["executionContextId"] == pay_ctx),
        "the frame's console message, from its context"
    );
    // Evaluation in the frame's context.
    let origin = c.page(
        "Runtime.evaluate",
        json!({"expression": "location.origin", "contextId": pay_ctx, "returnByValue": true}),
    );
    assert_eq!(origin["result"]["result"]["value"], "https://pay.test");
    // The iframe element knows its frame and its document.
    let iframe = c.page(
        "Runtime.evaluate",
        json!({"expression": "document.querySelector('iframe')"}),
    );
    let oid = iframe["result"]["result"]["objectId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let node = c.page("DOM.describeNode", json!({"objectId": oid}))["result"]["node"].clone();
    assert_eq!(node["frameId"], pay_id.as_str());
    assert_eq!(node["contentDocument"]["nodeName"], "#document");
    let owner = c.page("DOM.getFrameOwner", json!({"frameId": pay_id}));
    assert_eq!(owner["result"]["backendNodeId"], node["backendNodeId"]);
    // Quads in top-level coordinates: the iframe's box (as the page measures it) plus its
    // border (5) and padding (3), plus the button's place in the frame (0, 0).
    let rect = c.eval("(() => { const r = document.querySelector('iframe').getBoundingClientRect(); return [r.x, r.y] })()");
    let (ix, iy) = (
        rect["result"]["result"]["value"][0]
            .as_f64()
            .unwrap_or(-1.0),
        rect["result"]["result"]["value"][1]
            .as_f64()
            .unwrap_or(-1.0),
    );
    let button = c.page(
        "Runtime.evaluate",
        json!({"expression": "document.querySelector('#b')", "contextId": pay_ctx}),
    );
    let boid = button["result"]["result"]["objectId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let quads = c.page("DOM.getContentQuads", json!({"objectId": boid}));
    let q = &quads["result"]["quads"][0];
    assert_eq!(
        (q[0].as_f64(), q[1].as_f64()),
        (Some(ix + 8.0), Some(iy + 8.0)),
        "{quads}"
    );
    assert_eq!(
        (q[4].as_f64(), q[5].as_f64()),
        (Some(ix + 108.0), Some(iy + 28.0)),
        "{quads}"
    );
    // A click inside that quad lands in the frame.
    for ty in ["mouseMoved", "mousePressed", "mouseReleased"] {
        c.page(
            "Input.dispatchMouseEvent",
            json!({"type": ty, "x": ix + 20.0, "y": iy + 15.0, "button": "left", "clickCount": 1}),
        );
    }
    let paid = c.page(
        "Runtime.evaluate",
        json!({"expression": "document.querySelector('#b').textContent", "contextId": pay_ctx, "returnByValue": true}),
    );
    assert_eq!(paid["result"]["result"]["value"], "paid");
    // A click on the frame that failed lands on its <iframe>.
    c.page("Runtime.evaluate", json!({"expression": "document.querySelectorAll('iframe')[1].onclick = e => document.title = 'iframe hit'"}));
    let gone = c.page("Runtime.evaluate", json!({"expression": "(() => { const r = document.querySelectorAll('iframe')[1].getBoundingClientRect(); return [r.x + 10, r.y + 10] })()", "returnByValue": true}));
    let (x, y) = (
        gone["result"]["result"]["value"][0].clone(),
        gone["result"]["result"]["value"][1].clone(),
    );
    for ty in ["mousePressed", "mouseReleased"] {
        c.page(
            "Input.dispatchMouseEvent",
            json!({"type": ty, "x": x, "y": y, "button": "left", "clickCount": 1}),
        );
    }
    assert_eq!(
        c.eval("document.title")["result"]["result"]["value"],
        "iframe hit"
    );
    // A navigation detaches the child frames before the new document is announced.
    c.events.clear();
    c.page("Page.navigate", json!({"url": "https://shop.test/next"}));
    let order: Vec<String> = c
        .events
        .iter()
        .filter_map(|e| e["method"].as_str())
        .filter(|m| m.starts_with("Page.frame"))
        .map(str::to_string)
        .collect();
    assert_eq!(
        order[..3],
        [
            "Page.frameDetached",
            "Page.frameDetached",
            "Page.frameStartedLoading"
        ],
        "{order:?}"
    );
    let stale = c.page(
        "Runtime.evaluate",
        json!({"expression": "1", "contextId": pay_ctx}),
    );
    assert_eq!(
        stale["error"]["message"],
        "Cannot find context with specified id"
    );
}

/// The page APIs Playwright's injected script needs, and select-then-type.
#[test]
fn cssom_view_members_and_field_selection() {
    let session = Session::new(Replay::new(site(vec![html(
        "https://v.test/",
        "<style>body{margin:0}</style><div id=a style='height:50px'>a</div>\
         <div id=none style=display:none>x</div><div id=clear style='opacity:0'>y</div>\
         <div id=hid style='visibility:hidden'>z</div><input id=f value=old>",
    )])));
    let mut c = Cdp::new(&session);
    c.page("Page.navigate", json!({"url": "https://v.test/"}));
    let check = |c: &mut Cdp<'_>, expr: &str, want: Value| {
        let r = c.eval(expr);
        assert_eq!(r["result"]["result"]["value"], want, "{expr}: {r}");
    };
    check(
        &mut c,
        "['a', 'none', 'clear', 'hid'].map(id => document.getElementById(id).checkVisibility())",
        json!([true, false, true, true]),
    );
    check(
        &mut c,
        "[clear.checkVisibility({ checkOpacity: true }), hid.checkVisibility({ checkVisibilityCSS: true })]",
        json!([false, false]),
    );
    check(
        &mut c,
        "[document.elementFromPoint(5, 5).id, document.elementsFromPoint(5, 5).map(e => e.localName), document.elementFromPoint(-1, 5), document.elementsFromPoint(99999, 5).length]",
        json!(["a", ["div", "body", "html"], null, 0]),
    );
    check(&mut c, "a.scrollIntoView() instanceof Promise", json!(true));
    // Playwright's fill: select the field, then type over it (or delete it).
    let field = c.page(
        "Runtime.evaluate",
        json!({"expression": "(f.focus(), f.select(), f)"}),
    );
    assert!(field["result"]["result"]["objectId"].is_string());
    c.page("Input.insertText", json!({"text": "new"}));
    check(&mut c, "f.value", json!("new"));
    c.page("Runtime.evaluate", json!({"expression": "f.select()"}));
    c.page(
        "Input.dispatchKeyEvent",
        json!({"type": "keyDown", "key": "Backspace"}),
    );
    check(&mut c, "f.value", json!(""));
}

#[test]
fn browsergym_observations_name_elements_by_ref() {
    // BrowserGym: bids are the agent's refs, accepted by `Pageon.act`; a page-authored `bid` is
    // not an element id; nothing is written into the page.
    let session = Session::new(Replay::new(site(vec![html(
        "https://shop.test/",
        "<button bid=evil>Decoy</button><button onclick=\"document.title='paid'\">Pay</button>",
    )])));
    let mut c = Cdp::new(&session);
    c.page("Page.navigate", json!({"url": "https://shop.test/"}));
    let r = c.page("Pageon.browsergym", json!({}));
    let obs = &r["result"];
    let nodes = obs["axtree_object"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let pay = nodes
        .iter()
        .find(|n| n["name"]["value"] == "Pay")
        .and_then(|n| n["browsergym_id"].as_str())
        .unwrap_or_else(|| panic!("{obs}"))
        .to_string();
    assert!(pay.starts_with('e'), "{pay}");
    assert!(obs["extra_element_properties"][&pay]["visibility"].as_f64() > Some(0.5));
    assert!(obs["pageon_ax"].as_str().unwrap_or_default().contains(&pay));
    let dom = &obs["dom_object"];
    let evil = dom["strings"]
        .as_array()
        .and_then(|s| s.iter().position(|x| x == "evil"))
        .map(|i| i as i64);
    let used = dom["documents"][0]["nodes"]["attributes"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|a| a.as_array().cloned().unwrap_or_default())
        .any(|v| v.as_i64() == evil);
    assert!(!used, "a page-authored bid is still an attribute: {dom}");
    let r = c.page("Pageon.act", json!({"action": format!("click {pay}")}));
    assert!(
        r["result"]["outcome"]
            .as_str()
            .is_some_and(|o| o.starts_with("ok click")),
        "{r}"
    );
    let r = c.eval("[document.title, document.querySelectorAll('[bid]').length]");
    assert_eq!(r["result"]["result"]["value"], json!(["paid", 1]));
}

/// Chrome's deep serialization: BiDi-shaped values for every type, depth
/// limits, repeated objects, nodes with backend ids, windows, and awaited promises.
#[test]
fn deep_serialization_is_bidi_shaped() {
    let session = Session::new(shop());
    let mut c = Cdp::new(&session);
    c.page("Page.navigate", json!({"url": "https://shop.test/"}));
    let deep = |c: &mut Cdp, expr: &str, opts: Value| {
        let mut so = json!({"serialization": "deep"});
        if let Value::Object(o) = opts {
            so.as_object_mut().unwrap().extend(o);
        }
        let r = c.page(
            "Runtime.evaluate",
            json!({"expression": expr, "awaitPromise": true, "serializationOptions": so}),
        );
        r["result"]["result"]["deepSerializedValue"].clone()
    };
    let cases = [
        ("undefined", json!({"type": "undefined"})),
        ("null", json!({"type": "null"})),
        ("'s'", json!({"type": "string", "value": "s"})),
        ("1.5", json!({"type": "number", "value": 1.5})),
        ("-0", json!({"type": "number", "value": "-0"})),
        ("NaN", json!({"type": "number", "value": "NaN"})),
        ("-Infinity", json!({"type": "number", "value": "-Infinity"})),
        ("true", json!({"type": "boolean", "value": true})),
        (
            "2n ** 70n",
            json!({"type": "bigint", "value": "1180591620717411303424"}),
        ),
        ("Symbol('x')", json!({"type": "symbol"})),
        (
            "[1, 'a']",
            json!({"type": "array", "value": [{"type": "number", "value": 1}, {"type": "string", "value": "a"}]}),
        ),
        (
            "({a: 1})",
            json!({"type": "object", "value": [["a", {"type": "number", "value": 1}]]}),
        ),
        (
            "new Map([[1, 2]])",
            json!({"type": "map", "value": [[{"type": "number", "value": 1}, {"type": "number", "value": 2}]]}),
        ),
        (
            "new Set(['x'])",
            json!({"type": "set", "value": [{"type": "string", "value": "x"}]}),
        ),
        (
            "/a+/gi",
            json!({"type": "regexp", "value": {"pattern": "a+", "flags": "gi"}}),
        ),
        (
            "new Date(0)",
            json!({"type": "date", "value": "1970-01-01T00:00:00.000Z"}),
        ),
        ("() => 1", json!({"type": "function"})),
        ("new Error('e')", json!({"type": "error"})),
        ("Promise.resolve(7)", json!({"type": "number", "value": 7})),
        ("new WeakMap()", json!({"type": "weakmap"})),
        ("new Uint8Array(2)", json!({"type": "typedarray"})),
        ("(function*(){})()", json!({"type": "generator"})),
    ];
    for (expr, want) in cases {
        assert_eq!(deep(&mut c, expr, json!({})), want, "{expr}");
    }
    // Depth: containers below maxDepth carry only their type.
    assert_eq!(
        deep(&mut c, "({a: {b: 1}})", json!({"maxDepth": 1})),
        json!({"type": "object", "value": [["a", {"type": "object"}]]})
    );
    // Repeated objects and cycles are referenced.
    let cyc = deep(
        &mut c,
        "(() => { const o = {}; o.self = o; return [o, o] })()",
        json!({}),
    );
    let first = &cyc["value"][0];
    assert_eq!(first["type"], "object");
    let r = first["weakLocalObjectReference"].clone();
    assert!(r.is_u64(), "{cyc}");
    assert_eq!(
        first["value"][0][1],
        json!({"type": "object", "weakLocalObjectReference": r})
    );
    assert_eq!(
        cyc["value"][1],
        json!({"type": "object", "weakLocalObjectReference": r})
    );
    // Nodes: properties, backend ids that resolve, and children to maxNodeDepth.
    let n = deep(&mut c, "document.getElementById('q')", json!({}));
    assert_eq!(n["type"], "node");
    assert_eq!(n["value"]["nodeType"], 1);
    assert_eq!(n["value"]["localName"], "input");
    assert_eq!(n["value"]["namespaceURI"], "http://www.w3.org/1999/xhtml");
    assert_eq!(n["value"]["attributes"]["id"], "q");
    assert!(n["value"].get("children").is_none());
    let backend = n["value"]["backendNodeId"].as_u64().unwrap();
    assert!(n["value"]["loaderId"].is_string());
    let resolved = c.page("DOM.resolveNode", json!({"backendNodeId": backend}));
    assert_eq!(resolved["result"]["object"]["subtype"], "node");
    let body = deep(
        &mut c,
        "document.body",
        json!({"additionalParameters": {"maxNodeDepth": 1}}),
    );
    let kids = body["value"]["children"].as_array().unwrap();
    assert_eq!(
        kids.len() as u64,
        body["value"]["childNodeCount"].as_u64().unwrap()
    );
    assert!(
        kids.iter()
            .all(|k| k["type"] == "node" && k["value"]["backendNodeId"].is_u64())
    );
    // The window names its frame.
    let w = deep(&mut c, "window", json!({}));
    assert_eq!(w["type"], "window");
    assert!(w["value"]["context"].is_string(), "{w}");
    // `json` and `idOnly` are returnByValue true and false.
    let r = c.page(
        "Runtime.evaluate",
        json!({"expression": "({a: 1})", "serializationOptions": {"serialization": "json"}}),
    );
    assert_eq!(r["result"]["result"]["value"], json!({"a": 1}));
    let r = c.page(
        "Runtime.evaluate",
        json!({"expression": "1", "serializationOptions": {"serialization": "nope"}}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
}

#[test]
fn isolated_worlds_are_separate_javascript_over_one_document() {
    let env = Replay::new(site(vec![html(
        "https://shop.test/",
        "<p id=t>real</p><script>Array.prototype.map = () => ['poisoned'];\
         Object.defineProperty(Node.prototype, 'textContent', {get() { return 'lie' }});</script>",
    )]));
    let session = Session::new(env);
    let mut c = Cdp::new(&session);
    c.page("Page.navigate", json!({"url": "https://shop.test/"}));
    let frame = c.page("Page.getFrameTree", json!({}))["result"]["frameTree"]["frame"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let world = c.page(
        "Page.createIsolatedWorld",
        json!({"frameId": frame, "worldName": "__puppeteer_utility_world__"}),
    )["result"]["executionContextId"]
        .clone();
    let value = |c: &mut Cdp<'_>, ctx: Option<&Value>, expr: &str| {
        let mut p = json!({"expression": expr, "returnByValue": true});
        if let Some(ctx) = ctx {
            p["contextId"] = ctx.clone();
        }
        c.page("Runtime.evaluate", p)["result"]["result"]["value"].clone()
    };
    let q = "[[1, 2].map((x) => x * 2), document.getElementById('t').textContent]";
    assert_eq!(value(&mut c, None, q), json!([["poisoned"], "lie"]));
    assert_eq!(value(&mut c, Some(&world), q), json!([[2, 4], "real"]));
    // An object of the main world is not passed into the isolated world.
    let main_obj = c.page("Runtime.evaluate", json!({"expression": "document.body"}))["result"]["result"]["objectId"].clone();
    let refused = c.page(
        "Runtime.callFunctionOn",
        json!({"functionDeclaration": "(b) => b.tagName", "executionContextId": world, "arguments": [{"objectId": main_obj}]}),
    );
    assert!(
        refused["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("same JavaScript world")),
        "{refused}"
    );
}

#[test]
fn history_api_navigations_stay_in_the_document() {
    let env = Replay::new(site(vec![html(
        "https://spa.test/",
        "<title>SPA</title><script>window.go = (p) => history.pushState({p}, '', p)</script>",
    )]));
    let session = Session::new(env);
    let mut c = Cdp::new(&session);
    for m in ["Page.enable", "Runtime.enable"] {
        c.page(m, json!({}));
    }
    c.page("Page.navigate", json!({"url": "https://spa.test/"}));
    c.events.clear();
    c.eval("go('/orders')");
    let within: Vec<&Value> = c
        .events
        .iter()
        .filter(|e| e["method"] == "Page.navigatedWithinDocument")
        .collect();
    assert_eq!(within.len(), 1, "{:#?}", c.events);
    assert_eq!(within[0]["params"]["url"], "https://spa.test/orders");
    assert_eq!(within[0]["params"]["navigationType"], "historyApi");
    assert!(
        !c.events
            .iter()
            .any(|e| e["method"] == "Runtime.executionContextsCleared"
                || e["method"] == "Page.frameNavigated"),
        "the document stays: {:#?}",
        c.events
    );
    c.events.clear();
    c.eval("location.hash = 'x'");
    let frag = c
        .events
        .iter()
        .find(|e| e["method"] == "Page.navigatedWithinDocument");
    assert_eq!(
        frag.map(|e| e["params"]["navigationType"].clone()),
        Some(json!("fragment"))
    );
    let h = c.page("Page.getNavigationHistory", json!({}));
    assert_eq!(h["result"]["currentIndex"], 2, "{h}");
}

/// P-11: only the printed URL, with its per-run secret, reaches the browser;
/// discovery over HTTP is opt-in.
#[test]
fn the_endpoint_answers_only_its_printed_url() {
    let rec = file(
        "cdp-token",
        &site(vec![html("https://a.test/", "<p>a</p>")]),
    );
    let s = Server::start(&rec, &["--no-sandbox"]);
    let id = s.ws.rsplit('/').next().unwrap_or_default().to_string();
    assert_eq!(id.len(), 32, "128 random bits: {}", s.ws);
    assert_ne!(id, pageon::cdp::server::browser_id());
    for path in ["/json/version", "/json/list", "/json"] {
        let r = s.http(&format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"));
        assert!(r.starts_with("HTTP/1.1 404"), "{path}: {r}");
        assert!(r.contains("--discovery"), "{r}");
    }
    let guesses = [
        "/devtools/browser/".to_string(),
        format!("/devtools/browser/{}", pageon::cdp::server::browser_id()),
        "/session".to_string(),
        format!("/session/{}", "0".repeat(32)),
    ];
    for path in &guesses {
        let r = s.http(&format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ));
        assert!(r.starts_with("HTTP/1.1 404"), "{path}: {r}");
    }
    let tcp = TcpStream::connect(("127.0.0.1", s.port)).unwrap_or_else(|e| panic!("{e}"));
    let (mut ws, _) = tungstenite::client(s.ws.as_str(), tcp).unwrap_or_else(|e| panic!("{e}"));
    ws.send(tungstenite::Message::text(
        json!({"id": 1, "method": "Browser.getVersion"}).to_string(),
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    let reply = loop {
        if let Ok(tungstenite::Message::Text(t)) = ws.read() {
            let v: Value = serde_json::from_str(&t).unwrap_or_else(|e| panic!("{e}"));
            if v["id"] == 1 {
                break v;
            }
        }
    };
    assert!(reply["result"]["product"].is_string(), "{reply}");
}

#[test]
fn allowing_every_origin_warns() {
    let out = Command::new(env!("CARGO_BIN_EXE_pageon"))
        .args(["cdp", "--port", "1", "--allow-origin", "*"])
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("warning: --allow-origin '*'"), "{err}");
}

/// Spec 033: init scripts run before the page's own scripts, in every new document and frame,
/// script-less ones too; world scripts in their world; removal stops them.
#[test]
fn init_scripts_run_before_the_page() {
    let session = Session::new(Replay::new(site(vec![
        html(
            "https://init.test/",
            "<p id=seen></p><iframe src=/frame></iframe><script>seen.textContent = 'first script sees ' + window.stub</script>",
        ),
        html("https://init.test/frame", "<p>frame</p>"),
        html("https://init.test/static", "<p>no scripts</p>"),
        html("https://init.test/after", "<p>after</p>"),
    ])));
    let mut c = Cdp::new(&session);
    c.page("Page.enable", json!({}));
    c.page("Runtime.enable", json!({}));
    let r = c.page(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": "window.stub = 'stubbed'; window.runs = (window.runs || 0) + 1;"}),
    );
    let main = r["result"]["identifier"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(!main.is_empty(), "{r}");
    c.page(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": "throw new Error('init boom')"}),
    );
    c.page(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": "window.helper = 'world'", "worldName": "helpers"}),
    );
    c.page("Page.navigate", json!({"url": "https://init.test/"}));
    let seen = c.eval("document.getElementById('seen').textContent");
    assert_eq!(
        seen["result"]["result"]["value"], "first script sees stubbed",
        "{seen}"
    );
    let frame = c.eval("document.querySelector('iframe').contentWindow.stub");
    assert_eq!(
        frame["result"]["result"]["value"], "stubbed",
        "the frame too: {frame}"
    );
    assert_eq!(
        c.eval("window.helper === undefined")["result"]["result"]["value"],
        true,
        "worlds stay apart"
    );
    // The world's context has the helper.
    let world = c
        .events
        .iter()
        .filter(|e| e["method"] == "Runtime.executionContextCreated")
        .filter(|e| e["params"]["context"]["name"] == "helpers")
        .filter_map(|e| e["params"]["context"]["id"].as_i64())
        .next_back()
        .unwrap_or_else(|| panic!("no helpers context"));
    let h = c.page(
        "Runtime.evaluate",
        json!({"expression": "window.helper", "contextId": world, "returnByValue": true}),
    );
    assert_eq!(h["result"]["result"]["value"], "world", "{h}");
    c.page("Page.navigate", json!({"url": "https://init.test/static"}));
    assert_eq!(
        c.eval("window.stub")["result"]["result"]["value"],
        "stubbed",
        "script-less pages too"
    );
    c.page(
        "Page.removeScriptToEvaluateOnNewDocument",
        json!({"identifier": main}),
    );
    c.page("Page.navigate", json!({"url": "https://init.test/after"}));
    assert_eq!(
        c.eval("typeof window.stub")["result"]["result"]["value"],
        "undefined"
    );
}
