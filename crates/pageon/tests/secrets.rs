//! The secrets vault: handles bound to origins, resolved in the broker, never
//! shown.

mod common;

use common::*;
use pageon::{
    Action, ActionError, Budget, Dump, LoadOptions, Mode, Recording, Replay, Sandbox,
    SandboxOptions, Secret, Secrets, Session, Viewport,
};
use pageon_env::Env;

const PASSWORD: &str = "hunter2-very-secret";
const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

fn login_site() -> Vec<pageon_env::Exchange> {
    vec![
        html(
            "https://shop.test/login",
            "<form method=post action=/session><input name=user aria-label=User>\
             <input name=pw type=password aria-label=Password><input name=otp aria-label=Code>\
             <button>Sign in</button></form><iframe src='https://evil.test/frame'></iframe>",
        ),
        html(
            "https://evil.test/frame",
            "<input aria-label=Steal><div id=echo></div>",
        ),
        ex(
            "POST",
            "https://shop.test/session",
            200,
            &[("content-type", "text/html")],
            // A careless server echoes the password: views must still not show it.
            &format!("<h1>Welcome</h1><p>Your password is {PASSWORD}</p>"),
        ),
    ]
}

fn vault<E: Env>(env: E) -> Secrets<E> {
    Secrets::new(env)
        .with(
            "shop",
            Secret::value(PASSWORD).origins(["https://shop.test"]),
        )
        .with(
            "otp",
            Secret::totp(SEED)
                .unwrap_or_else(|e| panic!("{e}"))
                .origins(["https://shop.test"]),
        )
}

#[test]
fn secrets_reach_the_server_and_nothing_else() {
    let rec = Recording::new(Capture::new(site(login_site())));
    let env = vault(&rec);
    let session = Session::new(&env);
    let mut tab = session
        .open("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    tab.set_view(Budget::o200k(4000), Mode::Full);
    let obs = tab.observe();
    let mut seen = vec![obs.clone()];
    for a in [
        format!("type {} ada", find(&obs, "textbox", "User")),
        format!("type {} secret://shop", find(&obs, "textbox", "Password")),
        format!("type {} secret://otp", find(&obs, "textbox", "Code")),
    ] {
        let o = tab
            .act(a.parse().unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{a}: {e}"));
        seen.push(o.to_string());
        seen.push(format!("{o:?}"));
        seen.push(tab.observe());
    }
    let obs = tab.observe();
    assert!(obs.contains("textbox e2 \"Password\" =\"•••\""), "{obs}");
    // A secret on another origin's field is refused.
    let steal = find(&obs, "textbox", "Steal");
    let e = tab
        .act(Action::Type(steal, "secret://shop".into()))
        .err()
        .unwrap_or_else(|| panic!("must refuse"));
    assert!(matches!(e, ActionError::Secret(_)), "{e:?}");
    assert!(e.to_string().contains("not for https://evil.test"), "{e}");
    // Unknown secret.
    let e = tab
        .act(Action::Type(
            find(&obs, "textbox", "User"),
            "secret://nope".into(),
        ))
        .err()
        .unwrap_or_else(|| panic!("must refuse"));
    assert!(e.to_string().contains("not in the vault"), "{e}");
    let o = tab
        .act(Action::Press("Enter".into()))
        .unwrap_or_else(|e| panic!("{e}"));
    seen.push(o.to_string());
    for kind in [
        Dump::Text,
        Dump::Html,
        Dump::Ax,
        Dump::Markdown,
        Dump::Requests,
        Dump::Errors,
    ] {
        seen.push(tab.view(kind, Viewport::default(), Budget::o200k(4000)));
    }
    seen.push(tab.status_line());
    seen.push(tab.observe());
    let code = env
        .values()
        .into_iter()
        .find(|v| v.len() == 6)
        .unwrap_or_default();
    assert_eq!(code.len(), 6, "a TOTP code was handed out");
    // The values reached the server…
    let post = env
        .inner()
        .inner()
        .seen
        .borrow()
        .iter()
        .find(|r| r.method == "POST")
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .unwrap_or_default();
    assert_eq!(post, format!("user=ada&pw={PASSWORD}&otp={code}"));
    // …and appear nowhere the agent, a log or a recording can see.
    rec.redact(env.values());
    seen.push(serde_json::to_string(&rec.log()).unwrap_or_default());
    for text in &seen {
        assert!(!text.contains(PASSWORD), "password leaked:\n{text}");
        assert!(!text.contains(&code), "TOTP code leaked:\n{text}");
    }
    assert!(
        seen.iter()
            .any(|t| t.contains("Your password is secret://shop"))
    );
}

#[test]
fn secrets_cross_the_cage_by_handle() {
    let env = vault(Replay::new(site(login_site())));
    let mut sb = Sandbox::new(
        &env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut tab = sb
        .open("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    let o = tab
        .act(Action::Type(
            find(&obs, "textbox", "Password"),
            "secret://shop".into(),
        ))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(o.to_string().contains("secret://shop"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    assert!(!obs.contains(PASSWORD) && obs.contains("=\"•••\""), "{obs}");
}
