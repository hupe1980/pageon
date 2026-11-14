//! Random scripts over the DOM API with hostile arguments: the engine never panics.

use std::cell::RefCell;
use std::rc::Rc;

use pageon_dom::{ParseOptions, parse_document};
use pageon_js::{Engine, Limits, RealmConfig};
use pageon_layout::Viewport;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[(self.next() as usize) % v.len()]
    }
}

const TARGETS: &[&str] = &[
    "document",
    "document.body",
    "document.documentElement",
    "el",
    "el.firstChild",
    "frag",
    "txt",
    "window",
    "document.createElement('div')",
    "document.createElementNS('http://www.w3.org/2000/svg','svg')",
    "null",
    "undefined",
    "ev",
    "list",
    "el.style",
    "el.classList",
    "el.attributes",
    "el.dataset",
    "ctx",
    "document.implementation",
    "h",
    "u",
    "u.searchParams",
    "fd",
    "x",
    "ac",
    "ac.signal",
    "localStorage",
    "sessionStorage",
    "res",
    "req",
];
const ARGS: &[&str] = &[
    "''",
    "'div'",
    "'#x'",
    "'a['",
    "':checked'",
    "null",
    "undefined",
    "0",
    "-1",
    "1e309",
    "NaN",
    "{}",
    "[]",
    "Symbol()",
    "el",
    "document",
    "txt",
    "frag",
    "() => { throw 1 }",
    "'<p>x</p><script>1</script>'",
    "true",
    "'click'",
    "{ bubbles: true }",
    "'2d'",
    "'\\u0000'",
    "'x'.repeat(10000)",
    "new Event('x')",
    "window",
    "'https://b.test/x'",
    "'http://127.0.0.1/'",
    "'/api'",
    "{ method: 'POST', body: 'x', headers: { 'x-a': '1' } }",
    "{ mode: 'no-cors', redirect: 'manual', signal: ac.signal }",
    "'GET'",
    "'x-a'",
    "'a=1; Path=/'",
    "h",
    "fd",
    "new URLSearchParams('a=1&b')",
    "new Uint8Array(4)",
];
const CALLS: &[&str] = &[
    "appendChild",
    "insertBefore",
    "removeChild",
    "replaceChild",
    "cloneNode",
    "contains",
    "querySelector",
    "querySelectorAll",
    "closest",
    "matches",
    "setAttribute",
    "getAttribute",
    "removeAttribute",
    "toggleAttribute",
    "addEventListener",
    "removeEventListener",
    "dispatchEvent",
    "append",
    "prepend",
    "before",
    "after",
    "remove",
    "replaceWith",
    "getElementById",
    "getElementsByTagName",
    "getElementsByClassName",
    "createElement",
    "createTextNode",
    "createEvent",
    "getBoundingClientRect",
    "focus",
    "blur",
    "click",
    "item",
    "namedItem",
    "add",
    "toggle",
    "setProperty",
    "getPropertyValue",
    "insertAdjacentHTML",
    "compareDocumentPosition",
    "normalize",
    "getContext",
    "fillRect",
    "arc",
    "hasFeature",
    "getNamedItem",
    "showModal",
    "close",
    "setTimeout",
    "queueMicrotask",
    "requestAnimationFrame",
    "fetch",
    "set",
    "get",
    "has",
    "delete",
    "getAll",
    "sort",
    "open",
    "send",
    "setRequestHeader",
    "abort",
    "getResponseHeader",
    "getAllResponseHeaders",
    "text",
    "json",
    "clone",
    "setItem",
    "getItem",
    "removeItem",
    "key",
    "clear",
    "throwIfAborted",
    "entries",
    "forEach",
    "toString",
];
const PROPS: &[&str] = &[
    "innerHTML",
    "outerHTML",
    "textContent",
    "id",
    "className",
    "value",
    "checked",
    "hidden",
    "title",
    "src",
    "href",
    "tabIndex",
    "nodeValue",
    "data",
    "cssText",
    "display",
    "fillStyle",
    "lineWidth",
    "onclick",
    "selectedIndex",
    "cookie",
    "responseType",
    "withCredentials",
    "search",
    "hash",
    "pathname",
    "timeout",
    "onload",
    "onabort",
];

fn script(rng: &mut Lcg) -> String {
    let mut s = String::from(
        "var el = document.getElementById('a') || document.body; var frag = document.createDocumentFragment();\
         var txt = document.createTextNode('t'); var ev = new Event('x'); var list = document.querySelectorAll('*');\
         var ctx = document.createElement('canvas').getContext('2d'); var h = new Headers({ a: '1' });\
         var u = new URL('https://a.test/x?y=1#z'); var fd = new FormData(); var x = new XMLHttpRequest();\
         var ac = new AbortController(); var res = new Response('b'); var req = new Request('/r');\n",
    );
    for _ in 0..(rng.next() % 30) {
        let t = rng.pick(TARGETS);
        let line = if rng.next().is_multiple_of(3) {
            format!("{t}.{} = {};", rng.pick(PROPS), rng.pick(ARGS))
        } else {
            let args: Vec<&str> = (0..rng.next() % 4).map(|_| rng.pick(ARGS)).collect();
            format!("{t}.{}({});", rng.pick(CALLS), args.join(", "))
        };
        s.push_str(&format!("try {{ {line} }} catch (e) {{}}\n"));
    }
    s
}

#[test]
fn random_dom_scripts_never_panic() {
    let mut rng = Lcg(5);
    let mut engine = Some(Engine::new(1_700_000_000_000, 42, Limits::default()));
    for i in 0..1_000 {
        if i % 50 == 0 {
            drop(engine.take()); // isolates on a thread are dropped before the next is created
            engine = Some(Engine::new(1_700_000_000_000, 42, Limits::default()));
        }
        let Some(engine) = engine.as_mut() else {
            continue;
        };
        let dom = parse_document(
            "<div id=a class='x y'><p>t</p><input id=i><select><option>o</select><dialog id=d></dialog></div>",
            ParseOptions::default(),
        );
        let realm = engine.add_realm(
            Rc::new(RefCell::new(dom)),
            RealmConfig {
                url: "https://a.test/".into(),
                origin: "https://a.test".into(),
                navigator: pageon_env::Profile::Agent.navigator(),
                viewport: Viewport::default(),
                stylesheets: Vec::new(),
                client: pageon_net::Client::top(
                    &url::Url::parse("https://a.test/").unwrap_or_else(|e| panic!("{e}")),
                ),
                net: Rc::new(RefCell::new(pageon_net::Net::default())),
                accept_dialogs: false,
                crypto_key: [7; 32],
            },
        );
        let code = script(&mut rng);
        engine.run_script(realm, &code, "fuzz", None);
        // Every request fails (an empty recording): rejections and XHR errors run too.
        let _ = engine.settle(50.0, &pageon_env::Replay::new(pageon_env::Log::default()));
        let internal: Vec<_> = engine
            .errors()
            .into_iter()
            .filter(|e| e.message.contains("internal error in pageon"))
            .collect();
        assert!(
            internal.is_empty(),
            "a binding panicked on script {i}:\n{code}\n{internal:?}"
        );
    }
}
