//! Platform basics single-page apps call at startup: `matchMedia` (Stylo evaluates
//! the queries), `crypto` (a per-document ChaCha20 stream keyed from the `Env` seam),
//! `TextEncoder`/`TextDecoder` (encoding_rs), `ResizeObserver` (delivered at the rendering step
//! after IntersectionObserver), `requestIdleCallback` and the `Image`/`Option` constructors.

use html5ever::ns;
use pageon_dom::NodeId;

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{
    IdleRequestOptions, Iface, MediaQueryListEventInit, ResizeObserverOptions, TextDecodeOptions,
    TextDecoderOptions,
};
use crate::realm::{EventData, Slot, Target};

/// The largest `getRandomValues` request (WebCrypto).
pub const MAX_RANDOM: usize = 65_536;

fn now(cx: &Cx<'_, '_, '_>) -> f64 {
    let sh = cx.realm.shared.borrow();
    sh.now - sh.origin
}

// ----- matchMedia -----

/// A `MediaQueryList`.
#[derive(Debug, Clone)]
pub struct MediaData {
    query: String,
    media: String,
    matches: bool,
}

fn mql(cx: &Cx<'_, '_, '_>, this: Obj) -> R<MediaData> {
    match cx.realm.st.borrow().slot(this.slab()?) {
        Some(Slot::MediaQuery(m)) => Ok((**m).clone()),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// `matchMedia(query)`: a new list, evaluated now and re-evaluated when the viewport changes.
pub(crate) fn match_media(cx: &mut Cx<'_, '_, '_>, query: String) -> Obj {
    let viewport = cx.st().viewport;
    let (media, matches) = pageon_layout::media_query(viewport, &query);
    let o = cx.alloc(
        Iface::MediaQueryList,
        Slot::MediaQuery(Box::new(MediaData {
            query,
            media,
            matches,
        })),
    );
    if let Ok(id) = o.slab() {
        let mut st = cx.st();
        st.media_lists.push(id);
        st.media_viewport.get_or_insert(viewport);
    }
    o
}

/// Re-evaluates the page's media query lists after a viewport change and fires `change` at
/// those whose result changed. Returns whether an event was fired.
pub(crate) fn deliver_media(cx: &mut Cx<'_, '_, '_>) -> R<bool> {
    let (lists, viewport, changed) = {
        let st = cx.st();
        let changed = st.media_viewport.is_some_and(|v| v != st.viewport);
        (st.media_lists.clone(), st.viewport, changed)
    };
    if !changed {
        return Ok(false);
    }
    cx.st().media_viewport = Some(viewport);
    let mut fired = false;
    for id in lists {
        let Some(query) = (match cx.realm.st.borrow().slot(id) {
            Some(Slot::MediaQuery(m)) => Some(m.query.clone()),
            _ => None,
        }) else {
            continue;
        };
        let (media, matches) = pageon_layout::media_query(viewport, &query);
        let was = match cx.realm.st.borrow_mut().slot_mut(id) {
            Some(Slot::MediaQuery(m)) => std::mem::replace(&mut m.matches, matches),
            _ => continue,
        };
        if was == matches {
            continue;
        }
        let data = EventData {
            ty: "change".into(),
            initialized: true,
            trusted: true,
            time: now(cx),
            media: Some((media, matches)),
            ..EventData::default()
        };
        let e = cx.alloc(Iface::MediaQueryListEvent, Slot::Event(Box::new(data)));
        crate::dom::event_target::dispatch(cx, Target::Object(id), e)?;
        fired = true;
    }
    Ok(fired)
}

pub mod media_query_list {
    use super::*;

    pub fn get_media(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(mql(cx, this)?.media)
    }

    pub fn get_matches(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(mql(cx, this)?.matches)
    }

    /// `addListener(cb)`: `addEventListener('change', cb)`.
    pub fn add_listener(cx: &mut Cx<'_, '_, '_>, this: Obj, cb: Option<Value>) -> R<()> {
        crate::dom::event_target::add_event_listener(
            cx,
            this,
            "change".into(),
            cb,
            crate::cv::OrBool::Bool(false),
        )
    }

    pub fn remove_listener(cx: &mut Cx<'_, '_, '_>, this: Obj, cb: Option<Value>) -> R<()> {
        crate::dom::event_target::remove_event_listener(
            cx,
            this,
            "change".into(),
            cb,
            crate::cv::OrBool::Bool(false),
        )
    }
}

pub mod media_query_list_event {
    use super::*;

    pub fn constructor(
        cx: &mut Cx<'_, '_, '_>,
        ty: String,
        init: MediaQueryListEventInit,
    ) -> R<Obj> {
        let data = EventData {
            ty,
            bubbles: init.bubbles,
            cancelable: init.cancelable,
            composed: init.composed,
            initialized: true,
            time: now(cx),
            media: Some((init.media, init.matches)),
            ..EventData::default()
        };
        Ok(cx.alloc(Iface::MediaQueryListEvent, Slot::Event(Box::new(data))))
    }

    fn media(cx: &Cx<'_, '_, '_>, this: Obj) -> R<(String, bool)> {
        match cx.realm.st.borrow().slot(this.slab()?) {
            Some(Slot::Event(e)) => Ok(e.media.clone().unwrap_or_default()),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_media(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(media(cx, this)?.0)
    }

    pub fn get_matches(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        Ok(media(cx, this)?.1)
    }
}

// ----- crypto -----

pub mod crypto {
    use super::*;

    /// `getRandomValues(array)`: fills an integer typed array in place.
    pub fn get_random_values(cx: &mut Cx<'_, '_, '_>, _this: Obj, array: Value) -> R<Value> {
        let v = cx.local(&array);
        let integer = v.is_int8_array()
            || v.is_uint8_array()
            || v.is_uint8_clamped_array()
            || v.is_int16_array()
            || v.is_uint16_array()
            || v.is_int32_array()
            || v.is_uint32_array()
            || v.is_big_int64_array()
            || v.is_big_uint64_array();
        let view = v8::Local::<v8::ArrayBufferView>::try_from(v)
            .ok()
            .filter(|_| integer);
        let Some(view) = view else {
            return Err(Error::dom(
                "TypeMismatchError",
                "Failed to execute 'getRandomValues' on 'Crypto': The provided ArrayBufferView is not an integer array type.",
            ));
        };
        let len = view.byte_length();
        if len > MAX_RANDOM {
            return Err(Error::dom(
                "QuotaExceededError",
                format!(
                    "Failed to execute 'getRandomValues' on 'Crypto': The ArrayBufferView's byte length ({len}) exceeds the number of bytes of entropy available via this API ({MAX_RANDOM})."
                ),
            ));
        }
        let mut bytes = vec![0u8; len];
        cx.st().crypto.fill(&mut bytes);
        if let Some(store) = view.get_backing_store() {
            let start = view.byte_offset();
            for (cell, b) in store.iter().skip(start).zip(bytes) {
                cell.set(b);
            }
        }
        Ok(array)
    }

    /// `subtle`: Web Crypto ([SecureContext]: hidden on insecure pages).
    pub fn get_subtle(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(crate::dom::window_same(
            cx,
            "subtle",
            Iface::SubtleCrypto,
            Slot::Subtle,
        ))
    }

    /// `randomUUID()`: a version 4 UUID.
    pub fn random_uuid(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        let mut b = [0u8; 16];
        cx.st().crypto.fill(&mut b);
        b[6] = (b[6] & 0x0f) | 0x40;
        b[8] = (b[8] & 0x3f) | 0x80;
        let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
        Ok(format!(
            "{}-{}-{}-{}-{}",
            &h[0..8],
            &h[8..12],
            &h[12..16],
            &h[16..20],
            &h[20..32]
        ))
    }
}

/// A document's `crypto` stream: ChaCha20 keyed with 32 bytes from the environment.
pub struct CryptoStream(chacha20::ChaCha20Rng);

impl std::fmt::Debug for CryptoStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CryptoStream")
    }
}

impl CryptoStream {
    /// A stream keyed with `key`.
    pub fn new(key: [u8; 32]) -> Self {
        use rand_core::SeedableRng;
        Self(chacha20::ChaCha20Rng::from_seed(key))
    }

    /// Fills `out` with the stream's next bytes.
    pub fn fill(&mut self, out: &mut [u8]) {
        rand_core::Rng::fill_bytes(&mut self.0, out);
    }
}

// ----- TextEncoder / TextDecoder -----

fn bytes_of(cx: &mut Cx<'_, '_, '_>, v: &Value) -> R<Vec<u8>> {
    let l = cx.local(v);
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(l) {
        let mut buf = vec![0; view.byte_length()];
        view.copy_contents(&mut buf);
        return Ok(buf);
    }
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(l) {
        let store = ab.get_backing_store();
        return Ok(store
            .iter()
            .take(ab.byte_length())
            .map(std::cell::Cell::get)
            .collect());
    }
    if let Ok(ab) = v8::Local::<v8::SharedArrayBuffer>::try_from(l) {
        let store = ab.get_backing_store();
        return Ok(store.iter().map(std::cell::Cell::get).collect());
    }
    Err(Error::Type(
        "Failed to execute 'decode' on 'TextDecoder': The provided value is not of type '(ArrayBuffer or ArrayBufferView)'."
            .into(),
    ))
}

fn uint8_array<'s>(cx: &mut Cx<'_, 's, '_>, bytes: Vec<u8>) -> v8::Local<'s, v8::Value> {
    let len = bytes.len();
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let ab = v8::ArrayBuffer::with_backing_store(cx.scope, &store);
    match v8::Uint8Array::new(cx.scope, ab, 0, len) {
        Some(a) => a.into(),
        None => v8::undefined(cx.scope).into(),
    }
}

pub mod text_encoder {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        Ok(cx.alloc(Iface::TextEncoder, Slot::Encoder))
    }

    pub fn encode(cx: &mut Cx<'_, '_, '_>, _this: Obj, input: String) -> R<Value> {
        let a = uint8_array(cx, input.into_bytes());
        Ok(cx.keep(a))
    }

    /// `encodeInto(source, destination)`: whole code points only; `read` counts UTF-16 units.
    pub fn encode_into(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        source: String,
        destination: Value,
    ) -> R<Value> {
        let d = cx.local(&destination);
        let Some(view) = v8::Local::<v8::Uint8Array>::try_from(d).ok() else {
            return Err(Error::Type(
                "Failed to execute 'encodeInto' on 'TextEncoder': parameter 2 is not of type 'Uint8Array'.".into(),
            ));
        };
        let room = view.byte_length();
        let (mut read, mut written) = (0usize, 0usize);
        let mut out = Vec::with_capacity(room.min(source.len()));
        for c in source.chars() {
            let n = c.len_utf8();
            if written + n > room {
                break;
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            written += n;
            read += c.len_utf16();
        }
        if let Some(store) = view.get_backing_store() {
            let start = view.byte_offset();
            for (cell, b) in store.iter().skip(start).zip(out) {
                cell.set(b);
            }
        }
        let o = v8::Object::new(cx.scope);
        for (k, n) in [("read", read), ("written", written)] {
            let key = cx.str(k);
            let val = v8::Number::new(cx.scope, n as f64);
            o.set(cx.scope, key.into(), val.into());
        }
        Ok(cx.keep(o.into()))
    }
}

pub mod text_encoder_common {
    use super::*;

    pub fn get_encoding(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("utf-8".into())
    }
}

/// A `TextDecoder`.
pub struct DecoderData {
    encoding: &'static encoding_rs::Encoding,
    fatal: bool,
    ignore_bom: bool,
    /// A streaming decode in progress.
    state: Option<encoding_rs::Decoder>,
}

// A copy starts a new stream (an encoding_rs decoder cannot be cloned).
impl Clone for DecoderData {
    fn clone(&self) -> Self {
        Self {
            encoding: self.encoding,
            fatal: self.fatal,
            ignore_bom: self.ignore_bom,
            state: None,
        }
    }
}

impl std::fmt::Debug for DecoderData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecoderData")
            .field("encoding", &self.encoding.name())
            .field("fatal", &self.fatal)
            .finish_non_exhaustive()
    }
}

fn with_decoder<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&mut DecoderData) -> T) -> R<T> {
    match cx.realm.st.borrow_mut().slot_mut(this.slab()?) {
        Some(Slot::Decoder(d)) => Ok(f(d)),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

pub mod text_decoder {
    use super::*;

    pub fn constructor(
        cx: &mut Cx<'_, '_, '_>,
        label: String,
        options: TextDecoderOptions,
    ) -> R<Obj> {
        let encoding = encoding_rs::Encoding::for_label(label.trim().as_bytes())
            .filter(|e| *e != encoding_rs::REPLACEMENT)
            .ok_or_else(|| {
                Error::Range(format!(
                    "Failed to construct 'TextDecoder': The encoding label provided ('{label}') is invalid."
                ))
            })?;
        Ok(cx.alloc(
            Iface::TextDecoder,
            Slot::Decoder(Box::new(DecoderData {
                encoding,
                fatal: options.fatal,
                ignore_bom: options.ignore_bom,
                state: None,
            })),
        ))
    }

    /// `decode(input, {stream})`: a streaming decode keeps partial sequences until the call
    /// without `stream`.
    pub fn decode(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        input: Option<Value>,
        options: TextDecodeOptions,
    ) -> R<String> {
        let bytes = match &input {
            Some(v) if !cx.local(v).is_null_or_undefined() => bytes_of(cx, v)?,
            _ => Vec::new(),
        };
        let last = !options.stream;
        let out = with_decoder(cx, this, |d| {
            let mut dec = d.state.take().unwrap_or_else(|| {
                if d.ignore_bom {
                    d.encoding.new_decoder_without_bom_handling()
                } else {
                    d.encoding.new_decoder_with_bom_removal()
                }
            });
            let cap = dec
                .max_utf8_buffer_length(bytes.len())
                .unwrap_or(bytes.len() * 3 + 16);
            let mut s = String::with_capacity(cap);
            let ok = if d.fatal {
                let (r, _) = dec.decode_to_string_without_replacement(&bytes, &mut s, last);
                !matches!(r, encoding_rs::DecoderResult::Malformed(..))
            } else {
                let (_, _, _) = dec.decode_to_string(&bytes, &mut s, last);
                true
            };
            if !last && ok {
                d.state = Some(dec);
            }
            ok.then_some(s)
        })?;
        out.ok_or_else(|| {
            Error::Type(
                "Failed to execute 'decode' on 'TextDecoder': The encoded data was not valid."
                    .into(),
            )
        })
    }
}

pub mod text_decoder_common {
    use super::*;

    pub fn get_encoding(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with_decoder(cx, this, |d| d.encoding.name().to_ascii_lowercase())
    }

    pub fn get_fatal(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        with_decoder(cx, this, |d| d.fatal)
    }

    pub fn get_ignore_bom(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        with_decoder(cx, this, |d| d.ignore_bom)
    }
}

// ----- ResizeObserver -----

/// A `ResizeObserver`.
#[derive(Debug, Clone)]
pub struct ResizeData {
    callback: Value,
    /// (target, observe the border box, last reported size).
    targets: Vec<(NodeId, bool, (f64, f64))>,
}

/// A `ResizeObserverEntry`.
#[derive(Debug, Clone)]
pub struct ResizeEntryData {
    target: NodeId,
    /// The content box (x, y relative to the border box's padding edge, width, height).
    content: [f64; 4],
    /// The border box size.
    border: (f64, f64),
}

fn ro(cx: &Cx<'_, '_, '_>, this: Obj) -> R<u32> {
    let id = this.slab()?;
    match cx.realm.st.borrow().slot(id) {
        Some(Slot::Resize(_)) => Ok(id),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn with_ro<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&mut ResizeData) -> T) -> Option<T> {
    match cx.realm.st.borrow_mut().slot_mut(id) {
        Some(Slot::Resize(o)) => Some(f(o)),
        _ => None,
    }
}

fn px(cx: &Cx<'_, '_, '_>, n: NodeId, prop: &str) -> f64 {
    cx.realm
        .layout()
        .style(n, prop)
        .and_then(|v| v.trim_end_matches("px").parse::<f64>().ok())
        .unwrap_or(0.0)
}

/// The content box (in the border box) and the border box size; zero for unrendered elements.
fn boxes(cx: &Cx<'_, '_, '_>, n: NodeId) -> ([f64; 4], (f64, f64)) {
    let rendered = cx.realm.layout().visibility(n) != pageon_layout::Visibility::NotRendered;
    let Some(r) = rendered
        .then(|| crate::dom::elements::rect(cx, n))
        .flatten()
    else {
        return ([0.0; 4], (0.0, 0.0));
    };
    let (w, h) = (f64::from(r.width), f64::from(r.height));
    let [pl, pr, pt, pb, bl, br, bt, bb] = [
        "padding-left",
        "padding-right",
        "padding-top",
        "padding-bottom",
        "border-left-width",
        "border-right-width",
        "border-top-width",
        "border-bottom-width",
    ]
    .map(|p| px(cx, n, p));
    let cw = (w - pl - pr - bl - br).max(0.0);
    let ch = (h - pt - pb - bt - bb).max(0.0);
    ([pl, pt, cw, ch], (w, h))
}

/// Delivers resize observations (at most `MAX_PASSES` per rendering step; then the browser's
/// loop error). Returns whether a callback ran.
pub(crate) fn deliver_resize(cx: &mut Cx<'_, '_, '_>, pass: u32) -> R<bool> {
    const MAX_PASSES: u32 = 2;
    let observers = cx.st().resize_observers.clone();
    if observers.is_empty() {
        return Ok(false);
    }
    let mut ran = false;
    for id in observers {
        let Some(targets) = with_ro(cx, id, |o| o.targets.clone()) else {
            continue;
        };
        let mut entries = Vec::new();
        for (i, (t, border_box, last)) in targets.iter().enumerate() {
            if cx.dom().get(*t).is_none() {
                continue;
            }
            let (content, border) = boxes(cx, *t);
            let size = if *border_box {
                border
            } else {
                (content[2], content[3])
            };
            if size == *last {
                continue;
            }
            with_ro(cx, id, |o| {
                if let Some(e) = o.targets.get_mut(i) {
                    e.2 = size;
                }
            });
            entries.push(ResizeEntryData {
                target: *t,
                content,
                border,
            });
        }
        if entries.is_empty() {
            continue;
        }
        if pass >= MAX_PASSES {
            cx.realm.shared.borrow_mut().error(
                crate::PageErrorKind::Exception,
                "ResizeObserver loop completed with undelivered notifications.",
                cx.st().url.clone(),
            );
            crate::dom::event_target::fire(cx, Target::Window, "error", false, true)?;
            continue;
        }
        let Some(callback) = with_ro(cx, id, |o| o.callback.clone()) else {
            continue;
        };
        let ids: Vec<u32> = entries
            .into_iter()
            .filter_map(|e| {
                cx.alloc(Iface::ResizeObserverEntry, Slot::ResizeEntry(Box::new(e)))
                    .slab()
                    .ok()
            })
            .collect();
        let world = cx.world_of(&callback);
        cx.in_world(world, |cx| {
            let items: Vec<v8::Local<v8::Value>> =
                ids.iter().map(|&e| cx.wrap(Obj::Slab(e)).into()).collect();
            let list: v8::Local<v8::Value> = v8::Array::new_with_elements(cx.scope, &items).into();
            let me: v8::Local<v8::Value> = cx.wrap(Obj::Slab(id)).into();
            cx.invoke(&callback, me, &[list, me])
        })?;
        ran = true;
    }
    Ok(ran)
}

pub mod resize_observer {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, callback: Value) -> R<Obj> {
        if !cx.local(&callback).is_function() {
            return Err(Error::Type(
                "Failed to construct 'ResizeObserver': parameter 1 is not of type 'Function'."
                    .into(),
            ));
        }
        Ok(cx.alloc(
            Iface::ResizeObserver,
            Slot::Resize(Box::new(ResizeData {
                callback,
                targets: Vec::new(),
            })),
        ))
    }

    /// `observe(target, {box})`: reported at the next rendering step unless its size is 0×0.
    pub fn observe(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        target: Obj,
        options: ResizeObserverOptions,
    ) -> R<()> {
        let id = ro(cx, this)?;
        let t = target.node()?;
        let border_box = options.box_ == "border-box";
        with_ro(cx, id, |o| {
            o.targets.retain(|(n, ..)| *n != t);
            o.targets.push((t, border_box, (0.0, 0.0)));
        });
        let mut st = cx.st();
        if !st.resize_observers.contains(&id) {
            st.resize_observers.push(id);
        }
        Ok(())
    }

    pub fn unobserve(cx: &mut Cx<'_, '_, '_>, this: Obj, target: Obj) -> R<()> {
        let id = ro(cx, this)?;
        let t = target.node()?;
        with_ro(cx, id, |o| o.targets.retain(|(n, ..)| *n != t));
        Ok(())
    }

    pub fn disconnect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = ro(cx, this)?;
        with_ro(cx, id, |o| o.targets.clear());
        cx.st().resize_observers.retain(|&o| o != id);
        Ok(())
    }
}

fn resize_entry(cx: &Cx<'_, '_, '_>, this: Obj) -> R<ResizeEntryData> {
    match cx.realm.st.borrow().slot(this.slab()?) {
        Some(Slot::ResizeEntry(e)) => Ok((**e).clone()),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

fn size(cx: &mut Cx<'_, '_, '_>, (w, h): (f64, f64)) -> Obj {
    cx.alloc(Iface::ResizeObserverSize, Slot::ResizeSize([w, h]))
}

pub mod resize_observer_entry {
    use super::*;

    pub fn get_target(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        Ok(Obj::Node(resize_entry(cx, this)?.target))
    }

    pub fn get_content_rect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let c = resize_entry(cx, this)?.content;
        Ok(cx.alloc(Iface::DOMRectReadOnly, Slot::Rect(c)))
    }

    pub fn get_border_box_size(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<Obj>> {
        let b = resize_entry(cx, this)?.border;
        Ok(vec![size(cx, b)])
    }

    pub fn get_content_box_size(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<Obj>> {
        let c = resize_entry(cx, this)?.content;
        Ok(vec![size(cx, (c[2], c[3]))])
    }

    /// Device pixels: pageon paints at a device pixel ratio of 1.
    pub fn get_device_pixel_content_box_size(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<Obj>> {
        get_content_box_size(cx, this)
    }
}

fn wh(cx: &Cx<'_, '_, '_>, this: Obj) -> R<[f64; 2]> {
    match cx.realm.st.borrow().slot(this.slab()?) {
        Some(Slot::ResizeSize(s)) => Ok(*s),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

pub mod resize_observer_size {
    use super::*;

    pub fn get_inline_size(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(wh(cx, this)?[0])
    }

    pub fn get_block_size(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        Ok(wh(cx, this)?[1])
    }
}

// ----- requestIdleCallback -----

/// The idle period a callback gets (ms).
pub const IDLE_MS: f64 = 50.0;

/// `requestIdleCallback(cb, {timeout})`: runs when no other timer is due.
pub(crate) fn request_idle_callback(
    cx: &mut Cx<'_, '_, '_>,
    cb: Value,
    _options: IdleRequestOptions,
) -> R<u32> {
    if !cx.local(&cb).is_function() {
        return Err(Error::Type(
            "Failed to execute 'requestIdleCallback' on 'Window': The callback provided as parameter 1 is not a function.".into(),
        ));
    }
    let timer = crate::realm::Timer {
        id: 0,
        realm: cx.realm.id,
        cb,
        args: Vec::new(),
        interval: None,
        frame: false,
        idle: true,
        caused: false,
    };
    Ok(cx.realm.shared.borrow_mut().schedule(timer, 0.0))
}

/// The `IdleDeadline` of a callback starting now.
pub(crate) fn idle_deadline(cx: &mut Cx<'_, '_, '_>) -> Obj {
    let end = now(cx) + IDLE_MS;
    cx.alloc(Iface::IdleDeadline, Slot::Idle(end))
}

pub mod idle_deadline {
    use super::*;

    pub fn time_remaining(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        let end = match cx.realm.st.borrow().slot(this.slab()?) {
            Some(Slot::Idle(e)) => *e,
            _ => return Err(Error::Type("Illegal invocation".into())),
        };
        Ok((end - now(cx)).max(0.0))
    }

    pub fn get_did_timeout(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(false)
    }
}

// ----- Image and Option -----

pub mod html_image_element {

    /// `loading`: `lazy` or `eager`.
    pub fn get_loading(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        let lazy = cx
            .dom()
            .get(this.node()?)
            .and_then(|n| n.attr("loading"))
            .is_some_and(|v| v.eq_ignore_ascii_case("lazy"));
        Ok(if lazy { "lazy" } else { "eager" }.into())
    }

    pub fn set_loading(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        cx.dom_mut()
            .set_attr(this.node()?, "loading", &v)
            .map_err(|_| crate::dom::gone())
    }
    use super::*;

    /// `new Image(width, height)`.
    pub fn legacy_factory(
        cx: &mut Cx<'_, '_, '_>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> R<Obj> {
        let el = super::super::create_element(&mut cx.dom_mut(), ns!(html), "img");
        for (name, v) in [("width", width), ("height", height)] {
            if let Some(v) = v {
                let _ = cx.dom_mut().set_attr(el, name, &v.to_string());
            }
        }
        Ok(Obj::Node(el))
    }
}

/// `new Option(text, value, defaultSelected, selected)`.
pub(crate) fn new_option(
    cx: &mut Cx<'_, '_, '_>,
    text: String,
    value: Option<String>,
    default_selected: bool,
    selected: bool,
) -> R<Obj> {
    let el = super::create_element(&mut cx.dom_mut(), ns!(html), "option");
    if !text.is_empty() {
        let t = cx.dom_mut().create(pageon_dom::NodeData::Text(text));
        let _ = cx.dom_mut().append(el, t);
    }
    if let Some(v) = value {
        let _ = cx.dom_mut().set_attr(el, "value", &v);
    }
    if default_selected {
        let _ = cx.dom_mut().set_attr(el, "selected", "");
    }
    if selected {
        crate::dom::html_option_element::set_selected(cx, Obj::Node(el), true)?;
    }
    Ok(Obj::Node(el))
}
