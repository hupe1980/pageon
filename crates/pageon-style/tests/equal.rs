//! Styles computed on pageon's tree equal those the layout engine computes on its copy, on
//! the geometry fixtures.

use pageon_dom::{Dom, Edge, NodeData, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, Viewport};
use pageon_style::Styler;

const PROPERTIES: &[&str] = &[
    "display",
    "color",
    "font-size",
    "font-weight",
    "font-style",
    "visibility",
    "position",
    "background-color",
    "text-align",
    "opacity",
    "float",
    "z-index",
    "cursor",
    "white-space-collapse",
    "flex-direction",
    "list-style-type",
    "overflow-x",
];

fn elements(dom: &Dom) -> Vec<NodeId> {
    dom.traverse(dom.root())
        .filter_map(|e| match e {
            Edge::Enter(id) => {
                matches!(dom.get(id).map(|n| &n.data), Some(NodeData::Element { .. })).then_some(id)
            }
            Edge::Leave(_) => None,
        })
        .collect()
}

fn sheets(dom: &Dom) -> Vec<String> {
    elements(dom)
        .into_iter()
        .filter(|&id| dom.get(id).is_some_and(|n| n.is_html("style")))
        .map(|id| {
            dom.children(id)
                .filter_map(|c| match dom.get(c).map(|n| &n.data) {
                    Some(NodeData::Text(t)) => Some(t.clone()),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

#[test]
fn styles_equal_the_layout_engines() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pageon-layout/tests/geometry");
    let mut pages: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    pages.sort();
    let mut diffs = Vec::new();
    let mut compared = 0;
    for page in pages {
        let html = std::fs::read_to_string(&page).unwrap_or_else(|e| panic!("{e}"));
        let dom = parse_document(&html, ParseOptions::default());
        let layout = Layout::new(&dom, Viewport::default(), &[], None);
        let mut styler = Styler::new(1280.0, 720.0, "about:blank");
        let css = sheets(&dom);
        styler.set_sheets(&css.iter().map(|s| (pageon_style::text_hash(s), s.as_str())).collect::<Vec<_>>());
        styler.restyle_all(&dom);
        let name = page
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        for id in elements(&dom) {
            for p in PROPERTIES {
                let theirs = layout.style(id, p);
                let Some(theirs) = theirs else { continue };
                compared += 1;
                let ours = styler.value(&dom, id, p).unwrap_or_default();
                if ours != theirs {
                    let tag = dom
                        .get(id)
                        .and_then(|n| n.local_name())
                        .unwrap_or_default()
                        .to_string();
                    diffs.push(format!(
                        "{name} <{tag}> {p}: ours {ours:?}, layout {theirs:?}"
                    ));
                }
            }
        }
    }
    assert!(compared > 1000, "{compared}");
    assert!(
        diffs.is_empty(),
        "{} of {compared} differ:\n{}",
        diffs.len(),
        diffs[..diffs.len().min(40)].join("\n")
    );
}

/// Font-relative units resolve against the bundled fonts, as in layout.
#[test]
fn font_relative_units_match_layout() {
    let html = "<style>p{letter-spacing:1ch;word-spacing:1ex;text-indent:3cap;outline-offset:2ic}\
        code{font-family:monospace;letter-spacing:.5ch} h1{font-family:serif;word-spacing:2ex}</style>\
        <p>Text <code>x</code></p><h1>Title</h1>";
    let dom = parse_document(html, ParseOptions::default());
    let layout = Layout::new(&dom, Viewport::default(), &[], None);
    let mut styler =
        Styler::with_metrics(1280.0, 720.0, "about:blank", pageon_layout::font_metrics());
    let css = sheets(&dom);
    styler.set_sheets(&css.iter().map(|s| (pageon_style::text_hash(s), s.as_str())).collect::<Vec<_>>());
    styler.restyle_all(&dom);
    let mut compared = 0;
    for id in elements(&dom) {
        for p in ["letter-spacing", "word-spacing", "text-indent", "outline-offset"] {
            let Some(theirs) = layout.style(id, p) else { continue };
            compared += 1;
            assert_eq!(styler.value(&dom, id, p).as_deref(), Some(theirs.as_str()), "{p}");
        }
    }
    assert!(compared >= 8, "{compared}");
}
