//! Events: targets, dispatch (capture, target, bubble), handlers and the event classes.

use std::cell::Cell;
use std::rc::Rc;

use pageon_dom::NodeId;

use crate::cv::{Cx, Error, Obj, OrBool, R, Value};
use crate::generated::{
    AddEventListenerOptions, CustomEventInit, EventInit, EventListenerOptions, FocusEventInit,
    Iface, KeyboardEventInit, MouseEventInit, UIEventInit, WheelEventInit,
};
use crate::realm::{EventData, InputData, Listener, Slot, Target};

fn ev<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&EventData) -> T) -> R<T> {
    let id = this.slab()?;
    match cx.realm.st.borrow().slot(id) {
        Some(Slot::Event(e)) => Ok(f(e)),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn ev_mut<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&mut EventData) -> T) -> R<T> {
    let id = this.slab()?;
    match cx.realm.st.borrow_mut().slot_mut(id) {
        Some(Slot::Event(e)) => Ok(f(e)),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn target_obj(t: Target) -> Obj {
    match t {
        Target::Window => Obj::Window,
        Target::Node(n) => Obj::Node(n),
        Target::Object(i) => Obj::Slab(i),
    }
}

fn now(cx: &Cx<'_, '_, '_>) -> f64 {
    let sh = cx.realm.shared.borrow();
    sh.now - sh.origin
}

fn base(cx: &Cx<'_, '_, '_>, ty: String, init: &EventInit) -> EventData {
    EventData {
        ty,
        bubbles: init.bubbles,
        cancelable: init.cancelable,
        composed: init.composed,
        initialized: true,
        time: now(cx),
        ..EventData::default()
    }
}

fn related(o: Option<Obj>) -> Option<Target> {
    match o {
        Some(Obj::Window) => Some(Target::Window),
        Some(Obj::Node(n)) => Some(Target::Node(n)),
        _ => None,
    }
}

/// `EventTarget`.
pub mod event_target {
    use super::*;

    fn target(this: Obj) -> R<Target> {
        Ok(match this {
            Obj::Window => Target::Window,
            Obj::Node(n) => Target::Node(n),
            Obj::Slab(i) => Target::Object(i),
            Obj::Frame(..) | Obj::Foreign(..) => {
                return Err(Error::dom("NotSupportedError", crate::cv::FOREIGN));
            }
        })
    }

    pub fn add_event_listener(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        ty: String,
        cb: Option<Value>,
        options: OrBool<AddEventListenerOptions>,
    ) -> R<()> {
        let Some(cb) = cb else { return Ok(()) };
        let t = target(this)?;
        let (capture, once) = match options {
            OrBool::Bool(b) => (b, false),
            OrBool::Dict(d) => (d.capture, d.once),
        };
        let cb_local = cx.local(&cb);
        let mut st = cx.st();
        let list = st.listeners.entry(t).or_default();
        let exists = list.iter().any(|l| {
            !l.removed.get() && l.ty == ty && l.capture == capture && {
                let other = v8::Local::new(&*cx.scope, &l.cb);
                other.strict_equals(cb_local)
            }
        });
        if !exists {
            list.push(Listener {
                ty,
                cb,
                capture,
                once,
                removed: Rc::new(Cell::new(false)),
            });
        }
        Ok(())
    }

    pub fn remove_event_listener(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        ty: String,
        cb: Option<Value>,
        options: OrBool<EventListenerOptions>,
    ) -> R<()> {
        let Some(cb) = cb else { return Ok(()) };
        let t = target(this)?;
        let capture = match options {
            OrBool::Bool(b) => b,
            OrBool::Dict(d) => d.capture,
        };
        let cb_local = cx.local(&cb);
        let mut st = cx.st();
        if let Some(list) = st.listeners.get_mut(&t) {
            list.retain(|l| {
                let same = l.ty == ty && l.capture == capture && {
                    let other = v8::Local::new(&*cx.scope, &l.cb);
                    other.strict_equals(cb_local)
                };
                if same {
                    l.removed.set(true);
                }
                !same
            });
        }
        Ok(())
    }

    pub fn dispatch_event(cx: &mut Cx<'_, '_, '_>, this: Obj, event: Obj) -> R<bool> {
        let t = target(this)?;
        let (dispatching, initialized) = ev(cx, event, |e| (e.dispatching, e.initialized))?;
        if dispatching || !initialized {
            return Err(Error::dom(
                "InvalidStateError",
                "The event is already being dispatched or was not initialized.",
            ));
        }
        ev_mut(cx, event, |e| e.trusted = false)?;
        dispatch(cx, t, event)
    }

    /// The propagation path of a target: the target, its ancestors, then the window for nodes
    /// in the document (except for `load`, which stops at the document).
    /// The event path: through a slotted node's slot, and from a shadow root to its host when
    /// the event is composed (it stops at the root otherwise).
    fn path(cx: &Cx<'_, '_, '_>, t: Target, ty: &str, composed: bool) -> Vec<Target> {
        let mut out = vec![t];
        if let Target::Node(n) = t {
            let dom = cx.dom();
            let parent = |c: NodeId| {
                if let Some(slot) = dom.assigned_slot(c) {
                    return Some(slot);
                }
                match dom.parent(c) {
                    Some(p) => Some(p),
                    None if composed => dom.host(c),
                    None => None,
                }
            };
            let mut cur = parent(n);
            while let Some(c) = cur {
                out.push(Target::Node(c));
                cur = parent(c);
            }
            if out.last() == Some(&Target::Node(dom.root())) && ty != "load" {
                out.push(Target::Window);
            }
        }
        out
    }

    /// Dispatches `event` at `t`; returns `false` if it was canceled.
    pub fn dispatch(cx: &mut Cx<'_, '_, '_>, t: Target, event: Obj) -> R<bool> {
        let ty = ev(cx, event, |e| e.ty.clone())?;
        let bubbles = ev(cx, event, |e| e.bubbles)?;
        let composed = ev(cx, event, |e| e.composed)?;
        ev_mut(cx, event, |e| {
            e.dispatching = true;
            e.target = Some(t);
            e.stop = false;
            e.stop_immediate = false;
        })?;
        let path = path(cx, t, &ty, composed);
        ev_mut(cx, event, |e| e.path = path.clone())?;
        let mut result = Ok(());
        // Capture: from the window down to the target's parent.
        for (i, &cur) in path.iter().enumerate().rev() {
            if i == 0 {
                break;
            }
            result = invoke(cx, cur, event, &ty, 1, Some(true));
            if result.is_err() || ev(cx, event, |e| e.stop)? {
                break;
            }
        }
        if result.is_ok() && !ev(cx, event, |e| e.stop)? {
            // At target: capture listeners, then the rest.
            result = invoke(cx, t, event, &ty, 2, Some(true));
            if result.is_ok() && !ev(cx, event, |e| e.stop_immediate)? {
                result = invoke(cx, t, event, &ty, 2, Some(false));
            }
            if bubbles {
                for &cur in path.iter().skip(1) {
                    if result.is_err() || ev(cx, event, |e| e.stop)? {
                        break;
                    }
                    result = invoke(cx, cur, event, &ty, 3, Some(false));
                }
            }
        }
        ev_mut(cx, event, |e| {
            e.dispatching = false;
            e.phase = 0;
            e.current = None;
        })?;
        result?;
        Ok(!ev(cx, event, |e| e.canceled)?)
    }

    fn invoke(
        cx: &mut Cx<'_, '_, '_>,
        cur: Target,
        event: Obj,
        ty: &str,
        phase: u16,
        capture: Option<bool>,
    ) -> R<()> {
        ev_mut(cx, event, |e| {
            e.phase = phase;
            e.current = Some(cur);
        })?;
        let listeners: Vec<Listener> = cx
            .st()
            .listeners
            .get(&cur)
            .map(|l| {
                l.iter()
                    .filter(|l| l.ty == ty && capture.is_none_or(|c| l.capture == c))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        // The event handler attributes (one per world) run with the non-capture listeners.
        if capture != Some(true) {
            let worlds = u32::try_from(cx.realm.worlds.borrow().len()).unwrap_or(1);
            for w in 0..worlds.max(1) {
                cx.in_world(w, |cx| handler(cx, cur, event, ty))?;
                crate::cv::clean_up(cx);
            }
        }
        for l in listeners {
            if l.removed.get() {
                continue;
            }
            if l.once {
                l.removed.set(true);
                if let Some(list) = cx.st().listeners.get_mut(&cur) {
                    list.retain(|x| !x.removed.get());
                }
            }
            // Each listener runs in its own world, with that world's wrappers of the target
            // and the event.
            let world = cx.world_of(&l.cb);
            cx.in_world(world, |cx| {
                let this = cx.wrap(target_obj(cur));
                let ev_js = cx.wrap(event);
                cx.invoke(&l.cb, this.into(), &[ev_js.into()])
            })?;
            crate::cv::clean_up(cx);
            if ev(cx, event, |e| e.stop_immediate)? {
                break;
            }
        }
        Ok(())
    }

    /// Runs the event handler (IDL attribute, or the `on…` content attribute compiled lazily).
    fn handler(cx: &mut Cx<'_, '_, '_>, cur: Target, event: Obj, ty: &str) -> R<()> {
        let w = cx.world;
        let mut f = cx.st().handlers.get(&(w, cur, ty.to_string())).cloned();
        // Content attributes (`onclick="…"`) compile in the main world only.
        if f.is_none()
            && w == 0
            && let Target::Node(n) = cur
        {
            let src = cx
                .dom()
                .get(n)
                .and_then(|x| x.attr(&format!("on{ty}")).map(str::to_string));
            if let Some(src) = src {
                f = compile_handler(cx, &src);
                if let Some(func) = &f {
                    cx.st()
                        .handlers
                        .insert((w, cur, ty.to_string()), func.clone());
                }
            }
        }
        let Some(f) = f else { return Ok(()) };
        let this = cx.wrap(target_obj(cur));
        let ev_js = cx.wrap(event);
        let r = cx.invoke(&f, this.into(), &[ev_js.into()])?;
        if let Some(v) = r {
            let v = cx.local(&v);
            if v.is_false() {
                ev_mut(cx, event, |e| {
                    if e.cancelable {
                        e.canceled = true;
                    }
                })?;
            }
        }
        Ok(())
    }

    fn compile_handler(cx: &mut Cx<'_, '_, '_>, body: &str) -> Option<Value> {
        if cx.st().scripting_disabled() {
            return None;
        }
        let code = format!("(function (event) {{\n{body}\n}})");
        let src = v8::String::new(cx.scope, &code)?;
        let scope = &mut *cx.scope;
        v8::tc_scope!(let tc, scope);
        let f = v8::Script::compile(tc, src, None).and_then(|s| s.run(tc));
        if let (None, Some(ex)) = (&f, tc.exception()) {
            let msg = tc.message();
            let err = crate::engine::page_error(tc, ex, msg);
            cx.realm.shared.borrow_mut().errors.push(err);
        }
        f.map(|v| v8::Global::new(tc, v))
    }

    /// Fires a simple engine event; returns `false` if canceled.
    pub fn fire(
        cx: &mut Cx<'_, '_, '_>,
        t: Target,
        ty: &str,
        bubbles: bool,
        cancelable: bool,
    ) -> R<bool> {
        let data = EventData {
            ty: ty.to_string(),
            bubbles,
            cancelable,
            initialized: true,
            trusted: true,
            time: now(cx),
            ..EventData::default()
        };
        let e = cx.alloc(Iface::Event, Slot::Event(Box::new(data)));
        dispatch(cx, t, e)
    }

    /// `click()`: a synthetic click with its activation behaviour.
    pub fn click(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<()> {
        click_with(cx, el, false, (0.0, 0.0))
    }

    /// A click event at `point` (trusted when the engine dispatches it), with checkbox/radio
    /// pre-activation and, unless cancelled, the activation behaviour (links, forms, labels).
    pub fn click_with(
        cx: &mut Cx<'_, '_, '_>,
        el: NodeId,
        trusted: bool,
        point: (f64, f64),
    ) -> R<()> {
        let (checkbox, radio) = {
            let dom = cx.dom();
            let n = dom.get(el);
            let ty = n
                .filter(|n| n.is_html("input"))
                .and_then(|n| n.attr("type"))
                .map(str::to_ascii_lowercase);
            let disabled = n.is_some_and(|n| n.attr("disabled").is_some());
            if disabled {
                return Ok(());
            }
            (
                ty.as_deref() == Some("checkbox"),
                ty.as_deref() == Some("radio"),
            )
        };
        let before = crate::dom::html_input_element::checked(cx, el);
        if checkbox {
            crate::dom::html_input_element::set_checked_state(cx, el, !before);
        } else if radio {
            crate::dom::html_input_element::set_checked_state(cx, el, true);
        }
        // A trusted click's count and modifiers are the input action's.
        let (detail, mods) = if trusted {
            let st = cx.st();
            (i32::from(st.click_count.max(1)), st.mods)
        } else {
            (1, crate::dom::input::Mods::default())
        };
        let data = EventData {
            ty: "click".into(),
            bubbles: true,
            cancelable: true,
            composed: true,
            initialized: true,
            trusted,
            time: now(cx),
            ui_detail: detail,
            input: InputData {
                button: (0, 0),
                mods: (mods.ctrl, mods.shift, mods.alt, mods.meta),
                client: point,
                screen: (point.0 as i32, point.1 as i32),
                page_offset: crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport),
                ..InputData::default()
            },
            ..EventData::default()
        };
        let e = cx.alloc(Iface::MouseEvent, Slot::Event(Box::new(data)));
        let ok = dispatch(cx, Target::Node(el), e)?;
        if !ok && (checkbox || radio) {
            crate::dom::html_input_element::set_checked_state(cx, el, before);
        } else if checkbox || radio {
            fire(cx, Target::Node(el), "input", true, false)?;
            fire(cx, Target::Node(el), "change", true, false)?;
        }
        if ok {
            crate::dom::input::activate(cx, el)?;
        }
        Ok(())
    }

    /// Moves focus, firing `blur`/`focusout` and `focus`/`focusin`.
    pub fn move_focus(cx: &mut Cx<'_, '_, '_>, to: Option<NodeId>) -> R<()> {
        let from = cx.st().focused;
        if from == to {
            return Ok(());
        }
        cx.st().focused = to;
        cx.st().focus_seq = crate::realm::next_focus_seq();
        let focus_event =
            |cx: &mut Cx<'_, '_, '_>, ty: &str, bubbles: bool, related: Option<NodeId>| {
                let data = EventData {
                    ty: ty.into(),
                    bubbles,
                    composed: true,
                    initialized: true,
                    trusted: true,
                    time: now(cx),
                    related: related.map(Target::Node),
                    ..EventData::default()
                };
                cx.alloc(Iface::FocusEvent, Slot::Event(Box::new(data)))
            };
        if let Some(f) = from {
            let e = focus_event(cx, "blur", false, to);
            dispatch(cx, Target::Node(f), e)?;
            let e = focus_event(cx, "focusout", true, to);
            dispatch(cx, Target::Node(f), e)?;
        }
        if let Some(t) = to {
            let e = focus_event(cx, "focus", false, from);
            dispatch(cx, Target::Node(t), e)?;
            let e = focus_event(cx, "focusin", true, from);
            dispatch(cx, Target::Node(t), e)?;
            crate::dom::editing::focused(cx, t);
        }
        Ok(())
    }
}

/// `Event`.
pub mod event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: EventInit) -> R<Obj> {
        let data = base(cx, ty, &init);
        Ok(cx.alloc(Iface::Event, Slot::Event(Box::new(data))))
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        ev(cx, this, |e| e.ty.clone())
    }

    /// A node retargeted against the listener (shadow trees are invisible from outside).
    pub(crate) fn seen(
        cx: &Cx<'_, '_, '_>,
        t: Option<Target>,
        current: Option<Target>,
    ) -> Option<Obj> {
        let dom = cx.dom();
        match (t, current) {
            (Some(Target::Node(n)), cur) if dom.has_shadow() => {
                let against = match cur {
                    Some(Target::Node(c)) => c,
                    _ => dom.root(),
                };
                Some(Obj::Node(crate::dom::shadow::retarget(&dom, n, against)))
            }
            (t, _) => t.map(target_obj),
        }
    }

    pub fn get_target(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let (t, cur) = ev(cx, this, |e| (e.target, e.current))?;
        Ok(seen(cx, t, cur))
    }

    pub fn get_src_element(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        get_target(cx, this)
    }

    pub fn get_current_target(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        ev(cx, this, |e| e.current.map(target_obj))
    }

    /// The path, without nodes in closed shadow trees the current target is not inside.
    pub fn composed_path(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<Obj>> {
        let (path, dispatching, current) =
            ev(cx, this, |e| (e.path.clone(), e.dispatching, e.current))?;
        if !dispatching {
            return Ok(Vec::new());
        }
        let dom = cx.dom();
        let against = match current {
            Some(Target::Node(c)) => c,
            _ => dom.root(),
        };
        Ok(path
            .into_iter()
            .filter(|t| match t {
                Target::Node(n) => !crate::dom::shadow::closed_hidden(&dom, *n, against),
                _ => true,
            })
            .map(target_obj)
            .collect())
    }

    pub fn get_event_phase(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        ev(cx, this, |e| e.phase)
    }

    pub fn stop_propagation(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        ev_mut(cx, this, |e| e.stop = true)
    }

    pub fn get_cancel_bubble(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.stop)
    }

    pub fn set_cancel_bubble(cx: &mut Cx<'_, '_, '_>, this: Obj, v: bool) -> R<()> {
        ev_mut(cx, this, |e| e.stop |= v)
    }

    pub fn stop_immediate_propagation(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        ev_mut(cx, this, |e| {
            e.stop = true;
            e.stop_immediate = true;
        })
    }

    pub fn get_bubbles(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.bubbles)
    }

    pub fn get_cancelable(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.cancelable)
    }

    pub fn get_return_value(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| !e.canceled)
    }

    pub fn set_return_value(cx: &mut Cx<'_, '_, '_>, this: Obj, v: bool) -> R<()> {
        ev_mut(cx, this, |e| {
            if !v && e.cancelable {
                e.canceled = true;
            }
        })
    }

    pub fn prevent_default(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        ev_mut(cx, this, |e| {
            if e.cancelable {
                e.canceled = true;
            }
        })
    }

    pub fn get_default_prevented(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.canceled)
    }

    pub fn get_composed(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.composed)
    }

    pub fn get_is_trusted(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.trusted)
    }

    pub fn get_time_stamp(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        ev(cx, this, |e| e.time)
    }

    pub fn init_event(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        ty: String,
        bubbles: bool,
        cancelable: bool,
    ) -> R<()> {
        ev_mut(cx, this, |e| {
            if !e.dispatching {
                e.ty = ty;
                e.bubbles = bubbles;
                e.cancelable = cancelable;
                e.initialized = true;
                e.canceled = false;
                e.stop = false;
                e.stop_immediate = false;
                e.target = None;
            }
        })
    }
}

/// `CustomEvent`.
pub mod custom_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: CustomEventInit) -> R<Obj> {
        let mut data = base(
            cx,
            ty,
            &EventInit {
                bubbles: init.bubbles,
                cancelable: init.cancelable,
                composed: init.composed,
            },
        );
        data.detail = init.detail;
        Ok(cx.alloc(Iface::CustomEvent, Slot::Event(Box::new(data))))
    }

    pub fn get_detail(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let d = ev(cx, this, |e| e.detail.clone())?;
        Ok(d.unwrap_or_else(|| {
            let n = v8::null(cx.scope);
            cx.keep(n.into())
        }))
    }

    pub fn init_custom_event(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        ty: String,
        bubbles: bool,
        cancelable: bool,
        detail: Value,
    ) -> R<()> {
        super::event::init_event(cx, this, ty, bubbles, cancelable)?;
        ev_mut(cx, this, |e| e.detail = Some(detail))
    }
}

/// `UIEvent`.
pub mod ui_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: UIEventInit) -> R<Obj> {
        let mut data = base(
            cx,
            ty,
            &EventInit {
                bubbles: init.bubbles,
                cancelable: init.cancelable,
                composed: init.composed,
            },
        );
        data.ui_detail = init.detail;
        Ok(cx.alloc(Iface::UIEvent, Slot::Event(Box::new(data))))
    }

    pub fn get_view(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(Some(Obj::Window))
    }

    pub fn get_detail(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        ev(cx, this, |e| e.ui_detail)
    }

    pub fn get_which(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        ev(cx, this, |e| {
            if e.input.legacy.1 != 0 {
                e.input.legacy.1
            } else {
                u32::try_from(e.input.button.0 + 1).unwrap_or(0)
            }
        })
    }
}

/// `MouseEvent`.
/// `DragEvent`: a mouse event with the drag's data.
pub mod drag_event {
    use super::*;
    use crate::generated::{DragEventInit, MouseEventInit};

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: DragEventInit) -> R<Obj> {
        let mouse = MouseEventInit {
            bubbles: init.bubbles,
            cancelable: init.cancelable,
            composed: init.composed,
            detail: init.detail,
            screen_x: init.screen_x,
            screen_y: init.screen_y,
            client_x: init.client_x,
            client_y: init.client_y,
            ctrl_key: init.ctrl_key,
            shift_key: init.shift_key,
            alt_key: init.alt_key,
            meta_key: init.meta_key,
            button: init.button,
            buttons: init.buttons,
            related_target: init.related_target,
            ..MouseEventInit::default()
        };
        let mut data = super::mouse_event::data(cx, ty, mouse);
        data.drag = match init.data_transfer {
            Some(d) => Some(d.slab()?),
            None => None,
        };
        Ok(cx.alloc(Iface::DragEvent, Slot::Event(Box::new(data))))
    }

    pub fn get_data_transfer(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        match cx.st().slot(this.slab()?) {
            Some(Slot::Event(e)) => Ok(e.drag.map(Obj::Slab)),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }
}

pub mod mouse_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: MouseEventInit) -> R<Obj> {
        let data = data(cx, ty, init);
        Ok(cx.alloc(Iface::MouseEvent, Slot::Event(Box::new(data))))
    }

    /// The event data of a mouse event (and of events built on it).
    pub(crate) fn data(cx: &mut Cx<'_, '_, '_>, ty: String, init: MouseEventInit) -> EventData {
        let mut data = base(
            cx,
            ty,
            &EventInit {
                bubbles: init.bubbles,
                cancelable: init.cancelable,
                composed: init.composed,
            },
        );
        data.ui_detail = init.detail;
        data.input = InputData {
            screen: (init.screen_x, init.screen_y),
            client: (f64::from(init.client_x), f64::from(init.client_y)),
            mods: (init.ctrl_key, init.shift_key, init.alt_key, init.meta_key),
            button: (init.button, init.buttons),
            page_offset: crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport),
            ..InputData::default()
        };
        data.related = related(init.related_target);
        data
    }

    pub fn get_screen_x(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        ev(cx, this, |e| e.input.screen.0)
    }

    pub fn get_screen_y(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        ev(cx, this, |e| e.input.screen.1)
    }

    pub fn get_client_x(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        ev(cx, this, |e| e.input.client.0 as i32)
    }

    pub fn get_client_y(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        ev(cx, this, |e| e.input.client.1 as i32)
    }

    pub fn get_page_x(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        ev(cx, this, |e| e.input.client.0 + e.input.page_offset.0)
    }

    pub fn get_page_y(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        ev(cx, this, |e| e.input.client.1 + e.input.page_offset.1)
    }

    pub fn get_x(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        get_page_x(cx, this)
    }

    pub fn get_y(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        get_page_y(cx, this)
    }

    pub fn get_offset_x(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let (target, x) = ev(cx, this, |e| (e.target, e.input.client.0))?;
        let left = match target {
            Some(Target::Node(n)) => crate::dom::elements_rect_x(cx, n),
            _ => 0.0,
        };
        Ok(x - left)
    }

    pub fn get_offset_y(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let (target, y) = ev(cx, this, |e| (e.target, e.input.client.1))?;
        let top = match target {
            Some(Target::Node(n)) => crate::dom::elements_rect_y(cx, n),
            _ => 0.0,
        };
        Ok(y - top)
    }

    pub fn get_ctrl_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.mods.0)
    }

    pub fn get_shift_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.mods.1)
    }

    pub fn get_alt_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.mods.2)
    }

    pub fn get_meta_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.mods.3)
    }

    pub fn get_button(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i16> {
        ev(cx, this, |e| e.input.button.0)
    }

    pub fn get_buttons(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        ev(cx, this, |e| e.input.button.1)
    }

    pub fn get_related_target(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let (r, cur) = ev(cx, this, |e| (e.related, e.current))?;
        Ok(super::event::seen(cx, r, cur))
    }
}

/// `WheelEvent`.
pub mod wheel_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: WheelEventInit) -> R<Obj> {
        let mut data = base(
            cx,
            ty,
            &EventInit {
                bubbles: init.bubbles,
                cancelable: init.cancelable,
                composed: init.composed,
            },
        );
        data.ui_detail = init.detail;
        data.input = InputData {
            screen: (init.screen_x, init.screen_y),
            client: (f64::from(init.client_x), f64::from(init.client_y)),
            mods: (init.ctrl_key, init.shift_key, init.alt_key, init.meta_key),
            button: (init.button, init.buttons),
            page_offset: crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport),
            delta: (init.delta_x, init.delta_y, init.delta_z, init.delta_mode),
            ..InputData::default()
        };
        data.related = related(init.related_target);
        Ok(cx.alloc(Iface::WheelEvent, Slot::Event(Box::new(data))))
    }

    pub fn get_delta_x(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        ev(cx, this, |e| e.input.delta.0)
    }

    pub fn get_delta_y(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        ev(cx, this, |e| e.input.delta.1)
    }

    pub fn get_delta_z(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        ev(cx, this, |e| e.input.delta.2)
    }

    pub fn get_delta_mode(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        ev(cx, this, |e| e.input.delta.3)
    }
}

/// `KeyboardEvent`.
pub mod keyboard_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: KeyboardEventInit) -> R<Obj> {
        let mut data = base(
            cx,
            ty,
            &EventInit {
                bubbles: init.bubbles,
                cancelable: init.cancelable,
                composed: init.composed,
            },
        );
        data.input = InputData {
            mods: (init.ctrl_key, init.shift_key, init.alt_key, init.meta_key),
            key: (init.key, init.code),
            location: init.location,
            repeat: init.repeat,
            legacy: (init.char_code, init.key_code),
            ..InputData::default()
        };
        Ok(cx.alloc(Iface::KeyboardEvent, Slot::Event(Box::new(data))))
    }

    pub fn get_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        ev(cx, this, |e| e.input.key.0.clone())
    }

    pub fn get_code(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        ev(cx, this, |e| e.input.key.1.clone())
    }

    pub fn get_location(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        ev(cx, this, |e| e.input.location)
    }

    pub fn get_ctrl_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.mods.0)
    }

    pub fn get_shift_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.mods.1)
    }

    pub fn get_alt_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.mods.2)
    }

    pub fn get_meta_key(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.mods.3)
    }

    pub fn get_repeat(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.repeat)
    }

    pub fn get_is_composing(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(false)
    }

    pub fn get_char_code(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        ev(cx, this, |e| e.input.legacy.0)
    }

    pub fn get_key_code(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        ev(cx, this, |e| e.input.legacy.1)
    }

    pub fn get_modifier_state(cx: &mut Cx<'_, '_, '_>, this: Obj, key: String) -> R<bool> {
        ev(cx, this, |e| match key.as_str() {
            "Control" => e.input.mods.0,
            "Shift" => e.input.mods.1,
            "Alt" => e.input.mods.2,
            "Meta" => e.input.mods.3,
            _ => false,
        })
    }
}

/// `FocusEvent`.
pub mod focus_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: FocusEventInit) -> R<Obj> {
        let mut data = base(
            cx,
            ty,
            &EventInit {
                bubbles: init.bubbles,
                cancelable: init.cancelable,
                composed: init.composed,
            },
        );
        data.related = related(init.related_target);
        Ok(cx.alloc(Iface::FocusEvent, Slot::Event(Box::new(data))))
    }

    pub fn get_related_target(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let (r, cur) = ev(cx, this, |e| (e.related, e.current))?;
        Ok(super::event::seen(cx, r, cur))
    }
}

/// `PointerEvent`.
pub mod pointer_event {
    use super::*;
    use crate::generated::PointerEventInit;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: PointerEventInit) -> R<Obj> {
        let mut data = base(
            cx,
            ty,
            &EventInit {
                bubbles: init.bubbles,
                cancelable: init.cancelable,
                composed: init.composed,
            },
        );
        data.ui_detail = init.detail;
        data.input = InputData {
            screen: (init.screen_x, init.screen_y),
            client: (f64::from(init.client_x), f64::from(init.client_y)),
            mods: (init.ctrl_key, init.shift_key, init.alt_key, init.meta_key),
            button: (init.button, init.buttons),
            pointer: (init.pointer_id, init.pointer_type.clone(), init.is_primary),
            ..InputData::default()
        };
        data.related = related(init.related_target);
        Ok(cx.alloc(Iface::PointerEvent, Slot::Event(Box::new(data))))
    }

    pub fn get_pointer_id(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        ev(cx, this, |e| e.input.pointer.0)
    }

    pub fn get_pointer_type(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        ev(cx, this, |e| e.input.pointer.1.clone())
    }

    pub fn get_is_primary(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        ev(cx, this, |e| e.input.pointer.2)
    }
}
