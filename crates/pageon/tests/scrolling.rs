//! Scrolling: the CSSOM View API against Chromium's values, scroll events and observers,
//! fixed elements, fragments and keys; the agent's `scroll` action and auto-scroll before
//! actions, caged and in-process; views, screenshots and replay.

mod common;

use common::*;
use pageon::{LoadOptions, Replay};
use serde_json::Value;

const CSSOM: &str = include_str!("fixtures/scroll/cssom.html");
const ORACLE: &str = include_str!("fixtures/scroll/cssom.chromium.json");

#[test]
fn cssom_view_matches_chromium() {
    let env = Replay::new(site(vec![html("https://scroll.test/", CSSOM)]));
    let doc = pageon::load("https://scroll.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let ours: Vec<(String, f64)> =
        serde_json::from_str::<Vec<(String, f64)>>(&text_of(&doc, "out"))
            .unwrap_or_else(|e| panic!("{e}: {:?}", doc.errors()));
    let oracle: Value = serde_json::from_str(ORACLE).unwrap_or_default();
    let theirs: Vec<(String, f64)> =
        serde_json::from_value(oracle["results"].clone()).unwrap_or_default();
    assert_eq!(ours.len(), theirs.len(), "{ours:?}");
    let off: Vec<String> = ours
        .iter()
        .zip(&theirs)
        .filter(|((_, a), (_, b))| (a - b).abs() > 1.0)
        .map(|((k, a), (_, b))| format!("{k}: pageon {a}, Chromium {b}"))
        .collect();
    assert!(off.is_empty(), "{off:#?}");
}

use pageon::replay::{Caged, InProcess, rerun};
use pageon::{Action, Budget, Mode, Recording, Sandbox, SandboxOptions, Session};

const FIXED: &str = "<div style='position:fixed;top:0;left:0;right:0;height:50px;background:#eee'>Shop header</div>";

/// The fixture site: a long form, an infinite feed, a terms box, in-page links, a page that
/// cancels the wheel.
fn site_log() -> pageon_env::Log {
    let mut ex = vec![
        html(
            "https://scroll.test/form",
            &format!(
                "<body style='margin:0'>{FIXED}<div style='height:3000px'></div>\
                 <button onclick=\"log.textContent = 'clicked at clientY ' + event.clientY + ' pageY ' + event.pageY\">Deep</button>\
                 <p id=log>not clicked</p><div style='height:2000px'></div></body>"
            ),
        ),
        html(
            "https://scroll.test/feed",
            r#"<body style="margin:0"><h1>Feed</h1><ul id=list></ul><div id=sentinel style="height:1px"></div>
<script>
let page = 0;
const add = (items) => { for (const t of items) { const li = document.createElement('li'); li.style.height = '100px'; li.textContent = t; list.append(li); } };
new IntersectionObserver((entries) => {
  if (!entries.some((e) => e.isIntersecting) || page >= 3) return;
  page += 1;
  fetch('/feed?page=' + page).then((r) => r.json()).then(add);
}).observe(sentinel);
</script></body>"#,
        ),
        html(
            "https://scroll.test/terms",
            "<body style='margin:0'><div id=box style='overflow:auto;height:120px;width:400px'>\
             <div style='height:900px'>Terms of service</div>\
             <label><input type=checkbox id=agree> I agree</label></div></body>",
        ),
        html(
            "https://scroll.test/links",
            "<body style='margin:0'><a href='#faq'>Go to the FAQ</a><div style='height:3000px'></div>\
             <h2 id=faq>FAQ</h2><div style='height:2000px'></div></body>",
        ),
        html(
            "https://scroll.test/cover",
            "<body style='margin:0'><div style='height:3000px'></div>\
             <button onclick=\"log.textContent = 'paid'\">Pay</button><p id=log>not paid</p>\
             <div style='height:2000px'></div>\
             <div id=cover onclick=\"log.textContent = 'cover clicked'\" style='display:none;position:fixed;inset:0;background:#fff'>Win a prize</div>\
             <script>addEventListener('scroll', () => { cover.style.display = 'block' })</script></body>",
        ),
        html(
            "https://scroll.test/wheel",
            "<body style='margin:0'><div style='height:5000px'>Carousel</div>\
             <script>addEventListener('wheel', (e) => e.preventDefault(), {passive: false})</script></body>",
        ),
    ];
    for p in 1..=3 {
        let items: Vec<String> = (1..=10)
            .map(|i| format!("\"Item {}\"", (p - 1) * 10 + i))
            .collect();
        ex.push(json(
            &format!("https://scroll.test/feed?page={p}"),
            &[],
            &format!("[{}]", items.join(",")),
        ));
    }
    site(ex)
}

/// Runs `steps` (action text, with `{Name}` replaced by the ref of the element named so) on
/// `url`, caged or in-process; returns each outcome (or error) and the final observation.
fn drive(url: &str, steps: &[&str], caged: bool) -> (Vec<String>, String) {
    let env = Replay::new(site_log());
    let resolve = |obs: &str, step: &str| -> String {
        match (step.find('{'), step.find('}')) {
            (Some(a), Some(b)) => {
                let name = &step[a + 1..b];
                let r = obs
                    .lines()
                    .find(|l| l.contains(&format!("\"{name}\"")))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or_else(|| panic!("no {name:?} in {obs}"));
                format!("{}{r}{}", &step[..a], &step[b + 1..])
            }
            _ => step.to_string(),
        }
    };
    let mut outcomes = Vec::new();
    if caged {
        let mut sb = Sandbox::new(
            &env,
            SandboxOptions {
                program: Some(env!("CARGO_BIN_EXE_pageon").into()),
                ..SandboxOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let mut tab = sb
            .open(url, LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        for step in steps {
            let obs = tab.observe_full().unwrap_or_else(|e| panic!("{e}"));
            let a: Action = resolve(&obs, step)
                .parse()
                .unwrap_or_else(|e| panic!("{e}"));
            outcomes.push(
                tab.act(a)
                    .map_or_else(|e| format!("error {e}"), |o| o.to_string()),
            );
        }
        let last = tab.observe_full().unwrap_or_else(|e| panic!("{e}"));
        (outcomes, last)
    } else {
        let session = Session::new(&env);
        let mut tab = session
            .open(url, LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        tab.set_view(Budget::o200k(100_000), Mode::Full);
        for step in steps {
            let obs = tab.observe_full();
            let a: Action = resolve(&obs, step)
                .parse()
                .unwrap_or_else(|e| panic!("{e}"));
            outcomes.push(
                tab.act(a)
                    .map_or_else(|e| format!("error {e}"), |o| o.to_string()),
            );
        }
        (outcomes, tab.observe_full())
    }
}

/// Both ways, with identical results.
fn both(url: &str, steps: &[&str]) -> (Vec<String>, String) {
    let inproc = drive(url, steps, false);
    let caged = drive(url, steps, true);
    assert_eq!(inproc.0, caged.0, "outcomes differ caged/in-process");
    inproc
}

#[test]
fn actions_scroll_their_target_into_view() {
    let (out, obs) = both("https://scroll.test/form", &["click {Deep}"]);
    assert!(out[0].contains("scrolled to 0,"), "{}", out[0]);
    let y: f64 = obs
        .split("clicked at clientY ")
        .nth(1)
        .and_then(|t| t.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| panic!("{obs}"));
    assert!(
        (0.0..720.0).contains(&y),
        "the click landed off-screen: {y}"
    );
    let page_y: f64 = obs
        .split(" pageY ")
        .nth(1)
        .and_then(|t| t.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| panic!("{obs}"));
    assert!(page_y > 3000.0, "pageY includes the scroll: {page_y}");
    assert!(
        obs.contains("scroll:"),
        "the header shows the position: {obs}"
    );
}

#[test]
fn scrolling_down_loads_the_feed() {
    let steps = ["scroll down"; 8];
    let (out, obs) = both("https://scroll.test/feed", &steps);
    for i in [1, 10, 20, 30] {
        assert!(
            obs.contains(&format!("\"Item {i}\"")),
            "Item {i} missing: {obs}"
        );
    }
    assert!(!obs.contains("Item 31"), "{obs}");
    assert!(out[0].contains("scrolled to 0,"), "{out:?}");
    assert!(
        out.last().is_some_and(|o| o.contains("already at the end")),
        "{out:?}"
    );
}

#[test]
fn keys_scroll_the_page() {
    let (out, _) = both("https://scroll.test/feed", &["press End", "press Home"]);
    assert!(out[0].contains("scrolled to 0,"), "{out:?}");
    assert!(out[1].contains("scrolled to 0,0"), "{out:?}");
}

#[test]
fn containers_scroll_to_their_content() {
    let (out, obs) = both("https://scroll.test/terms", &["click {I agree}"]);
    assert!(out[0].starts_with("ok click"), "{out:?}");
    assert!(obs.contains("checked"), "{obs}");
}

#[test]
fn in_page_links_scroll_to_their_target() {
    let (out, obs) = both("https://scroll.test/links", &["click {Go to the FAQ}"]);
    assert!(out[0].contains("scrolled to 0,3"), "{out:?}");
    assert!(obs.contains("scroll:3"), "{obs}");
}

#[test]
fn a_cover_shown_while_scrolling_into_view_refuses_the_action() {
    let (out, obs) = both("https://scroll.test/cover", &["click {Pay}"]);
    assert!(
        out[0].starts_with("error") && out[0].contains("covered"),
        "{out:?}"
    );
    assert!(
        obs.contains("not paid") && !obs.contains("cover clicked"),
        "{obs}"
    );
}

#[test]
fn a_cancelled_wheel_does_not_scroll() {
    let (out, _) = both("https://scroll.test/wheel", &["scroll down"]);
    assert!(out[0].contains("the page prevented scrolling"), "{out:?}");
}

#[test]
fn scroll_actions_parse_and_print() {
    for (text, back) in [
        ("scroll down", "scroll down"),
        ("scroll up 300", "scroll up 300"),
        ("scroll by 120,-40", "scroll by 120,-40"),
        ("scroll e12", "scroll e12"),
        ("scroll e5 left", "scroll e5 left"),
        ("scroll e5 right 80", "scroll e5 right 80"),
    ] {
        let a: Action = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(a.to_string(), back);
    }
    for bad in [
        "scroll",
        "scroll sideways 3",
        "scroll down -5",
        "scroll by 1",
    ] {
        assert!(bad.parse::<Action>().is_err(), "{bad}");
    }
}

#[test]
fn scroll_sessions_replay() {
    let rec = Recording::new(Replay::new(site_log()));
    {
        let session = Session::new(&rec);
        let mut tab = session
            .open("https://scroll.test/feed", LoadOptions::default())
            .unwrap_or_else(|e| panic!("{e}"));
        for a in ["scroll down", "scroll down", "press End"] {
            let _ = tab.observe();
            tab.act(a.parse().unwrap_or_else(|e| panic!("{e}")))
                .unwrap_or_else(|e| panic!("{e}"));
        }
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

#[test]
fn screenshots_show_the_scrolled_viewport() {
    let env = Replay::new(site_log());
    let session = Session::new(&env);
    let mut tab = session
        .open("https://scroll.test/form", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let obs = tab.observe();
    let deep = find(&obs, "button", "Deep");
    let marks = pageon::ScreenshotOptions {
        marks: true,
        ..pageon::ScreenshotOptions::default()
    };
    let before = tab.screenshot(&marks).unwrap_or_else(|e| panic!("{e}"));
    let legend = before.legend.clone().unwrap_or_default();
    assert!(!legend.contains("Deep"), "below the fold: {legend}");
    let out = tab
        .act(
            format!("scroll {deep}")
                .parse()
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(out.to_string().contains("scrolled to 0,"), "{out}");
    let after = tab.screenshot(&marks).unwrap_or_else(|e| panic!("{e}"));
    let legend = after.legend.clone().unwrap_or_default();
    assert!(legend.contains("Deep"), "in view now: {legend}");
    assert_ne!(before.bytes, after.bytes);
    // Full pages are unchanged by scrolling.
    let full = pageon::ScreenshotOptions {
        area: pageon::Area::FullPage,
        scale: 0.25,
        ..pageon::ScreenshotOptions::default()
    };
    let a = tab.screenshot(&full).unwrap_or_else(|e| panic!("{e}"));
    tab.act("scroll up 10000".parse().unwrap_or_else(|e| panic!("{e}")))
        .unwrap_or_else(|e| panic!("{e}"));
    let b = tab.screenshot(&full).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(a.bytes, b.bytes);
}

#[test]
fn a_lookahead_scrolls_on_the_fork_only() {
    let env = Replay::new(site_log());
    let mut sb = Sandbox::new(
        &env,
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut tab = sb
        .open("https://scroll.test/feed", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let before = tab.observe_full().unwrap_or_else(|e| panic!("{e}"));
    let la = tab
        .lookahead(vec![
            "scroll down".parse().unwrap_or_else(|e| panic!("{e}")),
        ])
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(la.to_string().contains("scrolled to 0,"), "{la}");
    let after = tab.observe_full().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        before.lines().nth(1),
        after.lines().nth(1),
        "the real page did not scroll"
    );
}

#[test]
#[ignore = "timing: cargo test --release -p pageon --test scrolling -- --ignored --nocapture"]
#[allow(clippy::disallowed_methods)] // a wall-clock measurement
fn scrolling_is_cheap() {
    let rows: String = (0..200)
        .map(|i| format!("<p style='height:25px;margin:0'>Row {i} <a href='/r{i}'>open</a></p>"))
        .collect();
    let env = Replay::new(site(vec![html(
        "https://long.test/",
        &format!("<body style='margin:0'>{rows}</body>"),
    )]));
    let session = Session::new(&env);
    let mut tab = session
        .open("https://long.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let _ = tab.observe();
    let (n, mut total) = (6, std::time::Duration::ZERO);
    for i in 0..n {
        let a = if i % 2 == 0 {
            "scroll down"
        } else {
            "scroll up"
        };
        let t = std::time::Instant::now();
        let out = tab
            .act(a.parse().unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
        total += t.elapsed();
        assert!(out.to_string().contains("scrolled to"), "{out}");
    }
    let per = total.as_secs_f64() * 1000.0 / f64::from(n);
    println!("scroll: {per:.2} ms per action (5,000-px page)");
    assert!(per <= 2.0, "{per} ms");
}
