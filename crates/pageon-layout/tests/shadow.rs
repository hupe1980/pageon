//! Shadow trees in layout: the composed tree renders, styles stay in their trees, `:host`,
//! `::slotted` and `::part` apply, custom properties inherit through hosts, and pages
//! without shadow trees lay out exactly as before.

use pageon_dom::{Dom, Edge, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, Viewport, Visibility};

fn id(dom: &Dom, name: &str) -> NodeId {
    let mut stack = vec![dom.root()];
    while let Some(n) = stack.pop() {
        for e in dom.traverse(n) {
            if let Edge::Enter(x) = e {
                if dom.get(x).and_then(|y| y.attr("id")) == Some(name) {
                    return x;
                }
                if let Some(r) = dom.shadow_root(x) {
                    stack.push(r);
                }
            }
        }
    }
    panic!("no #{name}")
}

const PAGE: &str = r#"<!doctype html><html><head><style>
  body { margin: 0 }
  button { color: rgb(0, 0, 255) }
  .card-title { color: rgb(0, 128, 0) }
  my-card { --accent: rgb(255, 0, 0) }
  my-card::part(label) { font-weight: 700 }
</style></head><body>
<my-card id=card>
  <template shadowrootmode=open>
    <style>
      :host { display: block; padding: 10px }
      button { color: rgb(255, 165, 0) }
      ::slotted(h2) { margin: 0; color: rgb(128, 0, 128) }
      .tint { color: var(--accent) }
      [part=label] { font-style: italic }
    </style>
    <button id=inner>Inside</button>
    <span id=tint class=tint>tinted</span>
    <span id=label part=label>Label</span>
    <slot name=title></slot>
    <slot></slot>
  </template>
  <h2 slot=title id=title class=card-title>Title</h2>
  <p id=body>Body text</p>
  <em slot=nowhere id=lost>lost</em>
</my-card>
<button id=outer>Outside</button>
</body></html>"#;

#[test]
fn the_composed_tree_renders_with_scoped_styles() {
    let dom = parse_document(PAGE, ParseOptions::default());
    let l = Layout::new(&dom, Viewport::default(), &[], None);
    let style = |n: &str, p: &str| l.style(id(&dom, n), p).unwrap_or_default();
    // Shadow content and slotted content are laid out; unassigned children are not.
    for n in ["inner", "tint", "label", "title", "body"] {
        assert_eq!(l.visibility(id(&dom, n)), Visibility::Visible, "#{n}");
    }
    assert_eq!(l.visibility(id(&dom, "lost")), Visibility::NotRendered);
    // No leaks either way.
    assert_eq!(
        style("inner", "color"),
        "rgb(255, 165, 0)",
        "shadow rule inside"
    );
    assert_eq!(
        style("outer", "color"),
        "rgb(0, 0, 255)",
        "document rule outside"
    );
    // :host and ::slotted apply (the slotted colour is the document's: its class rule wins, as
    // in browsers).
    assert_eq!(style("card", "display"), "block");
    assert_eq!(style("title", "margin-top"), "0px");
    // Custom properties inherit through the host into the shadow tree.
    assert_eq!(style("tint", "color"), "rgb(255, 0, 0)");
    // ::part reaches the exposed part from outside.
    assert_eq!(style("label", "font-weight"), "700");
    assert_eq!(style("label", "font-style"), "italic");
    // Order on screen: shadow content, then the slots' content.
    let y = |n: &str| l.box_of(id(&dom, n)).map_or(-1.0, |r| r.y);
    assert!(y("inner") < y("title") && y("title") < y("body"));
}

#[test]
fn pages_without_shadow_trees_are_unchanged() {
    let html = r#"<style>p{color:red} .x>b::before{content:"a"}</style><p class=x><b>b</b></p>"#;
    let dom = parse_document(html, ParseOptions::default());
    assert!(!dom.has_shadow());
    let l = Layout::new(&dom, Viewport::default(), &[], None);
    let p = dom
        .traverse(dom.root())
        .find_map(|e| match e {
            Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("p")) => Some(n),
            _ => None,
        })
        .unwrap_or_else(|| panic!("p"));
    assert_eq!(l.style(p, "color").as_deref(), Some("rgb(255, 0, 0)"));
}
