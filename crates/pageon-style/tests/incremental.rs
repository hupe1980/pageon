//! Styles brought up to date from the document's change log equal a fresh styling, after
//! random changes to every geometry fixture.

use pageon_dom::{Dom, Edge, NodeData, NodeId, ParseOptions, parse_document};
use pageon_style::Styler;

const PROPERTIES: &[&str] = &[
    "display",
    "color",
    "font-weight",
    "visibility",
    "position",
    "background-color",
    "opacity",
    "float",
    "text-align",
    "z-index",
];

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

fn elements(dom: &Dom) -> Vec<NodeId> {
    let roots: Vec<NodeId> = std::iter::once(dom.root())
        .chain(dom.shadow_hosts().map(|(_, r)| r))
        .collect();
    roots
        .into_iter()
        .flat_map(|r| dom.traverse(r).collect::<Vec<_>>())
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

fn mutate(dom: &mut Dom, rng: &mut Rng, classes: &[String]) -> String {
    let skip = ["html", "head", "body", "style"];
    let els: Vec<NodeId> = elements(dom)
        .into_iter()
        .filter(|&id| {
            !skip
                .iter()
                .any(|t| dom.get(id).is_some_and(|n| n.is_html(t)))
        })
        .collect();
    if els.is_empty() {
        return String::new();
    }
    let el = els[rng.below(els.len())];
    let kind = rng.below(9);
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
                "color:red",
                "font-weight:700",
                "position:absolute",
                "opacity:.5",
            ];
            let _ = dom.set_attr(el, "style", v[rng.below(v.len())]);
        }
        2 => {
            let _ = dom.remove_attr(el, "class");
            let _ = dom.remove_attr(el, "style");
        }
        3 => {
            let _ = dom.set_attr(el, "id", ["main", "x", "hero"][rng.below(3)]);
        }
        4 => {
            let _ = dom.remove(el);
        }
        5 => {
            let to = els[rng.below(els.len())];
            if !std::iter::successors(Some(to), |&n| dom.parent(n)).any(|n| n == el) {
                let _ = dom.detach(el);
                let _ = dom.append(to, el);
            }
        }
        6 => {
            if let Some(p) = dom.parent(el)
                && let Some(first) = dom.children(p).next()
                && first != el
            {
                let _ = dom.detach(el);
                let _ = dom.insert_before(p, el, first);
            }
        }
        7 => {
            dom.control_mut(el).checked = Some(rng.below(2) == 0);
        }
        _ => {
            let open = dom.popover_open(el);
            dom.set_popover_open(el, !open);
        }
    }
    format!("change {kind}")
}

#[test]
fn styles_from_the_change_log_equal_a_fresh_styling() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pageon-layout/tests/geometry");
    let mut pages: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{e}"))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "html"))
        .collect();
    pages.sort();
    let extra = "<style>li:first-child{color:red} li+li{font-weight:700} input:checked+label{color:blue} \
        p:empty{display:none} .a{color:green} #main{opacity:.5} [popover]:popover-open{display:block}</style>\
        <ul><li>a<li>b<li>c</ul><input type=checkbox><label>x</label><p></p><div class=a popover>p</div>";
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
    sources.push(("selectors".into(), extra.into()));
    sources.push((
        "shadow".into(),
        "<style>my-card{--c:green} my-card::part(t){font-weight:700}</style>\
         <my-card class=a><template shadowrootmode=open><style>:host{display:block} \
         ::slotted(p){color:red} .x{color:var(--c)} b{float:left}</style><b class=x part=t>s</b>\
         <slot></slot><i>in</i></template><p>slotted</p><span slot=none>lost</span></my-card>"
            .into(),
    ));
    for (page, html) in sources {
        let mut dom = parse_document(&html, ParseOptions::default());
        let css = sheets(&dom);
        let css: Vec<&str> = css.iter().map(String::as_str).collect();
        let classes: Vec<String> = elements(&dom)
            .into_iter()
            .filter_map(|id| {
                dom.get(id)
                    .and_then(|n| n.attr("class"))
                    .map(str::to_string)
            })
            .chain(["a".into()])
            .collect();
        let mut kept = Styler::new(1280.0, 720.0, "about:blank");
        kept.set_sheets(
            &css.iter()
                .map(|s| (pageon_style::text_hash(s), *s))
                .collect::<Vec<_>>(),
        );
        let first = dom.take_style_changes();
        kept.sync(&dom, &first, pageon_style::Page::default());
        let mut rng = Rng(page.len() as u64 + 7);
        for step in 0..40 {
            let what = mutate(&mut dom, &mut rng, &classes);
            let changes = dom.take_style_changes();
            kept.sync(&dom, &changes, pageon_style::Page::default());
            let mut fresh = Styler::new(1280.0, 720.0, "about:blank");
            fresh.set_sheets(
                &css.iter()
                    .map(|s| (pageon_style::text_hash(s), *s))
                    .collect::<Vec<_>>(),
            );
            fresh.restyle_all(&dom);
            for id in elements(&dom) {
                for p in PROPERTIES {
                    let a = fresh.value(&dom, id, p);
                    let b = kept.value(&dom, id, p);
                    assert_eq!(
                        a,
                        b,
                        "{page} step {step} ({what}): {p} of {id:?} <{}>",
                        dom.get(id).and_then(|n| n.local_name()).unwrap_or_default()
                    );
                }
            }
        }
    }
}
