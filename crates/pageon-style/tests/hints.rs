//! Legacy attributes that style elements (presentational hints), as browsers map them.

use pageon_dom::{Dom, Edge, NodeId, ParseOptions, parse_document};
use pageon_style::Styler;

fn by_id(dom: &Dom, name: &str) -> NodeId {
    dom.traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(x) if dom.get(x).and_then(|n| n.attr("id")) == Some(name) => Some(x),
            _ => None,
        })
        .unwrap_or_else(|| panic!("#{name}"))
}

#[test]
fn legacy_attributes_map_to_css() {
    let dom = parse_document(
        r##"<body marginwidth=3 topmargin=4>
        <svg id=s width=20 height="2em"></svg>
        <img id=i border=2 hspace=5 vspace=6 width=30>
        <input id=b type=image width=40 border=1>
        <iframe id=f width=50 hspace=9></iframe>
        <table id=t width=0 bgcolor="#f00"><tr><td id=c width=0 height=7 align=center>x</td></tr></table>
        <p id=h hidden>x</p>"##,
        ParseOptions::default(),
    );
    let mut s = Styler::new(1280.0, 720.0, "about:blank");
    s.restyle_all(&dom);
    let v = |id: &str, p: &str| s.value(&dom, by_id(&dom, id), p).unwrap_or_default();
    let body = dom
        .traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(x) if dom.get(x).is_some_and(|n| n.is_html("body")) => Some(x),
            _ => None,
        })
        .unwrap_or_else(|| panic!("body"));
    let b = |p: &str| s.value(&dom, body, p).unwrap_or_default();
    assert_eq!(
        (b("margin-left"), b("margin-right"), b("margin-top")),
        ("3px".into(), "3px".into(), "4px".into())
    );
    assert_eq!(v("s", "width"), "20px");
    assert_eq!(v("s", "height"), "32px", "SVG lengths resolve as CSS (2em)");
    assert_eq!(v("i", "border-top-width"), "2px");
    assert_eq!(v("i", "border-left-style"), "solid");
    assert_eq!(
        (v("i", "margin-left"), v("i", "margin-top")),
        ("5px".into(), "6px".into())
    );
    assert_eq!(v("i", "width"), "30px");
    assert_eq!(
        (v("b", "width"), v("b", "border-top-width")),
        ("40px".into(), "1px".into())
    );
    assert_eq!(v("f", "width"), "50px");
    assert_eq!(v("f", "margin-left"), "0px", "iframe takes no hspace");
    assert_eq!(v("t", "width"), "auto", "table width ignores zero");
    assert_eq!(v("t", "background-color"), "rgb(255, 0, 0)");
    assert_eq!(
        (v("c", "width"), v("c", "height")),
        ("auto".into(), "7px".into())
    );
    assert_eq!(v("c", "text-align"), "-moz-center");
    assert_eq!(v("h", "display"), "none");
}
