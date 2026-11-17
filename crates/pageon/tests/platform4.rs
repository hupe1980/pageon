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

/// An `<iframe>` a script inserts gets its `about:blank` window at once, as in browsers: the
/// clean realm pages borrow globals from, a DOM to write, messages, `load`. A sandboxed one
/// (no `allow-same-origin`) is an opaque, cross-origin window: no document to reach.
const FRAMES: &str = r#"<!doctype html><pre id=out>waiting</pre><script>
const r = [];
const f = document.createElement('iframe');
f.onload = () => r.push('load');
document.body.appendChild(f);
const w = f.contentWindow;
r.push(!!w, w.Promise !== Promise, typeof w.Promise.prototype.finally, w.location.href, w.parent === window, f.contentDocument === w.document);
w.document.body.innerHTML = '<b>inside</b>';
r.push(w.document.body.textContent);
const got = [];
w.addEventListener('message', (e) => got.push(e.data));
w.postMessage('hi', '*');
const s = document.createElement('iframe');
s.setAttribute('sandbox', '');
document.body.appendChild(s);
let reached = 'no';
try { s.contentWindow.document; reached = 'yes'; } catch (e) { reached = e.name; }
r.push(s.contentWindow !== null && s.contentDocument === null && reached === 'SecurityError');
setTimeout(() => { r.push(got.join(), window.frames.length); document.getElementById('out').textContent = r.join('|'); }, 50);
</script>"#;

const FRAMES_WANT: &str = "true|true|function|about:blank|true|true|inside|true|load|hi|2";

#[test]
fn script_inserted_iframes_get_a_window() {
    let env = Capture::new(site(vec![html("https://p.test/", FRAMES)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(view.contains(FRAMES_WANT), "{view}");
    let mut sb = Sandbox::new(
        Replay::new(site(vec![html("https://p.test/", FRAMES)])),
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
    assert!(view.contains(FRAMES_WANT), "{view}");
}

/// Rules that cannot match are left out of a layout, and come back when the document changes:
/// a class added later applies its rules.
#[test]
fn rules_left_out_come_back_when_classes_change() {
    let page_html = r#"<!doctype html><style>.later .x { display: none } .never-used { color: red }</style>
<div class=x>Hidden once the body has the class</div><button id=b onclick="document.body.classList.add('later')">Go</button>"#;
    let env = Capture::new(site(vec![html("https://p.test/", page_html)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(view.contains("Hidden once"), "{view}");
    let button = view
        .lines()
        .find(|l| l.contains("\"Go\""))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("e1")
        .to_string();
    tab.act(
        format!("click {button}")
            .parse()
            .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(!view.contains("Hidden once"), "{view}");
}

/// A frame a script creates with a `src` loads after the task through the page loader: it
/// takes its window's place, `load` fires, messages carry the right `source`; one outside the
/// page's origins is blocked. The window's `load` waits for frames the page has.
#[test]
fn script_created_frames_load_under_the_policy() {
    let page_html = r#"<!doctype html><pre id=out>waiting</pre><iframe src="/static.html"></iframe><script>
const r = [];
const show = () => { document.getElementById('out').textContent = r.join('|'); };
const push = r.push.bind(r);
r.push = (x) => { push(x); show(); };
window.onload = () => r.push('page load sees ' + document.querySelector('iframe').contentDocument.title);
window.addEventListener('message', (e) => r.push(e.data + ':' + (e.source === f.contentWindow)));
const f = document.createElement('iframe');
f.src = '/child.html';
f.onload = () => { r.push('load ' + f.contentDocument.title); f.contentWindow.postMessage('ping', '*'); };
document.body.appendChild(f);
const evil = document.createElement('iframe');
evil.src = 'https://evil.test/x.html';
document.body.appendChild(evil);
</script>"#;
    let child = r#"<title>Child</title><script>addEventListener('message', (e) => e.source.postMessage('pong', '*'))</script>"#;
    let env = pageon::Policy::new(Capture::new(site(vec![
        html("https://p.test/", page_html),
        html("https://p.test/static.html", "<title>Static</title>"),
        html("https://p.test/child.html", child),
        html("https://evil.test/x.html", "<title>Evil</title><p>evil</p>"),
    ])))
    .read_write(["https://p.test"]);
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    for event in ["page load sees Static", "load Child", "pong:true"] {
        assert!(view.contains(event), "{event}: {view}");
    }
    assert!(!view.contains("evil\""), "{view}");
    let blocked =
        tab.document().requests().iter().any(|r| {
            r.url.contains("evil.test") && !matches!(r.outcome, pageon::RequestOutcome::Ok)
        });
    assert!(blocked, "{:#?}", tab.document().requests());
}

/// Computed styles: values that do not depend on layout come from styles alone, sizes from a
/// layout, both after the page changed (styles-only and full layouts of the same tree).
#[test]
fn computed_styles_follow_changes() {
    let page_html = r#"<!doctype html><div id=d style="width:100px">x</div><p id=out></p><script>
const d = document.getElementById('d'), cs = getComputedStyle(d);
const before = cs.display;
d.style.width = '250px'; d.style.display = 'inline-block';
const width = cs.width, display = cs.display, offset = d.offsetWidth;
d.style.color = 'rgb(1, 2, 3)';
const color = cs.color;
d.style.width = '40px';
document.getElementById('out').textContent = [before, width, display, offset, color, cs.width].join('|');
</script>"#;
    let env = Capture::new(site(vec![html("https://p.test/", page_html)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(
        view.contains("block|250px|inline-block|250|rgb(1, 2, 3)|40px"),
        "{view}"
    );
}

/// A custom element hidden until defined is shown once its definition runs, to the page's
/// scripts (computed style) and to the agent (the view) alike; a style read does not move the
/// page's clock, so whether a sheet was parsed before cannot be timed.
#[test]
fn defined_custom_elements_show_and_style_reads_take_no_time() {
    let page_html = r#"<!doctype html><style>x-card:not(:defined){display:none} x-card{display:block}</style>
<x-card id=c>Card content</x-card><p id=out></p><script>
const c = document.getElementById('c');
const before = getComputedStyle(c).display;
customElements.define('x-card', class extends HTMLElement {});
const t0 = performance.now();
const after = getComputedStyle(c).display;
const t1 = performance.now();
document.getElementById('out').textContent = [before, after, t1 - t0].join('|');
</script>"#;
    let env = Capture::new(site(vec![html("https://p.test/", page_html)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(view.contains("none|block|0"), "{view}");
    assert!(view.contains("Card content"), "{view}");
}
