//! Live ranges and the document's selection (DOM §5.5, Selection API).
//!
//! The arena owns every live range, so each mutation (parser, scripts, edits) runs the DOM
//! standard's live-range steps in one place. Offsets count UTF-16 code units in character data
//! and children elsewhere.

use crate::dom::{Dom, DomError, Mutation};
use crate::{NodeData, NodeId};

/// A boundary point: a node and an offset in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Boundary {
    /// The node.
    pub node: NodeId,
    /// UTF-16 code units in character data, children otherwise.
    pub offset: u32,
}

impl Boundary {
    /// A boundary point.
    pub fn new(node: NodeId, offset: u32) -> Self {
        Self { node, offset }
    }
}

/// A live range's handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RangeId(pub u32);

/// A selection's direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Direction {
    /// Set without a direction (`addRange`).
    #[default]
    None,
    /// Anchor before focus.
    Forward,
    /// Focus before anchor.
    Backward,
}

/// The live ranges of a document and its selection.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ranges {
    live: Vec<Option<(Boundary, Boundary)>>,
    selection: Option<(RangeId, Direction)>,
    selection_changed: bool,
}

/// Where a boundary point is relative to another (DOM §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Position {
    /// Before.
    Before,
    /// The same point.
    Equal,
    /// After.
    After,
}

fn utf16_to_byte(s: &str, units: u32) -> usize {
    let mut n = 0u32;
    for (i, c) in s.char_indices() {
        if n >= units {
            return i;
        }
        n += c.len_utf16() as u32;
    }
    s.len()
}

impl Dom {
    /// A node's length (DOM §4.2): UTF-16 code units of character data, else its children.
    pub fn node_length(&self, id: NodeId) -> u32 {
        match self.get(id).map(|n| &n.data) {
            Some(NodeData::Text(t) | NodeData::Comment(t)) => t.encode_utf16().count() as u32,
            Some(NodeData::ProcessingInstruction { data, .. }) => {
                data.encode_utf16().count() as u32
            }
            Some(NodeData::Doctype { .. }) | None => 0,
            Some(_) => self.children(id).count() as u32,
        }
    }

    /// A node's index among its siblings.
    pub fn index_of(&self, id: NodeId) -> u32 {
        std::iter::successors(self.prev_sibling(id), |&p| self.prev_sibling(p)).count() as u32
    }

    /// The child at `offset`, if any.
    pub fn child_at(&self, parent: NodeId, offset: u32) -> Option<NodeId> {
        self.children(parent).nth(offset as usize)
    }

    /// A new live range.
    pub fn create_range(&mut self, start: Boundary, end: Boundary) -> RangeId {
        self.ranges.live.push(Some((start, end)));
        RangeId(self.ranges.live.len() as u32 - 1)
    }

    /// A live range's boundary points.
    pub fn range(&self, id: RangeId) -> Option<(Boundary, Boundary)> {
        self.ranges.live.get(id.0 as usize).copied().flatten()
    }

    /// Moves a live range.
    pub fn set_range(&mut self, id: RangeId, start: Boundary, end: Boundary) {
        if let Some(slot) = self.ranges.live.get_mut(id.0 as usize) {
            *slot = Some((start, end));
            if self.ranges.selection.is_some_and(|(r, _)| r == id) {
                self.ranges.selection_changed = true;
            }
        }
    }

    /// The document's selection: its range and direction.
    pub fn selection(&self) -> Option<(RangeId, Direction)> {
        self.ranges.selection
    }

    /// Sets or empties the selection.
    pub fn set_selection(&mut self, sel: Option<(RangeId, Direction)>) {
        if self.ranges.selection != sel {
            self.ranges.selection = sel;
            self.ranges.selection_changed = true;
        }
    }

    /// Whether the selection changed since the last call (`selectionchange`).
    pub fn take_selection_changed(&mut self) -> bool {
        std::mem::take(&mut self.ranges.selection_changed)
    }

    /// Marks the selection changed (a text control's selection moved).
    pub fn touch_selection(&mut self) {
        self.ranges.selection_changed = true;
    }

    /// Where `a` is relative to `b` (DOM §5.2); `None` when they are in different trees.
    pub fn compare_points(&self, a: Boundary, b: Boundary) -> Option<Position> {
        use std::cmp::Ordering;
        if a.node == b.node {
            return Some(match a.offset.cmp(&b.offset) {
                Ordering::Less => Position::Before,
                Ordering::Equal => Position::Equal,
                Ordering::Greater => Position::After,
            });
        }
        let path = |n: NodeId| {
            let mut p: Vec<NodeId> = std::iter::once(n).chain(self.ancestors(n)).collect();
            p.reverse();
            p
        };
        let (pa, pb) = (path(a.node), path(b.node));
        if pa.first() != pb.first() {
            return None;
        }
        let common = pa.iter().zip(&pb).take_while(|(x, y)| x == y).count();
        // `a.node` is an ancestor of `b.node`: compare a's offset with the index of b's
        // ancestor that is a's child.
        if common == pa.len() {
            let child = pb[common];
            return Some(if a.offset <= self.index_of(child) {
                Position::Before
            } else {
                Position::After
            });
        }
        if common == pb.len() {
            let child = pa[common];
            return Some(if self.index_of(child) < b.offset {
                Position::Before
            } else {
                Position::After
            });
        }
        Some(if self.index_of(pa[common]) < self.index_of(pb[common]) {
            Position::Before
        } else {
            Position::After
        })
    }

    fn each_boundary(&mut self, mut f: impl FnMut(&mut Boundary)) {
        let mut changed = false;
        let sel = self.ranges.selection.map(|(r, _)| r.0 as usize);
        for (i, slot) in self.ranges.live.iter_mut().enumerate() {
            if let Some((s, e)) = slot {
                if Some(i) == sel {
                    let before = (*s, *e);
                    f(s);
                    f(e);
                    changed = before != (*s, *e);
                } else {
                    f(s);
                    f(e);
                }
            }
        }
        if changed {
            self.ranges.selection_changed = true;
        }
    }

    /// Live-range steps before `node` leaves `parent` at `index` (DOM §4.2.3 removing steps).
    pub(crate) fn ranges_before_remove(&mut self, node: NodeId, parent: NodeId) {
        if self.ranges.live.is_empty() {
            return;
        }
        let index = self.index_of(node);
        let nodes: Vec<NodeId> = self
            .traverse(node)
            .filter_map(|e| match e {
                crate::Edge::Enter(n) => Some(n),
                crate::Edge::Leave(_) => None,
            })
            .collect();
        // Most removals take a node or two: compare directly; a set for larger subtrees.
        let set: Option<std::collections::HashSet<NodeId>> =
            (nodes.len() > 8).then(|| nodes.iter().copied().collect());
        let inside = |n: NodeId| match &set {
            Some(set) => set.contains(&n),
            None => nodes.contains(&n),
        };
        self.each_boundary(|b| {
            if b.node == parent {
                if b.offset > index {
                    b.offset -= 1;
                }
            } else if inside(b.node) {
                *b = Boundary::new(parent, index);
            }
        });
    }

    /// Live-range steps after `node` was inserted into its parent (DOM §4.2.3 insert).
    pub(crate) fn ranges_after_insert(&mut self, node: NodeId) {
        if self.ranges.live.is_empty() {
            return;
        }
        let Some(parent) = self.parent(node) else {
            return;
        };
        let index = self.index_of(node);
        self.each_boundary(|b| {
            if b.node == parent && b.offset > index {
                b.offset += 1;
            }
        });
    }

    /// Boundaries into nodes that no longer exist go to the document's start.
    pub(crate) fn ranges_after_free(&mut self) {
        if self.ranges.live.is_empty() {
            return;
        }
        let root = self.root();
        let gone: Vec<NodeId> = self
            .ranges
            .live
            .iter()
            .flatten()
            .flat_map(|(s, e)| [s.node, e.node])
            .filter(|&n| self.get(n).is_none())
            .collect();
        if gone.is_empty() {
            return;
        }
        self.each_boundary(|b| {
            if gone.contains(&b.node) {
                *b = Boundary::new(root, 0);
            }
        });
    }

    /// DOM's "replace data": replaces `count` UTF-16 units at `offset` of a character data
    /// node with `data`, updating live ranges.
    pub fn replace_data(
        &mut self,
        id: NodeId,
        offset: u32,
        count: u32,
        data: &str,
    ) -> Result<(), DomError> {
        let length = self.node_length(id);
        let offset = offset.min(length);
        let count = count.min(length - offset);
        // Text styles nothing itself; its parent's `:empty` (and the like) may change.
        if let Some(parent) = self.parent(id) {
            self.log_style(crate::StyleChange::Children { parent });
        }
        let old = match &mut self.raw_mut(id).ok_or(DomError::Gone(id))?.data {
            NodeData::Text(t)
            | NodeData::Comment(t)
            | NodeData::ProcessingInstruction { data: t, .. } => {
                let old = t.clone();
                let a = utf16_to_byte(t, offset);
                let b = utf16_to_byte(t, offset + count);
                t.replace_range(a..b, data);
                old
            }
            _ => return Ok(()),
        };
        if self.observing {
            self.mutations
                .push(Mutation::CharacterData { target: id, old });
        }
        let added = data.encode_utf16().count() as u32;
        self.each_boundary(|b| {
            if b.node != id {
                return;
            }
            if b.offset > offset && b.offset <= offset + count {
                b.offset = offset;
            } else if b.offset > offset + count {
                b.offset = b.offset + added - count;
            }
        });
        Ok(())
    }

    /// DOM's "split a Text node" at `offset`: the text after it moves to a new node inserted
    /// after this one, live ranges with it.
    pub fn split_text(&mut self, id: NodeId, offset: u32) -> Result<NodeId, DomError> {
        let length = self.node_length(id);
        let offset = offset.min(length);
        let tail = match self.get(id).map(|n| &n.data) {
            Some(NodeData::Text(t)) => t[utf16_to_byte(t, offset)..].to_string(),
            _ => return Err(DomError::Gone(id)),
        };
        let new = self.create(NodeData::Text(tail));
        if let Some(parent) = self.parent(id) {
            match self.next_sibling(id) {
                Some(next) => self.insert_before(parent, new, next)?,
                None => self.append(parent, new)?,
            }
            let index = self.index_of(id);
            self.each_boundary(|b| {
                if b.node == id && b.offset > offset {
                    *b = Boundary::new(new, b.offset - offset);
                } else if b.node == parent && b.offset == index + 1 {
                    b.offset += 1;
                }
            });
        }
        self.replace_data(id, offset, length - offset, "")?;
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> (Dom, NodeId, NodeId) {
        let mut d = Dom::new();
        let p = d.create(NodeData::DocumentFragment);
        let root = d.root();
        d.append(root, p).unwrap();
        let t = d.create(NodeData::Text("hello world".into()));
        d.append(p, t).unwrap();
        (d, p, t)
    }

    #[test]
    fn replace_data_moves_boundaries() {
        let (mut d, _, t) = doc();
        let r = d.create_range(Boundary::new(t, 2), Boundary::new(t, 8));
        d.replace_data(t, 0, 5, "hi").unwrap();
        assert_eq!(d.range(r), Some((Boundary::new(t, 0), Boundary::new(t, 5))));
    }

    #[test]
    fn split_text_moves_later_boundaries() {
        let (mut d, p, t) = doc();
        let r = d.create_range(Boundary::new(t, 2), Boundary::new(t, 8));
        let after = d.create_range(Boundary::new(p, 1), Boundary::new(p, 1));
        let new = d.split_text(t, 5).unwrap();
        assert_eq!(
            d.range(r),
            Some((Boundary::new(t, 2), Boundary::new(new, 3)))
        );
        assert_eq!(
            d.range(after),
            Some((Boundary::new(p, 2), Boundary::new(p, 2)))
        );
    }

    #[test]
    fn removal_collapses_to_the_parent() {
        let (mut d, p, t) = doc();
        let r = d.create_range(Boundary::new(t, 3), Boundary::new(p, 1));
        d.remove(t).unwrap();
        assert_eq!(d.range(r), Some((Boundary::new(p, 0), Boundary::new(p, 0))));
    }

    #[test]
    fn insertion_shifts_later_offsets() {
        let (mut d, p, t) = doc();
        let r = d.create_range(Boundary::new(p, 0), Boundary::new(p, 1));
        let b = d.create(NodeData::Comment(String::new()));
        d.insert_before(p, b, t).unwrap();
        assert_eq!(d.range(r), Some((Boundary::new(p, 0), Boundary::new(p, 2))));
    }

    #[test]
    fn points_compare_in_tree_order() {
        let (d, p, t) = doc();
        assert_eq!(
            d.compare_points(Boundary::new(p, 0), Boundary::new(t, 4)),
            Some(Position::Before)
        );
        assert_eq!(
            d.compare_points(Boundary::new(p, 1), Boundary::new(t, 4)),
            Some(Position::After)
        );
    }
}
