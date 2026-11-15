//! Platform basics: the History API through the tab, and the globals single-page
//! apps and component libraries call at startup.

mod common;

use common::*;
use pageon::replay::{Caged, InProcess, rerun};
use pageon::{LoadOptions, Recording, Replay, Sandbox, SandboxOptions, Session};

const VUE_APP: &str = include_str!("fixtures/platform/vue-app.html");
const CHECKS: &str = include_str!("fixtures/platform/checks.html");
const POPOVER: &str = include_str!("fixtures/platform/popover.html");

fn js(url: &str, body: &str) -> pageon_env::Exchange {
    page(url, 200, &[("content-type", "text/javascript")], body)
}

/// A Vue 3 app with Vue Router's web history (the real libraries, vendored).
fn vue_site() -> pageon_env::Log {
    site(vec![
        html("https://vue.test/", VUE_APP),
        js(
            "https://vue.test/vendor/vue.js",
            include_str!("fixtures/vue/vue.js"),
        ),
        js(
            "https://vue.test/vendor/vue-router.js",
            include_str!("fixtures/vue/vue-router.js"),
        ),
    ])
}

const SPA: &str = r#"<!doctype html><title>Shop</title>
<nav><a href="/orders" data-link>Orders</a> <a href="/settings" data-link>Settings</a>
<a href="https://other.test/">Elsewhere</a></nav>
<main id=view>Home</main>
<script>
const render = () => { view.textContent = location.pathname === '/' ? 'Home' : 'Page ' + location.pathname; };
document.addEventListener('click', (e) => {
  const a = e.target.closest('a[data-link]');
  if (!a) return;
  e.preventDefault();
  history.pushState({ path: a.getAttribute('href') }, '', a.getAttribute('href'));
  render();
});
addEventListener('popstate', (e) => { render(); view.dataset.state = JSON.stringify(e.state); });
render();
</script>"#;

fn spa() -> Replay {
    Replay::new(site(vec![
        html("https://shop.test/", SPA),
        html(
            "https://other.test/",
            "<title>Other</title><p>elsewhere</p>",
        ),
        html("https://shop.test/settings", SPA),
    ]))
}

fn act<E: pageon_env::Env>(tab: &mut pageon::Tab<'_, E>, a: &str) -> String {
    tab.act(a.parse().unwrap_or_else(|e| panic!("{e}")))
        .map_or_else(|e| format!("error {e}"), |o| o.to_string())
}

#[test]
fn single_page_apps_route_in_place_and_back() {
    let env = spa();
    let session = Session::new(&env);
    let mut tab = session
        .open("https://shop.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    let orders = find(&obs, "link", "Orders");
    let settings = find(&obs, "link", "Settings");
    let out = act(&mut tab, &format!("click {orders}"));
    assert!(
        out.contains("navigated to \"https://shop.test/orders\" (same page)"),
        "{out}"
    );
    assert_eq!(text_of(tab.document(), "view"), "Page /orders");
    assert_eq!(tab.url(), "https://shop.test/orders");
    act(&mut tab, &format!("click {settings}"));
    assert_eq!(tab.history().0.len(), 3);
    let out = act(&mut tab, "back");
    assert!(
        out.contains("navigated to \"https://shop.test/orders\" (same page)"),
        "{out}"
    );
    assert_eq!(text_of(tab.document(), "view"), "Page /orders");
    assert_eq!(
        tab.document()
            .dump(pageon::Dump::Ax)
            .lines()
            .next()
            .map(|l| l.contains("https://shop.test/orders")),
        Some(true)
    );
    let out = act(&mut tab, "back");
    assert!(out.contains("(same page)"), "{out}");
    assert_eq!(text_of(tab.document(), "view"), "Home");
    let out = act(&mut tab, "forward");
    assert!(out.contains("/orders"), "{out}");
    println!("{}", tab.observe());
}

#[test]
fn a_vue_router_app_routes_in_place_back_and_forward() {
    let env = Replay::new(vue_site());
    let session = Session::new(&env);
    let mut tab = session
        .open("https://vue.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        tab.document().errors().is_empty(),
        "{:?}",
        tab.document().errors()
    );
    let obs = tab.observe();
    assert!(obs.contains("\"Welcome\""), "{obs}");
    let out = act(&mut tab, &format!("click {}", find(&obs, "link", "Orders")));
    assert!(
        out.contains("navigated to \"https://vue.test/orders\" (same page)"),
        "{out}"
    );
    let obs = tab.observe_full();
    assert!(
        obs.contains("heading \"Orders\"") || obs.contains("\"Orders\""),
        "{obs}"
    );
    let out = act(
        &mut tab,
        &format!("click {}", find(&obs, "link", "Order 7")),
    );
    assert!(out.contains("/orders/7\" (same page)"), "{out}");
    assert!(tab.observe_full().contains("Order 7"));
    let out = act(&mut tab, "back");
    assert!(
        out.contains("navigated to \"https://vue.test/orders\" (same page)"),
        "{out}"
    );
    let obs = tab.observe_full();
    assert!(
        obs.contains("link e") && obs.contains("\"Order 7\""),
        "the orders view again: {obs}"
    );
    act(&mut tab, "back");
    assert!(tab.observe_full().contains("\"Welcome\""));
    let out = act(&mut tab, "forward");
    assert!(out.contains("/orders\" (same page)"), "{out}");
    assert!(
        tab.document().errors().is_empty(),
        "{:?}",
        tab.document().errors()
    );
}

#[test]
fn spa_sessions_replay_caged_and_in_process() {
    let rec = Recording::new(Replay::new(vue_site()));
    {
        let session = Session::new(&rec);
        let mut tab = session
            .open("https://vue.test/", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let obs = tab.observe();
        act(&mut tab, &format!("click {}", find(&obs, "link", "Orders")));
        let obs = tab.observe();
        act(
            &mut tab,
            &format!("click {}", find(&obs, "link", "Order 7")),
        );
        let _ = tab.observe();
        act(&mut tab, "back");
    }
    let log = rec.log();
    let last = log.steps.len() - 1;
    for caged in [false, true] {
        let again = Recording::resume(log.clone(), last, Replay::new(log.clone()), true)
            .unwrap_or_else(|e| panic!("{e}"));
        let r = if caged {
            let mut sb = Sandbox::new(
                &again,
                SandboxOptions {
                    program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                    ..SandboxOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
            rerun(&mut Caged::new(&mut sb), &log.steps, last, &mut |_, _| {})
        } else {
            let session = Session::new(&again);
            rerun(
                &mut InProcess::new(&session),
                &log.steps,
                last,
                &mut |_, _| {},
            )
        };
        r.unwrap_or_else(|d| panic!("caged={caged}: {}", pageon::replay::report(&d)));
    }
}

fn check_results(doc: &pageon::Document) -> Vec<(String, bool)> {
    let out = text_of(doc, "out");
    serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("{e}: {out:?}\nerrors: {:#?}", doc.errors()))
}

#[test]
fn the_platform_checks_pass() {
    let env = Replay::new(site(vec![html("https://checks.test/", CHECKS)]));
    let doc = pageon::load("https://checks.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let results = check_results(&doc);
    let failed: Vec<&String> = results
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(n, _)| n)
        .collect();
    assert!(results.len() >= 70, "{}", results.len());
    assert!(
        failed.is_empty(),
        "failed: {failed:#?}\nerrors: {:#?}",
        doc.errors()
    );
    assert!(doc.errors().is_empty(), "{:#?}", doc.errors());
}

fn sandbox<E: pageon_env::Env>(env: E) -> Sandbox<E> {
    Sandbox::new(
        env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn the_platform_checks_pass_caged() {
    let env = Replay::new(site(vec![html("https://checks.test/", CHECKS)]));
    let mut sb = sandbox(&env);
    let mut tab = sb
        .open("https://checks.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let text = tab
        .view(
            pageon::Dump::Text,
            pageon::Viewport::default(),
            pageon::Budget::o200k(100_000),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let json = text
        .find("[[")
        .map(|i| text[i..].lines().next().unwrap_or_default())
        .unwrap_or_else(|| panic!("{text}"));
    let results: Vec<(String, bool)> = serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        results.len() >= 70 && results.iter().all(|(_, ok)| *ok),
        "{results:?}"
    );
}

#[test]
fn a_floating_ui_popover_is_positioned() {
    let env = Replay::new(site(vec![
        html("https://ui.test/", POPOVER),
        js(
            "https://ui.test/vendor/floating-ui.dom.js",
            include_str!("fixtures/floating-ui/floating-ui.dom.js"),
        ),
        js(
            "https://ui.test/vendor/floating-ui.core.js",
            include_str!("fixtures/floating-ui/floating-ui.core.js"),
        ),
    ]));
    let doc = pageon::load("https://ui.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(doc.errors().is_empty(), "{:#?}", doc.errors());
    // Centred under the 80 px anchor at 100,200 (30 high), 4 px below: (100 + 40 - 30, 234).
    assert_eq!(text_of(&doc, "out"), "placed 110,234");
}

#[test]
fn a_page_cannot_flood_or_trap_the_history() {
    let page = r#"<title>Trap</title><a id=go href="/next">Next</a><p id=out></p><script>
      // 60 entries on load, without user activation: a back-button trap.
      for (let i = 0; i < 60; i++) history.pushState(i, '', '/trap' + i);
      out.textContent = history.length;
      let big = '';
      try { history.pushState('x'.repeat(17 << 20), ''); } catch (e) { big = e.name; }
      out.textContent += ' ' + big;
    </script>"#;
    let env = Replay::new(site(vec![
        html(
            "https://start.test/",
            "<title>Start</title><a href='https://trap.test/'>Trap</a>",
        ),
        html("https://trap.test/", page),
        html(
            "https://start.test/",
            "<title>Start</title><p>back again</p>",
        ),
    ]));
    let session = Session::new(&env);
    let mut tab = session
        .open("https://start.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    act(&mut tab, &format!("click {}", find(&obs, "link", "Trap")));
    assert_eq!(
        text_of(tab.document(), "out"),
        "50 DataCloneError",
        "at most 50 entries; 16 MiB of state"
    );
    assert_eq!(tab.history().0.len(), 50);
    // The entries the page pushed without an activation are skipped: back leaves the trap.
    let out = act(&mut tab, "back");
    assert!(
        out.contains("navigated to \"https://start.test/\"") && !out.contains("same page"),
        "{out}"
    );
}

#[test]
fn crypto_values_differ_live_and_replay_exactly() {
    let page = "<p id=out></p><script>out.textContent = crypto.randomUUID() + ' ' + crypto.getRandomValues(new Uint32Array(2)).join('.')</script>";
    let read = |env: &dyn pageon_env::Env| {
        let doc = pageon::load("https://rng.test/", env, LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        text_of(&doc, "out")
    };
    // Live randomness from the OS: two sessions differ (the network is replayed).
    struct LiveRandom(Replay, pageon::Live);
    impl pageon_env::Env for LiveRandom {
        fn fetch(
            &self,
            r: &pageon_env::Request,
        ) -> Result<pageon_env::Response, pageon_env::NetError> {
            self.0.fetch(r)
        }
        fn now_ms(&self) -> Result<u64, pageon_env::NetError> {
            self.0.now_ms()
        }
        fn random_u64(&self) -> Result<u64, pageon_env::NetError> {
            self.1.random_u64()
        }
    }
    let log = site(vec![html("https://rng.test/", page)]);
    let a = read(&LiveRandom(
        Replay::new(log.clone()),
        pageon::Live::default(),
    ));
    let b = read(&LiveRandom(
        Replay::new(log.clone()),
        pageon::Live::default(),
    ));
    assert_ne!(a, b);
    // A recording replays to the same values.
    let rec = Recording::new(LiveRandom(
        Replay::new(log.clone()),
        pageon::Live::default(),
    ));
    let first = read(&rec);
    let again = read(&Replay::new(rec.log()));
    assert_eq!(first, again);
    assert_eq!(
        first.len(),
        36 + 1 + again.split(' ').nth(1).map_or(0, str::len)
    );
}

#[test]
fn a_resize_observer_loop_is_cut_and_reported() {
    let page = r#"<div id=b style="width:10px;height:10px"></div><p id=out></p><script>
      let calls = 0, errors = 0;
      addEventListener('error', () => { errors++; });
      new ResizeObserver(() => { calls++; b.style.width = (10 + calls) + 'px'; }).observe(b);
      setTimeout(() => { out.textContent = calls + ' ' + errors; }, 50);
    </script>"#;
    let env = Replay::new(site(vec![html("https://ro.test/", page)]));
    let doc = pageon::load("https://ro.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = text_of(&doc, "out");
    let (calls, errors): (u32, u32) = {
        let mut it = out.split(' ').map(|x| x.parse().unwrap_or(0));
        (it.next().unwrap_or(0), it.next().unwrap_or(0))
    };
    // Each rendering step delivers at most twice, then reports the loop; the page settles.
    assert!(calls >= 2 && errors >= 1, "{out}");
    assert!(
        doc.errors()
            .iter()
            .any(|e| e.message.contains("ResizeObserver loop completed")),
        "{:?}",
        doc.errors()
    );
}
