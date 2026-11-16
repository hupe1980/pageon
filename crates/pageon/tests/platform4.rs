//! Platform 4, from sites the audit ran on: `error` and `unhandledrejection` events at the
//! window (handled errors are not page errors), handlers in the order they were set, and the
//! CSSOM of `<style>` and `<link>` sheets, whose inserted rules reach layout (CSS-in-JS).

mod common;

use common::*;
use pageon::{LoadOptions, Replay, Sandbox, SandboxOptions, Session};

const CHECKS: &str = r##"<!doctype html><title>checks</title>
<style>a { color: blue }</style><link rel=stylesheet href="/s.css">
<div id=t>Shown until a rule hides it</div><div class=late>Hidden by a linked sheet</div>
<noscript>Enable JavaScript</noscript><svg><a href="#"><rect width=5 height=5 /></a></svg>
<pre id=out>running</pre>
<script>
const results = {};
const ok = (name, cond, detail) => { results[name] = cond ? true : String(detail); };
const order = [];
window.addEventListener('error', (e) => order.push('listener:' + (e.error instanceof TypeError) + ':' + (e.lineno > 0)));
window.onerror = (message, source, line, col, error) => { order.push('onerror:' + (error && error.name)); return message.includes('handled'); };
const rejections = [];
window.addEventListener('unhandledrejection', (e) => { rejections.push(e.reason); if (e.reason === 'quiet') e.preventDefault(); });
ok('PromiseRejectionEvent', new PromiseRejectionEvent('x', { promise: Promise.resolve(), reason: 1 }).reason === 1, typeof PromiseRejectionEvent);
setTimeout(() => { null.x; }, 0);
setTimeout(() => { throw new Error('handled here'); }, 1);
Promise.reject('loud'); Promise.reject('quiet');
// CSSOM.
const s = document.createElement('style');
document.head.appendChild(s);
s.appendChild(document.createTextNode(''));
ok('style.sheet', s.sheet && s.sheet === s.sheet && s.sheet.ownerNode === s, s.sheet);
ok('a detached style has none', document.createElement('style').sheet === null, 'sheet');
ok('document.styleSheets', document.styleSheets.length === 3 && document.styleSheets[0].cssRules[0].cssText === 'a { color: blue }', document.styleSheets.length);
s.sheet.insertRule('#t { display: none }', 0);
s.sheet.insertRule('.y { color: red }', s.sheet.cssRules.length);
ok('insertRule', s.sheet.cssRules.length === 2 && s.sheet.cssRules[1].cssText === '.y { color: red }' && s.textContent === '', s.sheet.cssRules.length);
s.sheet.deleteRule(1);
ok('deleteRule', s.sheet.cssRules.length === 1 && s.sheet.cssRules[0].type === 1, s.sheet.cssRules.length);
let linkThrew = '';
try { document.styleSheets[1].cssRules; } catch (e) { linkThrew = e.name; }
ok('link rules', linkThrew === 'SecurityError' && document.styleSheets[1].href.endsWith('/s.css'), linkThrew);
// Streams load on a page with an AMD loader and CommonJS globals.
let defined = false;
window.define = function () { defined = true; }; window.define.amd = {}; window.module = { exports: {} };
ok('streams beside AMD', new TransformStream().readable instanceof ReadableStream && !defined && !('WebStreamsPolyfill' in window), typeof ReadableStream);
// measure() takes the navigation's timing points as marks.
ok('measure navigationStart', performance.measure('since start', 'navigationStart').startTime === 0, 'threw');
// A thrown DOMException is reported by its name.
setTimeout(() => { document.querySelector(':::'); }, 2);
// SVG element interfaces.
const svgA = document.querySelector('svg a');
ok('SVGAElement', svgA instanceof SVGAElement && svgA instanceof SVGGraphicsElement && document.querySelector('rect') instanceof SVGGeometryElement, String(svgA));
// Inserted links load (as Vite's preloader waits for) and a stylesheet applies.
const linkEvents = [];
for (const [rel, href] of [['stylesheet', '/late.css'], ['stylesheet', '/missing.css'], ['modulepreload', '/m.js']]) {
  const l = document.createElement('link');
  l.rel = rel; l.href = href;
  l.onload = () => linkEvents.push(href + ':load');
  l.onerror = () => linkEvents.push(href + ':error');
  document.head.appendChild(l);
}
setTimeout(() => {
  ok('link events', linkEvents.sort().join() === '/late.css:load,/m.js:load,/missing.css:error', linkEvents.join());
  ok('error order', order.join() === 'listener:true:true,onerror:TypeError,listener:false:true,onerror:Error,listener:false:true,onerror:SyntaxError', order.join());
  ok('rejections', rejections.join() === 'loud,quiet', rejections.join());
  const failures = Object.fromEntries(Object.entries(results).filter(([, v]) => v !== true));
  document.getElementById('out').textContent = JSON.stringify({ count: Object.keys(results).length, ...failures });
}, 50);
</script>"##;

const EXPECTED: usize = 13;

fn log() -> pageon_env::Log {
    site(vec![
        html("https://p.test/", CHECKS),
        page(
            "https://p.test/s.css",
            200,
            &[("content-type", "text/css")],
            "p { margin: 0 }",
        ),
        page(
            "https://p.test/late.css",
            200,
            &[("content-type", "text/css")],
            ".late { display: none }",
        ),
        page(
            "https://p.test/missing.css",
            404,
            &[("content-type", "text/plain")],
            "no",
        ),
        page(
            "https://p.test/m.js",
            200,
            &[("content-type", "text/javascript")],
            "export {}",
        ),
    ])
}

fn results(view: &str) -> serde_json::Map<String, serde_json::Value> {
    let line = view
        .lines()
        .find(|l| l.contains("text \"{"))
        .unwrap_or_else(|| panic!("no output: {view}"));
    let q = line.split_once("text ").map_or("", |x| x.1);
    let text: String = serde_json::from_str(q).unwrap_or_default();
    serde_json::from_str::<serde_json::Value>(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"))
        .as_object()
        .cloned()
        .unwrap_or_default()
}

fn check(view: &str, errors: &str) {
    let r = results(view);
    let failed: Vec<String> = r
        .iter()
        .filter(|(k, _)| *k != "count")
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    assert!(failed.is_empty(), "{failed:#?}");
    assert_eq!(r["count"], EXPECTED, "{r:#?}");
    // The rule a script inserted and the sheet it linked hide elements from the agent;
    // `<noscript>` is not rendered while scripts run.
    assert!(!view.contains("Shown until"), "{view}");
    assert!(!view.contains("Hidden by a linked sheet"), "{view}");
    assert!(!view.contains("Enable JavaScript"), "{view}");
    // Handled errors and canceled rejections are not page errors; the others are.
    assert!(errors.contains("reading 'x'"), "{errors}");
    assert!(errors.contains("(in promise) loud"), "{errors}");
    assert!(!errors.contains("handled here"), "{errors}");
    assert!(!errors.contains("quiet"), "{errors}");
    assert!(errors.contains("Uncaught SyntaxError: "), "{errors}");
}

#[test]
fn the_checks_pass_in_process() {
    let env = Capture::new(log());
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    let errors = tab.view(
        pageon::Dump::Errors,
        pageon::Viewport::default(),
        pageon::Budget::o200k(4000),
    );
    check(&view, &errors);
}

#[test]
fn the_checks_pass_caged() {
    let mut sb = Sandbox::new(
        Replay::new(log()),
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let (t, _) = sb
        .open_tab("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = sb.observe_tab(t, true).unwrap_or_else(|e| panic!("{e}"));
    let errors = sb
        .view_tab(
            t,
            pageon::Dump::Errors,
            pageon::Viewport::default(),
            pageon::Budget::o200k(4000),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    check(&view, &errors);
}

/// An app styled the way styled-components and emotion do in production: rules inserted with
/// `insertRule` into an empty `<style>`, which a page's text never shows.
const CSS_IN_JS: &str = r#"<!doctype html><div class="sc-a">Menu</div><button class="sc-b">Buy</button>
<script>
const tag = document.createElement('style');
tag.setAttribute('data-styled', '');
document.head.appendChild(tag);
tag.appendChild(document.createTextNode(''));
const sheet = tag.sheet;
sheet.insertRule('.sc-a { display: none }', sheet.cssRules.length);
sheet.insertRule('.sc-b { width: 200px; height: 60px; padding: 0 }', sheet.cssRules.length);
</script>"#;

#[test]
fn rules_inserted_by_css_in_js_reach_layout() {
    let env = Capture::new(site(vec![html("https://p.test/", CSS_IN_JS)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(!view.contains("Menu"), "{view}");
    let boxes = tab.view(
        pageon::Dump::Boxes,
        pageon::Viewport::default(),
        pageon::Budget::o200k(4000),
    );
    assert!(boxes.contains("button 8,8 200×60"), "{boxes}");
}

/// Inserted links meet the page's origins: one elsewhere is blocked and fails, so its CSS never
/// applies.
#[test]
fn inserted_links_meet_the_origin_policy() {
    let page_html = r#"<!doctype html><div class=x>Visible</div><pre id=out>waiting</pre><script>
const l = document.createElement('link');
l.rel = 'stylesheet'; l.href = 'https://evil.test/x.css';
l.onload = () => { document.getElementById('out').textContent = 'loaded'; };
l.onerror = () => { document.getElementById('out').textContent = 'refused'; };
document.head.appendChild(l);
</script>"#;
    let env = pageon::Policy::new(Capture::new(site(vec![
        html("https://p.test/", page_html),
        page(
            "https://evil.test/x.css",
            200,
            &[("content-type", "text/css")],
            ".x { display: none }",
        ),
    ])))
    .read_write(["https://p.test"]);
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(view.contains("\"refused\""), "{view}");
    assert!(view.contains("\"Visible\""), "{view}");
    let blocked =
        tab.document().requests().iter().any(|r| {
            r.url.contains("evil.test") && !matches!(r.outcome, pageon::RequestOutcome::Ok)
        });
    assert!(blocked, "{:#?}", tab.document().requests());
}

/// A page learns that another origin's script failed, not what or where (as in browsers);
/// pageon's own record keeps the details.
#[test]
fn errors_of_other_origins_are_muted() {
    let page_html = r#"<!doctype html><pre id=out>waiting</pre>
<script>window.addEventListener('error', (e) => { document.getElementById('out').textContent = [e.message, e.filename, e.lineno, e.error].join('|'); });</script>
<script src="https://cdn.test/lib.js"></script>"#;
    let env = Capture::new(site(vec![
        html("https://p.test/", page_html),
        page(
            "https://cdn.test/lib.js",
            200,
            &[("content-type", "text/javascript")],
            "var secretToken = 'abc'; null.boom;",
        ),
    ]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(view.contains("\"Script error.||0|\""), "{view}");
    let errors = tab.view(
        pageon::Dump::Errors,
        pageon::Viewport::default(),
        pageon::Budget::o200k(4000),
    );
    assert!(
        errors.contains("cdn.test/lib.js") && errors.contains("boom"),
        "{errors}"
    );
}
