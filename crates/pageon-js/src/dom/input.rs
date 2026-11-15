//! User input dispatched by the engine (trusted events) and the browser's default actions:
//! pointer and mouse sequences, focus, typing, keys, `<select>`, hover; link navigation, form
//! submission, labels, `<details>`. Navigations are recorded on the realm and performed by the
//! caller after the task (never inside a V8 callback).

use pageon_dom::{Edge, NodeData, NodeId};

use crate::cv::{Cx, Obj, R};
use crate::dom::event_target::{dispatch, fire, move_focus};
use crate::generated::Iface;
use crate::realm::{EventData, InputData, NavRequest, Slot, Target};

/// Modifier keys held during an input action.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Mods {
    /// Control.
    pub ctrl: bool,
    /// Shift.
    pub shift: bool,
    /// Alt (Option).
    pub alt: bool,
    /// Meta (Command, Super, Windows).
    pub meta: bool,
}

impl Mods {
    /// The modifier a name stands for (`ctrl`, `control`, `shift`, `alt`, `option`, `meta`,
    /// `super`, `cmd`, `command`, `win`), case-insensitive.
    pub fn with(mut self, name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => self.ctrl = true,
            "shift" => self.shift = true,
            "alt" | "option" => self.alt = true,
            "meta" | "super" | "cmd" | "command" | "win" => self.meta = true,
            _ => return None,
        }
        Some(self)
    }

    /// Whether no modifier is held.
    pub fn none(self) -> bool {
        self == Self::default()
    }

    /// The held modifiers' DOM key names, in pressing order.
    fn keys(self) -> Vec<&'static str> {
        [
            (self.ctrl, "Control"),
            (self.shift, "Shift"),
            (self.alt, "Alt"),
            (self.meta, "Meta"),
        ]
        .into_iter()
        .filter_map(|(on, k)| on.then_some(k))
        .collect()
    }

    fn tuple(self) -> (bool, bool, bool, bool) {
        (self.ctrl, self.shift, self.alt, self.meta)
    }
}

/// A mouse button.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Button {
    /// The primary button.
    #[default]
    Left,
    /// The wheel button.
    Middle,
    /// The secondary button.
    Right,
}

impl Button {
    /// `MouseEvent.button`.
    fn code(self) -> i16 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
        }
    }

    /// The button's bit in `MouseEvent.buttons`.
    fn mask(self) -> u16 {
        match self {
            Self::Left => 1,
            Self::Middle => 4,
            Self::Right => 2,
        }
    }
}

/// How a click is given: the button, the clicks (1 single, 2 double, 3 triple), the modifiers
/// held, and whether only the `count`-th click runs (a protocol client sends each click).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClickKind {
    /// The button.
    pub button: Button,
    /// The click count.
    pub count: u8,
    /// Modifier keys.
    pub mods: Mods,
    /// Only the `count`-th click.
    pub nth: bool,
}

/// The DOM key for a key name, accepting xdotool-style names as computer-use models send them
/// (`Return`, `BackSpace`, `Page_Down`, `Left`, `super` …); `None` for an unknown name.
pub fn key_name(name: &str) -> Option<String> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(c.to_string());
    }
    let k = match name.to_ascii_lowercase().as_str() {
        "return" | "enter" | "kp_enter" => "Enter",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "escape" | "esc" => "Escape",
        "delete" | "del" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "page_up" | "pageup" | "prior" => "PageUp",
        "page_down" | "pagedown" | "next" => "PageDown",
        "left" | "arrowleft" => "ArrowLeft",
        "right" | "arrowright" => "ArrowRight",
        "up" | "arrowup" => "ArrowUp",
        "down" | "arrowdown" => "ArrowDown",
        "space" => " ",
        "ctrl" | "control" | "control_l" | "control_r" => "Control",
        "shift" | "shift_l" | "shift_r" => "Shift",
        "alt" | "option" | "alt_l" | "alt_r" => "Alt",
        "meta" | "super" | "super_l" | "super_r" | "cmd" | "command" | "win" => "Meta",
        "capslock" | "caps_lock" => "CapsLock",
        f if f.len() <= 3
            && f.starts_with('f')
            && f[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
        {
            return Some(format!("F{}", &f[1..]));
        }
        // Other DOM key names (`NumLock`, `ContextMenu`, `AudioVolumeUp`), as protocol clients
        // send them.
        _ if name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && name.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            return Some(name.to_string());
        }
        _ => return None,
    };
    Some(k.to_string())
}

/// A chord (`ctrl+shift+Left`, `Return`, `a`): its modifiers and its key; `None` when a part is
/// unknown. A chord of modifiers only (`shift`) is that modifier's key.
pub fn chord(spec: &str) -> Option<(Mods, String)> {
    let parts: Vec<&str> = spec.split('+').collect();
    // `+` itself, or a chord ending in `+` (`ctrl++`).
    let (mods, key) = match parts.as_slice() {
        [] => return None,
        [k] => (&[][..], *k),
        [m @ .., ""] => (m, "+"),
        [m @ .., k] => (m, *k),
    };
    let mut held = Mods::default();
    for m in mods {
        held = held.with(m)?;
    }
    Some((held, key_name(key)?))
}

fn now(cx: &Cx<'_, '_, '_>) -> f64 {
    let sh = cx.realm.shared.borrow();
    sh.now - sh.origin
}

fn attr(cx: &Cx<'_, '_, '_>, id: NodeId, name: &str) -> Option<String> {
    cx.dom().get(id)?.attr(name).map(str::to_string)
}

fn tag(cx: &Cx<'_, '_, '_>, id: NodeId) -> String {
    cx.dom()
        .get(id)
        .and_then(|n| n.local_name().map(str::to_string))
        .unwrap_or_default()
}

fn input_type(cx: &Cx<'_, '_, '_>, id: NodeId) -> String {
    attr(cx, id, "type")
        .map(|t| t.to_ascii_lowercase())
        .unwrap_or_else(|| "text".into())
}

/// The centre of an element's box (viewport coordinates; 0,0 without a box).
fn center(cx: &Cx<'_, '_, '_>, el: NodeId) -> (f64, f64) {
    cx.realm.layout().client_rect(el).map_or((0.0, 0.0), |r| {
        (
            f64::from(r.x) + f64::from(r.width) / 2.0,
            f64::from(r.y) + f64::from(r.height) / 2.0,
        )
    })
}

/// Fires a trusted mouse (or pointer) event; returns `false` if it was cancelled.
fn mouse(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    ty: &str,
    point: (f64, f64),
    related: Option<NodeId>,
) -> R<bool> {
    let pointer = ty.starts_with("pointer");
    let bubbles = !ty.ends_with("enter") && !ty.ends_with("leave");
    let cancelable = bubbles && !ty.ends_with("move") || ty == "mousemove" || ty == "pointermove";
    // The button is down from the press until the release (moves while dragging carry it).
    let pressed = matches!(ty, "mousedown" | "pointerdown")
        || (ty.ends_with("move") && cx.st().pressed.is_some());
    let page_offset = crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport);
    cx.st().scroll.pointer = Some(point);
    let (mods, button, clicks) = {
        let st = cx.st();
        (st.mods.tuple(), st.button, st.click_count.max(1))
    };
    let data = EventData {
        ty: ty.into(),
        bubbles,
        cancelable,
        composed: true,
        initialized: true,
        trusted: true,
        time: now(cx),
        ui_detail: if matches!(
            ty,
            "mousedown" | "mouseup" | "click" | "auxclick" | "dblclick" | "contextmenu"
        ) {
            i32::from(clicks)
        } else {
            0
        },
        input: InputData {
            client: point,
            screen: (point.0 as i32, point.1 as i32),
            mods,
            button: (button.code(), if pressed { button.mask() } else { 0 }),
            pointer: if pointer {
                (1, "mouse".into(), true)
            } else {
                (0, String::new(), false)
            },
            page_offset,
            ..InputData::default()
        },
        related: related.map(Target::Node),
        ..EventData::default()
    };
    let iface = if pointer {
        Iface::PointerEvent
    } else {
        Iface::MouseEvent
    };
    let e = cx.alloc(iface, Slot::Event(Box::new(data)));
    dispatch(cx, Target::Node(el), e)
}

/// A trusted, cancelable `wheel` event at `el` (pixel deltas); `false` if it was cancelled.
pub(crate) fn wheel_event(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    point: (f64, f64),
    dx: f64,
    dy: f64,
) -> R<bool> {
    let page_offset = crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport);
    let mods = cx.st().mods.tuple();
    let data = EventData {
        ty: "wheel".into(),
        bubbles: true,
        cancelable: true,
        composed: true,
        initialized: true,
        trusted: true,
        time: now(cx),
        input: InputData {
            client: point,
            screen: (point.0 as i32, point.1 as i32),
            page_offset,
            delta: (dx, dy, 0.0, 0),
            mods,
            ..InputData::default()
        },
        ..EventData::default()
    };
    let e = cx.alloc(Iface::WheelEvent, Slot::Event(Box::new(data)));
    dispatch(cx, Target::Node(el), e)
}

/// The element input at `point` (default: the centre of `el`) goes to: the topmost element
/// there when it is `el` or inside it (a link inside a tab, a button inside a header), else
/// `el` itself.
fn target_at(cx: &Cx<'_, '_, '_>, el: NodeId, point: Option<(f64, f64)>) -> (NodeId, (f64, f64)) {
    let p = point.unwrap_or_else(|| center(cx, el));
    let hit = cx.realm.layout().element_at(p.0 as f32, p.1 as f32);
    let inside = hit.filter(|&h| h == el || cx.dom().flat_ancestors(h).any(|a| a == el));
    (inside.unwrap_or(el), p)
}

/// Moves the pointer onto `el` (at `point`, default its centre): `out`/`leave` on the previous
/// element, `over`/`enter` and `move` on the new one.
pub fn hover(cx: &mut Cx<'_, '_, '_>, el: NodeId, point: Option<(f64, f64)>) -> R<()> {
    let (el, p) = target_at(cx, el, point);
    let prev = cx.st().hovered;
    if prev != Some(el) {
        if let Some(old) = prev.filter(|&o| cx.dom().get(o).is_some()) {
            for ty in ["pointerout", "pointerleave", "mouseout", "mouseleave"] {
                mouse(cx, old, ty, p, Some(el))?;
            }
        }
        cx.st().hovered = Some(el);
        for ty in ["pointerover", "pointerenter", "mouseover", "mouseenter"] {
            mouse(cx, el, ty, p, prev)?;
        }
    }
    mouse(cx, el, "pointermove", p, None)?;
    mouse(cx, el, "mousemove", p, None)?;
    Ok(())
}

/// Whether an element takes focus.
pub(crate) fn focusable(cx: &Cx<'_, '_, '_>, id: NodeId) -> bool {
    let dom = cx.dom();
    let Some(n) = dom.get(id) else { return false };
    if n.attr("disabled").is_some() {
        return false;
    }
    if n.attr("tabindex").is_some() || editable_host(&dom, id) {
        return true;
    }
    match n.local_name().unwrap_or_default() {
        "a" | "area" => n.attr("href").is_some(),
        "input" => n
            .attr("type")
            .is_none_or(|t| !t.eq_ignore_ascii_case("hidden")),
        "button" | "select" | "textarea" | "iframe" | "summary" => true,
        _ => false,
    }
}

fn editable_host(dom: &pageon_dom::Dom, id: NodeId) -> bool {
    let mut cur = Some(id);
    while let Some(c) = cur {
        match dom.get(c).and_then(|n| n.attr("contenteditable")) {
            Some(v) if v.eq_ignore_ascii_case("false") => return false,
            Some(_) => return true,
            None => cur = dom.parent(c),
        }
    }
    false
}

/// A full trusted click at `point` (default: the element's centre), on the topmost element
/// there inside `el`: hover, `pointerdown`/`mousedown`, focus, `pointerup`/`mouseup`, then
/// `click` with its activation behaviour.
pub fn click(cx: &mut Cx<'_, '_, '_>, el: NodeId, point: Option<(f64, f64)>) -> R<()> {
    let (el, p) = target_at(cx, el, point);
    hover(cx, el, Some(p))?;
    mouse(cx, el, "pointerdown", p, None)?;
    if mouse(cx, el, "mousedown", p, None)? {
        let mut target = Some(el);
        while let Some(t) = target {
            if focusable(cx, t) {
                break;
            }
            target = cx.dom().parent(t);
        }
        move_focus(cx, target)?;
    }
    mouse(cx, el, "pointerup", p, None)?;
    mouse(cx, el, "mouseup", p, None)?;
    crate::dom::event_target::click_with(cx, el, true, p)
}

/// A trusted click with any button, click count (1–3) and modifiers, as a person's: for the
/// primary button each click runs as [`click`] (`detail` counting up) and the second fires
/// `dblclick` (with `nth`, only the `count`-th click: a protocol client sends each one); the middle button fires `auxclick` (a link opens in a new tab), the secondary
/// `contextmenu` (no menu) then `auxclick`. Modifiers are on every event; a ctrl-, meta- or
/// shift-click on a link opens it in a new tab.
pub fn click_ex(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    point: Option<(f64, f64)>,
    kind: ClickKind,
) -> R<()> {
    let ClickKind {
        button,
        count,
        mods,
        nth,
    } = kind;
    let saved = {
        let mut st = cx.st();
        let saved = (st.mods, st.button, st.click_count);
        st.mods = mods;
        st.button = button;
        saved
    };
    let r = click_ex_inner(cx, el, point, button, count.clamp(1, 3), nth);
    {
        let mut st = cx.st();
        (st.mods, st.button, st.click_count) = saved;
    }
    r
}

fn click_ex_inner(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    point: Option<(f64, f64)>,
    button: Button,
    count: u8,
    nth: bool,
) -> R<()> {
    let (el, p) = target_at(cx, el, point);
    if button == Button::Left {
        for n in (if nth { count } else { 1 })..=count {
            cx.st().click_count = n;
            click(cx, el, Some(p))?;
            if n == 2 {
                mouse(cx, el, "dblclick", p, None)?;
            }
        }
        return Ok(());
    }
    cx.st().click_count = 1;
    hover(cx, el, Some(p))?;
    cx.st().pressed = Some(Press {
        node: el,
        point: p,
        moved: false,
    });
    mouse(cx, el, "pointerdown", p, None)?;
    let down = mouse(cx, el, "mousedown", p, None)?;
    if down && button == Button::Right {
        mouse(cx, el, "contextmenu", p, None)?;
    }
    mouse(cx, el, "pointerup", p, None)?;
    cx.st().pressed = None;
    mouse(cx, el, "mouseup", p, None)?;
    if mouse(cx, el, "auxclick", p, None)? && button == Button::Middle {
        // The middle button's default on a link: open it in a new tab.
        let mut cur = Some(el);
        while let Some(c) = cur {
            if matches!(tag(cx, c).as_str(), "a" | "area")
                && let Some(href) = attr(cx, c, "href")
            {
                follow_link(cx, &href, Some("_blank"));
                break;
            }
            cur = cx.dom().parent(c);
        }
    }
    Ok(())
}

/// The activation behaviour after a click that was not cancelled, of the clicked element or
/// its nearest ancestor that has one.
pub(crate) fn activate(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<()> {
    let mut cur = Some(el);
    while let Some(c) = cur {
        let t = tag(cx, c);
        let html = cx
            .dom()
            .get(c)
            .is_some_and(|n| n.namespace() == Some(pageon_dom::Namespace::Html));
        if html {
            match t.as_str() {
                "a" | "area" => {
                    if let Some(href) = attr(cx, c, "href") {
                        follow_link(cx, &href, attr(cx, c, "target").as_deref());
                        return Ok(());
                    }
                }
                "button" if !pageon_dom::forms::disabled(&cx.dom(), c) => {
                    let ty = attr(cx, c, "type").map(|t| t.to_ascii_lowercase());
                    match (ty.as_deref(), form_of(cx, c)) {
                        (None | Some("submit"), Some(form)) => {
                            return submit_form(cx, form, Some(c), true);
                        }
                        (Some("reset"), Some(form)) => return reset_form(cx, form),
                        _ => {}
                    }
                    return Ok(());
                }
                "input" if !pageon_dom::forms::disabled(&cx.dom(), c) => {
                    match (input_type(cx, c).as_str(), form_of(cx, c)) {
                        ("submit" | "image", Some(form)) => {
                            return submit_form(cx, form, Some(c), true);
                        }
                        ("reset", Some(form)) => return reset_form(cx, form),
                        // A file input opens the file chooser (filled by an upload).
                        ("file", _) => cx.st().chooser = Some(c),
                        _ => {}
                    }
                    return Ok(());
                }
                "label" => {
                    if let Some(control) = label_control(cx, c)
                        && control != el
                        && !cx.dom().ancestors(el).any(|a| a == control)
                    {
                        return crate::dom::event_target::click_with(
                            cx,
                            control,
                            true,
                            center(cx, control),
                        );
                    }
                    return Ok(());
                }
                "summary" => {
                    let parent = cx.dom().parent(c);
                    if let Some(details) = parent.filter(|&d| tag(cx, d) == "details") {
                        if attr(cx, details, "open").is_some() {
                            let _ = cx.dom_mut().remove_attr(details, "open");
                        } else {
                            let _ = cx.dom_mut().set_attr(details, "open", "");
                        }
                        fire(cx, Target::Node(details), "toggle", false, false)?;
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
        cur = cx.dom().parent(c);
    }
    Ok(())
}

/// The control a `<label>` labels (`for`, or its first labelable descendant).
fn label_control(cx: &Cx<'_, '_, '_>, label: NodeId) -> Option<NodeId> {
    let dom = cx.dom();
    if let Some(id) = dom.get(label)?.attr("for") {
        return dom.traverse(dom.root()).find_map(|e| match e {
            Edge::Enter(n) if dom.get(n).and_then(|x| x.attr("id")) == Some(id) => Some(n),
            _ => None,
        });
    }
    dom.traverse(label).find_map(|e| match e {
        Edge::Enter(n)
            if n != label
                && dom.get(n).is_some_and(|x| {
                    matches!(
                        x.local_name(),
                        Some("input" | "select" | "textarea" | "button")
                    )
                }) =>
        {
            Some(n)
        }
        _ => None,
    })
}

/// The form owner of a control (`form` attribute, or the nearest ancestor form).
pub(crate) fn form_of(cx: &Cx<'_, '_, '_>, el: NodeId) -> Option<NodeId> {
    pageon_dom::forms::owner(&cx.dom(), el)
}

fn base_url(cx: &Cx<'_, '_, '_>) -> Option<url::Url> {
    url::Url::parse(&cx.st().url).ok()
}

fn follow_link(cx: &mut Cx<'_, '_, '_>, href: &str, target: Option<&str>) {
    let href = href.trim();
    if let Some(code) = href.strip_prefix("javascript:") {
        run_javascript_url(cx, code);
        return;
    }
    let Some(url) = base_url(cx).and_then(|b| b.join(href).ok()) else {
        return;
    };
    let mods = cx.st().mods;
    let new_tab = mods.ctrl || mods.meta || mods.shift || cx.st().button == Button::Middle;
    if new_tab || target.is_some_and(|t| t.eq_ignore_ascii_case("_blank")) {
        cx.st().push_popup(url.to_string());
    } else {
        navigate(cx, url.as_str(), false);
    }
}

/// Runs the code of a `javascript:` URL (`javascript:void(0)` does nothing).
fn run_javascript_url(cx: &mut Cx<'_, '_, '_>, code: &str) {
    let code = percent_decode(code);
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    if let Some(src) = v8::String::new(tc, &code) {
        let ok = v8::Script::compile(tc, src, None)
            .and_then(|s| s.run(tc))
            .is_some();
        if !ok
            && !tc.is_execution_terminating()
            && let Some(ex) = tc.exception()
        {
            let msg = tc.message();
            let err = crate::engine::page_error(tc, ex, msg);
            cx.realm.shared.borrow_mut().errors.push(err);
        }
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Asks for a navigation of this document (the last request of a task wins). A change of the
/// fragment only is handled in place (`hashchange`).
pub(crate) fn navigate(cx: &mut Cx<'_, '_, '_>, url: &str, replace: bool) {
    let current = cx.st().url.clone();
    if let (Ok(a), Ok(b)) = (url::Url::parse(&current), url::Url::parse(url))
        && b.fragment().is_some()
        && a[..url::Position::AfterQuery] == b[..url::Position::AfterQuery]
    {
        cx.st().url = url.to_string();
        crate::dom::session::fragment(cx, url, replace);
        crate::dom::scroll::to_fragment(cx, b.fragment().unwrap_or_default());
        let _ = fire(cx, Target::Window, "hashchange", false, false);
        return;
    }
    cx.st().navigation = Some(NavRequest {
        url: url.to_string(),
        method: "GET".into(),
        body: Vec::new(),
        content_type: None,
        replace,
    });
}

/// Submits a form (`submitter` is the button, if any). `event`: fire `submit` first, which can
/// cancel (`requestSubmit`, buttons, implicit submission); `submit()` does not.
pub(crate) fn submit_form(
    cx: &mut Cx<'_, '_, '_>,
    form: NodeId,
    submitter: Option<NodeId>,
    event: bool,
) -> R<()> {
    // Interactive validation (HTML): unless `novalidate` or the submitter's `formnovalidate`.
    let validate = event
        && attr(cx, form, "novalidate").is_none()
        && submitter.is_none_or(|s| attr(cx, s, "formnovalidate").is_none());
    if validate && !crate::dom::validation::interactive(cx, form)? {
        return Ok(());
    }
    if event && !fire(cx, Target::Node(form), "submit", true, true)? {
        return Ok(());
    }
    let pick = |cx: &Cx<'_, '_, '_>, sub: &str, own: &str| {
        submitter
            .and_then(|s| attr(cx, s, sub))
            .or_else(|| attr(cx, form, own))
    };
    let method = pick(cx, "formmethod", "method")
        .map(|m| m.to_ascii_lowercase())
        .unwrap_or_else(|| "get".into());
    if method == "dialog" {
        let dialog = cx
            .dom()
            .ancestors(form)
            .find(|&a| cx.dom().get(a).is_some_and(|n| n.is_html("dialog")));
        if let Some(d) = dialog {
            let rv = submitter.and_then(|s| attr(cx, s, "value"));
            crate::dom::html_dialog_element::close(cx, Obj::Node(d), rv)?;
        }
        return Ok(());
    }
    let Some(base) = base_url(cx) else {
        return Ok(());
    };
    let action = pick(cx, "formaction", "action").unwrap_or_default();
    let Ok(mut url) = base.join(action.trim()) else {
        return Ok(());
    };
    let entries = crate::dom::urls::form_entries(cx, form, submitter)?;
    let target = pick(cx, "formtarget", "target");
    if method == "post" {
        let enctype = pick(cx, "formenctype", "enctype")
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let (body, ct) = if enctype == "multipart/form-data" {
            crate::dom::net::multipart(cx, &entries)
        } else {
            (
                crate::dom::urls::serialize_entries(&entries).into_bytes(),
                "application/x-www-form-urlencoded".to_string(),
            )
        };
        if target.is_some_and(|t| t.eq_ignore_ascii_case("_blank")) {
            cx.st().push_popup(url.to_string());
            return Ok(());
        }
        cx.st().navigation = Some(NavRequest {
            url: url.to_string(),
            method: "POST".into(),
            body,
            content_type: Some(ct),
            replace: false,
        });
        return Ok(());
    }
    let query = crate::dom::urls::serialize_entries(&entries);
    url.set_query(Some(&query));
    if target.is_some_and(|t| t.eq_ignore_ascii_case("_blank")) {
        cx.st().push_popup(url.to_string());
    } else {
        cx.st().navigation = Some(NavRequest {
            url: url.to_string(),
            method: "GET".into(),
            body: Vec::new(),
            content_type: None,
            replace: false,
        });
    }
    Ok(())
}

/// Sets a file input's files as a person picking them does: `input`, then `change`.
pub(crate) fn set_files(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    files: Vec<crate::dom::blobs::BlobData>,
) -> R<()> {
    let files: Vec<_> = files.into_iter().map(std::rc::Rc::new).collect();
    cx.st().files.insert(el, files);
    input_event(cx, el, "input", false)?;
    input_event(cx, el, "change", false)?;
    Ok(())
}

/// Resets a form (HTML): fires `reset` (cancelable); then each control returns to its default
/// value, checkedness and selectedness, user validity is cleared, and form-associated custom
/// elements get `formResetCallback`.
pub(crate) fn reset_form(cx: &mut Cx<'_, '_, '_>, form: NodeId) -> R<()> {
    if !fire(cx, Target::Node(form), "reset", true, true)? {
        return Ok(());
    }
    let controls = pageon_dom::forms::listed(&cx.dom(), form);
    for c in controls {
        let (select, face) = {
            let dom = cx.dom();
            (
                dom.get(c).is_some_and(|n| n.is_html("select")),
                pageon_dom::forms::is_form_associated(&dom, c),
            )
        };
        {
            let mut dom = cx.dom_mut();
            let st = dom.control_mut(c);
            st.value = None;
            st.checked = None;
            st.all_selected = false;
            st.user_edited = false;
            st.user_validity = false;
        }
        if select {
            let options: Vec<NodeId> = {
                let dom = cx.dom();
                dom.traverse(c)
                    .filter_map(|e| match e {
                        Edge::Enter(o) if dom.get(o).is_some_and(|n| n.is_html("option")) => {
                            Some(o)
                        }
                        _ => None,
                    })
                    .collect()
            };
            for o in options {
                cx.dom_mut().control_mut(o).selected = None;
            }
        }
        if face {
            crate::dom::custom_elements::form_reset(cx, c)?;
        }
    }
    Ok(())
}

// ----- keyboard -----

/// `code` and legacy `keyCode` of a key.
fn key_info(key: &str) -> (String, u32) {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c {
            'a'..='z' | 'A'..='Z' => (
                format!("Key{}", c.to_ascii_uppercase()),
                c.to_ascii_uppercase() as u32,
            ),
            '0'..='9' => (format!("Digit{c}"), c as u32),
            ' ' => ("Space".into(), 32),
            _ => (String::new(), c as u32),
        };
    }
    let code = match key {
        "Enter" => 13,
        "Tab" => 9,
        "Backspace" => 8,
        "Escape" => 27,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "Delete" => 46,
        "Insert" => 45,
        "Shift" => return ("ShiftLeft".into(), 16),
        "Control" => return ("ControlLeft".into(), 17),
        "Alt" => return ("AltLeft".into(), 18),
        "Meta" => return ("MetaLeft".into(), 91),
        "CapsLock" => 20,
        f if f.starts_with('F') && f[1..].parse::<u32>().is_ok() => {
            return (f.to_string(), 111 + f[1..].parse::<u32>().unwrap_or(1));
        }
        _ => 0,
    };
    (key.to_string(), code)
}

fn key_event(cx: &mut Cx<'_, '_, '_>, target: NodeId, ty: &str, key: &str) -> R<bool> {
    key_event_with(cx, target, ty, key, false)
}

fn key_event_with(
    cx: &mut Cx<'_, '_, '_>,
    target: NodeId,
    ty: &str,
    key: &str,
    repeat: bool,
) -> R<bool> {
    let (code, key_code) = key_info(key);
    let printable = key.chars().count() == 1;
    let char_code = if ty == "keypress" {
        if printable {
            key.chars().next().map_or(0, |c| c as u32)
        } else {
            13
        }
    } else {
        0
    };
    let mods = cx.st().mods.tuple();
    let data = EventData {
        ty: ty.into(),
        bubbles: true,
        cancelable: true,
        composed: true,
        initialized: true,
        trusted: true,
        time: now(cx),
        input: InputData {
            key: (key.to_string(), code),
            mods,
            repeat,
            legacy: (
                char_code,
                if ty == "keypress" {
                    char_code
                } else {
                    key_code
                },
            ),
            ..InputData::default()
        },
        ..EventData::default()
    };
    let e = cx.alloc(Iface::KeyboardEvent, Slot::Event(Box::new(data)));
    dispatch(cx, Target::Node(target), e)
}

/// A text control's kind: `Some(true)` for `<input>`/`<textarea>`, `Some(false)` for
/// `contenteditable`, `None` when not editable.
pub(crate) fn editable(cx: &Cx<'_, '_, '_>, el: NodeId) -> Option<bool> {
    let t = tag(cx, el);
    let disabled = attr(cx, el, "disabled").is_some() || attr(cx, el, "readonly").is_some();
    match t.as_str() {
        "textarea" if !disabled => Some(true),
        "input" if !disabled => matches!(
            input_type(cx, el).as_str(),
            "text"
                | "search"
                | "email"
                | "url"
                | "tel"
                | "password"
                | "number"
                | "date"
                | "time"
                | "datetime-local"
                | "month"
                | "week"
        )
        .then_some(true),
        _ => editable_host(&cx.dom(), el).then_some(false),
    }
}

fn value_of(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<String> {
    match tag(cx, el).as_str() {
        "input" => crate::dom::html_input_element::get_value(cx, Obj::Node(el)),
        "textarea" => crate::dom::html_text_area_element::get_value(cx, Obj::Node(el)),
        _ => Ok(cx.dom().text(el)),
    }
}

fn set_value(cx: &mut Cx<'_, '_, '_>, el: NodeId, v: &str) {
    if editable(cx, el) == Some(false) {
        let kids: Vec<NodeId> = cx.dom().children(el).collect();
        let mut dom = cx.dom_mut();
        for k in kids {
            let _ = dom.remove(k);
        }
        let t = dom.create(NodeData::Text(v.to_string()));
        let _ = dom.append(el, t);
    } else {
        let mut dom = cx.dom_mut();
        let c = dom.control_mut(el);
        c.value = Some(v.to_string());
        c.all_selected = false;
    }
}

/// Takes the "whole value selected" mark of a field (`select()`): whether it was set.
fn take_selection(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> bool {
    let selected = cx.dom().control(el).is_some_and(|c| c.all_selected);
    if selected {
        cx.dom_mut().control_mut(el).all_selected = false;
    }
    selected
}

fn input_event(cx: &mut Cx<'_, '_, '_>, el: NodeId, ty: &str, cancelable: bool) -> R<bool> {
    // A user edit (`tooLong`/`tooShort` apply) and a committed change (`:user-invalid`).
    match ty {
        "input" => cx.dom_mut().control_mut(el).user_edited = true,
        "change" => cx.dom_mut().control_mut(el).user_validity = true,
        _ => {}
    }
    let data = EventData {
        ty: ty.into(),
        bubbles: true,
        cancelable,
        composed: true,
        initialized: true,
        trusted: true,
        time: now(cx),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::Event, Slot::Event(Box::new(data)));
    dispatch(cx, Target::Node(el), e)
}

/// Inserts one character the way a keystroke does (respecting `maxlength`).
fn insert(cx: &mut Cx<'_, '_, '_>, el: NodeId, ch: char) -> R<()> {
    if !input_event(cx, el, "beforeinput", true)? {
        return Ok(());
    }
    // A selected value is replaced by what is typed.
    let mut v = if take_selection(cx, el) {
        String::new()
    } else {
        value_of(cx, el)?
    };
    let max = attr(cx, el, "maxlength").and_then(|m| m.parse::<usize>().ok());
    if max.is_some_and(|m| v.encode_utf16().count() >= m) {
        return Ok(());
    }
    v.push(ch);
    set_value(cx, el, &v);
    input_event(cx, el, "input", false).map(|_| ())
}

/// Replaces an editable element's text with `text`, typed key by key.
pub fn type_text(cx: &mut Cx<'_, '_, '_>, el: NodeId, text: &str) -> R<()> {
    move_focus(cx, Some(el))?;
    let initial = value_of(cx, el)?;
    if !initial.is_empty() {
        set_value(cx, el, "");
        input_event(cx, el, "input", false)?;
    }
    for ch in text.chars() {
        let key = if ch == '\n' {
            "Enter".to_string()
        } else {
            ch.to_string()
        };
        if key_event(cx, el, "keydown", &key)? && key_event(cx, el, "keypress", &key)? {
            if ch == '\n' && tag(cx, el) != "textarea" {
                implicit_submit(cx, el)?;
            } else {
                insert(cx, el, ch)?;
            }
        }
        key_event(cx, el, "keyup", &key)?;
    }
    if value_of(cx, el)? != initial {
        input_event(cx, el, "change", false)?;
    }
    Ok(())
}

/// Fills an editable element as a password manager does: focus, the whole value at once,
/// `input` and `change`, and no key events, so no partial value ever exists in the page.
pub fn fill(cx: &mut Cx<'_, '_, '_>, el: NodeId, value: &str) -> R<()> {
    move_focus(cx, Some(el))?;
    if cx.dom().get(el).is_none() || value_of(cx, el)? == value {
        return Ok(());
    }
    set_value(cx, el, value);
    input_event(cx, el, "input", false)?;
    input_event(cx, el, "change", false).map(|_| ())
}

/// Implicit submission (Enter in a field): the form's default button, else the form.
fn implicit_submit(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<()> {
    let Some(form) = form_of(cx, el) else {
        return Ok(());
    };
    let default = {
        let dom = cx.dom();
        dom.traverse(form).find_map(|e| match e {
            Edge::Enter(n) => {
                let x = dom.get(n)?;
                let submit = (x.is_html("button")
                    && x.attr("type")
                        .is_none_or(|t| t.eq_ignore_ascii_case("submit")))
                    || (x.is_html("input")
                        && x.attr("type").is_some_and(|t| {
                            t.eq_ignore_ascii_case("submit") || t.eq_ignore_ascii_case("image")
                        }));
                submit.then_some(n)
            }
            Edge::Leave(_) => None,
        })
    };
    match default {
        Some(b) if attr(cx, b, "disabled").is_some() => Ok(()),
        Some(b) => crate::dom::event_target::click_with(cx, b, true, center(cx, b)),
        None => submit_form(cx, form, None, true),
    }
}

/// Presses a key on the focused element (or the body), with its default action.
pub fn press(cx: &mut Cx<'_, '_, '_>, key: &str) -> R<()> {
    press_chord(cx, Mods::default(), key)
}

/// The element key events go to: the focused element, else the body.
fn key_target(cx: &Cx<'_, '_, '_>) -> Option<NodeId> {
    cx.st()
        .focused
        .filter(|&f| cx.dom().get(f).is_some())
        .or_else(|| {
            let dom = cx.dom();
            dom.traverse(dom.root()).find_map(|e| match e {
                Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("body")) => Some(n),
                _ => None,
            })
        })
}

/// Presses a chord: each modifier's `keydown` (the modifiers accumulating on the events), the
/// key with its default action, then the modifiers' `keyup` in reverse. With ctrl or meta, a
/// printable key inserts nothing; `a` selects a text control's whole value.
pub fn press_chord(cx: &mut Cx<'_, '_, '_>, mods: Mods, key: &str) -> R<()> {
    let saved = cx.st().mods;
    let r = press_chord_inner(cx, mods, key);
    cx.st().mods = saved;
    r
}

fn press_chord_inner(cx: &mut Cx<'_, '_, '_>, mods: Mods, key: &str) -> R<()> {
    let Some(t) = key_target(cx) else {
        return Ok(());
    };
    let mut held = Mods::default();
    for m in mods.keys() {
        held = held.with(m).unwrap_or(held);
        cx.st().mods = held;
        key_event(cx, t, "keydown", m)?;
    }
    cx.st().mods = mods;
    if !mods.keys().contains(&key) {
        press_key(cx, key)?;
    }
    for m in mods.keys().into_iter().rev() {
        cx.st().mods = held;
        key_event(cx, t, "keyup", m)?;
        held = match m {
            "Control" => Mods {
                ctrl: false,
                ..held
            },
            "Shift" => Mods {
                shift: false,
                ..held
            },
            "Alt" => Mods { alt: false, ..held },
            _ => Mods {
                meta: false,
                ..held
            },
        };
    }
    Ok(())
}

/// Holds a chord down (`keydown`, repeated when `repeat`) or releases it (`keyup`), for a key
/// held over time: the modifiers go down first and up last.
pub fn key_hold(cx: &mut Cx<'_, '_, '_>, mods: Mods, key: &str, down: bool, repeat: bool) -> R<()> {
    let Some(t) = key_target(cx) else {
        return Ok(());
    };
    let saved = cx.st().mods;
    cx.st().mods = mods;
    let r = (|| {
        if down {
            if !repeat {
                for m in mods.keys() {
                    key_event(cx, t, "keydown", m)?;
                }
            }
            if !mods.keys().contains(&key) || repeat {
                key_event_with(cx, t, "keydown", key, repeat)?;
            }
        } else {
            if !mods.keys().contains(&key) {
                key_event(cx, t, "keyup", key)?;
            }
            for m in mods.keys().into_iter().rev() {
                key_event(cx, t, "keyup", m)?;
            }
        }
        Ok(())
    })();
    cx.st().mods = saved;
    r
}

/// Types text into the focused element at its end, as keystrokes (no clearing first);
/// `false` when nothing editable is focused (the keys then go to the body).
pub fn type_focused(cx: &mut Cx<'_, '_, '_>, text: &str) -> R<bool> {
    let focused = cx.st().focused.filter(|&f| cx.dom().get(f).is_some());
    let Some(el) = focused.filter(|&f| editable(cx, f).is_some()) else {
        for ch in text.chars() {
            press(
                cx,
                &if ch == '\n' {
                    "Enter".into()
                } else {
                    ch.to_string()
                },
            )?;
        }
        return Ok(false);
    };
    let initial = value_of(cx, el)?;
    for ch in text.chars() {
        let key = if ch == '\n' {
            "Enter".to_string()
        } else {
            ch.to_string()
        };
        if key_event(cx, el, "keydown", &key)? && key_event(cx, el, "keypress", &key)? {
            if ch == '\n' && tag(cx, el) != "textarea" {
                implicit_submit(cx, el)?;
            } else {
                insert(cx, el, ch)?;
            }
        }
        key_event(cx, el, "keyup", &key)?;
    }
    if cx.dom().get(el).is_some() && value_of(cx, el)? != initial {
        input_event(cx, el, "change", false)?;
    }
    Ok(true)
}

/// One key on the focused element (or the body) with its default action, under the modifiers
/// in the realm's state.
fn press_key(cx: &mut Cx<'_, '_, '_>, key: &str) -> R<()> {
    let target = cx
        .st()
        .focused
        .filter(|&f| cx.dom().get(f).is_some())
        .or_else(|| {
            let dom = cx.dom();
            dom.traverse(dom.root()).find_map(|e| match e {
                Edge::Enter(n) if dom.get(n).is_some_and(|x| x.is_html("body")) => Some(n),
                _ => None,
            })
        });
    let Some(t) = target else { return Ok(()) };
    let mods = cx.st().mods;
    let command = mods.ctrl || mods.meta;
    let printable = key.chars().count() == 1;
    let mut go = key_event(cx, t, "keydown", key)?;
    if go && !command && (printable || key == "Enter") {
        go = key_event(cx, t, "keypress", key)?;
    }
    if go {
        let tg = tag(cx, t);
        match key {
            // Select all in a text control (full selection and the clipboard come later).
            "a" | "A" if command => {
                if editable(cx, t) == Some(true) {
                    cx.dom_mut().control_mut(t).all_selected = true;
                }
            }
            _ if command && printable => {}
            "Tab" if mods.shift => {
                let prev = prev_focusable(cx, t);
                move_focus(cx, prev)?;
            }
            "Enter" if tg == "textarea" => insert(cx, t, '\n')?,
            "Enter" if editable(cx, t) == Some(true) => implicit_submit(cx, t)?,
            "Enter" if matches!(tg.as_str(), "a" | "button" | "summary") => {
                crate::dom::event_target::click_with(cx, t, true, center(cx, t))?;
            }
            " " if tg == "button"
                || (tg == "input"
                    && matches!(
                        input_type(cx, t).as_str(),
                        "checkbox" | "radio" | "submit" | "button"
                    )) =>
            {
                crate::dom::event_target::click_with(cx, t, true, center(cx, t))?;
            }
            "Tab" => {
                let next = next_focusable(cx, t);
                move_focus(cx, next)?;
            }
            "Backspace" | "Delete" if editable(cx, t).is_some() => {
                // The caret is at the end: Backspace deletes the last character, Delete
                // nothing; with the whole value selected, both clear it.
                let mut v = value_of(cx, t)?;
                let changed = if take_selection(cx, t) {
                    v.clear();
                    true
                } else {
                    key == "Backspace" && v.pop().is_some()
                };
                if changed {
                    set_value(cx, t, &v);
                    input_event(cx, t, "input", false)?;
                }
            }
            "Escape" => {
                let top = cx.st().modals.last().copied();
                if let Some(top) = top
                    && fire(cx, Target::Node(top), "cancel", false, true)?
                {
                    crate::dom::html_dialog_element::close(cx, Obj::Node(top), None)?;
                }
            }
            k if printable && editable(cx, t).is_some() => {
                if let Some(c) = k.chars().next() {
                    insert(cx, t, c)?;
                }
            }
            // Scrolling keys, outside fields and selects: the focused element's nearest
            // scroll container, else the viewport.
            k if editable(cx, t).is_none() && tg != "select" => {
                use crate::dom::scroll::{Amount, chain};
                let amount = match k {
                    "PageDown" | " " => Some(Amount::Pages(0.0, 1.0)),
                    "PageUp" => Some(Amount::Pages(0.0, -1.0)),
                    "End" => Some(Amount::Edge(0.0, 1.0)),
                    "Home" => Some(Amount::Edge(0.0, -1.0)),
                    "ArrowDown" => Some(Amount::Pixels(0.0, 40.0)),
                    "ArrowUp" => Some(Amount::Pixels(0.0, -40.0)),
                    "ArrowRight" => Some(Amount::Pixels(40.0, 0.0)),
                    "ArrowLeft" => Some(Amount::Pixels(-40.0, 0.0)),
                    _ => None,
                };
                if let Some(a) = amount {
                    chain(cx, Some(t), a);
                }
            }
            _ => {}
        }
    }
    key_event(cx, t, "keyup", key).map(|_| ())
}

fn prev_focusable(cx: &Cx<'_, '_, '_>, from: NodeId) -> Option<NodeId> {
    let order = focus_order(cx);
    match order.iter().position(|&n| n == from) {
        Some(0) | None => order.last().copied(),
        Some(i) => order.get(i - 1).copied(),
    }
}

fn focus_order(cx: &Cx<'_, '_, '_>) -> Vec<NodeId> {
    let all: Vec<NodeId> = {
        let dom = cx.dom();
        dom.traverse(dom.root())
            .filter_map(|e| match e {
                Edge::Enter(n) => Some(n),
                Edge::Leave(_) => None,
            })
            .collect()
    };
    all.into_iter()
        .filter(|&n| focusable(cx, n) && attr(cx, n, "tabindex").is_none_or(|t| t.trim() != "-1"))
        .collect()
}

fn next_focusable(cx: &Cx<'_, '_, '_>, from: NodeId) -> Option<NodeId> {
    let all: Vec<NodeId> = {
        let dom = cx.dom();
        dom.traverse(dom.root())
            .filter_map(|e| match e {
                Edge::Enter(n) => Some(n),
                Edge::Leave(_) => None,
            })
            .collect()
    };
    let order: Vec<NodeId> = all
        .into_iter()
        .filter(|&n| focusable(cx, n) && attr(cx, n, "tabindex").is_none_or(|t| t.trim() != "-1"))
        .collect();
    let i = order.iter().position(|&n| n == from);
    match i {
        Some(i) => order.get(i + 1).or(order.first()).copied(),
        None => order.first().copied(),
    }
}

/// Selects the options of a `<select>` matching `values` (by value, then by label), then fires
/// `input` and `change`. Returns the options that matched nothing.
pub fn select(cx: &mut Cx<'_, '_, '_>, el: NodeId, values: &[String]) -> R<Vec<String>> {
    let options: Vec<(NodeId, String, String)> = {
        let opts: Vec<NodeId> = {
            let dom = cx.dom();
            dom.traverse(el)
                .filter_map(|e| match e {
                    Edge::Enter(o) if dom.get(o).is_some_and(|x| x.is_html("option")) => Some(o),
                    _ => None,
                })
                .collect()
        };
        let mut out = Vec::new();
        for o in opts {
            let v = crate::dom::html_option_element::get_value(cx, Obj::Node(o))?;
            let label = cx.dom().text(o).trim().to_string();
            out.push((o, v, label));
        }
        out
    };
    let mut chosen = Vec::new();
    let mut missing = Vec::new();
    for want in values {
        match options
            .iter()
            .find(|(_, v, _)| v == want)
            .or_else(|| options.iter().find(|(_, _, l)| l == want.trim()))
        {
            Some((o, _, _)) => chosen.push(*o),
            None => missing.push(want.clone()),
        }
    }
    if !missing.is_empty() {
        return Ok(missing);
    }
    move_focus(cx, Some(el))?;
    let multiple = attr(cx, el, "multiple").is_some();
    if multiple {
        for (o, _, _) in &options {
            crate::dom::html_option_element::set_selected_state(cx, *o, false);
        }
    }
    for o in chosen {
        crate::dom::html_option_element::set_selected_state(cx, o, true);
    }
    input_event(cx, el, "input", false)?;
    input_event(cx, el, "change", false)?;
    Ok(Vec::new())
}

/// The labels of a `<select>`'s options (for `NoSuchOption`).
pub fn option_labels(cx: &Cx<'_, '_, '_>, el: NodeId) -> Vec<String> {
    let dom = cx.dom();
    dom.traverse(el)
        .filter_map(|e| match e {
            Edge::Enter(o) if dom.get(o).is_some_and(|x| x.is_html("option")) => {
                Some(dom.text(o).trim().to_string())
            }
            _ => None,
        })
        .collect()
}

/// Where the pointer's button went down.
#[derive(Debug, Clone)]
pub struct Press {
    /// The element pressed.
    pub node: NodeId,
    /// The point.
    pub point: (f64, f64),
    /// The pointer has moved since.
    pub moved: bool,
}

/// An HTML drag and drop in progress.
#[derive(Debug, Clone)]
pub struct DragSession {
    /// The element dragged (`None`: data from outside the page, a drop of files).
    pub source: Option<NodeId>,
    /// Its `DataTransfer` (slab id).
    pub dt: u32,
    /// The element under the pointer that the drag entered last.
    pub target: Option<NodeId>,
    /// The target's last `dragover` was cancelled: it accepts the drop.
    pub accepted: bool,
    /// The client drives the drag (CDP's drag interception).
    pub intercepted: bool,
}

/// What a drag did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Dragged {
    /// HTML drag and drop ran (else a pointer drag).
    pub html: bool,
    /// A `drop` event fired (the target accepted).
    pub dropped: bool,
    /// The client took the drag over (CDP interception): its data, as (type, value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intercepted: Option<Vec<(String, String)>>,
}

/// The element a drag starts from when the press was on `el`: `draggable=true`, a link or an
/// image (`draggable=false` stops the search).
fn draggable(cx: &Cx<'_, '_, '_>, el: NodeId) -> Option<(NodeId, Vec<crate::dom::dnd::Item>)> {
    use crate::dom::dnd::Item;
    let dom = cx.dom();
    let mut cur = Some(el);
    while let Some(c) = cur {
        let n = dom.get(c)?;
        if n.local_name().is_some() {
            match n.attr("draggable").map(str::to_ascii_lowercase).as_deref() {
                Some("true") => return Some((c, Vec::new())),
                Some("false") => return None,
                _ => {}
            }
            let url = |a: &str| {
                let v = n.attr(a)?;
                let base = url::Url::parse(&cx.realm.st.borrow().url).ok();
                Some(match base {
                    Some(b) => b.join(v).map_or_else(|_| v.to_string(), |u| u.to_string()),
                    None => v.to_string(),
                })
            };
            let link = if n.is_html("a") { url("href") } else { None };
            let img = if n.is_html("img") { url("src") } else { None };
            if let Some(u) = link.or(img) {
                let items = vec![
                    Item::Text {
                        ty: "text/uri-list".into(),
                        data: u.clone(),
                    },
                    Item::Text {
                        ty: "text/plain".into(),
                        data: u,
                    },
                ];
                return Some((c, items));
            }
        }
        cur = dom.parent(c);
    }
    None
}

/// Fires a drag event at `el`; `false` if it was cancelled.
fn drag_event(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    ty: &str,
    point: (f64, f64),
    dt: u32,
) -> R<bool> {
    use crate::dom::dnd::{Mode, set_mode};
    let mode = match ty {
        "dragstart" => Mode::ReadWrite,
        "drop" => Mode::ReadOnly,
        _ => Mode::Protected,
    };
    set_mode(cx, dt, mode);
    let cancelable = !matches!(ty, "dragleave" | "dragend");
    let page_offset = crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport);
    let data = EventData {
        ty: ty.into(),
        bubbles: true,
        cancelable,
        composed: true,
        initialized: true,
        trusted: true,
        time: now(cx),
        input: InputData {
            client: point,
            screen: (point.0 as i32, point.1 as i32),
            page_offset,
            ..InputData::default()
        },
        drag: Some(dt),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::DragEvent, Slot::Event(Box::new(data)));
    let ok = dispatch(cx, Target::Node(el), e);
    set_mode(cx, dt, Mode::Protected);
    ok
}

/// Presses the pointer's button on `el` at `point`: hover, `pointerdown`/`mousedown`, focus.
pub fn pointer_down(cx: &mut Cx<'_, '_, '_>, el: NodeId, point: (f64, f64)) -> R<()> {
    cx.st().drag = None;
    // A press where the pointer already is moves nothing (no extra `mousemove`).
    if cx.st().hovered != Some(el) {
        hover(cx, el, Some(point))?;
    }
    cx.st().pressed = Some(Press {
        node: el,
        point,
        moved: false,
    });
    mouse(cx, el, "pointerdown", point, None)?;
    if mouse(cx, el, "mousedown", point, None)? {
        let mut target = Some(el);
        while let Some(t) = target {
            if focusable(cx, t) {
                break;
            }
            target = cx.dom().parent(t);
        }
        move_focus(cx, target)?;
    }
    Ok(())
}

/// Moves the pointer to `el` at `point`. With the button held from a draggable element, the
/// first move starts HTML drag and drop (`dragstart`); with `intercept`, the drag's data is
/// returned instead of running it (the client drives it). During a drag: `drag`, then
/// `dragenter`/`dragleave`/`dragover` on the element under the pointer.
pub fn pointer_move(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    point: (f64, f64),
    intercept: bool,
) -> R<Option<Vec<(String, String)>>> {
    if cx.st().drag.as_ref().is_some_and(|d| !d.intercepted) {
        drag_over(cx, el, point)?;
        return Ok(None);
    }
    let first = match cx.st().pressed.as_mut() {
        Some(p) if !p.moved => {
            p.moved = true;
            Some((p.node, p.point))
        }
        _ => None,
    };
    if let Some((pressed, at)) = first
        && let Some((source, items)) = draggable(cx, pressed)
    {
        // As in browsers, the move that starts a drag is a move first.
        hover(cx, el, Some(point))?;
        let dt = crate::dom::dnd::new_data_transfer(cx, crate::dom::dnd::DtData::drag(items));
        if drag_event(cx, source, "dragstart", at, dt)? {
            // The drag takes the pointer: no more mouse events for this press.
            mouse(cx, pressed, "pointercancel", at, None)?;
            cx.st().pressed = None;
            cx.st().drag = Some(DragSession {
                source: Some(source),
                dt,
                target: None,
                accepted: false,
                intercepted: intercept,
            });
            if intercept {
                let data = match cx.st().slot(dt) {
                    Some(Slot::DataTransfer(d)) => d.strings(),
                    _ => Vec::new(),
                };
                return Ok(Some(data));
            }
            drag_over(cx, el, point)?;
            return Ok(None);
        }
    }
    hover(cx, el, Some(point))?;
    Ok(None)
}

/// One step of a drag over the page: `drag` at the source, then enter/leave and `dragover` at
/// the element under the pointer; a cancelled `dragover` accepts the drop.
fn drag_over(cx: &mut Cx<'_, '_, '_>, el: NodeId, point: (f64, f64)) -> R<()> {
    let Some(s) = cx.st().drag.clone() else {
        return Ok(());
    };
    if let Some(src) = s.source.filter(|&n| cx.dom().get(n).is_some()) {
        drag_event(cx, src, "drag", point, s.dt)?;
    }
    if s.target != Some(el) {
        if let Some(old) = s.target.filter(|&n| cx.dom().get(n).is_some()) {
            drag_event(cx, old, "dragleave", point, s.dt)?;
        }
        drag_event(cx, el, "dragenter", point, s.dt)?;
    }
    // `dropEffect` starts from what `effectAllowed` allows; a cancelled `dragover` keeps
    // it (or what the page set), an uncancelled one makes it `none`.
    if let Some(Slot::DataTransfer(d)) = cx.st().slot_mut(s.dt) {
        d.drop_effect = match d.effect_allowed.as_str() {
            "none" => "none",
            "link" | "linkMove" => "link",
            "move" => "move",
            _ => "copy",
        }
        .into();
    }
    let accepted = !drag_event(cx, el, "dragover", point, s.dt)?;
    if !accepted && let Some(Slot::DataTransfer(d)) = cx.st().slot_mut(s.dt) {
        d.drop_effect = "none".into();
    }
    if let Some(d) = cx.st().drag.as_mut() {
        d.target = Some(el);
        d.accepted = accepted;
    }
    Ok(())
}

/// Ends a drag at `el`: `drop` there if it accepted (else `dragleave`), then `dragend` at the
/// source.
fn drag_finish(cx: &mut Cx<'_, '_, '_>, el: NodeId, point: (f64, f64), drop: bool) -> R<Dragged> {
    let Some(s) = cx.st().drag.take() else {
        return Ok(Dragged::default());
    };
    let mut dropped = false;
    if drop && s.accepted && s.target == Some(el) {
        drag_event(cx, el, "drop", point, s.dt)?;
        dropped = true;
    } else if let Some(t) = s.target.filter(|&n| cx.dom().get(n).is_some()) {
        drag_event(cx, t, "dragleave", point, s.dt)?;
    }
    if !dropped && let Some(Slot::DataTransfer(d)) = cx.st().slot_mut(s.dt) {
        d.drop_effect = "none".into();
    }
    if let Some(src) = s.source.filter(|&n| cx.dom().get(n).is_some()) {
        drag_event(cx, src, "dragend", point, s.dt)?;
    }
    cx.st().pressed = None;
    Ok(Dragged {
        html: true,
        dropped,
        intercepted: None,
    })
}

/// Releases the button on `el` at `point`: the drop of a drag in progress, else
/// `pointerup`/`mouseup` (and `click` when the pointer did not move).
pub fn pointer_up(cx: &mut Cx<'_, '_, '_>, el: NodeId, point: (f64, f64)) -> R<Dragged> {
    if cx.st().drag.as_ref().is_some_and(|d| !d.intercepted) {
        drag_over(cx, el, point)?;
        return drag_finish(cx, el, point, true);
    }
    let press = cx.st().pressed.take();
    if press.is_none() && cx.st().drag.is_some() {
        // The client drove an intercepted drag and never dropped it.
        cx.st().drag = None;
    }
    mouse(cx, el, "pointerup", point, None)?;
    mouse(cx, el, "mouseup", point, None)?;
    if let Some(p) = press
        && !p.moved
        && p.node == el
    {
        crate::dom::event_target::click_with(cx, el, true, point)?;
    }
    Ok(Dragged::default())
}

/// What a CDP client asks of a drag it drives (`Input.dispatchDragEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DragKind {
    /// The drag enters the page (or moves within it).
    Enter,
    /// The drag moves.
    Over,
    /// The drag is dropped.
    Drop,
    /// The drag is cancelled.
    Cancel,
}

/// A drag the client drives at `el`/`point`: the intercepted one, or one from outside the page
/// with `items`.
pub fn drag_from_client(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    point: (f64, f64),
    kind: DragKind,
    items: Vec<(String, String)>,
) -> R<Dragged> {
    use crate::dom::dnd::{DtData, Item};
    let active = cx.st().drag.is_some();
    if !active {
        if matches!(kind, DragKind::Drop | DragKind::Cancel) {
            return Ok(Dragged::default());
        }
        let items = items
            .into_iter()
            .map(|(ty, data)| Item::Text { ty, data })
            .collect();
        let dt = crate::dom::dnd::new_data_transfer(cx, DtData::drag(items));
        cx.st().drag = Some(DragSession {
            source: None,
            dt,
            target: None,
            accepted: false,
            intercepted: false,
        });
    }
    if let Some(d) = cx.st().drag.as_mut() {
        d.intercepted = false;
    }
    match kind {
        DragKind::Enter | DragKind::Over => {
            drag_over(cx, el, point)?;
            Ok(Dragged {
                html: true,
                ..Dragged::default()
            })
        }
        DragKind::Drop => drag_finish(cx, el, point, true),
        DragKind::Cancel => drag_finish(cx, el, point, false),
    }
}

/// A drag from `from` to `to` (centres): press, four moves, release.
pub fn drag(cx: &mut Cx<'_, '_, '_>, from: NodeId, to: NodeId) -> R<Dragged> {
    let (a, p0) = target_at(cx, from, None);
    pointer_down(cx, a, p0)?;
    let p1 = center(cx, to);
    for i in 1..=4 {
        let t = f64::from(i) / 4.0;
        let p = (p0.0 + (p1.0 - p0.0) * t, p0.1 + (p1.1 - p0.1) * t);
        let hit = cx.realm.layout().element_at(p.0 as f32, p.1 as f32);
        // The last step lands on the target (or what covers it inside it).
        let el = if i == 4 {
            target_at(cx, to, Some(p)).0
        } else {
            hit.unwrap_or(to)
        };
        pointer_move(cx, el, p, false)?;
    }
    let (b, _) = target_at(cx, to, Some(p1));
    pointer_up(cx, b, p1)
}

/// Drops files on `el` (a drop zone): `dragenter`, `dragover`, then `drop` if it accepts.
pub fn drop_files(
    cx: &mut Cx<'_, '_, '_>,
    el: NodeId,
    files: Vec<crate::dom::blobs::BlobData>,
) -> R<Dragged> {
    use crate::dom::dnd::{DtData, Item};
    let (el, p) = target_at(cx, el, None);
    let items = files
        .into_iter()
        .map(|f| Item::File(std::rc::Rc::new(f)))
        .collect();
    let dt = crate::dom::dnd::new_data_transfer(cx, DtData::drag(items));
    cx.st().drag = Some(DragSession {
        source: None,
        dt,
        target: None,
        accepted: false,
        intercepted: false,
    });
    hover(cx, el, Some(p))?;
    drag_over(cx, el, p)?;
    drag_finish(cx, el, p, true)
}

/// Whether an element (or an ancestor) listens for `drop`: a drop zone.
pub fn drop_zone(cx: &Cx<'_, '_, '_>, el: NodeId) -> bool {
    let st = cx.realm.st.borrow();
    let dom = cx.dom();
    let mut cur = Some(el);
    while let Some(c) = cur {
        let t = Target::Node(c);
        let listens = st
            .listeners
            .get(&t)
            .is_some_and(|l| l.iter().any(|x| x.ty == "drop"))
            || st
                .handlers
                .keys()
                .any(|(_, ht, ty)| *ht == t && ty == "drop")
            || dom.get(c).is_some_and(|n| n.attr("ondrop").is_some());
        if listens {
            return true;
        }
        cur = dom.parent(c);
    }
    false
}
