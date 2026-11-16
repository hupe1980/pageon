//! Platform 3: named form access and radio groups, element properties, the Popover API,
//! canvas text, and `document.write`, caged and in-process; an agent opening a popover menu.

mod common;

use common::*;
use pageon::{LoadOptions, Replay, Sandbox, SandboxOptions, Session};

const CHECKS: &str = r##"<!doctype html><title>checks</title>
<form id=f name=login action="/go?x=1" method=POST enctype="multipart/form-data">
  <input name=email value=ann><input name=submit value=s>
  <input type=radio name=plan value=free checked><input type=radio name=plan value=pro>
  <input id=n type=number value="4.5"><input id=d type=date value="2026-11-16">
</form>
<textarea id=ta rows=0></textarea><table id=t><thead><tr><td>h</td></tr></thead><tbody><tr><td>1</td></tr><tr><td>2</td></tr></tbody><tfoot><tr><td>f</td></tr></tfoot></table>
<a id=a href=/x dir=RTL translate=no>Link text</a><img id=i loading=LAZY>
<div id=p popover><span id=inner>in</span></div><div id=m popover=manual>m</div>
<button id=inv popovertarget=p popovertargetaction=show>Open</button>
<canvas id=c width=200 height=50></canvas>
<script>document.write('<p id=w>written</p>')</script>
<pre id=out>running</pre>
<script>
const results = {};
const ok = (name, cond, detail) => { results[name] = cond ? true : String(detail); };
const f = document.getElementById('f');
ok('form.email', f.email && f.email.value === 'ann', f.email);
ok('form["email"]', f['email'] === f.email, f['email']);
ok('a control named like a method wins', f.submit && f.submit.value === 's', typeof f.submit);
ok('form[0]', f[0] === f.email, f[0]);
ok('elements.email', f.elements.email === f.email, f.elements.email);
ok('radio group', f.elements.plan instanceof RadioNodeList && f.elements.plan.length === 2, f.elements.plan);
ok('radio value', f.elements.plan.value === 'free', f.elements.plan.value);
f.elements.plan.value = 'pro';
ok('radio value set', f.plan.value === 'pro' && f.plan[1].checked, f.plan.value);
ok('document.forms.login', document.forms.login === f, document.forms.login);
ok('form.action', f.action === location.origin + '/go?x=1', f.action);
ok('form.method', f.method === 'post', f.method);
ok('form.enctype', f.enctype === 'multipart/form-data' && f.encoding === f.enctype, f.enctype);
ok('valueAsNumber', document.getElementById('n').valueAsNumber === 4.5, document.getElementById('n').valueAsNumber);
ok('valueAsNumber date', document.getElementById('d').valueAsNumber === Date.UTC(2026, 10, 16), document.getElementById('d').valueAsNumber);
ok('textarea rows', document.getElementById('ta').rows === 2 && document.getElementById('ta').cols === 20, document.getElementById('ta').rows);
const t = document.getElementById('t');
ok('table.rows', t.rows.length === 4 && t.rows[3].textContent === 'f' && t.tBodies.length === 1, t.rows.length);
const a = document.getElementById('a');
ok('dir', a.dir === 'rtl', a.dir);
ok('translate', a.translate === false, a.translate);
ok('draggable', a.draggable === true && document.body.draggable === false, a.draggable);
ok('spellcheck', document.body.spellcheck === true, document.body.spellcheck);
ok('a.text', a.text === 'Link text', a.text);
ok('outerText', a.outerText === 'Link text', a.outerText);
ok('img.loading', document.getElementById('i').loading === 'lazy', document.getElementById('i').loading);
// Popover.
const p = document.getElementById('p'), m = document.getElementById('m');
const events = [];
p.addEventListener('beforetoggle', (e) => events.push('before:' + e.oldState + '>' + e.newState));
p.addEventListener('toggle', (e) => events.push(e.oldState + '>' + e.newState));
ok('popover attribute', p.popover === 'auto' && m.popover === 'manual' && a.popover === null, p.popover);
p.showPopover();
ok('shown', p.matches(':popover-open') && document.querySelector(':popover-open') === p, p.matches(':popover-open'));
ok('events', events.join() === 'before:closed>open,closed>open', events.join());
m.showPopover();
ok('manual stays with auto', m.matches(':popover-open') && p.matches(':popover-open'), 'm');
ok('togglePopover', p.togglePopover() === false && !p.matches(':popover-open'), 'toggle');
ok('togglePopover force', p.togglePopover(true) === true && p.togglePopover({ force: true }) === true, 'force');
p.hidePopover(); m.hidePopover();
let threw = '';
try { a.showPopover(); } catch (e) { threw = e.name; }
ok('not a popover', threw === 'NotSupportedError', threw);
p.addEventListener('beforetoggle', (e) => { if (e.newState === 'open' && window.cancelNext) e.preventDefault(); });
window.cancelNext = true; p.showPopover(); window.cancelNext = false;
ok('beforetoggle cancels', !p.matches(':popover-open'), 'opened');
const inv = document.getElementById('inv');
ok('invoker attributes', inv.popoverTargetElement === p && inv.popoverTargetAction === 'show', inv.popoverTargetAction);
ok('ToggleEvent', new ToggleEvent('toggle', { oldState: 'a', newState: 'b' }).newState === 'b', 'ctor');
// Canvas text.
const ctx = document.getElementById('c').getContext('2d');
ok('default font', ctx.font === '10px sans-serif', ctx.font);
ctx.font = 'bold 20px Arial';
ok('font', ctx.font === 'bold 20px Arial', ctx.font);
ctx.font = 'nonsense';
ok('invalid font kept', ctx.font === 'bold 20px Arial', ctx.font);
const w = ctx.measureText('Hello').width;
ok('measureText', w > 40 && w < 70, w);
const mt = ctx.measureText('Hg');
ok('metrics', mt.actualBoundingBoxAscent > 10 && mt.actualBoundingBoxDescent > 2 && mt.fontBoundingBoxAscent >= mt.actualBoundingBoxAscent, JSON.stringify([mt.actualBoundingBoxAscent, mt.actualBoundingBoxDescent]));
ctx.font = '20px Helvetica';
const h = ctx.measureText('Hello').width;
ctx.font = '20px sans-serif';
ok('the same fonts everywhere', h === ctx.measureText('Hello').width, h);
ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
ok('text styles', ctx.textAlign === 'center' && ctx.textBaseline === 'middle', ctx.textAlign);
ctx.textAlign = 'bogus';
ok('invalid align kept', ctx.textAlign === 'center', ctx.textAlign);
ctx.fillStyle = '#000'; ctx.fillText('Hello', 100, 25);
// select.add and remove, and measure's options.
const sel = document.createElement('select');
sel.add(new Option('a')); sel.add(new Option('c'));
sel.add(new Option('z'), 0); sel.options.add(new Option('b'), sel.options[2]);
ok('select.add', [...sel.options].map(o => o.text).join('') === 'zabc', [...sel.options].map(o => o.text).join(''));
sel.remove(0); sel.options.remove(2);
ok('select.remove', [...sel.options].map(o => o.text).join('') === 'ab' && sel.length === 2, sel.length);
let addThrew = '';
try { sel.add(document.createElement('div')); } catch (e) { addThrew = e.name; }
ok('select.add type', addThrew === 'TypeError', addThrew);
performance.mark('m3');
performance.measure('empty options', {}, 'm3');
let measureThrew = '';
try { performance.measure('detail only', { detail: 1 }); } catch (e) { measureThrew = e.name; }
ok('measure options', measureThrew === 'TypeError' && performance.getEntriesByName('empty options').length === 1, measureThrew);
// Constructors, subclasses and tags.
class Bus extends EventTarget { ping() { this.dispatchEvent(new CustomEvent('ping', { detail: 7 })); } }
const bus = new Bus(); let pinged = 0;
bus.addEventListener('ping', (e) => { pinged = e.detail; });
bus.ping();
ok('class extends EventTarget', pinged === 7 && bus instanceof EventTarget, pinged);
const rect = new DOMRect(1, 2, 3, 4); rect.x = 10;
ok('new DOMRect', rect.x === 10 && rect.right === 13 && rect instanceof DOMRectReadOnly, rect.right);
ok('toStringTag', Object.prototype.toString.call(new Event('x')) === '[object Event]' && Object.prototype.toString.call(document.body) === '[object HTMLBodyElement]', Object.prototype.toString.call(new Event('x')));
// document.fonts: the bundled fonts are always loaded.
ok('document.fonts', document.fonts === document.fonts && document.fonts.status === 'loaded' && document.fonts.check('16px Inter') && document.fonts.ready instanceof Promise, document.fonts && document.fonts.status);
let fontThrew = '';
try { document.fonts.check('nonsense'); } catch (e) { fontThrew = e.name; }
ok('fonts.check syntax', fontThrew === 'SyntaxError', fontThrew);
// document.write.
const wp = document.getElementById('w');
ok('document.write', wp && wp.previousElementSibling.tagName === 'SCRIPT', wp);
// The count and the failures only: the view clips long text.
const failures = Object.fromEntries(Object.entries(results).filter(([, v]) => v !== true));
document.getElementById('out').textContent = JSON.stringify({ count: Object.keys(results).length, ...failures });
</script>"##;

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

fn failed(r: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    r.iter()
        .filter(|(k, _)| *k != "count")
        .map(|(k, v)| format!("{k}: {v}"))
        .collect()
}

const EXPECTED: usize = 51;

fn log() -> pageon_env::Log {
    site(vec![html("https://p.test/", CHECKS)])
}

#[test]
fn the_checks_pass_in_process() {
    let env = Capture::new(log());
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let r = results(&tab.observe_full());
    assert!(failed(&r).is_empty(), "{:#?}", failed(&r));
    assert_eq!(r["count"], EXPECTED, "{r:#?}");
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
    let r = results(&sb.observe_tab(t, true).unwrap_or_else(|e| panic!("{e}")));
    assert!(failed(&r).is_empty(), "{:#?}", failed(&r));
    assert_eq!(r["count"], EXPECTED, "{r:#?}");
}

const MENU: &str = r#"<!doctype html><h1>App</h1><button>Save</button><button popovertarget=menu>Account</button>
<div id=menu popover style="width:300px;height:200px"><a href="/profile">Profile</a><a href="/out">Sign out</a></div>"#;

#[test]
fn an_agent_opens_a_popover_menu() {
    let env = Capture::new(site(vec![html("https://p.test/", MENU)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(
        !view.contains("Profile"),
        "a closed popover is in the view: {view}"
    );
    let account = find(&view, "button", "Account");
    tab.act(pageon::Action::Click(account))
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(
        view.contains("link e") && view.contains("\"Sign out\""),
        "{view}"
    );
    // The open popover covers what is under it: a click there is refused as covered.
    let save = find(&view, "button", "Save");
    let boxes = tab.document().boxes(pageon::Viewport::default());
    let covered = boxes.lines().any(|l| l.contains("covered-by:div"));
    let clicked = tab.act(pageon::Action::Click(save));
    assert!(covered || clicked.is_ok(), "{boxes}");
    // Escape closes it.
    tab.act(pageon::Action::Press("Escape".into()))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!tab.observe_full().contains("Sign out"));
}

#[test]
fn canvas_text_is_painted() {
    let page = r#"<!doctype html><body style="margin:0"><canvas id=c width=200 height=60 style="display:block"></canvas>
<script>const x = c.getContext('2d'); x.fillStyle = '#fff'; x.fillRect(0, 0, 200, 60); x.font = '40px sans-serif'; x.fillStyle = '#000'; x.fillText('Hi', 10, 45);</script>"#;
    let env = Capture::new(site(vec![html("https://p.test/", page)]));
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let img = tab
        .screenshot(&pageon::ScreenshotOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let png = image::load_from_memory(&img.bytes)
        .unwrap_or_else(|e| panic!("{e}"))
        .to_rgba8();
    // Dark pixels where "Hi" is drawn (x 10..60, y 10..45), none far right of it.
    let dark = |x0: u32, x1: u32| {
        (x0..x1)
            .flat_map(|x| (10..45).map(move |y| (x, y)))
            .filter(|&(x, y)| png.get_pixel(x, y).0.iter().take(3).all(|c| *c < 100))
            .count()
    };
    assert!(dark(10, 60) > 50, "no text pixels");
    assert_eq!(dark(120, 190), 0, "ink where there is no text");
}

#[test]
fn written_scripts_meet_the_origin_policy() {
    let written = r#"<!doctype html><script>document.write('<script src="https://evil.test/x.js"><\/script><p>after</p>')</script>"#;
    let env = pageon::Policy::new(Capture::new(site(vec![
        html("https://p.test/", written),
        page(
            "https://evil.test/x.js",
            200,
            &[("content-type", "text/javascript")],
            "document.title = 'ran'",
        ),
    ])))
    .read_write(["https://p.test"]);
    let s = Session::new(&env);
    let mut tab = s
        .open("https://p.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = tab.observe_full();
    assert!(view.contains("\"after\""), "{view}");
    assert!(
        !view.contains("\"ran\""),
        "a written script outside the origins ran: {view}"
    );
    let blocked =
        tab.document().requests().iter().any(|r| {
            r.url.contains("evil.test") && !matches!(r.outcome, pageon::RequestOutcome::Ok)
        });
    assert!(blocked, "{:#?}", tab.document().requests());
}

#[test]
#[ignore = "timing: run with --release -- --ignored"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn measuring_text_is_cheap() {
    let font = pageon_layout::CanvasFont::parse("16px sans-serif").unwrap_or_default();
    let _ = pageon_layout::canvas_text(&font, "warm up", false);
    let t = std::time::Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(pageon_layout::canvas_text(
            &font,
            "Twenty characters ok",
            false,
        ));
    }
    let us = t.elapsed().as_secs_f64() * 1000.0;
    println!("measureText of 20 characters: {us:.1} µs");
    assert!(us < 100.0, "{us:.1} µs");
}
