//! Paint and hit tests on hand-built trees (the layout pass does not run: locations, sizes and
//! borders are set directly on each box's layout).

use std::collections::HashMap;

use anyrender::ImageRenderer as _;
use html5ever::LocalName;
use pageon_dom::{Dom, NodeId, ParseOptions, parse_document};

use super::super::tree::{BoxId, Kind, Node, Tree};

const CSS: &str = "html, body { margin: 0 }
    #a { background: rgb(255, 0, 0); border: 10px solid rgb(0, 0, 255) }
    #b { position: relative; z-index: 1; background: rgb(0, 128, 0) }";

/// The element with `id`.
fn by_id(dom: &Dom, at: NodeId, id: &str) -> Option<NodeId> {
    if dom.get(at).and_then(|n| n.attr("id")) == Some(id) {
        return Some(at);
    }
    dom.children(at).find_map(|c| by_id(dom, c, id))
}

/// `html > body > (#b, #a)`, laid out by hand in a 100×100 viewport: `#a` at (10, 10),
/// 50×50 with a 10px border; `#b` (z-index 1, first in tree order) at (30, 30), 20×20.
fn tree() -> (Tree, [BoxId; 4]) {
    let html = r#"<html><body><div id="b"></div><div id="a"></div></body></html>"#;
    let dom = parse_document(html, ParseOptions::default());
    let mut styler = pageon_style::Styler::new(100.0, 100.0, "about:blank");
    styler.set_sheets(&[(pageon_style::text_hash(CSS), CSS)]);
    styler.restyle_all(&dom);

    let root = dom.root();
    let html_id = dom
        .children(root)
        .find(|&c| dom.get(c).and_then(|n| n.local_name()) == Some("html"));
    let html_id = html_id.unwrap_or_else(|| panic!("no html"));
    let body_id = dom
        .children(html_id)
        .find(|&c| dom.get(c).and_then(|n| n.local_name()) == Some("body"))
        .unwrap_or_else(|| panic!("no body"));
    let a_id = by_id(&dom, root, "a").unwrap_or_else(|| panic!("no #a"));
    let b_id = by_id(&dom, root, "b").unwrap_or_else(|| panic!("no #b"));

    let mut nodes = vec![Node::new(0, Kind::Document, Some(root))];
    let mut push = |dom_id: NodeId, tag: &str, parent: BoxId| -> BoxId {
        let id = nodes.len();
        let mut n = Node::new(id, Kind::Element, Some(dom_id));
        n.tag = LocalName::from(tag);
        n.style = styler.style(&dom, dom_id);
        n.parent = Some(parent);
        n.layout_parent.set(Some(parent));
        nodes.push(n);
        nodes[parent].children.push(id);
        id
    };
    let h = push(html_id, "html", 0);
    let body = push(body_id, "body", h);
    let b = push(b_id, "div", body);
    let a = push(a_id, "div", body);

    let set = |n: &mut Node, x: f32, y: f32, w: f32, hgt: f32, border: f32| {
        let l = &mut n.layout.layout;
        l.location = taffy::Point { x, y };
        l.size = taffy::Size {
            width: w,
            height: hgt,
        };
        l.border = taffy::Rect {
            left: border,
            right: border,
            top: border,
            bottom: border,
        };
        n.layout.unrounded = *l;
    };
    set(&mut nodes[h], 0.0, 0.0, 100.0, 100.0, 0.0);
    set(&mut nodes[body], 0.0, 0.0, 100.0, 100.0, 0.0);
    set(&mut nodes[b], 30.0, 30.0, 20.0, 20.0, 0.0);
    set(&mut nodes[a], 10.0, 10.0, 50.0, 50.0, 10.0);
    *nodes[0].layout_children.borrow_mut() = Some(vec![h]);
    *nodes[h].layout_children.borrow_mut() = Some(vec![body]);
    *nodes[body].layout_children.borrow_mut() = Some(vec![b, a]);
    *nodes[b].layout_children.borrow_mut() = Some(vec![]);
    *nodes[a].layout_children.borrow_mut() = Some(vec![]);

    let mut by_dom = HashMap::new();
    for n in &nodes {
        if let Some(d) = n.dom {
            by_dom.insert(d, n.id);
        }
    }
    let mut t = Tree {
        nodes,
        by_dom,
        images: None,
        wanted: Vec::new(),
        applied: 0,
        base: None,
        viewport: crate::Viewport {
            width: 100,
            height: 100,
        },
    };
    super::prepare(&mut t);
    (t, [h, body, a, b])
}

fn paint(tree: &Tree) -> Vec<u8> {
    let mut renderer = anyrender_vello_cpu::VelloCpuImageRenderer::new(100, 100);
    let mut data = Vec::new();
    renderer.render_to_vec(
        |scene| super::paint_scene(scene, tree, 1.0, 100, 100, 0, 0),
        &mut data,
    );
    data
}

fn pixel(data: &[u8], x: usize, y: usize) -> [u8; 4] {
    let i = (y * 100 + x) * 4;
    [data[i], data[i + 1], data[i + 2], data[i + 3]]
}

#[test]
fn paints_background_border_and_z_order() {
    let (tree, [_, _, a, b]) = tree();
    // The z-indexed box is hoisted to the root's stacking context.
    let sc = tree[tree_root(&tree)].stacking_context.as_ref();
    assert_eq!(sc.map(|s| s.children.len()), Some(1));
    assert_eq!(sc.map(|s| s.children[0].node_id), Some(b));
    assert_eq!(*tree[a].paint_children.borrow(), Some(vec![]));

    let data = paint(&tree);
    // Outside every box: nothing painted.
    assert_eq!(pixel(&data, 5, 5)[3], 0);
    assert_eq!(pixel(&data, 80, 80)[3], 0);
    // #a's border (blue) and background (red).
    assert_eq!(pixel(&data, 15, 15), [0, 0, 255, 255]);
    assert_eq!(pixel(&data, 25, 25), [255, 0, 0, 255]);
    assert_eq!(pixel(&data, 55, 35), [0, 0, 255, 255]);
    // #b (z-index 1) paints over #a although it comes first in tree order.
    assert_eq!(pixel(&data, 40, 40), [0, 128, 0, 255]);
}

#[test]
fn hits_the_topmost_box() {
    let (tree, [h, body, a, b]) = tree();
    assert_eq!(super::hit::hit(&tree, 40.0, 40.0), Some(b));
    assert_eq!(super::hit::hit(&tree, 25.0, 25.0), Some(a));
    assert_eq!(super::hit::hit(&tree, 15.0, 15.0), Some(a));
    assert_eq!(super::hit::hit(&tree, 80.0, 80.0), Some(body));
    assert_eq!(super::hit::hit(&tree, 150.0, 50.0), None);
    assert_ne!(h, body);
}

fn tree_root(tree: &Tree) -> BoxId {
    super::root_element(tree).unwrap_or_else(|| panic!("no root element"))
}
