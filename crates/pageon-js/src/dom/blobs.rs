//! The File API (W3C): `Blob`, `File`, `FileList`, `FileReader`. A blob is immutable bytes
//! shared by reference (`slice` copies only its range); files add a name and a date. No
//! `blob:` URLs and no streams.

use std::rc::Rc;

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{BlobPropertyBag, FilePropertyBag, Iface};
use crate::realm::Slot;

/// The bytes of a blob, its type, and for a file its name and last-modified time (ms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobData {
    /// Bytes.
    pub bytes: Rc<[u8]>,
    /// MIME type, lower-cased (empty when unknown).
    pub content_type: String,
    /// A file's name and last-modified time.
    pub file: Option<(String, i64)>,
}

impl BlobData {
    /// A file.
    pub fn file(name: String, content_type: String, bytes: Vec<u8>, modified: i64) -> Self {
        Self {
            bytes: bytes.into(),
            content_type,
            file: Some((name, modified)),
        }
    }
}

/// A `FormData` value: text, or a file (a blob appended to a form data is a file named
/// `blob`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A string.
    Text(String),
    /// A file.
    File(Rc<BlobData>),
}

impl Entry {
    /// The text form: the string, or the file's name (as `application/x-www-form-urlencoded`
    /// submits files).
    pub fn text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::File(f) => f.file.as_ref().map(|(n, _)| n.clone()).unwrap_or_default(),
        }
    }
}

/// A `type` as the API keeps it: lower-cased, empty if not printable ASCII.
fn normalize_type(t: &str) -> String {
    if t.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        t.to_ascii_lowercase()
    } else {
        String::new()
    }
}

/// The blob behind a `Blob`/`File` wrapper.
pub(crate) fn blob_of(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Rc<BlobData>> {
    match cx.st().slot(this.slab()?) {
        Some(Slot::Blob(b)) => Ok(b.clone()),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// A wrapper for a blob: a `File` when it has a name.
pub(crate) fn wrap(cx: &mut Cx<'_, '_, '_>, b: Rc<BlobData>) -> Obj {
    let iface = if b.file.is_some() {
        Iface::File
    } else {
        Iface::Blob
    };
    cx.alloc(iface, Slot::Blob(b))
}

/// The bytes of a JavaScript value used as blob data (a `Blob`, a buffer or view, else its
/// string form, UTF-8).
pub(crate) fn bytes_of(cx: &mut Cx<'_, '_, '_>, v: &Value) -> R<Vec<u8>> {
    let l = cx.local(v);
    if let Ok(Obj::Slab(s)) = crate::cv::obj(cx, l, Iface::Blob)
        && let Some(Slot::Blob(b)) = cx.st().slot(s)
    {
        return Ok(b.bytes.to_vec());
    }
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(l) {
        let mut buf = vec![0; view.byte_length()];
        view.copy_contents(&mut buf);
        return Ok(buf);
    }
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(l) {
        let len = ab.byte_length();
        let store = ab.get_backing_store();
        return Ok(store.iter().take(len).map(std::cell::Cell::get).collect());
    }
    Ok(crate::cv::string(cx, l)?.into_bytes())
}

fn concat(cx: &mut Cx<'_, '_, '_>, parts: Option<Vec<Value>>) -> R<Vec<u8>> {
    let mut out = Vec::new();
    for p in parts.unwrap_or_default() {
        out.extend(bytes_of(cx, &p)?);
    }
    Ok(out)
}

pub(crate) fn array_buffer(cx: &mut Cx<'_, '_, '_>, bytes: &[u8]) -> Value {
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes.to_vec()).make_shared();
    let ab: v8::Local<v8::Value> = v8::ArrayBuffer::with_backing_store(cx.scope, &store).into();
    cx.keep(ab)
}

fn resolved(cx: &mut Cx<'_, '_, '_>, v: Value) -> R<Value> {
    let resolver = v8::PromiseResolver::new(cx.scope).ok_or(Error::Terminated)?;
    let v = cx.local(&v);
    resolver.resolve(cx.scope, v);
    let p: v8::Local<v8::Value> = resolver.get_promise(cx.scope).into();
    Ok(cx.keep(p))
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64.
pub(crate) fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
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
    out
}

/// Text decoded as UTF-8 (BOM removed, errors replaced), or as `label`'s encoding.
fn decode(bytes: &[u8], label: Option<&str>) -> String {
    let enc = label
        .and_then(|l| encoding_rs::Encoding::for_label(l.trim().as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    enc.decode_with_bom_removal(bytes).0.into_owned()
}

/// `Blob`.
pub mod blob {
    use super::*;

    pub fn constructor(
        cx: &mut Cx<'_, '_, '_>,
        parts: Option<Vec<Value>>,
        options: BlobPropertyBag,
    ) -> R<Obj> {
        let bytes = concat(cx, parts)?;
        let b = BlobData {
            bytes: bytes.into(),
            content_type: normalize_type(&options.type_),
            file: None,
        };
        Ok(wrap(cx, Rc::new(b)))
    }

    pub fn get_size(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u64> {
        Ok(blob_of(cx, this)?.bytes.len() as u64)
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(blob_of(cx, this)?.content_type.clone())
    }

    /// `slice(start, end, type)`: negative positions count from the end, clamped.
    pub fn slice(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        start: Option<i64>,
        end: Option<i64>,
        content_type: Option<String>,
    ) -> R<Obj> {
        let b = blob_of(cx, this)?;
        let len = b.bytes.len() as i64;
        let pos = |v: i64| {
            if v < 0 { (len + v).max(0) } else { v.min(len) }
        };
        let from = pos(start.unwrap_or(0));
        let to = pos(end.unwrap_or(len)).max(from);
        let piece = BlobData {
            bytes: b.bytes[from as usize..to as usize].into(),
            content_type: content_type.map(|t| normalize_type(&t)).unwrap_or_default(),
            file: None,
        };
        Ok(wrap(cx, Rc::new(piece)))
    }

    pub fn text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let b = blob_of(cx, this)?;
        let s: v8::Local<v8::Value> = cx.str(&decode(&b.bytes, None)).into();
        let s = cx.keep(s);
        resolved(cx, s)
    }

    pub fn array_buffer(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let b = blob_of(cx, this)?;
        let ab = super::array_buffer(cx, &b.bytes);
        resolved(cx, ab)
    }

    pub fn bytes(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let b = blob_of(cx, this)?;
        let ab = super::array_buffer(cx, &b.bytes);
        let ab = cx.local(&ab);
        let ab = v8::Local::<v8::ArrayBuffer>::try_from(ab).map_err(|_| Error::Terminated)?;
        let u8a = v8::Uint8Array::new(cx.scope, ab, 0, b.bytes.len()).ok_or(Error::Terminated)?;
        let v: v8::Local<v8::Value> = u8a.into();
        let v = cx.keep(v);
        resolved(cx, v)
    }
}

/// `File`.
pub mod file {
    use super::*;

    pub fn constructor(
        cx: &mut Cx<'_, '_, '_>,
        parts: Vec<Value>,
        name: String,
        options: FilePropertyBag,
    ) -> R<Obj> {
        let bytes = concat(cx, Some(parts))?;
        let now = cx.realm.shared.borrow().now as i64;
        let b = BlobData {
            bytes: bytes.into(),
            content_type: normalize_type(&options.type_),
            file: Some((name, options.last_modified.unwrap_or(now))),
        };
        Ok(wrap(cx, Rc::new(b)))
    }

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        Ok(blob_of(cx, this)?
            .file
            .as_ref()
            .map(|(n, _)| n.clone())
            .unwrap_or_default())
    }

    pub fn get_last_modified(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<i64> {
        Ok(blob_of(cx, this)?.file.as_ref().map_or(0, |(_, m)| *m))
    }
}

/// `FileList`: a file input's files.
pub mod file_list {
    use super::*;

    fn files(cx: &Cx<'_, '_, '_>, this: Obj) -> R<Vec<Rc<BlobData>>> {
        match cx.st().slot(this.slab()?) {
            Some(Slot::FileList(l)) => Ok(l.clone()),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn item(cx: &mut Cx<'_, '_, '_>, this: Obj, i: u32) -> R<Option<Obj>> {
        let l = files(cx, this)?;
        Ok(l.get(i as usize).cloned().map(|b| wrap(cx, b)))
    }

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        Ok(u32::try_from(files(cx, this)?.len()).unwrap_or(u32::MAX))
    }
}

/// A `FileReader`'s state.
#[derive(Debug, Default, Clone)]
pub struct ReaderData {
    /// 0 EMPTY, 1 LOADING, 2 DONE.
    pub state: u16,
    /// The result (a string or an `ArrayBuffer`).
    pub result: Option<Value>,
    /// The read in flight, its kind and blob; `abort` clears it.
    pending: Option<(Read, Rc<BlobData>, Option<String>)>,
    /// Bumped by each read and abort, so a stale completion does nothing.
    generation: u32,
}

#[derive(Debug, Clone, Copy)]
enum Read {
    Buffer,
    Text,
    DataUrl,
}

/// `FileReader`: reads complete in a later task, with `loadstart`, `progress`, `load` and
/// `loadend` (or `abort`).
pub mod file_reader {
    use super::*;

    fn with<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&mut ReaderData) -> T) -> R<T> {
        match cx.st().slot_mut(this.slab()?) {
            Some(Slot::Reader(r)) => Ok(f(r)),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        Ok(cx.alloc(Iface::FileReader, Slot::Reader(Box::default())))
    }

    fn start(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        kind: Read,
        blob: Obj,
        enc: Option<String>,
    ) -> R<()> {
        let b = blob_of(cx, blob)?;
        let id = this.slab()?;
        let generation = with(cx, this, |r| {
            if r.state == 1 {
                return Err(Error::dom(
                    "InvalidStateError",
                    "The object is already busy reading Blobs.",
                ));
            }
            r.state = 1;
            r.result = None;
            r.generation += 1;
            r.pending = Some((kind, b, enc));
            Ok(r.generation)
        })??;
        // The read completes in a task of its own.
        let data = v8::Number::new(cx.scope, f64::from(id) * 65_536.0 + f64::from(generation));
        let f = v8::Function::builder(complete)
            .data(data.into())
            .build(cx.scope)
            .ok_or(Error::Terminated)?;
        let cb = cx.keep(f.into());
        let realm = cx.realm.id;
        cx.realm.shared.borrow_mut().schedule(
            crate::realm::Timer {
                id: 0,
                realm,
                cb,
                args: Vec::new(),
                interval: None,
                frame: false,
                idle: false,
                caused: false,
            },
            0.0,
        );
        Ok(())
    }

    fn complete(
        scope: &mut v8::PinScope,
        args: v8::FunctionCallbackArguments,
        mut rv: v8::ReturnValue,
    ) {
        let n = args.data().number_value(scope).unwrap_or(0.0) as u64;
        let (id, generation) = ((n >> 16) as u32, (n & 0xffff) as u32);
        crate::cv::call(scope, &args, &mut rv, |cx, _| {
            let pending = match cx.st().slot_mut(id) {
                Some(Slot::Reader(r)) if r.generation == generation => r.pending.take(),
                _ => None,
            };
            let Some((kind, b, enc)) = pending else {
                return cx.ret(());
            };
            let total = b.bytes.len() as f64;
            fire(cx, id, "loadstart", 0.0, total)?;
            let kept: Value = match kind {
                Read::Buffer => array_buffer(cx, &b.bytes),
                Read::Text => {
                    let s: v8::Local<v8::Value> = cx.str(&decode(&b.bytes, enc.as_deref())).into();
                    cx.keep(s)
                }
                Read::DataUrl => {
                    let ty = if b.content_type.is_empty() {
                        "application/octet-stream"
                    } else {
                        &b.content_type
                    };
                    let s: v8::Local<v8::Value> = cx
                        .str(&format!("data:{ty};base64,{}", base64(&b.bytes)))
                        .into();
                    cx.keep(s)
                }
            };
            if let Some(Slot::Reader(r)) = cx.st().slot_mut(id) {
                r.state = 2;
                r.result = Some(kept);
            }
            fire(cx, id, "progress", total, total)?;
            fire(cx, id, "load", total, total)?;
            fire(cx, id, "loadend", total, total)?;
            cx.ret(())
        });
    }

    fn fire(cx: &mut Cx<'_, '_, '_>, id: u32, ty: &str, loaded: f64, total: f64) -> R<()> {
        crate::dom::net::progress_event_at(cx, id, ty, loaded, total)
    }

    pub fn read_as_array_buffer(cx: &mut Cx<'_, '_, '_>, this: Obj, blob: Obj) -> R<()> {
        start(cx, this, Read::Buffer, blob, None)
    }

    pub fn read_as_text(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        blob: Obj,
        enc: Option<String>,
    ) -> R<()> {
        start(cx, this, Read::Text, blob, enc)
    }

    pub fn read_as_data_url(cx: &mut Cx<'_, '_, '_>, this: Obj, blob: Obj) -> R<()> {
        start(cx, this, Read::DataUrl, blob, None)
    }

    pub fn abort(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let was_loading = with(cx, this, |r| {
            let loading = r.state == 1;
            r.generation += 1;
            r.pending = None;
            if loading {
                r.state = 2;
                r.result = None;
            }
            loading
        })?;
        if was_loading {
            let id = this.slab()?;
            fire(cx, id, "abort", 0.0, 0.0)?;
            fire(cx, id, "loadend", 0.0, 0.0)?;
        }
        Ok(())
    }

    pub fn get_ready_state(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        with(cx, this, |r| r.state)
    }

    pub fn get_result(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Value>> {
        with(cx, this, |r| r.result.clone())
    }

    pub fn get_error(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Option<Obj>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
