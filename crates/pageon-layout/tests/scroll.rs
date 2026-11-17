//! Geometry under scroll: document and client boxes, fixed boxes, hit-testing, scroll
//! containers and their ranges, and painting a scrolled viewport.

use pageon_dom::{Dom, Edge, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, LayoutOptions, Rect, Viewport};

const PAGE: &str = r#"<html><body style="margin:0">
<div id=header style="position:fixed;top:0;left:0;width:100px;height:50px;background:red"></div>
<div id=bar style="position:fixed;bottom:0;left:0;width:1280px;height:100px;background:green"><span id=barText>cookies</span></div>
<div id=top style="height:3000px"></div>
<div id=target style="height:50px;width:200px;background:blue"></div>
<div id=box style="overflow:auto;height:100px;width:300px"><div style="height:500px"></div><p id=inner style="margin:0;height:20px">x</p></div>
<div id=hidden style="overflow:hidden;height:40px;width:300px"><div style="height:400px"></div></div>
<textarea id=area style="height:60px;width:150px;font-size:10px">LINES</textarea>
<div id=tail style="height:1000px"></div>
</body></html>"#;

fn dom() -> Dom {
    let lines = (0..60)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    parse_document(&PAGE.replace("LINES", &lines), ParseOptions::default())
}

fn id(dom: &Dom, name: &str) -> NodeId {
    dom.traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some(name) => Some(n),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no #{name}"))
}

fn layout(dom: &Dom, scroll: &[(NodeId, f64, f64)]) -> Layout {
    let opts = LayoutOptions {
        scroll,
        ..LayoutOptions::default()
    };
    Layout::with_options(dom, Viewport::default(), &[], None, &opts)
}

#[test]
fn boxes_follow_the_viewport_scroll() {
    let dom = dom();
    let l = layout(&dom, &[]);
    let target = id(&dom, "target");
    let header = id(&dom, "header");
    assert_eq!(l.viewport_scroll(), (0.0, 0.0));
    let (_, max_y) = l.viewport_range();
    assert!(max_y > 3000.0, "{max_y}");
    // Clamped to the range.
    assert_eq!(l.set_viewport_scroll(-5.0, 1e9), (0.0, max_y));
    assert_eq!(l.set_viewport_scroll(0.0, 2800.0), (0.0, 2800.0));
    // Document boxes stay; client boxes move; fixed boxes stay in the viewport.
    assert_eq!(l.box_of(target).map(|r| r.y), Some(3000.0));
    assert_eq!(l.client_rect(target).map(|r| r.y), Some(200.0));
    assert!(l.is_fixed(header) && !l.is_fixed(target));
    assert_eq!(l.box_of(header).map(|r| r.y), Some(2800.0));
    assert_eq!(l.client_rect(header).map(|r| r.y), Some(0.0));
    assert!(l.in_viewport(target) && l.in_viewport(header));
    assert!(!l.in_viewport(id(&dom, "box")) || l.client_rect(id(&dom, "box")).is_some());
}

#[test]
fn hit_testing_is_in_viewport_coordinates() {
    let dom = dom();
    let l = layout(&dom, &[]);
    let (header, bar, target) = (id(&dom, "header"), id(&dom, "bar"), id(&dom, "target"));
    l.set_viewport_scroll(0.0, 2800.0);
    assert_eq!(l.element_at(10.0, 10.0), Some(header));
    assert_eq!(l.element_at(150.0, 210.0), Some(target));
    assert_eq!(l.element_at(150.0, 5000.0), None, "outside the viewport");
    // The fixed bar at the bottom of the viewport, and — after a small scroll, where the hit
    // test would match the bar at its unscrolled place — the page beneath a point above it.
    assert!(
        l.element_at(500.0, 680.0)
            .is_some_and(|e| e == bar || dom.parent(e) == Some(bar))
    );
    l.set_viewport_scroll(0.0, 100.0);
    let above_bar = l.element_at(500.0, 560.0);
    assert!(
        above_bar.is_some_and(|e| e != bar && dom.parent(e) != Some(bar)),
        "{above_bar:?}"
    );
    assert!(
        l.element_at(500.0, 680.0)
            .is_some_and(|e| e == bar || dom.parent(e) == Some(bar))
    );
}

#[test]
fn scroll_containers_have_ranges_and_move_their_content() {
    let dom = dom();
    let (bx, inner, hidden, area) = (
        id(&dom, "box"),
        id(&dom, "inner"),
        id(&dom, "hidden"),
        id(&dom, "area"),
    );
    let l = layout(&dom, &[]);
    let (_, max) = l.scroll_range(bx).unwrap_or_else(|| panic!("#box scrolls"));
    assert!((max - 420.0).abs() <= 1.0, "{max}");
    assert!(l.scroll_range(hidden).is_some_and(|(_, y)| y > 300.0));
    assert!(l.scroll_range(id(&dom, "target")).is_none());
    let (_, area_max) = l
        .scroll_range(area)
        .unwrap_or_else(|| panic!("textarea scrolls"));
    assert!(
        area_max > 100.0,
        "the textarea's text overflows: {area_max}"
    );
    let before = l.box_of(inner).map(|r| r.y).unwrap_or_default();
    // Offsets are applied (clamped) when the layout is built.
    let l = layout(&dom, &[(bx, 0.0, 300.0), (hidden, 0.0, 1e6)]);
    assert_eq!(l.scroll_of(bx), (0.0, 300.0));
    assert!((l.scroll_of(hidden).1 - l.scroll_range(hidden).map_or(0.0, |r| r.1)).abs() < 0.5);
    let after = l.box_of(inner).map(|r| r.y).unwrap_or_default();
    assert!((before - after - 300.0).abs() < 0.5, "{before} → {after}");
    // The scroller itself stays where it is; only its content moves.
    let still = layout(&dom, &[]).box_of(bx);
    assert_eq!(l.box_of(bx), still);
}

const CLIPS: &str = r#"<html><body style="margin:0">
<div id=sr style="position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)"><p id=srText>hidden label</p></div>
<div id=cp style="clip-path:inset(50%)"><p id=cpText>clip-path</p></div>
<div style="height:0;overflow:hidden"><p id=zero>zero height</p></div>
<div id=scrolled style="height:40px;overflow:hidden"><p id=away style="margin:0;height:20px">away</p><div style="height:400px"></div><p id=end style="margin:0;height:20px">end</p></div>
<p id=ellipsis style="width:60px;overflow:hidden;white-space:nowrap;text-overflow:ellipsis">a long line that is cut with an ellipsis</p>
<div style="height:40px;overflow:auto"><div style="height:400px"></div><p id=scrollable>scroll to me</p></div>
<div style="height:0;overflow:hidden"><p id=fixed style="position:fixed;top:0">fixed</p></div>
<p id=plain>plain</p>
</body></html>"#;

#[test]
fn clipped_boxes_are_the_ones_a_person_cannot_see() {
    let dom = parse_document(CLIPS, ParseOptions::default());
    let scrolled = id(&dom, "scrolled");
    let l = layout(&dom, &[(scrolled, 0.0, 1e6)]);
    for name in ["srText", "sr", "cpText", "zero", "away"] {
        assert!(l.clipped(id(&dom, name)), "#{name} is clipped");
    }
    for name in [
        "end",
        "ellipsis",
        "scrollable",
        "fixed",
        "plain",
        "scrolled",
    ] {
        assert!(!l.clipped(id(&dom, name)), "#{name} is not clipped");
    }
}

#[test]
fn a_scrolled_viewport_paints_with_the_fixed_header() {
    let dom = dom();
    let mut l = layout(&dom, &[]);
    l.set_viewport_scroll(0.0, 2800.0);
    let (sx, sy) = l.viewport_scroll();
    let px = l
        .paint(
            Rect {
                x: sx,
                y: sy,
                width: 1280.0,
                height: 720.0,
            },
            1.0,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(px.pixel(10, 10), Some([255, 0, 0, 255]), "fixed header");
    assert_eq!(
        px.pixel(150, 210),
        Some([0, 0, 255, 255]),
        "scrolled content"
    );
    // Painting leaves the scroll as it was.
    assert_eq!(l.viewport_scroll(), (0.0, 2800.0));
}

#[test]
fn media_queries_match_as_css_does() {
    let v = Viewport::default();
    for (q, media, matches) in [
        ("(min-width: 600px)", "(min-width: 600px)", true),
        ("(max-width: 600px)", "(max-width: 600px)", false),
        (
            "screen and (orientation: landscape)",
            "screen and (orientation: landscape)",
            true,
        ),
        ("print", "print", false),
        (
            "(prefers-color-scheme: dark)",
            "(prefers-color-scheme: dark)",
            false,
        ),
        (
            "(prefers-reduced-motion: reduce)",
            "(prefers-reduced-motion: reduce)",
            false,
        ),
        (
            "(hover: hover) and (pointer: fine)",
            "(hover: hover) and (pointer: fine)",
            true,
        ),
        ("(min-width: foo)", "(min-width: foo)", false), // MQ4 general-enclosed (as Firefox)
        ("and and", "not all", false),
        ("", "", true),
    ] {
        assert_eq!(
            pageon_layout::media_query(v, q),
            (media.to_string(), matches),
            "{q}"
        );
    }
}
