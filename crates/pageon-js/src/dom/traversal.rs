//! `TreeWalker`, `NodeIterator`, `document.createTreeWalker` and `createNodeIterator` (DOM §6):
//! the standard's traversal algorithms, with `whatToShow` and a filter (a function, or an object
//! with `acceptNode`). A node iterator follows removals (its pre-removing steps).

use pageon_dom::{NodeData, NodeId};

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::Iface;
use crate::realm::Slot;

const ACCEPT: u16 = 1;
const REJECT: u16 = 2;
const SKIP: u16 = 3;

/// A `TreeWalker`.
#[derive(Debug, Clone)]
pub struct WalkerData {
    root: NodeId,
    what_to_show: u32,
    filter: Option<Value>,
    current: NodeId,
    active: bool,
    /// A `NodeIterator`'s `pointerBeforeReferenceNode` (`current` is its reference node);
    /// `None` for a `TreeWalker`.
    before: Option<bool>,
}

fn walker(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(u32, WalkerData)> {
    let id = this.slab()?;
    match cx.realm.st.borrow().slot(id) {
        Some(Slot::Walker(w)) => Ok((id, (**w).clone())),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn set_current(cx: &Cx<'_, '_, '_>, id: u32, n: NodeId) {
    if let Some(Slot::Walker(w)) = cx.realm.st.borrow_mut().slot_mut(id) {
        w.current = n;
    }
}

fn set_active(cx: &Cx<'_, '_, '_>, id: u32, on: bool) {
    if let Some(Slot::Walker(w)) = cx.realm.st.borrow_mut().slot_mut(id) {
        w.active = on;
    }
}

fn node_type(cx: &Cx<'_, '_, '_>, n: NodeId) -> u32 {
    match cx.dom().get(n).map(|x| &x.data) {
        Some(NodeData::Element { .. }) => 1,
        Some(NodeData::Text(_)) => 3,
        Some(NodeData::ProcessingInstruction { .. }) => 7,
        Some(NodeData::Comment(_)) => 8,
        Some(NodeData::Document) => 9,
        Some(NodeData::Doctype { .. }) => 10,
        Some(NodeData::DocumentFragment | NodeData::ShadowRoot { .. }) => 11,
        None => 0,
    }
}

/// "Filter a node" (DOM §6.1).
fn filter(cx: &mut Cx<'_, '_, '_>, id: u32, w: &WalkerData, n: NodeId) -> R<u16> {
    if w.active {
        return Err(Error::Dom(
            "InvalidStateError",
            "The filter is already running.".into(),
        ));
    }
    let t = node_type(cx, n);
    if t == 0 || w.what_to_show & (1 << (t - 1)) == 0 {
        return Ok(SKIP);
    }
    let Some(f) = &w.filter else {
        return Ok(ACCEPT);
    };
    set_active(cx, id, true);
    let node: v8::Local<v8::Value> = cx.wrap(Obj::Node(n)).into();
    let fv = cx.local(f);
    let r = {
        let scope = &mut *cx.scope;
        v8::tc_scope!(let tc, scope);
        let call = if let Ok(func) = v8::Local::<v8::Function>::try_from(fv) {
            let undefined = v8::undefined(tc).into();
            func.call(tc, undefined, &[node])
        } else if let Ok(o) = v8::Local::<v8::Object>::try_from(fv) {
            let key = v8::String::new(tc, "acceptNode").map(Into::into);
            match key
                .and_then(|k| o.get(tc, k))
                .and_then(|m| v8::Local::<v8::Function>::try_from(m).ok())
            {
                Some(m) => m.call(tc, o.into(), &[node]),
                None => {
                    let msg = v8::String::new(tc, "The provided callback is no longer runnable.");
                    if let Some(msg) = msg {
                        let ex = v8::Exception::type_error(tc, msg);
                        tc.throw_exception(ex);
                    }
                    None
                }
            }
        } else {
            None
        };
        match call {
            Some(v) => Ok(v.uint32_value(tc).unwrap_or(0) as u16),
            None if tc.has_terminated() || tc.is_execution_terminating() => Err(Error::Terminated),
            None => match tc.exception() {
                Some(ex) => Err(Error::Thrown(v8::Global::new(tc, ex))),
                None => Ok(SKIP),
            },
        }
    };
    set_active(cx, id, false);
    r
}

#[derive(Clone, Copy)]
enum Dir {
    First,
    Last,
}

fn first_or_last(cx: &Cx<'_, '_, '_>, n: NodeId, d: Dir) -> Option<NodeId> {
    match d {
        Dir::First => cx.dom().first_child(n),
        Dir::Last => cx.dom().last_child(n),
    }
}

fn sibling(cx: &Cx<'_, '_, '_>, n: NodeId, next: bool) -> Option<NodeId> {
    if next {
        cx.dom().next_sibling(n)
    } else {
        cx.dom().prev_sibling(n)
    }
}

fn parent(cx: &Cx<'_, '_, '_>, n: NodeId) -> Option<NodeId> {
    cx.dom().parent(n)
}

/// "Traverse children" (DOM §6.2).
fn children(cx: &mut Cx<'_, '_, '_>, this: Obj, d: Dir) -> R<Option<Obj>> {
    let (id, w) = walker(cx, this)?;
    let mut node = first_or_last(cx, w.current, d);
    'outer: while let Some(n) = node {
        match filter(cx, id, &w, n)? {
            ACCEPT => {
                set_current(cx, id, n);
                return Ok(Some(Obj::Node(n)));
            }
            SKIP => {
                if let Some(c) = first_or_last(cx, n, d) {
                    node = Some(c);
                    continue;
                }
            }
            _ => {}
        }
        let mut cur = n;
        loop {
            if let Some(s) = sibling(cx, cur, matches!(d, Dir::First)) {
                node = Some(s);
                continue 'outer;
            }
            match parent(cx, cur) {
                Some(p) if p != w.root && p != w.current => cur = p,
                _ => return Ok(None),
            }
        }
    }
    Ok(None)
}

/// "Traverse siblings" (DOM §6.2).
fn siblings(cx: &mut Cx<'_, '_, '_>, this: Obj, next: bool) -> R<Option<Obj>> {
    let (id, w) = walker(cx, this)?;
    let mut node = w.current;
    if node == w.root {
        return Ok(None);
    }
    loop {
        let mut sib = sibling(cx, node, next);
        while let Some(s) = sib {
            node = s;
            let result = filter(cx, id, &w, node)?;
            if result == ACCEPT {
                set_current(cx, id, node);
                return Ok(Some(Obj::Node(node)));
            }
            sib = first_or_last(cx, node, if next { Dir::First } else { Dir::Last });
            if result == REJECT || sib.is_none() {
                sib = sibling(cx, node, next);
            }
        }
        match parent(cx, node) {
            Some(p) if p != w.root => node = p,
            _ => return Ok(None),
        }
        if filter(cx, id, &w, node)? == ACCEPT {
            return Ok(None);
        }
    }
}

pub mod tree_walker {
    use super::*;

    pub fn get_root(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(walker(cx, this)?.1.root))
    }

    pub fn get_what_to_show(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(walker(cx, this)?.1.what_to_show)
    }

    pub fn get_filter(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Value>> {
        Ok(walker(cx, this)?.1.filter)
    }

    pub fn get_current_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(walker(cx, this)?.1.current))
    }

    pub fn set_current_node(cx: &mut Cx<'_, '_, '_>, this: Obj, n: Obj) -> R<()> {
        let (id, _) = walker(cx, this)?;
        set_current(cx, id, n.node()?);
        Ok(())
    }

    pub fn parent_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let (id, w) = walker(cx, this)?;
        let mut node = Some(w.current);
        while let Some(n) = node {
            if n == w.root {
                break;
            }
            node = parent(cx, n);
            if let Some(p) = node
                && filter(cx, id, &w, p)? == ACCEPT
            {
                set_current(cx, id, p);
                return Ok(Some(Obj::Node(p)));
            }
        }
        Ok(None)
    }

    pub fn first_child(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        children(cx, this, Dir::First)
    }

    pub fn last_child(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        children(cx, this, Dir::Last)
    }

    pub fn previous_sibling(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        siblings(cx, this, false)
    }

    pub fn next_sibling(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        siblings(cx, this, true)
    }

    pub fn previous_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let (id, w) = walker(cx, this)?;
        let mut node = w.current;
        while node != w.root {
            let mut sib = sibling(cx, node, false);
            while let Some(s) = sib {
                node = s;
                let mut result = filter(cx, id, &w, node)?;
                while result != REJECT
                    && let Some(c) = first_or_last(cx, node, Dir::Last)
                {
                    node = c;
                    result = filter(cx, id, &w, node)?;
                }
                if result == ACCEPT {
                    set_current(cx, id, node);
                    return Ok(Some(Obj::Node(node)));
                }
                sib = sibling(cx, node, false);
            }
            match parent(cx, node) {
                Some(p) if node != w.root => node = p,
                _ => return Ok(None),
            }
            if filter(cx, id, &w, node)? == ACCEPT {
                set_current(cx, id, node);
                return Ok(Some(Obj::Node(node)));
            }
        }
        Ok(None)
    }

    pub fn next_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let (id, w) = walker(cx, this)?;
        let mut node = w.current;
        let mut result = ACCEPT;
        loop {
            while result != REJECT
                && let Some(c) = first_or_last(cx, node, Dir::First)
            {
                node = c;
                result = filter(cx, id, &w, node)?;
                if result == ACCEPT {
                    set_current(cx, id, node);
                    return Ok(Some(Obj::Node(node)));
                }
            }
            let mut temporary = Some(node);
            let mut next = None;
            while let Some(t) = temporary {
                if t == w.root {
                    return Ok(None);
                }
                if let Some(s) = sibling(cx, t, true) {
                    next = Some(s);
                    break;
                }
                temporary = parent(cx, t);
            }
            let Some(n) = next else { return Ok(None) };
            node = n;
            result = filter(cx, id, &w, node)?;
            if result == ACCEPT {
                set_current(cx, id, node);
                return Ok(Some(Obj::Node(node)));
            }
        }
    }
}

/// `document.createTreeWalker(root, whatToShow, filter)`.
pub(crate) fn create(
    cx: &mut Cx<'_, '_, '_>,
    root: Obj,
    what_to_show: u32,
    filter: Option<Value>,
) -> R<Obj> {
    let root = root.node()?;
    let filter = filter.filter(|f| !cx.local(f).is_null_or_undefined());
    Ok(cx.alloc(
        Iface::TreeWalker,
        Slot::Walker(Box::new(WalkerData {
            root,
            what_to_show,
            filter,
            current: root,
            active: false,
            before: None,
        })),
    ))
}

/// `createNodeIterator(root, whatToShow, filter)`.
pub(crate) fn create_iterator(
    cx: &mut Cx<'_, '_, '_>,
    root: Obj,
    what_to_show: u32,
    filter: Option<Value>,
) -> R<Obj> {
    let root = root.node()?;
    let filter = filter.filter(|f| !cx.local(f).is_null_or_undefined());
    let o = cx.alloc(
        Iface::NodeIterator,
        Slot::Walker(Box::new(WalkerData {
            root,
            what_to_show,
            filter,
            current: root,
            active: false,
            before: Some(true),
        })),
    );
    if let Ok(id) = o.slab() {
        cx.st().iterators.push(id);
    }
    Ok(o)
}

/// The node after `n` in tree order, within `root`.
fn following(cx: &Cx<'_, '_, '_>, root: NodeId, n: NodeId) -> Option<NodeId> {
    let dom = cx.dom();
    if let Some(c) = dom.first_child(n) {
        return Some(c);
    }
    let mut cur = n;
    loop {
        if cur == root {
            return None;
        }
        if let Some(s) = dom.next_sibling(cur) {
            return Some(s);
        }
        cur = dom.parent(cur)?;
    }
}

/// The last inclusive descendant of `n` in tree order.
fn last_descendant(cx: &Cx<'_, '_, '_>, mut n: NodeId) -> NodeId {
    let dom = cx.dom();
    while let Some(c) = dom.last_child(n) {
        n = c;
    }
    n
}

/// The node before `n` in tree order, within `root`.
fn preceding(cx: &Cx<'_, '_, '_>, root: NodeId, n: NodeId) -> Option<NodeId> {
    if n == root {
        return None;
    }
    match cx.dom().prev_sibling(n) {
        Some(s) => Some(last_descendant(cx, s)),
        None => cx.dom().parent(n),
    }
}

/// The node iterators' pre-removing steps (DOM §6.1), before `removed` leaves its parent.
pub(crate) fn pre_removing(cx: &Cx<'_, '_, '_>, removed: NodeId) {
    let ids = cx.st().iterators.clone();
    for id in ids {
        let Some(w) = (match cx.st().slot(id) {
            Some(Slot::Walker(w)) => Some((**w).clone()),
            _ => None,
        }) else {
            continue;
        };
        let Some(mut before) = w.before else { continue };
        if removed == w.root || !cx.dom().is_inclusive_ancestor(removed, w.current) {
            continue;
        }
        let mut reference = None;
        if before {
            // The first node after the removed subtree, if it is still within the root.
            let last = last_descendant(cx, removed);
            match following(cx, w.root, last) {
                Some(x) => reference = Some(x),
                None => before = false,
            }
        }
        let reference = reference.or_else(|| match cx.dom().prev_sibling(removed) {
            Some(s) => Some(last_descendant(cx, s)),
            None => cx.dom().parent(removed),
        });
        if let (Some(r), Some(Slot::Walker(d))) = (reference, cx.st().slot_mut(id)) {
            d.current = r;
            d.before = Some(before);
        }
    }
}

pub mod node_iterator {
    use super::*;

    fn iter(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(u32, WalkerData)> {
        let (id, w) = walker(cx, this)?;
        if w.before.is_none() {
            return Err(Error::Type("Illegal invocation".into()));
        }
        Ok((id, w))
    }

    /// "Traverse" (DOM §6.1).
    fn traverse(cx: &mut Cx<'_, '_, '_>, this: Obj, next: bool) -> R<Option<Obj>> {
        let (id, w) = iter(cx, this)?;
        let mut node = w.current;
        let mut before = w.before.unwrap_or(true);
        loop {
            if next {
                if before {
                    before = false;
                } else {
                    match following(cx, w.root, node) {
                        Some(n) => node = n,
                        None => return Ok(None),
                    }
                }
            } else if !before {
                before = true;
            } else {
                match preceding(cx, w.root, node) {
                    Some(n) => node = n,
                    None => return Ok(None),
                }
            }
            let w = walker(cx, this)?.1;
            if filter(cx, id, &w, node)? == ACCEPT {
                break;
            }
        }
        if let Some(Slot::Walker(d)) = cx.st().slot_mut(id) {
            d.current = node;
            d.before = Some(before);
        }
        Ok(Some(Obj::Node(node)))
    }

    pub fn next_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        traverse(cx, this, true)
    }

    pub fn previous_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        traverse(cx, this, false)
    }

    pub fn get_root(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(iter(cx, this)?.1.root))
    }

    pub fn get_reference_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(iter(cx, this)?.1.current))
    }

    pub fn get_pointer_before_reference_node(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(iter(cx, this)?.1.before.unwrap_or(true))
    }

    pub fn get_what_to_show(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(iter(cx, this)?.1.what_to_show)
    }

    pub fn get_filter(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Value>> {
        Ok(iter(cx, this)?.1.filter)
    }

    /// `detach()`: does nothing (DOM).
    pub fn detach(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        Ok(())
    }
}
