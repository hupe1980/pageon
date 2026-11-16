//! What pages read about the platform: permissions and notifications (pageon grants nothing),
//! user activation, the screen's orientation and position, hardware concurrency, and the
//! PDF viewer lists the HTML standard prescribes (pageon reads PDFs).

use crate::cv::{self, Cx, Error, Obj, Out, R, Value};
use crate::generated::Iface;
use crate::realm::Slot;

/// The PDF viewer plugins, in the standard's order.
const PLUGINS: [&str; 5] = [
    "PDF Viewer",
    "Chrome PDF Viewer",
    "Chromium PDF Viewer",
    "Microsoft Edge PDF Viewer",
    "WebKit built-in PDF",
];
const MIME_TYPES: [&str; 2] = ["application/pdf", "text/pdf"];
/// Cores reported to pages: fixed, never the host's.
pub(crate) const HARDWARE_CONCURRENCY: u64 = 8;

fn singleton(cx: &mut Cx<'_, '_, '_>, key: &'static str, iface: Iface) -> Obj {
    super::window_same(cx, key, iface, Slot::Platform)
}

fn resolved(cx: &mut Cx<'_, '_, '_>, out: Out) -> R<Value> {
    let v = cx.js_value(out);
    super::net::settled(cx, Ok(v))
}

/// What a slab object of this module holds.
enum Held {
    Plugin(u8),
    MimeType(u8, u8),
    Permission(String),
}

fn slot(cx: &Cx<'_, '_, '_>, this: Obj) -> Option<Held> {
    let Obj::Slab(id) = this else { return None };
    match cx.st().slot(id)? {
        Slot::Plugin(i) => Some(Held::Plugin(*i)),
        Slot::MimeType(i, of) => Some(Held::MimeType(*i, *of)),
        Slot::Permission(n) => Some(Held::Permission(n.clone())),
        _ => None,
    }
}

pub mod navigator_plugins {
    use super::*;

    pub fn get_plugins(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(singleton(cx, "plugins", Iface::PluginArray))
    }

    pub fn get_mime_types(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(singleton(cx, "mimeTypes", Iface::MimeTypeArray))
    }

    pub fn java_enabled(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(false)
    }

    pub fn get_pdf_viewer_enabled(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(true)
    }
}

fn plugin(cx: &mut Cx<'_, '_, '_>, i: usize) -> Option<Obj> {
    let i = u8::try_from(i)
        .ok()
        .filter(|&i| usize::from(i) < PLUGINS.len())?;
    Some(Obj::Slab(cx.st().alloc(Iface::Plugin, Slot::Plugin(i))))
}

fn mime_type(cx: &mut Cx<'_, '_, '_>, i: usize, of: u8) -> Option<Obj> {
    let i = u8::try_from(i)
        .ok()
        .filter(|&i| usize::from(i) < MIME_TYPES.len())?;
    Some(Obj::Slab(
        cx.st().alloc(Iface::MimeType, Slot::MimeType(i, of)),
    ))
}

pub mod plugin_array {
    use super::*;

    pub fn refresh(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        Ok(())
    }

    pub fn get_length(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        Ok(PLUGINS.len() as u32)
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, _this: Obj, index: u32) -> R<Option<Obj>> {
        Ok(plugin(cx, index as usize))
    }

    pub fn named_item(cx: &mut Cx<'_, '_, '_>, _this: Obj, name: String) -> R<Option<Obj>> {
        Ok(PLUGINS
            .iter()
            .position(|p| *p == name)
            .and_then(|i| plugin(cx, i)))
    }
}

pub mod mime_type_array {
    use super::*;

    pub fn get_length(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        Ok(MIME_TYPES.len() as u32)
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, _this: Obj, index: u32) -> R<Option<Obj>> {
        Ok(mime_type(cx, index as usize, 0))
    }

    pub fn named_item(cx: &mut Cx<'_, '_, '_>, _this: Obj, name: String) -> R<Option<Obj>> {
        Ok(MIME_TYPES
            .iter()
            .position(|m| *m == name)
            .and_then(|i| mime_type(cx, i, 0)))
    }
}

pub mod plugin {
    use super::*;

    fn index(cx: &Cx<'_, '_, '_>, this: Obj) -> u8 {
        match slot(cx, this) {
            Some(Held::Plugin(i)) => i,
            _ => 0,
        }
    }

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(PLUGINS[usize::from(index(cx, this))].into())
    }

    pub fn get_description(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("Portable Document Format".into())
    }

    pub fn get_filename(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("internal-pdf-viewer".into())
    }

    pub fn get_length(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        Ok(MIME_TYPES.len() as u32)
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, index_: u32) -> R<Option<Obj>> {
        let of = index(cx, this);
        Ok(mime_type(cx, index_ as usize, of))
    }

    pub fn named_item(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Option<Obj>> {
        let of = index(cx, this);
        Ok(MIME_TYPES
            .iter()
            .position(|m| *m == name)
            .and_then(|i| mime_type(cx, i, of)))
    }
}

pub mod mime_type {
    use super::*;

    fn index(cx: &Cx<'_, '_, '_>, this: Obj) -> (u8, u8) {
        match slot(cx, this) {
            Some(Held::MimeType(i, of)) => (i, of),
            _ => (0, 0),
        }
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(MIME_TYPES[usize::from(index(cx, this).0)].into())
    }

    pub fn get_description(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("Portable Document Format".into())
    }

    pub fn get_suffixes(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("pdf".into())
    }

    pub fn get_enabled_plugin(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let of = index(cx, this).1;
        plugin(cx, usize::from(of)).ok_or_else(|| Error::Type("no plugin".into()))
    }
}

pub mod navigator_concurrent_hardware {
    use super::*;

    pub fn get_hardware_concurrency(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u64> {
        Ok(HARDWARE_CONCURRENCY)
    }
}

/// The permissions pageon knows: `prompt`, except `clipboard-write` (granted to a focused
/// page, as browsers do) and notifications once refused.
fn state(cx: &Cx<'_, '_, '_>, name: &str) -> &'static str {
    match name {
        "clipboard-write" => "granted",
        "notifications" | "push" if cx.st().notifications_denied => "denied",
        _ => "prompt",
    }
}

const KNOWN: [&str; 9] = [
    "geolocation",
    "notifications",
    "push",
    "midi",
    "camera",
    "microphone",
    "clipboard-read",
    "clipboard-write",
    "persistent-storage",
];

pub mod permissions {
    use super::*;

    pub fn query(cx: &mut Cx<'_, '_, '_>, _this: Obj, descriptor: Value) -> R<Value> {
        let name = (|| {
            let d = cx.local(&descriptor);
            let o = cv::object(cx, d, "PermissionDescriptor")?;
            let n = cv::get(cx, o, "name")?;
            if n.is_undefined() {
                return Err(Error::Type(
                    "Failed to read the 'name' property from 'PermissionDescriptor': Required member is undefined.".into(),
                ));
            }
            let n = cv::string(cx, n)?;
            if !KNOWN.contains(&n.as_str()) {
                return Err(Error::Type(format!(
                    "Failed to read the 'name' property from 'PermissionDescriptor': The provided value '{n}' is not a valid enum value of type PermissionName."
                )));
            }
            Ok(n)
        })();
        match name {
            Ok(n) => {
                let status = cx.st().alloc(Iface::PermissionStatus, Slot::Permission(n));
                resolved(cx, Out::Obj(Obj::Slab(status)))
            }
            Err(Error::Terminated) => Err(Error::Terminated),
            Err(e) => super::super::net::settled(cx, Err(e)),
        }
    }
}

pub mod permission_status {
    use super::*;

    fn name(cx: &Cx<'_, '_, '_>, this: Obj) -> String {
        match slot(cx, this) {
            Some(Held::Permission(n)) => n,
            _ => String::new(),
        }
    }

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(name(cx, this))
    }

    pub fn get_state(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let n = name(cx, this);
        Ok(state(cx, &n).into())
    }
}

pub mod notification {
    use super::*;

    /// `Notification.permission` (a static attribute, installed by the hooks).
    pub fn permission(cx: &Cx<'_, '_, '_>) -> &'static str {
        if cx.st().notifications_denied {
            "denied"
        } else {
            "default"
        }
    }

    /// `Notification.requestPermission`: always refused; the callback, if any, gets the
    /// answer too.
    pub fn request_permission(cx: &mut Cx<'_, '_, '_>, callback: Option<Value>) -> R<Value> {
        cx.st().notifications_denied = true;
        if let Some(cb) = callback {
            let f = cx.local(&cb);
            if let Ok(f) = v8::Local::<v8::Function>::try_from(f) {
                let arg = cx.str("denied").into();
                let recv = v8::undefined(&*cx.scope).into();
                let _ = f.call(&*cx.scope, recv, &[arg]);
            }
        }
        resolved(cx, Out::Str("denied".into()))
    }
}

pub mod user_activation {
    use super::*;

    pub fn get_has_been_active(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(cx.st().history.activated_at.is_some())
    }

    pub fn get_is_active(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(super::super::editing::activated(cx))
    }
}

pub mod screen_orientation {
    use super::*;

    pub fn get_type_(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("landscape-primary".into())
    }

    pub fn get_angle(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u16> {
        Ok(0)
    }

    pub fn lock(cx: &mut Cx<'_, '_, '_>, _this: Obj, _orientation: String) -> R<Value> {
        super::super::net::settled(
            cx,
            Err(Error::dom(
                "NotSupportedError",
                "screen.orientation.lock() is not available on this device.",
            )),
        )
    }

    pub fn unlock(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        Ok(())
    }
}

/// `navigator.permissions` and `navigator.userActivation`.
pub(crate) fn permissions(cx: &mut Cx<'_, '_, '_>) -> Obj {
    singleton(cx, "permissions", Iface::Permissions)
}

pub(crate) fn user_activation(cx: &mut Cx<'_, '_, '_>) -> Obj {
    singleton(cx, "userActivation", Iface::UserActivation)
}

pub(crate) fn orientation(cx: &mut Cx<'_, '_, '_>) -> Obj {
    singleton(cx, "orientation", Iface::ScreenOrientation)
}
