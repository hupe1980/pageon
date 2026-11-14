//! Native views against the Chromium-captured views of the same pages.
//!
//! Each static corpus page is loaded from a recording at the URL it was captured from, rendered
//! with pageon's own engine, and checked against the case expectations and, line by line (refs
//! normalised), against the view rendered from the Chromium capture.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use common::*;
use pageon::{Budget, Episode, LoadOptions, Mode, Replay, Viewport};
use pageon_agent::Capture;

/// Corpus pages compared with Chromium: the static ones and those built by script. `shadow-dom`
/// needs custom elements and Shadow DOM (a later spec).
const PAGES: [&str; 10] = [
    "checkout",
    "engine-vocabulary",
    "form-states",
    "hidden-fields",
    "i18n",
    "landmarks",
    "long-list",
    "nested-frames",
    "modal-cover",
    "xorigin-iframe",
];

fn corpus() -> PathBuf {
    root().join("crates/pageon-agent/tests/corpus")
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Case {
    budget: Option<usize>,
    must_contain: Vec<String>,
    must_not_contain: Vec<String>,
    forbidden_line_prefixes: Vec<String>,
    flagged: Vec<String>,
    first_line_prefix: Option<String>,
}

fn case(name: &str) -> Case {
    std::fs::read_to_string(corpus().join(format!("{name}.case.toml")))
        .map(|t| toml::from_str(&t).unwrap_or_else(|e| panic!("{name}.case.toml: {e}")))
        .unwrap_or_default()
}

fn native(name: &str, budget: Budget) -> String {
    let doc = pageon::load(
        &format!("{ORIGIN}{name}.html"),
        &Replay::new(fixtures()),
        corpus_options(),
    )
    .unwrap_or_else(|e| panic!("{name}: {e}"));
    doc.ax(Viewport::default(), budget)
}

/// Lines with refs replaced by `e#`, as a multiset.
fn lines(view: &str) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for l in view.lines() {
        let norm: Vec<String> = l
            .split(' ')
            .map(|w| {
                let bare = w.strip_prefix("covered-by:").unwrap_or(w);
                if bare.len() > 1
                    && bare.starts_with('e')
                    && bare[1..].chars().all(|c| c.is_ascii_digit())
                {
                    w.replace(bare, "e#")
                } else {
                    w.to_string()
                }
            })
            .collect();
        *m.entry(norm.join(" ")).or_default() += 1;
    }
    m
}

fn agreement(a: &str, b: &str) -> (usize, usize) {
    let (la, lb) = (lines(a), lines(b));
    let common: usize = la
        .iter()
        .map(|(k, n)| (*n).min(lb.get(k).copied().unwrap_or(0)))
        .sum();
    let total = la.values().sum::<usize>().max(lb.values().sum::<usize>());
    (common, total)
}

#[test]
fn native_views_match_the_chromium_views() {
    let (mut common, mut total) = (0, 0);
    for name in PAGES {
        let c = case(name);
        let budget = Budget::o200k(c.budget.unwrap_or(4000));
        let text = native(name, budget);
        for s in &c.must_contain {
            assert!(text.contains(s.as_str()), "{name}: missing {s:?}\n{text}");
        }
        for s in &c.must_not_contain {
            assert!(
                !text.contains(s.as_str()),
                "{name}: must not contain {s:?}\n{text}"
            );
        }
        for p in &c.forbidden_line_prefixes {
            assert!(
                !text.lines().any(|l| l.trim_start().starts_with(p.as_str())),
                "{name}: a line starts with {p:?}"
            );
        }
        for f in &c.flagged {
            assert!(
                text.lines()
                    .any(|l| l.contains("flagged") && l.contains(f.as_str())),
                "{name}: {f:?} not flagged\n{text}"
            );
        }
        if let Some(p) = &c.first_line_prefix {
            // The first line after the `@page` header.
            assert!(
                text.lines()
                    .nth(1)
                    .is_some_and(|l| l.starts_with(p.as_str())),
                "{name}: first line\n{text}"
            );
        }
        let capture = Capture::load(corpus().join(format!("{name}.capture.json.gz")))
            .unwrap_or_else(|e| panic!("{e}"));
        let chromium = pageon_agent::ax(&capture, budget);
        let (c, t) = agreement(&text, &chromium);
        println!(
            "{name:<20} {c:>4}/{t:<4} lines agree ({:.1} %)",
            100.0 * c as f64 / t.max(1) as f64
        );
        if c < t {
            let (ln, lc) = (lines(&text), lines(&chromium));
            for (l, n) in &ln {
                if lc.get(l).copied().unwrap_or(0) < *n {
                    println!("    native only:   {l}");
                }
            }
            for (l, n) in &lc {
                if ln.get(l).copied().unwrap_or(0) < *n {
                    println!("    chromium only: {l}");
                }
            }
        }
        common += c;
        total += t;
    }
    let pct = 100.0 * common as f64 / total as f64;
    println!("TOTAL {common}/{total} lines agree ({pct:.1} %)");
    assert!(pct >= 90.0, "line agreement {pct:.1} % (needs ≥ 90 %)");
}

#[test]
fn frames_are_stitched() {
    let text = native("nested-frames", Budget::o200k(4000));
    let want = [
        "== frame e1 http://a.localhost:47811\n  # \"Inner panel\"\n  button e2 \"Refresh\"\n  == frame e3 http://a.localhost:47811\n    button e4 \"Deep button\"\n  == /frame\n== /frame",
        "== frame e5 http://127.0.0.1:9 third-party state:failed",
    ];
    for w in want {
        assert!(text.contains(w), "missing\n{w}\nin\n{text}");
    }
}

#[test]
fn frame_limits_hold() {
    // Self-nesting and too many frames: loading ends, every frame is accounted for.
    let mut html = String::from("<iframe src=\"/self\"></iframe>");
    for _ in 0..20 {
        html.push_str("<iframe srcdoc=\"<iframe srcdoc='<iframe srcdoc=&quot;<iframe srcdoc=x></iframe>&quot;></iframe>'></iframe>\"></iframe>");
    }
    let mut l = log(vec![page(
        "https://a.test/self",
        200,
        &[("content-type", "text/html")],
        &html,
    )]);
    l.clock = (0..64).collect();
    let doc = pageon::load(
        "https://a.test/self",
        &Replay::new(l),
        LoadOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut loaded = 0;
    let mut reasons = Vec::new();
    let mut stack: Vec<&pageon::Document> = vec![&doc];
    while let Some(d) = stack.pop() {
        for f in d.frames() {
            match f.state() {
                pageon::FrameState::Loaded => loaded += 1,
                pageon::FrameState::Failed(r) => reasons.push(r.clone()),
            }
            stack.extend(f.document());
        }
    }
    assert_eq!(
        loaded,
        pageon::MAX_FRAMES - 1,
        "the self frame counts but fails"
    );
    assert!(
        reasons.iter().any(|r| r == "recursive frame"),
        "{reasons:?}"
    );
    assert!(reasons.iter().any(|r| r == "frame limit"), "{reasons:?}");
}

#[test]
fn third_party_frames_are_marked() {
    let mut l = log(vec![
        page(
            "https://a.test/",
            200,
            &[("content-type", "text/html")],
            "<h1>Shop</h1><iframe src=\"https://pay.test/w\" title=\"Pay\"></iframe>",
        ),
        page(
            "https://pay.test/w",
            200,
            &[("content-type", "text/html")],
            "<button>Pay now</button>",
        ),
    ]);
    l.clock = (0..16).collect();
    let doc = pageon::load("https://a.test/", &Replay::new(l), LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let text = doc.ax(Viewport::default(), Budget::o200k(4000));
    assert!(
        text.contains("== frame e1 https://pay.test third-party\n  button e2 \"Pay now\""),
        "{text}"
    );
}

#[test]
fn episodes_diff_native_observations() {
    let mut html = String::from("<main><h1>Cart</h1><p id=\"status\">2 items</p>");
    for i in 0..20 {
        html.push_str(&format!("<a href=\"/p/{i}\">Product {i}</a> "));
    }
    html.push_str("<button id=\"remove\">Remove</button><button>Checkout</button></main>");
    let mut doc = pageon::parse(&html, "https://a.test/");
    let mut ep = Episode::new(Budget::o200k(4000), Mode::Diff);
    let first = ep.observe(&doc.capture(Viewport::default()));
    let checkout = first
        .lines()
        .find(|l| l.contains("\"Checkout\""))
        .unwrap_or_default()
        .to_string();
    let target = ep
        .resolve("e22", &doc.capture(Viewport::default()))
        .unwrap_or_else(|e| panic!("{e:?}\n{first}"));
    let (frame, node) = doc
        .node_for(&target)
        .unwrap_or_else(|| panic!("target not found"));
    assert_eq!(
        (frame, doc.node(node).and_then(|n| n.local_name())),
        (0, Some("button"))
    );

    // The page changes in place (as script would change it): one button goes, a status changes.
    let find = |doc: &pageon::Document, id: &str| {
        doc.dom().traverse(doc.root()).find_map(|e| match e {
            pageon_dom::Edge::Enter(n) if doc.node(n).and_then(|x| x.attr("id")) == Some(id) => {
                Some(n)
            }
            _ => None,
        })
    };
    let remove = find(&doc, "remove").unwrap_or_else(|| panic!("no #remove"));
    let status = find(&doc, "status")
        .and_then(|s| doc.first_child(s))
        .unwrap_or_else(|| panic!("no status"));
    doc.dom_mut()
        .remove(remove)
        .unwrap_or_else(|e| panic!("{e}"));
    doc.dom_mut()
        .set_text(status, "1 item")
        .unwrap_or_else(|e| panic!("{e}"));
    let second = ep.observe(&doc.capture(Viewport::default()));
    assert!(!second.contains("full:fallback"), "{second}");
    assert!(second.contains("1 item"), "{second}");
    assert!(
        !second.contains("Product 3"),
        "unchanged lines are not repeated:\n{second}"
    );
    // The Checkout ref still resolves to the Checkout button after the change.
    let reference = checkout
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let after = ep
        .resolve(&reference, &doc.capture(Viewport::default()))
        .unwrap_or_else(|e| panic!("{e:?}"));
    let (_, node) = doc
        .node_for(&after)
        .unwrap_or_else(|| panic!("target not found"));
    assert_eq!(doc.dom().text(node), "Checkout");
}

#[test]
fn layout_failure_is_stated() {
    let doc = pageon::parse("<p>x</p>", "https://a.test/");
    let mut cap = doc.capture(Viewport::default());
    // A capture marked as built without geometry announces it in the header.
    cap.extras.insert("layout".into(), "unavailable".into());
    let text = pageon_agent::ax(&cap, Budget::o200k(4000));
    assert!(
        text.lines()
            .next()
            .is_some_and(|h| h.ends_with("layout:unavailable")),
        "{text}"
    );
}

#[test]
fn native_views_are_deterministic() {
    let first = native("checkout", Budget::o200k(4000));
    for _ in 0..100 {
        assert_eq!(native("checkout", Budget::o200k(4000)), first);
    }
}
