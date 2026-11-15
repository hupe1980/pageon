//! pageon's document model.
//!
//! An arena DOM whose [`NodeId`] handles never alias: a handle to a removed node never
//! resolves again, even after its storage is reused. Documents are built by html5ever's
//! standards-compliant tree builder, decoded by the HTML standard's encoding sniffing, and
//! serialized by the HTML fragment serialization algorithm.
//!
//! ```
//! use pageon_dom::{ParseOptions, parse_document};
//!
//! let dom = parse_document("<p>Hello <b>world</b></p>", ParseOptions::default());
//! assert_eq!(dom.text(dom.root()), "Hello world");
//! assert_eq!(dom.to_html(dom.root()), "<html><head></head><body><p>Hello <b>world</b></p></body></html>");
//! ```

mod dom;
mod dump;
pub mod encoding;
pub mod forms;
mod parser;
mod ranges;
mod serialize;
mod shadow;
mod sink;

pub use dom::{
    Attribute, Children, ControlState, Dom, DomError, Edge, Mutation, Namespace, Node, NodeData,
    NodeId, QuirksMode, Traverse,
};
pub use parser::{Parser, Step};
pub use ranges::{Boundary, Direction, Position, RangeId};
pub use shadow::{AttachError, Composed, ShadowInit, ShadowMode, SlotAssignment, is_custom_name};
pub use sink::{ParseOptions, parse_document, parse_fragment};
