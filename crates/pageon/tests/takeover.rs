//! Human takeover: a scripted person logs in through the live view; the
//! view refuses everyone else.

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::*;
use pageon::takeover::Ended;
use pageon::{Format, LoadOptions, Recording, Replay, Session};

/// One HTTP/1.1 request: (status, raw head, body).
fn http(url: &str, method: &str, headers: &[(&str, &str)], body: &str) -> (u16, String, Vec<u8>) {
    let rest = url.strip_prefix("http://").unwrap_or(url);
    let (host, path) = rest
        .split_once('/')
        .map_or((rest, "/".to_string()), |(h, p)| (h, format!("/{p}")));
    let mut s = TcpStream::connect(host).unwrap_or_else(|e| panic!("{url}: {e}"));
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str(&format!("Host: {host}\r\n"));
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes())
        .unwrap_or_else(|e| panic!("{e}"));
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap_or_else(|e| panic!("{e}"));
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(out.len());
    let head = String::from_utf8_lossy(&out[..split]).to_string();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (
        status,
        head,
        out.get(split + 4..).unwrap_or_default().to_vec(),
    )
}

fn login_site() -> pageon_env::Log {
    site(vec![
        html(
            "https://shop.test/login",
            r#"<body style="margin:0"><form action=/session method=post>
            <input name=user aria-label=User style="position:absolute;left:10px;top:10px">
            <button style="position:absolute;left:10px;top:60px">Sign in</button></form>
            <div style="height:3000px"></div>"#,
        ),
        ex(
            "POST",
            "https://shop.test/session",
            200,
            &[("content-type", "text/html")],
            "<h1>Welcome</h1>",
        ),
    ])
}

#[test]
#[allow(clippy::disallowed_methods)] // the first frame is timed in wall time on purpose
fn a_person_logs_in_through_the_live_view() {
    let rec = Recording::new(Replay::new(login_site()));
    let session = Session::new(&rec);
    let mut tab = session
        .open("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    let steps = rec.log().steps.len();
    let centre = |r: &str, tab: &mut pageon::Tab<'_, &Recording<Replay>>| {
        let p = tab
            .crop(r, 0.0, Format::Png, 1.0)
            .unwrap_or_else(|e| panic!("{e}"))
            .provenance
            .unwrap_or_else(|| panic!("provenance"));
        (
            p.rect.x + p.rect.width / 2.0,
            p.rect.y + p.rect.height / 2.0,
        )
    };
    let field = centre(&find(&obs, "textbox", "User"), &mut tab);
    let button = centre(&find(&obs, "button", "Sign in"), &mut tab);

    let (tx, rx) = mpsc::channel::<String>();
    let human = std::thread::spawn(move || {
        let url = rx.recv().unwrap_or_default();
        let base = url.trim_end_matches('/').to_string();
        let host = base
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or_default()
            .to_string();
        // Strangers are refused.
        let wrong = format!("http://{host}/t/{}", "0".repeat(32));
        assert_eq!(http(&wrong, "GET", &[], "").0, 404, "wrong token");
        assert_eq!(
            http(&base, "GET", &[("Host", "evil.test")], "").0,
            400,
            "foreign Host"
        );
        assert_eq!(
            http(&format!("{base}/frame.png"), "GET", &[], "").0,
            409,
            "no viewer yet"
        );
        // The person opens the page and sees the tab.
        let t = Instant::now();
        let (status, head, page) = http(&base, "GET", &[], "");
        assert_eq!(status, 200);
        let page = String::from_utf8_lossy(&page);
        assert!(
            page.contains("Its words, unverified: “2FA code needed”"),
            "{page}"
        );
        let cookie = head
            .lines()
            .find_map(|l| l.strip_prefix("Set-Cookie: "))
            .and_then(|c| c.split(';').next())
            .unwrap_or_default()
            .to_string();
        let me = [("Cookie", cookie.as_str())];
        let (status, _, png) = http(&format!("{base}/frame.png"), "GET", &me, "");
        let first = t.elapsed();
        assert_eq!(status, 200);
        assert!(png.starts_with(b"\x89PNG"));
        println!("first frame in {first:?}");
        // is a release figure (debug builds paint several times slower).
        if !cfg!(debug_assertions) {
            assert!(
                first < Duration::from_millis(200),
                "first frame in {first:?}"
            );
        }
        // A second viewer, and a cross-site post, are refused.
        assert_eq!(http(&base, "GET", &[], "").0, 409, "second viewer");
        let evil = [("Cookie", cookie.as_str()), ("Origin", "https://evil.test")];
        assert_eq!(
            http(&format!("{base}/input"), "POST", &evil, r#"{"text":"x"}"#).0,
            403
        );
        // The person scrolls down and back, clicks the field, types, and signs in.
        let input = |body: String| http(&format!("{base}/input"), "POST", &me, &body).0;
        assert_eq!(input(r#"{"wheel":[0,600]}"#.into()), 204);
        assert_eq!(input(r#"{"wheel":[0,-600]}"#.into()), 204);
        assert_eq!(
            input(format!(r#"{{"click":[{},{}]}}"#, field.0, field.1)),
            204
        );
        assert_eq!(input(r#"{"text":"ada"}"#.into()), 204);
        assert_eq!(
            input(format!(r#"{{"click":[{},{}]}}"#, button.0, button.1)),
            204
        );
        assert_eq!(http(&format!("{base}/done"), "POST", &me, "").0, 204);
        // After Done, the URL is gone.
        assert!(TcpStream::connect(&host).is_err() || http(&base, "GET", &me, "").0 != 200);
    });
    let mut ready = |url: &str| {
        let _ = tx.send(url.to_string());
    };
    let t = tab
        .takeover("2FA code needed", Duration::from_secs(30), &mut ready)
        .unwrap_or_else(|e| panic!("{e}"));
    human
        .join()
        .unwrap_or_else(|_| panic!("the scripted person failed"));
    assert_eq!(t.ended, Ended::Done);
    assert_eq!(t.inputs, 7, "two wheels, a click, three keys, a click");
    let after = tab.observe();
    assert!(after.contains("\"Welcome\""), "{after}");
    assert_eq!(tab.url(), "https://shop.test/session");
    // The person's input is theirs: the agent sees the page as it is now, not what was typed,
    // and the recording keeps the page's requests but no steps for it.
    for line in after.lines() {
        assert!(
            !line.starts_with("ok ")
                && !line.contains("press")
                && !line.contains("typed")
                && !line.contains("scrolled"),
            "an outcome line of the person's input: {line}\n{after}"
        );
    }
    let log = rec.log();
    assert_eq!(
        log.steps.len(),
        steps,
        "takeover input was recorded as steps"
    );
    assert!(
        log.exchanges
            .iter()
            .any(|e| e.method == "POST" && e.url == "https://shop.test/session"),
        "the page's own request is recorded"
    );
}

#[test]
fn nobody_comes_and_it_times_out() {
    let session = Session::new(Replay::new(login_site()));
    let mut tab = session
        .open("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let mut url = String::new();
    let t = tab
        .takeover("login", Duration::from_millis(300), &mut |u| {
            url = u.to_string()
        })
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(t.ended, Ended::TimedOut);
    assert!(
        url.starts_with("http://127.0.0.1:") && url.contains("/t/"),
        "{url}"
    );
    let host = url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(TcpStream::connect(host).is_err(), "the view is closed");
}

/// P-12: a client that trickles its request a byte a second cannot hold the view; the
/// person behind it is served within the request deadline (5 s).
#[test]
#[allow(clippy::disallowed_methods)] // wall time: the view must stay responsive
fn a_trickling_client_cannot_hold_the_view() {
    let session = Session::new(Replay::new(login_site()));
    let mut tab = session
        .open("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let (tx, rx) = mpsc::channel::<String>();
    let person = std::thread::spawn(move || {
        let url = rx.recv().unwrap_or_else(|e| panic!("{e}"));
        let host = url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or_default()
            .to_string();
        // The slow client: a byte a second, for longer than the deadline.
        let slow_host = host.clone();
        let slow = std::thread::spawn(move || {
            let mut s = TcpStream::connect(&slow_host).unwrap_or_else(|e| panic!("{e}"));
            for b in b"GET / HTTP/1.1\r\nX-Slow: 1".iter() {
                if s.write_all(&[*b]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1000));
            }
        });
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        let (status, _, _) = http(&url, "GET", &[], "");
        let waited = started.elapsed();
        assert_eq!(status, 200);
        assert!(waited < Duration::from_secs(7), "{waited:?}");
        drop(slow);
    });
    let mut ready = |url: &str| {
        let _ = tx.send(url.to_string());
    };
    let _ = tab
        .takeover("slow", Duration::from_secs(8), &mut ready)
        .unwrap_or_else(|e| panic!("{e}"));
    person
        .join()
        .unwrap_or_else(|_| panic!("the person was not served in time"));
}
