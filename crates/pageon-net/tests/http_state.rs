//! the WPT `http-state` parser cases. Each case's `Set-Cookie` lines are received for
//! `http://home.example.org:8888/cookie-parser?<name>`; the `Cookie` header for the result URL
//! must equal the expected one.

use pageon_net::cookie::{Context, Jar};
use url::Url;

/// 2026-01-01, inside every case's notion of "now" (expiry dates are far past or far future).
const NOW: u64 = 1_767_225_600_000;

fn cx(url: &Url) -> Context<'_> {
    Context {
        url,
        top_site: "http://example.org",
        same_site: true,
        top_level_safe_nav: true,
        script: false,
    }
}

fn header_lines<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim_matches(|c| c == ' ' || c == '\t'))
        })
        .collect()
}

#[test]
fn http_state_parser_cases() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/http-state");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(|e| {
            e.ok()?
                .file_name()
                .to_str()?
                .strip_suffix("-test")
                .map(str::to_string)
        })
        .collect();
    names.sort();
    let mut failures = Vec::new();
    for name in &names {
        let read = |suffix: &str| {
            let bytes = std::fs::read(dir.join(format!("{name}-{suffix}"))).unwrap_or_default();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let test = read("test");
        let expected = read("expected");
        let set_url = Url::parse(&format!(
            "http://home.example.org:8888/cookie-parser?{name}"
        ))
        .unwrap();
        let mut jar = Jar::default();
        for line in header_lines(&test, "set-cookie") {
            let _ = jar.set(line, &cx(&set_url), NOW);
        }
        let result_url = header_lines(&test, "location")
            .first()
            .and_then(|l| set_url.join(l).ok())
            .unwrap_or_else(|| {
                Url::parse(&format!(
                    "http://home.example.org:8888/cookie-parser-result?{name}"
                ))
                .unwrap()
            });
        let want = header_lines(&expected, "cookie")
            .first()
            .copied()
            .unwrap_or_default()
            .to_string();
        let got = jar.header(&cx(&result_url), NOW);
        if got != want {
            failures.push(format!("{name}: got {got:?}, want {want:?}"));
        }
    }
    let pass = names.len() - failures.len();
    let pct = 100.0 * pass as f64 / names.len() as f64;
    println!(
        "http-state: {pass}/{} cases give the expected Cookie header ({pct:.1} %)",
        names.len()
    );
    for f in &failures {
        println!("  {f}");
    }
    assert!(names.len() > 150, "the vendored cases are missing");
    assert!(pct >= 95.0, "needs ≥ 95 %");
}
