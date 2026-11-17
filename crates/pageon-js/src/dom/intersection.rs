//! `IntersectionObserver` (W3C Intersection Observer) against pageon's layout. At the end of
//! each task, after scroll events, targets are re-evaluated when they are new or when the
//! layout or a scroll position changed; an entry is queued when a target's threshold index or
//! `isIntersecting` changed (the spec's "update intersection observations" steps).

use pageon_dom::NodeId;

use crate::cv::{self, Cx, Error, Obj, R, Value};
use crate::generated::{Iface, IntersectionObserverInit};
use crate::realm::Slot;

/// An `IntersectionObserver`.
#[derive(Debug, Clone)]
pub struct IoData {
    callback: Value,
    root: Option<NodeId>,
    root_margin: String,
    thresholds: Vec<f64>,
    targets: Vec<NodeId>,
    /// Targets observed since the last notification.
    fresh: Vec<NodeId>,
    queue: Vec<u32>,
    /// Per target: the last threshold index and `isIntersecting`.
    state: std::collections::HashMap<NodeId, (i64, bool)>,
    /// (mutations, scroll moves) at the last evaluation.
    seen: (u64, u64),
}

/// An `IntersectionObserverEntry`.
#[derive(Debug, Clone)]
pub struct EntryData {
    time: f64,
    root_bounds: Option<[f64; 4]>,
    bounding: [f64; 4],
    intersection: [f64; 4],
    intersecting: bool,
    ratio: f64,
    target: NodeId,
}

fn io(cx: &Cx<'_, '_, '_>, this: Obj) -> R<u32> {
    let id = this.slab()?;
    match cx.realm.st.borrow().slot(id) {
        Some(Slot::Intersection(_)) => Ok(id),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn with_io<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&mut IoData) -> T) -> Option<T> {
    match cx.realm.st.borrow_mut().slot_mut(id) {
        Some(Slot::Intersection(o)) => Some(f(o)),
        _ => None,
    }
}

fn entry(cx: &Cx<'_, '_, '_>, this: Obj) -> R<EntryData> {
    match cx.realm.st.borrow().slot(this.slab()?) {
        Some(Slot::IoEntry(e)) => Ok((**e).clone()),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn rect(cx: &mut Cx<'_, '_, '_>, r: [f64; 4]) -> Obj {
    cx.alloc(Iface::DOMRect, Slot::Rect(r))
}

pub mod intersection_observer {
    use super::*;

    /// `new IntersectionObserver(callback, options)`.
    pub fn constructor(
        cx: &mut Cx<'_, '_, '_>,
        callback: Value,
        init: IntersectionObserverInit,
    ) -> R<Obj> {
        if !cx.local(&callback).is_function() {
            return Err(Error::Type(
                "Failed to construct 'IntersectionObserver': parameter 1 is not of type 'Function'.".into(),
            ));
        }
        let root = match &init.root {
            None => None,
            Some(v) => {
                let l = cx.local(v);
                if l.is_null_or_undefined() {
                    None
                } else {
                    match cv::unwrap_any(cx, l) {
                        Some((Obj::Node(n), i)) if i.is_a(Iface::Element) => Some(n),
                        Some((Obj::Node(_), _)) => None,
                        _ => {
                            return Err(Error::Type(
                                "Failed to construct 'IntersectionObserver': 'root' is not an Element or Document.".into(),
                            ));
                        }
                    }
                }
            }
        };
        let mut thresholds = match &init.threshold {
            None => vec![0.0],
            Some(v) => {
                let l = cx.local(v);
                if let Ok(a) = v8::Local::<v8::Array>::try_from(l) {
                    let mut t = Vec::new();
                    for i in 0..a.length() {
                        let x = a
                            .get_index(cx.scope, i)
                            .and_then(|x| x.number_value(cx.scope))
                            .unwrap_or(f64::NAN);
                        t.push(x);
                    }
                    if t.is_empty() { vec![0.0] } else { t }
                } else {
                    vec![l.number_value(cx.scope).unwrap_or(f64::NAN)]
                }
            }
        };
        if thresholds.iter().any(|t| !(0.0..=1.0).contains(t)) {
            return Err(Error::Range(
                "Failed to construct 'IntersectionObserver': Threshold values must be numbers between 0 and 1".into(),
            ));
        }
        thresholds.sort_by(f64::total_cmp);
        let margin = init.root_margin.split_whitespace().collect::<Vec<_>>();
        let root_margin = match margin.as_slice() {
            [a] => format!("{a} {a} {a} {a}"),
            [a, b] => format!("{a} {b} {a} {b}"),
            [a, b, c] => format!("{a} {b} {c} {b}"),
            [a, b, c, d] => format!("{a} {b} {c} {d}"),
            _ => "0px 0px 0px 0px".into(),
        };
        Ok(cx.alloc(
            Iface::IntersectionObserver,
            Slot::Intersection(Box::new(IoData {
                callback,
                root,
                root_margin,
                thresholds,
                targets: Vec::new(),
                fresh: Vec::new(),
                queue: Vec::new(),
                state: std::collections::HashMap::new(),
                seen: (0, 0),
            })),
        ))
    }

    pub fn get_root(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Value>> {
        let id = io(cx, this)?;
        let root = with_io(cx, id, |o| o.root).flatten();
        Ok(root.map(|n| {
            let w = cx.wrap(Obj::Node(n));
            cx.keep(w.into())
        }))
    }

    pub fn get_root_margin(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let id = io(cx, this)?;
        Ok(with_io(cx, id, |o| o.root_margin.clone()).unwrap_or_default())
    }

    pub fn get_thresholds(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<f64>> {
        let id = io(cx, this)?;
        Ok(with_io(cx, id, |o| o.thresholds.clone()).unwrap_or_default())
    }

    /// `observe(target)`: the target is reported at the end of this task.
    pub fn observe(cx: &mut Cx<'_, '_, '_>, this: Obj, target: Obj) -> R<()> {
        let id = io(cx, this)?;
        let target = target.node()?;
        with_io(cx, id, |o| {
            if !o.targets.contains(&target) {
                o.targets.push(target);
                o.fresh.push(target);
            }
        });
        let mut st = cx.realm.st.borrow_mut();
        if !st.intersections.contains(&id) {
            st.intersections.push(id);
        }
        Ok(())
    }

    pub fn unobserve(cx: &mut Cx<'_, '_, '_>, this: Obj, target: Obj) -> R<()> {
        let id = io(cx, this)?;
        let target = target.node()?;
        with_io(cx, id, |o| {
            o.targets.retain(|t| *t != target);
            o.fresh.retain(|t| *t != target);
            o.state.remove(&target);
        });
        Ok(())
    }

    pub fn disconnect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = io(cx, this)?;
        with_io(cx, id, |o| {
            o.targets.clear();
            o.fresh.clear();
            o.queue.clear();
            o.state.clear();
        });
        cx.realm.st.borrow_mut().intersections.retain(|&o| o != id);
        Ok(())
    }

    pub fn take_records(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<Obj>> {
        let id = io(cx, this)?;
        Ok(with_io(cx, id, |o| std::mem::take(&mut o.queue))
            .unwrap_or_default()
            .into_iter()
            .map(Obj::Slab)
            .collect())
    }
}

pub mod intersection_observer_entry {
    use super::*;

    pub fn get_time(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(entry(cx, this)?.time)
    }
    pub fn get_root_bounds(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        Ok(entry(cx, this)?.root_bounds.map(|r| rect(cx, r)))
    }
    pub fn get_bounding_client_rect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let r = entry(cx, this)?.bounding;
        Ok(rect(cx, r))
    }
    pub fn get_intersection_rect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let r = entry(cx, this)?.intersection;
        Ok(rect(cx, r))
    }
    pub fn get_is_intersecting(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(entry(cx, this)?.intersecting)
    }
    pub fn get_intersection_ratio(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(entry(cx, this)?.ratio)
    }
    pub fn get_target(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(entry(cx, this)?.target))
    }
}

/// The intersection of a target with the root (the viewport, or an element's box).
fn compute(cx: &Cx<'_, '_, '_>, root: Option<NodeId>, target: NodeId, time: f64) -> EntryData {
    let as_rect = |r: pageon_layout::Rect| {
        [
            f64::from(r.x),
            f64::from(r.y),
            f64::from(r.width),
            f64::from(r.height),
        ]
    };
    let root_bounds = match root {
        Some(r) => crate::dom::elements::rect(cx, r).map(as_rect),
        None => {
            let v = cx.realm.st.borrow().viewport;
            Some([0.0, 0.0, f64::from(v.width), f64::from(v.height)])
        }
    };
    // A target that is not rendered (`display: none`, or inside such a subtree) has no box
    // and never intersects.
    let rendered = cx.realm.layout().visibility(target) != pageon_layout::Visibility::NotRendered;
    let bounding = if rendered {
        crate::dom::elements::rect(cx, target).map(as_rect)
    } else {
        None
    };
    let (intersection, intersecting) = match (bounding, root_bounds) {
        (Some(b), Some(r)) => {
            let x0 = b[0].max(r[0]);
            let y0 = b[1].max(r[1]);
            let x1 = (b[0] + b[2]).min(r[0] + r[2]);
            let y1 = (b[1] + b[3]).min(r[1] + r[3]);
            if x1 >= x0 && y1 >= y0 {
                ([x0, y0, x1 - x0, y1 - y0], true)
            } else {
                ([0.0; 4], false)
            }
        }
        _ => ([0.0; 4], false),
    };
    let b = bounding.unwrap_or([0.0; 4]);
    let area = b[2] * b[3];
    let ratio = if !intersecting {
        0.0
    } else if area > 0.0 {
        (intersection[2] * intersection[3] / area).clamp(0.0, 1.0)
    } else {
        1.0
    };
    EntryData {
        time,
        root_bounds,
        bounding: b,
        intersection,
        intersecting,
        ratio,
        target,
    }
}

/// The spec's threshold index: the first threshold above the ratio, or their count.
fn threshold_index(thresholds: &[f64], ratio: f64) -> i64 {
    let i = thresholds
        .iter()
        .position(|&t| t > ratio)
        .unwrap_or(thresholds.len());
    i64::try_from(i).unwrap_or(i64::MAX)
}

/// The frame (1/60 s) of a virtual time in milliseconds (frame 0 is never a last update's).
pub(crate) fn frame_of(now: f64) -> u64 {
    (now * 60.0 / 1000.0) as u64 + 1
}

/// Whether observers that read the layout (intersection, resize) may run now: in the first
/// update of a frame of the virtual clock, as browsers observe when they render (`now_anyway`:
/// the event loop's update at the frame's end). Tasks later in the frame leave theirs due.
pub(crate) fn rendering(cx: &Cx<'_, '_, '_>, now_anyway: bool) -> bool {
    let sh = cx.realm.shared.borrow();
    now_anyway || frame_of(sh.now) != sh.frame
}

/// Leaves this document's rendering update for the end of the frame.
pub(crate) fn defer(cx: &Cx<'_, '_, '_>) {
    let realm = cx.realm.id;
    cx.realm.shared.borrow_mut().render_due.insert(realm);
}

/// Records a rendering update of this document in the current frame.
pub(crate) fn rendered(cx: &Cx<'_, '_, '_>) {
    let realm = cx.realm.id;
    let mut sh = cx.realm.shared.borrow_mut();
    sh.frame = frame_of(sh.now);
    sh.render_due.remove(&realm);
}

/// Re-evaluates observers whose targets are new or whose layout or scroll changed, and calls
/// back those with entries. Returns whether any callback ran.
///
/// Observations need a layout, so they are made only in a rendering update (`render`, see
/// [`rendering`]); otherwise they are left due.
pub(crate) fn deliver(cx: &mut Cx<'_, '_, '_>, render: bool) -> R<bool> {
    let (observers, now) = {
        let st = cx.realm.st.borrow();
        (st.intersections.clone(), (st.mutations, st.scroll.moves))
    };
    if observers.is_empty() {
        return Ok(false);
    }
    // Nothing new to observe: no layout needed.
    let work = observers
        .iter()
        .any(|&id| with_io(cx, id, |o| o.seen != now || !o.fresh.is_empty()).unwrap_or(false));
    if !work {
        return Ok(false);
    }
    if !render {
        defer(cx);
        return Ok(false);
    }
    let time = {
        let sh = cx.realm.shared.borrow();
        sh.now - sh.origin
    };
    let mut ran = false;
    for id in observers {
        let Some((root, due, thresholds)) = with_io(cx, id, |o| {
            let due: Vec<NodeId> = if o.seen == now {
                std::mem::take(&mut o.fresh)
            } else {
                o.fresh.clear();
                o.targets.clone()
            };
            o.seen = now;
            (o.root, due, o.thresholds.clone())
        }) else {
            continue;
        };
        for t in due {
            if cx.dom().get(t).is_none() {
                continue;
            }
            let e = compute(cx, root, t, time);
            let now_state = (threshold_index(&thresholds, e.ratio), e.intersecting);
            let changed = with_io(cx, id, |o| o.state.insert(t, now_state) != Some(now_state))
                .unwrap_or(false);
            if !changed {
                continue;
            }
            let e = cx.alloc(Iface::IntersectionObserverEntry, Slot::IoEntry(Box::new(e)));
            if let Ok(e) = e.slab() {
                with_io(cx, id, |o| o.queue.push(e));
            }
        }
        let Some((callback, queue)) = with_io(cx, id, |o| {
            (o.callback.clone(), std::mem::take(&mut o.queue))
        }) else {
            continue;
        };
        if queue.is_empty() {
            continue;
        }
        // Records are delivered in the observer's world, as its own wrappers.
        let world = cx.world_of(&callback);
        cx.in_world(world, |cx| {
            let items: Vec<v8::Local<v8::Value>> = queue
                .into_iter()
                .map(|e| cx.wrap(Obj::Slab(e)).into())
                .collect();
            let list: v8::Local<v8::Value> = v8::Array::new_with_elements(cx.scope, &items).into();
            let me: v8::Local<v8::Value> = cx.wrap(Obj::Slab(id)).into();
            cx.invoke(&callback, me, &[list, me])
        })?;
        ran = true;
    }
    Ok(ran)
}
