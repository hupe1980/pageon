//! User Timing (`performance.mark`, `measure`) and the Performance Timeline
//! (`getEntries*`, `PerformanceObserver`) for `mark` and `measure` entries, on the virtual
//! clock. Other entry types (`navigation`, `resource`, `paint`, …) are not recorded, and
//! observing them observes nothing.

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{Iface, PerformanceMarkOptions, PerformanceObserverInit};
use crate::realm::Slot;

/// The entry types pageon records.
pub const SUPPORTED: [&str; 2] = ["mark", "measure"];

/// A `PerformanceMark` or `PerformanceMeasure`.
#[derive(Debug, Clone)]
pub struct Entry {
    /// `mark` or `measure`.
    pub kind: &'static str,
    /// `name`.
    pub name: String,
    /// `startTime` (ms since the time origin).
    pub start: f64,
    /// `duration` (0 for marks).
    pub duration: f64,
    /// `detail` (a structured clone), or `null`.
    pub detail: Option<Value>,
}

/// A `PerformanceObserver`.
#[derive(Debug, Clone)]
pub struct ObserverData {
    /// The callback.
    pub callback: Value,
    /// Entry types observed.
    pub types: Vec<String>,
    /// Entries waiting for the callback.
    pub buffer: Vec<u32>,
}

fn now(cx: &Cx<'_, '_, '_>) -> f64 {
    let sh = cx.realm.shared.borrow();
    sh.now - sh.origin
}

fn entry<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&Entry) -> T) -> R<T> {
    match cx.st().slot(this.slab()?) {
        Some(Slot::PerfEntry(e)) => Ok(f(e)),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn clone_detail(cx: &mut Cx<'_, '_, '_>, detail: Option<Value>) -> R<Option<Value>> {
    match detail {
        Some(d) if !cx.local(&d).is_null_or_undefined() => {
            Ok(Some(crate::dom::session::structured_clone(cx, &d, &[])?))
        }
        _ => Ok(None),
    }
}

/// Records an entry on the timeline and hands it to the observers of its type.
fn record(cx: &mut Cx<'_, '_, '_>, iface: Iface, e: Entry) -> Obj {
    let kind = e.kind;
    let id = cx.st().alloc(iface, Slot::PerfEntry(Box::new(e)));
    let mut st = cx.st();
    st.timing.push(id);
    let observers = st.perf_observers.clone();
    for o in observers {
        if let Some(Slot::PerfObserver(d)) = st.slot_mut(o)
            && d.types.iter().any(|t| t == kind)
        {
            d.buffer.push(id);
        }
    }
    Obj::Slab(id)
}

/// Entries on the timeline, by start time, filtered.
fn entries(cx: &Cx<'_, '_, '_>, name: Option<&str>, kind: Option<&str>) -> Vec<Obj> {
    let st = cx.st();
    let mut out: Vec<(f64, u32)> = st
        .timing
        .iter()
        .filter_map(|&id| match st.slot(id) {
            Some(Slot::PerfEntry(e))
                if name.is_none_or(|n| e.name == n) && kind.is_none_or(|k| e.kind == k) =>
            {
                Some((e.start, id))
            }
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.into_iter().map(|(_, id)| Obj::Slab(id)).collect()
}

/// The start time of the latest mark named `name`.
fn mark_time(cx: &Cx<'_, '_, '_>, name: &str) -> R<f64> {
    let st = cx.st();
    st.timing
        .iter()
        .rev()
        .find_map(|&id| match st.slot(id) {
            Some(Slot::PerfEntry(e)) if e.kind == "mark" && e.name == name => Some(e.start),
            _ => None,
        })
        .ok_or_else(|| {
            Error::dom(
                "SyntaxError",
                format!("Failed to execute 'measure' on 'Performance': The mark '{name}' does not exist."),
            )
        })
}

pub(crate) fn mark(cx: &mut Cx<'_, '_, '_>, name: String, o: PerformanceMarkOptions) -> R<Obj> {
    let start = o.start_time.unwrap_or_else(|| now(cx));
    if start < 0.0 {
        return Err(Error::Type(
            "Failed to execute 'mark' on 'Performance': 'startTime' cannot be negative.".into(),
        ));
    }
    let detail = clone_detail(cx, o.detail)?;
    Ok(record(
        cx,
        Iface::PerformanceMark,
        Entry {
            kind: "mark",
            name,
            start,
            duration: 0.0,
            detail,
        },
    ))
}

pub(crate) fn clear(cx: &mut Cx<'_, '_, '_>, kind: &str, name: Option<String>) {
    let mut st = cx.st();
    let ids = std::mem::take(&mut st.timing);
    let keep: Vec<u32> = ids
        .into_iter()
        .filter(|&id| match st.slot(id) {
            Some(Slot::PerfEntry(e)) => {
                e.kind != kind || name.as_deref().is_some_and(|n| e.name != n)
            }
            _ => false,
        })
        .collect();
    st.timing = keep;
}

pub(crate) fn get_entries(cx: &Cx<'_, '_, '_>, name: Option<&str>, kind: Option<&str>) -> Vec<Obj> {
    entries(cx, name, kind)
}

/// `measure(name, startOrOptions, endMark)`, with the options or mark names as given.
pub(crate) fn measure_any(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, args| {
        let _ = cx.this(args, Iface::Performance)?;
        if args.length() == 0 {
            return Err(Error::args("Performance.measure", 0));
        }
        let name = crate::cv::string(cx, args.get(0))?;
        let a1 = args.get(1);
        let end_mark = args.get(2);
        let point = |cx: &mut Cx<'_, '_, '_>, v: v8::Local<v8::Value>| -> R<Option<f64>> {
            if v.is_null_or_undefined() {
                Ok(None)
            } else if v.is_number() {
                Ok(v.number_value(cx.scope))
            } else {
                let n = crate::cv::string(cx, v)?;
                mark_time(cx, &n).map(Some)
            }
        };
        // The options: an object with any of its members (an empty one counts as none).
        let options = if a1.is_object() && !a1.is_null() {
            let o = v8::Local::<v8::Object>::try_from(a1).map_err(|_| Error::Terminated)?;
            let mut fields = Vec::new();
            for k in ["start", "end", "duration", "detail"] {
                let key = cx.str(k);
                let v = o
                    .get(cx.scope, key.into())
                    .unwrap_or_else(|| v8::undefined(cx.scope).into());
                fields.push(v);
            }
            fields.iter().any(|v| !v.is_undefined()).then_some(fields)
        } else {
            None
        };
        let (start, end, detail) = if let Some(fields) = options {
            let (s, e, d, det) = (fields[0], fields[1], fields[2], fields[3]);
            const FAILED: &str = "Failed to execute 'measure' on 'Performance': ";
            if !end_mark.is_undefined() {
                return Err(Error::Type(format!(
                    "{FAILED}If a non-empty PerformanceMeasureOptions object was passed, |end_mark| must not be passed."
                )));
            }
            if s.is_undefined() && e.is_undefined() {
                return Err(Error::Type(format!(
                    "{FAILED}If a non-empty PerformanceMeasureOptions object was passed, at least one of its 'start' or 'end' properties must be present."
                )));
            }
            if !s.is_undefined() && !e.is_undefined() && !d.is_undefined() {
                return Err(Error::Type(format!(
                    "{FAILED}If a non-empty PerformanceMeasureOptions object was passed, it must not have all of its 'start', 'duration', and 'end' properties defined"
                )));
            }
            let start = point(cx, s)?;
            let end = point(cx, e)?;
            let duration = if d.is_number() {
                d.number_value(cx.scope)
            } else {
                None
            };
            let (start, end) = match (start, end, duration) {
                (Some(s), None, Some(d)) => (s, s + d),
                (None, Some(e), Some(d)) => (e - d, e),
                (s, e, _) => (s.unwrap_or(0.0), e.unwrap_or_else(|| now(cx))),
            };
            let detail = if det.is_null_or_undefined() {
                None
            } else {
                Some(cx.keep(det))
            };
            (start, end, detail)
        } else {
            let a1 = if a1.is_object() && !a1.is_null() {
                v8::undefined(cx.scope).into()
            } else {
                a1
            };
            let start = point(cx, a1)?.unwrap_or(0.0);
            let end = point(cx, end_mark)?.unwrap_or_else(|| now(cx));
            (start, end, None)
        };
        let detail = clone_detail(cx, detail)?;
        let r = record(
            cx,
            Iface::PerformanceMeasure,
            Entry {
                kind: "measure",
                name,
                start,
                duration: end - start,
                detail,
            },
        );
        cx.ret(r)
    });
}

/// Calls the observers with entries waiting, after a microtask checkpoint (as the other
/// observers are); `true` if any ran.
pub(crate) fn deliver(cx: &mut Cx<'_, '_, '_>) -> R<bool> {
    let observers = cx.st().perf_observers.clone();
    let mut ran = false;
    for id in observers {
        let (callback, batch) = match cx.st().slot_mut(id) {
            Some(Slot::PerfObserver(d)) if !d.buffer.is_empty() => {
                (d.callback.clone(), std::mem::take(&mut d.buffer))
            }
            _ => continue,
        };
        let list = cx.alloc(
            Iface::PerformanceObserverEntryList,
            Slot::PerfEntryList(batch),
        );
        let world = cx.world_of(&callback);
        cx.in_world(world, |cx| {
            let list: v8::Local<v8::Value> = cx.wrap(list).into();
            let me: v8::Local<v8::Value> = cx.wrap(Obj::Slab(id)).into();
            cx.invoke(&callback, me, &[list, me])
        })?;
        ran = true;
    }
    Ok(ran)
}

pub mod performance_entry {
    use super::*;

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        entry(cx, this, |e| e.name.clone())
    }

    pub fn get_entry_type(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        entry(cx, this, |e| e.kind.to_string())
    }

    pub fn get_start_time(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        entry(cx, this, |e| e.start)
    }

    pub fn get_duration(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        entry(cx, this, |e| e.duration)
    }

    pub fn to_json(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let (name, kind, start, duration) =
            entry(cx, this, |e| (e.name.clone(), e.kind, e.start, e.duration))?;
        let o = v8::Object::new(cx.scope);
        let set = |cx: &mut Cx<'_, '_, '_>, k: &str, v: v8::Local<v8::Value>| {
            let key = cx.str(k);
            o.set(cx.scope, key.into(), v);
        };
        let n: v8::Local<v8::Value> = cx.str(&name).into();
        set(cx, "name", n);
        let k: v8::Local<v8::Value> = cx.str(kind).into();
        set(cx, "entryType", k);
        let s: v8::Local<v8::Value> = v8::Number::new(cx.scope, start).into();
        set(cx, "startTime", s);
        let d: v8::Local<v8::Value> = v8::Number::new(cx.scope, duration).into();
        set(cx, "duration", d);
        Ok(cx.keep(o.into()))
    }
}

fn detail(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
    match entry(cx, this, |e| e.detail.clone())? {
        Some(d) => Ok(d),
        None => {
            let n: v8::Local<v8::Value> = v8::null(cx.scope).into();
            Ok(cx.keep(n))
        }
    }
}

pub mod performance_mark {
    use super::*;

    /// `new PerformanceMark(name, options)`: an entry that is not on the timeline.
    pub fn constructor(cx: &mut Cx<'_, '_, '_>, name: String, o: PerformanceMarkOptions) -> R<Obj> {
        let start = o.start_time.unwrap_or_else(|| now(cx));
        let detail = clone_detail(cx, o.detail)?;
        Ok(cx.alloc(
            Iface::PerformanceMark,
            Slot::PerfEntry(Box::new(Entry {
                kind: "mark",
                name,
                start,
                duration: 0.0,
                detail,
            })),
        ))
    }

    pub fn get_detail(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        detail(cx, this)
    }
}

pub mod performance_measure {
    use super::*;

    pub fn get_detail(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        detail(cx, this)
    }
}

pub mod performance_observer {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, callback: Value) -> R<Obj> {
        if !cx.local(&callback).is_function() {
            return Err(Error::Type(
                "Failed to construct 'PerformanceObserver': The callback provided as parameter 1 is not a function.".into(),
            ));
        }
        Ok(cx.alloc(
            Iface::PerformanceObserver,
            Slot::PerfObserver(Box::new(ObserverData {
                callback,
                types: Vec::new(),
                buffer: Vec::new(),
            })),
        ))
    }

    /// `observe({type, buffered})` or `observe({entryTypes})`; types pageon does not record
    /// are ignored, as browsers ignore unknown ones.
    pub fn observe(cx: &mut Cx<'_, '_, '_>, this: Obj, o: PerformanceObserverInit) -> R<()> {
        let id = this.slab()?;
        let types: Vec<String> = match (&o.type_, &o.entry_types) {
            (Some(_), Some(_)) | (None, None) => {
                return Err(Error::Type(
                    "Failed to execute 'observe' on 'PerformanceObserver': An observe() call must include either entryTypes or type arguments.".into(),
                ));
            }
            (Some(t), None) => vec![t.clone()],
            (None, Some(ts)) => ts.clone(),
        };
        let types: Vec<String> = types
            .into_iter()
            .filter(|t| SUPPORTED.contains(&t.as_str()))
            .collect();
        let buffered = if o.buffered.unwrap_or(false) && o.type_.is_some() {
            types
                .iter()
                .flat_map(|t| entries(cx, None, Some(t)))
                .filter_map(|e| e.slab().ok())
                .collect()
        } else {
            Vec::new()
        };
        let mut st = cx.st();
        if let Some(Slot::PerfObserver(d)) = st.slot_mut(id) {
            for t in types {
                if !d.types.contains(&t) {
                    d.types.push(t);
                }
            }
            d.buffer.extend(buffered);
        }
        if !st.perf_observers.contains(&id) {
            st.perf_observers.push(id);
        }
        Ok(())
    }

    pub fn disconnect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = this.slab()?;
        let mut st = cx.st();
        st.perf_observers.retain(|&o| o != id);
        if let Some(Slot::PerfObserver(d)) = st.slot_mut(id) {
            d.types.clear();
            d.buffer.clear();
        }
        Ok(())
    }

    pub fn take_records(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<Obj>> {
        let id = this.slab()?;
        match cx.st().slot_mut(id) {
            Some(Slot::PerfObserver(d)) => Ok(std::mem::take(&mut d.buffer)
                .into_iter()
                .map(Obj::Slab)
                .collect()),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }
}

pub mod performance_observer_entry_list {
    use super::*;

    fn list(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Vec<u32>> {
        match cx.st().slot(this.slab()?) {
            Some(Slot::PerfEntryList(l)) => Ok(l.clone()),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    fn filtered(
        cx: &Cx<'_, '_, '_>,
        this: Obj,
        name: Option<&str>,
        kind: Option<&str>,
    ) -> R<Vec<Obj>> {
        let ids = list(cx, this)?;
        let st = cx.st();
        Ok(ids
            .into_iter()
            .filter(|&id| match st.slot(id) {
                Some(Slot::PerfEntry(e)) => {
                    name.is_none_or(|n| e.name == n) && kind.is_none_or(|k| e.kind == k)
                }
                _ => false,
            })
            .map(Obj::Slab)
            .collect())
    }

    pub fn get_entries(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<Obj>> {
        filtered(cx, this, None, None)
    }

    pub fn get_entries_by_type(cx: &mut Cx<'_, '_, '_>, this: Obj, ty: String) -> R<Vec<Obj>> {
        filtered(cx, this, None, Some(&ty))
    }

    pub fn get_entries_by_name(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        name: String,
        ty: Option<String>,
    ) -> R<Vec<Obj>> {
        filtered(cx, this, Some(&name), ty.as_deref())
    }
}
