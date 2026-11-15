//! Script semantics: parse position, ordering, events, timers on virtual time, errors, identity.

mod common;

use common::*;
use pageon::{Budget, Document, LoadOptions, PageErrorKind, Replay, Viewport};

fn site(pages: &[(&str, &str, &str)]) -> pageon_env::Log {
    let ex = pages
        .iter()
        .map(|(url, ct, body)| page(url, 200, &[("content-type", ct)], body))
        .collect();
    let mut l = log(ex);
    l.clock = (0..64).map(|i| 1_700_000_000_000 + i * 1000).collect();
    l.random = (1..=16).collect();
    l
}

fn load(html: &str) -> Document {
    load_with(
        &[("https://a.test/", "text/html", html)],
        LoadOptions::default(),
    )
}

fn load_with(pages: &[(&str, &str, &str)], options: LoadOptions) -> Document {
    pageon::load(pages[0].0, &Replay::new(site(pages)), options).unwrap_or_else(|e| panic!("{e}"))
}

/// The text of the element with `id`.
fn text(doc: &Document, id: &str) -> String {
    let dom = doc.dom();
    dom.traverse(dom.root())
        .find_map(|e| match e {
            pageon_dom::Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some(id) => {
                Some(dom.text(n))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("#{id} missing in\n{}", doc.dump(pageon::Dump::Html)))
}

#[test]
fn scripts_see_the_document_parsed_so_far() {
    let doc = load(
        "<p>1</p><script>document.write; var n = document.querySelectorAll('p').length;</script><p>2</p>\
         <div id=out></div><script>document.getElementById('out').textContent = n + '/' + document.querySelectorAll('p').length</script>",
    );
    assert_eq!(text(&doc, "out"), "1/2");
}

#[test]
fn order_of_scripts_and_events() {
    let page = "<script>window.log = ['inline1']; document.addEventListener('DOMContentLoaded', () => log.push('dcl'));\
        addEventListener('load', () => { log.push('load'); document.getElementById('out').textContent = log.join(','); });</script>\
        <script src=/a.js async></script><script src=/d.js defer></script><script src=/e.js></script>\
        <script>log.push('inline2')</script><div id=out></div>";
    let doc = load_with(
        &[
            ("https://a.test/", "text/html", page),
            (
                "https://a.test/e.js",
                "text/javascript",
                "log.push('external')",
            ),
            (
                "https://a.test/a.js",
                "text/javascript",
                "log.push('async')",
            ),
            (
                "https://a.test/d.js",
                "text/javascript",
                "log.push('defer')",
            ),
        ],
        LoadOptions::default(),
    );
    assert_eq!(
        text(&doc, "out"),
        "inline1,external,inline2,async,defer,dcl,load"
    );
    assert_eq!(doc.summary().scripts, 5);
}

#[test]
fn timers_run_on_virtual_time() {
    let doc = load(
        "<div id=out></div><script>var t0 = Date.now(), p0 = performance.now();\
         setTimeout(() => { document.getElementById('out').textContent = (Date.now() - t0) + ' ' + Math.round(performance.now() - p0); }, 300);\
         setTimeout(() => { document.getElementById('out').textContent = 'too late'; }, 5000);\
         var n = 0; var id = setInterval(() => { if (++n === 3) clearInterval(id); }, 10);\
         requestAnimationFrame(t => document.body.dataset.frame = typeof t);</script>",
    );
    assert_eq!(text(&doc, "out"), "300 300");
    assert_eq!(
        doc.summary().pending_timers,
        1,
        "the 5 s timer stays pending"
    );
    assert!(
        doc.dump(pageon::Dump::Html)
            .contains("data-frame=\"number\"")
    );
}

#[test]
fn errors_are_reported_with_location_and_later_scripts_run() {
    let doc = load(
        "<div id=out>a</div><script>\nnull.x;</script><script>document.getElementById('out').textContent = 'b';</script>",
    );
    assert_eq!(text(&doc, "out"), "b");
    let e = &doc.errors()[0];
    assert_eq!(
        (e.kind, e.source.as_str(), e.line),
        (PageErrorKind::Exception, "<inline #1>", 2)
    );
    assert!(e.message.contains("TypeError"), "{e}");
    assert_eq!(doc.summary().errors, 1);
}

#[test]
fn failing_scripts_are_reported() {
    let doc = load_with(
        &[(
            "https://a.test/",
            "text/html",
            "<script type=module>x()</script><script src=/missing.js></script><script type=text/template>{{x}}</script>",
        )],
        LoadOptions::default(),
    );
    let kinds: Vec<PageErrorKind> = doc.errors().iter().map(|e| e.kind).collect();
    // The module runs after parsing and throws; data blocks never run.
    assert_eq!(kinds, [PageErrorKind::Load, PageErrorKind::Exception]);
}

#[test]
fn wrappers_keep_identity() {
    let doc = load(
        "<p id=p class=a></p><div id=out></div><script>var p = document.getElementById('p');\
         document.getElementById('out').textContent = [p === document.querySelector('p'), p.classList === p.classList,\
         p.style === p.style, document.body === document.body, p.parentNode === document.body].join(',');</script>",
    );
    assert_eq!(text(&doc, "out"), "true,true,true,true,true");
}

#[test]
fn events_capture_bubble_and_handlers() {
    let doc = load(
        "<div id=outer><button id=b onclick=\"log.push('attr')\">x</button></div><div id=out></div><script>var log = [];\
         outer.addEventListener('click', () => log.push('capture'), true);\
         outer.addEventListener('click', () => log.push('bubble'));\
         b.addEventListener('click', () => log.push('target'), { once: true });\
         b.click(); b.click();\
         var e = new Event('x', { cancelable: true }); b.addEventListener('x', ev => ev.preventDefault());\
         log.push(b.dispatchEvent(e));\
         document.getElementById('out').textContent = log.join(',');</script>",
    );
    assert_eq!(
        text(&doc, "out"),
        "capture,attr,target,bubble,capture,attr,bubble,false"
    );
}

#[test]
fn script_state_reaches_the_views() {
    let doc = load(
        "<label for=q>Query</label><input id=q><label><input type=checkbox id=c> Agree</label><select id=s><option>a<option>b</select>\
         <script>q.value = 'typed'; c.checked = true; s.value = 'b'; document.title = 'Set by script';</script>",
    );
    let ax = doc.ax(Viewport::default(), Budget::o200k(4000));
    for want in [
        "\"Set by script\"",
        "textbox e1 \"Query\" =\"typed\"",
        "checkbox e2 \"Agree\" checked",
        "combobox e3 =\"b\"",
    ] {
        assert!(ax.contains(want), "missing {want:?} in\n{ax}");
    }
    // Attributes keep the defaults (value set by script is form state, not markup).
    assert!(!doc.dump(pageon::Dump::Html).contains("value=\"typed\""));
}

#[test]
fn markup_scripts_never_run_and_selectors_validate() {
    let doc = load(
        "<div id=box></div><div id=out></div><script>box.innerHTML = '<script>window.ran = 1<\\/script><b>bold</b>';\
         var bad; try { document.querySelector('a[') } catch (e) { bad = e.name; }\
         document.getElementById('out').textContent = [typeof window.ran, box.querySelector('b').textContent, bad,\
         document.querySelectorAll('div:not(#out)').length].join(',');</script>",
    );
    assert_eq!(text(&doc, "out"), "undefined,bold,SyntaxError,1");
}

#[test]
fn missing_apis_are_absent_not_stubbed() {
    let doc = load(
        "<div id=out></div><script>document.getElementById('out').textContent = [typeof WebTransport, typeof document.write,\
         typeof BroadcastChannel, typeof SharedWorker, typeof Worker, 'getContext' in document.createElement('canvas')].join(',');</script>",
    );
    assert_eq!(
        text(&doc, "out"),
        "undefined,undefined,undefined,undefined,undefined,true"
    );
}

#[test]
fn scripts_can_be_turned_off() {
    let doc = load_with(
        &[(
            "https://a.test/",
            "text/html",
            "<div id=out>static</div><script>out.textContent = 'scripted'</script>",
        )],
        LoadOptions {
            scripts: false,
            ..LoadOptions::default()
        },
    );
    assert_eq!(text(&doc, "out"), "static");
    assert_eq!(doc.summary().scripts, 0);
}

#[test]
fn script_free_pages_need_no_engine() {
    // A recording without random values (no scripts were ever run) still replays.
    let mut l = log(vec![page(
        "https://a.test/",
        200,
        &[("content-type", "text/html")],
        "<p>plain</p>",
    )]);
    l.clock = (0..8).collect();
    let doc = pageon::load("https://a.test/", &Replay::new(l), LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(doc.summary().scripts, 0);
}

#[test]
fn replays_are_identical() {
    let html = "<div id=out></div><script>var s = ''; for (var i = 0; i < 5; i++) s += Math.random().toFixed(6) + ' ';\
                setTimeout(() => out.textContent = s + new Date().toISOString(), 250);</script>";
    let a = load(html);
    let b = load(html);
    assert_eq!(text(&a, "out"), text(&b, "out"));
    assert_eq!(a.dump(pageon::Dump::Html), b.dump(pageon::Dump::Html));
}
