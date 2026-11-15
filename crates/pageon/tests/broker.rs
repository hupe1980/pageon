//! Broker trust: a signed-in session works caged while its `HttpOnly` cookie stays
//! in the broker; storage state and replays carry it.

mod common;

use std::cell::RefCell;

use common::*;
use pageon::replay::{Caged, rerun};
use pageon::{LoadOptions, Recording, Replay, Sandbox, SandboxOptions};
use pageon_env::{Env, NetError, Request, Response};

fn login_site() -> pageon_env::Log {
    site(vec![
        html(
            "https://bank.test/",
            "<form method=post action=/login><input name=user aria-label=User><button>Sign in</button></form>",
        ),
        ex(
            "POST",
            "https://bank.test/login",
            200,
            &[
                ("content-type", "text/html"),
                ("set-cookie", "sid=s3cr3t-session; Path=/; HttpOnly; Secure"),
                ("set-cookie", "theme=dark; Path=/"),
            ],
            "<a href=/account>Account</a><p id=seen></p><script>seen.textContent = 'cookies: ' + document.cookie</script>",
        ),
        html("https://bank.test/account", "<h1>Balance 42</h1>"),
    ])
}

/// Keeps the `Cookie` header of every request.
struct Cookies<E>(E, RefCell<Vec<(String, String)>>);

impl<E: Env> Env for Cookies<E> {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        let c = req
            .headers
            .iter()
            .find(|(k, _)| k == "cookie")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        self.1.borrow_mut().push((req.url.clone(), c));
        self.0.fetch(req)
    }
    fn now_ms(&self) -> Result<u64, NetError> {
        self.0.now_ms()
    }
    fn random_u64(&self) -> Result<u64, NetError> {
        self.0.random_u64()
    }
}

fn sandbox<E: Env>(env: E) -> Sandbox<E> {
    Sandbox::new(
        env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

fn act(s: &str) -> pageon::Action {
    s.parse().unwrap_or_else(|e| panic!("{e}"))
}

/// P-04: the session works, scripts see only `theme`, the server gets `sid`, the
/// saved state has `sid` as HttpOnly, and a state file puts it back into the broker.
#[test]
fn http_only_sessions_work_caged_with_the_cookie_in_the_broker() {
    let env = Cookies(Replay::new(login_site()), RefCell::default());
    let mut sb = sandbox(&env);
    let opts = LoadOptions {
        robots: false,
        ..LoadOptions::default()
    };
    let state = {
        let mut tab = sb
            .open("https://bank.test/", opts.clone())
            .unwrap_or_else(|e| panic!("{e}"));
        let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
        tab.act(act(&format!("type {} ada", find(&obs, "textbox", "User"))))
            .unwrap_or_else(|e| panic!("{e}"));
        tab.act(act(&format!("click {}", find(&obs, "button", "Sign in"))))
            .unwrap_or_else(|e| panic!("{e}"));
        let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
        assert!(obs.contains("cookies: theme=dark"), "{obs}");
        assert!(!obs.contains("s3cr3t"), "{obs}");
        tab.act(act(&format!("click {}", find(&obs, "link", "Account"))))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            tab.observe()
                .unwrap_or_else(|e| panic!("{e}"))
                .contains("Balance 42")
        );
        drop(tab);
        sb.storage_state().unwrap_or_else(|e| panic!("{e}"))
    };
    let sent = env.1.borrow();
    let account = sent
        .iter()
        .find(|(u, _)| u.ends_with("/account"))
        .unwrap_or_else(|| panic!("{sent:?}"));
    assert!(
        account.1.contains("theme=dark") && account.1.contains("sid=s3cr3t-session"),
        "{account:?}"
    );
    let sid = state
        .cookies
        .iter()
        .find(|c| c.name == "sid")
        .unwrap_or_else(|| panic!("{state:?}"));
    assert!(sid.http_only && sid.value == "s3cr3t-session");
    assert_eq!(state.cookies.iter().filter(|c| c.name == "sid").count(), 1);
    drop(sent);

    // A new caged session loads the state: the server gets `sid` again; scripts never do.
    let env2 = Cookies(Replay::new(login_site()), RefCell::default());
    let mut sb2 = sandbox(&env2);
    sb2.set_storage_state(&state)
        .unwrap_or_else(|e| panic!("{e}"));
    let mut tab = sb2
        .open("https://bank.test/account", opts)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        tab.observe()
            .unwrap_or_else(|e| panic!("{e}"))
            .contains("Balance 42")
    );
    let sent = env2.1.borrow();
    assert!(
        sent.iter()
            .any(|(u, c)| u.ends_with("/account") && c.contains("sid=s3cr3t-session")),
        "{sent:?}"
    );
}

/// A caged signed-in session records and replays, caged.
#[test]
fn a_caged_session_with_http_only_cookies_replays() {
    let rec = Recording::new(Replay::new(login_site()));
    {
        let mut sb = sandbox(&rec);
        let mut tab = sb
            .open(
                "https://bank.test/",
                LoadOptions {
                    robots: false,
                    ..LoadOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
        tab.act(act(&format!("type {} ada", find(&obs, "textbox", "User"))))
            .unwrap_or_else(|e| panic!("{e}"));
        tab.act(act(&format!("click {}", find(&obs, "button", "Sign in"))))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    let log = rec.log();
    assert_eq!(
        log.steps.len(),
        3,
        "{:#?}",
        log.steps.iter().map(|s| &s.action).collect::<Vec<_>>()
    );
    assert_eq!(log.steps[1].action, "type e1 \"ada\"");
    let last = log.steps.len() - 1;
    let again = Recording::resume(log.clone(), last, Replay::new(log.clone()), true)
        .unwrap_or_else(|e| panic!("{e}"));
    let mut sb = sandbox(&again);
    rerun(&mut Caged::new(&mut sb), &log.steps, last, &mut |_, _| {})
        .unwrap_or_else(|d| panic!("{}", pageon::replay::report(&d)));
}
