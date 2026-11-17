//! Styles on pageon's tree with shadow trees: scoped sheets, `:host`, `::slotted`, `::part`,
//! custom properties through hosts, unassigned children not styled; equal to the layout
//! engine's (which styles the composed tree).

use pageon_dom::{Dom, Edge, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, Viewport};
use pageon_style::{Page, Styler};

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

fn document_sheets(dom: &Dom) -> Vec<String> {
    dom.traverse(dom.root())
        .filter_map(|e| match e {
            Edge::Enter(x) if dom.get(x).is_some_and(|n| n.is_html("style")) => Some(
                dom.children(x)
                    .filter_map(|c| match dom.get(c).map(|n| &n.data) {
                        Some(pageon_dom::NodeData::Text(t)) => Some(t.clone()),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        })
        .collect()
}

#[test]
fn shadow_trees_style_as_in_layout() {
    let mut dom = parse_document(PAGE, ParseOptions::default());
    let layout = Layout::new(&dom, Viewport::default(), &[], None);
    let mut styler = Styler::with_metrics(1280.0, 720.0, "about:blank", pageon_layout::font_metrics());
    let css = document_sheets(&dom);
    styler.set_sheets(&css.iter().map(|s| (pageon_style::text_hash(s), s.as_str())).collect::<Vec<_>>());
    let changes = dom.take_style_changes();
    styler.sync(&dom, &changes, Page::default());
    for n in ["card", "inner", "tint", "label", "title", "body", "outer"] {
        for p in ["display", "color", "font-weight", "font-style"] {
            let theirs = layout.style(id(&dom, n), p);
            assert_eq!(styler.value(&dom, id(&dom, n), p), theirs, "#{n} {p}");
        }
    }
    assert_eq!(styler.value(&dom, id(&dom, "lost"), "display"), None, "unassigned: not styled");
    // A change inside the shadow tree is synced.
    let inner = id(&dom, "inner");
    let _ = dom.set_attr(inner, "style", "color: rgb(1, 2, 3)");
    let changes = dom.take_style_changes();
    styler.sync(&dom, &changes, Page::default());
    assert_eq!(styler.value(&dom, inner, "color").as_deref(), Some("rgb(1, 2, 3)"));
}
