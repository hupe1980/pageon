//! Layout with styles computed on the document itself (no sheets given to the layout engine)
//! equals layout with the engine's own styling, on every geometry fixture, before and after
//! random changes.

use pageon_dom::{Dom, Edge, NodeData, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, LayoutOptions, Viewport};
use pageon_style::{Page, Styler};

fn elements(dom: &Dom) -> Vec<NodeId> {
    dom.traverse(dom.root())
        .filter_map(|e| match e {
            Edge::Enter(id) => matches!(dom.get(id).map(|n| &n.data), Some(NodeData::Element { .. }))
                .then_some(id),
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

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

fn same(dom: &Dom, ours: &Layout, theirs: &Layout, what: &str) {
    for id in elements(dom) {
        assert_eq!(
            ours.box_of(id),
            theirs.box_of(id),
            "{what}: box of {id:?} <{}>",
            dom.get(id).and_then(|n| n.local_name()).unwrap_or_default()
        );
        for p in ["display", "color", "font-size"] {
            assert_eq!(ours.style(id, p), theirs.style(id, p), "{what}: {p} of {id:?}");
        }
    }
}

#[test]
fn layout_with_native_styles_equals_the_engines_own() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/geometry");
    let mut pages: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    pages.sort();
    let vp = Viewport::default();
    for page in pages {
        let name = page.file_name().unwrap_or_default().to_string_lossy().to_string();
        let html = std::fs::read_to_string(&page).unwrap_or_else(|e| panic!("{e}"));
        let mut dom = parse_document(&html, ParseOptions::default());
        let css = sheets(&dom);
        let css: Vec<&str> = css.iter().map(String::as_str).collect();
        let mut styler =
            Styler::with_metrics(1280.0, 720.0, "about:blank", pageon_layout::font_metrics());
        styler.set_sheets(&css.iter().map(|s| (pageon_style::text_hash(s), *s)).collect::<Vec<_>>());
        let changes = dom.take_style_changes();
        styler.sync(&dom, &changes, Page::default());
        let mut prev: Option<Layout> = None;
        let mut rng = Rng(name.len() as u64);
        for step in 0..12 {
            let opts = LayoutOptions { styles: Some(&styler), ..LayoutOptions::default() };
            let ours = Layout::again(&dom, vp, &[], None, &opts, prev.take());
            let theirs = Layout::with_options(&dom, vp, &[], None, &LayoutOptions::default());
            same(&dom, &ours, &theirs, &format!("{name} step {step}"));
            prev = Some(ours);
            // A change, then the styles brought up to date.
            let els: Vec<NodeId> = elements(&dom)
                .into_iter()
                .filter(|&id| !["html", "head", "body", "style"].iter().any(|t| dom.get(id).is_some_and(|n| n.is_html(t))))
                .collect();
            if els.is_empty() {
                break;
            }
            let el = els[rng.below(els.len())];
            match rng.below(4) {
                0 => {
                    let _ = dom.set_attr(el, "style", ["display:none", "width:40px", "padding:7px"][rng.below(3)]);
                }
                1 => {
                    let _ = dom.remove(el);
                }
                2 => {
                    let to = els[rng.below(els.len())];
                    if !std::iter::successors(Some(to), |&n| dom.parent(n)).any(|n| n == el) {
                        let _ = dom.detach(el);
                        let _ = dom.append(to, el);
                    }
                }
                _ => {
                    let t = dom.create(NodeData::Text("more text here".into()));
                    let _ = dom.append(el, t);
                }
            }
            let changes = dom.take_style_changes();
            styler.sync(&dom, &changes, Page::default());
        }
    }
}
