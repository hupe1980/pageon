//! Lookahead on live forks: a fork of the caged content process tries actions; the
//! tab, the recording and the world are untouched.

mod common;

use common::*;
use pageon::replay::{Caged, rerun};
use pageon::{
    Action, Budget, Dump, LoadOptions, Recording, Replay, Sandbox, SandboxOptions, Viewport,
};

fn sandbox<E: pageon::Env>(env: E) -> Sandbox<E> {
    Sandbox::new(
        env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
        },
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

fn act(s: &str) -> Action {
    s.parse().unwrap_or_else(|e| panic!("{e}"))
}

/// The lookahead's observation, and the observation after really acting in a twin sandbox.
fn lookahead_vs_acting(
    log: &pageon_env::Log,
    url: &str,
    options: LoadOptions,
    pick: &dyn Fn(&str) -> String,
) {
    let mut a = sandbox(Replay::new(log.clone()));
    let mut ta = a.open(url, options).unwrap_or_else(|e| panic!("{e}"));
    let obs = ta.observe().unwrap_or_else(|e| panic!("{e}"));
    let action = pick(&obs);
    let la = ta
        .lookahead(vec![act(&action)])
        .unwrap_or_else(|e| panic!("{url}: {e}"));
    assert!(la.error.is_none() && la.pending.is_none(), "{url}: {la}");
    let mut b = sandbox(Replay::new(log.clone()));
    let mut tb = b.open(url, options).unwrap_or_else(|e| panic!("{e}"));
    tb.observe().unwrap_or_else(|e| panic!("{e}"));
    tb.act(act(&action)).unwrap_or_else(|e| panic!("{e}"));
    let real = tb.observe().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        la.observation.as_deref(),
        Some(real.as_str()),
        "{url}: {action}"
    );
    // The tab itself did not move.
    let after = ta.observe().unwrap_or_else(|e| panic!("{e}"));
    assert!(after.contains("no change"), "{url}: {after}");
}

#[test]
fn a_lookahead_shows_what_acting_would_show() {
    // A corpus page with a cross-origin frame.
    lookahead_vs_acting(
        &fixtures(),
        &format!("{ORIGIN}xorigin-iframe.html"),
        corpus_options(),
        &|obs| format!("click {}", find(obs, "button", "Back")),
    );
    // MiniWoB++ pages: scripts, timers, randomness.
    let Some(mw) = miniwob_log() else { return };
    for task in [
        "click-button",
        "enter-text",
        "login-user",
        "click-checkboxes",
        "choose-list",
    ] {
        lookahead_vs_acting(
            &mw,
            &format!("{MINIWOB_ORIGIN}miniwob/{task}.html"),
            LoadOptions::default(),
            &|obs| match refs(obs, "textbox").first() {
                Some((r, _)) => format!("type {r} ada"),
                None => {
                    let (r, _) = refs(obs, "button")
                        .into_iter()
                        .chain(refs(obs, "checkbox"))
                        .chain(refs(obs, "combobox"))
                        .next()
                        .unwrap_or_else(|| panic!("nothing to act on:\n{obs}"));
                    format!("click {r}")
                }
            },
        );
    }
}

const COUNTER: &str = "<title>Shop</title><p id=n>0</p>\
    <button onclick=\"n.textContent = +n.textContent + 1; localStorage.k = 'branch'; document.cookie = 'c=branch'\">Add</button>\
    <button onclick=\"for (let i = 0; i < 16; i++) { let a = new Array(1e6).fill(i); } n.textContent = 'heavy'\">Heavy</button>\
    <script>let t = 0; setInterval(() => { t++ }, 100)</script>";

fn counter_site() -> pageon_env::Log {
    site(vec![html("https://shop.test/", COUNTER)])
}

#[test]
fn the_main_line_is_untouched() {
    let mut sb = sandbox(Replay::new(counter_site()));
    let mut tab = sb
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    let add = find(&obs, "button", "Add");
    let html = |tab: &mut pageon::RemoteTab<'_, Replay>| {
        tab.view(Dump::Html, Viewport::default(), Budget::o200k(10_000))
            .unwrap_or_else(|e| panic!("{e}"))
    };
    let before = html(&mut tab);
    for _ in 0..50 {
        let la = tab
            .lookahead(vec![act(&format!("click {add}"))])
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            la.observation
                .as_deref()
                .unwrap_or_default()
                .contains("text \"1\""),
            "{la}"
        );
    }
    assert_eq!(html(&mut tab), before, "the page changed");
    assert!(
        tab.observe()
            .unwrap_or_else(|e| panic!("{e}"))
            .contains("no change")
    );
    drop(tab);
    // Branch cookie and localStorage writes never reach the main line.
    let state = sb.storage_state().unwrap_or_else(|e| panic!("{e}"));
    let text = serde_json::to_string(&state).unwrap_or_default();
    assert!(!text.contains("branch"), "{text}");
}

#[test]
fn a_fork_is_sound_under_memory_pressure() {
    // The fork test: scripts, timers and a major GC (allocation pressure) in the child; the
    // parent then runs on correctly.
    let mut sb = sandbox(Replay::new(counter_site()));
    let mut tab = sb
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    let heavy = find(&obs, "button", "Heavy");
    let add = find(&obs, "button", "Add");
    let la = tab
        .lookahead(vec![
            act(&format!("click {heavy}")),
            act("wait 1000"),
            act(&format!("click {heavy}")),
        ])
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(la.outcomes.len(), 3, "{la}");
    assert!(
        la.observation
            .as_deref()
            .unwrap_or_default()
            .contains("heavy"),
        "{la}"
    );
    let out = tab
        .act(act(&format!("click {add}")))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(out.to_string().starts_with("ok click"));
    assert!(
        tab.observe()
            .unwrap_or_else(|e| panic!("{e}"))
            .contains("text \"1\"")
    );
}

#[test]
fn failures_and_empty_lookaheads() {
    let mut sb = sandbox(Replay::new(counter_site()));
    let mut tab = sb
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    let add = find(&obs, "button", "Add");
    let la = tab
        .lookahead(vec![
            act(&format!("click {add}")),
            act("click e999"),
            act(&format!("click {add}")),
        ])
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(la.outcomes.len(), 1);
    assert!(
        matches!(la.error, Some(pageon::ActionError::UnknownRef(_))),
        "{la}"
    );
    assert!(la.to_string().contains("observe first"), "guidance: {la}");
    let la = tab.lookahead(vec![]).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        la.observation
            .as_deref()
            .unwrap_or_default()
            .contains("no change"),
        "{la}"
    );
}

const SHOP: &str = "<title>Pay</title>\
    <form method=post action=/orders><button>Pay now</button></form>\
    <button onclick=\"fetch('/api', {method: 'POST', body: 'x'})\">Save</button>\
    <button onclick=\"let x = new XMLHttpRequest(); x.open('PUT', '/api'); x.send('y')\">Put</button>\
    <button onclick=\"fetch('/price').then(r => r.text()).then(t => out.textContent = t)\">Price</button>\
    <p id=out></p>";

#[test]
fn writes_are_held_never_sent() {
    let cap = Capture::new(site(vec![
        html("https://shop.test/", SHOP),
        ex(
            "POST",
            "https://shop.test/orders",
            200,
            &[("content-type", "text/html")],
            "<h1>Paid</h1>",
        ),
        ex("POST", "https://shop.test/api", 200, &[], "saved"),
        ex("PUT", "https://shop.test/api", 200, &[], "put"),
        page(
            "https://shop.test/price",
            200,
            &[("content-type", "text/plain")],
            "42 EUR",
        ),
    ]));
    let mut sb = sandbox(&cap);
    let mut tab = sb
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
    for (name, method, url) in [
        ("Pay now", "POST", "https://shop.test/orders"),
        ("Save", "POST", "https://shop.test/api"),
        ("Put", "PUT", "https://shop.test/api"),
    ] {
        let la = tab
            .lookahead(vec![act(&format!("click {}", find(&obs, "button", name)))])
            .unwrap_or_else(|e| panic!("{e}"));
        let p = la
            .pending
            .clone()
            .unwrap_or_else(|| panic!("{name}: nothing held: {la}"));
        assert_eq!((p.method.as_str(), p.url.as_str()), (method, url));
        assert!(la.observation.is_none());
        assert!(
            la.to_string().contains(&format!("pending: {method} {url}")),
            "{la}"
        );
    }
    let la = tab
        .lookahead(vec![act(&format!(
            "click {}",
            find(&obs, "button", "Price")
        ))])
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(la.pending.is_none(), "{la}");
    assert!(
        la.observation
            .as_deref()
            .unwrap_or_default()
            .contains("42 EUR"),
        "a read is served: {la}"
    );
    // The summary says what went out: the read, and no write.
    assert!(
        la.to_string()
            .starts_with("lookahead (the page is unchanged; writes were held, 1 read(s) sent)"),
        "{la}"
    );
    let sent: Vec<String> = cap
        .urls()
        .into_iter()
        .filter(|u| !u.starts_with("GET "))
        .collect();
    assert!(sent.is_empty(), "a write reached the environment: {sent:?}");
}

#[test]
fn lookaheads_leave_recordings_exact() {
    let rec = Recording::new(Replay::new(counter_site()));
    {
        let mut sb = sandbox(&rec);
        let mut tab = sb
            .open("https://shop.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
        let add = find(&obs, "button", "Add");
        tab.lookahead(vec![act(&format!("click {add}"))])
            .unwrap_or_else(|e| panic!("{e}"));
        tab.act(act(&format!("click {add}")))
            .unwrap_or_else(|e| panic!("{e}"));
        tab.lookahead(vec![act(&format!("click {add}")), act("wait 500")])
            .unwrap_or_else(|e| panic!("{e}"));
        tab.act(act(&format!("click {add}")))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    let log = rec.log();
    assert_eq!(log.steps.len(), 3, "lookaheads are not steps");
    let replay = Recording::resume(log.clone(), 2, Replay::new(log.clone()), true)
        .unwrap_or_else(|e| panic!("{e}"));
    let mut sb = sandbox(&replay);
    let mut driver = Caged::new(&mut sb);
    rerun(&mut driver, &log.steps, 2, &mut |_, _| {}).unwrap_or_else(|d| panic!("diverged: {d}"));
}

#[test]
#[ignore = "timing: cargo test --release -p pageon --test fork -- --ignored --nocapture"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn lookahead_beats_act_then_reset() {
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    // (name, site, url, options, actions before the lookahead, how to pick the action)
    type Pick = fn(&str) -> String;
    let first: Pick = |obs| match refs(obs, "textbox").first() {
        Some((r, _)) => format!("type {r} ada"),
        None => format!("click {}", refs(obs, "button")[0].0),
    };
    let login: Pick = |obs| format!("click {}", find(obs, "button", "Login"));
    let mut cases: Vec<(&str, pageon_env::Log, String, LoadOptions, usize, Pick)> = vec![(
        "corpus xorigin-iframe",
        fixtures(),
        format!("{ORIGIN}xorigin-iframe.html"),
        corpus_options(),
        0,
        first,
    )];
    if let Some(mw) = miniwob_log() {
        let url = format!("{MINIWOB_ORIGIN}miniwob/login-user.html");
        cases.push((
            "MiniWoB++ login-user",
            mw.clone(),
            url.clone(),
            LoadOptions::default(),
            0,
            first,
        ));
        cases.push((
            "MiniWoB++ login-user, 2 steps in",
            mw,
            url,
            LoadOptions::default(),
            2,
            login,
        ));
    }
    for (name, log, url, options, depth, pick) in cases {
        // The steps before the lookahead: type into the first `depth` text boxes.
        let prefix = |obs: &str| -> Vec<String> {
            refs(obs, "textbox")
                .iter()
                .take(depth)
                .map(|(r, _)| format!("type {r} ada"))
                .collect()
        };
        // L1: lookahead.
        let mut sb = sandbox(Replay::new(log.clone()));
        let mut tab = sb.open(&url, options).unwrap_or_else(|e| panic!("{e}"));
        let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
        for a in prefix(&obs) {
            tab.act(act(&a)).unwrap_or_else(|e| panic!("{e}"));
        }
        let obs = tab.observe_full().unwrap_or_else(|e| panic!("{e}"));
        let target = pick(&obs);
        let mut l1 = Vec::new();
        for _ in 0..20 {
            let t = std::time::Instant::now();
            tab.lookahead(vec![act(&target)])
                .unwrap_or_else(|e| panic!("{e}"));
            l1.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        drop(tab);
        // L0: act, then reset to the step before it: a fresh session re-runs the
        // steps so far from the session's recording.
        let mut l0 = Vec::new();
        for _ in 0..20 {
            let rec = Recording::new(Replay::new(log.clone()));
            let mut sb = sandbox(&rec);
            let mut d = Caged::new(&mut sb);
            pageon::replay::Driver::open(&mut d, &url, options).unwrap_or_else(|e| panic!("{e}"));
            let obs = pageon::replay::Driver::observe(&mut d).unwrap_or_else(|e| panic!("{e}"));
            for a in prefix(&obs) {
                pageon::replay::Driver::act(&mut d, act(&a)).unwrap_or_else(|e| panic!("{e}"));
            }
            pageon::replay::Driver::observe(&mut d).unwrap_or_else(|e| panic!("{e}"));
            let t = std::time::Instant::now();
            pageon::replay::Driver::act(&mut d, act(&target)).unwrap_or_else(|e| panic!("{e}"));
            pageon::replay::Driver::observe(&mut d).unwrap_or_else(|e| panic!("{e}"));
            let steps = pageon_env::Recorder::steps(&rec);
            pageon_env::Recorder::rewind(&rec, depth).unwrap_or_else(|e| panic!("{e}"));
            let _ = d;
            sb.clear().unwrap_or_else(|e| panic!("{e}"));
            let mut d = Caged::new(&mut sb);
            rerun(&mut d, &steps, depth, &mut |_, _| {}).unwrap_or_else(|e| panic!("{e}"));
            l0.push(t.elapsed().as_secs_f64() * 1000.0);
        }
        let (a, b) = (median(l1), median(l0));
        println!("{name}: lookahead {a:.1} ms vs act + reset {b:.1} ms (median of 20)");
        // A static page at step 0 re-opens from the recording faster than a fork on macOS
        // (fork ≈ 4.5 ms there): reported, not asserted. Scripted pages are where L1 wins.
        if name.starts_with("MiniWoB") {
            assert!(
                a < b,
                "{name}: lookahead {a:.1} ms is not faster than act + reset {b:.1} ms"
            );
            assert!(a <= 30.0, "{name}: {a:.1} ms");
        }
    }
}
