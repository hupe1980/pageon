//! Actions on live tabs: input and default actions, typed errors, navigation,
//! dialogs, popups, downloads, the cache, replay, and a scripted MiniWoB++ oracle that uses
//! only the public API.

mod common;

use common::*;
use pageon::{Action, ActionError, Budget, LoadOptions, Mode, Reason, Replay, Session, Tab};
use pageon_env::Env;

// ----- helpers -----

/// A tab in full-observation mode, observed once (refs issued).
fn open<'s, E: Env>(session: &'s Session<E>, url: &str) -> (Tab<'s, E>, String) {
    let mut tab = session
        .open(url, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    tab.set_view(Budget::o200k(4000), Mode::Full);
    let obs = tab.observe();
    (tab, obs)
}

fn act<E: Env>(tab: &mut Tab<'_, E>, a: &str) -> pageon::Outcome {
    tab.act(a.parse().unwrap_or_else(|e| panic!("{e}")))
        .unwrap_or_else(|e| panic!("{a}: {e} ({})", e.guidance()))
}

// ----- US1: input, errors -----

const FORM: &str = "<h1>Form</h1><input id=t aria-label=Name><input id=n aria-label=Digitless>\
<select id=s aria-label=Country><option value=de>Germany<option value=fr>France</select>\
<button id=b>Count</button><button disabled>Off</button>\
<nav><button id=menu>Menu</button><ul id=sub hidden><li><button>Inside</button></ul></nav>\
<button id=hide>Hide name</button><button id=rm>Remove count</button><button id=dlg>Open dialog</button>\
<dialog id=d><button>In dialog</button></dialog><div id=out></div><div id=log></div>\
<script>var clicks = 0, log = [];\
b.addEventListener('click', e => { clicks++; out.textContent = 'clicks ' + clicks + (e.isTrusted ? ' trusted' : ''); });\
n.addEventListener('keydown', e => { if (/[0-9]/.test(e.key)) e.preventDefault(); });\
['input', 'change'].forEach(t => n.addEventListener(t, () => log.push(t)));\
s.addEventListener('change', () => log.push('select:' + s.value));\
menu.addEventListener('mouseenter', () => sub.hidden = false);\
hide.onclick = () => t.style.display = 'none';\
rm.onclick = () => b.remove();\
dlg.onclick = () => d.showModal();\
setInterval(() => document.getElementById('log').textContent = log.join(','), 50);</script>";

fn form_session() -> Session<Replay> {
    Session::new(Replay::new(site(vec![html("https://a.test/", FORM)])))
}

#[test]
fn us1_click_type_select_hover() {
    let session = form_session();
    let (mut tab, obs) = open(&session, "https://a.test/");
    let o = act(
        &mut tab,
        &format!("click {}", find(&obs, "button", "Count")),
    );
    assert!(o.to_string().starts_with("ok click e"), "{o}");
    assert_eq!(text_of(tab.document(), "out"), "clicks 1 trusted");
    act(
        &mut tab,
        &format!("type {} a1b2c", find(&obs, "textbox", "Digitless")),
    );
    act(
        &mut tab,
        &format!("select {} France", find(&obs, "combobox", "Country")),
    );
    act(&mut tab, "wait 100");
    assert_eq!(
        text_of(tab.document(), "log"),
        "input,input,input,change,select:fr"
    );
    let html = tab.document().dump(pageon::Dump::Ax);
    assert!(html.contains("=\"abc\""), "{html}");
    // Hover reveals the menu; its item becomes actionable.
    act(&mut tab, &format!("hover {}", find(&obs, "button", "Menu")));
    let obs = tab.observe();
    act(
        &mut tab,
        &format!("click {}", find(&obs, "button", "Inside")),
    );
    // Keys go to the focused element.
    act(
        &mut tab,
        &format!("click {}", find(&obs, "textbox", "Name")),
    );
    act(&mut tab, "press x");
    act(&mut tab, "press Backspace");
    act(&mut tab, "press y");
    assert!(tab.observe().contains("textbox e1 \"Name\" =\"y\""));
}

#[test]
fn us1_errors_are_typed_and_change_nothing() {
    let session = form_session();
    let (mut tab, obs) = open(&session, "https://a.test/");
    let name = find(&obs, "textbox", "Name");
    let count = find(&obs, "button", "Count");
    let off = find(&obs, "button", "Off");
    let country = find(&obs, "combobox", "Country");
    let outside = find(&obs, "button", "Hide name");
    let check = |tab: &mut Tab<'_, Replay>, a: String, want: &dyn Fn(&ActionError) -> bool| {
        let before = tab.document().dump(pageon::Dump::Html);
        let e = tab
            .act(a.parse().unwrap_or_else(|e| panic!("{e}")))
            .err()
            .unwrap_or_else(|| panic!("{a} must fail"));
        assert!(want(&e), "{a}: {e:?}");
        assert!(!e.guidance().is_empty());
        assert_eq!(
            tab.document().dump(pageon::Dump::Html),
            before,
            "{a} changed the page"
        );
        e
    };
    check(&mut tab, "click e999".into(), &|e| {
        matches!(e, ActionError::UnknownRef(_))
    });
    check(&mut tab, format!("click {off}"), &|e| {
        matches!(
            e,
            ActionError::NotActionable {
                reason: Reason::Disabled,
                ..
            }
        )
    });
    check(&mut tab, format!("type {count} hi"), &|e| {
        matches!(
            e,
            ActionError::NotActionable {
                reason: Reason::NotEditable,
                ..
            }
        )
    });
    let e = check(&mut tab, format!("select {country} Spain"), &|e| {
        matches!(e, ActionError::NoSuchOption { .. })
    });
    assert_eq!(e.guidance(), "use one of: Germany, France");
    // Hidden: hide the field, then act on its ref.
    act(&mut tab, &format!("click {outside}"));
    check(&mut tab, format!("type {name} x"), &|e| {
        matches!(
            e,
            ActionError::NotActionable {
                reason: Reason::Hidden,
                ..
            }
        )
    });
    // Stale: remove the button, then click its ref.
    act(
        &mut tab,
        &format!("click {}", find(&obs, "button", "Remove count")),
    );
    check(&mut tab, format!("click {count}"), &|e| {
        matches!(e, ActionError::StaleRef(_))
    });
    // Inert: a modal dialog is open.
    act(
        &mut tab,
        &format!("click {}", find(&obs, "button", "Open dialog")),
    );
    check(&mut tab, format!("click {outside}"), &|e| {
        matches!(
            e,
            ActionError::NotActionable {
                reason: Reason::Inert | Reason::Covered { .. },
                ..
            }
        )
    });
    let obs = tab.observe();
    assert!(
        obs.starts_with("err click e999: unknown ref e999\n"),
        "outcomes prefix the observation:\n{obs}"
    );
    assert!(obs.contains("\nok click "), "{obs}");
}

// ----- US2: navigation -----

fn shop() -> Vec<pageon_env::Exchange> {
    vec![
        html(
            "https://shop.test/",
            "<a href=/catalog>Catalog</a><form action=/search><input name=q aria-label=Search><button>Go</button></form>",
        ),
        html(
            "https://shop.test/catalog",
            "<h1>Catalog</h1><a href=/>Home</a>",
        ),
        html(
            "https://shop.test/search?q=red+shoes",
            "<h1>Results for red shoes</h1>",
        ),
        html(
            "https://shop.test/login",
            "<form method=post action=/session><input name=user aria-label=User>\
             <input name=pw type=password aria-label=Password><button>Sign in</button></form>",
        ),
        ex(
            "POST",
            "https://shop.test/session",
            303,
            &[
                ("location", "/account"),
                ("set-cookie", "sid=1; Path=/; HttpOnly; Secure"),
            ],
            "",
        ),
        html("https://shop.test/account", "<h1>Account</h1>"),
        html(
            "https://shop.test/upload",
            "<form method=post enctype=multipart/form-data action=/u><input name=a value=1>\
             <button>Send</button></form>",
        ),
        ex(
            "POST",
            "https://shop.test/u",
            200,
            &[("content-type", "text/html")],
            "<h1>Uploaded</h1>",
        ),
        html(
            "https://shop.test/js",
            "<script>location.replace('/catalog')</script>",
        ),
        html(
            "https://shop.test/loop",
            "<script>location.href = '/loop2'</script>",
        ),
        html(
            "https://shop.test/loop2",
            "<script>location.href = '/loop3'</script>",
        ),
        html(
            "https://shop.test/loop3",
            "<script>location.href = '/loop4'</script>",
        ),
        html(
            "https://shop.test/loop4",
            "<script>location.href = '/loop5'</script>",
        ),
        html(
            "https://shop.test/loop5",
            "<script>location.href = '/loop6'</script>",
        ),
        html(
            "https://shop.test/loop6",
            "<script>location.href = '/loop7'</script>",
        ),
        html(
            "https://shop.test/hash",
            "<a href=#details>Details</a><div id=out></div><script>addEventListener('hashchange', () => out.textContent = location.hash)</script>",
        ),
    ]
}

#[test]
fn us2_links_history_and_get_forms() {
    let mut ex = shop();
    ex.push(html("https://shop.test/", "<a href=/catalog>Catalog</a>"));
    ex.push(html("https://shop.test/catalog", "<h1>Catalog again</h1>"));
    let env = Capture::new(site(ex));
    let session = Session::new(&env);
    let (mut tab, obs) = open(&session, "https://shop.test/");
    let o = act(
        &mut tab,
        &format!("click {}", find(&obs, "link", "Catalog")),
    );
    assert_eq!(o.navigated.as_deref(), Some("https://shop.test/catalog"));
    assert!(tab.observe().contains("# \"Catalog\""));
    assert_eq!(
        env.header("GET", "https://shop.test/catalog", "sec-fetch-site")
            .as_deref(),
        Some("same-origin")
    );
    assert_eq!(
        env.header("GET", "https://shop.test/catalog", "referer")
            .as_deref(),
        Some("https://shop.test/")
    );
    act(&mut tab, "back");
    assert_eq!(tab.url(), "https://shop.test/");
    act(&mut tab, "forward");
    assert_eq!(tab.url(), "https://shop.test/catalog");
    assert!(
        tab.document()
            .dump(pageon::Dump::Text)
            .contains("Catalog again")
    );
    assert_eq!(tab.history().0.len(), 2);
    assert!(matches!(
        tab.act(Action::Forward),
        Err(ActionError::Unsupported(_))
    ));

    let env = Capture::new(site(shop()));
    let session = Session::new(&env);
    let (mut tab, obs) = open(&session, "https://shop.test/");
    act(
        &mut tab,
        &format!("type {} red shoes", find(&obs, "textbox", "Search")),
    );
    let o = act(&mut tab, "press Enter");
    assert_eq!(
        o.navigated.as_deref(),
        Some("https://shop.test/search?q=red+shoes"),
        "{o}"
    );
}

#[test]
fn us2_post_forms_set_cookies_and_redirect() {
    let env = Capture::new(site(shop()));
    let session = Session::new(&env);
    let (mut tab, obs) = open(&session, "https://shop.test/login");
    act(
        &mut tab,
        &format!("type {} ada", find(&obs, "textbox", "User")),
    );
    act(
        &mut tab,
        &format!("type {} s3cret", find(&obs, "textbox", "Password")),
    );
    let o = act(
        &mut tab,
        &format!("click {}", find(&obs, "button", "Sign in")),
    );
    assert_eq!(
        o.navigated.as_deref(),
        Some("https://shop.test/account"),
        "{o}"
    );
    let post = env
        .seen
        .borrow()
        .iter()
        .find(|r| r.method == "POST")
        .cloned()
        .unwrap_or_else(|| panic!("no POST: {:?}", env.urls()));
    assert_eq!(String::from_utf8_lossy(&post.body), "user=ada&pw=s3cret");
    let h = |n: &str| env.header("POST", "https://shop.test/session", n);
    assert_eq!(
        h("content-type").as_deref(),
        Some("application/x-www-form-urlencoded")
    );
    assert_eq!(h("origin").as_deref(), Some("https://shop.test"));
    assert_eq!(h("sec-fetch-user").as_deref(), Some("?1"));
    // 303 turns the POST into a GET that carries the new cookie.
    assert_eq!(
        env.header("GET", "https://shop.test/account", "cookie")
            .as_deref(),
        Some("sid=1")
    );
    assert_eq!(o.requests.len(), 1, "{o}");
    assert_eq!(o.requests[0].method, "POST");
    assert_eq!(
        o.requests[0].final_url.as_deref(),
        Some("https://shop.test/account")
    );
}

#[test]
fn us2_multipart_script_navigation_and_fragments() {
    let env = Capture::new(site(shop()));
    let session = Session::new(&env);
    let (mut tab, obs) = open(&session, "https://shop.test/upload");
    act(&mut tab, &format!("click {}", find(&obs, "button", "Send")));
    let ct = env
        .header("POST", "https://shop.test/u", "content-type")
        .unwrap_or_default();
    assert!(ct.starts_with("multipart/form-data; boundary="), "{ct}");

    // A client-side redirect while loading: one history entry, the final page.
    let tab = session
        .open("https://shop.test/js", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(tab.url(), "https://shop.test/catalog");
    assert_eq!(tab.history().0, ["https://shop.test/catalog"]);
    // Endless client-side redirects stop with an error.
    let e = session
        .open("https://shop.test/loop", LoadOptions::default())
        .err()
        .unwrap_or_else(|| panic!("must stop"));
    assert!(matches!(e, pageon::LoadError::TooManyRedirects(_)), "{e}");

    // A fragment link stays on the page: a same-document navigation, nothing requested.
    let (mut tab, obs) = open(&session, "https://shop.test/hash");
    let before = env.urls().len();
    let o = act(
        &mut tab,
        &format!("click {}", find(&obs, "link", "Details")),
    );
    assert!(
        o.same_document && o.navigated.as_deref() == Some("https://shop.test/hash#details"),
        "{o}"
    );
    assert_eq!(env.urls().len(), before);
    assert_eq!(text_of(tab.document(), "out"), "#details");
}

// ----- US3: dialogs, popups, downloads -----

const DIALOGS: &str = "<button id=del>Delete</button><button id=ask>Ask</button><button id=pop>Popup</button>\
<a href=/new target=_blank>New tab</a><a href=/report.pdf>Report</a><a href=/data>Export</a><div id=out></div>\
<script>del.onclick = () => { out.textContent = confirm('Delete?') ? 'deleted' : 'kept'; };\
ask.onclick = () => { alert('Hi'); out.textContent = String(prompt('Name?', 'Ada')); };\
pop.onclick = () => { out.textContent = String(window.open('/popup')); };</script>";

fn dialog_site() -> Vec<pageon_env::Exchange> {
    vec![
        html("https://a.test/", DIALOGS),
        page(
            "https://a.test/report.pdf",
            200,
            &[("content-type", "application/pdf")],
            "%PDF-1.4 x",
        ),
        page(
            "https://a.test/data",
            200,
            &[
                ("content-type", "text/csv"),
                ("content-disposition", "attachment; filename=\"export.csv\""),
            ],
            "a,b\n1,2\n",
        ),
    ]
}

#[test]
fn us3_dialogs_popups_and_downloads() {
    let session = Session::new(Replay::new(site(dialog_site())));
    let (mut tab, obs) = open(&session, "https://a.test/");
    let o = act(
        &mut tab,
        &format!("click {}", find(&obs, "button", "Delete")),
    );
    assert_eq!(text_of(tab.document(), "out"), "kept");
    assert!(
        o.to_string()
            .contains("dialog confirm \"Delete?\" → dismissed"),
        "{o}"
    );
    let o = act(&mut tab, &format!("click {}", find(&obs, "button", "Ask")));
    assert_eq!(text_of(tab.document(), "out"), "null");
    assert_eq!(o.dialogs.len(), 2);
    let o = act(
        &mut tab,
        &format!("click {}", find(&obs, "button", "Popup")),
    );
    assert_eq!(o.popups, ["https://a.test/popup"]);
    assert_eq!(text_of(tab.document(), "out"), "null");
    let o = act(
        &mut tab,
        &format!("click {}", find(&obs, "link", "New tab")),
    );
    assert_eq!(o.popups, ["https://a.test/new"]);
    assert!(o.navigated.is_none());
    let o = act(&mut tab, &format!("click {}", find(&obs, "link", "Report")));
    assert!(
        o.to_string()
            .contains("download \"report.pdf\" (\"application/pdf\", 10 B)"),
        "{o}"
    );
    assert_eq!(tab.url(), "https://a.test/");
    act(&mut tab, &format!("click {}", find(&obs, "link", "Export")));
    let names: Vec<&str> = tab
        .downloads()
        .iter()
        .map(|d| d.filename.as_str())
        .collect();
    assert_eq!(names, ["report.pdf", "export.csv"]);
    assert_eq!(tab.downloads()[1].bytes, b"a,b\n1,2\n");

    let accept = LoadOptions {
        dialogs: pageon::DialogPolicy::Accept,
        ..LoadOptions::default()
    };
    let session = Session::new(Replay::new(site(dialog_site())));
    let mut tab = session
        .open("https://a.test/", accept)
        .unwrap_or_else(|e| panic!("{e}"));
    tab.set_view(Budget::o200k(4000), Mode::Full);
    let obs = tab.observe();
    act(
        &mut tab,
        &format!("click {}", find(&obs, "button", "Delete")),
    );
    assert_eq!(text_of(tab.document(), "out"), "deleted");
    act(&mut tab, &format!("click {}", find(&obs, "button", "Ask")));
    assert_eq!(text_of(tab.document(), "out"), "Ada");
}

// ----- US4: cache -----

#[test]
fn us4_fresh_scripts_are_not_fetched_again() {
    let pagejs = |url: &str| {
        html(
            url,
            "<script src=/app.js></script><script src=/nostore.js></script><a href=/two>Two</a>",
        )
    };
    let js = |url: &str, cc: &str| {
        page(
            url,
            200,
            &[("content-type", "text/javascript"), ("cache-control", cc)],
            "",
        )
    };
    let env = Capture::new(site(vec![
        pagejs("https://a.test/"),
        js("https://a.test/app.js", "max-age=600"),
        js("https://a.test/nostore.js", "no-store"),
        pagejs("https://a.test/two"),
        js("https://a.test/nostore.js", "no-store"),
    ]));
    let session = Session::new(&env);
    let (mut tab, obs) = open(&session, "https://a.test/");
    let o = act(&mut tab, &format!("click {}", find(&obs, "link", "Two")));
    let fetched = |u: &str| env.urls().iter().filter(|x| x.ends_with(u)).count();
    assert_eq!(fetched("/app.js"), 1);
    assert_eq!(fetched("/nostore.js"), 2);
    assert!(
        o.requests
            .iter()
            .any(|r| r.from_cache && r.url == "https://a.test/app.js"),
        "{o}"
    );
}

// ----- replay -----

#[test]
fn replayed_tab_sessions_are_identical() {
    let run = || {
        let session = Session::new(Replay::new(site(shop())));
        let (mut tab, obs) = open(&session, "https://shop.test/login");
        let mut lines = vec![obs];
        for a in [
            format!("type {} ada", find(&lines[0], "textbox", "User")),
            format!("type {} pw", find(&lines[0], "textbox", "Password")),
            "press Tab".to_string(),
            "wait 300".to_string(),
            format!("click {}", find(&lines[0], "button", "Sign in")),
        ] {
            let o = act(&mut tab, &a);
            lines.push(o.to_string());
            lines.push(tab.observe());
        }
        lines.push(tab.document().dump(pageon::Dump::Html));
        lines
    };
    assert_eq!(run(), run());
}

// ----- the MiniWoB++ oracle -----

/// The task's query (the first text line of the view).
fn query(obs: &str) -> String {
    obs.lines()
        .find_map(|l| {
            l.trim_start()
                .strip_prefix("text ")
                .map(|t| quoted(t).concat())
        })
        .unwrap_or_default()
}

/// Solves one episode through the public API; returns the reward `#reward-last` shows.
fn solve(task: &str, log: pageon_env::Log) -> Result<f64, String> {
    let session = Session::new(Replay::new(log));
    let (mut tab, obs) = open(&session, &format!("{MINIWOB_ORIGIN}miniwob/{task}.html"));
    let q = query(&obs);
    let args: Vec<String> = q
        .split('"')
        .skip(1)
        .step_by(2)
        .map(|s| s.trim().to_string())
        .collect();
    fn step<E: Env>(tab: &mut Tab<'_, E>, a: String) -> Result<(), String> {
        tab.act(a.parse()?)
            .map(|_| ())
            .map_err(|e| format!("{a}: {e}"))
    }
    let tab = &mut tab;
    let run = |tab: &mut Tab<'_, Replay>, a: String| step(tab, a);
    let button = |name: &str| find(&obs, "button", name);
    match task {
        "click-button" => run(tab, format!("click {}", button(&args[0])))?,
        "click-test" => run(tab, format!("click {}", refs(&obs, "button")[0].0))?,
        "click-test-2" => {
            let name = q.trim_start_matches("Click button ").trim_end_matches('.');
            run(tab, format!("click {}", button(name)))?;
        }
        "click-dialog" => run(tab, format!("click {}", refs(&obs, "button")[0].0))?,
        "click-dialog-2" => {
            let r = if args[0] == "x" {
                refs(&obs, "button")[0].0.clone()
            } else {
                button(&args[0])
            };
            run(tab, format!("click {r}"))?;
        }
        "click-tab" => {
            let name = q.trim_start_matches("Click on ").trim_end_matches('.');
            run(tab, format!("click {}", find(&obs, "tab", name)))?;
        }
        "click-checkboxes" => {
            let list = q
                .trim_start_matches("Select ")
                .trim_end_matches(" and click Submit.");
            if list != "nothing" {
                for name in list
                    .split([',', ' '])
                    .filter(|w| !w.is_empty() && *w != "and")
                {
                    run(tab, format!("click {}", find(&obs, "checkbox", name)))?;
                }
            }
            run(tab, format!("click {}", button("Submit")))?;
        }
        "click-option" => {
            let name = q
                .trim_start_matches("Select ")
                .trim_end_matches(" and click Submit.");
            run(tab, format!("click {}", find(&obs, "radio", name)))?;
            run(tab, format!("click {}", button("Submit")))?;
        }
        "click-collapsible" => {
            run(tab, format!("click {}", refs(&obs, "tab")[0].0))?;
            let now = tab.observe();
            let submit = refs(&now, "button")
                .into_iter()
                .chain(refs(&now, "tab"))
                .find(|(_, n)| n == "Submit")
                .map(|(r, _)| r)
                .ok_or_else(|| format!("no Submit in\n{now}"))?;
            run(tab, format!("click {submit}"))?;
        }
        "focus-text" => run(tab, format!("click {}", refs(&obs, "textbox")[0].0))?,
        "focus-text-2" => {
            let n = ["1st", "2nd", "3rd"]
                .iter()
                .position(|w| q.contains(w))
                .ok_or(format!("query {q:?}"))?;
            run(tab, format!("click {}", refs(&obs, "textbox")[n].0))?;
        }
        "enter-text" => {
            run(
                tab,
                format!("type {} {}", refs(&obs, "textbox")[0].0, args[0]),
            )?;
            run(tab, format!("click {}", button("Submit")))?;
        }
        "enter-password" => {
            for (r, _) in refs(&obs, "textbox") {
                run(tab, format!("type {r} {}", args[0]))?;
            }
            run(tab, format!("click {}", button("Submit")))?;
        }
        "login-user" => {
            let fields = refs(&obs, "textbox");
            run(tab, format!("type {} {}", fields[0].0, args[0]))?;
            run(tab, format!("type {} {}", fields[1].0, args[1]))?;
            run(tab, format!("click {}", button("Login")))?;
        }
        "choose-list" => {
            let name = q
                .trim_start_matches("Select ")
                .trim_end_matches(" from the list and click Submit.");
            run(
                tab,
                format!("select {} {name}", refs(&obs, "combobox")[0].0),
            )?;
            run(tab, format!("click {}", button("Submit")))?;
        }
        other => return Err(format!("no oracle for {other}")),
    }
    let r = text_of(tab.document(), "reward-last");
    r.trim()
        .parse::<f64>()
        .map_err(|_| format!("no reward (#reward-last = {r:?}); query {q:?}"))
}

const ORACLE_TASKS: &[&str] = &[
    "click-button",
    "click-checkboxes",
    "click-collapsible",
    "click-dialog",
    "click-dialog-2",
    "click-option",
    "click-tab",
    "click-test",
    "click-test-2",
    "choose-list",
    "enter-password",
    "enter-text",
    "focus-text",
    "focus-text-2",
    "login-user",
];

#[test]
fn miniwob_oracle_earns_rewards() {
    let Some(rec) = miniwob_log() else {
        println!("skipped: run `uv run pageon-bench setup` in bench/ to fetch MiniWoB++");
        return;
    };
    let mut ok = 0;
    let mut total = 0;
    let mut failures = Vec::new();
    for task in ORACLE_TASKS {
        for seed in 0..3u64 {
            total += 1;
            let mut log = rec.clone();
            log.random = log
                .random
                .iter()
                .map(|v| v.wrapping_mul(seed * 2 + 1).wrapping_add(seed))
                .collect();
            match solve(task, log) {
                Ok(r) if r > 0.0 => ok += 1,
                Ok(r) => failures.push(format!("{task} #{seed}: reward {r}")),
                Err(e) => failures.push(format!("{task} #{seed}: {e}")),
            }
        }
    }
    let pct = 100.0 * f64::from(ok) / f64::from(total);
    println!("MiniWoB++ oracle: {ok}/{total} episodes rewarded ({pct:.1} %)");
    for f in &failures {
        println!("  {f}");
    }
    assert!(pct >= 90.0, "needs ≥ 90 %");
}

// ----- random action sequences -----

#[test]
fn random_action_sequences_never_panic() {
    let mut x: u64 = 7;
    let mut next = move |n: usize| {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        ((x >> 33) as usize) % n
    };
    let verbs = [
        "click", "type", "press", "select", "hover", "wait", "back", "forward", "goto",
    ];
    let texts = [
        "",
        "a1",
        "hello world",
        "\u{1F600}",
        "x".repeat(300).leak(),
        "\n",
    ];
    let keys = [
        "Enter",
        "Tab",
        "Escape",
        "Backspace",
        " ",
        "a",
        "ArrowDown",
        "Delete",
    ];
    let mut actions = 0;
    for round in 0..50 {
        let mut ex = vec![html("https://a.test/", FORM)];
        ex.extend(dialog_site().into_iter().skip(1));
        ex.extend(shop());
        let session = Session::new(Replay::new(site(ex)));
        let start = [
            "https://a.test/",
            "https://shop.test/",
            "https://shop.test/login",
        ][round % 3];
        let Ok(mut tab) = session.open(start, LoadOptions::default()) else {
            continue;
        };
        for _ in 0..20 {
            if next(4) == 0 {
                tab.observe();
            }
            let r = format!("e{}", next(16) + 1);
            let a = match verbs[next(verbs.len())] {
                "click" => Action::Click(r),
                "type" => Action::Type(r, texts[next(texts.len())].to_string()),
                "press" => Action::Press(keys[next(keys.len())].to_string()),
                "select" => Action::Select(r, vec!["France".into()]),
                "hover" => Action::Hover(r),
                "wait" => Action::Wait(next(2000) as u64),
                "back" => Action::Back,
                "forward" => Action::Forward,
                _ => Action::Goto("https://shop.test/catalog".into()),
            };
            let _ = tab.act(a);
            actions += 1;
            let internal = tab
                .document()
                .errors()
                .iter()
                .any(|e| e.message.contains("internal error in pageon"));
            assert!(!internal, "{:?}", tab.document().errors());
        }
    }
    assert!(actions >= 900, "{actions}");
}

// ----- speed -----

#[test]
#[ignore = "timing; run in release"]
#[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
fn click_is_fast() {
    let Some(rec) = miniwob_log() else { return };
    let url = format!("{MINIWOB_ORIGIN}miniwob/click-button.html");
    let (mut open_ms, mut click_ms) = (0.0, 0.0);
    for _ in 0..5 {
        let session = Session::new(Replay::new(rec.clone()));
        let t = std::time::Instant::now();
        let (mut tab, obs) = open(&session, &url);
        open_ms += t.elapsed().as_secs_f64() * 1000.0 / 5.0;
        let q = query(&obs);
        let name = q.split('"').nth(1).unwrap_or_default().to_string();
        let r = find(&obs, "button", &name);
        let t = std::time::Instant::now();
        act(&mut tab, &format!("click {r}"));
        click_ms += t.elapsed().as_secs_f64() * 1000.0 / 5.0;
    }
    println!("click-button: open {open_ms:.1} ms, click with settle {click_ms:.1} ms");
    assert!(click_ms < 20.0, "click < 20 ms");
    assert!(open_ms < 150.0, "open < 150 ms");
}
