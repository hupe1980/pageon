//! Files: uploads of operator-staged files by name (in-process, caged, replayed),
//! the File API, and downloads as handles.

mod common;

use std::cell::RefCell;

use common::*;
use pageon::replay::{InProcess, rerun};
use pageon::{Files, LoadOptions, Recording, Replay, Sandbox, SandboxOptions, Session};
use pageon_env::{Env, NetError, Request, Response};

const APPLY: &str = include_str!("fixtures/files/apply.html");
const CHECKS: &str = include_str!("fixtures/files/checks.html");
const CV: &str = "Hello CV, line two";

fn jobs() -> pageon_env::Log {
    site(vec![
        html("https://jobs.test/", APPLY),
        html(
            "https://evil.test/grab.html",
            include_str!("fixtures/files/grab.html"),
        ),
        ex(
            "POST",
            "https://jobs.test/apply",
            200,
            &[("content-type", "text/html")],
            "<h1>Thanks</h1>",
        ),
    ])
}

/// The staged CV, a fresh file per test.
fn cv_file(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pageon-files-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let p = dir.join("cv.txt");
    std::fs::write(&p, CV).unwrap_or_else(|e| panic!("{e}"));
    p
}

/// Keeps the body of every POST.
struct Bodies<E>(E, RefCell<Vec<(String, Vec<u8>, String)>>);

impl<E: Env> Env for Bodies<E> {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        if req.method == "POST" {
            let ct = req
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            self.1
                .borrow_mut()
                .push((req.url.clone(), req.body.clone(), ct));
        }
        self.0.fetch(req)
    }
    fn now_ms(&self) -> Result<u64, NetError> {
        self.0.now_ms()
    }
    fn random_u64(&self) -> Result<u64, NetError> {
        self.0.random_u64()
    }
}

fn opts() -> LoadOptions {
    LoadOptions {
        robots: false,
        ..LoadOptions::default()
    }
}

/// The ref on the observation line naming `name`, whatever its role.
fn ref_named(obs: &str, name: &str) -> String {
    obs.lines()
        .find(|l| l.contains(&format!("\"{name}\"")))
        .and_then(|l| {
            l.split_whitespace().find(|w| {
                w.starts_with('e') && w[1..].chars().all(|c| c.is_ascii_digit()) && w.len() > 1
            })
        })
        .map(str::to_string)
        .unwrap_or_else(|| panic!("no {name:?} in {obs}"))
}

fn act(s: &str) -> pageon::Action {
    s.parse().unwrap_or_else(|e| panic!("{e}"))
}

/// The application: attach through the label, see the page's preview, submit the file.
fn apply<E: Env>(tab: &mut pageon::Tab<'_, E>) {
    let obs = tab.observe();
    let attach = ref_named(&obs, "Attach CV");
    let o = tab
        .act(act(&format!("upload {attach} cv")))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        o.to_string().starts_with(&format!("ok upload {attach} cv")),
        "{o}"
    );
    let obs = tab.observe();
    assert!(obs.contains("cv.txt (18 bytes, text/plain)"), "{obs}");
    assert!(obs.contains("preview: Hello CV, line two"), "{obs}");
    tab.act(act(&format!("type {} Ada", find(&obs, "textbox", "Name"))))
        .unwrap_or_else(|e| panic!("{e}"));
    tab.act(act(&format!("click {}", find(&obs, "button", "Apply"))))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(tab.observe().contains("Thanks"));
}

fn check_body(bodies: &[(String, Vec<u8>, String)]) {
    let (url, body, ct) = bodies
        .iter()
        .find(|(u, _, _)| u.ends_with("/apply"))
        .unwrap_or_else(|| panic!("no POST"));
    assert!(
        ct.starts_with("multipart/form-data; boundary="),
        "{url} {ct}"
    );
    let text = String::from_utf8_lossy(body);
    assert!(
        text.contains("Content-Disposition: form-data; name=\"cv\"; filename=\"cv.txt\"\r\nContent-Type: text/plain\r\n\r\nHello CV, line two\r\n"),
        "{text}"
    );
    assert!(text.contains("name=\"name\"\r\n\r\nAda\r\n"), "{text}");
}

#[test]
fn an_agent_attaches_a_staged_file() {
    let env = Files::new(Bodies(Replay::new(jobs()), RefCell::default())).with(
        "cv",
        cv_file("inproc"),
        ["https://jobs.test"],
    );
    let session = Session::new(&env);
    let mut tab = session
        .open("https://jobs.test/", opts())
        .unwrap_or_else(|e| panic!("{e}"));
    apply(&mut tab);
    drop(tab);
}

#[test]
fn an_agent_attaches_a_staged_file_caged() {
    let bodies = Bodies(Replay::new(jobs()), RefCell::default());
    let env = Files::new(&bodies).with("cv", cv_file("caged"), ["https://jobs.test"]);
    let mut sb = Sandbox::new(
        &env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut tab = sb
        .open("https://jobs.test/", opts())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    let attach = ref_named(&obs, "Attach CV");
    tab.act(act(&format!("upload {attach} cv")))
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    assert!(obs.contains("preview: Hello CV, line two"), "{obs}");
    tab.act(act(&format!("type {} Ada", find(&obs, "textbox", "Name"))))
        .unwrap_or_else(|e| panic!("{e}"));
    tab.act(act(&format!("click {}", find(&obs, "button", "Apply"))))
        .unwrap_or_else(|e| panic!("{e}"));
    drop(tab);
    check_body(&bodies.1.borrow());
}

/// Unstaged names, other origins, several files without `multiple`, and
/// elements that open no chooser are refused with what to do.
#[test]
fn uploads_are_refused_with_guidance() {
    let env = Files::new(Replay::new(jobs())).with("cv", cv_file("refuse"), ["https://jobs.test"]);
    let session = Session::new(&env);
    let mut tab = session
        .open("https://jobs.test/", opts())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    let attach = ref_named(&obs, "Attach CV");
    let err = |tab: &mut pageon::Tab<'_, _>, a: &str| {
        tab.act(act(a))
            .map(|o| o.to_string())
            .unwrap_err()
            .to_string()
    };
    let e = err(&mut tab, &format!("upload {attach} /etc/passwd"));
    assert!(e.contains("is not staged") && e.contains("--file"), "{e}");
    let e = err(&mut tab, &format!("upload {attach} cv,cv"));
    assert!(e.contains("takes one file"), "{e}");
    let e = err(
        &mut tab,
        &format!("upload {} cv", find(&obs, "button", "Apply")),
    );
    assert!(e.contains("not a file input"), "{e}");
    let partner = ref_named(&obs, "Partner upload");
    let e = err(&mut tab, &format!("upload {partner} cv"));
    assert!(e.contains("is not for https://evil.test"), "{e}");
    // A click that opens the chooser says how to fill it.
    let o = tab
        .act(act(&format!("click {attach}")))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        o.to_string()
            .contains(&format!("file chooser for {attach} (upload {attach} NAME)")),
        "{o}"
    );
}

#[test]
fn an_upload_replays() {
    let file = cv_file("replay");
    let rec = Recording::new(Replay::new(jobs()));
    {
        let env = Files::new(&rec).with("cv", &file, ["https://jobs.test"]);
        let session = Session::new(&env);
        let mut tab = session
            .open("https://jobs.test/", opts())
            .unwrap_or_else(|e| panic!("{e}"));
        apply(&mut tab);
    }
    let log = rec.log();
    assert!(
        log.steps.iter().any(|s| s.action.starts_with("upload ")),
        "{:?}",
        log.steps
    );
    let last = log.steps.len() - 1;
    let again = Recording::resume(log.clone(), last, Replay::new(log.clone()), true)
        .unwrap_or_else(|e| panic!("{e}"));
    let env = Files::new(&again).with("cv", &file, ["https://jobs.test"]);
    let session = Session::new(&env);
    rerun(
        &mut InProcess::new(&session),
        &log.steps,
        last,
        &mut |_, _| {},
    )
    .unwrap_or_else(|d| panic!("{}", pageon::replay::report(&d)));
}

/// A download uploads back to its own origin by handle, nowhere else.
#[test]
fn downloads_are_handles() {
    let page = "<a href=/invoice.pdf>Invoice</a><input type=file id=f aria-label=Receipt><p id=out></p><script>f.onchange = () => out.textContent = f.files[0].name + ' ' + f.files[0].size</script>";
    let session = Session::new(Replay::new(site(vec![
        html("https://shop.test/", page),
        bytes(
            "https://shop.test/invoice.pdf",
            "application/pdf",
            b"%PDF-1.7 invoice",
        ),
    ])));
    let mut tab = session
        .open("https://shop.test/", opts())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    let o = tab
        .act(act(&format!("click {}", find(&obs, "link", "Invoice"))))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(o.downloads[0].handle, "download:1");
    assert!(o.to_string().contains("download:1 \"invoice.pdf\""), "{o}");
    let json = serde_json::to_string(&o).unwrap_or_default();
    assert!(
        json.contains("\"bytes\":\"JVBERi0xLjcgaW52b2ljZQ==\""),
        "base64, not numbers: {json}"
    );
    tab.act(act(&format!(
        "upload {} download:1",
        ref_named(&obs, "Receipt")
    )))
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        tab.observe().contains("invoice.pdf 16"),
        "{}",
        tab.observe()
    );
}

#[test]
fn the_file_api_checks_pass() {
    let doc = pageon::load(
        "https://checks.test/",
        &Replay::new(site(vec![html("https://checks.test/", CHECKS)])),
        opts(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let out = text_of(&doc, "out");
    let results: Vec<(String, bool)> =
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out:?}\n{:#?}", doc.errors()));
    let failed: Vec<&String> = results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n)
        .collect();
    assert!(results.len() >= 40, "{}", results.len());
    assert!(failed.is_empty(), "{failed:#?}\n{:#?}", doc.errors());
}

/// SC-004: a 10 MiB upload through the broker into a caged page, in under a second (release).
#[test]
#[ignore = "timing: cargo test --release -p pageon --test files -- --ignored"]
#[allow(clippy::disallowed_methods)] // wall time
fn a_large_upload_is_fast() {
    let dir = std::env::temp_dir().join(format!("pageon-big-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let big = dir.join("big.bin");
    std::fs::write(&big, vec![7u8; 10 << 20]).unwrap_or_else(|e| panic!("{e}"));
    let page = "<input type=file id=f aria-label=Data><p id=out></p><script>f.onchange = () => out.textContent = 'size ' + f.files[0].size</script>";
    let base = Replay::new(site(vec![html("https://big.test/", page)]));
    let env = Files::new(&base).with("big", &big, ["https://big.test"]);
    let mut sb = Sandbox::new(
        &env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut tab = sb
        .open("https://big.test/", opts())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    let started = std::time::Instant::now();
    tab.act(act(&format!("upload {} big", ref_named(&obs, "Data"))))
        .unwrap_or_else(|e| panic!("{e}"));
    let took = started.elapsed();
    assert!(
        tab.observe()
            .unwrap_or_else(|e| panic!("{e}"))
            .contains("size 10485760")
    );
    eprintln!("10 MiB caged upload: {took:?}");
    assert!(took < std::time::Duration::from_secs(1), "{took:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
