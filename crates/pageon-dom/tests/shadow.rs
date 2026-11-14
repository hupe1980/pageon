//! Shadow trees: declarative shadow roots, slot assignment, shadow-including walks and the
//! composed tree.

use pageon_dom::{
    Dom, Edge, NodeData, NodeId, ParseOptions, ShadowInit, ShadowMode, parse_document,
    parse_fragment,
};

fn by_id(dom: &Dom, root: NodeId, id: &str) -> NodeId {
    dom.traverse(root)
        .find_map(|e| match e {
            Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some(id) => Some(n),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no #{id}"))
}

const CARD: &str = r#"<my-card id=card>
<template shadowrootmode=open shadowrootdelegatesfocus><header><slot name=title id=ts>Untitled</slot></header><slot id=def></slot><footer><slot name=foot id=fs>no footer</slot></footer></template>
<h2 slot=title id=title>Hi</h2>text<p id=body>Body</p><span slot=nowhere id=lost>lost</span></my-card>
<x-closed id=closed><template shadowrootmode=closed><b id=secret>inner</b></template></x-closed>"#;

#[test]
fn declarative_shadow_roots_attach_in_documents_only() {
    let dom = parse_document(CARD, ParseOptions::default());
    let card = by_id(&dom, dom.root(), "card");
    let root = dom
        .shadow_root(card)
        .unwrap_or_else(|| panic!("no shadow root"));
    assert!(matches!(
        dom.get(root).map(|n| &n.data),
        Some(NodeData::ShadowRoot {
            mode: ShadowMode::Open,
            delegates_focus: true,
            ..
        })
    ));
    assert_eq!(dom.host(root), Some(card));
    // The template is gone: its contents are the shadow tree.
    assert!(
        dom.children(card)
            .all(|c| !dom.get(c).is_some_and(|n| n.is_html("template")))
    );
    let closed = by_id(&dom, dom.root(), "closed");
    let croot = dom
        .shadow_root(closed)
        .unwrap_or_else(|| panic!("closed root"));
    assert!(matches!(
        dom.get(croot).map(|n| &n.data),
        Some(NodeData::ShadowRoot {
            mode: ShadowMode::Closed,
            ..
        })
    ));
    // Not in fragments (innerHTML).
    let frag = parse_fragment(CARD, "div", ParseOptions::default());
    assert!(!frag.has_shadow());
}

#[test]
fn slots_take_their_nodes() {
    let dom = parse_document(CARD, ParseOptions::default());
    let card = by_id(&dom, dom.root(), "card");
    let root = dom.shadow_root(card).unwrap_or_else(|| panic!("root"));
    let (ts, def, fs) = (
        by_id(&dom, root, "ts"),
        by_id(&dom, root, "def"),
        by_id(&dom, root, "fs"),
    );
    let (title, body, lost) = (
        by_id(&dom, card, "title"),
        by_id(&dom, card, "body"),
        by_id(&dom, card, "lost"),
    );
    assert_eq!(dom.assigned_nodes(ts, false), vec![title]);
    let d = dom.assigned_nodes(def, false);
    assert!(d.contains(&body) && d.len() >= 2, "text and <p>: {d:?}");
    assert!(dom.assigned_nodes(fs, false).is_empty());
    assert_eq!(dom.assigned_slot(title), Some(ts));
    assert_eq!(dom.assigned_slot(lost), None);
    // Flattened: an empty slot gives its fallback.
    let fallback = dom.assigned_nodes(fs, true);
    assert_eq!(fallback.len(), 1);
    assert_eq!(dom.text(fallback[0]), "no footer");
}

#[test]
fn shadow_including_walks() {
    let dom = parse_document(CARD, ParseOptions::default());
    let card = by_id(&dom, dom.root(), "card");
    let root = dom.shadow_root(card).unwrap_or_else(|| panic!("root"));
    let ts = by_id(&dom, root, "ts");
    let title = by_id(&dom, card, "title");
    assert!(dom.is_connected(ts));
    assert_eq!(dom.root_of(ts), root);
    assert_eq!(dom.shadow_including_root(ts), dom.root());
    assert_eq!(dom.flat_parent(title), Some(ts));
    assert!(dom.flat_ancestors(ts).any(|a| a == card));
    assert_eq!(dom.flat_parent(by_id(&dom, card, "lost")), None);
    // Detached hosts take their shadow trees with them.
    let mut dom = dom;
    let _ = dom.detach(card);
    assert!(!dom.is_connected(ts));
}

#[test]
fn the_composed_tree_is_what_renders() {
    let dom = parse_document(CARD, ParseOptions::default());
    let card = by_id(&dom, dom.root(), "card");
    let c = dom.composed();
    let flat = &c.dom;
    // Same ids, relinked: the host holds its shadow tree; slots hold what they render.
    assert_eq!(
        flat.text(card).split_whitespace().collect::<Vec<_>>(),
        ["Hi", "textBody", "no", "footer"]
    );
    assert!(!flat.text(card).contains("lost"));
    assert!(!flat.has_shadow());
    let title = by_id(&dom, card, "title");
    assert_eq!(c.slotted.get(&title), Some(&1));
    assert_eq!(c.host_of.get(&card), Some(&1));
    let closed = by_id(&dom, dom.root(), "closed");
    assert_eq!(c.host_of.get(&closed), Some(&2));
    assert!(
        flat.text(closed).contains("inner"),
        "closed roots render too"
    );
    // Pages without shadow trees compose to themselves.
    let plain = parse_document("<p>x</p>", ParseOptions::default());
    assert_eq!(
        plain.composed().dom.to_html(plain.root()),
        plain.to_html(plain.root())
    );
}

#[test]
fn scripted_roots() {
    let mut dom = parse_document("<div id=a></div><input id=i>", ParseOptions::default());
    let a = by_id(&dom, dom.root(), "a");
    let i = by_id(&dom, dom.root(), "i");
    assert!(
        dom.attach_shadow(i, ShadowInit::open()).is_err(),
        "inputs cannot host"
    );
    let r = dom
        .attach_shadow(a, ShadowInit::open())
        .unwrap_or_else(|e| panic!("{e:?}"));
    assert!(
        dom.attach_shadow(a, ShadowInit::open()).is_err(),
        "only once"
    );
    let b = dom.create(NodeData::Text("inside".into()));
    let _ = dom.append(r, b);
    assert_eq!(dom.composed().dom.text(a), "inside");
    assert!(pageon_dom::is_custom_name("my-card") && !pageon_dom::is_custom_name("font-face"));
    assert!(!pageon_dom::is_custom_name("My-card") && !pageon_dom::is_custom_name("card"));
}
