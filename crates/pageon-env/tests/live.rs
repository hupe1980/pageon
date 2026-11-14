//! The live transport against local servers: the private network is refused by default (as
//! URL literals and as DNS answers), allowed per request, and proxies are used.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use pageon_env::{Env, Live, NetError, Request};

/// Serves one HTTP/1.1 response whose body is the request line, and returns the port.
fn echo_once() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = l.local_addr().map(|a| a.port()).unwrap_or_default();
    std::thread::spawn(move || {
        let Ok((s, _)) = l.accept() else { return };
        let mut r = BufReader::new(s);
        let mut line = String::new();
        let _ = r.read_line(&mut line);
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).map_or(true, |n| n == 0) || h == "\r\n" {
                break;
            }
        }
        let body = line.trim_end();
        let mut s = r.into_inner();
        let _ = write!(
            s,
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    port
}

fn allowed(url: &str) -> Request {
    Request {
        allow_private: true,
        ..Request::get(url)
    }
}

#[test]
fn private_addresses_are_refused_before_connecting() {
    let live = Live::default();
    for url in [
        "http://127.0.0.1:9/",
        "http://localhost:9/",
        "http://[::1]:9/",
        "http://169.254.169.254/latest/",
        "http://0x7f.1:9/",
    ] {
        match live.fetch(&Request::get(url)) {
            Err(NetError::Blocked(why)) => assert!(why.contains("private network"), "{why}"),
            other => panic!("{url}: {other:?}"),
        }
    }
}

#[test]
fn the_request_may_allow_the_private_network() {
    let port = echo_once();
    let live = Live::default();
    let r = live
        .fetch(&allowed(&format!("http://127.0.0.1:{port}/hello")))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.status, 200);
    assert_eq!(String::from_utf8_lossy(&r.body), "GET /hello HTTP/1.1");
}

#[test]
fn dns_answers_on_the_private_network_are_refused() {
    // `localhost` is refused from the URL already; the resolver is checked on its own here
    // (a public name that resolves or rebinds to a private address takes this path).
    let live = Live::default();
    let err = live
        .resolve_public("localhost")
        .err()
        .unwrap_or_else(|| panic!("localhost must not resolve to a public address"));
    assert!(err.contains("private network address"), "{err}");
}

#[test]
fn requests_go_through_the_proxy() {
    let port = echo_once();
    let live = Live::default()
        .with_proxy(&format!("http://127.0.0.1:{port}"))
        .unwrap_or_else(|e| panic!("{e}"));
    // The proxy (an operator setting) may be local; the destination is public.
    let r = live
        .fetch(&Request::get("http://example.test/via-proxy"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        String::from_utf8_lossy(&r.body),
        "GET http://example.test/via-proxy HTTP/1.1"
    );
}

#[test]
fn bodies_and_methods_are_sent() {
    let port = echo_once();
    let r = Live::default()
        .fetch(&Request {
            method: "PUT".into(),
            body: b"x=1".to_vec(),
            ..allowed(&format!("http://127.0.0.1:{port}/item"))
        })
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(String::from_utf8_lossy(&r.body), "PUT /item HTTP/1.1");
}

#[test]
#[ignore = "network"]
fn https_smoke() {
    // HTTP/2 is negotiated by ALPN where the server offers it.
    let r = Live::default()
        .fetch(&Request::get("https://www.example.com/"))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.status, 200);
}
