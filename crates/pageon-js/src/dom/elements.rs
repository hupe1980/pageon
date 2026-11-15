//! Element, HTMLElement and the element mixins.

use pageon_dom::{NodeData, NodeId};

use super::{detach, gone, insert, parse_fragment_into};
use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{FocusOptions, Iface, ScrollToOptions};
use crate::realm::{Coll, Slot};

fn is_html(cx: &Cx<'_, '_, '_>, id: NodeId) -> bool {
    cx.dom()
        .get(id)
        .is_some_and(|n| n.namespace() == Some(pageon_dom::Namespace::Html))
}

/// Attribute names are lowercased on HTML elements (HTML documents).
fn attr_name(cx: &Cx<'_, '_, '_>, id: NodeId, name: &str) -> String {
    if is_html(cx, id) {
        name.to_ascii_lowercase()
    } else {
        name.to_string()
    }
}

fn valid_name(name: &str) -> R<()> {
    let bad = name.is_empty()
        || name
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '>' | '/' | '=' | '<' | '\0'));
    if bad {
        return Err(Error::dom(
            "InvalidCharacterError",
            format!("'{name}' is not a valid attribute name."),
        ));
    }
    Ok(())
}

/// The box of an element, if it has one.
/// An element's border box relative to the viewport (`getBoundingClientRect`).
pub(crate) fn rect(cx: &Cx<'_, '_, '_>, id: NodeId) -> Option<pageon_layout::Rect> {
    let layout = cx.realm.layout();
    layout.client_rect(id)
}

/// A computed style length in px.
pub(crate) fn px(cx: &Cx<'_, '_, '_>, id: NodeId, prop: &str) -> f64 {
    let layout = cx.realm.layout();
    layout
        .style(id, prop)
        .and_then(|v| v.trim_end_matches("px").parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// `Element`.
pub mod element {
    use super::*;

    pub use crate::dom::shadow::element_shadow::{attach_shadow, get_shadow_root};

    pub fn get_slot(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        Ok(cx
            .dom()
            .get(id)
            .and_then(|n| n.attr("slot").map(str::to_string))
            .unwrap_or_default())
    }

    pub fn set_slot(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let id = this.node()?;
        cx.dom_mut()
            .set_attr(id, "slot", &v)
            .map_err(|_| Error::Type("Illegal invocation".into()))
    }

    /// `tagName` of a node: uppercase for HTML elements.
    pub fn tag_name(n: &pageon_dom::Node) -> String {
        let NodeData::Element { name, .. } = &n.data else {
            return String::new();
        };
        let q = match &name.prefix {
            Some(p) => format!("{}:{}", p, name.local),
            None => name.local.to_string(),
        };
        if n.namespace() == Some(pageon_dom::Namespace::Html) {
            q.to_ascii_uppercase()
        } else {
            q
        }
    }

    pub fn get_namespace_uri(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        let ns =
            super::super::namespace_url(cx.dom().get(this.node()?).and_then(|n| n.namespace()));
        Ok((!ns.is_empty()).then(|| ns.to_string()))
    }

    pub fn get_prefix(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        Ok(match cx.dom().get(this.node()?).map(|n| &n.data) {
            Some(NodeData::Element { name, .. }) => name.prefix.as_ref().map(ToString::to_string),
            _ => None,
        })
    }

    pub fn get_local_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(cx
            .dom()
            .get(this.node()?)
            .and_then(|n| n.local_name())
            .unwrap_or_default()
            .to_string())
    }

    pub fn get_tag_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(cx.dom().get(this.node()?).map(tag_name).unwrap_or_default())
    }

    pub fn get_id(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(cx
            .dom()
            .get(this.node()?)
            .and_then(|n| n.attr("id"))
            .unwrap_or_default()
            .to_string())
    }

    pub fn set_id(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "id", &v)
            .map_err(|_| gone())
    }

    pub fn get_class_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(cx
            .dom()
            .get(this.node()?)
            .and_then(|n| n.attr("class"))
            .unwrap_or_default()
            .to_string())
    }

    pub fn set_class_name(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "class", &v)
            .map_err(|_| gone())
    }

    pub fn get_class_list(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "classList",
            Iface::DOMTokenList,
            Slot::TokenList(id, "class"),
        ))
    }

    pub fn get_attributes(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "attributes",
            Iface::NamedNodeMap,
            Slot::NamedNodeMap(id),
        ))
    }

    pub fn has_attributes(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(cx
            .dom()
            .get(this.node()?)
            .is_some_and(|n| !n.attrs().is_empty()))
    }

    pub fn get_attribute_names(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<String>> {
        Ok(cx
            .dom()
            .get(this.node()?)
            .map(|n| {
                n.attrs()
                    .iter()
                    .map(pageon_dom::Attribute::qualified_name)
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn get_attribute(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Option<String>> {
        let id = this.node()?;
        let name = attr_name(cx, id, &name);
        let dom = cx.dom();
        Ok(dom
            .get(id)
            .and_then(|n| n.attrs().iter().find(|a| a.qualified_name() == name))
            .map(|a| a.value.clone()))
    }

    pub fn get_attribute_ns(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        _ns: Option<String>,
        local: String,
    ) -> R<Option<String>> {
        Ok(cx
            .dom()
            .get(this.node()?)
            .and_then(|n| n.attr(&local))
            .map(str::to_string))
    }

    pub fn set_attribute(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String, value: String) -> R<()> {
        valid_name(&name)?;
        let id = this.node()?;
        let name = attr_name(cx, id, &name);
        if name == "src" && cx.dom().get(id).is_some_and(|n| n.is_html("iframe")) {
            cx.st().frames_changed = true;
        }
        cx.dom_mut().set_attr(id, &name, &value).map_err(|_| gone())
    }

    pub fn set_attribute_ns(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        _ns: Option<String>,
        name: String,
        value: String,
    ) -> R<()> {
        valid_name(&name)?;
        let local = name.rsplit(':').next().unwrap_or(&name).to_string();
        cx.dom_mut()
            .set_attr(this.node()?, &local, &value)
            .map_err(|_| gone())
    }

    /// `getAttributeNode(name)`: the attribute as an `Attr`, or `null`.
    pub fn get_attribute_node(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Option<Obj>> {
        let id = this.node()?;
        let name = attr_name(cx, id, &name);
        let present = cx.dom().get(id).is_some_and(|n| n.attr(&name).is_some());
        Ok(present.then(|| cx.alloc(crate::generated::Iface::Attr, Slot::Attr(id, name))))
    }

    /// `removeAttributeNode(attr)`: removes the attribute `attr` names from this element
    /// (sanitizers such as DOMPurify remove attributes this way).
    pub fn remove_attribute_node(cx: &mut Cx<'_, '_, '_>, this: Obj, attr: Obj) -> R<Obj> {
        let id = this.node()?;
        let (owner, name) = match cx.st().slot(attr.slab()?) {
            Some(Slot::Attr(n, name)) => (*n, name.clone()),
            _ => return Err(Error::Type("Failed to execute 'removeAttributeNode' on 'Element': parameter 1 is not of type 'Attr'.".into())),
        };
        if owner != id || cx.dom().get(id).is_none_or(|n| n.attr(&name).is_none()) {
            return Err(Error::dom(
                "NotFoundError",
                "Failed to execute 'removeAttributeNode' on 'Element': The node provided is owned by another element.",
            ));
        }
        cx.dom_mut().remove_attr(id, &name).map_err(|_| gone())?;
        Ok(attr)
    }

    pub fn remove_attribute(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<()> {
        let id = this.node()?;
        let name = attr_name(cx, id, &name);
        cx.dom_mut().remove_attr(id, &name).map_err(|_| gone())
    }

    pub fn remove_attribute_ns(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        _ns: Option<String>,
        local: String,
    ) -> R<()> {
        cx.dom_mut()
            .remove_attr(this.node()?, &local)
            .map_err(|_| gone())
    }

    pub fn toggle_attribute(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        name: String,
        force: Option<bool>,
    ) -> R<bool> {
        valid_name(&name)?;
        let id = this.node()?;
        let name = attr_name(cx, id, &name);
        let has = cx.dom().get(id).is_some_and(|n| n.attr(&name).is_some());
        let want = force.unwrap_or(!has);
        if want && !has {
            cx.dom_mut().set_attr(id, &name, "").map_err(|_| gone())?;
        } else if !want && has {
            cx.dom_mut().remove_attr(id, &name).map_err(|_| gone())?;
        }
        Ok(want)
    }

    pub fn has_attribute(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<bool> {
        let id = this.node()?;
        let name = attr_name(cx, id, &name);
        Ok(cx
            .dom()
            .get(id)
            .is_some_and(|n| n.attrs().iter().any(|a| a.qualified_name() == name)))
    }

    pub fn has_attribute_ns(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        _ns: Option<String>,
        local: String,
    ) -> R<bool> {
        Ok(cx
            .dom()
            .get(this.node()?)
            .is_some_and(|n| n.attr(&local).is_some()))
    }

    pub fn closest(cx: &mut Cx<'_, '_, '_>, this: Obj, selector: String) -> R<Option<Obj>> {
        let s = crate::select::parse(&selector)?;
        let (focused, modals) = super::super::sel_ctx(cx);
        let ctx = crate::select::Ctx {
            focused,
            modals: &modals,
        };
        let dom = cx.dom();
        let mut cur = Some(this.node()?);
        while let Some(c) = cur {
            if s.matches(&dom, c, ctx) {
                return Ok(Some(Obj::Node(c)));
            }
            cur = dom.parent(c);
        }
        Ok(None)
    }

    pub fn matches(cx: &mut Cx<'_, '_, '_>, this: Obj, selector: String) -> R<bool> {
        let s = crate::select::parse(&selector)?;
        let (focused, modals) = super::super::sel_ctx(cx);
        let ctx = crate::select::Ctx {
            focused,
            modals: &modals,
        };
        Ok(s.matches(&cx.dom(), this.node()?, ctx))
    }

    pub fn webkit_matches_selector(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        selector: String,
    ) -> R<bool> {
        matches(cx, this, selector)
    }

    pub fn get_elements_by_tag_name(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Obj> {
        Ok(cx.alloc(
            Iface::HTMLCollection,
            Slot::Collection(Coll::Tag(this.node()?, name)),
        ))
    }

    pub fn get_elements_by_tag_name_ns(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        ns: Option<String>,
        local: String,
    ) -> R<Obj> {
        Ok(cx.alloc(
            Iface::HTMLCollection,
            Slot::Collection(Coll::TagNs(this.node()?, ns, local)),
        ))
    }

    pub fn get_elements_by_class_name(cx: &mut Cx<'_, '_, '_>, this: Obj, names: String) -> R<Obj> {
        let classes = names.split_ascii_whitespace().map(str::to_string).collect();
        Ok(cx.alloc(
            Iface::HTMLCollection,
            Slot::Collection(Coll::Class(this.node()?, classes)),
        ))
    }

    fn adjacent(cx: &Cx<'_, '_, '_>, id: NodeId, position: &str, node: NodeId) -> R<bool> {
        let dom = cx.dom();
        let (parent, before) = match position.to_ascii_lowercase().as_str() {
            "beforebegin" => (dom.parent(id), Some(id)),
            "afterbegin" => (Some(id), dom.first_child(id)),
            "beforeend" => (Some(id), None),
            "afterend" => (dom.parent(id), dom.next_sibling(id)),
            _ => {
                return Err(Error::dom(
                    "SyntaxError",
                    format!("'{position}' is not a valid position."),
                ));
            }
        };
        drop(dom);
        let Some(parent) = parent else {
            return Ok(false);
        };
        insert(cx, parent, node, before.filter(|&b| b != node))?;
        Ok(true)
    }

    pub fn insert_adjacent_element(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        position: String,
        el: Obj,
    ) -> R<Option<Obj>> {
        Ok(adjacent(cx, this.node()?, &position, el.node()?)?.then_some(el))
    }

    pub fn insert_adjacent_text(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        position: String,
        text: String,
    ) -> R<()> {
        let t = cx.dom_mut().create(NodeData::Text(text));
        adjacent(cx, this.node()?, &position, t).map(|_| ())
    }

    pub fn insert_adjacent_html(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        position: String,
        html: String,
    ) -> R<()> {
        let id = this.node()?;
        let context_el = match position.to_ascii_lowercase().as_str() {
            "beforebegin" | "afterend" => cx.dom().parent(id).unwrap_or(id),
            _ => id,
        };
        let (local, svg) = context_of(cx, context_el);
        let nodes = parse_fragment_into(cx, &local, svg, &html);
        let frag = cx.dom_mut().create(NodeData::DocumentFragment);
        for n in nodes {
            cx.dom_mut().append(frag, n).map_err(|_| gone())?;
        }
        adjacent(cx, id, &position, frag).map(|_| ())
    }

    fn context_of(cx: &Cx<'_, '_, '_>, id: NodeId) -> (String, bool) {
        let dom = cx.dom();
        let n = dom.get(id);
        let local = n.and_then(|n| n.local_name()).unwrap_or("body").to_string();
        let svg = n.is_some_and(|n| n.namespace() == Some(pageon_dom::Namespace::Svg));
        (local, svg)
    }

    pub fn get_inner_html(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = this.node()?;
        let dom = cx.dom();
        let holder = match dom.get(id).map(|n| &n.data) {
            Some(NodeData::Element {
                template_contents: Some(t),
                ..
            }) => *t,
            _ => id,
        };
        Ok(dom.children(holder).map(|c| dom.to_html(c)).collect())
    }

    pub fn set_inner_html(cx: &mut Cx<'_, '_, '_>, this: Obj, html: String) -> R<()> {
        let id = this.node()?;
        let holder = match cx.dom().get(id).map(|n| &n.data) {
            Some(NodeData::Element {
                template_contents: Some(t),
                ..
            }) => *t,
            _ => id,
        };
        let (local, svg) = context_of(cx, id);
        let nodes = parse_fragment_into(cx, &local, svg, &html);
        let kids: Vec<NodeId> = cx.dom().children(holder).collect();
        for k in kids {
            detach(cx, k)?;
        }
        for n in nodes {
            insert(cx, holder, n, None)?;
        }
        Ok(())
    }

    pub fn get_outer_html(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(cx.dom().to_html(this.node()?))
    }

    pub fn set_outer_html(cx: &mut Cx<'_, '_, '_>, this: Obj, html: String) -> R<()> {
        let id = this.node()?;
        let Some(parent) = cx.dom().parent(id) else {
            return Ok(());
        };
        if parent == cx.dom().root() {
            return Err(Error::dom(
                "NoModificationAllowedError",
                "The element's parent is the document.",
            ));
        }
        let (local, svg) = context_of(cx, parent);
        let nodes = parse_fragment_into(cx, &local, svg, &html);
        let next = cx.dom().next_sibling(id);
        detach(cx, id)?;
        for n in nodes {
            insert(cx, parent, n, next)?;
        }
        Ok(())
    }

    fn rect_obj(cx: &mut Cx<'_, '_, '_>, r: Option<pageon_layout::Rect>) -> Obj {
        let r = r.map_or([0.0; 4], |r| {
            [
                f64::from(r.x),
                f64::from(r.y),
                f64::from(r.width),
                f64::from(r.height),
            ]
        });
        cx.alloc(Iface::DOMRect, Slot::Rect(r))
    }

    /// `checkVisibility(options)` (CSSOM View): false without a box (`display: none`, or in
    /// such a subtree); then, if asked, false for an opacity of 0 on the element or an ancestor,
    /// or a `visibility` other than `visible`.
    pub fn check_visibility(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        o: crate::generated::CheckVisibilityOptions,
    ) -> R<bool> {
        let id = this.node()?;
        let layout = cx.realm.layout();
        if layout.visibility(id) == pageon_layout::Visibility::NotRendered {
            return Ok(false);
        }
        if o.check_opacity || o.opacity_property {
            let dom = cx.dom();
            let transparent = std::iter::once(id).chain(dom.ancestors(id)).any(|n| {
                layout
                    .style(n, "opacity")
                    .and_then(|v| v.parse::<f32>().ok())
                    .is_some_and(|v| v == 0.0)
            });
            if transparent {
                return Ok(false);
            }
        }
        if (o.check_visibility_css || o.visibility_property)
            && layout
                .style(id, "visibility")
                .is_some_and(|v| v != "visible")
        {
            return Ok(false);
        }
        Ok(true)
    }

    /// A promise resolved at once: every scroll is instant.
    fn done(cx: &mut Cx<'_, '_, '_>) -> R<Value> {
        let r = v8::PromiseResolver::new(cx.scope).ok_or(Error::Terminated)?;
        let undefined = v8::undefined(cx.scope).into();
        r.resolve(cx.scope, undefined);
        let p = r.get_promise(cx.scope);
        Ok(cx.keep(p.into()))
    }

    /// `scrollIntoView(arg)`: `true` (or nothing) aligns the top, `false` the bottom;
    /// options choose `block` and `inline` (`start`, `center`, `end`, `nearest`).
    pub fn scroll_into_view(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        arg: crate::cv::OrBool<crate::generated::ScrollIntoViewOptions>,
    ) -> R<Value> {
        let id = this.node()?;
        let (block, inline) = match &arg {
            crate::cv::OrBool::Bool(false) => ("end".to_string(), "nearest".to_string()),
            crate::cv::OrBool::Bool(true) => ("start".to_string(), "nearest".to_string()),
            crate::cv::OrBool::Dict(o) => (o.block.clone(), o.inline.clone()),
        };
        crate::dom::scroll::into_view(cx, id, &block, &inline, false);
        done(cx)
    }

    fn to(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        x: Option<f64>,
        y: Option<f64>,
        by: bool,
    ) -> R<Value> {
        let id = this.node()?;
        let s = crate::dom::scroll::scroller_of(cx, id);
        let (ox, oy) = crate::dom::scroll::offset(cx, s);
        let (base_x, base_y) = if by { (ox, oy) } else { (0.0, 0.0) };
        let tx = x.map_or(ox, |v| base_x + finite(v));
        let ty = y.map_or(oy, |v| base_y + finite(v));
        crate::dom::scroll::set(cx, s, tx, ty);
        done(cx)
    }

    pub fn scroll(cx: &mut Cx<'_, '_, '_>, this: Obj, o: ScrollToOptions) -> R<Value> {
        to(cx, this, o.left, o.top, false)
    }

    pub fn scroll_x_y(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64) -> R<Value> {
        to(cx, this, Some(x), Some(y), false)
    }

    pub fn scroll_to(cx: &mut Cx<'_, '_, '_>, this: Obj, o: ScrollToOptions) -> R<Value> {
        to(cx, this, o.left, o.top, false)
    }

    pub fn scroll_to_x_y(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64) -> R<Value> {
        to(cx, this, Some(x), Some(y), false)
    }

    pub fn scroll_by(cx: &mut Cx<'_, '_, '_>, this: Obj, o: ScrollToOptions) -> R<Value> {
        to(cx, this, o.left, o.top, true)
    }

    pub fn scroll_by_x_y(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64) -> R<Value> {
        to(cx, this, Some(x), Some(y), true)
    }

    pub fn get_bounding_client_rect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let r = rect(cx, this.node()?);
        Ok(rect_obj(cx, r))
    }

    pub fn get_client_rects(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let r = rect(cx, this.node()?).map(|r| {
            vec![[
                f64::from(r.x),
                f64::from(r.y),
                f64::from(r.width),
                f64::from(r.height),
            ]]
        });
        Ok(cx.alloc(Iface::DOMRectList, Slot::RectList(r.unwrap_or_default())))
    }

    fn is_root_element(cx: &Cx<'_, '_, '_>, id: NodeId) -> bool {
        let dom = cx.dom();
        dom.parent(id) == Some(dom.root())
    }

    fn viewport(cx: &Cx<'_, '_, '_>) -> (i32, i32) {
        let v = cx.st().viewport;
        (
            i32::try_from(v.width).unwrap_or(0),
            i32::try_from(v.height).unwrap_or(0),
        )
    }

    pub fn get_client_width(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        let id = this.node()?;
        if is_root_element(cx, id) {
            return Ok(viewport(cx).0);
        }
        let Some(r) = rect(cx, id) else { return Ok(0) };
        Ok(
            (f64::from(r.width)
                - px(cx, id, "border-left-width")
                - px(cx, id, "border-right-width"))
            .max(0.0) as i32,
        )
    }

    pub fn get_client_height(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        let id = this.node()?;
        if is_root_element(cx, id) {
            return Ok(viewport(cx).1);
        }
        let Some(r) = rect(cx, id) else { return Ok(0) };
        Ok((f64::from(r.height)
            - px(cx, id, "border-top-width")
            - px(cx, id, "border-bottom-width"))
        .max(0.0) as i32)
    }

    pub fn get_client_top(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        Ok(px(cx, this.node()?, "border-top-width") as i32)
    }

    pub fn get_client_left(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        Ok(px(cx, this.node()?, "border-left-width") as i32)
    }

    /// The content size: the client size plus how far it scrolls (the document's size for the
    /// document element).
    pub fn get_scroll_width(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        let id = this.node()?;
        let s = crate::dom::scroll::scroller_of(cx, id);
        let (mx, _) = crate::dom::scroll::range(cx, s);
        Ok(get_client_width(cx, this)? + mx.round() as i32)
    }

    pub fn get_scroll_height(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        let id = this.node()?;
        let s = crate::dom::scroll::scroller_of(cx, id);
        let (_, my) = crate::dom::scroll::range(cx, s);
        Ok(get_client_height(cx, this)? + my.round() as i32)
    }

    pub fn get_scroll_top(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let s = crate::dom::scroll::scroller_of(cx, this.node()?);
        Ok(crate::dom::scroll::offset(cx, s).1)
    }

    pub fn set_scroll_top(cx: &mut Cx<'_, '_, '_>, this: Obj, v: f64) -> R<()> {
        let s = crate::dom::scroll::scroller_of(cx, this.node()?);
        let (x, _) = crate::dom::scroll::offset(cx, s);
        crate::dom::scroll::set(cx, s, x, finite(v));
        Ok(())
    }

    pub fn get_scroll_left(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let s = crate::dom::scroll::scroller_of(cx, this.node()?);
        Ok(crate::dom::scroll::offset(cx, s).0)
    }

    pub fn set_scroll_left(cx: &mut Cx<'_, '_, '_>, this: Obj, v: f64) -> R<()> {
        let s = crate::dom::scroll::scroller_of(cx, this.node()?);
        let (_, y) = crate::dom::scroll::offset(cx, s);
        crate::dom::scroll::set(cx, s, finite(v), y);
        Ok(())
    }

    /// Non-finite scroll values are 0 (CSSOM View's "normalize non-finite values").
    fn finite(v: f64) -> f64 {
        if v.is_finite() { v } else { 0.0 }
    }
}

/// The `ElementCSSInlineStyle` mixin.
pub mod element_css_inline_style {
    use super::*;

    pub fn get_style(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "style",
            Iface::CSSStyleProperties,
            Slot::Style(id),
        ))
    }
}

/// `HTMLElement`.
pub mod html_element {
    use super::*;

    /// `attachInternals()`.
    pub fn attach_internals(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        crate::dom::internals::attach(cx, this.node()?)
    }

    pub fn get_inner_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(cx.dom().text(this.node()?))
    }

    pub fn set_inner_text(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        super::super::node::replace_with_text(cx, this.node()?, &v)
    }

    pub fn get_hidden(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Value>> {
        let v = cx
            .dom()
            .get(this.node()?)
            .and_then(|n| n.attr("hidden").map(str::to_string));
        let js: v8::Local<v8::Value> = match v {
            Some(s) if s.eq_ignore_ascii_case("until-found") => cx.str("until-found").into(),
            Some(_) => v8::Boolean::new(cx.scope, true).into(),
            None => v8::Boolean::new(cx.scope, false).into(),
        };
        Ok(Some(cx.keep(js)))
    }

    pub fn set_hidden(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<Value>) -> R<()> {
        let id = this.node()?;
        let on = v.is_some_and(|v| {
            let l = cx.local(&v);
            l.boolean_value(cx.scope)
        });
        if on {
            cx.dom_mut().set_attr(id, "hidden", "")
        } else {
            cx.dom_mut().remove_attr(id, "hidden")
        }
        .map_err(|_| gone())
    }

    pub fn click(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        crate::dom::event_target::click(cx, this.node()?)
    }

    /// The nearest positioned ancestor, or the body.
    fn offset_parent(cx: &Cx<'_, '_, '_>, id: NodeId) -> Option<NodeId> {
        let layout = cx.realm.layout();
        if layout.visibility(id) == pageon_layout::Visibility::NotRendered {
            return None;
        }
        let dom = cx.dom();
        let mut cur = dom.parent(id);
        while let Some(c) = cur {
            let Some(n) = dom.get(c) else { break };
            if n.is_html("body") {
                return Some(c);
            }
            if super::super::is_element(&dom, c) {
                let pos = layout.style(c, "position").unwrap_or_default();
                if pos != "static" && !pos.is_empty()
                    || n.is_html("td")
                    || n.is_html("th")
                    || n.is_html("table")
                {
                    return Some(c);
                }
            }
            cur = dom.parent(c);
        }
        None
    }

    pub fn get_offset_parent(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(offset_parent(cx, this.node()?).map(Obj::Node))
    }

    fn offset(cx: &Cx<'_, '_, '_>, id: NodeId) -> (f64, f64) {
        let Some(r) = rect(cx, id) else {
            return (0.0, 0.0);
        };
        match offset_parent(cx, id) {
            Some(p) if !cx.dom().get(p).is_some_and(|n| n.is_html("body")) => {
                let pr = rect(cx, p).unwrap_or(pageon_layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 0.0,
                });
                (
                    f64::from(r.x - pr.x) - px(cx, p, "border-left-width"),
                    f64::from(r.y - pr.y) - px(cx, p, "border-top-width"),
                )
            }
            _ => (f64::from(r.x), f64::from(r.y)),
        }
    }

    pub fn get_offset_top(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        Ok(offset(cx, this.node()?).1.round() as i32)
    }

    pub fn get_offset_left(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        Ok(offset(cx, this.node()?).0.round() as i32)
    }

    pub fn get_offset_width(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        Ok(rect(cx, this.node()?).map_or(0, |r| r.width.round() as i32))
    }

    pub fn get_offset_height(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        Ok(rect(cx, this.node()?).map_or(0, |r| r.height.round() as i32))
    }
}

/// The `HTMLOrSVGOrMathMLElement` mixin.
pub mod html_or_svg_or_math_ml_element {
    use super::*;

    pub fn get_dataset(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let id = this.node()?;
        Ok(super::super::same(
            cx,
            id,
            "dataset",
            Iface::DOMStringMap,
            Slot::DataSet(id),
        ))
    }

    /// Whether an element can take focus without script.
    pub(crate) fn focusable(cx: &Cx<'_, '_, '_>, id: NodeId) -> bool {
        let dom = cx.dom();
        let Some(n) = dom.get(id) else { return false };
        if n.attr("tabindex").and_then(crate::cv::parse_int).is_some() {
            return true;
        }
        // An editing host takes focus.
        if crate::dom::editing::editing_host(&dom, id) == Some(id) {
            return true;
        }
        ((n.is_html("a") || n.is_html("area")) && n.attr("href").is_some())
            || (["button", "input", "select", "textarea", "iframe", "summary"]
                .iter()
                .any(|t| n.is_html(t))
                && n.attr("disabled").is_none()
                && !n
                    .attr("type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("hidden")))
    }

    pub fn get_tab_index(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        let id = this.node()?;
        let v = cx
            .dom()
            .get(id)
            .and_then(|n| n.attr("tabindex"))
            .and_then(crate::cv::parse_int);
        Ok(v.map_or(if focusable(cx, id) { 0 } else { -1 }, |v| {
            v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        }))
    }

    pub fn set_tab_index(cx: &mut Cx<'_, '_, '_>, this: Obj, v: i32) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "tabindex", &v.to_string())
            .map_err(|_| gone())
    }

    pub fn focus(cx: &mut Cx<'_, '_, '_>, this: Obj, _o: FocusOptions) -> R<()> {
        let mut id = this.node()?;
        // `delegatesFocus`: the host focuses the first focusable element of its shadow tree.
        let delegate = {
            let dom = cx.dom();
            dom.shadow_root(id).filter(|&r| {
                matches!(
                    dom.get(r).map(|n| &n.data),
                    Some(NodeData::ShadowRoot {
                        delegates_focus: true,
                        ..
                    })
                )
            })
        };
        if let Some(root) = delegate {
            let inner: Vec<NodeId> = cx
                .dom()
                .traverse(root)
                .filter_map(|e| match e {
                    pageon_dom::Edge::Enter(n) if n != root => Some(n),
                    _ => None,
                })
                .collect();
            if let Some(f) = inner.into_iter().find(|&n| focusable(cx, n)) {
                id = f;
            }
        }
        if !focusable(cx, id) {
            return Ok(());
        }
        crate::dom::event_target::move_focus(cx, Some(id))
    }

    pub fn blur(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = this.node()?;
        if cx.st().focused == Some(id) {
            crate::dom::event_target::move_focus(cx, None)?;
        }
        Ok(())
    }
}

/// `SVGGraphicsElement`.
pub mod svg_graphics_element {
    use super::*;

    /// The element's box (pageon lays out SVG as boxes; user-space geometry is not modelled).
    pub fn get_b_box(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        _o: crate::generated::SVGBoundingBoxOptions,
    ) -> R<Obj> {
        let r = rect(cx, this.node()?).map_or([0.0; 4], |r| {
            [0.0, 0.0, f64::from(r.width), f64::from(r.height)]
        });
        Ok(cx.alloc(Iface::DOMRect, Slot::Rect(r)))
    }
}
