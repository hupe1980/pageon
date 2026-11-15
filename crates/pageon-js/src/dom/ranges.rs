//! `AbstractRange`, `Range` and `StaticRange` (DOM §5) over the arena's live ranges, with
//! `getClientRects` from layout (CSSOM View).

use pageon_dom::{Boundary, Dom, NodeData, NodeId, Position, RangeId};

use crate::cv::{Cx, Error, Obj, R};
use crate::dom::gone;
use crate::generated::{Iface, StaticRangeInit};
use crate::realm::Slot;

pub(crate) fn invalid_node_type() -> Error {
    Error::dom(
        "InvalidNodeTypeError",
        "The node provided is of an invalid type.",
    )
}

fn index_size(offset: u32, node: NodeId, dom: &Dom) -> Error {
    Error::dom(
        "IndexSizeError",
        format!(
            "The offset {offset} is larger than the node's length ({}).",
            dom.node_length(node)
        ),
    )
}

fn hierarchy(msg: &str) -> Error {
    Error::dom("HierarchyRequestError", msg)
}

pub(crate) fn is_char_data(dom: &Dom, n: NodeId) -> bool {
    matches!(
        dom.get(n).map(|x| &x.data),
        Some(NodeData::Text(_) | NodeData::Comment(_) | NodeData::ProcessingInstruction { .. })
    )
}

pub(crate) fn is_text(dom: &Dom, n: NodeId) -> bool {
    matches!(dom.get(n).map(|x| &x.data), Some(NodeData::Text(_)))
}

fn is_doctype(dom: &Dom, n: NodeId) -> bool {
    matches!(dom.get(n).map(|x| &x.data), Some(NodeData::Doctype { .. }))
}

/// The root of a node's tree.
pub(crate) fn root_of(dom: &Dom, n: NodeId) -> NodeId {
    dom.ancestors(n).last().unwrap_or(n)
}

/// A validated boundary point (`InvalidNodeTypeError`, `IndexSizeError`).
pub(crate) fn point(dom: &Dom, node: NodeId, offset: u32) -> R<Boundary> {
    if dom.get(node).is_none() {
        return Err(gone());
    }
    if is_doctype(dom, node) {
        return Err(invalid_node_type());
    }
    if offset > dom.node_length(node) {
        return Err(index_size(offset, node, dom));
    }
    Ok(Boundary::new(node, offset))
}

fn before(dom: &Dom, a: Boundary, b: Boundary) -> bool {
    dom.compare_points(a, b) == Some(Position::Before)
}

fn after(dom: &Dom, a: Boundary, b: Boundary) -> bool {
    dom.compare_points(a, b) == Some(Position::After)
}

/// A live range's id (`Range` objects only).
pub(crate) fn live_id(cx: &Cx<'_, '_, '_>, this: Obj) -> R<RangeId> {
    match this {
        Obj::Slab(id) => match cx.st().slot(id) {
            Some(Slot::Range(r)) => Ok(*r),
            _ => Err(Error::Type("Illegal invocation".into())),
        },
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// The boundary points of a `Range` or `StaticRange`.
pub(crate) fn bounds(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(Boundary, Boundary)> {
    let slot = match this {
        Obj::Slab(id) => cx.st().slot(id).cloned(),
        _ => None,
    };
    match slot {
        Some(Slot::Range(r)) => cx.dom().range(r).ok_or_else(gone),
        Some(Slot::StaticRange(s, e)) => Ok((s, e)),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// A new `Range` object over a new live range.
pub(crate) fn new_range(cx: &Cx<'_, '_, '_>, start: Boundary, end: Boundary) -> Obj {
    let r = cx.dom_mut().create_range(start, end);
    Obj::Slab(cx.st().alloc(Iface::Range, Slot::Range(r)))
}

/// A new `StaticRange`.
pub(crate) fn new_static(cx: &Cx<'_, '_, '_>, start: Boundary, end: Boundary) -> Obj {
    Obj::Slab(
        cx.st()
            .alloc(Iface::StaticRange, Slot::StaticRange(start, end)),
    )
}

/// DOM's "set the start or end": a point in another tree or past the other end collapses the
/// range to it.
pub(crate) fn set_bound(cx: &Cx<'_, '_, '_>, r: RangeId, bp: Boundary, start: bool) -> R<()> {
    let (s, e) = cx.dom().range(r).ok_or_else(gone)?;
    let (s, e) = {
        let dom = cx.dom();
        let other_root = root_of(&dom, if start { e.node } else { s.node });
        let new_root = root_of(&dom, bp.node);
        if start {
            if other_root != new_root || after(&dom, bp, e) {
                (bp, bp)
            } else {
                (bp, e)
            }
        } else if other_root != new_root || before(&dom, bp, s) {
            (bp, bp)
        } else {
            (s, bp)
        }
    };
    cx.dom_mut().set_range(r, s, e);
    Ok(())
}

/// Whether `n` is contained in the range (DOM §5.5).
fn contained(dom: &Dom, n: NodeId, s: Boundary, e: Boundary) -> bool {
    root_of(dom, n) == root_of(dom, s.node)
        && after(dom, Boundary::new(n, 0), s)
        && before(dom, Boundary::new(n, dom.node_length(n)), e)
}

fn partially_contained(dom: &Dom, n: NodeId, s: Boundary, e: Boundary) -> bool {
    dom.is_inclusive_ancestor(n, s.node) != dom.is_inclusive_ancestor(n, e.node)
}

fn substring(dom: &Dom, n: NodeId, from: u32, to: u32) -> String {
    let data = match dom.get(n).map(|x| &x.data) {
        Some(NodeData::Text(t) | NodeData::Comment(t)) => t.clone(),
        Some(NodeData::ProcessingInstruction { data, .. }) => data.clone(),
        _ => return String::new(),
    };
    let units: Vec<u16> = data.encode_utf16().collect();
    let to = (to as usize).min(units.len());
    let from = (from as usize).min(to);
    String::from_utf16_lossy(&units[from..to])
}

fn clone_shallow(cx: &Cx<'_, '_, '_>, n: NodeId) -> R<NodeId> {
    crate::dom::copy_within(&mut cx.dom_mut(), n, false).ok_or_else(gone)
}

fn clone_with_data(cx: &Cx<'_, '_, '_>, n: NodeId, data: &str) -> R<NodeId> {
    let c = clone_shallow(cx, n)?;
    cx.dom_mut().set_text(c, data).map_err(|_| gone())?;
    Ok(c)
}

fn append(cx: &Cx<'_, '_, '_>, parent: NodeId, child: NodeId) -> R<()> {
    crate::dom::insert(cx, parent, child, None)
}

/// The point after the deepest common start-side node (DOM's "new node and new offset").
fn new_point(dom: &Dom, s: Boundary, e: Boundary) -> Boundary {
    if dom.is_inclusive_ancestor(s.node, e.node) {
        return s;
    }
    let mut reference = s.node;
    while let Some(p) = dom.parent(reference) {
        if dom.is_inclusive_ancestor(p, e.node) {
            return Boundary::new(p, dom.index_of(reference) + 1);
        }
        reference = p;
    }
    s
}

struct Parts {
    common: NodeId,
    first: Option<NodeId>,
    last: Option<NodeId>,
    contained: Vec<NodeId>,
}

fn parts(dom: &Dom, s: Boundary, e: Boundary) -> R<Parts> {
    let mut common = s.node;
    while !dom.is_inclusive_ancestor(common, e.node) {
        common = dom.parent(common).ok_or_else(gone)?;
    }
    let child_toward = |n: NodeId| {
        dom.children(common)
            .find(|&c| dom.is_inclusive_ancestor(c, n))
    };
    let first = (!dom.is_inclusive_ancestor(s.node, e.node))
        .then(|| child_toward(s.node))
        .flatten();
    let last = (!dom.is_inclusive_ancestor(e.node, s.node))
        .then(|| child_toward(e.node))
        .flatten();
    let contained: Vec<NodeId> = dom
        .children(common)
        .filter(|&c| contained(dom, c, s, e))
        .collect();
    if contained.iter().any(|&c| is_doctype(dom, c)) {
        return Err(hierarchy(
            "A range containing a doctype cannot be cloned or extracted.",
        ));
    }
    Ok(Parts {
        common,
        first,
        last,
        contained,
    })
}

/// DOM's "clone the contents" (`extract` false) or "extract" (`extract` true) of a range.
fn contents(cx: &Cx<'_, '_, '_>, s: Boundary, e: Boundary, extract: bool) -> R<NodeId> {
    let frag = cx.dom_mut().create(NodeData::DocumentFragment);
    if s == e {
        return Ok(frag);
    }
    if s.node == e.node && is_char_data(&cx.dom(), s.node) {
        let data = substring(&cx.dom(), s.node, s.offset, e.offset);
        let c = clone_with_data(cx, s.node, &data)?;
        append(cx, frag, c)?;
        if extract {
            cx.dom_mut()
                .replace_data(s.node, s.offset, e.offset - s.offset, "")
                .map_err(|_| gone())?;
        }
        return Ok(frag);
    }
    let p = parts(&cx.dom(), s, e)?;
    let _ = p.common;
    let new = new_point(&cx.dom(), s, e);
    if let Some(first) = p.first {
        if is_char_data(&cx.dom(), first) {
            let len = cx.dom().node_length(s.node);
            let data = substring(&cx.dom(), s.node, s.offset, len);
            let c = clone_with_data(cx, s.node, &data)?;
            append(cx, frag, c)?;
            if extract {
                cx.dom_mut()
                    .replace_data(s.node, s.offset, len - s.offset, "")
                    .map_err(|_| gone())?;
            }
        } else {
            let c = clone_shallow(cx, first)?;
            append(cx, frag, c)?;
            let len = cx.dom().node_length(first);
            let sub = contents(cx, s, Boundary::new(first, len), extract)?;
            append(cx, c, sub)?;
        }
    }
    for &child in &p.contained {
        if extract {
            append(cx, frag, child)?;
        } else {
            let c = crate::dom::copy_within(&mut cx.dom_mut(), child, true).ok_or_else(gone)?;
            append(cx, frag, c)?;
        }
    }
    if let Some(last) = p.last {
        if is_char_data(&cx.dom(), last) {
            let data = substring(&cx.dom(), e.node, 0, e.offset);
            let c = clone_with_data(cx, e.node, &data)?;
            append(cx, frag, c)?;
            if extract {
                cx.dom_mut()
                    .replace_data(e.node, 0, e.offset, "")
                    .map_err(|_| gone())?;
            }
        } else {
            let c = clone_shallow(cx, last)?;
            append(cx, frag, c)?;
            let sub = contents(cx, Boundary::new(last, 0), e, extract)?;
            append(cx, c, sub)?;
        }
    }
    let _ = new;
    Ok(frag)
}

/// Extracts `s..e` into a new fragment (DOM's "extract").
pub(crate) fn extract(cx: &Cx<'_, '_, '_>, s: Boundary, e: Boundary) -> R<NodeId> {
    contents(cx, s, e, true)
}

/// Clones `s..e` into a new fragment (DOM's "clone the contents").
pub(crate) fn clone(cx: &Cx<'_, '_, '_>, s: Boundary, e: Boundary) -> R<NodeId> {
    contents(cx, s, e, false)
}

/// DOM's "delete the contents" of a range; returns where the range collapses to.
pub(crate) fn delete(cx: &Cx<'_, '_, '_>, s: Boundary, e: Boundary) -> R<Boundary> {
    if s == e {
        return Ok(s);
    }
    if s.node == e.node && is_char_data(&cx.dom(), s.node) {
        cx.dom_mut()
            .replace_data(s.node, s.offset, e.offset - s.offset, "")
            .map_err(|_| gone())?;
        return Ok(s);
    }
    let (remove, new) = {
        let dom = cx.dom();
        let mut remove = Vec::new();
        for edge in dom.traverse(root_of(&dom, s.node)) {
            if let pageon_dom::Edge::Enter(n) = edge
                && contained(&dom, n, s, e)
                && !dom.parent(n).is_some_and(|p| contained(&dom, p, s, e))
            {
                remove.push(n);
            }
        }
        (remove, new_point(&dom, s, e))
    };
    if is_char_data(&cx.dom(), s.node) {
        let len = cx.dom().node_length(s.node);
        cx.dom_mut()
            .replace_data(s.node, s.offset, len - s.offset, "")
            .map_err(|_| gone())?;
    }
    for n in remove {
        crate::dom::detach(cx, n)?;
    }
    if is_char_data(&cx.dom(), e.node) {
        cx.dom_mut()
            .replace_data(e.node, 0, e.offset, "")
            .map_err(|_| gone())?;
    }
    Ok(new)
}

/// DOM's "insert" a node at a range's start.
pub(crate) fn insert_at(cx: &Cx<'_, '_, '_>, r: RangeId, node: NodeId) -> R<()> {
    let (s, _) = cx.dom().range(r).ok_or_else(gone)?;
    let (parent, mut reference) = {
        let dom = cx.dom();
        let data = &dom.get(s.node).ok_or_else(gone)?.data;
        if matches!(
            data,
            NodeData::Comment(_) | NodeData::ProcessingInstruction { .. }
        ) || (is_text(&dom, s.node) && dom.parent(s.node).is_none())
            || s.node == node
        {
            return Err(hierarchy("The range's start cannot hold the node."));
        }
        let reference = if is_text(&dom, s.node) {
            Some(s.node)
        } else {
            dom.child_at(s.node, s.offset)
        };
        let parent = match reference {
            Some(rf) => dom.parent(rf).ok_or_else(gone)?,
            None => s.node,
        };
        crate::dom::check_insert(&dom, parent, node, reference)?;
        (parent, reference)
    };
    if is_text(&cx.dom(), s.node) {
        reference = Some(
            cx.dom_mut()
                .split_text(s.node, s.offset)
                .map_err(|_| gone())?,
        );
    }
    if reference == Some(node) {
        reference = cx.dom().next_sibling(node);
    }
    if cx.dom().parent(node).is_some() {
        crate::dom::detach(cx, node)?;
    }
    let new_offset = {
        let dom = cx.dom();
        let base = match reference {
            Some(rf) => dom.index_of(rf),
            None => dom.node_length(parent),
        };
        base + if matches!(
            dom.get(node).map(|x| &x.data),
            Some(NodeData::DocumentFragment)
        ) {
            dom.node_length(node)
        } else {
            1
        }
    };
    crate::dom::insert(cx, parent, node, reference)?;
    let (s, e) = cx.dom().range(r).ok_or_else(gone)?;
    if s == e {
        cx.dom_mut()
            .set_range(r, s, Boundary::new(parent, new_offset));
    }
    Ok(())
}

/// The text of a range (Range's stringifier).
pub(crate) fn text(dom: &Dom, s: Boundary, e: Boundary) -> String {
    if s.node == e.node && is_text(dom, s.node) {
        return substring(dom, s.node, s.offset, e.offset);
    }
    let mut out = String::new();
    if is_text(dom, s.node) {
        out.push_str(&substring(dom, s.node, s.offset, dom.node_length(s.node)));
    }
    for edge in dom.traverse(root_of(dom, s.node)) {
        if let pageon_dom::Edge::Enter(n) = edge
            && is_text(dom, n)
            && contained(dom, n, s, e)
        {
            out.push_str(&substring(dom, n, 0, dom.node_length(n)));
        }
    }
    if is_text(dom, e.node) {
        out.push_str(&substring(dom, e.node, 0, e.offset));
    }
    out
}

/// The client rectangles of a range: its text from layout, and the border boxes of the
/// elements it contains whose parents it does not.
pub(crate) fn rects(cx: &Cx<'_, '_, '_>, s: Boundary, e: Boundary) -> Vec<pageon_layout::Rect> {
    let layout = cx.realm.layout();
    let dom = cx.dom();
    let mut out = Vec::new();
    if s.node == e.node && is_text(&dom, s.node) {
        return layout.text_rects(s.node, s.offset, e.offset);
    }
    if is_text(&dom, s.node) {
        out.extend(layout.text_rects(s.node, s.offset, dom.node_length(s.node)));
    }
    for edge in dom.traverse(root_of(&dom, s.node)) {
        let pageon_dom::Edge::Enter(n) = edge else {
            continue;
        };
        if !contained(&dom, n, s, e) {
            continue;
        }
        if is_text(&dom, n) {
            out.extend(layout.text_rects(n, 0, dom.node_length(n)));
        } else if dom.get(n).is_some_and(|x| x.local_name().is_some())
            && !dom.parent(n).is_some_and(|p| contained(&dom, p, s, e))
            && let Some(r) = layout.client_rect(n)
        {
            out.push(r);
        }
    }
    if is_text(&dom, e.node) && e.node != s.node {
        out.extend(layout.text_rects(e.node, 0, e.offset));
    }
    out
}

fn rect_list(cx: &mut Cx<'_, '_, '_>, rs: &[pageon_layout::Rect]) -> Obj {
    let v = rs
        .iter()
        .map(|r| {
            [
                f64::from(r.x),
                f64::from(r.y),
                f64::from(r.width),
                f64::from(r.height),
            ]
        })
        .collect();
    cx.alloc(Iface::DOMRectList, Slot::RectList(v))
}

/// The union of rectangles (`getBoundingClientRect`), empty ones ignored unless all are.
pub(crate) fn union(rs: &[pageon_layout::Rect]) -> [f64; 4] {
    let some: Vec<&pageon_layout::Rect> = rs
        .iter()
        .filter(|r| r.width > 0.0 || r.height > 0.0)
        .collect();
    let pick: Vec<&pageon_layout::Rect> = if some.is_empty() {
        rs.iter().collect()
    } else {
        some
    };
    let Some(first) = pick.first() else {
        return [0.0; 4];
    };
    let (mut x0, mut y0) = (first.x, first.y);
    let (mut x1, mut y1) = (first.x + first.width, first.y + first.height);
    for r in &pick[1..] {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.x + r.width);
        y1 = y1.max(r.y + r.height);
    }
    [
        f64::from(x0),
        f64::from(y0),
        f64::from(x1 - x0),
        f64::from(y1 - y0),
    ]
}

/// `AbstractRange`.
pub mod abstract_range {
    use super::*;

    pub fn get_start_container(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(bounds(cx, this)?.0.node))
    }

    pub fn get_start_offset(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(bounds(cx, this)?.0.offset)
    }

    pub fn get_end_container(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(bounds(cx, this)?.1.node))
    }

    pub fn get_end_offset(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(bounds(cx, this)?.1.offset)
    }

    pub fn get_collapsed(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        let (s, e) = bounds(cx, this)?;
        Ok(s == e)
    }
}

/// `StaticRange`.
pub mod static_range {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, init: StaticRangeInit) -> R<Obj> {
        let need = |o: Option<Obj>, m: &str| {
            o.ok_or_else(|| {
                Error::Type(format!(
                    "Failed to construct 'StaticRange': required member {m} is undefined."
                ))
            })
        };
        let sc = need(init.start_container, "startContainer")?.node()?;
        let ec = need(init.end_container, "endContainer")?.node()?;
        let so = init
            .start_offset
            .ok_or_else(|| Error::Type("startOffset is required".into()))?;
        let eo = init
            .end_offset
            .ok_or_else(|| Error::Type("endOffset is required".into()))?;
        {
            let dom = cx.dom();
            if is_doctype(&dom, sc) || is_doctype(&dom, ec) {
                return Err(invalid_node_type());
            }
        }
        Ok(new_static(cx, Boundary::new(sc, so), Boundary::new(ec, eo)))
    }
}

/// `Range`.
pub mod range {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        let root = cx.dom().root();
        let p = Boundary::new(root, 0);
        Ok(new_range(cx, p, p))
    }

    pub fn get_common_ancestor_container(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let (s, e) = bounds(cx, this)?;
        let dom = cx.dom();
        let mut c = s.node;
        while !dom.is_inclusive_ancestor(c, e.node) {
            c = dom.parent(c).ok_or_else(gone)?;
        }
        Ok(Obj::Node(c))
    }

    pub fn set_start(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj, offset: u32) -> R<()> {
        let r = live_id(cx, this)?;
        let bp = point(&cx.dom(), node.node()?, offset)?;
        set_bound(cx, r, bp, true)
    }

    pub fn set_end(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj, offset: u32) -> R<()> {
        let r = live_id(cx, this)?;
        let bp = point(&cx.dom(), node.node()?, offset)?;
        set_bound(cx, r, bp, false)
    }

    fn beside(cx: &Cx<'_, '_, '_>, node: Obj, after: bool) -> R<Boundary> {
        let n = node.node()?;
        let dom = cx.dom();
        let p = dom.parent(n).ok_or_else(invalid_node_type)?;
        Ok(Boundary::new(p, dom.index_of(n) + u32::from(after)))
    }

    pub fn set_start_before(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj) -> R<()> {
        let r = live_id(cx, this)?;
        let bp = beside(cx, node, false)?;
        set_bound(cx, r, bp, true)
    }

    pub fn set_start_after(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj) -> R<()> {
        let r = live_id(cx, this)?;
        let bp = beside(cx, node, true)?;
        set_bound(cx, r, bp, true)
    }

    pub fn set_end_before(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj) -> R<()> {
        let r = live_id(cx, this)?;
        let bp = beside(cx, node, false)?;
        set_bound(cx, r, bp, false)
    }

    pub fn set_end_after(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj) -> R<()> {
        let r = live_id(cx, this)?;
        let bp = beside(cx, node, true)?;
        set_bound(cx, r, bp, false)
    }

    pub fn collapse(cx: &mut Cx<'_, '_, '_>, this: Obj, to_start: bool) -> R<()> {
        let r = live_id(cx, this)?;
        let (s, e) = bounds(cx, this)?;
        let p = if to_start { s } else { e };
        cx.dom_mut().set_range(r, p, p);
        Ok(())
    }

    pub fn select_node(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj) -> R<()> {
        let r = live_id(cx, this)?;
        let s = beside(cx, node, false)?;
        let e = Boundary::new(s.node, s.offset + 1);
        cx.dom_mut().set_range(r, s, e);
        Ok(())
    }

    pub fn select_node_contents(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj) -> R<()> {
        let r = live_id(cx, this)?;
        let n = node.node()?;
        let len = {
            let dom = cx.dom();
            if is_doctype(&dom, n) {
                return Err(invalid_node_type());
            }
            dom.node_length(n)
        };
        cx.dom_mut()
            .set_range(r, Boundary::new(n, 0), Boundary::new(n, len));
        Ok(())
    }

    pub fn compare_boundary_points(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        how: u16,
        source: Obj,
    ) -> R<i16> {
        if how > 3 {
            return Err(Error::dom(
                "NotSupportedError",
                "The comparison method provided must be one of 'START_TO_START', 'START_TO_END', 'END_TO_END', or 'END_TO_START'.",
            ));
        }
        let (s, e) = bounds(cx, this)?;
        let (os, oe) = bounds(cx, source)?;
        let dom = cx.dom();
        if root_of(&dom, s.node) != root_of(&dom, os.node) {
            return Err(Error::dom(
                "WrongDocumentError",
                "The two ranges are in different documents.",
            ));
        }
        let (a, b) = match how {
            0 => (s, os),
            1 => (e, os),
            2 => (e, oe),
            _ => (s, oe),
        };
        Ok(match dom.compare_points(a, b) {
            Some(Position::Before) => -1,
            Some(Position::After) => 1,
            _ => 0,
        })
    }

    pub fn delete_contents(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let r = live_id(cx, this)?;
        let (s, e) = bounds(cx, this)?;
        let p = delete(cx, s, e)?;
        cx.dom_mut().set_range(r, p, p);
        Ok(())
    }

    pub fn extract_contents(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let r = live_id(cx, this)?;
        let (s, e) = bounds(cx, this)?;
        let new = new_point(&cx.dom(), s, e);
        let f = contents(cx, s, e, true)?;
        if s != e {
            cx.dom_mut().set_range(r, new, new);
        }
        Ok(Obj::Node(f))
    }

    pub fn clone_contents(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let (s, e) = bounds(cx, this)?;
        Ok(Obj::Node(contents(cx, s, e, false)?))
    }

    pub fn insert_node(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj) -> R<()> {
        let r = live_id(cx, this)?;
        insert_at(cx, r, node.node()?)
    }

    pub fn surround_contents(cx: &mut Cx<'_, '_, '_>, this: Obj, new_parent: Obj) -> R<()> {
        let r = live_id(cx, this)?;
        let np = new_parent.node()?;
        let (s, e) = bounds(cx, this)?;
        {
            let dom = cx.dom();
            let bad = dom.traverse(root_of(&dom, s.node)).any(|edge| {
                matches!(edge, pageon_dom::Edge::Enter(n)
                    if !is_text(&dom, n) && partially_contained(&dom, n, s, e))
            });
            if bad {
                return Err(Error::dom(
                    "InvalidStateError",
                    "The Range has partially selected a non-Text node.",
                ));
            }
            if matches!(
                dom.get(np).map(|x| &x.data),
                Some(NodeData::Document | NodeData::Doctype { .. } | NodeData::DocumentFragment)
            ) {
                return Err(invalid_node_type());
            }
        }
        let frag = contents(cx, s, e, true)?;
        let new = new_point(&cx.dom(), s, e);
        if s != e {
            cx.dom_mut().set_range(r, new, new);
        }
        let kids: Vec<NodeId> = cx.dom().children(np).collect();
        for k in kids {
            crate::dom::detach(cx, k)?;
        }
        insert_at(cx, r, np)?;
        append(cx, np, frag)?;
        select_node(cx, this, Obj::Node(np))
    }

    pub fn clone_range(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let (s, e) = bounds(cx, this)?;
        Ok(new_range(cx, s, e))
    }

    pub fn detach(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        Ok(())
    }

    fn check(
        cx: &Cx<'_, '_, '_>,
        this: Obj,
        node: Obj,
        offset: u32,
    ) -> R<Option<(Boundary, Boundary, Boundary)>> {
        let (s, e) = bounds(cx, this)?;
        let n = node.node()?;
        let dom = cx.dom();
        if root_of(&dom, n) != root_of(&dom, s.node) {
            return Ok(None);
        }
        let bp = point(&dom, n, offset)?;
        Ok(Some((s, e, bp)))
    }

    pub fn is_point_in_range(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        node: Obj,
        offset: u32,
    ) -> R<bool> {
        let Some((s, e, bp)) = check(cx, this, node, offset)? else {
            return Ok(false);
        };
        let dom = cx.dom();
        Ok(!before(&dom, bp, s) && !after(&dom, bp, e))
    }

    pub fn compare_point(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj, offset: u32) -> R<i16> {
        let Some((s, e, bp)) = check(cx, this, node, offset)? else {
            return Err(Error::dom(
                "WrongDocumentError",
                "The node provided and the Range are not in the same tree.",
            ));
        };
        let dom = cx.dom();
        Ok(if before(&dom, bp, s) {
            -1
        } else if after(&dom, bp, e) {
            1
        } else {
            0
        })
    }

    pub fn intersects_node(cx: &mut Cx<'_, '_, '_>, this: Obj, node: Obj) -> R<bool> {
        let (s, e) = bounds(cx, this)?;
        let n = node.node()?;
        let dom = cx.dom();
        if root_of(&dom, n) != root_of(&dom, s.node) {
            return Ok(false);
        }
        let Some(p) = dom.parent(n) else {
            return Ok(true);
        };
        let i = dom.index_of(n);
        Ok(before(&dom, Boundary::new(p, i), e) && after(&dom, Boundary::new(p, i + 1), s))
    }

    pub fn to_string(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let (s, e) = bounds(cx, this)?;
        Ok(text(&cx.dom(), s, e))
    }

    pub fn get_client_rects(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let (s, e) = bounds(cx, this)?;
        let rs = rects(cx, s, e);
        Ok(rect_list(cx, &rs))
    }

    pub fn get_bounding_client_rect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let (s, e) = bounds(cx, this)?;
        let rs = rects(cx, s, e);
        let u = union(&rs);
        Ok(cx.alloc(Iface::DOMRect, Slot::Rect(u)))
    }
}

/// `CaretPosition`.
pub mod caret_position {
    use super::*;

    fn caret(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(NodeId, u32)> {
        match this {
            Obj::Slab(id) => match cx.st().slot(id) {
                Some(Slot::CaretPosition(n, o)) => Ok((*n, *o)),
                _ => Err(Error::Type("Illegal invocation".into())),
            },
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_offset_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(caret(cx, this)?.0))
    }

    pub fn get_offset(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(caret(cx, this)?.1)
    }

    pub fn get_client_rect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let (n, o) = caret(cx, this)?;
        let p = Boundary::new(n, o);
        let rs = rects(cx, p, p);
        Ok(rs.first().map(|r| {
            let r = [
                f64::from(r.x),
                f64::from(r.y),
                f64::from(r.width),
                f64::from(r.height),
            ];
            cx.alloc(Iface::DOMRect, Slot::Rect(r))
        }))
    }
}
