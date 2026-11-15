//! The document's `Selection` (Selection API) over the arena's selection range, and
//! `selectionchange`.

use pageon_dom::{Boundary, Direction, NodeId, Position, RangeId};

use crate::cv::{Cx, Error, Obj, R};
use crate::dom::editing::{Granularity, Step};
use crate::dom::gone;
use crate::dom::ranges::{point, root_of};
use crate::generated::Iface;
use crate::realm::{Slot, Target};

/// The selection: start, end, direction and its range.
pub(crate) fn current(cx: &Cx<'_, '_, '_>) -> Option<(Boundary, Boundary, Direction, RangeId)> {
    let dom = cx.dom();
    let (r, dir) = dom.selection()?;
    let (s, e) = dom.range(r)?;
    Some((s, e, dir, r))
}

/// Anchor and focus of the selection.
pub(crate) fn anchor_focus(cx: &Cx<'_, '_, '_>) -> Option<(Boundary, Boundary)> {
    let (s, e, dir, _) = current(cx)?;
    Some(if dir == Direction::Backward {
        (e, s)
    } else {
        (s, e)
    })
}

/// Selects from `anchor` to `focus` (a new range when scripts hold the current one).
pub(crate) fn select(cx: &Cx<'_, '_, '_>, anchor: Boundary, focus: Boundary) {
    let backward = cx.dom().compare_points(focus, anchor) == Some(Position::Before);
    let (s, e) = if backward {
        (focus, anchor)
    } else {
        (anchor, focus)
    };
    let dir = if anchor == focus {
        Direction::None
    } else if backward {
        Direction::Backward
    } else {
        Direction::Forward
    };
    let cur = cx.dom().selection().map(|(r, _)| r);
    let exposed = cx.st().sel_obj.map(|(r, _)| r);
    let r = match cur {
        Some(r) if exposed != Some(r) => {
            cx.dom_quiet().set_range(r, s, e);
            r
        }
        _ => cx.dom_quiet().create_range(s, e),
    };
    cx.dom_quiet().set_selection(Some((r, dir)));
}

/// Collapses the selection at `p`.
pub(crate) fn collapse_at(cx: &Cx<'_, '_, '_>, p: Boundary) {
    select(cx, p, p);
}

/// Empties the selection.
pub(crate) fn clear(cx: &Cx<'_, '_, '_>) {
    cx.dom_quiet().set_selection(None);
}

/// Fires `selectionchange` at the document and at text controls whose selection changed.
pub(crate) fn deliver(cx: &mut Cx<'_, '_, '_>) -> R<bool> {
    // Taking the flag changes no node: no layout is invalidated.
    let doc = cx.dom_quiet().take_selection_changed();
    let controls = std::mem::take(&mut cx.st().control_sel);
    let mut fired = false;
    for c in controls {
        if cx.dom().get(c).is_some() {
            crate::dom::event_target::fire(cx, Target::Node(c), "selectionchange", true, false)?;
            fired = true;
        }
    }
    if doc {
        let root = cx.dom().root();
        crate::dom::event_target::fire(cx, Target::Node(root), "selectionchange", false, false)?;
        fired = true;
    }
    Ok(fired)
}

fn this_doc_has_selection(cx: &Cx<'_, '_, '_>, doc: NodeId) -> bool {
    doc == cx.dom().root()
}

/// `getSelection()` on the window or the document: the same object; `null` for documents
/// without a browsing context.
pub(crate) fn object(cx: &mut Cx<'_, '_, '_>, doc: NodeId) -> Option<Obj> {
    if !this_doc_has_selection(cx, doc) {
        return None;
    }
    Some(crate::dom::window_same(
        cx,
        "selection",
        Iface::Selection,
        Slot::Selection,
    ))
}

fn in_document(cx: &Cx<'_, '_, '_>, n: NodeId) -> bool {
    let dom = cx.dom();
    root_of(&dom, n) == dom.root()
}

fn no_range() -> Error {
    Error::dom("InvalidStateError", "There is no selection.")
}

/// `Selection`.
pub mod selection {
    use super::*;

    pub fn get_anchor_node(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(anchor_focus(cx).map(|(a, _)| Obj::Node(a.node)))
    }

    pub fn get_anchor_offset(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        Ok(anchor_focus(cx).map_or(0, |(a, _)| a.offset))
    }

    pub fn get_focus_node(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(anchor_focus(cx).map(|(_, f)| Obj::Node(f.node)))
    }

    pub fn get_focus_offset(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        Ok(anchor_focus(cx).map_or(0, |(_, f)| f.offset))
    }

    pub fn get_is_collapsed(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(current(cx).is_none_or(|(s, e, _, _)| s == e))
    }

    pub fn get_range_count(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        Ok(u32::from(current(cx).is_some()))
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(match current(cx) {
            None => "None",
            Some((s, e, _, _)) if s == e => "Caret",
            Some(_) => "Range",
        }
        .into())
    }

    pub fn get_direction(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(match current(cx).map(|c| c.2) {
            Some(Direction::Forward) => "forward",
            Some(Direction::Backward) => "backward",
            _ => "none",
        }
        .into())
    }

    pub fn get_range_at(cx: &mut Cx<'_, '_, '_>, _this: Obj, index: u32) -> R<Obj> {
        let Some((_, _, _, r)) = current(cx).filter(|_| index == 0) else {
            return Err(Error::dom(
                "IndexSizeError",
                format!(
                    "The index provided ({index}) is greater than or equal to the maximum bound (1)."
                ),
            ));
        };
        let cached = cx.st().sel_obj.filter(|(id, _)| *id == r);
        if let Some((_, slab)) = cached {
            return Ok(Obj::Slab(slab));
        }
        let slab = cx.st().alloc(Iface::Range, Slot::Range(r));
        cx.st().sel_obj = Some((r, slab));
        Ok(Obj::Slab(slab))
    }

    pub fn add_range(cx: &mut Cx<'_, '_, '_>, _this: Obj, range: Obj) -> R<()> {
        let r = crate::dom::ranges::live_id(cx, range)?;
        let (s, _) = cx.dom().range(r).ok_or_else(gone)?;
        if !in_document(cx, s.node) || current(cx).is_some() {
            return Ok(());
        }
        if let Obj::Slab(slab) = range {
            cx.st().sel_obj = Some((r, slab));
        }
        cx.dom_quiet().set_selection(Some((r, Direction::None)));
        Ok(())
    }

    pub fn remove_range(cx: &mut Cx<'_, '_, '_>, _this: Obj, range: Obj) -> R<()> {
        let r = crate::dom::ranges::live_id(cx, range)?;
        if current(cx).map(|c| c.3) != Some(r) {
            return Err(Error::dom(
                "NotFoundError",
                "The given range isn't in document.",
            ));
        }
        clear(cx);
        Ok(())
    }

    pub fn remove_all_ranges(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        clear(cx);
        Ok(())
    }

    pub fn empty(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        remove_all_ranges(cx, this)
    }

    pub fn collapse(cx: &mut Cx<'_, '_, '_>, _this: Obj, node: Option<Obj>, offset: u32) -> R<()> {
        let Some(node) = node else {
            clear(cx);
            return Ok(());
        };
        let p = point(&cx.dom(), node.node()?, offset)?;
        if in_document(cx, p.node) {
            collapse_at(cx, p);
        }
        Ok(())
    }

    pub fn set_position(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        node: Option<Obj>,
        offset: u32,
    ) -> R<()> {
        collapse(cx, this, node, offset)
    }

    pub fn collapse_to_start(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        let (s, _, _, _) = current(cx).ok_or_else(no_range)?;
        collapse_at(cx, s);
        Ok(())
    }

    pub fn collapse_to_end(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        let (_, e, _, _) = current(cx).ok_or_else(no_range)?;
        collapse_at(cx, e);
        Ok(())
    }

    pub fn extend(cx: &mut Cx<'_, '_, '_>, _this: Obj, node: Obj, offset: u32) -> R<()> {
        let (anchor, _) = anchor_focus(cx).ok_or_else(no_range)?;
        let p = point(&cx.dom(), node.node()?, offset)?;
        if !in_document(cx, p.node) {
            return Ok(());
        }
        select(cx, anchor, p);
        Ok(())
    }

    pub fn set_base_and_extent(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        anchor_node: Obj,
        anchor_offset: u32,
        focus_node: Obj,
        focus_offset: u32,
    ) -> R<()> {
        let a = point(&cx.dom(), anchor_node.node()?, anchor_offset)?;
        let f = point(&cx.dom(), focus_node.node()?, focus_offset)?;
        if in_document(cx, a.node) && in_document(cx, f.node) {
            select(cx, a, f);
        }
        Ok(())
    }

    pub fn select_all_children(cx: &mut Cx<'_, '_, '_>, _this: Obj, node: Obj) -> R<()> {
        let n = node.node()?;
        let len = {
            let dom = cx.dom();
            if matches!(
                dom.get(n).map(|x| &x.data),
                Some(pageon_dom::NodeData::Doctype { .. })
            ) {
                return Err(crate::dom::ranges::invalid_node_type());
            }
            dom.node_length(n)
        };
        if in_document(cx, n) {
            select(cx, Boundary::new(n, 0), Boundary::new(n, len));
        }
        Ok(())
    }

    pub fn modify(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        alter: Option<String>,
        direction: Option<String>,
        granularity: Option<String>,
    ) -> R<()> {
        let lower = |s: Option<String>| s.unwrap_or_default().to_ascii_lowercase();
        let (alter, direction, granularity) = (lower(alter), lower(direction), lower(granularity));
        let extend = match alter.as_str() {
            "move" => false,
            "extend" => true,
            _ => return Ok(()),
        };
        let forward = match direction.as_str() {
            "forward" | "right" => true,
            "backward" | "left" => false,
            _ => return Ok(()),
        };
        let Some(unit) = Granularity::parse(&granularity) else {
            return Ok(());
        };
        let Some((anchor, focus)) = anchor_focus(cx) else {
            return Ok(());
        };
        let to = crate::dom::editing::step(cx, focus, Step { forward, unit });
        if extend {
            select(cx, anchor, to);
        } else {
            collapse_at(cx, to);
        }
        Ok(())
    }

    pub fn delete_from_document(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        let Some((s, e, _, _)) = current(cx) else {
            return Ok(());
        };
        let p = crate::dom::ranges::delete(cx, s, e)?;
        collapse_at(cx, p);
        Ok(())
    }

    pub fn contains_node(cx: &mut Cx<'_, '_, '_>, _this: Obj, node: Obj, partial: bool) -> R<bool> {
        let Some((s, e, _, _)) = current(cx) else {
            return Ok(false);
        };
        let n = node.node()?;
        let dom = cx.dom();
        if root_of(&dom, n) != root_of(&dom, s.node) {
            return Ok(false);
        }
        let start = Boundary::new(n, 0);
        let end = Boundary::new(n, dom.node_length(n));
        let cmp = |a, b| dom.compare_points(a, b);
        Ok(if partial {
            cmp(start, e) != Some(Position::After) && cmp(end, s) != Some(Position::Before)
        } else {
            cmp(start, s) != Some(Position::Before) && cmp(end, e) != Some(Position::After)
        })
    }

    pub fn to_string(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        // A focused text control's selected text, as browsers report it.
        let focused = cx.st().focused;
        if let Some(f) = focused
            && let Some(t) = crate::dom::editing::control_selected_text(cx, f)
        {
            return Ok(t);
        }
        Ok(current(cx).map_or_else(String::new, |(s, e, _, _)| {
            crate::dom::ranges::text(&cx.dom(), s, e)
        }))
    }
}
