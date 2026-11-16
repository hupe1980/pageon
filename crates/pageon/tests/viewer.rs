//! The viewer: steps, images, pause and approvals over its loopback HTTP API, with
//! `pageon mcp --approve` caged and in-process, and `pageon open --ui`.

use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

const SECRET: &str = "s3cret-viewer-value-77";

/// A page server: a login form that posts, and an answer page. It counts the POSTs it got.
fn serve() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let origin = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().map(|a| a.port()).unwrap_or(0)
    );
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut s = stream;
            let mut buf = vec![0u8; 16384];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = if req.starts_with("GET / ") {
                "<title>Pay</title><form method=post action=/pay><label>Card <input type=password name=card></label><button>Pay now <img src=x onerror=alert(1)></button></form>"
            } else {
                "<title>Paid</title><p>Thank you"
            };
            if req.starts_with("POST")
                && let Ok(mut l) = log.lock()
            {
                l.push(req);
            }
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (origin, seen)
}

/// `pageon mcp` over stdio with the viewer; replies arrive on a channel.
struct Mcp {
    child: Child,
    input: ChildStdin,
    replies: Receiver<Value>,
    next: u64,
    url: String,
}

impl Mcp {
    fn start(origin: &str, caged: bool) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pageon"));
        cmd.args([
            "mcp",
            "--approve",
            "--origin",
            origin,
            "--private-network",
            "allow",
            "--robots",
            "ignore",
            "--secret",
            &format!("card@{origin}"),
        ]);
        if !caged {
            cmd.arg("--no-sandbox");
        }
        let mut child = cmd
            .env("PAGEON_SECRET_CARD", SECRET)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{e}"));
        let input = child.stdin.take().unwrap_or_else(|| panic!("stdin"));
        let out = child.stdout.take().unwrap_or_else(|| panic!("stdout"));
        let err = child.stderr.take().unwrap_or_else(|| panic!("stderr"));
        let (tx, replies) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && v.get("id").is_some()
                {
                    let _ = tx.send(v);
                }
            }
        });
        let mut err = BufReader::new(err);
        let mut url = String::new();
        let mut line = String::new();
        while err.read_line(&mut line).unwrap_or(0) > 0 {
            if let Some(u) = line.trim().strip_prefix("viewer: ") {
                url = u.to_string();
                break;
            }
            line.clear();
        }
        std::thread::spawn(move || {
            let mut sink = String::new();
            let _ = err.read_to_string(&mut sink);
        });
        assert!(
            url.starts_with("http://127.0.0.1:"),
            "no viewer URL: {line}"
        );
        Self {
            child,
            input,
            replies,
            next: 1,
            url,
        }
    }

    /// Sends a tool call; the reply comes through `reply`.
    fn send(&mut self, tool: &str, args: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
            "name": tool, "arguments": args,
            "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                      "io.modelcontextprotocol/clientCapabilities": {}}}});
        let _ = writeln!(self.input, "{msg}");
        let _ = self.input.flush();
        id
    }

    fn reply(&self, wait: Duration) -> Option<String> {
        let v = self.replies.recv_timeout(wait).ok()?;
        Some(
            v.pointer("/result/content/0/text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        )
    }

    fn call(&mut self, tool: &str, args: Value) -> String {
        self.send(tool, args);
        self.reply(Duration::from_secs(60))
            .unwrap_or_else(|| panic!("no reply to {tool}"))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A browser of the viewer: its cookie and the page token.
struct Browser {
    port: u16,
    path: String,
    cookie: String,
    token: String,
}

fn http(port: u16, raw: &str) -> (u16, String, Vec<u8>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap_or_else(|e| panic!("{e}"));
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    s.write_all(raw.as_bytes())
        .unwrap_or_else(|e| panic!("{e}"));
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = buf.get(split + 4..).unwrap_or_default().to_vec();
    (status, head, body)
}

impl Browser {
    fn open(url: &str) -> Self {
        let rest = url.strip_prefix("http://127.0.0.1:").unwrap_or_default();
        let (port, path) = rest.split_once('/').unwrap_or_default();
        let port: u16 = port.parse().unwrap_or(0);
        let path = format!("/{path}");
        let (status, head, body) = http(
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
        );
        assert_eq!(status, 200, "{head}");
        assert!(head.contains("HttpOnly; SameSite=Strict"), "{head}");
        assert!(head.contains("script-src 'self'"), "{head}");
        assert!(!head.contains("unsafe-inline"), "{head}");
        let cookie = head
            .split("pageon_viewer=")
            .nth(1)
            .and_then(|s| s.split(';').next())
            .unwrap_or_default()
            .to_string();
        let html = String::from_utf8_lossy(&body);
        assert!(!html.contains("<script>"), "inline script in the page");
        let token = html
            .split("name=\"pageon-token\" content=\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap_or_default()
            .to_string();
        Self {
            port,
            path,
            cookie,
            token,
        }
    }

    fn get(&self, rest: &str) -> (u16, Vec<u8>) {
        let (s, _, b) = http(
            self.port,
            &format!(
                "GET {}{rest} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: pageon_viewer={}\r\n\r\n",
                self.path, self.port, self.cookie
            ),
        );
        (s, b)
    }

    fn post(&self, rest: &str, body: &str, token: &str, origin: Option<&str>) -> u16 {
        let origin = origin
            .map(|o| format!("Origin: {o}\r\n"))
            .unwrap_or_default();
        http(
            self.port,
            &format!(
                "POST {}{rest} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: pageon_viewer={}\r\nX-Pageon-Token: {token}\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                self.path, self.port, self.cookie, body.len()
            ),
        )
        .0
    }

    fn decide(&self, id: u64, verdict: &str) -> u16 {
        let own = format!("http://127.0.0.1:{}", self.port);
        self.post(
            "/decide",
            &json!({"id": id, "verdict": verdict}).to_string(),
            &self.token.clone(),
            Some(&own),
        )
    }

    /// The event stream, as (event, data) pairs on a channel.
    fn events(&self) -> Receiver<(String, Value)> {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap_or_else(|e| panic!("{e}"));
        let _ = write!(
            s,
            "GET {}/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: pageon_viewer={}\r\n\r\n",
            self.path, self.port, self.cookie
        );
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut kind = String::new();
            for line in BufReader::new(s).lines().map_while(Result::ok) {
                if let Some(k) = line.strip_prefix("event: ") {
                    kind = k.to_string();
                } else if let Some(d) = line.strip_prefix("data: ")
                    && let Ok(v) = serde_json::from_str(d)
                    && tx.send((kind.clone(), v)).is_err()
                {
                    return;
                }
            }
        });
        rx
    }
}

/// The next event of `kind` that `pick` accepts.
fn next(rx: &Receiver<(String, Value)>, kind: &str, pick: impl Fn(&Value) -> bool) -> Value {
    loop {
        let (k, v) = rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("no {kind} event"));
        if k == kind && pick(&v) {
            return v;
        }
    }
}

fn waiting(v: &Value) -> bool {
    v["state"] == "waiting"
}

fn supervise(caged: bool) {
    let (origin, posts) = serve();
    let mut mcp = Mcp::start(&origin, caged);
    let viewer = Browser::open(&mcp.url);
    let events = viewer.events();

    // Only the first browser views; the right Host only; writes need the token and Origin.
    let (port, path) = (viewer.port, viewer.path.clone());
    let (s, _, _) = http(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n"),
    );
    assert_eq!(s, 409, "a second browser was let in");
    let (s, _, _) = http(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: evil.example\r\n\r\n"),
    );
    assert_eq!(s, 400, "a foreign Host was served");
    assert_eq!(viewer.post("/pause", "", "wrong", None), 403);
    assert_eq!(
        viewer.post(
            "/pause",
            "",
            &viewer.token.clone(),
            Some("https://evil.example")
        ),
        403
    );

    let page = mcp.call("browser_open", json!({"url": format!("{origin}/")}));
    assert!(page.contains("button e2 \"Pay now\""), "{page}");
    let step = next(&events, "step", |v| v["tool"] == "browser_open");
    assert!(
        step["text"]
            .as_str()
            .unwrap_or_default()
            .contains("Pay now")
    );
    // Watched: the step has an image.
    assert_eq!(step["image"], true, "{step}");
    let (s, img) = viewer.get(&format!("/shot/{}.jpg", step["n"]));
    assert_eq!(s, 200);
    assert!(img.starts_with(&[0xff, 0xd8]), "not a JPEG");

    mcp.call(
        "browser_type",
        json!({"ref": "e1", "text": "secret://card"}),
    );

    // Deny: the agent hears it was not approved; nothing was posted.
    mcp.send("browser_click", json!({"ref": "e2"}));
    let a = next(&events, "approval", waiting);
    assert_eq!(a["effect"]["method"], "POST");
    assert_eq!(a["effect"]["origin"], origin.as_str());
    let labels = a["effect"]["labels"].to_string();
    assert!(labels.contains("secret:card"), "{labels}");
    assert_eq!(a["effect"]["cause"]["target_untrusted"], true, "{a}");
    assert!(
        !a.to_string().contains(SECRET),
        "the approval shows the secret"
    );
    // A decision without the page token changes nothing.
    assert_eq!(
        viewer.post(
            "/decide",
            &json!({"id": a["id"], "verdict": "once"}).to_string(),
            "x",
            None
        ),
        403
    );
    assert_eq!(viewer.decide(a["id"].as_u64().unwrap_or(0), "deny"), 204);
    let out = mcp.reply(Duration::from_secs(30)).unwrap_or_default();
    assert!(out.contains("not approved"), "{out}");
    assert!(posts.lock().map(|p| p.is_empty()).unwrap_or(false));

    // Once: sent; the next write asks again.
    mcp.call("browser_open", json!({"url": format!("{origin}/")}));
    mcp.call(
        "browser_type",
        json!({"ref": "e1", "text": "secret://card"}),
    );
    mcp.send("browser_click", json!({"ref": "e2"}));
    let a = next(&events, "approval", waiting);
    assert_eq!(viewer.decide(a["id"].as_u64().unwrap_or(0), "once"), 204);
    let out = mcp.reply(Duration::from_secs(30)).unwrap_or_default();
    assert!(out.contains("Paid"), "{out}");
    assert_eq!(posts.lock().map(|p| p.len()).unwrap_or(0), 1);
    assert!(posts.lock().map(|p| p[0].contains(SECRET)).unwrap_or(false));

    // For the session: this write and later ones to the origin go.
    mcp.call("browser_open", json!({"url": format!("{origin}/")}));
    mcp.send("browser_click", json!({"ref": "e2"}));
    let a = next(&events, "approval", waiting);
    assert_eq!(viewer.decide(a["id"].as_u64().unwrap_or(0), "session"), 204);
    mcp.reply(Duration::from_secs(30)).unwrap_or_default();
    mcp.call("browser_open", json!({"url": format!("{origin}/")}));
    let out = mcp.call("browser_click", json!({"ref": "e2"}));
    assert!(out.contains("Paid"), "{out}");
    assert_eq!(posts.lock().map(|p| p.len()).unwrap_or(0), 3);
    let status = next(&events, "status", |v| {
        !v["grants"].as_array().is_none_or(Vec::is_empty)
    });
    assert_eq!(status["grants"][0], format!("POST {origin}"));

    // Pause holds the next call until resume.
    let own = format!("http://127.0.0.1:{}", viewer.port);
    assert_eq!(
        viewer.post("/pause", "", &viewer.token.clone(), Some(&own)),
        204
    );
    mcp.send("browser_find", json!({"query": "thank"}));
    assert!(
        mcp.reply(Duration::from_millis(400)).is_none(),
        "a paused session answered"
    );
    assert_eq!(
        viewer.post("/resume", "", &viewer.token.clone(), Some(&own)),
        204
    );
    assert!(mcp.reply(Duration::from_secs(30)).is_some());

    // Nothing the viewer streamed holds the secret.
    let (s, js) = viewer.get("/app.js");
    assert_eq!(s, 200);
    let js = String::from_utf8_lossy(&js);
    assert!(!js.contains("innerHTML"), "the page builds markup");
    let mut all = String::new();
    while let Ok((_, v)) = events.recv_timeout(Duration::from_millis(200)) {
        all.push_str(&v.to_string());
    }
    assert!(!all.contains(SECRET));
}

#[test]
fn supervise_caged() {
    supervise(true);
}

#[test]
fn supervise_in_process() {
    supervise(false);
}

#[cfg(unix)]
#[test]
fn cli_sessions_serve_the_viewer() {
    let dir = std::env::temp_dir().join(format!("pgv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let page = dir.with_extension("html");
    std::fs::write(&page, "<title>Hi</title><p>Hello").unwrap_or_else(|e| panic!("{e}"));
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_pageon"))
            .args(args)
            .env("PAGEON_SESSIONS", &dir)
            .env_remove("PAGEON_SESSION")
            .output()
            .unwrap_or_else(|e| panic!("{e}"))
    };
    let o = run(&[
        "open",
        &page.display().to_string(),
        "--allow-files",
        "--no-sandbox",
        "--ui",
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let err = String::from_utf8_lossy(&o.stderr);
    let url = err
        .lines()
        .find_map(|l| l.strip_prefix("viewer: "))
        .unwrap_or_else(|| panic!("no viewer line: {err}"))
        .to_string();
    let o = run(&["ui"]);
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), url);
    let viewer = Browser::open(&url);
    let events = viewer.events();
    let o = run(&["find", "hello"]);
    assert!(o.status.success());
    next(&events, "step", |v| v["tool"] == "browser_find");
    run(&["close"]);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&page);
}

#[test]
#[ignore = "timing: run with --release -- --ignored"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn unwatched_viewers_cost_nothing() {
    let (origin, _) = serve();
    let mut with = Mcp::start(&origin, false);
    with.call("browser_open", json!({"url": format!("{origin}/")}));
    let time = |m: &mut Mcp| {
        let t = std::time::Instant::now();
        for _ in 0..200 {
            m.call("browser_find", json!({"query": "pay"}));
        }
        t.elapsed().as_secs_f64() * 1000.0 / 200.0
    };
    let ms = time(&mut with);
    println!("find with an unwatched viewer: {ms:.3} ms per call");
    assert!(ms < 5.0, "{ms:.3} ms");
}
