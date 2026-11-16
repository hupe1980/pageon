//! Popovers: hidden until open, then laid out above the page.

use pageon_dom::{Edge, ParseOptions, parse_document};
use pageon_layout::{Layout, Viewport};

#[test]
fn open_popovers_render_and_closed_ones_do_not() {
    let mut dom = parse_document(
        r#"<body style="margin:0"><p>page</p><div id=menu popover>Menu</div></body>"#,
        ParseOptions::default(),
    );
    let menu = dom
        .traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some("menu") => Some(n),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no menu"));
    let vp = Viewport {
        width: 800,
        height: 600,
    };
    let closed = Layout::new(&dom, vp, &[], None);
    assert!(
        closed.box_of(menu).is_none_or(|r| r.width == 0.0),
        "a closed popover rendered"
    );
    dom.set_popover_open(menu, true);
    let open = Layout::new(&dom, vp, &[], None);
    let r = open
        .box_of(menu)
        .unwrap_or_else(|| panic!("an open popover did not render"));
    assert!(r.width > 0.0 && r.height > 0.0, "{r:?}");
    // Centred in the viewport.
    assert!((r.x + r.width / 2.0 - 400.0).abs() < 2.0, "{r:?}");
}
