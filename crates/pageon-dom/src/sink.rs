//! Tree construction: html5ever's tree builder writing into pageon's arena.

use std::borrow::Cow;
use std::cell::{Ref, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use html5ever::interface::{
    ElemName, ElementFlags, NodeOrText, QuirksMode as HtmlQuirks, TreeSink,
};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{
    Attribute as HtmlAttribute, LocalName, Namespace as HtmlNs, ParseOpts, QualName, local_name, ns,
};

use crate::dom::{Attribute, Dom, NodeData, NodeId, QuirksMode};

/// Parser options.
#[derive(Debug, Clone, Copy, Default)]
pub struct ParseOptions {
    /// Whether scripting is enabled (affects `<noscript>`). pageon runs no JS yet, so the
    /// default is `false`: `<noscript>` content is parsed as markup.
    pub scripting: bool,
}

pub(crate) fn opts(o: ParseOptions) -> ParseOpts {
    let mut p = ParseOpts::default();
    p.tree_builder.scripting_enabled = o.scripting;
    p
}

/// Parses a complete document.
pub fn parse_document(html: &str, options: ParseOptions) -> Dom {
    html5ever::parse_document(Sink::default(), opts(options)).one(StrTendril::from(html))
}

/// Parses a fragment in the context of an element `context` (HTML namespace unless `context`
/// is `svg <name>` or `math <name>`), as `innerHTML` does. The fragment's nodes are the children
/// of the returned DOM's `<html>` element.
pub fn parse_fragment(html: &str, context: &str, options: ParseOptions) -> Dom {
    let (namespace, local) = match context.split_once(' ') {
        Some(("svg", l)) => (ns!(svg), l),
        Some(("math", l)) => (ns!(mathml), l),
        _ => (ns!(html), context),
    };
    let name = QualName::new(None, namespace, LocalName::from(local));
    html5ever::parse_fragment(
        Sink {
            fragment: true,
            ..Sink::default()
        },
        opts(options),
        name,
        Vec::new(),
        options.scripting,
    )
    .one(StrTendril::from(html))
}

/// The tree sink.
#[derive(Default)]
pub struct Sink {
    pub(crate) dom: Rc<RefCell<Dom>>,
    /// Fragment parsing (`innerHTML`): declarative shadow roots are not attached.
    pub(crate) fragment: bool,
}

/// An element's name, borrowed from the arena.
#[derive(Debug)]
pub struct NameRef<'a>(Ref<'a, QualName>);

impl ElemName for NameRef<'_> {
    fn ns(&self) -> &HtmlNs {
        &self.0.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

fn attrs(v: Vec<HtmlAttribute>) -> Vec<Attribute> {
    v.into_iter()
        .map(|a| Attribute {
            name: a.name,
            value: a.value.to_string(),
        })
        .collect()
}

impl Sink {
    fn append_text_or_node(&self, parent: NodeId, child: NodeOrText<NodeId>) {
        let mut dom = self.dom.borrow_mut();
        match child {
            NodeOrText::AppendText(text) => {
                if let Some(last) = dom.last_child(parent)
                    && let Some(node) = dom.get_mut(last)
                    && let NodeData::Text(t) = &mut node.data
                {
                    t.push_str(&text);
                    return;
                }
                let t = dom.create(NodeData::Text(text.to_string()));
                let _ = dom.append(parent, t);
            }
            NodeOrText::AppendNode(n) => {
                let _ = dom.append(parent, n);
            }
        }
    }

    /// "Maybe clone an option into selectedcontent" (HTML standard, customizable `<select>`).
    fn clone_option_into_selectedcontent(&self, option: NodeId) {
        let mut dom = self.dom.borrow_mut();
        let Some(select) = nearest_select(&dom, option) else {
            return;
        };
        let Some(node) = dom.get(select) else { return };
        if node.attr("multiple").is_some() {
            return;
        }
        let Some(selected_content) = dom
            .traverse(select)
            .filter_map(|e| match e {
                crate::dom::Edge::Enter(id) if id != select => Some(id),
                _ => None,
            })
            .find(|&id| dom.get(id).is_some_and(|n| n.is_html("selectedcontent")))
        else {
            return;
        };
        if dom.get(option).and_then(|n| n.attr("selected")).is_none() {
            return;
        }
        let old: Vec<NodeId> = dom.children(selected_content).collect();
        for c in old {
            let _ = dom.remove(c);
        }
        let kids: Vec<NodeId> = dom.children(option).collect();
        for k in kids {
            let copy = deep_clone(&mut dom, k);
            let _ = dom.append(selected_content, copy);
        }
    }
}

fn nearest_select(dom: &Dom, option: NodeId) -> Option<NodeId> {
    let mut seen_optgroup = false;
    let mut cur = dom.parent(option);
    while let Some(c) = cur {
        if let Some(n) = dom.get(c) {
            if n.is_html("datalist") || n.is_html("hr") || n.is_html("option") {
                return None;
            }
            if n.is_html("optgroup") {
                if seen_optgroup {
                    return None;
                }
                seen_optgroup = true;
            }
            if n.is_html("select") {
                return Some(c);
            }
        }
        cur = dom.parent(c);
    }
    None
}

fn deep_clone(dom: &mut Dom, id: NodeId) -> NodeId {
    let Some(data) = dom.get(id).map(|n| n.data.clone()) else {
        return dom.create(NodeData::DocumentFragment);
    };
    let data = match data {
        NodeData::Element {
            name,
            attrs,
            mathml_annotation_xml_integration_point,
            template_contents,
        } => {
            let tc = template_contents.map(|t| deep_clone(dom, t));
            NodeData::Element {
                name,
                attrs,
                template_contents: tc,
                mathml_annotation_xml_integration_point,
            }
        }
        other => other,
    };
    let copy = dom.create(data);
    let kids: Vec<NodeId> = dom.children(id).collect();
    for k in kids {
        let c = deep_clone(dom, k);
        let _ = dom.append(copy, c);
    }
    copy
}

/// The deepest the parser nests (Chromium's `kMaximumHTMLParserDOMTreeDepth`): deeper content
/// is appended to the element at this depth, so no page builds a tree that tree walks cannot
/// survive.
const MAX_PARSER_DEPTH: usize = 512;

impl Sink {
    /// `parent`, or its ancestor at [`MAX_PARSER_DEPTH`] when `parent` is deeper.
    fn capped(&self, parent: NodeId) -> NodeId {
        let dom = self.dom.borrow();
        let mut chain = vec![parent];
        while let Some(p) = dom.parent(*chain.last().unwrap_or(&parent)) {
            chain.push(p);
        }
        // chain ends at the document (depth 0); `parent` is at depth chain.len() - 1.
        let depth = chain.len() - 1;
        if depth < MAX_PARSER_DEPTH {
            return parent;
        }
        chain[depth - (MAX_PARSER_DEPTH - 1)]
    }
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = Dom;
    type ElemName<'a>
        = NameRef<'a>
    where
        Self: 'a;

    fn finish(self) -> Dom {
        Rc::try_unwrap(self.dom).map_or_else(|shared| shared.borrow().clone(), RefCell::into_inner)
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> NodeId {
        self.dom.borrow().root()
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> NameRef<'a> {
        NameRef(Ref::map(self.dom.borrow(), |d| {
            match d.get(*target).map(|n| &n.data) {
                Some(NodeData::Element { name, .. }) => name,
                _ => panic!("html5ever asked for the name of a non-element"),
            }
        }))
    }

    fn create_element(
        &self,
        name: QualName,
        attributes: Vec<HtmlAttribute>,
        flags: ElementFlags,
    ) -> NodeId {
        let mut dom = self.dom.borrow_mut();
        let template_contents = flags
            .template
            .then(|| dom.create(NodeData::DocumentFragment));
        dom.create(NodeData::Element {
            name,
            attrs: attrs(attributes),
            template_contents,
            mathml_annotation_xml_integration_point: flags.mathml_annotation_xml_integration_point,
        })
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.dom
            .borrow_mut()
            .create(NodeData::Comment(text.to_string()))
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.dom
            .borrow_mut()
            .create(NodeData::ProcessingInstruction {
                target: target.to_string(),
                data: data.to_string(),
            })
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        let parent = self.capped(*parent);
        self.append_text_or_node(parent, child);
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = self.dom.borrow().parent(*element).is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let mut dom = self.dom.borrow_mut();
        let d = dom.create(NodeData::Doctype {
            name: name.to_string(),
            public_id: public_id.to_string(),
            system_id: system_id.to_string(),
        });
        let root = dom.root();
        let _ = dom.append(root, d);
    }

    fn allow_declarative_shadow_roots(&self, _intended_parent: &NodeId) -> bool {
        !self.fragment
    }

    /// `<template shadowrootmode>`: the template's contents become the host's shadow tree.
    fn attach_declarative_shadow(
        &self,
        host: &NodeId,
        template: &NodeId,
        attrs: &[HtmlAttribute],
    ) -> bool {
        let get = |n: &str| attrs.iter().find(|a| &*a.name.local == n);
        let mode = match get("shadowrootmode").map(|a| a.value.as_ref()) {
            Some("open") => crate::ShadowMode::Open,
            Some("closed") => crate::ShadowMode::Closed,
            _ => return false,
        };
        let init = crate::ShadowInit {
            mode,
            delegates_focus: get("shadowrootdelegatesfocus").is_some(),
            clonable: get("shadowrootclonable").is_some(),
            serializable: get("shadowrootserializable").is_some(),
            slot_assignment: crate::SlotAssignment::Named,
        };
        let mut dom = self.dom.borrow_mut();
        let Ok(root) = dom.attach_shadow(*host, init) else {
            return false;
        };
        if let Some(n) = dom.get_mut(*template)
            && let NodeData::Element {
                template_contents, ..
            } = &mut n.data
        {
            *template_contents = Some(root);
        }
        true
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        match self.dom.borrow().get(*target).map(|n| &n.data) {
            Some(NodeData::Element {
                template_contents: Some(t),
                ..
            }) => *t,
            _ => panic!("html5ever asked for template contents of a non-template"),
        }
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: HtmlQuirks) {
        self.dom.borrow_mut().quirks_mode = match mode {
            HtmlQuirks::Quirks => QuirksMode::Quirks,
            HtmlQuirks::LimitedQuirks => QuirksMode::LimitedQuirks,
            HtmlQuirks::NoQuirks => QuirksMode::NoQuirks,
        };
    }

    fn append_before_sibling(&self, sibling: &NodeId, child: NodeOrText<NodeId>) {
        let mut dom = self.dom.borrow_mut();
        let Some(parent) = dom.parent(*sibling) else {
            return;
        };
        let node = match child {
            NodeOrText::AppendText(text) => {
                if let Some(prev) = dom.prev_sibling(*sibling)
                    && let Some(NodeData::Text(t)) = dom.get_mut(prev).map(|n| &mut n.data)
                {
                    t.push_str(&text);
                    return;
                }
                dom.create(NodeData::Text(text.to_string()))
            }
            NodeOrText::AppendNode(n) => n,
        };
        let _ = dom.insert_before(parent, node, *sibling);
    }

    fn add_attrs_if_missing(&self, target: &NodeId, new_attrs: Vec<HtmlAttribute>) {
        let mut dom = self.dom.borrow_mut();
        if let Some(NodeData::Element {
            attrs: existing, ..
        }) = dom.get_mut(*target).map(|n| &mut n.data)
        {
            let names: HashSet<QualName> = existing.iter().map(|a| a.name.clone()).collect();
            existing.extend(
                attrs(new_attrs)
                    .into_iter()
                    .filter(|a| !names.contains(&a.name)),
            );
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        let _ = self.dom.borrow_mut().detach(*target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        let mut dom = self.dom.borrow_mut();
        let kids: Vec<NodeId> = dom.children(*node).collect();
        for k in kids {
            let _ = dom.append(*new_parent, k);
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &NodeId) -> bool {
        matches!(
            self.dom.borrow().get(*handle).map(|n| &n.data),
            Some(NodeData::Element {
                mathml_annotation_xml_integration_point: true,
                ..
            })
        )
    }

    fn maybe_clone_an_option_into_selectedcontent(&self, option: &NodeId) {
        let is_option = self
            .dom
            .borrow()
            .get(*option)
            .is_some_and(|n| n.local_name() == Some(&local_name!("option")));
        if is_option {
            self.clone_option_into_selectedcontent(*option);
        }
    }
}
