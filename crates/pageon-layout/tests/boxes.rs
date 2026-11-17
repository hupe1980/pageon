//! Every element's border box on every geometry fixture, exactly as recorded in
//! `tests/geometry/boxes.golden` (first recorded from the layout engine pageon replaced). A
//! deliberate layout change re-records it: `PAGEON_BLESS=1 cargo test -p pageon-layout --test
//! boxes`, then review the diff.

use std::fmt::Write as _;

use pageon_dom::{Edge, ParseOptions, parse_document};
use pageon_layout::{Layout, Viewport};

fn snapshot() -> String {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/geometry");
    let mut pages: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    pages.sort();
    let mut out = String::new();
    for page in pages {
        let html = std::fs::read_to_string(&page).unwrap_or_else(|e| panic!("{e}"));
        let dom = parse_document(&html, ParseOptions::default());
        let layout = Layout::new(&dom, Viewport::default(), &[], None);
        let name = page.file_name().unwrap_or_default().to_string_lossy();
        let _ = writeln!(out, "# {name}");
        for e in dom.traverse(dom.root()) {
            let Edge::Enter(id) = e else { continue };
            let Some(tag) = dom.get(id).and_then(|n| n.local_name()) else {
                continue;
            };
            let b = layout.box_of(id).map_or_else(
                || "-".to_string(),
                |r| format!("{:.2} {:.2} {:.2} {:.2}", r.x, r.y, r.width, r.height),
            );
            let _ = writeln!(out, "{tag} {b}");
        }
    }
    out
}

#[test]
fn boxes_match_the_recorded_geometry() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/geometry/boxes.golden");
    let now = snapshot();
    if std::env::var_os("PAGEON_BLESS").is_some() {
        std::fs::write(&path, &now).unwrap_or_else(|e| panic!("{e}"));
        return;
    }
    let golden = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}; bless first"));
    let diff: Vec<String> = golden
        .lines()
        .zip(now.lines())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| format!("line {}: recorded `{a}`, now `{b}`", i + 1))
        .take(40)
        .collect();
    assert!(
        diff.is_empty() && golden.lines().count() == now.lines().count(),
        "{}",
        diff.join("\n")
    );
}
