//! Globals sites read at startup: `screen`, the `CSS` namespace, `DOMParser` and
//! `createHTMLDocument` (inert documents), `MessageChannel`, `InputEvent`.

use pageon_dom::{NodeData, NodeId, ParseOptions};

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{Iface, InputEventInit};
use crate::realm::{EventData, InputData, Slot, Target};

// ----- screen -----

pub mod screen {
    use super::*;

    fn size(cx: &Cx<'_, '_, '_>) -> (i32, i32) {
        let v = cx.st().viewport;
        (
            i32::try_from(v.width).unwrap_or(i32::MAX),
            i32::try_from(v.height).unwrap_or(i32::MAX),
        )
    }

    pub fn get_width(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<i32> {
        Ok(size(cx).0)
    }

    pub fn get_height(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<i32> {
        Ok(size(cx).1)
    }

    pub fn get_avail_width(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<i32> {
        Ok(size(cx).0)
    }

    pub fn get_avail_height(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<i32> {
        Ok(size(cx).1)
    }

    pub fn get_color_depth(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        Ok(24)
    }

    pub fn get_pixel_depth(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        Ok(24)
    }

    pub fn get_orientation(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(crate::dom::platform_apis::orientation(cx))
    }
}

// ----- CSS -----

pub mod css {
    use super::*;

    /// CSSOM's serialize-an-identifier.
    pub fn escape(_cx: &mut Cx<'_, '_, '_>, _this: Obj, ident: String) -> R<String> {
        let chars: Vec<char> = ident.chars().collect();
        let mut out = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let first_dash = i == 1 && chars[0] == '-';
            match c {
                '\0' => out.push('\u{FFFD}'),
                '\u{1}'..='\u{1f}' | '\u{7f}' => out.push_str(&format!("\\{:x} ", c as u32)),
                '0'..='9' if i == 0 || first_dash => out.push_str(&format!("\\{:x} ", c as u32)),
                '-' if i == 0 && chars.len() == 1 => out.push_str("\\-"),
                c if c as u32 >= 0x80 || c == '-' || c == '_' || c.is_ascii_alphanumeric() => {
                    out.push(c);
                }
                c => {
                    out.push('\\');
                    out.push(c);
                }
            }
        }
        Ok(out)
    }

    /// `CSS.supports(property, value)` or `CSS.supports(conditionText)`, as `@supports`
    /// evaluates them.
    pub fn supports(_cx: &mut Cx<'_, '_, '_>, _this: Obj, a: String, b: Option<String>) -> R<bool> {
        Ok(match b {
            Some(value) => !a.is_empty() && pageon_layout::css_supports(&format!("({a}: {value})")),
            None => pageon_layout::css_supports(&a),
        })
    }
}

// ----- DOMParser, createHTMLDocument -----

/// A new document in the realm's arena, never connected to the realm's own: no browsing
/// context, so nothing in it runs, loads or lays out. Its scripts are marked already started.
pub(crate) fn inert_document(cx: &Cx<'_, '_, '_>, html: &str) -> NodeId {
    let parsed = pageon_dom::parse_document(html, ParseOptions { scripting: false });
    let mut dom = cx.dom_mut();
    let doc = dom.create(NodeData::Document);
    let mut kids = Vec::new();
    for c in parsed.children(parsed.root()).collect::<Vec<_>>() {
        if let Some(n) = crate::dom::import(&mut dom, &parsed, c) {
            let _ = dom.append(doc, n);
            kids.push(n);
        }
    }
    drop(dom);
    crate::dom::mark_scripts_started(cx, &kids);
    doc
}

pub mod dom_parser {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        Ok(cx.alloc(Iface::DOMParser, Slot::Serializer))
    }

    /// `parseFromString(string, 'text/html')`: an inert document. pageon parses no XML yet.
    pub fn parse_from_string(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        text: String,
        ty: String,
    ) -> R<Obj> {
        match ty.as_str() {
            "text/html" => Ok(Obj::Node(inert_document(cx, &text))),
            "text/xml" | "application/xml" | "application/xhtml+xml" | "image/svg+xml" => {
                Err(Error::dom(
                    "NotSupportedError",
                    format!(
                        "Failed to execute 'parseFromString' on 'DOMParser': pageon does not parse {ty} yet; only text/html."
                    ),
                ))
            }
            _ => Err(Error::Type(format!(
                "Failed to execute 'parseFromString' on 'DOMParser': The provided value '{ty}' is not a valid enum value of type DOMParserSupportedType."
            ))),
        }
    }
}

// ----- MessageChannel -----

/// One end of a `MessageChannel`.
#[derive(Debug, Default, Clone)]
pub struct Port {
    /// The other end (slab id).
    pub peer: Option<u32>,
    /// Messages wait until the port is started (`start()` or setting `onmessage`).
    pub started: bool,
    /// Closed ends neither send nor receive.
    pub closed: bool,
    /// Messages that arrived before the port started.
    pub waiting: Vec<Value>,
}

fn port<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&mut Port) -> T) -> R<T> {
    match cx.st().slot_mut(id) {
        Some(Slot::Port(p)) => Ok(f(p)),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// Queues `data` for the port as a task (delivered by the engine in posting order).
fn queue(cx: &Cx<'_, '_, '_>, to: u32, data: Value) {
    let realm = cx.realm.id;
    cx.realm
        .shared
        .borrow_mut()
        .port_mail
        .push_back((realm, to, data));
}

/// Starts a port: what waited is delivered.
fn start_port(cx: &Cx<'_, '_, '_>, id: u32) -> R<()> {
    let waiting = port(cx, id, |p| {
        p.started = true;
        std::mem::take(&mut p.waiting)
    })?;
    for d in waiting {
        queue(cx, id, d);
    }
    Ok(())
}

/// Delivers one message to a port: a `message` event, unless the port closed meanwhile.
pub(crate) fn deliver(cx: &mut Cx<'_, '_, '_>, to: u32, data: Value) -> R<()> {
    if port(cx, to, |p| p.closed).unwrap_or(true) {
        return Ok(());
    }
    let time = {
        let sh = cx.realm.shared.borrow();
        sh.now - sh.origin
    };
    let ev = EventData {
        ty: "message".into(),
        initialized: true,
        trusted: true,
        time,
        detail: Some(data),
        message: Some(Box::new((String::new(), String::new(), None))),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::MessageEvent, Slot::Event(Box::new(ev)));
    crate::dom::event_target::dispatch(cx, Target::Object(to), e).map(|_| ())
}

pub mod message_channel {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        let a = cx
            .st()
            .alloc(Iface::MessagePort, Slot::Port(Box::default()));
        let b = cx
            .st()
            .alloc(Iface::MessagePort, Slot::Port(Box::default()));
        port(cx, a, |p| p.peer = Some(b))?;
        port(cx, b, |p| p.peer = Some(a))?;
        Ok(cx.alloc(Iface::MessageChannel, Slot::Channel(a, b)))
    }

    fn ends(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(u32, u32)> {
        match cx.st().slot(this.slab()?) {
            Some(Slot::Channel(a, b)) => Ok((*a, *b)),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_port1(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Slab(ends(cx, this)?.0))
    }

    pub fn get_port2(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Slab(ends(cx, this)?.1))
    }
}

pub mod message_port {
    use super::*;

    /// `postMessage(message, transfer)`: a structured clone queued for the other end. Ports
    /// stay in their document: transferring one (or posting one inside the message) is a
    /// `DataCloneError`.
    pub fn post_message(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        message: Value,
        transfer: Vec<Value>,
    ) -> R<()> {
        if !transfer.is_empty() {
            return Err(Error::dom(
                "DataCloneError",
                "Failed to execute 'postMessage' on 'MessagePort': pageon does not transfer objects between ports.",
            ));
        }
        let id = this.slab()?;
        let (peer, closed) = port(cx, id, |p| (p.peer, p.closed))?;
        let data = crate::dom::session::structured_clone(cx, &message, &[])?;
        let Some(peer) = peer.filter(|_| !closed) else {
            return Ok(());
        };
        let (started, peer_closed) = port(cx, peer, |p| (p.started, p.closed))?;
        if peer_closed {
            return Ok(());
        }
        if started {
            queue(cx, peer, data);
        } else {
            port(cx, peer, |p| p.waiting.push(data))?;
        }
        Ok(())
    }

    /// `postMessage(message, transfer)` or `postMessage(message, {transfer})`: both overloads.
    pub(crate) fn post_message_any(
        scope: &mut v8::PinScope,
        args: v8::FunctionCallbackArguments,
        mut rv: v8::ReturnValue,
    ) {
        crate::cv::call(scope, &args, &mut rv, |cx, args| {
            let this = cx.this(args, Iface::MessagePort)?;
            if args.length() == 0 {
                return Err(Error::args("MessagePort.postMessage", 0));
            }
            let message = crate::cv::value(cx, args.get(0));
            let second = args.get(1);
            let list = if second.is_array() {
                second
            } else if let Ok(o) = v8::Local::<v8::Object>::try_from(second) {
                let key = cx.str("transfer");
                o.get(cx.scope, key.into())
                    .unwrap_or_else(|| v8::undefined(cx.scope).into())
            } else {
                v8::undefined(cx.scope).into()
            };
            let transfer = if list.is_null_or_undefined() {
                Vec::new()
            } else {
                crate::cv::seq(cx, list, |cx, v| Ok(crate::cv::value(cx, v)))?
            };
            post_message(cx, this, message, transfer)?;
            cx.ret(())
        });
    }

    pub fn start(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        start_port(cx, this.slab()?)
    }

    pub fn close(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = this.slab()?;
        let peer = port(cx, id, |p| {
            p.closed = true;
            p.peer.take()
        })?;
        if let Some(peer) = peer {
            let _ = port(cx, peer, |p| p.peer = None);
        }
        Ok(())
    }
}

pub mod message_event_target {
    use super::*;

    pub fn get_onmessage(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Value>> {
        crate::cv::handler_get(cx, this, "message")
    }

    /// Setting `onmessage` starts the port, as in browsers.
    pub fn set_onmessage(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<Value>) -> R<()> {
        crate::cv::handler_set(cx, this, "message", v)?;
        if let Ok(id) = this.slab()
            && matches!(cx.st().slot(id), Some(Slot::Port(_)))
        {
            start_port(cx, id)?;
        }
        Ok(())
    }

    pub fn get_onmessageerror(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Value>> {
        crate::cv::handler_get(cx, this, "messageerror")
    }

    pub fn set_onmessageerror(cx: &mut Cx<'_, '_, '_>, this: Obj, v: Option<Value>) -> R<()> {
        crate::cv::handler_set(cx, this, "messageerror", v)
    }
}

// ----- InputEvent -----

pub mod input_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: InputEventInit) -> R<Obj> {
        let time = {
            let sh = cx.realm.shared.borrow();
            sh.now - sh.origin
        };
        let data = EventData {
            ty,
            bubbles: init.bubbles,
            cancelable: init.cancelable,
            composed: init.composed,
            initialized: true,
            time,
            ui_detail: init.detail,
            input: InputData::default(),
            input_kind: Some(Box::new((init.input_type, init.data, init.is_composing))),
            ..EventData::default()
        };
        Ok(cx.alloc(Iface::InputEvent, Slot::Event(Box::new(data))))
    }

    fn kind(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(String, Option<String>, bool)> {
        match cx.st().slot(this.slab()?) {
            Some(Slot::Event(e)) => Ok(e.input_kind.as_deref().cloned().unwrap_or_default()),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_input_type(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(kind(cx, this)?.0)
    }

    pub fn get_data(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<String>> {
        Ok(kind(cx, this)?.1)
    }

    pub fn get_is_composing(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(kind(cx, this)?.2)
    }

    pub fn get_data_transfer(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        crate::dom::editing::event_transfer(cx, this)
    }

    /// `getTargetRanges()`: what a `beforeinput` would change, as static ranges.
    pub fn get_target_ranges(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<Obj>> {
        let Obj::Slab(id) = this else {
            return Err(Error::Type("Illegal invocation".into()));
        };
        let ranges = match cx.st().slot(id) {
            Some(Slot::Event(e)) if e.dispatching && e.ty == "beforeinput" => e.ranges.clone(),
            Some(Slot::Event(_)) => Vec::new(),
            _ => return Err(Error::Type("Illegal invocation".into())),
        };
        Ok(ranges
            .into_iter()
            .map(|(s, e)| crate::dom::ranges::new_static(cx, s, e))
            .collect())
    }
}
