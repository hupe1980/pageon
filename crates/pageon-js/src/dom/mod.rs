//! Hand-written implementations of the allowlisted members, grouped by area. Each submodule is
//! named after the interface or mixin that declares the members (the generated code calls
//! `crate::dom::<owner>::<member>`).

pub mod blobs;
mod canvas;
mod collections;
pub(crate) mod custom_elements;
pub mod dnd;
mod documents;
mod elements;
mod events;
mod globals;
mod html;
pub(crate) mod input;
pub(crate) mod internals;
pub(crate) mod intersection;
pub(crate) mod net;
mod nodes;
pub(crate) mod observers;
pub(crate) mod platform;
pub mod realtime;
pub(crate) mod scroll;
pub(crate) mod session;
pub(crate) mod shadow;
pub(crate) mod sheets;
pub(crate) mod traversal;
pub(crate) mod urls;
pub(crate) mod validation;
pub(crate) mod webmcp;
mod webstorage;
mod xml;

pub use crate::frames::{htmli_frame_element, message_event};
pub use canvas::{
    canvas_compositing, canvas_draw_path, canvas_fill_stroke_styles, canvas_path,
    canvas_path_drawing_styles, canvas_rect, canvas_rendering_context2_d, canvas_state,
    canvas_transform, html_canvas_element,
};
pub use collections::{
    attr, css_style_declaration, css_style_properties, dom_rect_list, dom_rect_read_only,
    dom_token_list, html_collection, named_node_map, node_list,
};
pub use custom_elements::custom_element_registry;
pub use documents::{
    document, document_or_shadow_root, document_type, dom_implementation, non_element_parent_node,
};
pub use elements::{
    element, element_css_inline_style, html_element, html_or_svg_or_math_ml_element,
    svg_graphics_element,
};
pub use events::{
    custom_event, drag_event, event, event_target, focus_event, keyboard_event, mouse_event,
    pointer_event, ui_event, wheel_event,
};
pub use globals::{
    animation_frame_provider, console, dom_exception, location, navigator_automation_information,
    navigator_cookies, navigator_id, navigator_language, navigator_on_line, navigator_ua,
    navigator_ua_data, performance, visual_viewport, window, window_or_worker_global_scope,
};
pub use html::{
    html_button_element, html_dialog_element, html_form_element, html_hyperlink_element_utils,
    html_input_element, html_option_element, html_script_element, html_select_element,
    html_template_element, html_text_area_element, hyperlink_element_utils,
};
pub use intersection::{intersection_observer, intersection_observer_entry};
pub use nodes::{
    character_data, child_node, comment, document_fragment, node, non_document_type_child_node,
    parent_node, text,
};
pub use observers::{mutation_observer, mutation_record};
pub use platform::{
    crypto, html_image_element, idle_deadline, media_query_list, media_query_list_event,
    resize_observer, resize_observer_entry, resize_observer_size, text_decoder,
    text_decoder_common, text_encoder, text_encoder_common,
};
pub use session::{history, pop_state_event};
pub use shadow::{html_slot_element, shadow_root, slottable};
pub use traversal::tree_walker;
pub use xml::xml_serializer;

pub use blobs::{blob, file, file_list, file_reader};
pub use dnd::{data_transfer, data_transfer_item, data_transfer_item_list};
pub use internals::{aria_mixin, custom_state_set, element_internals};
pub use net::{
    abort_controller, abort_signal, body, headers, progress_event, request, response,
    xml_http_request,
};
pub use realtime::{close_event, event_source, web_socket};
pub use sheets::css_style_sheet;
pub use urls::{form_data, url, url_search_params};
pub use validation::{html_field_set_element, html_output_element, validity_state};
pub use webmcp::model_context;
pub use webstorage::{storage, window_local_storage, window_session_storage};

pub mod hooks;

pub(crate) use canvas::Canvases;

use html5ever::{LocalName, QualName, ns};
use pageon_dom::{Dom, Edge, NodeData, NodeId, ParseOptions};

use crate::cv::{Cx, Error, Obj, R};
use crate::generated::Iface;
use crate::realm::{Coll, Slot};

/// Resolves a URL against the document URL.
pub fn resolve_url(cx: &Cx<'_, '_, '_>, s: &str) -> Option<String> {
    let base = ::url::Url::parse(&cx.realm.st.borrow().url).ok()?;
    base.join(s.trim()).ok().map(|u| u.to_string())
}

/// JavaScript's `Number::toString` for the values reflection produces.
pub fn number_to_string(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// `NotFoundError`.
pub(crate) fn gone() -> Error {
    Error::dom("NotFoundError", "The node no longer exists.")
}

/// `HierarchyRequestError`.
pub(crate) fn hierarchy(msg: &str) -> Error {
    Error::dom("HierarchyRequestError", msg.to_string())
}

pub(crate) fn is_element(dom: &Dom, id: NodeId) -> bool {
    dom.get(id)
        .is_some_and(|n| matches!(n.data, NodeData::Element { .. }))
}

pub(crate) fn element_children(dom: &Dom, id: NodeId) -> Vec<NodeId> {
    dom.children(id).filter(|&c| is_element(dom, c)).collect()
}

/// Concatenated descendant text (`textContent`).
pub(crate) fn text_content(dom: &Dom, id: NodeId) -> String {
    let mut s = String::new();
    for e in dom.traverse(id) {
        if let Edge::Enter(n) = e
            && let Some(NodeData::Text(t)) = dom.get(n).map(|x| &x.data)
        {
            s.push_str(t);
        }
    }
    s
}

/// A new element in the HTML namespace (or `ns`), with its template contents.
pub(crate) fn create_element(
    dom: &mut Dom,
    namespace: html5ever::Namespace,
    local: &str,
) -> NodeId {
    let is_template = namespace == ns!(html) && local == "template";
    let template_contents = is_template.then(|| dom.create(NodeData::DocumentFragment));
    dom.create(NodeData::Element {
        name: QualName::new(None, namespace, LocalName::from(local)),
        attrs: Vec::new(),
        template_contents,
        mathml_annotation_xml_integration_point: false,
    })
}

/// A deep copy of `node` (from `src`) into `dst`, detached.
pub(crate) fn import(dst: &mut Dom, src: &Dom, node: NodeId) -> Option<NodeId> {
    let data = match &src.get(node)?.data {
        NodeData::Element {
            name,
            attrs,
            template_contents,
            ..
        } => {
            let tc = template_contents.and_then(|t| import(dst, src, t));
            NodeData::Element {
                name: name.clone(),
                attrs: attrs.clone(),
                template_contents: tc,
                mathml_annotation_xml_integration_point: false,
            }
        }
        other => other.clone(),
    };
    let copy = dst.create(data);
    for c in src.children(node).collect::<Vec<_>>() {
        if let Some(cc) = import(dst, src, c) {
            let _ = dst.append(copy, cc);
        }
    }
    Some(copy)
}

/// A copy of `node` within the same document (with its subtree when `deep`), detached.
/// Copies a node (and its subtree when `deep`) from another document into `dst`.
pub(crate) fn copy_between(src: &Dom, node: NodeId, dst: &mut Dom, deep: bool) -> Option<NodeId> {
    let data = match src.get(node)?.data.clone() {
        NodeData::Element {
            name,
            attrs,
            template_contents,
            ..
        } => {
            let tc = template_contents
                .filter(|_| deep)
                .and_then(|t| copy_between(src, t, dst, true));
            NodeData::Element {
                name,
                attrs,
                template_contents: tc,
                mathml_annotation_xml_integration_point: false,
            }
        }
        other => other,
    };
    let copy = dst.create(data);
    if let Some(s) = src.control(node).cloned() {
        *dst.control_mut(copy) = s;
    }
    if deep {
        for c in src.children(node) {
            if let Some(cc) = copy_between(src, c, dst, true) {
                let _ = dst.append(copy, cc);
            }
        }
    }
    Some(copy)
}

pub(crate) fn copy_within(dom: &mut Dom, node: NodeId, deep: bool) -> Option<NodeId> {
    let data = match dom.get(node)?.data.clone() {
        NodeData::Element {
            name,
            attrs,
            template_contents,
            ..
        } => {
            let tc = template_contents
                .filter(|_| deep)
                .and_then(|t| copy_within(dom, t, true));
            NodeData::Element {
                name,
                attrs,
                template_contents: tc,
                mathml_annotation_xml_integration_point: false,
            }
        }
        other => other,
    };
    let copy = dom.create(data);
    if deep {
        for c in dom.children(node).collect::<Vec<_>>() {
            if let Some(cc) = copy_within(dom, c, true) {
                let _ = dom.append(copy, cc);
            }
        }
    }
    Some(copy)
}

/// Parses `html` as `innerHTML` of an element with local name `context`; the new nodes are
/// detached in the realm's document, in order. Scripts in them never run.
pub(crate) fn parse_fragment_into(
    cx: &Cx<'_, '_, '_>,
    context: &str,
    svg: bool,
    html: &str,
) -> Vec<NodeId> {
    let ctx = if svg {
        format!("svg {context}")
    } else {
        context.to_string()
    };
    let frag = pageon_dom::parse_fragment(html, &ctx, ParseOptions { scripting: true });
    let Some(holder) = frag.children(frag.root()).find(|&c| is_element(&frag, c)) else {
        return Vec::new();
    };
    let mut dom = cx.dom_mut();
    let mut out = Vec::new();
    for c in frag.children(holder).collect::<Vec<_>>() {
        if let Some(n) = import(&mut dom, &frag, c) {
            out.push(n);
        }
    }
    drop(dom);
    mark_scripts_started(cx, &out);
    out
}

/// A clone keeps its scripts' "already started" flag (HTML's cloning steps), so a script that
/// `innerHTML` made inert stays inert when cloned and inserted. `copy` is a copy of `original`.
pub(crate) fn copy_started(cx: &Cx<'_, '_, '_>, original: NodeId, copy: NodeId) {
    let dom = cx.dom();
    let enter = |r| {
        dom.traverse(r).filter_map(|e| match e {
            Edge::Enter(n) => Some(n),
            _ => None,
        })
    };
    let pairs: Vec<(NodeId, NodeId)> = enter(original).zip(enter(copy)).collect();
    drop(dom);
    let mut st = cx.st();
    let started: Vec<NodeId> = pairs
        .into_iter()
        .filter(|(o, _)| st.started_scripts.contains(o))
        .map(|(_, c)| c)
        .collect();
    st.started_scripts.extend(started);
}

/// Scripts inserted through markup never run: mark them as already started.
fn mark_scripts_started(cx: &Cx<'_, '_, '_>, roots: &[NodeId]) {
    let dom = cx.dom();
    let mut started = Vec::new();
    for &r in roots {
        for e in dom.traverse(r) {
            if let Edge::Enter(n) = e
                && dom.get(n).is_some_and(|x| x.is_html("script"))
            {
                started.push(n);
            }
        }
    }
    drop(dom);
    cx.st().started_scripts.extend(started);
}

/// The node kinds that may be inserted, and where (DOM's pre-insertion validity, simplified to
/// what can be represented).
pub(crate) fn check_insert(
    dom: &Dom,
    parent: NodeId,
    node: NodeId,
    before: Option<NodeId>,
) -> Result<(), Error> {
    let p = dom.get(parent).ok_or_else(gone)?;
    if !matches!(
        p.data,
        NodeData::Document
            | NodeData::DocumentFragment
            | NodeData::ShadowRoot { .. }
            | NodeData::Element { .. }
    ) {
        return Err(hierarchy("This node type does not support this method."));
    }
    let n = dom.get(node).ok_or_else(gone)?;
    if dom.is_inclusive_ancestor(node, parent) {
        return Err(hierarchy("The new child element contains the parent."));
    }
    if let Some(b) = before
        && dom.parent(b) != Some(parent)
    {
        return Err(Error::dom(
            "NotFoundError",
            "The node before which the new node is to be inserted is not a child of this node.",
        ));
    }
    match (&p.data, &n.data) {
        (_, NodeData::Document) => Err(hierarchy("Nodes of type '#document' may not be inserted.")),
        (_, NodeData::ShadowRoot { .. }) => Err(hierarchy("A shadow root may not be inserted.")),
        (NodeData::Document, NodeData::Text(_)) => Err(hierarchy(
            "Nodes of type '#text' may not be inserted inside nodes of type '#document'.",
        )),
        (NodeData::Document, NodeData::Element { .. })
            if element_children(dom, parent).iter().any(|&c| c != node) =>
        {
            Err(hierarchy("Only one element on document allowed."))
        }
        (
            NodeData::Element { .. } | NodeData::DocumentFragment | NodeData::ShadowRoot { .. },
            NodeData::Doctype { .. },
        ) => Err(hierarchy(
            "Nodes of type '#doctype' may only be inserted into a document.",
        )),
        _ => Ok(()),
    }
}

/// Inserts `node` (a fragment's children, for fragments) into `parent` before `before`.
pub(crate) fn insert(
    cx: &Cx<'_, '_, '_>,
    parent: NodeId,
    node: NodeId,
    before: Option<NodeId>,
) -> R<()> {
    check_insert(&cx.dom(), parent, node, before)?;
    let nodes: Vec<NodeId> = {
        let dom = cx.dom();
        if matches!(
            dom.get(node).map(|n| &n.data),
            Some(NodeData::DocumentFragment)
        ) {
            dom.children(node).collect()
        } else {
            vec![node]
        }
    };
    let iframes = {
        let dom = cx.dom();
        nodes.iter().any(|&n| {
            dom.traverse(n).any(
                |e| matches!(e, Edge::Enter(x) if dom.get(x).is_some_and(|y| y.is_html("iframe"))),
            )
        })
    };
    let mut dom = cx.dom_mut();
    for &n in &nodes {
        let r = match before {
            Some(b) if b != n => dom.insert_before(parent, n, b),
            Some(_) => Ok(()),
            None => dom.append(parent, n),
        };
        r.map_err(|e| hierarchy(&e.to_string()))?;
    }
    drop(dom);
    if iframes {
        cx.st().frames_changed = true;
    }
    crate::modules::inserted(cx, &nodes);
    Ok(())
}

/// Removes `node` from its parent (it stays alive for scripts that hold it).
pub(crate) fn detach(cx: &Cx<'_, '_, '_>, node: NodeId) -> R<()> {
    if cx.dom().parent(node).is_none() {
        return Ok(());
    }
    cx.dom_mut().detach(node).map_err(|_| gone())?;
    let mut st = cx.st();
    if st.focused.is_some_and(|f| {
        !cx.realm
            .dom
            .borrow()
            .is_inclusive_ancestor(cx.realm.dom.borrow().root(), f)
    }) {
        st.focused = None;
    }
    Ok(())
}

/// Converts `(Node or DOMString)…` into one node (a fragment for several), as DOM's
/// "convert nodes into a node".
pub(crate) fn nodes_into_node(
    cx: &Cx<'_, '_, '_>,
    items: Vec<crate::cv::NodeOrString>,
) -> R<Option<NodeId>> {
    let mut ids = Vec::new();
    for i in items {
        match i {
            crate::cv::NodeOrString::Node(o) => ids.push(o.node()?),
            crate::cv::NodeOrString::Str(s) => ids.push(cx.dom_mut().create(NodeData::Text(s))),
        }
    }
    if ids.len() <= 1 {
        return Ok(ids.pop());
    }
    let mut dom = cx.dom_mut();
    let frag = dom.create(NodeData::DocumentFragment);
    for i in ids {
        dom.append(frag, i).map_err(|e| hierarchy(&e.to_string()))?;
    }
    Ok(Some(frag))
}

/// The `[SameObject]` object for (node, key), created on first use.
pub(crate) fn same(
    cx: &mut Cx<'_, '_, '_>,
    node: NodeId,
    key: &'static str,
    iface: Iface,
    slot: Slot,
) -> Obj {
    if let Some(&id) = cx.st().same.get(&(node, key)) {
        return Obj::Slab(id);
    }
    let id = cx.st().alloc(iface, slot);
    cx.st().same.insert((node, key), id);
    Obj::Slab(id)
}

/// The window-level `[SameObject]` object for `key`.
pub(crate) fn window_same(
    cx: &mut Cx<'_, '_, '_>,
    key: &'static str,
    iface: Iface,
    slot: Slot,
) -> Obj {
    if let Some(&id) = cx.st().window_same.get(key) {
        return Obj::Slab(id);
    }
    let id = cx.st().alloc(iface, slot);
    cx.st().window_same.insert(key, id);
    Obj::Slab(id)
}

/// The items of a live collection, evaluated now.
pub(crate) fn collect(dom: &Dom, c: &Coll) -> Vec<NodeId> {
    let descendants = |root: NodeId| -> Vec<NodeId> {
        dom.traverse(root)
            .filter_map(|e| match e {
                Edge::Enter(n) if n != root && is_element(dom, n) => Some(n),
                _ => None,
            })
            .collect()
    };
    let html = |n: NodeId, t: &str| dom.get(n).is_some_and(|x| x.is_html(t));
    match c {
        Coll::Tag(root, name) => {
            let lower = name.to_ascii_lowercase();
            descendants(*root)
                .into_iter()
                .filter(|&n| {
                    name == "*"
                        || dom.get(n).is_some_and(|x| {
                            let local = x.local_name().unwrap_or_default();
                            if x.namespace() == Some(pageon_dom::Namespace::Html) {
                                local == lower
                            } else {
                                local == name
                            }
                        })
                })
                .collect()
        }
        Coll::TagNs(root, ns, local) => descendants(*root)
            .into_iter()
            .filter(|&n| {
                dom.get(n).is_some_and(|x| {
                    let ns_ok = match ns.as_deref() {
                        Some("*") => true,
                        Some(u) => namespace_url(x.namespace()) == u,
                        None => x.namespace().is_none(),
                    };
                    ns_ok && (local == "*" || x.local_name() == Some(local.as_str()))
                })
            })
            .collect(),
        Coll::Class(root, classes) => {
            if classes.is_empty() {
                return Vec::new();
            }
            descendants(*root)
                .into_iter()
                .filter(|&n| {
                    let have: Vec<&str> = dom
                        .get(n)
                        .and_then(|x| x.attr("class"))
                        .unwrap_or_default()
                        .split_ascii_whitespace()
                        .collect();
                    classes.iter().all(|c| have.contains(&c.as_str()))
                })
                .collect()
        }
        Coll::Name(name) => descendants(dom.root())
            .into_iter()
            .filter(|&n| dom.get(n).and_then(|x| x.attr("name")) == Some(name.as_str()))
            .collect(),
        Coll::Children(p) => element_children(dom, *p),
        Coll::Options(sel) => descendants(*sel)
            .into_iter()
            .filter(|&n| html(n, "option"))
            .collect(),
        Coll::FormElements(form) => pageon_dom::forms::listed(dom, *form)
            .into_iter()
            .filter(|&n| !html(n, "input") || pageon_dom::forms::input_type(dom, n) != "image")
            .collect(),
        Coll::Doc(which) => descendants(dom.root())
            .into_iter()
            .filter(|&n| match *which {
                "images" => html(n, "img"),
                "links" => {
                    (html(n, "a") || html(n, "area"))
                        && dom.get(n).is_some_and(|x| x.attr("href").is_some())
                }
                "forms" => html(n, "form"),
                "scripts" => html(n, "script"),
                _ => false,
            })
            .collect(),
    }
}

/// The namespace URL of a pageon namespace.
pub(crate) fn namespace_url(ns: Option<pageon_dom::Namespace>) -> &'static str {
    match ns {
        Some(pageon_dom::Namespace::Html) => "http://www.w3.org/1999/xhtml",
        Some(pageon_dom::Namespace::Svg) => "http://www.w3.org/2000/svg",
        Some(pageon_dom::Namespace::MathMl) => "http://www.w3.org/1998/Math/MathML",
        _ => "",
    }
}

/// The selector matching context of a realm.
pub(crate) fn sel_ctx(cx: &Cx<'_, '_, '_>) -> (Option<NodeId>, Vec<NodeId>) {
    let st = cx.realm.st.borrow();
    (st.focused, st.modals.clone())
}

/// The left edge of an element's box (0 without a box).
pub(crate) fn elements_rect_x(cx: &Cx<'_, '_, '_>, n: NodeId) -> f64 {
    cx.realm
        .layout()
        .client_rect(n)
        .map_or(0.0, |r| f64::from(r.x))
}

/// The top edge of an element's box (0 without a box).
pub(crate) fn elements_rect_y(cx: &Cx<'_, '_, '_>, n: NodeId) -> f64 {
    cx.realm
        .layout()
        .client_rect(n)
        .map_or(0.0, |r| f64::from(r.y))
}
