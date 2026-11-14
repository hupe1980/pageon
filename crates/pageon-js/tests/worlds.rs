//! Isolated worlds: separate JavaScript over one document, as Chromium's (spec 026).

use std::cell::RefCell;
use std::rc::Rc;

use pageon_js::{Engine, Limits, RealmConfig};
use serde_json::{Value, json};

const PAGE: &str = r#"<!doctype html><button id=b>Go</button><p id=p>text</p>
<script>
  // A hostile page poisons what automation helpers use.
  Array.prototype.map = function () { return ['poisoned'] };
  const rect = Element.prototype.getBoundingClientRect;
  Element.prototype.getBoundingClientRect = function () { return {x: 0, y: 0, width: 0, height: 0} };
  Object.defineProperty(Node.prototype, 'textContent', {get() { return 'lie' }});
  window.getComputedStyle = () => ({display: 'none', visibility: 'hidden'});
  JSON.stringify = () => '"lie"';
  window.pageLog = [];
  document.getElementById('b').addEventListener('click', (e) => pageLog.push('page:' + (e instanceof Event)));
</script>"#;

fn engine() -> (Engine, usize) {
    let mut e = Engine::new(0, 1, Limits::default());
    let dom = pageon_dom::parse_document(PAGE, pageon_dom::ParseOptions::default());
    let realm = e.add_realm(
        Rc::new(RefCell::new(dom)),
        RealmConfig {
            url: "https://a.test/".into(),
            origin: "https://a.test".into(),
            navigator: pageon_env::Profile::Agent.navigator(),
            viewport: pageon_layout::Viewport::default(),
            stylesheets: Vec::new(),
            client: pageon_net::Client::top(
                &url::Url::parse("https://a.test/").unwrap_or_else(|e| panic!("{e}")),
            ),
            net: Rc::default(),
            accept_dialogs: false,
            crypto_key: [7; 32],
        },
    );
    let script = PAGE
        .split("<script>")
        .nth(1)
        .and_then(|s| s.split("</script>").next())
        .unwrap_or_default();
    e.run_script(realm, script, "page", None);
    (e, realm)
}

fn eval(e: &mut Engine, ctx: usize, src: &str) -> Value {
    let h = e
        .evaluate(ctx, src)
        .unwrap_or_else(|t| panic!("{}: {src}", t.text));
    e.describe(ctx, h, true)
        .unwrap_or_else(|err| panic!("{err}"))
        .get("value")
        .cloned()
        .unwrap_or(Value::Null)
}

#[test]
fn a_world_sees_its_own_builtins_over_the_same_document() {
    let (mut e, main) = engine();
    let w = e
        .world(main, "__puppeteer_utility_world__")
        .unwrap_or_else(|err| panic!("{err}"));
    assert_ne!(w, main);
    assert_eq!(
        e.world(main, "__puppeteer_utility_world__"),
        Ok(w),
        "one world per name"
    );
    // The page's lies hold in the main world…
    assert_eq!(
        eval(&mut e, main, "[1, 2].map((x) => x * 2)"),
        json!(["poisoned"])
    );
    assert_eq!(
        eval(&mut e, main, "document.getElementById('p').textContent"),
        json!("lie")
    );
    // …but not in the world, which acts on the same nodes.
    assert_eq!(eval(&mut e, w, "[1, 2].map((x) => x * 2)"), json!([2, 4]));
    assert_eq!(
        eval(&mut e, w, "document.getElementById('p').textContent"),
        json!("text")
    );
    assert_eq!(
        eval(&mut e, w, "JSON.stringify({a: 1})"),
        json!("{\"a\":1}")
    );
    assert_eq!(
        eval(
            &mut e,
            w,
            "getComputedStyle(document.getElementById('b')).display"
        ),
        json!("inline-block")
    );
    assert!(
        eval(
            &mut e,
            w,
            "document.getElementById('b').getBoundingClientRect().width"
        )
        .as_f64()
        .is_some_and(|x| x > 0.0)
    );
    eval(
        &mut e,
        w,
        "document.getElementById('p').textContent = 'set by the world'",
    );
    assert_eq!(
        eval(&mut e, main, "document.getElementById('p').firstChild.data"),
        json!("set by the world"),
        "one document"
    );
    // The page sees nothing of the world.
    eval(
        &mut e,
        w,
        "window.secret = 1; document.body.worldExpando = 1",
    );
    assert_eq!(
        eval(
            &mut e,
            main,
            "typeof window.secret + ',' + typeof document.body.worldExpando"
        ),
        json!("undefined,undefined")
    );
    // Content-attribute-free page: worlds stay deterministic (the page's clock and RNG seam).
    assert_eq!(
        eval(&mut e, w, "Date.now()"),
        eval(&mut e, main, "Date.now()")
    );
}

#[test]
fn listeners_and_observers_get_their_own_worlds_objects() {
    let (mut e, main) = engine();
    let w = e
        .world(main, "sandbox")
        .unwrap_or_else(|err| panic!("{err}"));
    eval(
        &mut e,
        w,
        "window.log = [];
         const b = document.getElementById('b');
         b.addEventListener('click', function (ev) {
           log.push('world:' + (ev instanceof Event) + ':' + (this === b) + ':' + (ev.target === b));
         });
         b.onclick = () => log.push('world handler');
         new MutationObserver((records) => {
           log.push('records:' + (records[0] instanceof MutationRecord) + ':' + (records[0].target === document.body));
         }).observe(document.body, {childList: true});
         setTimeout(function () { log.push('timer:' + (this === window)) }, 0);",
    );
    let node = {
        let r = e.realm(main).cloned().unwrap_or_else(|| panic!("realm"));
        let dom = r.dom.borrow();
        dom.traverse(dom.root())
            .find_map(|edge| match edge {
                pageon_dom::Edge::Enter(n)
                    if dom.get(n).and_then(|x| x.attr("id")) == Some("b") =>
                {
                    Some(n)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("#b"))
    };
    e.click(main, node, None);
    eval(
        &mut e,
        main,
        "document.body.append(document.createElement('i'))",
    );
    e.wait(10, &pageon_env::Replay::new(pageon_env::Log::default()));
    let log = eval(&mut e, w, "log.slice().sort().join('|')");
    assert_eq!(
        log,
        json!("records:true:true|timer:true|world handler|world:true:true:true")
    );
    assert_eq!(eval(&mut e, main, "pageLog.join('|')"), json!("page:true"));
}

#[test]
fn worlds_cannot_define_custom_elements_and_are_bounded() {
    let (mut e, main) = engine();
    let w = e.world(main, "w").unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(
        eval(
            &mut e,
            w,
            "try { customElements.define('x-y', class extends HTMLElement {}); 'defined' } catch (err) { err.name }"
        ),
        json!("NotSupportedError")
    );
    for i in 0..15 {
        e.world(main, &format!("extra {i}"))
            .unwrap_or_else(|err| panic!("{err}"));
    }
    let err = e.world(main, "one too many").expect_err("capped");
    assert!(err.contains("at most 16"), "{err}");
    assert_eq!(
        eval(&mut e, w, "1 + 1"),
        json!(2),
        "existing worlds keep working"
    );
}

#[test]
fn content_attribute_handlers_run_once_in_the_main_world() {
    let mut e = Engine::new(0, 1, Limits::default());
    let dom = pageon_dom::parse_document(
        "<button id=b onclick=\"this.textContent = (this.textContent === 'Pay' ? 'clicked' : 'twice')\">Pay</button>",
        pageon_dom::ParseOptions::default(),
    );
    let main = e.add_realm(
        Rc::new(RefCell::new(dom)),
        RealmConfig {
            url: "https://a.test/".into(),
            origin: "https://a.test".into(),
            navigator: pageon_env::Profile::Agent.navigator(),
            viewport: pageon_layout::Viewport::default(),
            stylesheets: Vec::new(),
            client: pageon_net::Client::top(
                &url::Url::parse("https://a.test/").unwrap_or_else(|e| panic!("{e}")),
            ),
            net: Rc::default(),
            accept_dialogs: false,
            crypto_key: [7; 32],
        },
    );
    let w = e
        .world(main, "utility")
        .unwrap_or_else(|err| panic!("{err}"));
    eval(&mut e, w, "document.getElementById('b').click()");
    assert_eq!(
        eval(&mut e, main, "document.getElementById('b').textContent"),
        json!("clicked")
    );
}
