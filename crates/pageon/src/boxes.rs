//! The `boxes` dump: element outline with geometry and actionability flags.

use pageon_dom::{Dom, Edge, NodeData};
use pageon_layout::{Layout, Visibility};

const KEY_ATTRS: &[&str] = &["id", "href", "name", "type", "role", "aria-label"];

pub(crate) fn dump(dom: &Dom, layout: &Layout) -> String {
    let mut out = String::new();
    if let Some(e) = layout.error() {
        return format!("(no layout: {e})\n");
    }
    let mut depth = 0usize;
    let mut skip = 0usize;
    for edge in dom.traverse(dom.root()) {
        match edge {
            Edge::Enter(id) => {
                let Some(node) = dom.get(id) else { continue };
                let NodeData::Element { .. } = &node.data else {
                    continue;
                };
                let vis = layout.visibility(id);
                if skip > 0 || vis == Visibility::NotRendered {
                    skip += 1;
                    continue;
                }
                let name = node.local_name().unwrap_or_default();
                out.push_str(&"  ".repeat(depth));
                out.push_str(name);
                if let Some(b) = layout.box_of(id) {
                    out.push_str(&format!(
                        " {},{} {}×{}",
                        b.x.round(),
                        b.y.round(),
                        b.width.round(),
                        b.height.round()
                    ));
                }
                for k in KEY_ATTRS {
                    if let Some(v) = node.attr(k) {
                        out.push_str(&format!(" {k}={}", quote(v)));
                    }
                }
                if vis == Visibility::Hidden {
                    out.push_str(" hidden");
                } else if !layout.in_viewport(id) {
                    out.push_str(" offscreen");
                } else if let Some(c) = layout.covered_by(id) {
                    let tag = dom.get(c).and_then(|n| n.local_name()).unwrap_or("?");
                    out.push_str(&format!(" covered-by:{tag}"));
                }
                out.push('\n');
                depth += 1;
            }
            Edge::Leave(id) => {
                if !matches!(dom.get(id).map(|n| &n.data), Some(NodeData::Element { .. })) {
                    continue;
                }
                if skip > 0 {
                    skip -= 1;
                } else {
                    depth = depth.saturating_sub(1);
                }
            }
        }
    }
    out
}

fn quote(s: &str) -> String {
    let short: String = s.chars().take(60).collect();
    format!("{:?}", short)
}
