//! Visibility, viewport, cover and hit-testing verdicts on constructed pages.

use pageon_dom::{Dom, Edge, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, Viewport, Visibility};

fn find(dom: &Dom, id: &str) -> NodeId {
    dom.traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some(id) => Some(n),
            _ => None,
        })
        .unwrap_or_else(|| panic!("#{id} missing"))
}

fn page(html: &str) -> (Dom, Layout) {
    let dom = parse_document(html, ParseOptions::default());
    let layout = Layout::new(&dom, Viewport::default(), &[], None);
    assert!(layout.error().is_none(), "{:?}", layout.error());
    (dom, layout)
}

#[test]
fn modal_overlay_covers_content() {
    let (dom, l) = page(
        r#"<button id="behind">Subscribe</button>
        <div id="overlay" style="position:fixed;inset:0;background:rgba(0,0,0,.5)">
          <div id="dialog" style="margin:100px auto;width:300px;background:#fff">Cookies <button id="ok">Accept</button></div>
        </div>"#,
    );
    let by = l
        .covered_by(find(&dom, "behind"))
        .unwrap_or_else(|| panic!("not covered"));
    assert_eq!(dom.get(by).and_then(|n| n.attr("id")), Some("overlay"));
    assert_eq!(
        l.covered_by(find(&dom, "ok")),
        None,
        "the dialog's own button is on top"
    );
}

#[test]
fn hidden_states() {
    let (dom, l) = page(
        r#"<input id="op" style="opacity:0"><input id="zero" style="width:0;height:0;border:0;padding:0">
        <p id="none" style="display:none">x <span id="inside">y</span></p><p id="vh" style="visibility:hidden">z</p>
        <div style="opacity:0"><button id="ancestor">a</button></div><p id="ok">visible</p>"#,
    );
    assert_eq!(l.visibility(find(&dom, "op")), Visibility::Hidden);
    assert_eq!(l.visibility(find(&dom, "zero")), Visibility::Hidden);
    assert_eq!(l.visibility(find(&dom, "none")), Visibility::NotRendered);
    assert_eq!(l.visibility(find(&dom, "inside")), Visibility::NotRendered);
    assert_eq!(l.visibility(find(&dom, "vh")), Visibility::Hidden);
    assert_eq!(l.visibility(find(&dom, "ancestor")), Visibility::Hidden);
    assert_eq!(l.visibility(find(&dom, "ok")), Visibility::Visible);
}

#[test]
fn labelled_custom_checkbox_is_not_covered() {
    let (dom, l) = page(
        r#"<label><input id="cb" type="checkbox" style="position:absolute;opacity:0">
        <span style="display:inline-block;padding:4px;border:1px solid">I accept</span></label>"#,
    );
    assert_eq!(l.covered_by(find(&dom, "cb")), None);
}

#[test]
fn hit_testing_and_viewport() {
    let (dom, l) = page(
        r#"<a id="link" href="/x" style="display:block;width:200px;height:40px">Go</a><div style="height:3000px"></div><p id="far">far</p>"#,
    );
    let link = find(&dom, "link");
    let b = l.box_of(link).unwrap_or_else(|| panic!("no box"));
    assert_eq!(l.element_at(b.x + 5.0, b.y + 5.0), Some(link));
    assert!(l.in_viewport(link));
    assert!(!l.in_viewport(find(&dom, "far")));
    assert_eq!(l.style(link, "display").as_deref(), Some("block"));
}

#[test]
fn stylesheets_apply_and_layout_is_deterministic() {
    let dom = parse_document(
        r#"<link id="css" rel="stylesheet" href="s.css"><p id="p">x</p>"#,
        ParseOptions::default(),
    );
    let sheet = pageon_layout::Stylesheet::new(find(&dom, "css"), "#p { margin-left: 77px }");
    let a = Layout::new(
        &dom,
        Viewport::default(),
        std::slice::from_ref(&sheet),
        Some("https://a.test/"),
    );
    assert_eq!(a.box_of(find(&dom, "p")).map(|r| r.x), Some(85.0));
    for _ in 0..20 {
        let b = Layout::new(
            &dom,
            Viewport::default(),
            std::slice::from_ref(&sheet),
            Some("https://a.test/"),
        );
        assert_eq!(a.box_of(find(&dom, "p")), b.box_of(find(&dom, "p")));
    }
}
