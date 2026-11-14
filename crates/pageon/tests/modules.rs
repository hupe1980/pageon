//! Script loading: module scripts, import maps, dynamic `import()`, inserted
//! scripts. Real bundler output (esbuild ESM, webpack 5) and hand-written conformance pages,
//! replayed offline, in-process and caged.

mod common;

use common::*;
use pageon::{Budget, Dump, LoadOptions, Replay, Sandbox, SandboxOptions, Session, Viewport};

const JS: &[(&str, &str)] = &[("content-type", "text/javascript")];

fn js(url: &str, body: &str) -> pageon_env::Exchange {
    page(url, 200, JS, body)
}

macro_rules! bundle {
    ($p:literal) => {
        include_str!(concat!("fixtures/bundles/", $p))
    };
}

/// The esbuild (ESM, code splitting) and webpack 5 (JSONP chunks) builds of the same app.
fn bundles() -> pageon_env::Log {
    let shell =
        |script: &str| format!("<!doctype html><title>Shop</title><div id=app></div>{script}");
    site(vec![
        html(
            "https://esm.test/",
            &shell(r#"<script type=module src="/main.js"></script>"#),
        ),
        js("https://esm.test/main.js", bundle!("esbuild/main.js")),
        js(
            "https://esm.test/chunk-KYN4RSRL.js",
            bundle!("esbuild/chunk-KYN4RSRL.js"),
        ),
        js(
            "https://esm.test/chunk-5S6S7CKR.js",
            bundle!("esbuild/chunk-5S6S7CKR.js"),
        ),
        html(
            "https://wp.test/",
            &shell(r#"<script src="/main.js"></script>"#),
        ),
        js("https://wp.test/main.js", bundle!("webpack/main.js")),
        js(
            "https://wp.test/route.chunk.js",
            bundle!("webpack/route.chunk.js"),
        ),
        html(
            "https://iife.test/",
            &shell(r#"<script src="/main.js"></script>"#),
        ),
        js("https://iife.test/main.js", bundle!("iife/main.js")),
        // The webpack app on a server that lost its chunk.
        html(
            "https://broken.test/",
            &shell(r#"<script src="/main.js"></script>"#),
        ),
        js("https://broken.test/main.js", bundle!("webpack/main.js")),
        page("https://broken.test/route.chunk.js", 404, &[], "not found"),
    ])
}

fn open<'a>(session: &'a Session<Replay>, url: &str) -> pageon::Tab<'a, Replay> {
    let mut tab = session
        .open(url, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{url}: {e}"));
    tab.set_view(Budget::o200k(4000), pageon::Mode::Full);
    tab
}

fn errors(tab: &pageon::Tab<'_, Replay>) -> String {
    tab.view(Dump::Errors, Viewport::default(), Budget::o200k(4000))
}

fn requests(tab: &pageon::Tab<'_, Replay>) -> String {
    tab.view(Dump::Requests, Viewport::default(), Budget::o200k(4000))
}

#[test]
fn esbuild_esm_app_renders_and_loads_its_route() {
    let session = Session::new(Replay::new(bundles()));
    let mut tab = open(&session, "https://esm.test/");
    let obs = tab.observe();
    assert!(obs.contains("Module shop"), "{obs}");
    assert!(
        text_of(tab.document(), "app").contains("Open route"),
        "{obs}"
    );
    let reqs = requests(&tab);
    assert_eq!(reqs.matches("chunk-KYN4RSRL.js").count(), 1, "{reqs}");
    assert!(
        !reqs.contains("chunk-5S6S7CKR.js"),
        "the route is lazy: {reqs}"
    );
    let link = find(&obs, "link", "Open route");
    let out = tab
        .act(
            format!("click {link}")
                .parse()
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        out.requests
            .iter()
            .any(|r| r.url.ends_with("chunk-5S6S7CKR.js")),
        "{out:?}"
    );
    let obs = tab.observe();
    assert!(obs.contains("Route loaded"), "{obs}");
    let errs = errors(&tab);
    assert!(!errs.contains("unsupported"), "{errs}");
    assert!(tab.document().errors().is_empty(), "{errs}");
}

#[test]
fn webpack_app_loads_chunks_by_inserted_scripts() {
    let session = Session::new(Replay::new(bundles()));
    let mut tab = open(&session, "https://wp.test/");
    let obs = tab.observe();
    assert!(obs.contains("Webpack shop"), "{obs}");
    let link = find(&obs, "link", "Open route");
    tab.act(
        format!("click {link}")
            .parse()
            .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    assert!(obs.contains("Route loaded"), "{obs}");
    assert!(tab.document().errors().is_empty(), "{}", errors(&tab));
    // A missing chunk fires `error`: the app shows its own error state instead of hanging.
    let mut tab = open(&session, "https://broken.test/");
    let obs = tab.observe();
    let link = find(&obs, "link", "Open route");
    tab.act(
        format!("click {link}")
            .parse()
            .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let failed = text_of(tab.document(), "failed");
    assert!(failed.starts_with("Could not load the route"), "{failed}");
    assert!(errors(&tab).contains("route.chunk.js"), "{}", errors(&tab));
}

/// A page that writes what it saw into `#out`, one line per check.
fn conformance() -> pageon_env::Log {
    site(vec![
        html(
            "https://m.test/",
            r#"<pre id=out></pre>
<script>window.log = (s) => { out.textContent += s + '\n' }</script>
<script type=importmap>{"imports": {"lib": "/vendor/lib.js", "util/": "/vendor/util/"},
  "scopes": {"/scoped/": {"lib": "/vendor/lib2.js"}}}</script>
<script nomodule src="/legacy.js"></script>
<script>log('supports ' + ['classic', 'module', 'importmap', 'json', 'speculationrules'].map(t => HTMLScriptElement.supports(t)).join(','))</script>
<script type=module>
  import { name } from 'lib';
  import { twice } from 'util/math.js';
  import data from './data.json' with { type: 'json' };
  import { a } from './cycle-a.js';
  import { scoped } from './scoped/s.js';
  import * as again from '/vendor/lib.js';
  log('map ' + name + ' ' + twice(21) + ' ' + data.items.length + ' ' + a() + ' ' + scoped);
  log('one instance ' + (again.counter === 1));
  log('meta ' + import.meta.url + ' ' + import.meta.resolve('lib'));
  log('tla ' + await new Promise(r => setTimeout(() => r('waited'), 50)));
</script>
<script type=module>
  import('nope').catch(e => log('bare ' + e.name + ': ' + e.message));
  import('./missing.js').catch(e => log('missing ' + e.message));
  import('./data.json', { with: { type: 'css' } }).catch(e => log('attr ' + e.name));
</script>
<script type=module src="/html-served.js"></script>
<script type=module src="/throws.js"></script>
<script type=module src="/after-throw.js"></script>
<script>
  import('/dyn.js').then(m => log('classic import ' + m.v));
  const s = document.createElement('script'); s.src = '/b.js'; s.async = false;
  const t = document.createElement('script'); t.src = '/a.js'; t.async = false;
  s.onload = () => log('onload b'); document.head.append(s, t);
  const bad = document.createElement('script'); bad.src = '/404.js';
  bad.onerror = () => log('onerror 404'); document.head.append(bad);
  const inline = document.createElement('script'); inline.id = 'inl';
  inline.textContent = "log('inline ' + document.currentScript.id)"; document.body.append(inline);
  log('after append');
  document.body.insertAdjacentHTML('beforeend', '<div id=d></div>');
  d.innerHTML = "<script>log('innerHTML ran')<\/script>";
</script>"#,
        ),
        page("https://m.test/legacy.js", 200, JS, "log('legacy ran')"),
        page(
            "https://m.test/vendor/lib.js",
            200,
            JS,
            "export const name = 'lib'; globalThis.libLoads = (globalThis.libLoads || 0) + 1; export const counter = globalThis.libLoads;",
        ),
        page(
            "https://m.test/vendor/lib2.js",
            200,
            JS,
            "export const name = 'lib2';",
        ),
        page(
            "https://m.test/vendor/util/math.js",
            200,
            JS,
            "export const twice = (x) => x * 2;",
        ),
        page(
            "https://m.test/data.json",
            200,
            &[("content-type", "application/json")],
            r#"{"items": [1, 2, 3]}"#,
        ),
        page(
            "https://m.test/cycle-a.js",
            200,
            JS,
            "import { b } from './cycle-b.js'; export const a = () => 'a' + b();",
        ),
        page(
            "https://m.test/cycle-b.js",
            200,
            JS,
            "import { a } from './cycle-a.js'; export const b = () => 'b';",
        ),
        page(
            "https://m.test/scoped/s.js",
            200,
            JS,
            "import { name } from 'lib'; export const scoped = name;",
        ),
        page(
            "https://m.test/html-served.js",
            200,
            &[("content-type", "text/html")],
            "log('html-served ran')",
        ),
        page(
            "https://m.test/throws.js",
            200,
            JS,
            "throw new Error('boom in module');",
        ),
        page(
            "https://m.test/after-throw.js",
            200,
            JS,
            "import './throws.js'; log('dependent ran');",
        ),
        page("https://m.test/dyn.js", 200, JS, "export const v = 'ok';"),
        page("https://m.test/a.js", 200, JS, "log('a')"),
        page("https://m.test/b.js", 200, JS, "log('b')"),
        page("https://m.test/404.js", 404, &[], "missing"),
        page("https://m.test/missing.js", 404, &[], "missing"),
    ])
}

#[test]
fn modules_import_maps_and_inserted_scripts_follow_html() {
    let session = Session::new(Replay::new(conformance()));
    let tab = open(&session, "https://m.test/");
    let out = text_of(tab.document(), "out");
    let lines: Vec<&str> = out.lines().collect();
    let has = |l: &str| lines.contains(&l);
    assert!(has("supports true,true,true,false,false"), "{out}");
    assert!(has("map lib 42 3 ab lib2"), "{out}");
    assert!(has("one instance true"), "{out}");
    assert!(
        has("meta https://m.test/ https://m.test/vendor/lib.js"),
        "{out}"
    );
    assert!(has("tla waited"), "{out}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("bare TypeError: Failed to resolve module specifier \"nope\"")),
        "{out}"
    );
    assert!(
        has("missing Failed to fetch dynamically imported module: https://m.test/missing.js"),
        "{out}"
    );
    assert!(has("attr TypeError"), "{out}");
    assert!(has("classic import ok"), "{out}");
    // Inserted scripts: ordered ones in insertion order, then `load`; a 404 fires `error`; an
    // inline one runs after the inserting task with `currentScript`; innerHTML never runs.
    let pos = |l: &str| lines.iter().position(|x| *x == l).unwrap_or(usize::MAX);
    assert!(pos("after append") < pos("inline inl"), "{out}");
    assert!(pos("b") < pos("a") && pos("a") != usize::MAX, "{out}");
    assert!(has("onload b"), "{out}");
    assert!(has("onerror 404"), "{out}");
    assert!(!out.contains("innerHTML ran"), "{out}");
    assert!(!out.contains("legacy ran"), "{out}");
    assert!(!out.contains("html-served ran"), "{out}");
    assert!(!out.contains("dependent ran"), "{out}");
    let reqs = requests(&tab);
    assert!(
        !reqs.contains("legacy.js"),
        "nomodule is never fetched: {reqs}"
    );
    assert_eq!(reqs.matches("vendor/lib.js").count(), 1, "{reqs}");
    let errs = errors(&tab);
    assert!(
        errs.contains("MIME type of \\\"text/html\\\"")
            || errs.contains("MIME type of \"text/html\""),
        "{errs}"
    );
    assert!(errs.contains("boom in module"), "{errs}");
    assert!(!errs.contains("not supported"), "{errs}");
}

#[test]
fn late_import_maps_merge_and_refused_imports_say_why() {
    let replay = Replay::new(site(vec![
        html(
            "https://late.test/",
            r#"<p id=out></p>
<script type=module>import('./x.js').then(() => out.textContent += 'x ')</script>
<script type=importmap>{"imports": {"y": "/y.js"}}</script>
<script type=module>import('y').then(() => out.textContent += 'y ')</script>
<script type=module>import('https://cdn.test/lib.js').then(() => out.textContent += 'cdn ', e => out.textContent += 'refused ')</script>"#,
        ),
        page("https://late.test/x.js", 200, JS, ""),
        page("https://late.test/y.js", 200, JS, ""),
        page(
            "https://cdn.test/lib.js",
            200,
            &[
                ("content-type", "text/javascript"),
                ("access-control-allow-origin", "*"),
            ],
            "",
        ),
    ]));
    // The egress policy keeps the page to its own origin.
    let env = pageon::Policy::new(&replay).read_write(["https://late.test"]);
    let session = Session::new(&env);
    let tab = session
        .open("https://late.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let out = text_of(tab.document(), "out");
    assert!(out.contains("x "), "{out}");
    assert!(out.contains("refused"), "{out}");
    let errs = tab.view(Dump::Errors, Viewport::default(), Budget::o200k(4000));
    assert!(out.contains("y "), "a later import map is merged: {out}");
    assert!(errs.contains("outside the task's origins"), "{errs}");
}

#[test]
fn caged_equals_in_process_and_replays() {
    let log = bundles();
    let mut views = Vec::new();
    for caged in [false, true] {
        let replay = Replay::new(log.clone());
        let mut seen = Vec::new();
        if caged {
            let mut sb = Sandbox::new(
                &replay,
                SandboxOptions {
                    program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                    ..SandboxOptions::default()
                },
            )
            .unwrap_or_else(|e| panic!("{e}"));
            for url in ["https://esm.test/", "https://wp.test/"] {
                let mut tab = sb
                    .open(url, LoadOptions::default())
                    .unwrap_or_else(|e| panic!("{e}"));
                let obs = tab.observe().unwrap_or_else(|e| panic!("{e}"));
                let link = find(&obs, "link", "Open route");
                tab.act(
                    format!("click {link}")
                        .parse()
                        .unwrap_or_else(|e| panic!("{e}")),
                )
                .unwrap_or_else(|e| panic!("{e}"));
                seen.push(tab.observe().unwrap_or_else(|e| panic!("{e}")));
            }
        } else {
            let session = Session::new(&replay);
            for url in ["https://esm.test/", "https://wp.test/"] {
                let mut tab = session
                    .open(url, LoadOptions::default())
                    .unwrap_or_else(|e| panic!("{e}"));
                let obs = tab.observe();
                let link = find(&obs, "link", "Open route");
                tab.act(
                    format!("click {link}")
                        .parse()
                        .unwrap_or_else(|e| panic!("{e}")),
                )
                .unwrap_or_else(|e| panic!("{e}"));
                seen.push(tab.observe());
            }
        }
        assert!(seen.iter().all(|o| o.contains("Route loaded")), "{seen:?}");
        views.push(seen);
    }
    assert_eq!(views[0], views[1], "caged and in-process differ");
}

#[test]
#[ignore = "timing: cargo test --release -p pageon --test modules -- --ignored --nocapture"]
#[allow(clippy::disallowed_methods)]
fn modules_cost_little() {
    let mut log = bundles();
    // Replay answers each recorded exchange once: one copy per open.
    let one = log.exchanges.clone();
    for _ in 0..25 {
        log.exchanges.extend(one.iter().cloned());
    }
    log.clock = (0..100_000).map(|i| 1_800_000_000_000 + i * 10).collect();
    let session = Session::new(Replay::new(log));
    let time = |url: &str| {
        let mut t: Vec<f64> = (0..20)
            .map(|_| {
                let s = std::time::Instant::now();
                let tab = session
                    .open(url, LoadOptions::default())
                    .unwrap_or_else(|e| panic!("{e}"));
                drop(tab);
                s.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        t.sort_by(f64::total_cmp);
        t[10]
    };
    let (esm, iife) = (time("https://esm.test/"), time("https://iife.test/"));
    println!(
        "open: ESM {esm:.2} ms, classic bundle {iife:.2} ms ({:.2}×)",
        esm / iife
    );
    assert!(
        esm <= iife * 1.5 + 0.5,
        "ESM open within 1.5× of a classic bundle"
    );
}
