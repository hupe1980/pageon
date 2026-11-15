//! Embedding pageon: a `Config` gives a program the binary's exact session (origins, secrets,
//! files, recording), and `mcp::serve_with` serves MCP over it.

mod common;

use std::io::{Read as _, Write as _};

use common::*;
use pageon::{Config, LoadOptions, Secret, Session};
use serde_json::{Value, json};

const PASSWORD: &str = "hunter2-embedded";

fn requests(lines: &[Value]) -> String {
    lines
        .iter()
        .map(|l| serde_json::to_string(l).unwrap_or_default() + "\n")
        .collect()
}

fn call(id: i64, tool: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}})
}

#[test]
fn an_embedder_serves_a_configured_browser() {
    let rec = file(
        "embed-shop",
        &site(vec![html(
            "https://shop.test/login",
            "<form method=post action=/session><input name=pw type=password aria-label=Password>\
             <input type=file name=cv aria-label=CV><button>Send</button></form>",
        )]),
    );
    let cv = std::env::temp_dir().join("pageon-embed-cv.txt");
    std::fs::write(&cv, "my cv").unwrap_or_else(|e| panic!("{e}"));
    let config = Config::new()
        .origin("https://shop.test")
        .secret(
            "shop",
            Secret::value(PASSWORD).origins(["https://shop.test"]),
        )
        .file("cv", &cv, ["https://shop.test"])
        .replay(&rec);
    assert_eq!(config.check(), Ok(vec![]));
    assert!(!format!("{config:?}").contains(PASSWORD));
    let input = requests(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "embedder", "version": "0"}}}),
        call(2, "browser_open", json!({"url": "https://shop.test/login"})),
        call(
            3,
            "browser_type",
            json!({"ref": "e1", "text": "secret://shop"}),
        ),
        call(4, "browser_upload", json!({"ref": "e2", "files": ["cv"]})),
        call(5, "browser_open", json!({"url": "https://other.test/"})),
        json!({"jsonrpc": "2.0", "id": 6, "method": "pageon/toolset/definition", "params": {"toolset": "browser_toolset_20260801"}}),
    ]);
    let mut out = Vec::new();
    let options = pageon::mcp::McpOptions {
        sandbox: false,
        load: LoadOptions::default(),
        ..Default::default()
    };
    pageon::mcp::serve_with(&config, options, input.as_bytes(), &mut out)
        .unwrap_or_else(|e| panic!("{e}"));
    let out = String::from_utf8_lossy(&out);
    let replies: Vec<Value> = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect();
    assert_eq!(replies.len(), 6, "{out}");
    let typed = replies[2].to_string();
    assert!(
        typed.contains("•••") && !typed.contains("isError\":true"),
        "{typed}"
    );
    assert!(replies[3].to_string().contains("cv"), "{}", replies[3]);
    // The origin set holds: another site is refused, as with `pageon mcp --origin`.
    let refused = replies[4].to_string();
    assert!(
        refused.contains("isError\":true") && refused.contains("other.test"),
        "{refused}"
    );
    // Staged files make the toolsets offer file_upload.
    assert_eq!(
        replies[5]["result"]["configs"]["file_upload"]["enabled"], true,
        "{}",
        replies[5]
    );
    assert!(!out.contains(PASSWORD), "{out}");
}

/// A one-page HTTP server on loopback for a live, recorded session.
fn serve_login() -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let origin = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().map(|a| a.port()).unwrap_or(0)
    );
    // The form sends the password in its URL: the recording sees it and must redact it.
    let page = "<form action=/s><input type=password name=pw aria-label=Password></form>";
    let handle = std::thread::spawn(move || {
        for stream in listener.incoming().take(4).flatten() {
            let mut s = stream;
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let body = if req.starts_with("GET / ") { page } else { "" };
            let status = if body.is_empty() {
                "404 Not Found"
            } else {
                "200 OK"
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (origin, handle)
}

#[cfg(unix)]
#[test]
fn recordings_through_config_are_redacted_and_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let (origin, server) = serve_login();
    let path = std::env::temp_dir().join(format!("pageon-embed-{}.rec.json", std::process::id()));
    let config = Config::new()
        .secret("shop", Secret::value(PASSWORD).origins([origin.clone()]))
        .record(&path);
    config
        .run(|env| {
            let session = Session::new(env);
            let options = LoadOptions {
                allow_private_network: true,
                robots: false,
                ..LoadOptions::default()
            };
            let mut tab = session
                .open(&format!("{origin}/"), options)
                .unwrap_or_else(|e| panic!("{e}"));
            let obs = tab.observe_full();
            let pw = find(&obs, "textbox", "Password");
            for a in [
                format!("type {pw} secret://shop"),
                "press Enter".to_string(),
            ] {
                tab.act(a.parse().unwrap_or_else(|e| panic!("{e}")))
                    .unwrap_or_else(|e| panic!("{a}: {e}"));
            }
        })
        .unwrap_or_else(|e| panic!("{e}"));
    drop(server);
    let saved = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}"));
    // The submitted URL was recorded, with the value replaced by its handle.
    assert!(saved.contains("/s?pw="), "{saved}");
    assert!(
        saved.contains("secret://shop") || saved.contains("secret%3A%2F%2Fshop"),
        "{saved}"
    );
    assert!(!saved.contains(PASSWORD), "the recording holds the secret");
    let mode = std::fs::metadata(&path)
        .unwrap_or_else(|e| panic!("{e}"))
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn errors_name_the_fix_and_never_the_value() {
    let config = Config::new()
        .secret(
            "shop",
            Secret::value(PASSWORD).origins(["https://shop.test"]),
        )
        .file_spec("cv=./cv.pdf");
    let e = config.check().unwrap_err();
    assert!(e.to_string().contains("NAME@ORIGIN=PATH") && !e.to_string().contains(PASSWORD));
    assert!(!e.guidance().contains(PASSWORD));
}
