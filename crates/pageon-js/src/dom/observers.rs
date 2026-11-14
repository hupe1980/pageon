//! `MutationObserver` and `MutationRecord` (DOM §4.3): the document logs its changes while an
//! observer exists, and the realm turns them into records and calls observers at each task's
//! and microtask checkpoint's end ("notify mutation observers").

use pageon_dom::{Mutation, NodeId};

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{Iface, MutationObserverInit};
use crate::realm::Slot;

/// One `observe(target, options)` registration (options already normalized).
#[derive(Debug, Clone)]
pub struct Registration {
    target: NodeId,
    child_list: bool,
    attributes: bool,
    character_data: bool,
    subtree: bool,
    attribute_old_value: bool,
    character_data_old_value: bool,
    attribute_filter: Option<Vec<String>>,
}

/// A `MutationObserver`.
#[derive(Debug, Clone)]
pub struct ObserverData {
    callback: Value,
    registrations: Vec<Registration>,
    queue: Vec<u32>,
}

/// A `MutationRecord`.
#[derive(Debug, Clone)]
pub struct RecordData {
    kind: &'static str,
    target: NodeId,
    added: Vec<NodeId>,
    removed: Vec<NodeId>,
    previous: Option<NodeId>,
    next: Option<NodeId>,
    name: Option<String>,
    old: Option<String>,
    lists: [Option<u32>; 2],
}

fn observer(cx: &Cx<'_, '_, '_>, this: Obj) -> R<u32> {
    let id = this.slab()?;
    match cx.realm.st.borrow().slot(id) {
        Some(Slot::Observer(_)) => Ok(id),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn record(cx: &Cx<'_, '_, '_>, this: Obj) -> R<RecordData> {
    match cx.realm.st.borrow().slot(this.slab()?) {
        Some(Slot::Record(r)) => Ok((**r).clone()),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// Whether any observer of the realm has a registration, or any custom element is defined
/// (the document logs changes only then).
pub(crate) fn update_observing(cx: &Cx<'_, '_, '_>) {
    let on = {
        let st = cx.realm.st.borrow();
        !st.observers.is_empty() || st.custom.active()
    };
    cx.realm.dom.borrow_mut().set_observing(on);
}

pub mod mutation_observer {
    use super::*;

    /// `new MutationObserver(callback)`.
    pub fn constructor(cx: &mut Cx<'_, '_, '_>, callback: Value) -> R<Obj> {
        if !cx.local(&callback).is_function() {
            return Err(Error::Type(
                "Failed to construct 'MutationObserver': parameter 1 is not of type 'Function'."
                    .into(),
            ));
        }
        Ok(cx.alloc(
            Iface::MutationObserver,
            Slot::Observer(Box::new(ObserverData {
                callback,
                registrations: Vec::new(),
                queue: Vec::new(),
            })),
        ))
    }

    /// `observe(target, options)`, with the DOM standard's option rules.
    pub fn observe(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        target: Obj,
        o: MutationObserverInit,
    ) -> R<()> {
        let id = observer(cx, this)?;
        let target = target.node()?;
        let attributes = o
            .attributes
            .unwrap_or(o.attribute_old_value.is_some() || o.attribute_filter.is_some());
        let character_data = o
            .character_data
            .unwrap_or(o.character_data_old_value.is_some());
        let fail = |m: &str| {
            Err(Error::Type(format!(
                "Failed to execute 'observe' on 'MutationObserver': {m}"
            )))
        };
        if !o.child_list && !attributes && !character_data {
            return fail(
                "The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.",
            );
        }
        if o.attribute_old_value == Some(true) && !attributes {
            return fail(
                "The options object may only set 'attributeOldValue' to true when 'attributes' is true or not present.",
            );
        }
        if o.attribute_filter.is_some() && !attributes {
            return fail(
                "The options object may only set 'attributeFilter' when 'attributes' is true or not present.",
            );
        }
        if o.character_data_old_value == Some(true) && !character_data {
            return fail(
                "The options object may only set 'characterDataOldValue' to true when 'characterData' is true or not present.",
            );
        }
        let reg = Registration {
            target,
            child_list: o.child_list,
            attributes,
            character_data,
            subtree: o.subtree,
            attribute_old_value: o.attribute_old_value.unwrap_or(false),
            character_data_old_value: o.character_data_old_value.unwrap_or(false),
            attribute_filter: o.attribute_filter,
        };
        {
            let mut st = cx.realm.st.borrow_mut();
            if let Some(Slot::Observer(ob)) = st.slot_mut(id) {
                match ob.registrations.iter_mut().find(|r| r.target == target) {
                    Some(r) => *r = reg,
                    None => ob.registrations.push(reg),
                }
            }
            if !st.observers.contains(&id) {
                st.observers.push(id);
            }
        }
        update_observing(cx);
        Ok(())
    }

    /// `disconnect()`: no more records, and the queued ones are dropped.
    pub fn disconnect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = observer(cx, this)?;
        {
            let mut st = cx.realm.st.borrow_mut();
            if let Some(Slot::Observer(ob)) = st.slot_mut(id) {
                ob.registrations.clear();
                ob.queue.clear();
            }
            st.observers.retain(|&o| o != id);
        }
        update_observing(cx);
        Ok(())
    }

    /// `takeRecords()`: the queued records, now not delivered.
    pub fn take_records(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<Obj>> {
        let id = observer(cx, this)?;
        // Records are queued as changes happen (here: from the document's log, now).
        queue(cx);
        let mut st = cx.realm.st.borrow_mut();
        Ok(match st.slot_mut(id) {
            Some(Slot::Observer(ob)) => std::mem::take(&mut ob.queue)
                .into_iter()
                .map(Obj::Slab)
                .collect(),
            _ => Vec::new(),
        })
    }
}

pub mod mutation_record {
    use super::*;

    fn list(cx: &mut Cx<'_, '_, '_>, this: Obj, which: usize) -> R<Obj> {
        let r = record(cx, this)?;
        if let Some(l) = r.lists[which] {
            return Ok(Obj::Slab(l));
        }
        let nodes = if which == 0 { r.added } else { r.removed };
        let l = cx.alloc(Iface::NodeList, Slot::Static(nodes)).slab()?;
        if let Some(Slot::Record(rec)) = cx.realm.st.borrow_mut().slot_mut(this.slab()?) {
            rec.lists[which] = Some(l);
        }
        Ok(Obj::Slab(l))
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(record(cx, this)?.kind.to_string())
    }
    pub fn get_target(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(record(cx, this)?.target))
    }
    pub fn get_added_nodes(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        list(cx, this, 0)
    }
    pub fn get_removed_nodes(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        list(cx, this, 1)
    }
    pub fn get_previous_sibling(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(record(cx, this)?.previous.map(Obj::Node))
    }
    pub fn get_next_sibling(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(record(cx, this)?.next.map(Obj::Node))
    }
    pub fn get_attribute_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        Ok(record(cx, this)?.name)
    }
    pub fn get_attribute_namespace(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<String>> {
        Ok(None)
    }
    pub fn get_old_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        Ok(record(cx, this)?.old)
    }
}

/// Queues the document's logged changes as records for the observers interested in them.
/// Returns whether any observer now has records.
fn queue(cx: &mut Cx<'_, '_, '_>) -> bool {
    let mutations = cx.realm.dom.borrow_mut().take_mutations();
    cx.realm.st.borrow_mut().custom.cursor = 0;
    let observers = cx.realm.st.borrow().observers.clone();
    if observers.is_empty() {
        return false;
    }
    for m in mutations {
        let (target, kind) = match &m {
            Mutation::ChildList { target, .. } => (*target, "childList"),
            Mutation::Attribute { target, .. } => (*target, "attributes"),
            Mutation::CharacterData { target, .. } => (*target, "characterData"),
        };
        for &o in &observers {
            // The registrations that see this change (on the target, or an ancestor with subtree).
            let regs: Vec<Registration> = match cx.realm.st.borrow().slot(o) {
                Some(Slot::Observer(ob)) => ob.registrations.clone(),
                _ => continue,
            };
            let dom = cx.realm.dom.borrow();
            let mut wants = false;
            let mut old_value = false;
            for r in &regs {
                if r.target != target && !(r.subtree && dom.is_inclusive_ancestor(r.target, target))
                {
                    continue;
                }
                let ok = match &m {
                    Mutation::ChildList { .. } => r.child_list,
                    Mutation::Attribute { name, .. } => {
                        r.attributes
                            && r.attribute_filter
                                .as_ref()
                                .is_none_or(|f| f.iter().any(|x| x == name))
                    }
                    Mutation::CharacterData { .. } => r.character_data,
                };
                if ok {
                    wants = true;
                    old_value |= match &m {
                        Mutation::Attribute { .. } => r.attribute_old_value,
                        Mutation::CharacterData { .. } => r.character_data_old_value,
                        Mutation::ChildList { .. } => false,
                    };
                }
            }
            drop(dom);
            if !wants {
                continue;
            }
            let rec = match &m {
                Mutation::ChildList {
                    added,
                    removed,
                    previous,
                    next,
                    ..
                } => RecordData {
                    kind,
                    target,
                    added: added.iter().copied().collect(),
                    removed: removed.iter().copied().collect(),
                    previous: *previous,
                    next: *next,
                    name: None,
                    old: None,
                    lists: [None, None],
                },
                Mutation::Attribute { name, old, .. } => RecordData {
                    kind,
                    target,
                    added: Vec::new(),
                    removed: Vec::new(),
                    previous: None,
                    next: None,
                    name: Some(name.clone()),
                    old: if old_value { old.clone() } else { None },
                    lists: [None, None],
                },
                Mutation::CharacterData { old, .. } => RecordData {
                    kind,
                    target,
                    added: Vec::new(),
                    removed: Vec::new(),
                    previous: None,
                    next: None,
                    name: None,
                    old: old_value.then(|| old.clone()),
                    lists: [None, None],
                },
            };
            let r = cx.alloc(Iface::MutationRecord, Slot::Record(Box::new(rec)));
            if let (Ok(r), Some(Slot::Observer(ob))) =
                (r.slab(), cx.realm.st.borrow_mut().slot_mut(o))
            {
                ob.queue.push(r);
            }
        }
    }
    let st = cx.realm.st.borrow();
    observers
        .iter()
        .any(|&o| matches!(st.slot(o), Some(Slot::Observer(ob)) if !ob.queue.is_empty()))
}

/// Notifies mutation observers: queues the logged changes, then calls each observer that has
/// records with them, in creation order. Returns whether any callback ran.
pub(crate) fn deliver(cx: &mut Cx<'_, '_, '_>) -> R<bool> {
    if !queue(cx) {
        return Ok(false);
    }
    let observers = cx.realm.st.borrow().observers.clone();
    for o in observers {
        let (callback, records) = {
            let mut st = cx.realm.st.borrow_mut();
            match st.slot_mut(o) {
                // The isolate is locked here (delivery runs inside a task), so the callback's
                // handle may be cloned.
                Some(Slot::Observer(ob)) if !ob.queue.is_empty() => {
                    (ob.callback.clone(), std::mem::take(&mut ob.queue))
                }
                _ => continue,
            }
        };
        // Records are delivered in the observer's world, as its own wrappers.
        let world = cx.world_of(&callback);
        cx.in_world(world, |cx| {
            let items: Vec<v8::Local<v8::Value>> = records
                .into_iter()
                .map(|r| cx.wrap(Obj::Slab(r)).into())
                .collect();
            let list: v8::Local<v8::Value> = v8::Array::new_with_elements(cx.scope, &items).into();
            let me: v8::Local<v8::Value> = cx.wrap(Obj::Slab(o)).into();
            cx.invoke(&callback, me, &[list, me])
        })?;
    }
    Ok(true)
}
