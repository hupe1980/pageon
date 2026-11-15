//! Identity: signatures that verify (https only), refusals reported as typed outcomes,
//! robots.txt honoured, and the profile's values where scripts and servers see them.

mod common;

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use common::*;
use pageon::{
    Env, Identity, Live, LoadError, LoadOptions, Profile, Recording, RefusalKind, Sandbox,
    SandboxOptions, Session,
};
use pageon_env::{NetError, Request, Response};

/// A test identity (RFC 9421's `test-key-ed25519`).
fn identity() -> Identity {
    let d = URL_SAFE_NO_PAD
        .decode("n4Ni-HpISpVObnQMW0wOhCKROaIKqKtW_2ZYb2p9KcU")
        .unwrap_or_default();
    Identity::from_seed(d.try_into().unwrap_or([0; 32]), "https://bot.example")
        .unwrap_or_else(|e| panic!("{e}"))
}

type Seen = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;

/// A local HTTP/1.1 server: `routes` maps a path to (status, headers, body); every request's
/// path and headers are kept.
fn server(routes: Vec<(&'static str, u16, &'static str, &'static str)>) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
    let port = listener.local_addr().map_or(0, |a| a.port());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let kept = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap_or_else(|e| panic!("{e}")));
            let mut line = String::new();
            let _ = r.read_line(&mut line);
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let mut headers = Vec::new();
            let mut len = 0;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.trim_end().split_once(':') {
                    let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
                    if k == "content-length" {
                        len = v.parse().unwrap_or(0);
                    }
                    headers.push((k, v));
                }
            }
            let mut body = vec![0; len];
            let _ = r.read_exact(&mut body);
            kept.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((path.clone(), headers));
            let (status, extra, body) = routes
                .iter()
                .find(|(p, ..)| *p == path)
                .map_or((404, "", "not found"), |(_, s, h, b)| (*s, *h, *b));
            let head = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body.as_bytes());
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

fn local() -> LoadOptions {
    LoadOptions {
        allow_private_network: true,
        ..LoadOptions::default()
    }
}

/// Verifies a request's Web Bot Auth signature independently: rebuilds the signature base from
/// what arrived and checks it against the directory's public key.
fn verify(headers: &[(String, String)], public: &[u8]) -> Result<(), String> {
    let h = |n: &str| {
        headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.clone())
            .ok_or(format!("no {n}"))
    };
    let input = h("signature-input")?;
    let params = input.strip_prefix("sig1=").ok_or("no sig1")?;
    if !params.starts_with(r#"("@authority" "signature-agent")"#)
        || !params.contains(r#"tag="web-bot-auth""#)
    {
        return Err(format!("unexpected parameters {params}"));
    }
    let base = format!(
        "\"@authority\": {}\n\"signature-agent\": {}\n\"@signature-params\": {params}",
        h("host")?,
        h("signature-agent")?
    );
    let sig = h("signature")?;
    let sig = STANDARD
        .decode(sig.trim_start_matches("sig1=:").trim_end_matches(':'))
        .map_err(|e| e.to_string())?;
    aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ED25519, public)
        .verify(base.as_bytes(), &sig)
        .map_err(|_| "the signature does not verify".to_string())
}

const PAGE: &str = r#"<!doctype html><title>Signed</title><link rel=stylesheet href=/s.css>
<script src=/a.js></script><iframe src=/frame></iframe>
<script>fetch('/api', {headers: {'Signature': 'sig1=:forged:', 'Signature-Input': 'sig1=()',
  'Signature-Agent': '"https://evil.example"'}}).then(r => r.text()).then(t => document.title = t)</script>"#;

fn routes() -> Vec<(&'static str, u16, &'static str, &'static str)> {
    vec![
        ("/", 302, "location: /home\r\n", ""),
        ("/home", 200, "", PAGE),
        ("/s.css", 200, "", "body{color:red}"),
        ("/a.js", 200, "", "window.x = 1"),
        ("/frame", 200, "", "<p>frame"),
        ("/api", 200, "", "done"),
    ]
}

/// The directory's public key, as a verifier would fetch it.
fn public_key() -> Vec<u8> {
    let (dir, _) = identity().directory("bot.example", 0, 1);
    let dir: serde_json::Value = serde_json::from_str(&dir).unwrap_or_default();
    URL_SAFE_NO_PAD
        .decode(dir["keys"][0]["x"].as_str().unwrap_or_default())
        .unwrap_or_default()
}

#[test]
fn https_signatures_verify_independently() {
    let headers: Vec<(String, String)> = identity()
        .sign("shop.example", 1_800_000_000, &[7; 32])
        .into_iter()
        .chain([("host".to_string(), "shop.example".to_string())])
        .collect();
    verify(&headers, &public_key()).unwrap_or_else(|e| panic!("{e}"));
}

/// Plain http is never signed (an on-path attacker could replay the signature over https), and
/// a page cannot add its own `Signature*` headers.
#[test]
fn http_requests_carry_no_signature_and_pages_cannot_forge_one() {
    let (base, seen) = server(routes());
    for caged in [false, true] {
        seen.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let live = Live::default().with_identity(identity());
        if caged {
            let mut sb = Sandbox::new(
                &live,
                SandboxOptions {
                    program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                    ..SandboxOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
            let mut tab = sb
                .open(&format!("{base}/"), local())
                .unwrap_or_else(|e| panic!("{e}"));
            let _ = tab.observe();
        } else {
            let session = Session::new(&live);
            let _tab = session
                .open(&format!("{base}/"), local())
                .unwrap_or_else(|e| panic!("{e}"));
        }
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let paths: Vec<&str> = seen.iter().map(|(p, _)| p.as_str()).collect();
        for p in [
            "/robots.txt",
            "/",
            "/home",
            "/s.css",
            "/a.js",
            "/frame",
            "/api",
        ] {
            assert!(
                paths.contains(&p),
                "caged={caged}: {p} missing from {paths:?}"
            );
        }
        for (path, headers) in &seen {
            for (k, v) in headers {
                assert!(
                    !k.starts_with("signature"),
                    "caged={caged} {path}: {k}: {v}"
                );
            }
        }
    }
}

#[test]
fn recordings_never_hold_signatures_or_keys() {
    let (base, _) = server(routes());
    let rec = Recording::new(Live::default().with_identity(identity()));
    let session = Session::new(&rec);
    drop(
        session
            .open(&format!("{base}/"), local())
            .unwrap_or_else(|e| panic!("{e}")),
    );
    let json = serde_json::to_string(&rec.log()).unwrap_or_default();
    assert!(!json.is_empty());
    for secret in [
        "signature",
        "web-bot-auth",
        "n4Ni-HpISpVObnQMW0wOhCKROaIKqKtW_2ZYb2p9KcU",
    ] {
        assert!(!json.contains(secret), "{secret} leaked into the recording");
    }
}

#[test]
fn the_identity_commands() {
    let dir = std::env::temp_dir().join(format!("pageon-identity-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let key = dir.join("id.json");
    let out = assert_cmd::Command::cargo_bin("pageon")
        .unwrap_or_else(|e| panic!("{e}"))
        .args(["identity", "new", "--agent", "https://bot.example", "--out"])
        .arg(&key)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("keyid") && text.contains("/.well-known/http-message-signatures-directory")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&key).map_or(0, |m| m.permissions().mode());
        assert_eq!(mode & 0o777, 0o600);
    }
    let id = Identity::load(&key).unwrap_or_else(|e| panic!("{e}"));
    let out = assert_cmd::Command::cargo_bin("pageon")
        .unwrap_or_else(|e| panic!("{e}"))
        .args(["identity", "directory", "--identity"])
        .arg(&key)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    let body = String::from_utf8_lossy(&out.stdout);
    let headers = String::from_utf8_lossy(&out.stderr);
    assert!(body.contains(id.keyid()), "{body}");
    assert!(headers.contains("Content-Type: application/http-message-signatures-directory+json"));
    assert!(headers.contains("tag=\"http-message-signatures-directory\""));
    // A second `new` never overwrites the key.
    let again = assert_cmd::Command::cargo_bin("pageon")
        .unwrap_or_else(|e| panic!("{e}"))
        .args(["identity", "new", "--agent", "https://bot.example", "--out"])
        .arg(&key)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!again.status.success());
    let _ = std::fs::remove_dir_all(&dir);
}

fn refusals_site() -> pageon_env::Log {
    site(vec![
        html(
            "https://shop.test/",
            "<a href=/cf>Deals</a> <a href=/login>Log in</a>",
        ),
        page(
            "https://shop.test/cf",
            403,
            &[
                ("content-type", "text/html"),
                ("cf-mitigated", "challenge"),
                ("server", "cloudflare"),
            ],
            "<title>Just a moment...</title><p>Checking your browser before accessing shop.test.",
        ),
        page(
            "https://shop.test/busy",
            429,
            &[("content-type", "text/html"), ("retry-after", "30")],
            "slow down",
        ),
        page(
            "https://shop.test/signed",
            403,
            &[
                ("content-type", "text/html"),
                (
                    "accept-signature",
                    r#"sig1=("@authority" "signature-agent");tag="agent-browser-auth""#,
                ),
            ],
            "Signature required",
        ),
        page(
            "https://shop.test/dd",
            403,
            &[("content-type", "text/html"), ("x-datadome", "protected")],
            "<iframe src=https://geo.captcha-delivery.com/captcha/?x></iframe>",
        ),
        html(
            "https://shop.test/login",
            "<script src=https://www.google.com/recaptcha/api.js?render=k></script><form><input name=u></form>",
        ),
        page(
            "https://www.google.com/recaptcha/api.js?render=k",
            200,
            &[("content-type", "text/javascript")],
            "",
        ),
    ])
}

#[test]
fn refusals_are_typed_everywhere() {
    let log = refusals_site();
    let replay = pageon::Replay::new(log.clone());
    let session = Session::new(&replay);
    let expect = [
        ("/cf", Some((RefusalKind::Challenge, Some("cloudflare")))),
        ("/busy", Some((RefusalKind::RateLimited, None))),
        ("/dd", Some((RefusalKind::Challenge, Some("datadome")))),
        ("/signed", Some((RefusalKind::SignatureRequired, None))),
        ("/login", None),
    ];
    for (path, want) in expect {
        let mut tab = session
            .open(&format!("https://shop.test{path}"), LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let got = tab
            .document()
            .refusal()
            .map(|r| (r.kind, r.vendor.as_deref().map(str::to_string)));
        assert_eq!(got, want.map(|(k, v)| (k, v.map(str::to_string))), "{path}");
        let obs = tab.observe();
        assert_eq!(
            obs.starts_with("refused: "),
            want.is_some(),
            "{path}: {obs}"
        );
        if path == "/signed" {
            assert!(
                obs.contains("signature required (HTTP 403, tag \"agent-browser-auth\")")
                    && obs.contains("--signature-tag agent-browser-auth"),
                "{obs}"
            );
        }
        if path == "/busy" {
            assert_eq!(
                tab.document().refusal().and_then(|r| r.retry_after),
                Some(30)
            );
            assert!(obs.contains("wait 30 s"), "{obs}");
            assert!(
                tab.status_line()
                    .contains("refused: rate limited (HTTP 429), retry after 30 s")
            );
        }
        if path == "/cf" {
            assert!(obs.contains("does not solve challenges"), "{obs}");
        }
    }
    // A click that lands on a challenge carries it in the outcome; caged = in-process.
    let replay = pageon::Replay::new(log.clone());
    let session = Session::new(&replay);
    let mut tab = session
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let deals = find(&tab.observe(), "link", "Deals");
    let out = tab
        .act(pageon::Action::Click(deals.clone()))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        out.refusal.as_ref().map(|r| r.kind),
        Some(RefusalKind::Challenge)
    );
    assert!(
        out.to_string().contains("refused: challenge by cloudflare"),
        "{out}"
    );
    let replay = pageon::Replay::new(log);
    let mut sb = Sandbox::new(
        &replay,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut tab = sb
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let _ = tab.observe();
    let caged = tab
        .act(pageon::Action::Click(deals))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(caged.refusal, out.refusal);
    let (line, status, refusal) = tab.status().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(status, 403);
    assert!(line.contains("refused: challenge"), "{line}");
    assert_eq!(refusal, out.refusal);
}

#[test]
fn the_cli_exits_6_on_a_refusal() {
    let file = file("identity-refusals", &refusals_site());
    let run = |url: &str| {
        assert_cmd::Command::cargo_bin("pageon")
            .unwrap_or_else(|e| panic!("{e}"))
            .args(["fetch", url, "--replay"])
            .arg(&file)
            .output()
            .unwrap_or_else(|e| panic!("{e}"))
    };
    let out = run("https://shop.test/cf");
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(6), "{err}");
    assert!(
        err.contains("refused: challenge by cloudflare (HTTP 403, cf-mitigated: challenge)"),
        "{err}"
    );
    assert!(!out.stdout.is_empty(), "the page is still printed");
    assert_eq!(run("https://shop.test/login").status.code(), Some(0));
}

fn robots_site() -> pageon_env::Log {
    site(vec![
        page(
            "https://site.test/robots.txt",
            200,
            &[("content-type", "text/plain")],
            "User-agent: *\nDisallow: /private\n",
        ),
        html("https://site.test/public", "<p>public"),
        html("https://site.test/private/x", "<p>private"),
        html("https://site.test/", "<a href=/private/x>Secret</a>"),
    ])
}

#[test]
fn robots_txt_is_honoured_for_the_pages_an_agent_opens() {
    let cap = Capture::new(robots_site());
    let session = Session::new(&cap);
    let err = session
        .open("https://site.test/private/x", LoadOptions::default())
        .err()
        .unwrap_or_else(|| panic!("a disallowed page loaded"));
    let LoadError::Blocked { reason, .. } = &err else {
        panic!("{err:?}");
    };
    assert_eq!(
        reason,
        "robots.txt disallows /private/x (rule \"Disallow: /private\")"
    );
    assert!(
        err.guidance().contains("--robots ignore"),
        "{}",
        err.guidance()
    );
    assert!(
        !cap.urls()
            .contains(&"GET https://site.test/private/x".to_string()),
        "the page was requested: {:?}",
        cap.urls()
    );
    assert!(
        session
            .open("https://site.test/public", LoadOptions::default())
            .is_ok()
    );
    // robots.txt is fetched once per origin per session.
    let robots = cap
        .urls()
        .iter()
        .filter(|u| u.ends_with("/robots.txt"))
        .count();
    assert_eq!(robots, 1, "{:?}", cap.urls());
    // A followed link is checked too; a person-directed session can turn it off.
    let mut tab = session
        .open("https://site.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let link = find(&tab.observe(), "link", "Secret");
    let e = tab.act(pageon::Action::Click(link)).err();
    assert!(
        matches!(&e, Some(pageon::ActionError::Navigation(LoadError::Blocked { reason, .. })) if reason.starts_with("robots.txt")),
        "{e:?}"
    );
    let ignore = LoadOptions {
        robots: false,
        ..LoadOptions::default()
    };
    let other = Session::new(pageon::Replay::new(robots_site()));
    assert!(other.open("https://site.test/private/x", ignore).is_ok());
}

/// An environment that says it is `profile`: what scripts and servers see follows it.
struct As<E: Env>(E, Profile);

impl<E: Env> Env for As<E> {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        self.0.fetch(req)
    }
    fn now_ms(&self) -> Result<u64, NetError> {
        self.0.now_ms()
    }
    fn random_u64(&self) -> Result<u64, NetError> {
        self.0.random_u64()
    }
    fn profile(&self) -> Profile {
        self.1
    }
}

const PROBE: &str = r#"<p id=out></p><p id=he></p><script>
document.getElementById('out').textContent = JSON.stringify({
  ua: navigator.userAgent, webdriver: navigator.webdriver, platform: navigator.platform,
  vendor: navigator.vendor, data: 'userAgentData' in navigator ? navigator.userAgentData.toJSON() : null,
});
if ('userAgentData' in navigator) navigator.userAgentData.getHighEntropyValues(['architecture', 'fullVersionList'])
  .then(v => document.getElementById('he').textContent = v.architecture + ' ' + v.fullVersionList.length);
</script>"#;

fn probe(profile: Profile) -> (serde_json::Value, String, Vec<(String, String)>) {
    let cap = Capture::new(site(vec![html("https://probe.test/", PROBE)]));
    let env = As(&cap, profile);
    let session = Session::new(&env);
    let tab = session
        .open("https://probe.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let v = serde_json::from_str(&text_of(tab.document(), "out")).unwrap_or_default();
    let title = text_of(tab.document(), "he");
    (v, title, cap.headers("GET", "https://probe.test/"))
}

#[test]
fn scripts_and_servers_see_one_identity() {
    // The honest agent: pageon's user agent, webdriver true, no client hints.
    let (v, _, headers) = probe(Profile::Agent);
    assert_eq!(v["webdriver"], true);
    assert_eq!(v["ua"], Profile::Agent.user_agent());
    assert!(v["data"].is_null(), "{v}");
    assert!(
        !headers.iter().any(|(k, _)| k.starts_with("sec-ch-ua")),
        "{headers:?}"
    );
    // chrome, on each platform: every value scripts read agrees with the headers sent.
    for platform in [
        pageon::Platform::Windows,
        pageon::Platform::Macos,
        pageon::Platform::Linux,
    ] {
        let p = Profile::BrowserCompatible(platform);
        let (v, title, headers) = probe(p);
        let h = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
        let nav = p.navigator();
        assert_eq!(v["webdriver"], false);
        assert_eq!(v["ua"], p.user_agent());
        assert_eq!(v["platform"], nav.platform);
        assert_eq!(v["vendor"], "Google Inc.");
        let brands: Vec<(String, String)> = v["data"]["brands"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|b| {
                (
                    b["brand"].as_str().unwrap_or_default().into(),
                    b["version"].as_str().unwrap_or_default().into(),
                )
            })
            .collect();
        assert_eq!(h("sec-ch-ua"), Some(pageon_env::brand_header(&brands)));
        let ch = nav.ua_data.as_ref().map(|d| d.platform).unwrap_or_default();
        assert_eq!(v["data"]["platform"], ch);
        assert_eq!(h("sec-ch-ua-platform"), Some(format!("\"{ch}\"")));
        assert_eq!(h("sec-ch-ua-mobile").as_deref(), Some("?0"));
        assert_eq!(v["data"]["mobile"], false);
        assert_eq!(
            h("accept").as_deref(),
            Some(pageon_env::chrome::ACCEPT_DOCUMENT)
        );
        let arch = nav
            .ua_data
            .as_ref()
            .map(|d| d.architecture)
            .unwrap_or_default();
        assert_eq!(title, format!("{arch} 3"), "{platform:?}");
    }
}

#[test]
#[ignore = "timing: cargo test --release -p pageon --test identity -- --ignored --nocapture"]
#[allow(clippy::disallowed_methods)] // a wall-clock measurement
fn signing_is_cheap() {
    let id = identity();
    let n = 10_000;
    let start = std::time::Instant::now();
    for i in 0..n {
        std::hint::black_box(id.sign("shop.example", 1_800_000_000 + i, &[7; 32]));
    }
    let per = start.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!("signing: {per:.4} ms per request");
    assert!(per <= 0.2, "{per} ms");
}
