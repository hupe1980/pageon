//! Node, the tree mixins, character data, text, comments and fragments.

use pageon_dom::{Dom, NodeData, NodeId};

use super::{detach, element_children, gone, hierarchy, insert, nodes_into_node, text_content};
use crate::cv::{Cx, Error, NodeOrString, Obj, R};
use crate::generated::{GetRootNodeOptions, Iface};
use crate::realm::{Coll, Slot};

/// `Node`.
pub mod node {
    use super::*;

    /// The interface a node's wrapper has.
    pub fn iface_of(dom: &Dom, id: NodeId) -> Iface {
        let Some(n) = dom.get(id) else {
            return Iface::Node;
        };
        match &n.data {
            NodeData::Document => Iface::Document,
            NodeData::DocumentFragment => Iface::DocumentFragment,
            NodeData::ShadowRoot { .. } => Iface::ShadowRoot,
            NodeData::Doctype { .. } => Iface::DocumentType,
            NodeData::Text(_) => Iface::Text,
            NodeData::Comment(_) => Iface::Comment,
            NodeData::ProcessingInstruction { .. } => Iface::CharacterData,
            NodeData::Element { .. } => {
                if n.namespace() == Some(pageon_dom::Namespace::Svg) {
                    return svg_iface(n.local_name().unwrap_or_default());
                }
                if n.namespace() != Some(pageon_dom::Namespace::Html) {
                    return Iface::Element;
                }
                let local = n.local_name().unwrap_or_default();
                html_iface(local)
            }
        }
    }

    /// The interface of an SVG element by its tag (`a` → `SVGAElement`); tags without one of
    /// their own are graphics or plain SVG elements.
    fn svg_iface(local: &str) -> Iface {
        let name = match local {
            "svg" => "SVGSVGElement",
            "g" => "SVGGElement",
            "defs" => "SVGDefsElement",
            "desc" => "SVGDescElement",
            "metadata" => "SVGMetadataElement",
            "title" => "SVGTitleElement",
            "symbol" => "SVGSymbolElement",
            "use" => "SVGUseElement",
            "switch" => "SVGSwitchElement",
            "style" => "SVGStyleElement",
            "rect" => "SVGRectElement",
            "circle" => "SVGCircleElement",
            "ellipse" => "SVGEllipseElement",
            "line" => "SVGLineElement",
            "polyline" => "SVGPolylineElement",
            "polygon" => "SVGPolygonElement",
            "path" => "SVGGeometryElement",
            "text" => "SVGTextElement",
            "tspan" => "SVGTSpanElement",
            "textPath" => "SVGTextPathElement",
            "image" => "SVGImageElement",
            "foreignObject" => "SVGForeignObjectElement",
            "marker" => "SVGMarkerElement",
            "linearGradient" => "SVGLinearGradientElement",
            "radialGradient" => "SVGRadialGradientElement",
            "stop" => "SVGStopElement",
            "pattern" => "SVGPatternElement",
            "a" => "SVGAElement",
            "view" => "SVGViewElement",
            "clipPath" | "mask" => "SVGElement",
            _ => "SVGElement",
        };
        Iface::from_name(name).unwrap_or(Iface::SVGElement)
    }

    fn html_iface(local: &str) -> Iface {
        let name = match local {
            "a" => "HTMLAnchorElement",
            "area" => "HTMLAreaElement",
            "audio" => "HTMLAudioElement",
            "base" => "HTMLBaseElement",
            "blockquote" | "q" => "HTMLQuoteElement",
            "body" => "HTMLBodyElement",
            "br" => "HTMLBRElement",
            "button" => "HTMLButtonElement",
            "canvas" => "HTMLCanvasElement",
            "caption" => "HTMLTableCaptionElement",
            "col" | "colgroup" => "HTMLTableColElement",
            "data" => "HTMLDataElement",
            "datalist" => "HTMLDataListElement",
            "del" | "ins" => "HTMLModElement",
            "details" => "HTMLDetailsElement",
            "dialog" => "HTMLDialogElement",
            "div" => "HTMLDivElement",
            "dl" => "HTMLDListElement",
            "embed" => "HTMLEmbedElement",
            "fieldset" => "HTMLFieldSetElement",
            "form" => "HTMLFormElement",
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "HTMLHeadingElement",
            "head" => "HTMLHeadElement",
            "hr" => "HTMLHRElement",
            "html" => "HTMLHtmlElement",
            "iframe" => "HTMLIFrameElement",
            "img" => "HTMLImageElement",
            "input" => "HTMLInputElement",
            "label" => "HTMLLabelElement",
            "legend" => "HTMLLegendElement",
            "li" => "HTMLLIElement",
            "link" => "HTMLLinkElement",
            "map" => "HTMLMapElement",
            "menu" => "HTMLMenuElement",
            "meta" => "HTMLMetaElement",
            "meter" => "HTMLMeterElement",
            "object" => "HTMLObjectElement",
            "ol" => "HTMLOListElement",
            "optgroup" => "HTMLOptGroupElement",
            "option" => "HTMLOptionElement",
            "output" => "HTMLOutputElement",
            "p" => "HTMLParagraphElement",
            "picture" => "HTMLPictureElement",
            "pre" | "listing" | "xmp" => "HTMLPreElement",
            "progress" => "HTMLProgressElement",
            "script" => "HTMLScriptElement",
            "select" => "HTMLSelectElement",
            "slot" => "HTMLSlotElement",
            "source" => "HTMLSourceElement",
            "span" => "HTMLSpanElement",
            "style" => "HTMLStyleElement",
            "table" => "HTMLTableElement",
            "tbody" | "thead" | "tfoot" => "HTMLTableSectionElement",
            "td" | "th" => "HTMLTableCellElement",
            "template" => "HTMLTemplateElement",
            "textarea" => "HTMLTextAreaElement",
            "time" => "HTMLTimeElement",
            "title" => "HTMLTitleElement",
            "tr" => "HTMLTableRowElement",
            "track" => "HTMLTrackElement",
            "ul" => "HTMLUListElement",
            "video" => "HTMLVideoElement",
            "abbr" | "address" | "article" | "aside" | "b" | "bdi" | "bdo" | "cite" | "code"
            | "dd" | "dfn" | "dt" | "em" | "figcaption" | "figure" | "footer" | "header"
            | "hgroup" | "i" | "kbd" | "main" | "mark" | "nav" | "noscript" | "rp" | "rt"
            | "ruby" | "s" | "samp" | "search" | "section" | "small" | "strong" | "sub"
            | "summary" | "sup" | "u" | "var" | "wbr" | "center" | "nobr" | "tt" | "big"
            | "strike" | "acronym" | "noframes" | "plaintext" => "HTMLElement",
            l if l.contains('-') => "HTMLElement",
            _ => "HTMLUnknownElement",
        };
        Iface::from_name(name).unwrap_or(Iface::HTMLElement)
    }

    fn opt(id: Option<NodeId>) -> Option<Obj> {
        id.map(Obj::Node)
    }

    pub fn get_node_type(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        let id = this.node()?;
        Ok(match cx.dom().get(id).map(|n| &n.data) {
            Some(NodeData::Element { .. }) => 1,
            Some(NodeData::Text(_)) => 3,
            Some(NodeData::ProcessingInstruction { .. }) => 7,
            Some(NodeData::Comment(_)) => 8,
            Some(NodeData::Document) => 9,
            Some(NodeData::Doctype { .. }) => 10,
            Some(NodeData::DocumentFragment | NodeData::ShadowRoot { .. }) => 11,
            None => 0,
        })
    }

    pub fn get_node_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        let dom = cx.dom();
        let n = dom.get(id).ok_or_else(gone)?;
        Ok(match &n.data {
            NodeData::Element { .. } => super::super::element::tag_name(n),
            NodeData::Text(_) => "#text".into(),
            NodeData::Comment(_) => "#comment".into(),
            NodeData::Document => "#document".into(),
            NodeData::DocumentFragment | NodeData::ShadowRoot { .. } => "#document-fragment".into(),
            NodeData::Doctype { name, .. } => name.clone(),
            NodeData::ProcessingInstruction { target, .. } => target.clone(),
        })
    }

    pub fn get_base_uri(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().url.clone())
    }

    pub fn get_is_connected(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        let id = this.node()?;
        Ok(cx.dom().is_connected(id))
    }

    pub fn get_owner_document(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let id = this.node()?;
        let dom = cx.dom();
        let is_doc = |n| {
            matches!(
                dom.get(n).map(|x| &x.data),
                Some(pageon_dom::NodeData::Document)
            )
        };
        if is_doc(id) {
            return Ok(None);
        }
        // A node of a document DOMParser made belongs to it; every other node to the realm's.
        let top = dom.root_of(id);
        Ok(Some(Obj::Node(if is_doc(top) { top } else { dom.root() })))
    }

    /// The root of the node's tree; with `composed`, through shadow hosts.
    pub fn get_root_node(cx: &mut Cx<'_, '_, '_>, this: Obj, o: GetRootNodeOptions) -> R<Obj> {
        let id = this.node()?;
        let dom = cx.dom();
        Ok(Obj::Node(if o.composed {
            dom.shadow_including_root(id)
        } else {
            dom.root_of(id)
        }))
    }

    pub fn get_parent_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(opt(cx.dom().parent(this.node()?)))
    }

    pub fn get_parent_element(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let dom = cx.dom();
        Ok(opt(dom
            .parent(this.node()?)
            .filter(|&p| super::super::is_element(&dom, p))))
    }

    pub fn has_child_nodes(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(cx.dom().first_child(this.node()?).is_some())
    }

    pub fn get_child_nodes(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "childNodes",
            Iface::NodeList,
            Slot::ChildNodes(id),
        ))
    }

    pub fn get_first_child(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(opt(cx.dom().first_child(this.node()?)))
    }

    pub fn get_last_child(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(opt(cx.dom().last_child(this.node()?)))
    }

    pub fn get_previous_sibling(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(opt(cx.dom().prev_sibling(this.node()?)))
    }

    pub fn get_next_sibling(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(opt(cx.dom().next_sibling(this.node()?)))
    }

    pub fn get_node_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        Ok(match cx.dom().get(this.node()?).map(|n| &n.data) {
            Some(NodeData::Text(t) | NodeData::Comment(t)) => Some(t.clone()),
            Some(NodeData::ProcessingInstruction { data, .. }) => Some(data.clone()),
            _ => None,
        })
    }

    pub fn set_node_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<String>) -> R<()> {
        let id = this.node()?;
        let is_data = matches!(
            cx.dom().get(id).map(|n| &n.data),
            Some(NodeData::Text(_) | NodeData::Comment(_))
        );
        if is_data {
            cx.dom_mut()
                .set_text(id, &v.unwrap_or_default())
                .map_err(|_| gone())?;
        }
        Ok(())
    }

    pub fn get_text_content(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        let id = this.node()?;
        let dom = cx.dom();
        Ok(match dom.get(id).map(|n| &n.data) {
            Some(NodeData::Document | NodeData::Doctype { .. }) | None => None,
            Some(NodeData::Text(t) | NodeData::Comment(t)) => Some(t.clone()),
            Some(NodeData::ProcessingInstruction { data, .. }) => Some(data.clone()),
            _ => Some(text_content(&dom, id)),
        })
    }

    /// Replaces all children with one text node (none for the empty string).
    pub fn replace_with_text(cx: &Cx<'_, '_, '_>, id: NodeId, text: &str) -> R<()> {
        let kids: Vec<NodeId> = cx.dom().children(id).collect();
        for k in kids {
            detach(cx, k)?;
        }
        if !text.is_empty() {
            let t = cx.dom_mut().create(NodeData::Text(text.to_string()));
            cx.dom_mut()
                .append(id, t)
                .map_err(|e| hierarchy(&e.to_string()))?;
        }
        Ok(())
    }

    pub fn set_text_content(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<String>) -> R<()> {
        let id = this.node()?;
        let kind = cx.dom().get(id).map(|n| n.data.clone());
        match kind {
            Some(NodeData::Text(_) | NodeData::Comment(_)) => cx
                .dom_mut()
                .set_text(id, &v.unwrap_or_default())
                .map_err(|_| gone()),
            Some(NodeData::Element { .. } | NodeData::DocumentFragment) => {
                replace_with_text(cx, id, &v.unwrap_or_default())
            }
            _ => Ok(()),
        }
    }

    pub fn normalize(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = this.node()?;
        let texts: Vec<NodeId> = {
            let dom = cx.dom();
            dom.traverse(id)
                .filter_map(|e| match e {
                    pageon_dom::Edge::Enter(n)
                        if matches!(dom.get(n).map(|x| &x.data), Some(NodeData::Text(_))) =>
                    {
                        Some(n)
                    }
                    _ => None,
                })
                .collect()
        };
        for t in texts {
            if cx.dom().get(t).is_none() {
                continue;
            }
            let mut data = match cx.dom().get(t).map(|n| &n.data) {
                Some(NodeData::Text(s)) => s.clone(),
                _ => continue,
            };
            loop {
                let next = {
                    let dom = cx.dom();
                    dom.next_sibling(t)
                        .filter(|&n| matches!(dom.get(n).map(|x| &x.data), Some(NodeData::Text(_))))
                };
                let Some(next) = next else { break };
                let more = match cx.dom().get(next).map(|n| &n.data) {
                    Some(NodeData::Text(s)) => s.clone(),
                    _ => String::new(),
                };
                data.push_str(&more);
                detach(cx, next)?;
            }
            if data.is_empty() {
                detach(cx, t)?;
            } else {
                cx.dom_mut().set_text(t, &data).map_err(|_| gone())?;
            }
        }
        Ok(())
    }

    pub fn clone_node(cx: &mut Cx<'_, '_, '_>, this: Obj, deep: bool) -> R<Obj> {
        let id = this.node()?;
        let copy = super::super::copy_within(&mut cx.dom_mut(), id, deep);
        let copy = copy.ok_or_else(gone)?;
        // Form control state is cloned with the node (value, checkedness).
        let state = cx.dom().control(id).cloned();
        if let Some(s) = state {
            *cx.dom_mut().control_mut(copy) = s;
        }
        super::super::copy_started(cx, id, copy);
        Ok(Obj::Node(copy))
    }

    pub fn is_equal_node(cx: &mut Cx<'_, '_, '_>, this: Obj, other: Option<Obj>) -> R<bool> {
        let Some(o) = other else { return Ok(false) };
        let (a, b) = (this.node()?, o.node()?);
        let dom = cx.dom();
        Ok(dom.to_html(a) == dom.to_html(b)
            && dom.get(a).map(|n| std::mem::discriminant(&n.data))
                == dom.get(b).map(|n| std::mem::discriminant(&n.data)))
    }

    pub fn is_same_node(_cx: &mut Cx<'_, '_, '_>, this: Obj, other: Option<Obj>) -> R<bool> {
        Ok(other == Some(this))
    }

    pub fn compare_document_position(cx: &mut Cx<'_, '_, '_>, this: Obj, other: Obj) -> R<u16> {
        let (a, b) = (this.node()?, other.node()?);
        if a == b {
            return Ok(0);
        }
        let dom = cx.dom();
        let root = |mut n: NodeId| {
            while let Some(p) = dom.parent(n) {
                n = p;
            }
            n
        };
        if root(a) != root(b) {
            return Ok(1 | 32 | if a.index() < b.index() { 4 } else { 2 });
        }
        if dom.is_inclusive_ancestor(b, a) {
            return Ok(8 | 2); // other contains this, and precedes it
        }
        if dom.is_inclusive_ancestor(a, b) {
            return Ok(16 | 4); // other is contained by this, and follows it
        }
        let order: Vec<NodeId> = dom
            .traverse(root(a))
            .filter_map(|e| match e {
                pageon_dom::Edge::Enter(n) if n == a || n == b => Some(n),
                _ => None,
            })
            .collect();
        Ok(if order.first() == Some(&b) { 2 } else { 4 })
    }

    pub fn contains(cx: &mut Cx<'_, '_, '_>, this: Obj, other: Option<Obj>) -> R<bool> {
        let Some(Obj::Node(o)) = other else {
            return Ok(false);
        };
        Ok(cx.dom().is_inclusive_ancestor(this.node()?, o))
    }

    pub fn insert_before(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        node: Obj,
        child: Option<Obj>,
    ) -> R<Obj> {
        let child = match child {
            Some(c) => Some(c.node()?),
            None => None,
        };
        insert(cx, this.node()?, node.node()?, child)?;
        Ok(node)
    }

    pub fn append_child(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj) -> R<Obj> {
        insert(cx, this.node()?, node.node()?, None)?;
        Ok(node)
    }

    pub fn replace_child(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj, child: Obj) -> R<Obj> {
        let (parent, n, c) = (this.node()?, node.node()?, child.node()?);
        if cx.dom().parent(c) != Some(parent) {
            return Err(Error::dom(
                "NotFoundError",
                "The node to be replaced is not a child of this node.",
            ));
        }
        if n == c {
            return Ok(child);
        }
        let next = cx.dom().next_sibling(c).filter(|&x| x != n);
        detach(cx, c)?;
        insert(cx, parent, n, next)?;
        Ok(child)
    }

    pub fn remove_child(cx: &mut Cx<'_, '_, '_>, this: Obj, child: Obj) -> R<Obj> {
        let (parent, c) = (this.node()?, child.node()?);
        if cx.dom().parent(c) != Some(parent) {
            return Err(Error::dom(
                "NotFoundError",
                "The node to be removed is not a child of this node.",
            ));
        }
        detach(cx, c)?;
        Ok(child)
    }
}

/// The `ChildNode` mixin.
pub mod child_node {
    use super::*;

    fn insert_relative(
        cx: &Cx<'_, '_, '_>,
        this: NodeId,
        items: Vec<NodeOrString>,
        after: bool,
    ) -> R<()> {
        let Some(parent) = cx.dom().parent(this) else {
            return Ok(());
        };
        let excluded: Vec<NodeId> = items
            .iter()
            .filter_map(|i| {
                if let NodeOrString::Node(o) = i {
                    o.node().ok()
                } else {
                    None
                }
            })
            .collect();
        let reference = if after {
            std::iter::successors(cx.dom().next_sibling(this), |&s| cx.dom().next_sibling(s))
                .find(|s| !excluded.contains(s))
        } else {
            Some(this)
        };
        let reference = if after {
            reference
        } else {
            // `before`: the first preceding sibling not being inserted, then its next sibling.
            let prev =
                std::iter::successors(cx.dom().prev_sibling(this), |&s| cx.dom().prev_sibling(s))
                    .find(|s| !excluded.contains(s));
            match prev {
                Some(p) => cx.dom().next_sibling(p),
                None => cx.dom().first_child(parent),
            }
        };
        if let Some(node) = nodes_into_node(cx, items)? {
            insert(cx, parent, node, reference.filter(|&r| r != node))?;
        }
        Ok(())
    }

    pub fn before(cx: &mut Cx<'_, '_, '_>, this: Obj, items: Vec<NodeOrString>) -> R<()> {
        insert_relative(cx, this.node()?, items, false)
    }

    pub fn after(cx: &mut Cx<'_, '_, '_>, this: Obj, items: Vec<NodeOrString>) -> R<()> {
        insert_relative(cx, this.node()?, items, true)
    }

    pub fn replace_with(cx: &mut Cx<'_, '_, '_>, this: Obj, items: Vec<NodeOrString>) -> R<()> {
        let id = this.node()?;
        let Some(parent) = cx.dom().parent(id) else {
            return Ok(());
        };
        let next = cx.dom().next_sibling(id);
        let node = nodes_into_node(cx, items)?;
        detach(cx, id)?;
        if let Some(n) = node {
            let next = next.filter(|&x| cx.dom().parent(x) == Some(parent) && x != n);
            insert(cx, parent, n, next)?;
        }
        Ok(())
    }

    pub fn remove(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        detach(cx, this.node()?)
    }
}

/// The `ParentNode` mixin.
pub mod parent_node {
    use super::*;

    pub fn get_children(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "children",
            Iface::HTMLCollection,
            Slot::Collection(Coll::Children(id)),
        ))
    }

    pub fn get_first_element_child(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(element_children(&cx.dom(), this.node()?)
            .first()
            .copied()
            .map(Obj::Node))
    }

    pub fn get_last_element_child(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(element_children(&cx.dom(), this.node()?)
            .last()
            .copied()
            .map(Obj::Node))
    }

    pub fn get_child_element_count(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(element_children(&cx.dom(), this.node()?).len()).unwrap_or(u32::MAX))
    }

    pub fn prepend(cx: &mut Cx<'_, '_, '_>, this: Obj, items: Vec<NodeOrString>) -> R<()> {
        let id = this.node()?;
        if let Some(n) = nodes_into_node(cx, items)? {
            let first = cx.dom().first_child(id).filter(|&f| f != n);
            insert(cx, id, n, first)?;
        }
        Ok(())
    }

    pub fn append(cx: &mut Cx<'_, '_, '_>, this: Obj, items: Vec<NodeOrString>) -> R<()> {
        let id = this.node()?;
        if let Some(n) = nodes_into_node(cx, items)? {
            insert(cx, id, n, None)?;
        }
        Ok(())
    }

    pub fn replace_children(cx: &mut Cx<'_, '_, '_>, this: Obj, items: Vec<NodeOrString>) -> R<()> {
        let id = this.node()?;
        let node = nodes_into_node(cx, items)?;
        let kids: Vec<NodeId> = cx.dom().children(id).collect();
        for k in kids {
            detach(cx, k)?;
        }
        if let Some(n) = node {
            insert(cx, id, n, None)?;
        }
        Ok(())
    }

    pub fn query_selector(cx: &mut Cx<'_, '_, '_>, this: Obj, selector: String) -> R<Option<Obj>> {
        let s = crate::select::parse(&selector)?;
        let (focused, modals) = super::super::sel_ctx(cx);
        let ctx = crate::select::Ctx {
            focused,
            modals: &modals,
        };
        Ok(s.query(&cx.dom(), this.node()?, ctx, true)
            .first()
            .copied()
            .map(Obj::Node))
    }

    pub fn query_selector_all(cx: &mut Cx<'_, '_, '_>, this: Obj, selector: String) -> R<Obj> {
        let s = crate::select::parse(&selector)?;
        let (focused, modals) = super::super::sel_ctx(cx);
        let ctx = crate::select::Ctx {
            focused,
            modals: &modals,
        };
        let found = s.query(&cx.dom(), this.node()?, ctx, false);
        Ok(cx.alloc(Iface::NodeList, Slot::Static(found)))
    }
}

/// The `NonDocumentTypeChildNode` mixin.
pub mod non_document_type_child_node {
    use super::*;

    pub fn get_previous_element_sibling(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let dom = cx.dom();
        Ok(
            std::iter::successors(dom.prev_sibling(this.node()?), |&s| dom.prev_sibling(s))
                .find(|&s| super::super::is_element(&dom, s))
                .map(Obj::Node),
        )
    }

    pub fn get_next_element_sibling(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let dom = cx.dom();
        Ok(
            std::iter::successors(dom.next_sibling(this.node()?), |&s| dom.next_sibling(s))
                .find(|&s| super::super::is_element(&dom, s))
                .map(Obj::Node),
        )
    }
}

/// `CharacterData`.
pub mod character_data {
    use super::*;

    pub(crate) fn data(cx: &Cx<'_, '_, '_>, id: NodeId) -> R<String> {
        match cx.dom().get(id).map(|n| &n.data) {
            Some(NodeData::Text(t) | NodeData::Comment(t)) => Ok(t.clone()),
            Some(NodeData::ProcessingInstruction { data, .. }) => Ok(data.clone()),
            _ => Err(gone()),
        }
    }

    pub fn get_data(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        data(cx, this.node()?)
    }

    pub fn set_data(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut().set_text(this.node()?, &v).map_err(|_| gone())
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(data(cx, this.node()?)?.encode_utf16().count()).unwrap_or(u32::MAX))
    }

    pub fn append_data(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let id = this.node()?;
        let len = cx.dom().node_length(id);
        cx.dom_mut()
            .replace_data(id, len, 0, &v)
            .map_err(|_| gone())
    }

    fn check(cx: &Cx<'_, '_, '_>, id: NodeId, offset: u32) -> R<u32> {
        let len = cx.dom().node_length(id);
        if offset > len {
            return Err(Error::dom(
                "IndexSizeError",
                format!("The offset {offset} is greater than the node's length ({len})."),
            ));
        }
        Ok(len)
    }

    pub fn substring_data(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        offset: u32,
        count: u32,
    ) -> R<String> {
        let id = this.node()?;
        let len = check(cx, id, offset)?;
        let units: Vec<u16> = data(cx, id)?.encode_utf16().collect();
        let end = offset.saturating_add(count).min(len);
        Ok(String::from_utf16_lossy(
            &units[offset as usize..end as usize],
        ))
    }

    pub fn insert_data(cx: &mut Cx<'_, '_, '_>, this: Obj, offset: u32, v: String) -> R<()> {
        replace_data(cx, this, offset, 0, v)
    }

    pub fn delete_data(cx: &mut Cx<'_, '_, '_>, this: Obj, offset: u32, count: u32) -> R<()> {
        replace_data(cx, this, offset, count, String::new())
    }

    pub fn replace_data(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        offset: u32,
        count: u32,
        v: String,
    ) -> R<()> {
        let id = this.node()?;
        check(cx, id, offset)?;
        cx.dom_mut()
            .replace_data(id, offset, count, &v)
            .map_err(|_| gone())
    }
}

/// `Text`.
pub mod text {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, data: String) -> R<Obj> {
        Ok(Obj::Node(cx.dom_mut().create(NodeData::Text(data))))
    }

    pub fn split_text(cx: &mut Cx<'_, '_, '_>, this: Obj, offset: u32) -> R<Obj> {
        let id = this.node()?;
        let len = cx.dom().node_length(id);
        if offset > len {
            return Err(Error::dom(
                "IndexSizeError",
                format!("The offset {offset} is larger than the Text node's length."),
            ));
        }
        Ok(Obj::Node(
            cx.dom_mut().split_text(id, offset).map_err(|_| gone())?,
        ))
    }

    pub fn get_whole_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        let dom = cx.dom();
        let is_text = |n: NodeId| matches!(dom.get(n).map(|x| &x.data), Some(NodeData::Text(_)));
        let mut first = id;
        while let Some(p) = dom.prev_sibling(first).filter(|&p| is_text(p)) {
            first = p;
        }
        let mut s = String::new();
        let mut cur = Some(first);
        while let Some(c) = cur.filter(|&c| is_text(c)) {
            if let Some(NodeData::Text(t)) = dom.get(c).map(|x| &x.data) {
                s.push_str(t);
            }
            cur = dom.next_sibling(c);
        }
        Ok(s)
    }
}

/// `Comment`.
pub mod comment {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, data: String) -> R<Obj> {
        Ok(Obj::Node(cx.dom_mut().create(NodeData::Comment(data))))
    }
}

/// `DocumentFragment`.
pub mod document_fragment {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        Ok(Obj::Node(cx.dom_mut().create(NodeData::DocumentFragment)))
    }
}
