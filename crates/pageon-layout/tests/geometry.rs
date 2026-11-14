//! The geometry oracle: every rendered element's box against Chromium's (JS off, 1280×720),
//! matched by pre-order index of `<body>` descendants. Refresh with
//! `uv run pageon-bench geometry-oracle` in `bench/`.

use std::path::Path;

use pageon_dom::{Edge, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, Viewport, Visibility};

struct Rates {
    name: String,
    both: usize,
    within2: usize,
    within8: usize,
    worst: Vec<String>,
}

fn compare(path: &Path) -> Rates {
    let html = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{e}"));
    let oracle: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path.with_extension("boxes.json"))
            .unwrap_or_else(|e| panic!("{e}; run geometry-oracle")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let dom = parse_document(&html, ParseOptions::default());
    let layout = Layout::new(&dom, Viewport::default(), &[], None);
    let body = dom
        .traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(id) if dom.get(id).is_some_and(|n| n.is_html("body")) => Some(id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no body"));
    let ours: Vec<NodeId> = dom
        .traverse(body)
        .filter_map(|e| match e {
            Edge::Enter(id) if id != body && dom.get(id).and_then(|n| n.local_name()).is_some() => {
                Some(id)
            }
            _ => None,
        })
        .collect();
    let theirs = oracle["elements"].as_array().cloned().unwrap_or_default();
    let name = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    assert_eq!(
        ours.len(),
        theirs.len(),
        "{name}: element count differs from Chromium"
    );
    let mut r = Rates {
        name,
        both: 0,
        within2: 0,
        within8: 0,
        worst: Vec::new(),
    };
    for (id, c) in ours.iter().zip(&theirs) {
        let tag = dom
            .get(*id)
            .and_then(|n| n.local_name())
            .unwrap_or_default();
        assert_eq!(
            tag,
            c["tag"].as_str().unwrap_or_default(),
            "{}: element order differs",
            r.name
        );
        let rendered = layout.visibility(*id) != Visibility::NotRendered;
        if !(rendered && c["rendered"].as_bool().unwrap_or(false)) {
            continue;
        }
        let Some(b) = layout.box_of(*id) else {
            continue;
        };
        let f = |k: &str| c[k].as_f64().unwrap_or(0.0) as f32;
        let d = [
            (b.x - f("x")).abs(),
            (b.y - f("y")).abs(),
            (b.x + b.width - f("x") - f("w")).abs(),
            (b.y + b.height - f("y") - f("h")).abs(),
        ]
        .into_iter()
        .fold(0.0f32, f32::max);
        r.both += 1;
        r.within2 += usize::from(d <= 2.0);
        r.within8 += usize::from(d <= 8.0);
        if d > 8.0 && r.worst.len() < 3 {
            r.worst.push(format!(
                "{tag} ours {:.0},{:.0} {:.0}×{:.0} chromium {:.0},{:.0} {:.0}×{:.0}",
                b.x,
                b.y,
                b.width,
                b.height,
                f("x"),
                f("y"),
                f("w"),
                f("h")
            ));
        }
    }
    r
}

#[test]
fn boxes_match_chromium() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/geometry");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    files.sort();
    let (mut both, mut w2, mut w8) = (0, 0, 0);
    let mut macro8 = Vec::new();
    for f in &files {
        let r = compare(f);
        println!(
            "{:18} {:5} elements  ≤2px {:5.1}%  ≤8px {:5.1}%  {}",
            r.name,
            r.both,
            100.0 * r.within2 as f64 / r.both.max(1) as f64,
            100.0 * r.within8 as f64 / r.both.max(1) as f64,
            r.worst.join(" | ")
        );
        macro8.push(100.0 * r.within8 as f64 / r.both.max(1) as f64);
        both += r.both;
        w2 += r.within2;
        w8 += r.within8;
    }
    let (p2, p8) = (
        100.0 * w2 as f64 / both as f64,
        100.0 * w8 as f64 / both as f64,
    );
    let m8 = macro8.iter().sum::<f64>() / macro8.len() as f64;
    println!("TOTAL {both} elements  ≤2px {p2:.1}%  ≤8px {p8:.1}%  per-fixture mean ≤8px {m8:.1}%");
    // Targets: ≤2px ≥ 90 % (met), ≤8px ≥ 98 % (97.97 %: missed by 0.03 pt since `<select>` boxes
    // are compared, see below). The assertions are regression floors at the measured values.
    assert!(
        p2 >= 90.0 && p8 >= 97.9,
        "geometry regressed: ≤2px {p2:.1}% (≥90), ≤8px {p8:.2}% (floor 97.9, target 98)"
    );
    // The pooled rate is dominated by large pages; the per-fixture mean is the ratchet for gaps.
    // Target 80 %, measured 74.7 %: inline boxes take their line's height (labels beside form
    // controls) and lines holding controls are a few px taller, which accumulates down a form;
    // tables lack caption boxes; CJK has no bundled font. Since scripts run, `<select>` has a box,
    // so more elements are compared and the `forms` fixture's accumulated drift now shows. This
    // floor only stops regressions; raise it as gaps close.
    assert!(
        m8 >= 74.5,
        "per-fixture mean ≤8px {m8:.1}% regressed (floor 74.5, target 80)"
    );
}
