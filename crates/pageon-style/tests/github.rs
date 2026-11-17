//! Restyle cost on a large real page, pageon-style beside the layout engine's styling.
//! `PAGEON_PAGE_HTML=page.html PAGEON_PAGE_REC=page.rec.json cargo test --release -p
//! pageon-style --test github -- --ignored --nocapture`
// A timing harness measures wall time on purpose.
#![allow(clippy::disallowed_methods)]

use std::collections::HashMap;
use std::time::Instant;

use pageon_dom::{Dom, Edge, NodeData, NodeId, ParseOptions, parse_document};
use pageon_layout::{Layout, LayoutOptions, Stylesheet, Viewport};
use pageon_style::Styler;

fn elements(dom: &Dom) -> Vec<NodeId> {
    dom.traverse(dom.root())
        .filter_map(|e| match e {
            Edge::Enter(id) => {
                matches!(dom.get(id).map(|n| &n.data), Some(NodeData::Element { .. })).then_some(id)
            }
            Edge::Leave(_) => None,
        })
        .collect()
}

#[test]
#[ignore = "timing; needs a page and its recording"]
fn restyle_a_large_page() {
    let (Ok(html), Ok(rec)) = (
        std::env::var("PAGEON_PAGE_HTML"),
        std::env::var("PAGEON_PAGE_REC"),
    ) else {
        return;
    };
    let html = std::fs::read_to_string(html).unwrap_or_else(|e| panic!("{e}"));
    let rec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(rec).unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
    let base = rec["steps"][0]["input"]["open"]["url"]
        .as_str()
        .unwrap_or("about:blank")
        .to_string();
    let bodies: HashMap<String, String> = rec["exchanges"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some((
                e["url"].as_str()?.to_string(),
                e["body"].as_str()?.to_string(),
            ))
        })
        .collect();
    let mut dom = parse_document(&html, ParseOptions::default());
    let base_url = url::Url::parse(&base).unwrap_or_else(|e| panic!("{e}"));
    // The sheets in cascade order: style elements and linked sheets.
    let mut ordered: Vec<String> = Vec::new();
    let mut linked: Vec<Stylesheet> = Vec::new();
    for id in elements(&dom) {
        let Some(n) = dom.get(id) else { continue };
        if n.is_html("style") {
            ordered.push(
                dom.children(id)
                    .filter_map(|c| match dom.get(c).map(|n| &n.data) {
                        Some(NodeData::Text(t)) => Some(t.clone()),
                        _ => None,
                    })
                    .collect(),
            );
        } else if n.is_html("link")
            && n.attr("rel").is_some_and(|r| {
                r.split_ascii_whitespace()
                    .any(|w| w.eq_ignore_ascii_case("stylesheet"))
            })
            && let Some(css) = n
                .attr("href")
                .and_then(|h| base_url.join(h).ok())
                .and_then(|u| bodies.get(u.as_str()))
        {
            ordered.push(css.clone());
            linked.push(Stylesheet::new(id, css.as_str()));
        }
    }
    let count = elements(&dom).len();
    let css: usize = ordered.iter().map(String::len).sum();
    println!(
        "{count} elements, {} sheets, {css} bytes of CSS",
        ordered.len()
    );
    let vp = Viewport::default();
    let styles = LayoutOptions {
        styles_only: true,
        ..LayoutOptions::default()
    };

    let t = Instant::now();
    let first = Layout::with_options(&dom, vp, &linked, Some(&base), &styles);
    let blitz_first = t.elapsed();
    let mut native = Styler::new(1280.0, 720.0, &base);
    let t = Instant::now();
    native.set_sheets(&ordered.iter().map(String::as_str).collect::<Vec<_>>());
    let first_changes = dom.take_style_changes();
    native.sync(&dom, &first_changes, pageon_style::Page::default());
    let native_first = t.elapsed();

    // A full restyle with the sheets parsed: a class on <body> (Blitz restyles the parent's
    // subtree on any attribute change).
    let body = elements(&dom)
        .into_iter()
        .find(|&id| dom.get(id).is_some_and(|n| n.is_html("body")))
        .unwrap_or(dom.root());
    let mut blitz_again = Vec::new();
    let mut native_again = Vec::new();
    let mut prev = Some(first);
    for i in 0..5 {
        let _ = dom.set_attr(body, "data-x", &i.to_string());
        let t = Instant::now();
        let l = Layout::again(&dom, vp, &linked, Some(&base), &styles, prev.take());
        blitz_again.push(t.elapsed());
        prev = Some(l);
        let _ = dom.take_style_changes();
        let t = Instant::now();
        native.restyle_all(&dom);
        native_again.push(t.elapsed());
    }
    let med = |v: &mut Vec<std::time::Duration>| {
        v.sort();
        v[v.len() / 2]
    };
    println!("first (sheets parsed): layout engine {blitz_first:?}, pageon-style {native_first:?}");
    println!(
        "full restyle again:    layout engine {:?}, pageon-style {:?}",
        med(&mut blitz_again),
        med(&mut native_again)
    );
    // Incremental: a page toggling a class on a few elements, as scripts do between layouts.
    let all: Vec<NodeId> = elements(&dom)
        .into_iter()
        .filter(|&id| {
            dom.get(id)
                .is_some_and(|n| !["html", "head", "body"].iter().any(|t| n.is_html(t)))
        })
        .collect();
    let mut blitz_small = Vec::new();
    let mut native_small = Vec::new();
    for round in 0..20usize {
        for k in 0..3usize {
            let id = all[(round * 131 + k * 577) % all.len()];
            let old = dom
                .get(id)
                .and_then(|n| n.attr("class"))
                .map(str::to_string);
            let new = match &old {
                Some(c) if c.split_ascii_whitespace().any(|w| w == "text-bold") => c
                    .split_ascii_whitespace()
                    .filter(|w| *w != "text-bold")
                    .collect::<Vec<_>>()
                    .join(" "),
                Some(c) => format!("{c} text-bold"),
                None => "text-bold".into(),
            };
            let _ = dom.set_attr(id, "class", &new);
        }
        let changes = dom.take_style_changes();
        let t = Instant::now();
        let l = Layout::again(&dom, vp, &linked, Some(&base), &styles, prev.take());
        blitz_small.push(t.elapsed());
        prev = Some(l);
        let t = Instant::now();
        let r = native.sync(&dom, &changes, pageon_style::Page::default());
        native_small.push(t.elapsed());
        if round == 0 {
            println!("incremental restyle styled {} elements", r.styled);
        }
    }
    println!(
        "3 class changes:       layout engine {:?}, pageon-style {:?}",
        med(&mut blitz_small),
        med(&mut native_small)
    );
    // Same styles on the page.
    let l = prev.unwrap_or_else(|| panic!("no layout"));
    let mut differ = 0;
    let mut compared = 0;
    for id in elements(&dom) {
        for p in [
            "display",
            "color",
            "visibility",
            "position",
            "font-size",
            "font-weight",
            "background-color",
        ] {
            let Some(theirs) = l.style(id, p) else {
                continue;
            };
            compared += 1;
            if native.value(&dom, id, p).as_deref() != Some(theirs.as_str()) {
                differ += 1;
            }
        }
    }
    println!("styles: {differ} of {compared} differ");
}
