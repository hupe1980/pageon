//! Networking: script requests, CORS, cookies, storage, egress and honest headers.
//! Pages are served from recordings; a capturing `Env` (the interception seam) shows what was
//! sent.

mod common;

use common::*;
use pageon::{Document, LoadError, LoadOptions, RequestOutcome as Outcome, Session};
use pageon_env::Exchange;

fn load(env: &Capture, url: &str) -> Document {
    pageon::load(url, env, LoadOptions::default()).unwrap_or_else(|e| panic!("{e}"))
}

/// The text of `#out`.
fn out(doc: &Document) -> String {
    let dom = doc.dom();
    dom.traverse(dom.root())
        .find_map(|e| match e {
            pageon_dom::Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some("out") => {
                Some(dom.text(n))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("#out missing in\n{}", doc.dump(pageon::Dump::Html)))
}

// ----- script requests -----

#[test]
fn us1_fetch_renders_json() {
    let env = Capture::new(site(vec![
        html(
            "https://a.test/",
            "<ul id=list></ul><script>fetch('/api/items').then(r => r.json()).then(items => {\
             for (const i of items) { const li = document.createElement('li'); li.textContent = i.name; list.append(li); } });</script>",
        ),
        json(
            "https://a.test/api/items",
            &[],
            r#"[{"name":"Apples"},{"name":"Pears"}]"#,
        ),
    ]));
    let doc = load(&env, "https://a.test/");
    let ax = doc.ax(pageon::Viewport::default(), pageon::Budget::o200k(2000));
    assert!(ax.contains("Apples") && ax.contains("Pears"), "{ax}");
    assert!(doc.errors().is_empty(), "{:?}", doc.errors());
    let requests = doc.dump(pageon::Dump::Requests);
    assert!(
        requests.contains("GET 200 fetch https://a.test/api/items"),
        "{requests}"
    );
    assert_eq!(doc.summary().requests, 2);
}

#[test]
fn us1_xhr_renders_json() {
    let env = Capture::new(site(vec![
        html(
            "https://a.test/",
            "<div id=out></div><script>var x = new XMLHttpRequest(); var states = [];\
             x.onreadystatechange = () => states.push(x.readyState);\
             x.open('GET', '/api/user'); x.responseType = 'json';\
             x.onload = () => { out.textContent = x.status + ' ' + x.response.name + ' ' + states.join('') + ' ' + x.getResponseHeader('Content-Type'); };\
             x.send();</script>",
        ),
        json("https://a.test/api/user", &[], r#"{"name":"Ada"}"#),
    ]));
    let doc = load(&env, "https://a.test/");
    assert_eq!(out(&doc), "200 Ada 1234 application/json");
}

#[test]
fn us1_jquery_get_json() {
    let jq =
        root().join("bench/.cache/miniwob/miniwob/html/core/jquery-ui/external/jquery/jquery.js");
    let Ok(src) = std::fs::read_to_string(&jq) else {
        println!("skipped: run `uv run pageon-bench setup` in bench/ for jQuery");
        return;
    };
    let env = Capture::new(site(vec![
        html(
            "https://a.test/",
            "<div id=out></div><script src=/jquery.js></script><script>\
             $.getJSON('/api/n', function (d) { $('#out').text('n=' + d.n); });</script>",
        ),
        page(
            "https://a.test/jquery.js",
            200,
            &[("content-type", "text/javascript")],
            &src,
        ),
        json("https://a.test/api/n", &[], r#"{"n":42}"#),
    ]));
    let doc = load(&env, "https://a.test/");
    assert!(doc.errors().is_empty(), "{:?}", doc.errors());
    assert_eq!(out(&doc), "n=42");
}

/// One CORS/fetch case: the script's outcome, and the exchanges it may use.
struct Case {
    name: &'static str,
    script: &'static str,
    exchanges: Vec<Exchange>,
    want: &'static str,
}

/// Runs `script` (which must leave a string in `window.r`, maybe asynchronously) on
/// `https://a.test/`.
fn run_case(c: Case) -> (String, Capture, Document) {
    let page_html = format!(
        "<div id=out></div><script>window.r = 'pending';\
         const show = v => {{ out.textContent = v; }};\
         const fail = e => show(e.name || String(e));\
         {}</script>",
        c.script
    );
    let mut exchanges = vec![html("https://a.test/", &page_html)];
    exchanges.extend(c.exchanges);
    let env = Capture::new(site(exchanges));
    let doc = load(&env, "https://a.test/");
    (out(&doc), env, doc)
}

#[test]
fn us1_fetch_and_cors_matrix() {
    let acao = |o: &'static str| ("access-control-allow-origin", o);
    let cases = vec![
        Case {
            name: "same-origin",
            script: "fetch('/d').then(r => show(r.status + ' ' + r.type), fail)",
            exchanges: vec![json("https://a.test/d", &[], "{}")],
            want: "200 basic",
        },
        Case {
            name: "cross-origin without ACAO",
            script: "fetch('https://b.test/d').then(r => show(r.status), fail)",
            exchanges: vec![json("https://b.test/d", &[], "{}")],
            want: "TypeError",
        },
        Case {
            name: "cross-origin ACAO *",
            script: "fetch('https://b.test/d').then(r => show(r.status + ' ' + r.type), fail)",
            exchanges: vec![json("https://b.test/d", &[acao("*")], "{}")],
            want: "200 cors",
        },
        Case {
            name: "cross-origin ACAO exact",
            script: "fetch('https://b.test/d').then(r => show(r.status + ' ' + r.type), fail)",
            exchanges: vec![json("https://b.test/d", &[acao("https://a.test")], "{}")],
            want: "200 cors",
        },
        Case {
            name: "cross-origin ACAO other",
            script: "fetch('https://b.test/d').then(r => show(r.status), fail)",
            exchanges: vec![json("https://b.test/d", &[acao("https://c.test")], "{}")],
            want: "TypeError",
        },
        Case {
            name: "credentials with ACAO *",
            script: "fetch('https://b.test/d', {credentials: 'include'}).then(r => show(r.status), fail)",
            exchanges: vec![json("https://b.test/d", &[acao("*")], "{}")],
            want: "TypeError",
        },
        Case {
            name: "credentials with ACAO exact and ACAC",
            script: "fetch('https://b.test/d', {credentials: 'include'}).then(r => show(r.status + ' ' + r.type), fail)",
            exchanges: vec![json(
                "https://b.test/d",
                &[
                    acao("https://a.test"),
                    ("access-control-allow-credentials", "true"),
                ],
                "{}",
            )],
            want: "200 cors",
        },
        Case {
            name: "credentials without ACAC",
            script: "fetch('https://b.test/d', {credentials: 'include'}).then(r => show(r.status), fail)",
            exchanges: vec![json("https://b.test/d", &[acao("https://a.test")], "{}")],
            want: "TypeError",
        },
        Case {
            name: "no-cors is opaque",
            script: "fetch('https://b.test/d', {mode: 'no-cors'}).then(r => r.text().then(t => show(r.status + ' ' + r.type + ' [' + t + ']')), fail)",
            exchanges: vec![json("https://b.test/d", &[], "{\"secret\":1}")],
            want: "0 opaque []",
        },
        Case {
            name: "same-origin mode refuses cross-origin",
            script: "fetch('https://b.test/d', {mode: 'same-origin'}).then(r => show(r.status), fail)",
            exchanges: vec![],
            want: "TypeError",
        },
        Case {
            name: "preflight allows JSON POST",
            script: "fetch('https://b.test/d', {method: 'POST', headers: {'content-type': 'application/json', 'x-token': 't'}, body: '{}'})\
                     .then(r => show(r.status + ' ' + r.type), fail)",
            exchanges: vec![
                ex(
                    "OPTIONS",
                    "https://b.test/d",
                    204,
                    &[
                        acao("https://a.test"),
                        ("access-control-allow-headers", "Content-Type, X-Token"),
                    ],
                    "",
                ),
                ex(
                    "POST",
                    "https://b.test/d",
                    200,
                    &[acao("https://a.test")],
                    "{}",
                ),
            ],
            want: "200 cors",
        },
        Case {
            name: "preflight refuses a header",
            script: "fetch('https://b.test/d', {method: 'POST', headers: {'x-token': 't'}}).then(r => show(r.status), fail)",
            exchanges: vec![
                ex(
                    "OPTIONS",
                    "https://b.test/d",
                    204,
                    &[acao("https://a.test")],
                    "",
                ),
                ex(
                    "POST",
                    "https://b.test/d",
                    200,
                    &[acao("https://a.test")],
                    "{}",
                ),
            ],
            want: "TypeError",
        },
        Case {
            name: "preflight allows PUT",
            script: "fetch('https://b.test/d', {method: 'put'}).then(r => show(r.status), fail)",
            exchanges: vec![
                ex(
                    "OPTIONS",
                    "https://b.test/d",
                    200,
                    &[acao("*"), ("access-control-allow-methods", "GET, PUT")],
                    "",
                ),
                ex("PUT", "https://b.test/d", 200, &[acao("*")], ""),
            ],
            want: "200",
        },
        Case {
            name: "preflight refuses DELETE",
            script: "fetch('https://b.test/d', {method: 'DELETE'}).then(r => show(r.status), fail)",
            exchanges: vec![ex(
                "OPTIONS",
                "https://b.test/d",
                200,
                &[acao("*"), ("access-control-allow-methods", "GET, PUT")],
                "",
            )],
            want: "TypeError",
        },
        Case {
            name: "preflight 404",
            script: "fetch('https://b.test/d', {method: 'PATCH'}).then(r => show(r.status), fail)",
            exchanges: vec![ex("OPTIONS", "https://b.test/d", 404, &[acao("*")], "")],
            want: "TypeError",
        },
        Case {
            name: "same-origin redirect",
            script: "fetch('/old').then(r => show(r.status + ' ' + r.redirected + ' ' + r.url), fail)",
            exchanges: vec![
                redirect("https://a.test/old", "/new"),
                json("https://a.test/new", &[], "{}"),
            ],
            want: "200 true https://a.test/new",
        },
        Case {
            name: "redirect: error",
            script: "fetch('/old', {redirect: 'error'}).then(r => show(r.status), fail)",
            exchanges: vec![redirect("https://a.test/old", "/new")],
            want: "TypeError",
        },
        Case {
            name: "redirect: manual",
            script: "fetch('/old', {redirect: 'manual'}).then(r => show(r.status + ' ' + r.type), fail)",
            exchanges: vec![redirect("https://a.test/old", "/new")],
            want: "0 opaqueredirect",
        },
        Case {
            name: "cross-origin redirect taints the origin",
            script: "fetch('https://b.test/hop').then(r => show(r.status + ' ' + r.url), fail)",
            exchanges: vec![
                ex(
                    "GET",
                    "https://b.test/hop",
                    302,
                    &[("location", "https://c.test/d"), acao("https://a.test")],
                    "",
                ),
                json("https://c.test/d", &[acao("null")], "{}"),
            ],
            want: "200 https://c.test/d",
        },
        Case {
            name: "exposed headers",
            script: "fetch('https://b.test/d').then(r => show([r.headers.get('x-a'), r.headers.get('x-b'), r.headers.get('content-type')].join(',')), fail)",
            exchanges: vec![json(
                "https://b.test/d",
                &[
                    acao("*"),
                    ("x-a", "1"),
                    ("x-b", "2"),
                    ("access-control-expose-headers", "X-A"),
                ],
                "{}",
            )],
            want: "1,,application/json",
        },
        Case {
            name: "abort before send",
            script: "const c = new AbortController(); fetch('/d', {signal: c.signal}).then(r => show(r.status), fail); c.abort();",
            exchanges: vec![],
            want: "AbortError",
        },
        Case {
            name: "AbortSignal.timeout(0) fires first",
            script: "fetch('/d', {signal: AbortSignal.timeout(0)}).then(r => show(r.status), fail);",
            exchanges: vec![json("https://a.test/d", &[], "{}")],
            want: "TimeoutError",
        },
        Case {
            name: "Response and Headers objects",
            script: "const h = new Headers([['B', '2'], ['a', '1']]); h.append('a', '3');\
                     const res = Response.json({k: 1}, {status: 201});\
                     res.json().then(j => show([...h].map(p => p.join('=')).join('&') + ' ' + res.status + ' ' + j.k + ' ' + res.bodyUsed));",
            exchanges: vec![],
            want: "a=1, 3&b=2 201 1 true",
        },
    ];
    let n = cases.len();
    let mut failures = Vec::new();
    for c in cases {
        let (name, want) = (c.name, c.want);
        let (got, _, _) = run_case(c);
        if got != want {
            failures.push(format!("{name}: got {got:?}, want {want:?}"));
        }
    }
    println!("fetch/CORS matrix: {}/{n} cases", n - failures.len());
    assert!(n >= 20, "needs ≥ 20 cases");
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn us1_cors_details_reach_the_log_not_the_script() {
    let (got, env, doc) = run_case(Case {
        name: "preflight refusal",
        script: "fetch('https://b.test/d', {method: 'POST', headers: {'x-token': 't'}}).then(r => show(r.status), e => show(e.message))",
        exchanges: vec![ex(
            "OPTIONS",
            "https://b.test/d",
            204,
            &[("access-control-allow-origin", "https://a.test")],
            "",
        )],
        want: "",
    });
    assert_eq!(got, "Failed to fetch");
    // The actual request was never sent.
    assert!(
        !env.urls().contains(&"POST https://b.test/d".to_string()),
        "{:?}",
        env.urls()
    );
    let log = doc.dump(pageon::Dump::Requests);
    assert!(
        log.contains("OPTIONS cors-blocked fetch https://b.test/d (header x-token not allowed by Access-Control-Allow-Headers)"),
        "{log}"
    );
    assert_eq!(
        env.header(
            "OPTIONS",
            "https://b.test/d",
            "access-control-request-headers"
        )
        .as_deref(),
        Some("x-token")
    );
}

#[test]
fn us1_unhandled_rejections_are_page_errors() {
    let (_, _, doc) = run_case(Case {
        name: "unhandled",
        script: "fetch('https://b.test/d'); Promise.reject(new Error('boom')); Promise.reject(1).catch(() => {});",
        exchanges: vec![json("https://b.test/d", &[], "{}")],
        want: "",
    });
    let msgs: Vec<String> = doc.errors().iter().map(|e| e.message.clone()).collect();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert!(
        msgs.iter()
            .any(|m| m.contains("Uncaught (in promise) Error: boom")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter()
            .any(|m| m.contains("Uncaught (in promise) TypeError: Failed to fetch")),
        "{msgs:?}"
    );
}

#[test]
fn us1_url_and_search_params() {
    let (got, _, _) = run_case(Case {
        name: "url",
        script: "const u = new URL('/p?a=1&b=2#h', location.href); u.searchParams.append('c', '3 4');\
                 const p = new URLSearchParams({q: 'a b', r: '1'}); p.sort();\
                 show([u.href, u.searchParams.get('b'), String(u), URL.canParse('nope'), u.origin,\
                 [...p.keys()].join('+'), p.toString(), JSON.stringify(u)].join(' | '));",
        exchanges: vec![],
        want: "",
    });
    assert_eq!(
        got,
        "https://a.test/p?a=1&b=2&c=3+4#h | 2 | https://a.test/p?a=1&b=2&c=3+4#h | false | https://a.test | q+r | q=a+b&r=1 | \"https://a.test/p?a=1&b=2&c=3+4#h\""
    );
}

#[test]
fn us1_form_data_from_a_form() {
    let env = Capture::new(site(vec![html(
        "https://a.test/",
        "<form id=f><input name=user value=ada><input name=pw type=password value=s3><input type=checkbox name=keep checked>\
         <input type=checkbox name=no><select name=lang><option>en<option selected>de</select><input name=off disabled value=x></form>\
         <div id=out></div><script>out.textContent = [...new FormData(f)].map(p => p.join('=')).join('&');</script>",
    )]));
    let doc = load(&env, "https://a.test/");
    assert_eq!(out(&doc), "user=ada&pw=s3&keep=on&lang=de");
}

#[test]
fn us1_replays_are_identical() {
    let make = || {
        Capture::new(site(vec![
            html(
                "https://a.test/",
                "<div id=out></div><script>document.cookie = 'v=1';\
                 Promise.all([fetch('/a').then(r => r.text()), fetch('/b').then(r => r.text())])\
                 .then(([a, b]) => { out.textContent = a + b + document.cookie; });</script>",
            ),
            page("https://a.test/a", 200, &[("set-cookie", "s=2")], "A"),
            page("https://a.test/b", 200, &[], "B"),
        ]))
    };
    let (e1, e2) = (make(), make());
    let (a, b) = (load(&e1, "https://a.test/"), load(&e2, "https://a.test/"));
    assert_eq!(out(&a), "ABv=1; s=2");
    assert_eq!(a.dump(pageon::Dump::Html), b.dump(pageon::Dump::Html));
    assert_eq!(a.dump(pageon::Dump::Ax), b.dump(pageon::Dump::Ax));
    assert_eq!(a.requests(), b.requests());
    assert_eq!(e1.urls(), e2.urls());
}

// ----- cookies and storage -----

#[test]
fn us2_login_cookies_follow_redirects_and_loads() {
    let env = Capture::new(site(vec![
        html(
            "https://shop.test/login",
            "<div id=out></div><script>fetch('/api/login', {method: 'POST', body: new URLSearchParams({user: 'ada'})})\
             .then(r => r.json()).then(j => { out.textContent = j.ok + ' [' + document.cookie + ']'; });</script>",
        ),
        ex(
            "POST",
            "https://shop.test/api/login",
            200,
            &[
                ("content-type", "application/json"),
                ("set-cookie", "sid=abc; Path=/; HttpOnly; Secure"),
                ("set-cookie", "theme=dark; Path=/"),
            ],
            r#"{"ok":true}"#,
        ),
        ex(
            "GET",
            "https://shop.test/account",
            302,
            &[
                ("location", "/account/home"),
                ("set-cookie", "seen=1; Path=/account"),
            ],
            "",
        ),
        html("https://shop.test/account/home", "<p>Welcome back</p>"),
    ]));
    let session = Session::new(&env);
    let first = session
        .load("https://shop.test/login", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    // HttpOnly stays invisible to script.
    assert_eq!(out(&first), "true [theme=dark]");
    assert_eq!(
        env.header("POST", "https://shop.test/api/login", "content-type")
            .as_deref(),
        Some("application/x-www-form-urlencoded;charset=UTF-8")
    );
    session
        .load("https://shop.test/account", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        env.header("GET", "https://shop.test/account", "cookie")
            .as_deref(),
        Some("sid=abc; theme=dark")
    );
    // The cookie set on the redirect goes along to the next hop (longest path first).
    assert_eq!(
        env.header("GET", "https://shop.test/account/home", "cookie")
            .as_deref(),
        Some("seen=1; sid=abc; theme=dark")
    );
    let names: Vec<String> = session.cookies().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["sid", "theme", "seen"]);
}

#[test]
fn us2_storage_survives_loads_in_a_session() {
    let env = Capture::new(site(vec![
        html(
            "https://a.test/1",
            "<script>localStorage.setItem('k', 'v1'); localStorage.n = 2; sessionStorage.s = 'x';</script>",
        ),
        html(
            "https://a.test/2",
            "<div id=out></div><script>out.textContent = [localStorage.getItem('k'), localStorage.n, localStorage.length,\
             sessionStorage.getItem('s'), Object.keys(localStorage).join('+'), typeof localStorage.getItem].join(' ');</script>",
        ),
        html(
            "https://b.test/",
            "<div id=out></div><script>out.textContent = localStorage.length;</script>",
        ),
    ]));
    let session = Session::new(&env);
    let opts = LoadOptions::default();
    session
        .load("https://a.test/1", opts.clone())
        .unwrap_or_else(|e| panic!("{e}"));
    let two = session
        .load("https://a.test/2", opts.clone())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(out(&two), "v1 2 2 x k+n function");
    let other = session
        .load("https://b.test/", opts)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(out(&other), "0", "storage is per origin");
}

#[test]
fn us2_public_suffix_and_prefix_rejections_are_logged() {
    let env = Capture::new(site(vec![page(
        "https://shop.example.co.uk/",
        200,
        &[
            ("content-type", "text/html"),
            ("set-cookie", "a=1; Domain=co.uk"),
            (
                "set-cookie",
                "__Host-b=1; Secure; Path=/; Domain=example.co.uk",
            ),
            ("set-cookie", "ok=1; Domain=example.co.uk"),
        ],
        "<p>shop</p>",
    )]));
    let session = Session::new(&env);
    let doc = session
        .load("https://shop.example.co.uk/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let log = doc.dump(pageon::Dump::Requests);
    assert!(
        log.contains("cookie rejected: a (Domain=co.uk is a public suffix)"),
        "{log}"
    );
    assert!(log.contains("cookie rejected: __Host-b"), "{log}");
    let names: Vec<String> = session.cookies().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["ok"]);
}

#[test]
fn us2_partitioned_cookies_stay_with_their_top_level_site() {
    let frame = |top: &str| html(top, "<iframe src='https://widget.test/embed'></iframe>");
    let env = Capture::new(site(vec![
        frame("https://a.test/"),
        page(
            "https://widget.test/embed",
            200,
            &[
                ("content-type", "text/html"),
                ("set-cookie", "p=1; Secure; SameSite=None; Partitioned"),
                ("set-cookie", "u=1; Secure; SameSite=None"),
                ("set-cookie", "lax=1"),
            ],
            "<p>widget</p>",
        ),
        frame("https://b.test/"),
        html("https://widget.test/embed", "<p>widget</p>"),
    ]));
    let session = Session::new(&env);
    let opts = LoadOptions::default();
    let a = session
        .load("https://a.test/", opts.clone())
        .unwrap_or_else(|e| panic!("{e}"));
    session
        .load("https://b.test/", opts)
        .unwrap_or_else(|e| panic!("{e}"));
    // Under b.test: the unpartitioned SameSite=None cookie only; `lax=1` was refused
    // (SameSite=Lax set from a cross-site frame).
    let sent: Vec<String> = env
        .seen
        .borrow()
        .iter()
        .filter(|r| r.url == "https://widget.test/embed")
        .map(|r| {
            r.headers
                .iter()
                .find(|(k, _)| k == "cookie")
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(sent, ["", "u=1"]);
    assert!(
        a.dump(pageon::Dump::Requests)
            .contains("cookie rejected: lax")
    );
}

#[test]
fn us2_cookies_load_from_a_storage_state_file() {
    let dir = std::env::temp_dir().join(format!("pageon-net-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let path = dir.join("state.json");
    std::fs::write(
        &path,
        r#"{"cookies":[{"name":"sid","value":"s","domain":".a.test","path":"/","expires":-1,"httpOnly":true,"secure":true,"sameSite":"Lax"}],"origins":[]}"#,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let env = Capture::new(site(vec![html("https://www.a.test/", "<p>hi</p>")]));
    let session = Session::new(&env);
    assert_eq!(
        session
            .load_cookies(&path)
            .unwrap_or_else(|e| panic!("{e}")),
        1
    );
    session
        .load("https://www.a.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        env.header("GET", "https://www.a.test/", "cookie")
            .as_deref(),
        Some("sid=s")
    );
    let bad = dir.join("bad.json");
    std::fs::write(&bad, "{\"cookies\": 3}").unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(session.load_cookies(&bad), Err(LoadError::Io(_))));
}

// ----- egress -----

const PRIVATE: &[&str] = &[
    "http://127.0.0.1/x",
    "http://127.1.2.3:8080/x",
    "http://localhost/x",
    "http://LOCALHOST./x",
    "http://api.localhost:3000/x",
    "http://0x7f.1/x",
    "http://2130706433/x",
    "http://0177.0.0.1/x",
    "http://0.0.0.0/x",
    "http://10.0.0.5/x",
    "http://172.16.3.4/x",
    "http://192.168.1.1/x",
    "http://100.64.0.1/x",
    "http://169.254.169.254/latest/meta-data/",
    "http://198.18.0.1/x",
    "http://224.0.0.1/x",
    "http://255.255.255.255/x",
    "http://[::1]/x",
    "http://[::]/x",
    "http://[::ffff:127.0.0.1]/x",
    "http://[::ffff:a9fe:a9fe]/x",
    "http://[fd00::1]/x",
    "http://[fe80::1]/x",
    "http://[64:ff9b::a9fe:a9fe]/x",
    "http://[ff02::1]/x",
];

/// Pages on a public http origin that reach `target` in each way a page can.
fn egress_pages(target: &str) -> Vec<(&'static str, String)> {
    vec![
        ("script", format!("<script src='{target}'></script>")),
        ("style", format!("<link rel=stylesheet href='{target}'>")),
        ("frame", format!("<iframe src='{target}'></iframe>")),
        (
            "fetch",
            format!("<script>fetch('{target}').catch(() => {{}})</script>"),
        ),
        (
            "xhr",
            format!(
                "<script>var x = new XMLHttpRequest(); x.open('GET', '{target}'); x.send();</script>"
            ),
        ),
    ]
}

#[test]
fn us3_private_destinations_are_blocked_by_default() {
    let mut checked = 0;
    let mut escapes = Vec::new();
    for target in PRIVATE {
        // As the URL a caller loads, and behind a public redirect.
        let env = Capture::new(site(vec![redirect("http://pub.test/r", target)]));
        for url in [*target, "http://pub.test/r"] {
            checked += 1;
            match pageon::load(url, &env, LoadOptions::default()) {
                Err(LoadError::Blocked { reason, .. }) if reason.contains("private network") => {}
                other => escapes.push(format!(
                    "{url} (→ {target}): {:?}",
                    other.map(|d| d.final_url().to_string())
                )),
            }
        }
        assert!(
            !env.urls().iter().any(|u| u.contains(
                target
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .unwrap_or("?")
            )),
            "a request reached the environment: {:?}",
            env.urls()
        );
        for (kind, body) in egress_pages(target) {
            checked += 1;
            let env = Capture::new(site(vec![html("http://pub.test/", &body)]));
            let doc = load(&env, "http://pub.test/");
            let blocked = doc.requests().iter().any(
                |r| matches!(&r.outcome, Outcome::Blocked(why) if why.contains("private network")),
            );
            let sent = env
                .urls()
                .iter()
                .filter(|u| !u.ends_with("/robots.txt"))
                .count();
            if !blocked || sent != 1 {
                escapes.push(format!(
                    "{kind} {target}: {}",
                    doc.dump(pageon::Dump::Requests)
                ));
            }
        }
    }
    println!(
        "egress: {checked} private-network attempts, {} escaped",
        escapes.len()
    );
    assert!(PRIVATE.len() >= 25, "needs ≥ 25 destinations");
    assert!(escapes.is_empty(), "{escapes:#?}");
}

#[test]
fn us3_the_option_allows_private_destinations() {
    let allow = LoadOptions {
        allow_private_network: true,
        ..LoadOptions::default()
    };
    for target in PRIVATE {
        for (kind, body) in egress_pages(target) {
            let env = Capture::new(site(vec![html("http://pub.test/", &body)]));
            let doc = pageon::load("http://pub.test/", &env, allow.clone())
                .unwrap_or_else(|e| panic!("{e}"));
            assert!(
                !doc.requests()
                    .iter()
                    .any(|r| matches!(r.outcome, Outcome::Blocked(_))),
                "{kind} {target} blocked with the option:\n{}",
                doc.dump(pageon::Dump::Requests)
            );
            assert!(env.seen.borrow().iter().all(|r| r.allow_private));
        }
    }
}

#[test]
fn us3_blocked_loads_say_what_to_do() {
    let env = Capture::new(site(vec![]));
    let e = pageon::load("http://localhost:3000/", &env, LoadOptions::default())
        .err()
        .unwrap_or_else(|| panic!("must be blocked"));
    assert_eq!(
        e.to_string(),
        "blocked http://localhost:3000/: private network address localhost"
    );
    assert!(
        e.guidance().contains("--private-network allow"),
        "{}",
        e.guidance()
    );
}

#[test]
fn us3_mixed_content_is_blocked() {
    let env = Capture::new(site(vec![html(
        "https://a.test/",
        "<script src='http://b.test/x.js'></script><link rel=stylesheet href='http://b.test/x.css'>\
         <iframe src='http://b.test/f'></iframe><div id=out></div>\
         <script>fetch('http://b.test/d').then(() => out.textContent = 'leak', e => out.textContent = e.name);</script>",
    )]));
    let doc = load(&env, "https://a.test/");
    assert_eq!(out(&doc), "TypeError");
    let mixed = doc
        .requests()
        .iter()
        .filter(|r| matches!(&r.outcome, Outcome::Blocked(w) if w.contains("mixed content")))
        .count();
    assert_eq!(mixed, 4, "{}", doc.dump(pageon::Dump::Requests));
    assert_eq!(
        env.urls(),
        ["GET https://a.test/robots.txt", "GET https://a.test/"]
    );
}

// ----- honest headers -----

#[test]
fn us4_requests_carry_honest_headers() {
    let env = Capture::new(site(vec![
        html(
            "https://www.a.test/p?q=1#frag",
            "<link rel=stylesheet href=/s.css><script src=/app.js></script>\
             <script>fetch('https://api.a.test/v1', {headers: {'x-requested-with': 'pageon', cookie: 'forged=1', 'sec-foo': 'x'}})\
             .catch(() => {}); fetch('https://other.test/v1', {mode: 'no-cors'});</script>",
        ),
        page(
            "https://www.a.test/s.css",
            200,
            &[("content-type", "text/css")],
            "p{}",
        ),
        page(
            "https://www.a.test/app.js",
            200,
            &[("content-type", "text/javascript")],
            "",
        ),
        ex(
            "OPTIONS",
            "https://api.a.test/v1",
            204,
            &[
                ("access-control-allow-origin", "*"),
                ("access-control-allow-headers", "*"),
            ],
            "",
        ),
        json(
            "https://api.a.test/v1",
            &[("access-control-allow-origin", "*")],
            "{}",
        ),
        json("https://other.test/v1", &[], "{}"),
    ]));
    load(&env, "https://www.a.test/p?q=1#frag");
    let h = |m: &str, u: &str, n: &str| env.header(m, u, n);
    let doc = "https://www.a.test/p?q=1#frag";
    assert_eq!(h("GET", doc, "sec-fetch-site").as_deref(), Some("none"));
    assert_eq!(h("GET", doc, "sec-fetch-mode").as_deref(), Some("navigate"));
    assert_eq!(h("GET", doc, "sec-fetch-dest").as_deref(), Some("document"));
    assert_eq!(h("GET", doc, "sec-fetch-user").as_deref(), Some("?1"));
    assert!(h("GET", doc, "accept").is_some_and(|a| a.starts_with("text/html")));
    assert_eq!(h("GET", doc, "referer"), None);
    let css = "https://www.a.test/s.css";
    assert_eq!(h("GET", css, "sec-fetch-dest").as_deref(), Some("style"));
    assert_eq!(
        h("GET", css, "sec-fetch-site").as_deref(),
        Some("same-origin")
    );
    assert_eq!(
        h("GET", css, "referer").as_deref(),
        Some("https://www.a.test/p?q=1")
    );
    assert_eq!(
        h("GET", css, "accept").as_deref(),
        Some("text/css,*/*;q=0.1")
    );
    let js = "https://www.a.test/app.js";
    assert_eq!(h("GET", js, "sec-fetch-mode").as_deref(), Some("no-cors"));
    let api = "https://api.a.test/v1";
    assert_eq!(
        h("GET", api, "sec-fetch-site").as_deref(),
        Some("same-site")
    );
    assert_eq!(h("GET", api, "sec-fetch-mode").as_deref(), Some("cors"));
    assert_eq!(
        h("GET", api, "origin").as_deref(),
        Some("https://www.a.test")
    );
    assert_eq!(
        h("GET", api, "referer").as_deref(),
        Some("https://www.a.test/")
    );
    assert_eq!(h("GET", api, "x-requested-with").as_deref(), Some("pageon"));
    assert_eq!(h("GET", api, "cookie"), None, "a page cannot forge Cookie");
    assert_eq!(h("GET", api, "sec-foo"), None);
    assert_eq!(
        h("GET", api, "accept-language").as_deref(),
        Some("en-US,en;q=0.9")
    );
    assert_eq!(
        h("OPTIONS", api, "access-control-request-headers").as_deref(),
        Some("x-requested-with")
    );
    let other = "https://other.test/v1";
    assert_eq!(
        h("GET", other, "sec-fetch-site").as_deref(),
        Some("cross-site")
    );
    assert_eq!(
        h("GET", other, "sec-fetch-mode").as_deref(),
        Some("no-cors")
    );
    assert_eq!(h("GET", other, "origin"), None);
}
