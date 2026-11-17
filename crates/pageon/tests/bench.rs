//! `pageon bench`: a page is recorded once into a private cache, then measured offline; the
//! report has every column, and `--against` shows changes.

use std::io::{Read as _, Write as _};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const PAGE: &str = "<title>Bench</title><main><h1>Fast</h1><p id=p>x</p><script>
const p = document.getElementById('p'); p.style.width = '120px'; p.textContent = p.offsetWidth;
</script></main>";

/// Serves the page; counts document requests.
fn serve() -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let origin = format!(
        "http://127.0.0.1:{}",
        listener.local_addr().map(|a| a.port()).unwrap_or(0)
    );
    let hits = Arc::new(AtomicUsize::new(0));
    let count = hits.clone();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let (status, body) = if req.starts_with("GET / ") {
                count.fetch_add(1, Ordering::SeqCst);
                ("200 OK", PAGE)
            } else {
                ("404 Not Found", "")
            };
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (origin, hits)
}

/// A fresh directory for one test.
fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("pageon-bench-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
    d
}

fn bench(cache: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_pageon"))
        .arg("bench")
        .args(args)
        .env("XDG_CACHE_HOME", cache)
        .output()
        .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn a_page_is_recorded_once_and_measured_offline() {
    let (origin, hits) = serve();
    let url = format!("{origin}/");
    let cache = dir("once");
    let opts = ["--", "--private-network", "allow", "--robots", "ignore"];
    let mut args = vec![url.as_str(), "--runs", "2", "--json"];
    args.extend(opts);
    let out = bench(&cache, &args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}"));
    let row = &report["pages"][0];
    assert!(row["error"].is_null(), "{row}");
    assert!(
        row["first_view_ms"]["p50"].as_f64().unwrap_or(0.0) > 0.0,
        "{row}"
    );
    assert!(row["cpu_ms"]["p50"].as_f64().unwrap_or(0.0) > 0.0, "{row}");
    assert!(row["memory_mb"].as_f64().unwrap_or(0.0) > 1.0, "{row}");
    assert!(row["tokens"].as_u64().unwrap_or(0) > 0, "{row}");
    assert!(row["layouts"].as_u64().unwrap_or(0) >= 1, "{row}");
    assert_eq!(row["steady"], true, "{row}");
    // Recorded once (the document fetched once), private, in the cache.
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let rec = std::path::PathBuf::from(row["recording"].as_str().unwrap_or_default());
    assert!(rec.starts_with(&cache), "{rec:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&rec)
            .map(|m| m.permissions().mode())
            .unwrap_or(0);
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
    // Again, against the first report: offline, with the change per column.
    let before = cache.join("before.json");
    std::fs::write(&before, &out.stdout).unwrap_or_else(|e| panic!("{e}"));
    let before = before.display().to_string();
    let mut args = vec![url.as_str(), "--runs", "1", "--against", before.as_str()];
    args.extend(opts);
    let out = bench(&cache, &args);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("first view") && text.contains('%'), "{text}");
    assert_eq!(hits.load(Ordering::SeqCst), 1, "replayed offline");
    let _ = std::fs::remove_dir_all(&cache);
}

#[test]
fn a_page_that_fails_is_reported() {
    let cache = dir("fails");
    let out = bench(&cache, &["http://127.0.0.1:9/", "--runs", "1"]);
    assert_eq!(out.status.code(), Some(3));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("error: recording failed"), "{text}");
    let left = std::fs::read_dir(cache.join("pageon/bench"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(left, 0, "no half-written recording");
    let _ = std::fs::remove_dir_all(&cache);
}
