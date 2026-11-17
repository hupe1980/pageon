//! Text geometry: where a point in text is laid out (`Range.getClientRects`) and which point in
//! text is at a position (`caretPositionFromPoint`, clicks in text).
//!
//! Each inline formatting context is one Parley layout, its text collapsed and transformed,
//! with box brushes. A text node's place in it is found by aligning the DOM text of the
//! context, in box tree order, with the laid-out text.

use pageon_dom::NodeId;
use parley::{Affinity, Cursor, Selection};
use style::values::specified::box_::{DisplayInside, DisplayOutside};

use crate::boxes::tree::{Kind, TextLayout};
use crate::{BoxId as BId, Layout, Rect};

/// A text node's characters mapped to layout byte indexes: `units[i]` is where UTF-16 offset
/// `i` is (one entry past the end).
struct Segment {
    node: NodeId,
    units: Vec<usize>,
}

/// An inline formatting context: its box and the text nodes in it.
struct Context {
    root: BId,
    segments: Vec<Segment>,
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}')
}

impl Layout {
    fn inline_layout(&self, b: BId) -> Option<&TextLayout> {
        self.tree.nodes.get(b)?.inline_layout.as_deref()
    }

    /// The nearest box (or self) that lays out inline content: up the box tree (an anonymous
    /// block holds text among blocks), else the flat tree.
    fn context_root(&self, b: BId) -> Option<BId> {
        let mut cur = Some(b);
        while let Some(c) = cur {
            if self.inline_layout(c).is_some() {
                return Some(c);
            }
            let n = &self.tree[c];
            cur = n.layout_parent.get().or(n.parent);
        }
        None
    }

    /// The text nodes of a context in layout order, skipping what does not add text
    /// (`display: none`, inline boxes with their own layout).
    fn context_texts(&self, root: BId) -> Vec<(NodeId, String)> {
        let mut out = Vec::new();
        let r = &self.tree[root];
        let mut stack: Vec<BId> = r.after.into_iter().collect();
        stack.extend(r.children.iter().rev().copied());
        stack.extend(r.before);
        while let Some(b) = stack.pop() {
            let node = &self.tree[b];
            match node.kind {
                Kind::Text => {
                    if let Some(id) = node.dom {
                        out.push((id, node.text.clone()));
                    }
                }
                Kind::Element | Kind::Anonymous | Kind::Pseudo => {
                    let display = node.primary_styles().map(|s| s.clone_display());
                    let inline = display.is_some_and(|d| {
                        (d.outside() == DisplayOutside::Inline && d.inside() == DisplayInside::Flow)
                            || d.inside() == DisplayInside::Contents
                    });
                    if inline && self.inline_layout(b).is_none() {
                        stack.extend(node.after);
                        stack.extend(node.children.iter().rev().copied());
                        stack.extend(node.before);
                    }
                }
                Kind::Document => {}
            }
        }
        out
    }

    fn context(&self, root: BId) -> Option<Context> {
        let text = &self.inline_layout(root)?.text;
        let laid: Vec<(usize, char)> = text.char_indices().collect();
        let mut li = 0usize;
        let byte = |i: usize| laid.get(i).map_or(text.len(), |&(b, _)| b);
        let mut segments = Vec::new();
        for (node, data) in self.context_texts(root) {
            let mut units = Vec::with_capacity(data.len() + 1);
            for c in data.chars() {
                for _ in 0..c.len_utf16() {
                    units.push(byte(li));
                }
                if is_space(c) {
                    if laid.get(li).is_some_and(|&(_, l)| is_space(l)) {
                        li += 1;
                    }
                    continue;
                }
                // Skip what layout adds (markers, `<br>` breaks) to the next match nearby.
                let found = (li..(li + 4).min(laid.len())).find(|&k| {
                    let l = laid[k].1;
                    l == c || l.to_lowercase().eq(c.to_lowercase())
                });
                li = found.map_or(li + 1, |k| k + 1).min(laid.len());
                if let (Some(k), Some(last)) = (found, units.last_mut())
                    && *last < byte(k)
                {
                    let n = c.len_utf16();
                    let len = units.len();
                    for u in &mut units[len - n..] {
                        *u = byte(k);
                    }
                }
            }
            units.push(byte(li));
            segments.push(Segment { node, units });
        }
        Some(Context { root, segments })
    }

    /// The content-box origin of a context in client coordinates.
    fn origin(&self, root: BId) -> Option<(f32, f32)> {
        let node = &self.tree[root];
        let r = crate::boxes::border_box(&self.tree, root)?;
        let l = node.final_layout();
        let offset = self.inline_layout(root).map_or(0.0, |t| t.block_offset);
        let (mut x, mut y) = (
            r.x + l.border.left + l.padding.left,
            r.y + l.border.top + l.padding.top + offset,
        );
        let (sx, sy) = self.viewport_scroll();
        if !self.box_fixed(root) {
            x -= sx;
            y -= sy;
        }
        Some((x, y))
    }

    fn locate(&self, node: NodeId) -> Option<(Context, usize)> {
        let b = self.bx(node)?;
        let root = self.context_root(b)?;
        let ctx = self.context(root)?;
        let i = ctx.segments.iter().position(|s| s.node == node)?;
        Some((ctx, i))
    }

    /// The client rectangles of the text node's UTF-16 range `start..end`, one per line; for
    /// an empty range, the caret (width 0). Empty when the node is not laid out.
    pub fn text_rects(&self, node: NodeId, start: u32, end: u32) -> Vec<Rect> {
        let Some((ctx, i)) = self.locate(node) else {
            return Vec::new();
        };
        let Some(text) = self.inline_layout(ctx.root) else {
            return Vec::new();
        };
        let Some((ox, oy)) = self.origin(ctx.root) else {
            return Vec::new();
        };
        let units = &ctx.segments[i].units;
        let at = |u: u32| units[(u as usize).min(units.len() - 1)];
        let rect = |b: parley::BoundingBox| Rect {
            x: ox + b.x0 as f32,
            y: oy + b.y0 as f32,
            width: (b.x1 - b.x0) as f32,
            height: (b.y1 - b.y0) as f32,
        };
        let layout = &text.layout;
        let (a, z) = (at(start), at(end.max(start)));
        if a == z {
            let c = Cursor::from_byte_index(layout, a, Affinity::Downstream);
            let mut r = rect(c.geometry(layout, 0.0));
            r.width = 0.0;
            return vec![r];
        }
        let sel = Selection::new(
            Cursor::from_byte_index(layout, a, Affinity::Downstream),
            Cursor::from_byte_index(layout, z, Affinity::Upstream),
        );
        sel.geometry(layout)
            .into_iter()
            .map(|(b, _)| rect(b))
            .collect()
    }

    /// The text position at a client point: a text node and a UTF-16 offset, or an element
    /// and a child offset where there is no text (an empty paragraph).
    pub fn caret_at(&self, x: f32, y: f32) -> Option<(NodeId, u32)> {
        let hit = self.element_at(x, y)?;
        let b = self.bx(hit)?;
        let root = self
            .context_root(b)
            .or_else(|| self.nearest_context(b, y))?;
        let ctx = self.context(root)?;
        let text = self.inline_layout(root)?;
        let (ox, oy) = self.origin(root)?;
        let cursor = Cursor::from_point(&text.layout, x - ox, y - oy);
        let idx = cursor.index();
        let upstream = cursor.affinity() == Affinity::Upstream;
        let mut best: Option<(NodeId, u32)> = None;
        for s in &ctx.segments {
            let (first, last) = (s.units[0], *s.units.last().unwrap_or(&0));
            if idx < first || idx > last {
                continue;
            }
            let off = s
                .units
                .iter()
                .position(|&u| u >= idx)
                .unwrap_or(s.units.len() - 1);
            best = Some((s.node, off as u32));
            // At the end of one node and the start of the next, downstream takes the next.
            if !(idx == last && !upstream) {
                break;
            }
        }
        best.or_else(|| {
            let root_id = self.tree[root].dom.or(Some(hit))?;
            Some((root_id, 0))
        })
    }

    /// The context below `b` whose box is nearest to `y` (a click beside or between blocks).
    fn nearest_context(&self, b: BId, y: f32) -> Option<BId> {
        let mut stack = vec![b];
        let mut best: Option<(f32, BId)> = None;
        while let Some(c) = stack.pop() {
            let node = &self.tree[c];
            if self.inline_layout(c).is_some() {
                if let Some((_, oy)) = self.origin(c) {
                    let h = node.final_layout().size.height;
                    let d = if y < oy {
                        oy - y
                    } else if y > oy + h {
                        y - oy - h
                    } else {
                        0.0
                    };
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, c));
                    }
                }
                continue;
            }
            stack.extend(node.layout_children.borrow().iter().flatten().copied());
        }
        best.map(|(_, c)| c)
    }

    /// The start and end of the laid-out line holding a text position, as text positions.
    pub fn line_bounds(&self, node: NodeId, offset: u32) -> Option<((NodeId, u32), (NodeId, u32))> {
        let (ctx, i) = self.locate(node)?;
        let text = self.inline_layout(ctx.root)?;
        let units = &ctx.segments[i].units;
        let idx = units[(offset as usize).min(units.len() - 1)];
        let line = text
            .layout
            .lines()
            .find(|l| l.text_range().contains(&idx) || l.text_range().end == idx)?;
        let range = line.text_range();
        let back = |byte: usize, end: bool| -> Option<(NodeId, u32)> {
            let mut pick = None;
            for s in &ctx.segments {
                if let Some(p) = s.units.iter().position(|&u| u >= byte) {
                    if s.units[p] == byte || !end {
                        pick = Some((s.node, p as u32));
                        if !end {
                            return pick;
                        }
                    }
                } else if end {
                    pick = Some((s.node, (s.units.len() - 1) as u32));
                }
            }
            pick
        };
        // A line's end excludes its trailing break or space.
        let end = text.text[..range.end]
            .trim_end_matches(['\n', ' '])
            .len()
            .max(range.start);
        Some((back(range.start, false)?, back(end, true)?))
    }
}
