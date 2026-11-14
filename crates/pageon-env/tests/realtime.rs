//! Realtime connections: live over loopback, recorded, then replayed offline.

#![allow(clippy::disallowed_methods)] // a test server in real time

use std::io::{Read, Write};
use std::net::TcpListener;

use pageon_env::{
    Env, Live, NetError, Recording, Replay, Request, StreamCall, StreamEvent, StreamKind,
    StreamReply,
};

/// A WebSocket server: greets, then echoes each text message once.
fn echo_server() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = l.local_addr().map(|a| a.port()).unwrap_or_default();
    std::thread::spawn(move || {
        let Ok((s, _)) = l.accept() else { return };
        let Ok(mut ws) = tungstenite::accept(s) else {
            return;
        };
        let _ = ws.send(tungstenite::Message::text("hello"));
        while let Ok(m) = ws.read() {
            match m {
                tungstenite::Message::Text(t) => {
                    let _ = ws.send(tungstenite::Message::text(format!("echo: {t}")));
                }
                tungstenite::Message::Close(_) => {
                    let _ = ws.flush();
                }
                _ => {}
            }
        }
    });
    format!("ws://127.0.0.1:{port}/chat")
}

fn open(env: &dyn Env, url: &str, allow_private: bool, kind: StreamKind) -> Result<u64, NetError> {
    let mut req = Request::get(url);
    req.allow_private = allow_private;
    match env.stream(&StreamCall::Open { req, kind })? {
        StreamReply::Opened { id, .. } => Ok(id),
        r => panic!("{r:?}"),
    }
}

fn poll(env: &dyn Env, now: u64, wait_ms: u64) -> Vec<(u64, StreamEvent)> {
    match env.stream(&StreamCall::Poll { now, wait_ms }) {
        Ok(StreamReply::Events(e)) => e,
        r => panic!("{r:?}"),
    }
}

fn send(env: &dyn Env, id: u64, url: &str, text: &str) {
    let mut req = Request::get(url);
    req.method = "SEND".into();
    req.body = text.as_bytes().to_vec();
    env.stream(&StreamCall::Send {
        id,
        req,
        text: true,
    })
    .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn websocket_live_recorded_and_replayed() {
    let url = echo_server();
    let ws = || StreamKind::WebSocket { protocols: vec![] };
    let rec = Recording::new(Live::default());
    // The private network is refused unless allowed.
    assert!(matches!(
        open(&rec, &url, false, ws()),
        Err(NetError::Blocked(_))
    ));
    let id = open(&rec, &url, true, ws()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        poll(&rec, 1000, 5000),
        vec![(id, StreamEvent::Text("hello".into()))]
    );
    send(&rec, id, &url, "hi");
    assert_eq!(
        poll(&rec, 2000, 5000),
        vec![(id, StreamEvent::Text("echo: hi".into()))]
    );
    rec.stream(&StreamCall::Close {
        id,
        code: 1000,
        reason: "bye".into(),
    })
    .unwrap_or_else(|e| panic!("{e}"));
    let log = rec.log();
    assert_eq!(log.realtime.len(), 5, "{:?}", log.realtime);
    // Replay: the same messages at the same virtual times, offline.
    let replay = Replay::new(log);
    let id2 = open(&replay, &url, false, ws()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(id2, id);
    assert!(poll(&replay, 999, 0).is_empty());
    assert_eq!(
        poll(&replay, 1000, 0),
        vec![(id, StreamEvent::Text("hello".into()))]
    );
    send(&replay, id, &url, "hi");
    assert_eq!(
        poll(&replay, 5000, 0),
        vec![(id, StreamEvent::Text("echo: hi".into()))]
    );
    assert!(poll(&replay, 9000, 0).is_empty());
    // A connection the recording lacks.
    assert!(matches!(
        open(&replay, &url, false, ws()),
        Err(NetError::ReplayMiss(_))
    ));
}

#[test]
fn event_stream_live() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = l.local_addr().map(|a| a.port()).unwrap_or_default();
    std::thread::spawn(move || {
        let Ok((mut s, _)) = l.accept() else { return };
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: one\n\n");
        let _ = s.flush();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let _ = s.write_all(b"event: tick\ndata: two\n\n");
    });
    let live = Live::default();
    let url = format!("http://127.0.0.1:{port}/events");
    let id = open(&live, &url, true, StreamKind::EventSource).unwrap_or_else(|e| panic!("{e}"));
    let mut body = Vec::new();
    let mut closed = false;
    for _ in 0..50 {
        for (i, e) in poll(&live, 0, 200) {
            assert_eq!(i, id);
            match e {
                StreamEvent::Binary(b) => body.extend(b),
                StreamEvent::Closed { clean, .. } => {
                    assert!(clean);
                    closed = true;
                }
                StreamEvent::Text(t) => panic!("{t}"),
            }
        }
        if closed {
            break;
        }
    }
    assert!(closed);
    assert_eq!(
        String::from_utf8_lossy(&body),
        "data: one\n\nevent: tick\ndata: two\n\n"
    );
}

#[test]
fn connections_and_messages_are_bounded() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = l.local_addr().map(|a| a.port()).unwrap_or_default();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::spawn(move || {
                let Ok(mut ws) = tungstenite::accept(s) else {
                    return;
                };
                // The first message asks for a large one.
                if let Ok(tungstenite::Message::Text(t)) = ws.read()
                    && t.as_str() == "big"
                {
                    let _ = ws.send(tungstenite::Message::binary(vec![0u8; 5 << 20]));
                }
                while ws.read().is_ok() {}
            });
        }
    });
    let url = format!("ws://127.0.0.1:{port}/");
    let live = Live::default();
    let ws = || StreamKind::WebSocket { protocols: vec![] };
    let ids: Vec<u64> = (0..pageon_env::MAX_STREAMS)
        .map(|_| open(&live, &url, true, ws()).unwrap_or_else(|e| panic!("{e}")))
        .collect();
    match open(&live, &url, true, ws()) {
        Err(NetError::Blocked(m)) => assert!(m.contains("16 realtime connections"), "{m}"),
        r => panic!("{r:?}"),
    }
    // Over 4 MiB: the connection closes with 1009.
    send(&live, ids[0], &url, "big");
    let mut closed = None;
    for _ in 0..50 {
        for (id, e) in poll(&live, 0, 200) {
            if let (true, StreamEvent::Closed { code, .. }) = (id == ids[0], e) {
                closed = Some(code);
            }
        }
        if closed.is_some() {
            break;
        }
    }
    assert_eq!(closed, Some(1009));
    // A closed connection frees its place.
    assert!(open(&live, &url, true, ws()).is_ok());
}
