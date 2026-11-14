//! The window, timers, location, navigator, performance, console and DOMException.

use crate::cv::{self, Cx, Error, Obj, R, Value};
use crate::generated::{Iface, ScrollToOptions};
use crate::realm::{Slot, Timer};

fn viewport(cx: &Cx<'_, '_, '_>) -> (i32, i32) {
    let v = cx.st().viewport;
    (
        i32::try_from(v.width).unwrap_or(0),
        i32::try_from(v.height).unwrap_or(0),
    )
}

/// `Window`.
pub mod window {
    use super::*;

    pub fn get_history(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "history",
            Iface::History,
            Slot::History,
        ))
    }

    pub fn match_media(cx: &mut Cx<'_, '_, '_>, _this: Obj, query: String) -> R<Obj> {
        Ok(crate::dom::platform::match_media(cx, query))
    }

    pub fn request_idle_callback(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        cb: Value,
        options: crate::generated::IdleRequestOptions,
    ) -> R<u32> {
        crate::dom::platform::request_idle_callback(cx, cb, options)
    }

    pub fn cancel_idle_callback(cx: &mut Cx<'_, '_, '_>, _this: Obj, id: u32) -> R<()> {
        cx.realm.shared.borrow_mut().cancel(id);
        Ok(())
    }

    /// Window members the IDL describes but the generator does not install (none today).
    pub fn install_extras(_scope: &mut v8::PinScope<'_, '_>, _global: v8::Local<'_, v8::Object>) {}

    fn dialog(cx: &mut Cx<'_, '_, '_>, kind: &'static str, message: String) -> bool {
        let accepted = kind == "alert" || cx.st().accept_dialogs;
        cx.st().dialogs.push(crate::realm::Dialog {
            kind: kind.into(),
            message,
            accepted,
        });
        accepted
    }

    /// `alert()`: never blocks; recorded for the caller.
    pub fn alert(cx: &mut Cx<'_, '_, '_>, _this: Obj, message: Option<String>) -> R<()> {
        dialog(cx, "alert", message.unwrap_or_default());
        Ok(())
    }

    /// `confirm()`: answered by the tab's dialog policy (dismiss by default).
    pub fn confirm(cx: &mut Cx<'_, '_, '_>, _this: Obj, message: String) -> R<bool> {
        Ok(dialog(cx, "confirm", message))
    }

    /// `prompt()`: the default value when accepted, `null` when dismissed.
    pub fn prompt(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        message: String,
        default: String,
    ) -> R<Option<String>> {
        Ok(dialog(cx, "prompt", message).then_some(default))
    }

    /// `window.open()`: the URL is reported to the caller as a popup; there is no script
    /// access to other windows, so it returns `null`.
    pub fn open(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        url: String,
        _target: String,
        _features: String,
    ) -> R<Option<Obj>> {
        if !url.trim().is_empty()
            && let Some(u) = crate::dom::resolve_url(cx, &url)
        {
            cx.st().popups.push(u);
        }
        Ok(None)
    }

    pub fn get_window(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(Obj::Window)
    }

    pub fn get_self_(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(Obj::Window)
    }

    pub fn get_document(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(Obj::Node(cx.dom().root()))
    }

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().window_name.clone())
    }

    pub fn set_name(cx: &mut Cx<'_, '_, '_>, _this: Obj, v: String) -> R<()> {
        cx.st().window_name = v;
        Ok(())
    }

    pub fn get_location(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "location",
            Iface::Location,
            Slot::Location,
        ))
    }

    pub fn get_navigator(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "navigator",
            Iface::Navigator,
            Slot::Navigator,
        ))
    }

    pub fn get_top(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(Some(crate::frames::window::top(cx)))
    }

    pub fn get_parent(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(Some(crate::frames::window::parent(cx)))
    }

    /// `postMessage(message, targetOrigin, transfer)` to this window.
    pub fn post_message(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        message: Value,
        target_origin: String,
        transfer: Vec<Value>,
    ) -> R<()> {
        crate::frames::window::post_message(cx, &message, &target_origin, transfer.len())
    }

    pub fn get_frames(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(Obj::Window)
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        if let Some(children) = crate::frames::window::children(cx) {
            return Ok(u32::try_from(children.len()).unwrap_or(0));
        }
        let dom = cx.dom();
        let n = dom
            .traverse(dom.root())
            .filter(|e| matches!(e, pageon_dom::Edge::Enter(n) if dom.get(*n).is_some_and(|x| x.is_html("iframe"))))
            .count();
        Ok(u32::try_from(n).unwrap_or(0))
    }

    pub fn get_frame_element(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(crate::frames::window::frame_element(cx))
    }

    pub fn focus(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        Ok(())
    }

    pub fn blur(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        Ok(())
    }

    pub fn get_computed_style(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        el: Obj,
        _pseudo: Option<Option<String>>,
    ) -> R<Obj> {
        let id = el.node()?;
        Ok(cx.alloc(Iface::CSSStyleProperties, Slot::Computed(id)))
    }

    pub fn get_inner_width(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<i32> {
        Ok(viewport(cx).0)
    }

    pub fn get_inner_height(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<i32> {
        Ok(viewport(cx).1)
    }

    pub fn get_outer_width(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        get_inner_width(cx, this)
    }

    pub fn get_outer_height(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i32> {
        get_inner_height(cx, this)
    }

    pub fn get_scroll_x(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport).0)
    }

    pub fn get_scroll_y(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport).1)
    }

    pub fn get_page_x_offset(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        get_scroll_x(cx, this)
    }

    pub fn get_page_y_offset(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        get_scroll_y(cx, this)
    }

    fn to(cx: &mut Cx<'_, '_, '_>, x: Option<f64>, y: Option<f64>, by: bool) -> R<Value> {
        use crate::dom::scroll::{Scroller, offset, set};
        let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
        let (ox, oy) = offset(cx, Scroller::Viewport);
        let (bx, by_) = if by { (ox, oy) } else { (0.0, 0.0) };
        set(
            cx,
            Scroller::Viewport,
            x.map_or(ox, |v| bx + finite(v)),
            y.map_or(oy, |v| by_ + finite(v)),
        );
        let r = v8::PromiseResolver::new(cx.scope).ok_or(crate::cv::Error::Terminated)?;
        let undefined = v8::undefined(cx.scope).into();
        r.resolve(cx.scope, undefined);
        let p = r.get_promise(cx.scope);
        Ok(cx.keep(p.into()))
    }

    pub fn scroll(cx: &mut Cx<'_, '_, '_>, _this: Obj, o: ScrollToOptions) -> R<Value> {
        to(cx, o.left, o.top, false)
    }

    pub fn scroll_x_y(cx: &mut Cx<'_, '_, '_>, _this: Obj, x: f64, y: f64) -> R<Value> {
        to(cx, Some(x), Some(y), false)
    }

    pub fn scroll_to(cx: &mut Cx<'_, '_, '_>, _this: Obj, o: ScrollToOptions) -> R<Value> {
        to(cx, o.left, o.top, false)
    }

    pub fn scroll_to_x_y(cx: &mut Cx<'_, '_, '_>, _this: Obj, x: f64, y: f64) -> R<Value> {
        to(cx, Some(x), Some(y), false)
    }

    pub fn scroll_by(cx: &mut Cx<'_, '_, '_>, _this: Obj, o: ScrollToOptions) -> R<Value> {
        to(cx, o.left, o.top, true)
    }

    pub fn scroll_by_x_y(cx: &mut Cx<'_, '_, '_>, _this: Obj, x: f64, y: f64) -> R<Value> {
        to(cx, Some(x), Some(y), true)
    }

    pub fn get_device_pixel_ratio(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(1.0)
    }

    pub fn get_custom_elements(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "customElements",
            Iface::CustomElementRegistry,
            Slot::CustomElements,
        ))
    }

    pub fn get_visual_viewport(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(Some(super::super::window_same(
            cx,
            "visualViewport",
            Iface::VisualViewport,
            Slot::VisualViewport,
        )))
    }
}

/// `VisualViewport`: the layout viewport at its scroll position, at scale 1.
pub mod visual_viewport {
    use super::*;

    pub fn get_offset_left(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(0.0)
    }

    pub fn get_offset_top(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(0.0)
    }

    pub fn get_page_left(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport).0)
    }

    pub fn get_page_top(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(crate::dom::scroll::offset(cx, crate::dom::scroll::Scroller::Viewport).1)
    }

    pub fn get_width(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(f64::from(viewport(cx).0))
    }

    pub fn get_height(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(f64::from(viewport(cx).1))
    }

    pub fn get_scale(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(1.0)
    }
}

/// The `WindowOrWorkerGlobalScope` mixin.
pub mod window_or_worker_global_scope {
    use super::*;

    pub fn get_crypto(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "crypto",
            Iface::Crypto,
            Slot::Crypto,
        ))
    }

    pub fn structured_clone(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        value: Value,
        options: crate::generated::StructuredSerializeOptions,
    ) -> R<Value> {
        crate::dom::session::structured_clone(cx, &value, &options.transfer)
    }

    pub fn fetch(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        input: Value,
        init: crate::generated::RequestInit,
    ) -> R<Value> {
        crate::dom::net::fetch(cx, input, init)
    }

    fn timer(cx: &mut Cx<'_, '_, '_>, cb: Value, ms: i32, args: Vec<Value>, repeat: bool) -> i32 {
        let delay = f64::from(ms.max(0));
        let t = Timer {
            id: 0,
            realm: cx.realm.id,
            cb,
            args,
            interval: repeat.then_some(delay),
            frame: false,
            idle: false,
        };
        let id = cx.realm.shared.borrow_mut().schedule(t, delay);
        i32::try_from(id).unwrap_or(i32::MAX)
    }

    pub fn set_timeout(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        cb: Value,
        ms: i32,
        args: Vec<Value>,
    ) -> R<i32> {
        Ok(timer(cx, cb, ms, args, false))
    }

    pub fn set_interval(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        cb: Value,
        ms: i32,
        args: Vec<Value>,
    ) -> R<i32> {
        Ok(timer(cx, cb, ms, args, true))
    }

    pub fn clear_timeout(cx: &mut Cx<'_, '_, '_>, _this: Obj, id: i32) -> R<()> {
        if let Ok(id) = u32::try_from(id) {
            cx.realm.shared.borrow_mut().cancel(id);
        }
        Ok(())
    }

    pub fn clear_interval(cx: &mut Cx<'_, '_, '_>, this: Obj, id: i32) -> R<()> {
        clear_timeout(cx, this, id)
    }

    pub fn queue_microtask(cx: &mut Cx<'_, '_, '_>, _this: Obj, cb: Value) -> R<()> {
        let f = cx.local(&cb);
        let Ok(f) = v8::Local::<v8::Function>::try_from(f) else {
            return Err(Error::Type(
                "Failed to execute 'queueMicrotask': parameter 1 is not of type 'Function'.".into(),
            ));
        };
        cx.scope.enqueue_microtask(f);
        Ok(())
    }

    pub fn get_origin(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().origin.clone())
    }

    pub fn get_is_secure_context(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        let o = cx.st().origin.clone();
        Ok(o.starts_with("https:") || o.contains("://localhost") || o.contains("://127.0.0.1"))
    }

    pub fn get_performance(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "performance",
            Iface::Performance,
            Slot::Performance,
        ))
    }

    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn btoa(_cx: &mut Cx<'_, '_, '_>, _this: Obj, data: String) -> R<String> {
        let mut bytes = Vec::with_capacity(data.len());
        for c in data.chars() {
            let b = u8::try_from(u32::from(c)).map_err(|_| {
                Error::dom(
                    "InvalidCharacterError",
                    "The string to be encoded contains characters outside of the Latin1 range.",
                )
            })?;
            bytes.push(b);
        }
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        Ok(out)
    }

    pub fn atob(_cx: &mut Cx<'_, '_, '_>, _this: Obj, data: String) -> R<String> {
        let mut s: Vec<u8> = data.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        if s.len().is_multiple_of(4) && s.ends_with(b"==") {
            s.truncate(s.len() - 2);
        } else if s.len().is_multiple_of(4) && s.ends_with(b"=") {
            s.truncate(s.len() - 1);
        }
        let bad = || {
            Error::dom(
                "InvalidCharacterError",
                "The string to be decoded is not correctly encoded.",
            )
        };
        if s.len() % 4 == 1 {
            return Err(bad());
        }
        let mut bits = 0u32;
        let mut n = 0;
        let mut out = String::new();
        for b in s {
            let v = B64.iter().position(|&x| x == b).ok_or_else(bad)?;
            bits = (bits << 6) | u32::try_from(v).unwrap_or(0);
            n += 6;
            if n >= 8 {
                n -= 8;
                out.push(char::from(((bits >> n) & 0xff) as u8));
            }
        }
        Ok(out)
    }
}

/// The `AnimationFrameProvider` mixin: frames every 16 ms of virtual time.
pub mod animation_frame_provider {
    use super::*;

    pub fn request_animation_frame(cx: &mut Cx<'_, '_, '_>, _this: Obj, cb: Value) -> R<u32> {
        let mut sh = cx.realm.shared.borrow_mut();
        let elapsed = sh.now - sh.origin;
        let delay = 16.0 - elapsed.rem_euclid(16.0);
        let t = Timer {
            id: 0,
            realm: cx.realm.id,
            cb,
            args: Vec::new(),
            interval: None,
            frame: true,
            idle: false,
        };
        Ok(sh.schedule(t, delay))
    }

    pub fn cancel_animation_frame(cx: &mut Cx<'_, '_, '_>, _this: Obj, id: u32) -> R<()> {
        cx.realm.shared.borrow_mut().cancel(id);
        Ok(())
    }
}

/// `Location` (navigation from script is not supported yet: setters are reported).
pub mod location {
    use super::*;

    fn part(cx: &Cx<'_, '_, '_>, which: &str) -> String {
        let url = cx.st().url.clone();
        crate::dom::html::url_part(url::Url::parse(&url).ok().as_ref(), which)
    }

    /// Navigates to the current URL changed by a `url::quirks` setter.
    fn set_part(cx: &mut Cx<'_, '_, '_>, f: impl FnOnce(&mut url::Url)) -> R<()> {
        let current = cx.st().url.clone();
        let Ok(mut u) = url::Url::parse(&current) else {
            return Ok(());
        };
        f(&mut u);
        crate::dom::input::navigate(cx, u.as_str(), false);
        Ok(())
    }

    fn resolve(cx: &Cx<'_, '_, '_>, v: &str) -> R<String> {
        crate::dom::resolve_url(cx, v)
            .ok_or_else(|| Error::dom("SyntaxError", format!("'{v}' is not a valid URL.")))
    }

    pub fn get_href(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(part(cx, "href"))
    }

    pub fn set_href(cx: &mut Cx<'_, '_, '_>, _this: Obj, v: String) -> R<()> {
        let u = resolve(cx, &v)?;
        crate::dom::input::navigate(cx, &u, false);
        Ok(())
    }

    pub fn assign(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        set_href(cx, this, v)
    }

    pub fn replace(cx: &mut Cx<'_, '_, '_>, _this: Obj, v: String) -> R<()> {
        let u = resolve(cx, &v)?;
        crate::dom::input::navigate(cx, &u, true);
        Ok(())
    }

    pub fn reload(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        let u = cx.st().url.clone();
        cx.st().navigation = Some(crate::realm::NavRequest {
            url: u,
            method: "GET".into(),
            body: Vec::new(),
            content_type: None,
            replace: true,
        });
        Ok(())
    }

    macro_rules! loc {
        ($get:ident, $set:ident, $name:literal, $quirk:path) => {
            pub fn $get(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
                Ok(part(cx, $name))
            }
            pub fn $set(cx: &mut Cx<'_, '_, '_>, _this: Obj, v: String) -> R<()> {
                set_part(cx, |u| {
                    let _ = $quirk(u, &v);
                })
            }
        };
    }
    loc!(
        get_protocol,
        set_protocol,
        "protocol",
        url::quirks::set_protocol
    );
    loc!(get_host, set_host, "host", url::quirks::set_host);
    loc!(
        get_hostname,
        set_hostname,
        "hostname",
        url::quirks::set_hostname
    );
    loc!(get_port, set_port, "port", url::quirks::set_port);

    pub fn get_pathname(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(part(cx, "pathname"))
    }

    pub fn set_pathname(cx: &mut Cx<'_, '_, '_>, _this: Obj, v: String) -> R<()> {
        set_part(cx, |u| url::quirks::set_pathname(u, &v))
    }

    pub fn get_search(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(part(cx, "search"))
    }

    pub fn set_search(cx: &mut Cx<'_, '_, '_>, _this: Obj, v: String) -> R<()> {
        set_part(cx, |u| url::quirks::set_search(u, &v))
    }

    pub fn get_hash(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(part(cx, "hash"))
    }

    pub fn set_hash(cx: &mut Cx<'_, '_, '_>, _this: Obj, v: String) -> R<()> {
        set_part(cx, |u| url::quirks::set_hash(u, &v))
    }

    pub fn get_origin(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(part(cx, "origin"))
    }
}

/// `NavigatorID`.
pub mod navigator_id {
    use super::*;

    pub fn get_app_code_name(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("Mozilla".into())
    }

    pub fn get_app_name(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("Netscape".into())
    }

    pub fn get_app_version(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        let ua = cx.st().navigator.user_agent.clone();
        Ok(ua.strip_prefix("Mozilla/").unwrap_or(&ua).to_string())
    }

    pub fn get_platform(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().navigator.platform.to_string())
    }

    pub fn get_product(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("Gecko".into())
    }

    pub fn get_product_sub(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("20030107".into())
    }

    pub fn get_user_agent(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().navigator.user_agent.clone())
    }

    pub fn get_vendor(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().navigator.vendor.to_string())
    }

    pub fn get_vendor_sub(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(String::new())
    }
}

/// `NavigatorAutomationInformation` (WebDriver): true for pageon's honest agent profile, the
/// automation-controlled user agent it is.
pub mod navigator_automation_information {
    use super::*;

    pub fn get_webdriver(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(cx.st().navigator.webdriver)
    }
}

/// `NavigatorUA` (UA Client Hints). Exposed only in secure contexts of the browser-compatible
/// profile; the realm removes it elsewhere.
pub mod navigator_ua {
    use super::*;

    pub fn get_user_agent_data(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(super::super::window_same(
            cx,
            "userAgentData",
            Iface::NavigatorUAData,
            Slot::UaData,
        ))
    }
}

/// `NavigatorUAData`: the profile's brands and platform, the same values the client hints
/// headers carry (one pinned definition).
pub mod navigator_ua_data {
    use super::*;

    fn data(cx: &mut Cx<'_, '_, '_>) -> R<pageon_env::UaData> {
        cx.st()
            .navigator
            .ua_data
            .clone()
            .ok_or_else(|| Error::Type("userAgentData is not available".into()))
    }

    fn brands_json(brands: &[(String, String)]) -> serde_json::Value {
        brands
            .iter()
            .map(|(b, v)| serde_json::json!({"brand": b, "version": v}))
            .collect()
    }

    fn js(cx: &mut Cx<'_, '_, '_>, v: &serde_json::Value) -> R<Value> {
        let text = cx.str(&v.to_string());
        let scope = &mut *cx.scope;
        v8::tc_scope!(let tc, scope);
        v8::json::parse(tc, text)
            .map(|v| v8::Global::new(tc, v))
            .ok_or(Error::Terminated)
    }

    fn low_entropy(d: &pageon_env::UaData) -> serde_json::Value {
        serde_json::json!({
            "brands": brands_json(&d.brands),
            "mobile": d.mobile,
            "platform": d.platform,
        })
    }

    pub fn get_brands(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Vec<Value>> {
        let d = data(cx)?;
        let mut out = Vec::new();
        for b in brands_json(&d.brands).as_array().into_iter().flatten() {
            out.push(js(cx, b)?);
        }
        Ok(out)
    }

    pub fn get_mobile(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(data(cx)?.mobile)
    }

    pub fn get_platform(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(data(cx)?.platform.to_string())
    }

    pub fn get_high_entropy_values(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        hints: Vec<String>,
    ) -> R<Value> {
        let d = data(cx)?;
        let mut v = low_entropy(&d);
        for h in &hints {
            let value = match h.as_str() {
                "architecture" => serde_json::json!(d.architecture),
                "bitness" => serde_json::json!(d.bitness),
                "formFactors" => serde_json::json!(d.form_factors),
                "fullVersionList" => brands_json(&d.full_version_list),
                "model" => serde_json::json!(d.model),
                "platformVersion" => serde_json::json!(d.platform_version),
                "uaFullVersion" => serde_json::json!(d.ua_full_version),
                "wow64" => serde_json::json!(false),
                _ => continue,
            };
            v[h.as_str()] = value;
        }
        let value = js(cx, &v)?;
        let r = v8::PromiseResolver::new(cx.scope).ok_or(Error::Terminated)?;
        let local = cx.local(&value);
        r.resolve(cx.scope, local);
        let p = r.get_promise(cx.scope);
        Ok(cx.keep(p.into()))
    }

    pub fn to_json(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Value> {
        let d = data(cx)?;
        js(cx, &low_entropy(&d))
    }
}

/// `NavigatorLanguage`.
pub mod navigator_language {
    use super::*;

    pub fn get_language(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("en-US".into())
    }

    pub fn get_languages(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Vec<String>> {
        Ok(vec!["en-US".into(), "en".into()])
    }
}

/// `NavigatorOnLine`.
pub mod navigator_on_line {
    use super::*;

    pub fn get_on_line(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(true)
    }
}

/// `NavigatorCookies`.
pub mod navigator_cookies {
    use super::*;

    pub fn get_cookie_enabled(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(true)
    }
}

/// `Performance`.
pub mod performance {
    use super::*;

    pub fn now(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        let sh = cx.realm.shared.borrow();
        Ok(sh.now - sh.origin)
    }

    pub fn get_time_origin(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<f64> {
        Ok(cx.realm.shared.borrow().origin)
    }
}

/// The `console` namespace: messages are kept with the page (capped).
pub mod console {
    use super::*;

    fn record(cx: &mut Cx<'_, '_, '_>, level: &str, items: Vec<Value>) -> R<()> {
        let mut parts = Vec::new();
        for v in items {
            let l = cx.local(&v);
            parts.push(cv::string(cx, l).unwrap_or_else(|_| "<value>".into()));
        }
        let mut sh = cx.realm.shared.borrow_mut();
        if sh.console.len() < 1000 {
            sh.console.push((level.to_string(), parts.join(" ")));
        }
        Ok(())
    }

    pub fn log(cx: &mut Cx<'_, '_, '_>, _this: Obj, items: Vec<Value>) -> R<()> {
        record(cx, "log", items)
    }

    pub fn info(cx: &mut Cx<'_, '_, '_>, _this: Obj, items: Vec<Value>) -> R<()> {
        record(cx, "info", items)
    }

    pub fn warn(cx: &mut Cx<'_, '_, '_>, _this: Obj, items: Vec<Value>) -> R<()> {
        record(cx, "warn", items)
    }

    pub fn error(cx: &mut Cx<'_, '_, '_>, _this: Obj, items: Vec<Value>) -> R<()> {
        record(cx, "error", items)
    }

    pub fn debug(cx: &mut Cx<'_, '_, '_>, _this: Obj, items: Vec<Value>) -> R<()> {
        record(cx, "debug", items)
    }

    pub fn trace(cx: &mut Cx<'_, '_, '_>, _this: Obj, items: Vec<Value>) -> R<()> {
        record(cx, "trace", items)
    }

    pub fn dir(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        item: Option<Value>,
        _options: Option<Option<Value>>,
    ) -> R<()> {
        record(cx, "dir", item.into_iter().collect())
    }

    pub fn assert(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        condition: bool,
        items: Vec<Value>,
    ) -> R<()> {
        if condition {
            Ok(())
        } else {
            record(cx, "assert", items)
        }
    }

    pub fn group(cx: &mut Cx<'_, '_, '_>, _this: Obj, items: Vec<Value>) -> R<()> {
        record(cx, "group", items)
    }

    pub fn group_collapsed(cx: &mut Cx<'_, '_, '_>, _this: Obj, items: Vec<Value>) -> R<()> {
        record(cx, "group", items)
    }

    pub fn group_end(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        Ok(())
    }

    pub fn table(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        data: Option<Value>,
        _columns: Option<Vec<String>>,
    ) -> R<()> {
        record(cx, "table", data.into_iter().collect())
    }

    pub fn count(_cx: &mut Cx<'_, '_, '_>, _this: Obj, _label: String) -> R<()> {
        Ok(())
    }

    pub fn time(_cx: &mut Cx<'_, '_, '_>, _this: Obj, _label: String) -> R<()> {
        Ok(())
    }

    pub fn time_end(_cx: &mut Cx<'_, '_, '_>, _this: Obj, _label: String) -> R<()> {
        Ok(())
    }
}

/// `DOMException`.
pub mod dom_exception {
    use super::*;

    const CODES: &[(&str, u16)] = &[
        ("IndexSizeError", 1),
        ("HierarchyRequestError", 3),
        ("WrongDocumentError", 4),
        ("InvalidCharacterError", 5),
        ("NoModificationAllowedError", 7),
        ("NotFoundError", 8),
        ("NotSupportedError", 9),
        ("InvalidStateError", 11),
        ("SyntaxError", 12),
        ("InvalidModificationError", 13),
        ("NamespaceError", 14),
        ("InvalidAccessError", 15),
        ("TypeMismatchError", 17),
        ("SecurityError", 18),
        ("NetworkError", 19),
        ("AbortError", 20),
        ("URLMismatchError", 21),
        ("QuotaExceededError", 22),
        ("TimeoutError", 23),
        ("InvalidNodeTypeError", 24),
        ("DataCloneError", 25),
    ];

    fn parts(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(String, String)> {
        match cx.realm.st.borrow().slot(this.slab()?) {
            Some(Slot::Exception { name, message }) => Ok((name.clone(), message.clone())),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, message: String, name: String) -> R<Obj> {
        Ok(cx.alloc(Iface::DOMException, Slot::Exception { name, message }))
    }

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(parts(cx, this)?.0)
    }

    pub fn get_message(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(parts(cx, this)?.1)
    }

    pub fn get_code(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        let name = parts(cx, this)?.0;
        Ok(CODES
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(0, |(_, c)| *c))
    }
}
