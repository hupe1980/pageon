//! Document, DocumentType and the document mixins.

use html5ever::ns;
use pageon_dom::{NodeData, NodeId};

use super::{create_element, element_children, gone, insert, text_content};
use crate::cv::{Cx, Error, Obj, R};
use crate::generated::Iface;
use crate::realm::{Coll, EventData, Slot};

fn root_element(cx: &Cx<'_, '_, '_>) -> Option<NodeId> {
    let root = cx.dom().root();
    root_element_of(cx, root)
}

fn html_child(cx: &Cx<'_, '_, '_>, tag: &str) -> Option<NodeId> {
    let root = cx.dom().root();
    html_child_of(cx, root, tag)
}

/// The document `this` is: a document `DOMParser` made, else the realm's own.
pub(crate) fn doc_of(cx: &Cx<'_, '_, '_>, this: Obj) -> NodeId {
    let dom = cx.dom();
    match this {
        Obj::Node(id) if matches!(dom.get(id).map(|n| &n.data), Some(NodeData::Document)) => id,
        _ => dom.root(),
    }
}

fn root_element_of(cx: &Cx<'_, '_, '_>, doc: NodeId) -> Option<NodeId> {
    let dom = cx.dom();
    element_children(&dom, doc).first().copied()
}

fn html_child_of(cx: &Cx<'_, '_, '_>, doc: NodeId, tag: &str) -> Option<NodeId> {
    let root = root_element_of(cx, doc)?;
    let dom = cx.dom();
    element_children(&dom, root)
        .into_iter()
        .find(|&c| dom.get(c).is_some_and(|n| n.is_html(tag)))
}

fn valid_name(name: &str) -> R<()> {
    let mut chars = name.chars();
    let ok = chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == ':')
        && name
            .chars()
            .all(|c| !c.is_whitespace() && !matches!(c, '<' | '>' | '"' | '\'' | '/' | '=' | '\0'));
    if ok {
        Ok(())
    } else {
        Err(Error::dom(
            "InvalidCharacterError",
            format!("The tag name provided ('{name}') is not a valid name."),
        ))
    }
}

/// `Document`.
pub mod document {
    use super::*;

    /// `document.elementFromPoint(x, y)`: the element painted at a viewport point (`null`
    /// outside the viewport).
    /// The document element: it scrolls the viewport (standards mode).
    /// `hidden`: an agent's tab is the visible one.
    pub fn get_hidden(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(false)
    }

    /// `visibilityState`: `"visible"` (see `hidden`).
    pub fn get_visibility_state(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("visible".into())
    }

    pub fn get_scrolling_element(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(root_element(cx).map(Obj::Node))
    }

    pub fn element_from_point(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        x: f64,
        y: f64,
    ) -> R<Option<Obj>> {
        let v = cx.st().viewport;
        if x < 0.0 || y < 0.0 || x > f64::from(v.width) || y > f64::from(v.height) {
            return Ok(None);
        }
        Ok(cx
            .realm
            .layout()
            .element_at(x as f32, y as f32)
            .map(Obj::Node))
    }

    /// `document.elementsFromPoint(x, y)`: the element at the point, then its ancestors (the
    /// boxes that contain the point in paint order, for layouts without overlapping siblings).
    pub fn elements_from_point(cx: &mut Cx<'_, '_, '_>, this: Obj, x: f64, y: f64) -> R<Vec<Obj>> {
        let Some(Obj::Node(top)) = element_from_point(cx, this, x, y)? else {
            return Ok(Vec::new());
        };
        let dom = cx.dom();
        let mut out = vec![Obj::Node(top)];
        out.extend(
            dom.ancestors(top)
                .filter(|&a| dom.get(a).is_some_and(|n| n.local_name().is_some()))
                .map(Obj::Node),
        );
        Ok(out)
    }

    /// `document.createTreeWalker(root, whatToShow, filter)`.
    pub fn create_tree_walker(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        root: Obj,
        what_to_show: u32,
        filter: Option<crate::cv::Value>,
    ) -> R<Obj> {
        crate::dom::traversal::create(cx, root, what_to_show, filter)
    }

    pub fn create_node_iterator(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        root: Obj,
        what_to_show: u32,
        filter: Option<crate::cv::Value>,
    ) -> R<Obj> {
        crate::dom::traversal::create_iterator(cx, root, what_to_show, filter)
    }

    /// `document.cookie`: the cookies of the document's URL that script may see (not
    /// `HttpOnly`), at the virtual time.
    pub fn get_cookie(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        let now = cx.realm.shared.borrow().now as u64;
        let st = cx.realm.st.borrow();
        let r = st.net.borrow().document_cookie(&st.client, now);
        Ok(r)
    }

    /// `document.cookie = "…"`: stored as if from a response, but never `HttpOnly`.
    pub fn set_cookie(cx: &mut Cx<'_, '_, '_>, _this: Obj, v: String) -> R<()> {
        let now = cx.realm.shared.borrow().now as u64;
        let st = cx.realm.st.borrow();
        st.net.borrow_mut().set_document_cookie(&st.client, &v, now);
        Ok(())
    }

    pub fn get_url(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().url.clone())
    }

    pub fn get_document_uri(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get_url(cx, this)
    }

    pub fn get_compat_mode(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(if cx.dom().quirks_mode == pageon_dom::QuirksMode::Quirks {
            "BackCompat"
        } else {
            "CSS1Compat"
        }
        .into())
    }

    pub fn get_character_set(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("UTF-8".into())
    }

    pub fn get_charset(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get_character_set(cx, this)
    }

    pub fn get_input_encoding(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get_character_set(cx, this)
    }

    pub fn get_content_type(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("text/html".into())
    }

    pub fn get_doctype(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let doc = doc_of(cx, this);
        let dom = cx.dom();
        Ok(dom
            .children(doc)
            .find(|&c| matches!(dom.get(c).map(|n| &n.data), Some(NodeData::Doctype { .. })))
            .map(Obj::Node))
    }

    pub fn get_document_element(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let doc = doc_of(cx, this);
        Ok(root_element_of(cx, doc).map(Obj::Node))
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

    pub fn create_element(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        local: String,
        _options: String,
    ) -> R<Obj> {
        valid_name(&local)?;
        let local = local.to_ascii_lowercase();
        let el = super::create_element(&mut cx.dom_mut(), ns!(html), &local);
        // A defined custom element is constructed now.
        crate::dom::custom_elements::created(cx, el)?;
        Ok(Obj::Node(el))
    }

    pub fn create_element_ns(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        ns: Option<String>,
        qname: String,
        _options: String,
    ) -> R<Obj> {
        valid_name(&qname)?;
        let local = qname.rsplit(':').next().unwrap_or(&qname).to_string();
        let namespace = match ns.as_deref() {
            Some("http://www.w3.org/1999/xhtml") => ns!(html),
            Some("http://www.w3.org/2000/svg") => ns!(svg),
            Some("http://www.w3.org/1998/Math/MathML") => ns!(mathml),
            Some(other) => html5ever::Namespace::from(other),
            None => ns!(),
        };
        Ok(Obj::Node(super::create_element(
            &mut cx.dom_mut(),
            namespace,
            &local,
        )))
    }

    pub fn create_document_fragment(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(Obj::Node(cx.dom_mut().create(NodeData::DocumentFragment)))
    }

    pub fn create_text_node(cx: &mut Cx<'_, '_, '_>, _this: Obj, data: String) -> R<Obj> {
        Ok(Obj::Node(cx.dom_mut().create(NodeData::Text(data))))
    }

    pub fn create_comment(cx: &mut Cx<'_, '_, '_>, _this: Obj, data: String) -> R<Obj> {
        Ok(Obj::Node(cx.dom_mut().create(NodeData::Comment(data))))
    }

    /// A copy of `node` for this document (`deep`: with its subtree). pageon's documents share
    /// one arena per realm, so this is `cloneNode`.
    pub fn import_node(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        node: Obj,
        deep: crate::cv::OrBool<crate::generated::ImportNodeOptions>,
    ) -> R<Obj> {
        if let Obj::Foreign(realm, id) = node {
            // A node of another document of this engine (a same-origin frame): copied in.
            let deep = match deep {
                crate::cv::OrBool::Bool(b) => b,
                crate::cv::OrBool::Dict(_) => true,
            };
            let src = crate::frames::foreign_dom(cx, realm)
                .ok_or_else(|| Error::dom("NotSupportedError", crate::cv::FOREIGN))?;
            let src = src.borrow();
            if matches!(
                src.get(id).map(|n| &n.data),
                None | Some(NodeData::Document | NodeData::ShadowRoot { .. })
            ) {
                return Err(Error::dom(
                    "NotSupportedError",
                    "The node provided is a document or a shadow root, which may not be imported.",
                ));
            }
            let copy = super::super::copy_between(&src, id, &mut cx.dom_mut(), deep);
            return copy
                .map(Obj::Node)
                .ok_or_else(|| Error::dom("NotFoundError", "the node is gone"));
        }
        let id = node.node()?;
        if matches!(
            cx.dom().get(id).map(|n| &n.data),
            Some(NodeData::Document | NodeData::ShadowRoot { .. })
        ) {
            return Err(Error::dom(
                "NotSupportedError",
                "The node provided is a document or a shadow root, which may not be imported.",
            ));
        }
        let deep = match deep {
            crate::cv::OrBool::Bool(b) => b,
            crate::cv::OrBool::Dict(_) => true,
        };
        crate::dom::node::clone_node(cx, node, deep)
    }

    pub fn adopt_node(cx: &mut Cx<'_, '_, '_>, _this: Obj, node: Obj) -> R<Obj> {
        let id = node.node()?;
        if matches!(cx.dom().get(id).map(|n| &n.data), Some(NodeData::Document)) {
            return Err(Error::dom(
                "NotSupportedError",
                "The node provided is a document, which may not be adopted.",
            ));
        }
        super::super::detach(cx, id)?;
        Ok(node)
    }

    pub fn create_event(cx: &mut Cx<'_, '_, '_>, _this: Obj, interface: String) -> R<Obj> {
        let iface = match interface.to_ascii_lowercase().as_str() {
            "event" | "events" | "htmlevents" | "svgevents" => Iface::Event,
            "customevent" => Iface::CustomEvent,
            "uievent" | "uievents" => Iface::UIEvent,
            "mouseevent" | "mouseevents" => Iface::MouseEvent,
            "keyboardevent" => Iface::KeyboardEvent,
            "focusevent" => Iface::FocusEvent,
            _ => {
                return Err(Error::dom(
                    "NotSupportedError",
                    format!("The provided event type ('{interface}') is invalid."),
                ));
            }
        };
        let time = {
            let sh = cx.realm.shared.borrow();
            sh.now - sh.origin
        };
        Ok(cx.alloc(
            iface,
            Slot::Event(Box::new(EventData {
                time,
                ..EventData::default()
            })),
        ))
    }

    pub fn get_location(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(Some(super::super::window::get_location(cx, Obj::Window)?))
    }

    pub fn get_domain(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        let url = cx.st().url.clone();
        Ok(url::Url::parse(&url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default())
    }

    pub fn set_domain(_cx: &mut Cx<'_, '_, '_>, _this: Obj, _v: String) -> R<()> {
        Err(Error::dom(
            "SecurityError",
            "Setting document.domain is not supported.",
        ))
    }

    pub fn get_referrer(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(String::new())
    }

    pub fn get_last_modified(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        let now = cx.realm.shared.borrow().origin;
        Ok(format_date(now))
    }

    pub fn get_ready_state(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().ready_state.to_string())
    }

    pub fn get_title(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let doc = doc_of(cx, this);
        let dom = cx.dom();
        let title = dom.traverse(doc).find_map(|e| match e {
            pageon_dom::Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("title")) => Some(n),
            _ => None,
        });
        Ok(title
            .map(|t| {
                text_content(&dom, t)
                    .split_ascii_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default())
    }

    pub fn set_title(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let doc = doc_of(cx, this);
        let existing = {
            let dom = cx.dom();
            dom.traverse(doc).find_map(|e| match e {
                pageon_dom::Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("title")) => {
                    Some(n)
                }
                _ => None,
            })
        };
        let title = match existing {
            Some(t) => t,
            None => {
                let Some(head) = html_child_of(cx, doc, "head") else {
                    return Ok(());
                };
                let t = super::create_element(&mut cx.dom_mut(), ns!(html), "title");
                insert(cx, head, t, None)?;
                t
            }
        };
        super::super::node::replace_with_text(cx, title, &v)
    }

    pub fn get_dir(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let doc = doc_of(cx, this);
        Ok(root_element_of(cx, doc)
            .and_then(|r| {
                cx.dom()
                    .get(r)
                    .and_then(|n| n.attr("dir"))
                    .map(str::to_string)
            })
            .unwrap_or_default())
    }

    pub fn set_dir(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        let doc = doc_of(cx, this);
        if let Some(r) = root_element_of(cx, doc) {
            cx.dom_mut().set_attr(r, "dir", &v).map_err(|_| gone())?;
        }
        Ok(())
    }

    pub fn get_body(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let doc = doc_of(cx, this);
        Ok(html_child_of(cx, doc, "body")
            .or_else(|| html_child_of(cx, doc, "frameset"))
            .map(Obj::Node))
    }

    pub fn set_body(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<Obj>) -> R<()> {
        let Some(new) = v else {
            return Err(Error::dom(
                "HierarchyRequestError",
                "The new body must not be null.",
            ));
        };
        let new = new.node()?;
        let doc = doc_of(cx, this);
        let Some(root) = root_element_of(cx, doc) else {
            return Ok(());
        };
        if let Some(Obj::Node(old)) = get_body(cx, this)? {
            super::super::node::replace_child(cx, Obj::Node(root), Obj::Node(new), Obj::Node(old))?;
        } else {
            insert(cx, root, new, None)?;
        }
        Ok(())
    }

    pub fn get_head(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let doc = doc_of(cx, this);
        Ok(html_child_of(cx, doc, "head").map(Obj::Node))
    }

    pub fn get_images(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "images",
            Iface::HTMLCollection,
            Slot::Collection(Coll::Doc("images")),
        ))
    }

    pub fn get_links(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "links",
            Iface::HTMLCollection,
            Slot::Collection(Coll::Doc("links")),
        ))
    }

    pub fn get_forms(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "forms",
            Iface::HTMLCollection,
            Slot::Collection(Coll::Doc("forms")),
        ))
    }

    pub fn get_scripts(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "scripts",
            Iface::HTMLCollection,
            Slot::Collection(Coll::Doc("scripts")),
        ))
    }

    pub fn get_elements_by_name(cx: &mut Cx<'_, '_, '_>, _this: Obj, name: String) -> R<Obj> {
        Ok(cx.alloc(Iface::NodeList, Slot::Collection(Coll::Name(name))))
    }

    pub fn get_model_context(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(crate::dom::webmcp::model_context_of(cx))
    }

    pub fn get_current_script(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(cx.st().current_script.map(Obj::Node))
    }

    pub fn get_default_view(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(Some(Obj::Window))
    }

    pub fn has_focus(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(true)
    }

    pub fn get_implementation(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "implementation",
            Iface::DOMImplementation,
            Slot::Implementation,
        ))
    }

    /// An HTTP date (`MM/DD/YYYY hh:mm:ss`, as `lastModified` formats it).
    fn format_date(ms: f64) -> String {
        let secs = (ms / 1000.0).floor() as i64;
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        // Civil from days (Howard Hinnant).
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };
        format!(
            "{m:02}/{d:02}/{y:04} {:02}:{:02}:{:02}",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }
}

/// The `DocumentOrShadowRoot` mixin.
pub mod document_or_shadow_root {
    use super::*;

    pub fn get_adopted_style_sheets(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<crate::cv::Value> {
        crate::dom::sheets::get_adopted(cx, this.node()?)
    }

    pub fn set_adopted_style_sheets(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Vec<Obj>) -> R<()> {
        crate::dom::sheets::set_adopted(cx, this.node()?, v)
    }

    /// The focused element, retargeted: the host for focus inside a shadow tree; on a shadow
    /// root, the focused element in its tree (or a host within it).
    pub fn get_active_element(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let me = this.node()?;
        let focused = cx.st().focused;
        let dom = cx.dom();
        if dom.root() == me {
            return Ok(focused
                .map(|f| crate::dom::shadow::retarget(&dom, f, me))
                .or_else(|| {
                    drop(dom);
                    html_child(cx, "body")
                })
                .map(Obj::Node));
        }
        Ok(focused
            .filter(|&f| crate::dom::shadow::includes(&dom, me, f) && f != me)
            .map(|f| crate::dom::shadow::retarget(&dom, f, me))
            .filter(|&r| dom.root_of(r) == me)
            .map(Obj::Node))
    }
}

/// `DocumentType`.
pub mod document_type {
    use super::*;

    fn field(cx: &Cx<'_, '_, '_>, this: Obj, which: u8) -> R<String> {
        match cx.dom().get(this.node()?).map(|n| &n.data) {
            Some(NodeData::Doctype {
                name,
                public_id,
                system_id,
            }) => Ok(match which {
                0 => name.clone(),
                1 => public_id.clone(),
                _ => system_id.clone(),
            }),
            _ => Err(gone()),
        }
    }

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        field(cx, this, 0)
    }

    pub fn get_public_id(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        field(cx, this, 1)
    }

    pub fn get_system_id(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        field(cx, this, 2)
    }
}

/// The `NonElementParentNode` mixin.
pub mod non_element_parent_node {
    use super::*;

    pub fn get_element_by_id(cx: &mut Cx<'_, '_, '_>, this: Obj, id: String) -> R<Option<Obj>> {
        if id.is_empty() {
            return Ok(None);
        }
        let root = this.node()?;
        let dom = cx.dom();
        Ok(dom
            .traverse(root)
            .find_map(|e| match e {
                pageon_dom::Edge::Enter(n)
                    if n != root && dom.get(n).and_then(|x| x.attr("id")) == Some(id.as_str()) =>
                {
                    Some(n)
                }
                _ => None,
            })
            .map(Obj::Node))
    }
}

/// `DOMImplementation`.
pub mod dom_implementation {
    use super::*;

    /// `createHTMLDocument(title)`: an inert document (as `DOMParser` makes) with a doctype,
    /// `<html>`, `<head>` with the title when given, and `<body>`.
    pub fn create_html_document(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        title: Option<String>,
    ) -> R<Obj> {
        let t = title
            .map(|t| {
                let t = t.replace('&', "&amp;").replace('<', "&lt;");
                format!("<title>{t}</title>")
            })
            .unwrap_or_default();
        let html = format!("<!DOCTYPE html><html><head>{t}</head><body></body></html>");
        Ok(Obj::Node(crate::dom::basics::inert_document(cx, &html)))
    }

    /// Historical; always `true`, as the DOM standard says.
    pub fn has_feature(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(true)
    }
}
