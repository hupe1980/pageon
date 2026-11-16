//! Cost: every MCP reply's tokens and time, kept in recordings, reported per run.

use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const SECRET: &str = "cost-secret-value-77";

fn serve() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let origin = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().map(|a| a.port()).unwrap_or(0)
    );
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut s = stream;
            let mut buf = vec![0u8; 16384];
            let _ = s.read(&mut buf);
            let body = "<title>Shop</title><h1>Shop</h1><label>Password <input type=password></label><a href=\"/next?token=abcdefgh12345\">Next</a><button onclick=\"setTimeout(()=>document.title='done',300)\">Later</button>";
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    origin
}

#[test]
fn replies_carry_their_cost_and_recordings_report_it() {
    let origin = serve();
    let rec = std::env::temp_dir().join(format!("pageon-cost-{}.rec.json", std::process::id()));
    let secret = format!("shop@{origin}");
    let mut child = Command::new(env!("CARGO_BIN_EXE_pageon"))
        .args([
            "mcp",
            "--private-network",
            "allow",
            "--robots",
            "ignore",
            "--no-sandbox",
            "--secret",
            &secret,
            "--record",
            &rec.display().to_string(),
        ])
        .env("PAGEON_SECRET_SHOP", SECRET)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("{e}"));
    let mut input = child.stdin.take().unwrap_or_else(|| panic!("stdin"));
    let mut output = BufReader::new(child.stdout.take().unwrap_or_else(|| panic!("stdout")));
    let calls = [
        ("browser_open", json!({"url": format!("{origin}/")})),
        (
            "browser_type",
            json!({"ref": "e1", "text": "secret://shop"}),
        ),
        ("browser_click", json!({"ref": "e3"})),
        ("browser_find", json!({"query": "next"})),
        ("browser_click", json!({"ref": "e2"})),
    ];
    for (i, (tool, args)) in calls.iter().enumerate() {
        let msg = json!({"jsonrpc": "2.0", "id": i, "method": "tools/call", "params": {
            "name": tool, "arguments": args,
            "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                      "io.modelcontextprotocol/clientCapabilities": {}}}});
        writeln!(input, "{msg}").unwrap_or_else(|e| panic!("{e}"));
        let mut line = String::new();
        output
            .read_line(&mut line)
            .unwrap_or_else(|e| panic!("{e}"));
        let r: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
        let r = &r["result"];
        let text: String = r["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let cost = &r["structuredContent"]["cost"];
        assert_eq!(
            cost["tokens"].as_u64(),
            Some(pageon_agent::count_tokens(&text) as u64),
            "{tool}: {r}"
        );
        assert!(
            cost["wall_ms"].is_u64() && cost["virtual_ms"].is_u64(),
            "{tool}: {cost}"
        );
        if *tool == "browser_click" && i == 2 {
            // The button's timer runs 300 ms on the page's clock while it settles.
            assert!(cost["virtual_ms"].as_u64().unwrap_or(0) >= 300, "{cost}");
        }
    }
    drop(input);
    let _ = child.wait();
    let o = Command::new(env!("CARGO_BIN_EXE_pageon"))
        .args(["replay", &rec.display().to_string(), "--report"])
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    let report = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{report}");
    assert!(
        report.starts_with("step  tokens  wall ms  page ms  action\n"),
        "{report}"
    );
    assert!(report.contains("type e1 \"secret://shop\""), "{report}");
    assert!(
        !report.contains(SECRET) && !report.contains("abcdefgh12345"),
        "{report}"
    );
    assert!(report.contains("4 steps: "), "{report}");
    for line in ["tokens   p50 ", "wall ms  p50 ", "page ms  p50 "] {
        assert!(report.contains(line), "{line}: {report}");
    }
    let _ = std::fs::remove_file(&rec);
}

#[test]
#[ignore = "timing: run with --release -- --ignored"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn counting_is_cheap() {
    // A typical action reply: an outcome line and a diff of a few lines.
    let reply = "ok click e12 · navigated to \"https://shop.example/cart\" · 3 request(s) · settled idle after 120 ms v4→v5\n@diff v4→v5\n+ # \"Cart\"\n+ text \"Trail Runner 3, size 42\"\n~ button e12 \"Checkout\" focused\nstep 6";
    let _ = pageon_agent::count_tokens(reply);
    let t = std::time::Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(pageon_agent::count_tokens(std::hint::black_box(reply)));
    }
    let us = t.elapsed().as_secs_f64() * 1e3;
    println!("counting a typical reply: {us:.1} µs");
    assert!(us < 200.0, "{us:.1} µs");
}
