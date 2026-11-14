//! The public API and its dump goldens.

mod common;

use pageon::{Dump, NodeKind};

#[test]
fn walk_and_inspect() {
    let doc = pageon::parse(common::ARTICLE, "https://a.test/");
    let html = doc
        .children(doc.root())
        .find(|&c| doc.node(c).and_then(|n| n.local_name()) == Some("html"));
    let html = html.unwrap_or_else(|| panic!("no html"));
    let names: Vec<&str> = doc
        .children(html)
        .filter_map(|c| doc.node(c)?.local_name())
        .collect();
    assert_eq!(names, ["head", "body"]);
    assert_eq!(
        doc.node(doc.root()).map(|n| n.kind()),
        Some(NodeKind::Document)
    );
    assert_eq!(doc.mode(), pageon::QuirksMode::NoQuirks);
}

#[test]
fn removed_nodes_never_resolve() {
    let mut doc = pageon::parse(common::ARTICLE, "https://a.test/");
    let main = doc
        .dom()
        .traverse(doc.root())
        .find_map(|e| match e {
            pageon_dom::Edge::Enter(id)
                if doc.node(id).and_then(|n| n.local_name()) == Some("main") =>
            {
                Some(id)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no main"));
    doc.dom_mut().remove(main).unwrap_or_else(|e| panic!("{e}"));
    assert!(doc.node(main).is_none());
    // Reuse the freed storage: the old handle must still not resolve.
    for _ in 0..50 {
        doc.dom_mut()
            .create(pageon_dom::NodeData::Comment("x".into()));
    }
    assert!(doc.node(main).is_none());
}

#[test]
fn dump_goldens() {
    let deep = format!(
        "<!DOCTYPE html>{}deep{}",
        "<div>".repeat(5000),
        "</div>".repeat(5000)
    );
    let form = "<!DOCTYPE html><form action=\"/login\"><label>Email <input type=email name=email></label>\
<select name=c><option>DE<option selected>FR</select><button type=submit>Sign in</button><textarea>a\n  b</textarea></form>";
    for (name, html) in [
        ("article", common::ARTICLE.to_string()),
        ("form", form.to_string()),
        ("deep", deep),
    ] {
        let doc = pageon::parse(&html, "https://a.test/");
        let text = doc.dump(Dump::Text);
        let tree = doc.dump(Dump::Tree);
        if name == "deep" {
            assert_eq!(text, "deep");
            assert_eq!(tree.lines().count(), 5003);
            continue;
        }
        insta::assert_snapshot!(format!("{name}-text"), text);
        insta::assert_snapshot!(format!("{name}-tree"), tree);
    }
}
