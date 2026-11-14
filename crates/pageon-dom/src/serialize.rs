//! HTML fragment serialization (HTML standard), driven by html5ever's serializer over an
//! iterative walk. `<template>` elements serialize their template contents.

use std::io;

use html5ever::QualName;
use html5ever::serialize::{Serialize, SerializeOpts, Serializer, TraversalScope, serialize};

use crate::dom::{Dom, NodeData, NodeId};

struct View<'a> {
    dom: &'a Dom,
    id: NodeId,
}

enum Op {
    Open(NodeId),
    Close(QualName),
}

impl Dom {
    /// The children a serializer visits: template contents for `<template>`, else children.
    fn serialized_children(&self, id: NodeId) -> Vec<NodeId> {
        match self.get(id).map(|n| &n.data) {
            Some(NodeData::Element {
                template_contents: Some(t),
                ..
            }) => self.children(*t).collect(),
            _ => self.children(id).collect(),
        }
    }

    /// Serializes a node as HTML: documents and fragments serialize their children; other
    /// nodes serialize themselves (outerHTML).
    pub fn to_html(&self, id: NodeId) -> String {
        let scope = match self.get(id).map(|n| &n.data) {
            Some(NodeData::Document | NodeData::DocumentFragment) => {
                TraversalScope::ChildrenOnly(None)
            }
            Some(_) => TraversalScope::IncludeNode,
            None => return String::new(),
        };
        let mut out = Vec::new();
        let opts = SerializeOpts {
            traversal_scope: scope,
            ..SerializeOpts::default()
        };
        // Writing into a Vec cannot fail.
        let _ = serialize(&mut out, &View { dom: self, id }, opts);
        String::from_utf8(out).unwrap_or_default()
    }

    /// Serializes the children of a node (innerHTML).
    pub fn inner_html(&self, id: NodeId) -> String {
        let mut out = String::new();
        for c in self.serialized_children(id) {
            out.push_str(&self.to_html(c));
        }
        out
    }
}

impl Serialize for View<'_> {
    fn serialize<S: Serializer>(
        &self,
        serializer: &mut S,
        scope: TraversalScope,
    ) -> io::Result<()> {
        let mut ops: Vec<Op> = match scope {
            TraversalScope::IncludeNode => vec![Op::Open(self.id)],
            TraversalScope::ChildrenOnly(_) => self
                .dom
                .serialized_children(self.id)
                .into_iter()
                .rev()
                .map(Op::Open)
                .collect(),
        };
        while let Some(op) = ops.pop() {
            match op {
                Op::Close(name) => serializer.end_elem(name)?,
                Op::Open(id) => {
                    let Some(node) = self.dom.get(id) else {
                        continue;
                    };
                    match &node.data {
                        NodeData::Element { name, attrs, .. } => {
                            serializer.start_elem(
                                name.clone(),
                                attrs.iter().map(|a| (&a.name, a.value.as_str())),
                            )?;
                            ops.push(Op::Close(name.clone()));
                            ops.extend(
                                self.dom
                                    .serialized_children(id)
                                    .into_iter()
                                    .rev()
                                    .map(Op::Open),
                            );
                        }
                        NodeData::Doctype { name, .. } => serializer.write_doctype(name)?,
                        NodeData::Text(t) => serializer.write_text(t)?,
                        NodeData::Comment(t) => serializer.write_comment(t)?,
                        NodeData::ProcessingInstruction { target, data } => {
                            serializer.write_processing_instruction(target, data)?;
                        }
                        NodeData::Document
                        | NodeData::DocumentFragment
                        | NodeData::ShadowRoot { .. } => {
                            ops.extend(
                                self.dom
                                    .children(id)
                                    .collect::<Vec<_>>()
                                    .into_iter()
                                    .rev()
                                    .map(Op::Open),
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
