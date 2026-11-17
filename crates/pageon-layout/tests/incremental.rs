//! A layout laid out again in its previous tree (updated in place) computes what a fresh layout
//! computes: every element's box and style, after random changes to real-shaped pages.

use pageon_dom::{Dom, Edge, NodeData, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, LayoutOptions, Viewport};

/// A small deterministic generator (no dependency, same sequence on every run).
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

fn nodes(dom: &Dom, want: impl Fn(&NodeData) -> bool) -> Vec<NodeId> {
    dom.traverse(dom.root())
        .filter_map(|e| match e {
            Edge::Enter(id) => dom.get(id).filter(|n| want(&n.data)).map(|_| id),
            Edge::Leave(_) => None,
        })
        .collect()
}

fn elements(dom: &Dom) -> Vec<NodeId> {
    nodes(dom, |d| matches!(d, NodeData::Element { .. }))
}

fn is(dom: &Dom, id: NodeId, tag: &str) -> bool {
    dom.get(id).is_some_and(|n| n.is_html(tag))
}

/// One random change, as a page's script would make it.
fn mutate(dom: &mut Dom, rng: &mut Rng, classes: &[String]) -> String {
    let skip = ["html", "head", "body"];
    let els: Vec<NodeId> = elements(dom)
        .into_iter()
        .filter(|&id| !skip.iter().any(|t| is(dom, id, t)))
        .collect();
    if els.is_empty() {
        return String::new();
    }
    let el = els[rng.below(els.len())];
    let kind = rng.below(8);
    let tag = dom
        .get(el)
        .and_then(|n| n.local_name())
        .unwrap_or_default()
        .to_string();
    match kind {
        0 => {
            let c = classes
                .get(rng.below(classes.len()))
                .cloned()
                .unwrap_or_default();
            let _ = dom.set_attr(el, "class", &c);
        }
        1 => {
            let v = [
                "display:none",
                "width:37px",
                "padding:9px",
                "flex:1",
                "position:absolute",
            ];
            let _ = dom.set_attr(el, "style", v[rng.below(v.len())]);
        }
        2 => {
            let _ = dom.remove_attr(el, "class");
            let _ = dom.remove_attr(el, "style");
        }
        3 => {
            let texts = nodes(dom, |d| matches!(d, NodeData::Text(_)));
            if let Some(&t) = texts.get(rng.below(texts.len())) {
                let _ = dom.set_text(t, "changed text that is a little longer than before");
            }
        }
        4 => {
            let _ = dom.remove(el);
        }
        5 => {
            // Move an element under another (not into itself).
            let to = els[rng.below(els.len())];
            let inside = std::iter::successors(Some(to), |&n| dom.parent(n)).any(|n| n == el);
            if !inside {
                let _ = dom.detach(el);
                let _ = dom.append(to, el);
            }
        }
        6 => {
            let t = dom.create(NodeData::Text("new".into()));
            let _ = dom.append(el, t);
        }
        _ => {
            // Move an element before its parent's first child.
            if let Some(p) = dom.parent(el)
                && let Some(first) = dom.children(p).next()
                && first != el
            {
                let _ = dom.detach(el);
                let _ = dom.insert_before(p, el, first);
            }
        }
    }
    format!("change {kind} on <{tag}> {el:?}")
}

fn same(dom: &Dom, fresh: &Layout, again: &Layout, page: &str, step: &str) {
    for id in elements(dom) {
        assert_eq!(
            fresh.box_of(id),
            again.box_of(id),
            "{page} step {step}: box of {:?} <{}>",
            id,
            dom.get(id).and_then(|n| n.local_name()).unwrap_or_default()
        );
        for p in ["display", "color", "width"] {
            assert_eq!(
                fresh.style(id, p),
                again.style(id, p),
                "{page} step {step}: {p} of {id:?}"
            );
        }
    }
}

#[test]
fn incremental_layout_matches_a_fresh_one() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/geometry");
    let mut pages: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    pages.sort();
    // A page with an inline sheet whose text changes, and a shadow tree.
    let extra = r#"<style id=s>.a{color:red}.b{display:flex}</style>
        <div class=a><p>one</p><p class=b>two <span>three</span></p></div>
        <ul><li>x<li>y<li>z</ul><textarea>t</textarea><!-- c -->"#;
    let mut sources: Vec<(String, String)> = pages
        .iter()
        .map(|p| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            (
                name,
                std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{e}")),
            )
        })
        .collect();
    sources.push(("inline".into(), extra.into()));
    let viewport = Viewport::default();
    let opts = LayoutOptions::default();
    for (page, html) in sources {
        let mut dom = parse_document(&html, ParseOptions::default());
        let classes: Vec<String> = elements(&dom)
            .into_iter()
            .filter_map(|id| {
                dom.get(id)
                    .and_then(|n| n.attr("class"))
                    .map(str::to_string)
            })
            .chain(["a".into(), "b".into()])
            .collect();
        let mut rng = Rng(page.len() as u64);
        let mut prev = Some(Layout::with_options(&dom, viewport, &[], None, &opts));
        for step in 0..40 {
            let what = mutate(&mut dom, &mut rng, &classes);
            if step % 9 == 5
                && let Some(s) = nodes(&dom, |d| matches!(d, NodeData::Text(_)))
                    .into_iter()
                    .find(|&t| dom.parent(t).is_some_and(|p| is(&dom, p, "style")))
            {
                let _ = dom.set_text(s, ".a{color:blue}.b{display:block;width:50px}");
            }
            let again = Layout::again(&dom, viewport, &[], None, &opts, prev.take());
            let fresh = Layout::with_options(&dom, viewport, &[], None, &opts);
            same(&dom, &fresh, &again, &page, &format!("{step} ({what})"));
            prev = Some(again);
        }
    }
}
