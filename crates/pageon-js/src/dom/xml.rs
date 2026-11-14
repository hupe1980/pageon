//! `XMLSerializer` (DOM Parsing): XML serialization of a node, with `xmlns` declarations where
//! an element's namespace differs from its parent's, self-closed empty elements, and escaped
//! text and attribute values.

use pageon_dom::{Dom, Namespace, NodeData, NodeId};

use crate::cv::{Cx, Obj, R};
use crate::generated::Iface;
use crate::realm::Slot;

fn uri(ns: Namespace) -> Option<&'static str> {
    Some(match ns {
        Namespace::Html => "http://www.w3.org/1999/xhtml",
        Namespace::Svg => "http://www.w3.org/2000/svg",
        Namespace::MathMl => "http://www.w3.org/1998/Math/MathML",
        Namespace::Xlink => "http://www.w3.org/1999/xlink",
        Namespace::Xml => "http://www.w3.org/XML/1998/namespace",
        Namespace::Xmlns => "http://www.w3.org/2000/xmlns/",
        Namespace::None => return None,
    })
}

const VOID: &[&str] = &[
    "area", "base", "basefont", "bgsound", "br", "col", "embed", "frame", "hr", "img", "input",
    "keygen", "link", "meta", "param", "source", "track", "wbr",
];

fn escape(s: &str, attr: bool, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// Serializes `id` as XML into `out`; `inherited` is the parent element's namespace.
pub(crate) fn serialize(dom: &Dom, id: NodeId, inherited: Option<Namespace>, out: &mut String) {
    let Some(node) = dom.get(id) else { return };
    match &node.data {
        NodeData::Document | NodeData::DocumentFragment | NodeData::ShadowRoot { .. } => {
            for c in dom.children(id) {
                serialize(dom, c, inherited, out);
            }
        }
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            if !public_id.is_empty() {
                out.push_str(&format!(" PUBLIC \"{public_id}\""));
            }
            if !system_id.is_empty() {
                if public_id.is_empty() {
                    out.push_str(" SYSTEM");
                }
                out.push_str(&format!(" \"{system_id}\""));
            }
            out.push('>');
        }
        NodeData::Text(t) => escape(t, false, out),
        NodeData::Comment(t) => {
            out.push_str("<!--");
            out.push_str(t);
            out.push_str("-->");
        }
        NodeData::ProcessingInstruction { target, data } => {
            out.push_str(&format!("<?{target} {data}?>"));
        }
        NodeData::Element {
            template_contents, ..
        } => {
            let ns = node.namespace().unwrap_or(Namespace::None);
            let local = node.local_name().unwrap_or_default();
            out.push('<');
            out.push_str(local);
            if Some(ns) != inherited
                && let Some(u) = uri(ns)
            {
                out.push_str(&format!(" xmlns=\"{u}\""));
            }
            for a in node.attrs() {
                out.push(' ');
                out.push_str(&a.qualified_name());
                out.push_str("=\"");
                escape(&a.value, true, out);
                out.push('"');
            }
            let kids = template_contents.unwrap_or(id);
            let empty = dom.children(kids).next().is_none();
            if empty && (ns != Namespace::Html || VOID.contains(&local)) {
                out.push_str(if ns == Namespace::Html { " />" } else { "/>" });
                return;
            }
            out.push('>');
            for c in dom.children(kids) {
                serialize(dom, c, Some(ns), out);
            }
            out.push_str(&format!("</{local}>"));
        }
    }
}

pub mod xml_serializer {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        Ok(cx.alloc(Iface::XMLSerializer, Slot::Serializer))
    }

    pub fn serialize_to_string(cx: &mut Cx<'_, '_, '_>, _this: Obj, root: Obj) -> R<String> {
        let id = root.node()?;
        let mut out = String::new();
        serialize(&cx.dom(), id, None, &mut out);
        Ok(out)
    }
}
