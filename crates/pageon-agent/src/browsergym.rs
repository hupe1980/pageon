//! BrowserGym's observation objects: the AX tree and DOM snapshot in CDP's formats,
//! with the agent's refs as BrowserGym `bid`s, and BrowserGym's extra element properties.
//!
//! pageon's capture already speaks CDP's formats (`Accessibility.getFullAXTree`,
//! `DOMSnapshot.captureSnapshot`); this adds the bids to those objects, never to the page.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::Episode;
use crate::capture::Capture;
use crate::page::{Page, Rect};

/// `{axtree_object, dom_object, extra_element_properties, focused_element_bid}` of a capture,
/// with `episode`'s refs as bids (reconciled as an observation would, without changing what the
/// agent's next diff reports).
pub fn browsergym(episode: &mut Episode, capture: &Capture) -> Value {
    let bids: HashMap<(String, i64), String> = episode
        .elements(capture)
        .into_iter()
        .map(|e| ((e.target.frame_id, e.target.backend_node_id), e.reference))
        .collect();
    let page = Page::build(capture);
    json!({
        "axtree_object": axtree(capture, &bids),
        "dom_object": dom(capture, &bids),
        "extra_element_properties": extra(&page, &bids),
        "focused_element_bid": focused(capture, &bids),
    })
}

/// The frames' AX nodes merged into one tree, as BrowserGym's `extract_merged_axtree`: child
/// frame ids are prefixed `f<i>:` and hang below their `<iframe>`.
fn axtree(capture: &Capture, bids: &HashMap<(String, i64), String>) -> Value {
    let id = |i: usize, s: &str| {
        if i == 0 {
            s.to_string()
        } else {
            format!("f{i}:{s}")
        }
    };
    let mut nodes: Vec<Value> = Vec::new();
    // (frame index, AX id of its `<iframe>` owner in the parent) → child frame root.
    let mut adopt: Vec<(String, String)> = Vec::new();
    for (i, frame) in capture.frames.iter().enumerate() {
        if i > 0
            && let (Some(parent), Some(owner)) =
                (&frame.parent_frame_id, frame.owner_backend_node_id)
            && let Some(p) = capture.frames.iter().position(|f| &f.frame_id == parent)
            && let Some(o) = capture.frames[p]
                .ax_nodes
                .iter()
                .find(|n| n.backend_dom_node_id == Some(owner))
            && let Some(root) = frame.ax_nodes.iter().find(|n| n.parent_id.is_none())
        {
            adopt.push((id(p, &o.node_id), id(i, &root.node_id)));
        }
        for n in &frame.ax_nodes {
            let mut v = serde_json::to_value(n).unwrap_or(Value::Null);
            // CDP omits absent fields; BrowserGym tests for presence (`"value" in node`).
            if let Some(o) = v.as_object_mut() {
                o.retain(|_, x| !x.is_null());
                // Ignored nodes (not rendered, hidden) have no name, as in Chromium's tree: their
                // text never reaches the agent.
                if n.ignored {
                    o.remove("name");
                    o.remove("value");
                } else {
                    o.entry("name")
                        .or_insert_with(|| json!({"type": "computedString", "value": ""}));
                }
                o.entry("role")
                    .or_insert_with(|| json!({"type": "role", "value": "none"}));
            }
            v["nodeId"] = json!(id(i, &n.node_id));
            if let Some(p) = &n.parent_id {
                v["parentId"] = json!(id(i, p));
            }
            v["childIds"] = json!(n.child_ids.iter().map(|c| id(i, c)).collect::<Vec<_>>());
            if let Some(b) = n
                .backend_dom_node_id
                .and_then(|b| bids.get(&(frame.frame_id.clone(), b)))
            {
                v["browsergym_id"] = json!(b);
            }
            nodes.push(v);
        }
    }
    for (owner, root) in adopt {
        if let Some(n) = nodes.iter_mut().find(|n| n["nodeId"] == owner.as_str())
            && let Some(c) = n["childIds"].as_array_mut()
        {
            c.push(json!(root));
        }
    }
    json!({ "nodes": nodes })
}

/// The top snapshot with a `bid` attribute pair on every node that has a ref.
fn dom(capture: &Capture, bids: &HashMap<(String, i64), String>) -> Value {
    let Some(mut snap) = capture.snapshots.first().cloned() else {
        return json!({"documents": [], "strings": []});
    };
    // A page may author `bid` (or BrowserGym's marking attributes) itself: dropped, so only
    // pageon's refs read as element ids.
    let reserved: Vec<i64> = snap
        .strings
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            matches!(
                s.as_str(),
                "bid" | "browsergym_visibility_ratio" | "browsergym_set_of_marks"
            )
        })
        .map(|(i, _)| i as i64)
        .collect();
    for doc in &mut snap.documents {
        for attrs in &mut doc.nodes.attributes {
            if attrs.chunks(2).any(|p| reserved.contains(&p[0])) {
                *attrs = attrs
                    .chunks(2)
                    .filter(|p| !reserved.contains(&p[0]))
                    .flatten()
                    .copied()
                    .collect();
            }
        }
    }
    let name = snap.strings.len() as i64;
    snap.strings.push("bid".into());
    for (d, doc) in snap.documents.iter_mut().enumerate() {
        let Some(frame) = capture.frames.iter().find(|f| {
            f.document
                .is_some_and(|r| r.snapshot == 0 && r.document == d)
        }) else {
            continue;
        };
        for (n, backend) in doc.nodes.backend_node_id.iter().enumerate() {
            let Some(bid) = bids.get(&(frame.frame_id.clone(), *backend)) else {
                continue;
            };
            if let Some(attrs) = doc.nodes.attributes.get_mut(n) {
                attrs.push(name);
                attrs.push(snap.strings.len() as i64);
                snap.strings.push(bid.clone());
            }
        }
    }
    serde_json::to_value(snap).unwrap_or(Value::Null)
}

/// BrowserGym's `extra_element_properties`: visibility (the share of the box in the viewport),
/// bounding box, clickability and whether set-of-marks labels it.
fn extra(page: &Page, bids: &HashMap<(String, i64), String>) -> Value {
    let mut out = serde_json::Map::new();
    for frame in &page.frames {
        for node in &frame.dom {
            let Some(bid) = bids.get(&(frame.frame_id.clone(), node.backend_id)) else {
                continue;
            };
            let rendered = node.layout.as_ref().filter(|l| {
                l.display != "none"
                    && l.visibility != "hidden"
                    && l.visibility != "collapse"
                    && l.opacity > 0.0
                    && l.bounds.area() > 0.0
            });
            let visibility = rendered.map_or(0.0, |l| share(&l.bounds, &page.viewport));
            let bbox = node
                .layout
                .as_ref()
                .filter(|l| l.bounds.area() > 0.0)
                .map(|l| json!([l.bounds.x, l.bounds.y, l.bounds.w, l.bounds.h]));
            out.insert(
                bid.clone(),
                json!({
                    "visibility": visibility,
                    "bbox": bbox,
                    "clickable": node.clickable,
                    "set_of_marks": visibility > 0.5,
                }),
            );
        }
    }
    Value::Object(out)
}

fn share(b: &Rect, viewport: &Rect) -> f64 {
    b.intersect(viewport).map_or(0.0, |i| i.area() / b.area())
}

/// The ref of the focused element, or `""`.
fn focused(capture: &Capture, bids: &HashMap<(String, i64), String>) -> String {
    for frame in &capture.frames {
        for n in &frame.ax_nodes {
            let is_focused = n
                .properties
                .iter()
                .any(|p| p.name == "focused" && p.value.value == Some(Value::Bool(true)));
            if is_focused
                && let Some(b) = n
                    .backend_dom_node_id
                    .and_then(|b| bids.get(&(frame.frame_id.clone(), b)))
            {
                return b.clone();
            }
        }
    }
    String::new()
}
