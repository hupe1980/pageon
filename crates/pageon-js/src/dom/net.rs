//! Script networking: `fetch`, `Headers`, `Request`, `Response`, `AbortController`,
//! `AbortSignal`, `XMLHttpRequest` and `ProgressEvent`.
//!
//! Requests are queued on the page's shared state and sent while the page settles, in issue
//! order, through the session's `pageon-net` state (cookies, CORS, the egress policy); see
//! `Engine::settle`. Responses arrive as tasks on the virtual clock.

use pageon_net::{Credentials, Dest, FetchRequest, Fetched, Mode, Redirect};
use url::Url;

use crate::cv::{self, Cx, Error, Obj, R, Value};
use crate::dom::event_target;
use crate::generated::{Iface, ProgressEventInit, RequestInit, ResponseInit};
use crate::realm::{Done, EventData, Pending, Realm, Slot, Target, Timer};

/// What may change a `Headers` object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Anything.
    None,
    /// A request's headers: forbidden names are ignored.
    Request,
    /// A response's headers: `Set-Cookie` is ignored.
    Response,
    /// Nothing (fetched responses).
    Immutable,
}

/// A `Headers` list (lower-case names, in insertion order).
#[derive(Debug, Clone)]
pub struct HeaderList {
    /// The headers.
    pub list: Vec<(String, String)>,
    /// The guard.
    pub guard: Guard,
}

/// A `Request`.
#[derive(Debug, Clone)]
pub struct RequestData {
    method: String,
    url: Url,
    headers: u32,
    body: Vec<u8>,
    body_used: bool,
    mode: Mode,
    credentials: Credentials,
    redirect: Redirect,
    signal: u32,
}

/// A `Response`.
#[derive(Debug, Clone)]
pub struct ResponseData {
    kind: &'static str,
    url: String,
    redirected: bool,
    status: u16,
    status_text: String,
    headers: u32,
    body: Vec<u8>,
    body_used: bool,
}

/// An `AbortSignal`.
#[derive(Debug, Clone, Default)]
pub struct SignalData {
    aborted: bool,
    reason: Option<Value>,
    dependents: Vec<u32>,
}

impl SignalData {
    /// Whether the signal is aborted.
    pub(crate) fn is_aborted(&self) -> bool {
        self.aborted
    }
}

/// An `XMLHttpRequest`.
#[derive(Debug, Clone, Default)]
pub struct XhrData {
    state: u16,
    method: String,
    url: Option<Url>,
    headers: Vec<(String, String)>,
    sent: bool,
    response: Option<Fetched>,
    failed: bool,
    response_type: String,
    with_credentials: bool,
    timeout: u32,
}

fn illegal() -> Error {
    Error::Type("Illegal invocation".into())
}

fn slot<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&Slot) -> Option<T>) -> R<T> {
    cx.realm
        .st
        .borrow()
        .slot(id)
        .and_then(f)
        .ok_or_else(illegal)
}

fn slot_mut<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&mut Slot) -> Option<T>) -> R<T> {
    cx.realm
        .st
        .borrow_mut()
        .slot_mut(id)
        .and_then(f)
        .ok_or_else(illegal)
}

fn dom_exception<'s>(
    cx: &mut Cx<'_, 's, '_>,
    name: &str,
    message: &str,
) -> v8::Local<'s, v8::Value> {
    let o = cx.alloc(
        Iface::DOMException,
        Slot::Exception {
            name: name.into(),
            message: message.into(),
        },
    );
    cx.wrap(o).into()
}

fn type_error<'s>(cx: &mut Cx<'_, 's, '_>, message: &str) -> v8::Local<'s, v8::Value> {
    let m = cx.str(message);
    v8::Exception::type_error(cx.scope, m)
}

/// A new promise and its resolver.
pub(crate) fn promise(cx: &mut Cx<'_, '_, '_>) -> R<(v8::Global<v8::PromiseResolver>, Value)> {
    let r = v8::PromiseResolver::new(cx.scope).ok_or(Error::Terminated)?;
    let p = r.get_promise(cx.scope);
    Ok((v8::Global::new(cx.scope, r), cx.keep(p.into())))
}

fn settle_promise(
    cx: &mut Cx<'_, '_, '_>,
    resolver: &v8::Global<v8::PromiseResolver>,
    result: Result<v8::Local<'_, v8::Value>, v8::Local<'_, v8::Value>>,
) {
    let r = v8::Local::new(cx.scope, resolver);
    let _ = match result {
        Ok(v) => r.resolve(cx.scope, v),
        Err(e) => r.reject(cx.scope, e),
    };
}

/// A promise resolved with `undefined` (`Ok`) or rejected (`Err`).
pub(crate) fn settled_unit(cx: &mut Cx<'_, '_, '_>, result: R<()>) -> R<Value> {
    match result {
        Ok(()) => {
            let u = v8::undefined(cx.scope).into();
            settled(cx, Ok(u))
        }
        Err(Error::Terminated) => Err(Error::Terminated),
        Err(e) => settled(cx, Err(e)),
    }
}

/// A new, unaborted `AbortSignal` (a tool call's).
pub(crate) fn fresh_signal(cx: &mut Cx<'_, '_, '_>) -> Obj {
    Obj::Slab(new_signal(cx))
}

/// A promise already resolved (`Ok`) or rejected (`Err`).
pub(crate) fn settled(
    cx: &mut Cx<'_, '_, '_>,
    result: Result<v8::Local<'_, v8::Value>, Error>,
) -> R<Value> {
    let (resolver, p) = promise(cx)?;
    match result {
        Ok(v) => settle_promise(cx, &resolver, Ok(v)),
        Err(e) => {
            let v = cv::error_value(cx, e).ok_or(Error::Terminated)?;
            settle_promise(cx, &resolver, Err(v));
        }
    }
    Ok(p)
}

// ----- header and body helpers -----

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn normalize_value(v: &str) -> R<String> {
    let v = v.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
    if v.contains(['\0', '\n', '\r']) {
        return Err(Error::Type(format!("Invalid header value {v:?}")));
    }
    Ok(v.to_string())
}

fn header_name(name: &str) -> R<String> {
    if !is_token(name) {
        return Err(Error::Type(format!("Invalid header name {name:?}")));
    }
    Ok(name.to_ascii_lowercase())
}

fn forbidden_request_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("proxy-")
        || n.starts_with("sec-")
        || matches!(
            n.as_str(),
            "accept-charset"
                | "accept-encoding"
                | "access-control-request-headers"
                | "access-control-request-method"
                | "connection"
                | "content-length"
                | "cookie"
                | "cookie2"
                | "date"
                | "dnt"
                | "expect"
                | "host"
                | "keep-alive"
                | "origin"
                | "referer"
                | "set-cookie"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "via"
        )
}

fn ignored(guard: Guard, name: &str) -> R<bool> {
    match guard {
        Guard::Immutable => Err(Error::Type("Headers are immutable".into())),
        Guard::Request => Ok(forbidden_request_header(name)),
        Guard::Response => Ok(name.eq_ignore_ascii_case("set-cookie")),
        Guard::None => Ok(false),
    }
}

/// The pairs of a `HeadersInit` (a `Headers`, a sequence of pairs or a record).
fn header_init(cx: &mut Cx<'_, '_, '_>, init: &Value) -> R<Vec<(String, String)>> {
    let v = cx.local(init);
    if v.is_null_or_undefined() {
        return Ok(Vec::new());
    }
    if let Some((Obj::Slab(id), Iface::Headers)) = cv::unwrap_any(cx, v) {
        return slot(cx, id, |s| match s {
            Slot::Headers(h) => Some(h.list.clone()),
            _ => None,
        });
    }
    let Ok(obj) = v8::Local::<v8::Object>::try_from(v) else {
        return Err(Error::Type(
            "The provided value is not a sequence or record of headers".into(),
        ));
    };
    let mut out = Vec::new();
    if v.is_array() || has_iterator(cx, obj) {
        let pairs = cv::seq(cx, v, |cx, p| cv::seq(cx, p, cv::string))?;
        for p in pairs {
            let [k, val] = <[String; 2]>::try_from(p)
                .map_err(|_| Error::Type("Each header pair must have two items".into()))?;
            out.push((header_name(&k)?, normalize_value(&val)?));
        }
        return Ok(out);
    }
    let keys = obj
        .get_own_property_names(cx.scope, v8::GetPropertyNamesArgs::default())
        .ok_or(Error::Terminated)?;
    for i in 0..keys.length() {
        let k = keys.get_index(cx.scope, i).ok_or(Error::Terminated)?;
        let name = cv::string(cx, k)?;
        let val = obj.get(cx.scope, k).ok_or(Error::Terminated)?;
        let val = cv::string(cx, val)?;
        out.push((header_name(&name)?, normalize_value(&val)?));
    }
    Ok(out)
}

fn has_iterator(cx: &mut Cx<'_, '_, '_>, obj: v8::Local<'_, v8::Object>) -> bool {
    let sym = v8::Symbol::get_iterator(cx.scope);
    obj.get(cx.scope, sym.into())
        .is_some_and(|f| f.is_function())
}

/// A `multipart/form-data` body and its content type. Boundaries count per page, not random:
/// loads replay identically.
pub(crate) fn multipart(
    cx: &Cx<'_, '_, '_>,
    pairs: &[(String, crate::dom::blobs::Entry)],
) -> (Vec<u8>, String) {
    let n = {
        let mut sh = cx.realm.shared.borrow_mut();
        sh.seq += 1;
        sh.seq
    };
    let boundary = format!("----pageonFormBoundary{n:016x}");
    let esc = |s: &str| {
        s.replace('"', "%22")
            .replace('\r', "%0D")
            .replace('\n', "%0A")
    };
    let mut body: Vec<u8> = Vec::new();
    for (k, val) in pairs {
        match val {
            crate::dom::blobs::Entry::Text(t) => body.extend(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{t}\r\n",
                    esc(k)
                )
                .into_bytes(),
            ),
            crate::dom::blobs::Entry::File(f) => {
                let name = f.file.as_ref().map(|(n, _)| n.as_str()).unwrap_or("blob");
                let ct = if f.content_type.is_empty() {
                    "application/octet-stream"
                } else {
                    &f.content_type
                };
                body.extend(
                    format!(
                        "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {ct}\r\n\r\n",
                        esc(k),
                        esc(name)
                    )
                    .into_bytes(),
                );
                body.extend_from_slice(&f.bytes);
                body.extend_from_slice(b"\r\n");
            }
        }
    }
    body.extend(format!("--{boundary}--\r\n").into_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

/// The bytes and content type of a body (`BodyInit`).
pub(crate) fn body_init(
    cx: &mut Cx<'_, '_, '_>,
    v: v8::Local<'_, v8::Value>,
) -> R<(Vec<u8>, Option<String>)> {
    if v.is_null_or_undefined() {
        return Ok((Vec::new(), None));
    }
    if let Some((Obj::Slab(id), iface)) = cv::unwrap_any(cx, v) {
        match iface {
            Iface::URLSearchParams => {
                let pairs = slot(cx, id, |s| match s {
                    Slot::Params(p, _) => Some(p.clone()),
                    _ => None,
                })?;
                return Ok((
                    crate::dom::urls::serialize(&pairs).into_bytes(),
                    Some("application/x-www-form-urlencoded;charset=UTF-8".into()),
                ));
            }
            Iface::FormData => {
                let pairs = slot(cx, id, |s| match s {
                    Slot::FormData(p) => Some(p.clone()),
                    _ => None,
                })?;
                let (body, ct) = multipart(cx, &pairs);
                return Ok((body, Some(ct)));
            }
            Iface::Blob | Iface::File => {
                let b = slot(cx, id, |s| match s {
                    Slot::Blob(b) => Some(b.clone()),
                    _ => None,
                })?;
                let ct = (!b.content_type.is_empty()).then(|| b.content_type.clone());
                return Ok((b.bytes.to_vec(), ct));
            }
            _ => {}
        }
    }
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        let mut buf = vec![0; view.byte_length()];
        view.copy_contents(&mut buf);
        return Ok((buf, None));
    }
    if let Ok(ab) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        let len = ab.byte_length();
        let store = ab.get_backing_store();
        let bytes: Vec<u8> = store.iter().take(len).map(std::cell::Cell::get).collect();
        return Ok((bytes, None));
    }
    let s = cv::string(cx, v)?;
    Ok((s.into_bytes(), Some("text/plain;charset=UTF-8".into())))
}

fn new_headers(cx: &mut Cx<'_, '_, '_>, list: Vec<(String, String)>, guard: Guard) -> u32 {
    match cx.alloc(Iface::Headers, Slot::Headers(HeaderList { list, guard })) {
        Obj::Slab(i) => i,
        _ => 0,
    }
}

fn header_list(cx: &Cx<'_, '_, '_>, id: u32) -> Vec<(String, String)> {
    slot(cx, id, |s| match s {
        Slot::Headers(h) => Some(h.list.clone()),
        _ => None,
    })
    .unwrap_or_default()
}

fn new_signal(cx: &mut Cx<'_, '_, '_>) -> u32 {
    match cx.alloc(Iface::AbortSignal, Slot::Signal(SignalData::default())) {
        Obj::Slab(i) => i,
        _ => 0,
    }
}

fn normalize_method(m: &str) -> R<String> {
    if !is_token(m) {
        return Err(Error::Type(format!("'{m}' is not a valid HTTP method")));
    }
    let up = m.to_ascii_uppercase();
    if matches!(up.as_str(), "CONNECT" | "TRACE" | "TRACK") {
        return Err(Error::Type(format!("'{m}' HTTP method is unsupported")));
    }
    Ok(
        if matches!(
            up.as_str(),
            "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT"
        ) {
            up
        } else {
            m.to_string()
        },
    )
}

fn base_url(cx: &Cx<'_, '_, '_>) -> Option<Url> {
    Url::parse(&cx.realm.st.borrow().url).ok()
}

fn parse_url(cx: &Cx<'_, '_, '_>, s: &str) -> Option<Url> {
    match base_url(cx) {
        Some(b) => b.join(s).ok(),
        None => Url::parse(s).ok(),
    }
}

fn utf8(body: &[u8]) -> String {
    let b = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
    String::from_utf8_lossy(b).into_owned()
}

fn fetch_request(cx: &Cx<'_, '_, '_>, r: &RequestData, dest: Dest) -> FetchRequest {
    FetchRequest {
        method: r.method.clone(),
        url: r.url.clone(),
        headers: header_list(cx, r.headers),
        body: r.body.clone(),
        dest,
        mode: r.mode,
        credentials: r.credentials,
        redirect: r.redirect,
        max_redirects: 20,
        client: Some(cx.realm.st.borrow().client.clone()),
    }
}

fn request_data(cx: &mut Cx<'_, '_, '_>, input: &Value, init: RequestInit) -> R<RequestData> {
    let iv = cx.local(input);
    let base = match cv::unwrap_any(cx, iv) {
        Some((Obj::Slab(id), Iface::Request)) => Some(slot(cx, id, |s| match s {
            Slot::Request(r) => Some((**r).clone()),
            _ => None,
        })?),
        _ => None,
    };
    let url = match &base {
        Some(b) => b.url.clone(),
        None => {
            let s = cv::string(cx, iv)?;
            parse_url(cx, &s).ok_or_else(|| Error::Type(format!("Failed to parse URL from {s}")))?
        }
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::Type(
            "Request cannot be constructed from a URL that includes credentials".into(),
        ));
    }
    if let Some(b) = &base
        && b.body_used
    {
        return Err(Error::Type(
            "Cannot construct a Request with a used body".into(),
        ));
    }
    let mut method = base
        .as_ref()
        .map_or_else(|| "GET".to_string(), |b| b.method.clone());
    let mut headers = base
        .as_ref()
        .map(|b| header_list(cx, b.headers))
        .unwrap_or_default();
    let mut body = base.as_ref().map(|b| b.body.clone()).unwrap_or_default();
    let mut mode = base.as_ref().map_or(Mode::Cors, |b| b.mode);
    let mut credentials = base
        .as_ref()
        .map_or(Credentials::SameOrigin, |b| b.credentials);
    let mut redirect = base.as_ref().map_or(Redirect::Follow, |b| b.redirect);
    let mut signal = base.as_ref().map(|b| b.signal);
    if let Some(m) = &init.method {
        method = normalize_method(m)?;
    }
    if let Some(m) = &init.mode {
        mode = match m.as_str() {
            "same-origin" => Mode::SameOrigin,
            "no-cors" => Mode::NoCors,
            "cors" => Mode::Cors,
            other => {
                return Err(Error::Type(format!(
                    "Cannot construct a Request with mode {other:?}"
                )));
            }
        };
    }
    if let Some(c) = &init.credentials {
        credentials = match c.as_str() {
            "omit" => Credentials::Omit,
            "include" => Credentials::Include,
            _ => Credentials::SameOrigin,
        };
    }
    if let Some(r) = &init.redirect {
        redirect = match r.as_str() {
            "error" => Redirect::Error,
            "manual" => Redirect::Manual,
            _ => Redirect::Follow,
        };
    }
    if let Some(h) = &init.headers {
        headers = header_init(cx, h)?;
    }
    headers.retain(|(k, _)| !forbidden_request_header(k));
    if let Some(b) = &init.body {
        match b {
            Some(b) => {
                let v = cx.local(b);
                let (bytes, ct) = body_init(cx, v)?;
                if !v.is_null_or_undefined() {
                    body = bytes;
                    if let Some(ct) = ct
                        && !headers.iter().any(|(k, _)| k == "content-type")
                    {
                        headers.push(("content-type".into(), ct));
                    }
                }
            }
            None => body.clear(),
        }
    }
    if !body.is_empty() && matches!(method.as_str(), "GET" | "HEAD") {
        return Err(Error::Type(
            "Request with GET/HEAD method cannot have body.".into(),
        ));
    }
    if mode == Mode::NoCors && !matches!(method.as_str(), "GET" | "HEAD" | "POST") {
        return Err(Error::Type(format!(
            "'{method}' is unsupported in no-cors mode"
        )));
    }
    match &init.signal {
        Some(Some(Obj::Slab(s))) => signal = Some(*s),
        Some(_) => signal = None,
        None => {}
    }
    let signal = match signal {
        Some(s) => s,
        None => new_signal(cx),
    };
    let headers = new_headers(cx, headers, Guard::Request);
    Ok(RequestData {
        method,
        url,
        headers,
        body,
        body_used: false,
        mode,
        credentials,
        redirect,
        signal,
    })
}

fn signal_aborted(cx: &Cx<'_, '_, '_>, id: u32) -> Option<Value> {
    match cx.realm.st.borrow().slot(id) {
        Some(Slot::Signal(s)) if s.aborted => s.reason.clone(),
        _ => None,
    }
}

fn response_obj(cx: &mut Cx<'_, '_, '_>, f: Fetched) -> Obj {
    let headers = new_headers(cx, f.headers, Guard::Immutable);
    cx.alloc(
        Iface::Response,
        Slot::Response(Box::new(ResponseData {
            kind: f.kind.as_str(),
            url: f.url.to_string(),
            redirected: f.redirected,
            status: f.status,
            status_text: f.status_text,
            headers,
            body: f.body,
            body_used: false,
        })),
    )
}

/// Whether a queued request's `fetch` was aborted (it is not sent).
pub(crate) fn aborted_before_send(realm: &Realm, done: &Done) -> bool {
    match done {
        Done::Fetch {
            signal: Some(s), ..
        } => matches!(realm.st.borrow().slot(*s), Some(Slot::Signal(d)) if d.aborted),
        _ => false,
    }
}

/// Runs a request's completion task.
pub(crate) fn complete(
    cx: &mut Cx<'_, '_, '_>,
    done: Done,
    result: Result<Fetched, pageon_net::FetchError>,
) -> R<()> {
    match done {
        Done::Fetch { resolver, .. } => {
            // The response is the fetching world's object.
            let r = v8::Local::new(cx.scope, &resolver);
            let world = cx.world_of_object(r.into());
            cx.in_world(world, |cx| match result {
                Ok(f) => {
                    let o = response_obj(cx, f);
                    let w = cx.wrap(o);
                    settle_promise(cx, &resolver, Ok(w.into()));
                }
                Err(_) => {
                    // The reason is in the request log; scripts see only a network error.
                    let e = type_error(cx, "Failed to fetch");
                    settle_promise(cx, &resolver, Err(e));
                }
            });
            Ok(())
        }
        Done::Xhr(id) => xml_http_request::finish(cx, id, result),
    }
}

/// Most requests one document makes (`fetch`, XHR): past it they fail as network errors and
/// one page error says why.
pub const MAX_REQUESTS: u32 = 10_000;

/// Counts a request of the document: whether it may be made.
pub(crate) fn count_request(cx: &mut Cx<'_, '_, '_>) -> bool {
    let n = {
        let mut st = cx.st();
        st.requests_made = st.requests_made.saturating_add(1);
        st.requests_made
    };
    if n <= MAX_REQUESTS {
        return true;
    }
    crate::dom::internals::refuse(
        cx,
        "requests",
        format!("this document made {MAX_REQUESTS} requests; further requests fail"),
    );
    false
}

/// `fetch()`.
pub(crate) fn fetch(cx: &mut Cx<'_, '_, '_>, input: Value, init: RequestInit) -> R<Value> {
    let data = match request_data(cx, &input, init) {
        Ok(d) => d,
        Err(e) => return settled(cx, Err(e)),
    };
    if let Some(reason) = signal_aborted(cx, data.signal) {
        let r = cx.local(&reason);
        return settled(cx, Err(Error::Thrown(cx.keep(r))));
    }
    if !count_request(cx) {
        return settled(cx, Err(Error::Type("Failed to fetch".into())));
    }
    let req = fetch_request(cx, &data, Dest::Fetch);
    let (resolver, p) = promise(cx)?;
    cx.realm.shared.borrow_mut().requests.push_back(Pending {
        realm: cx.realm.id,
        req,
        done: Done::Fetch {
            resolver,
            signal: Some(data.signal),
        },
    });
    Ok(p)
}

/// `Headers`.
pub mod headers {
    use super::*;

    fn with<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&mut HeaderList) -> R<T>) -> R<T> {
        let id = this.slab()?;
        let mut st = cx.realm.st.borrow_mut();
        match st.slot_mut(id) {
            Some(Slot::Headers(h)) => f(h),
            _ => Err(illegal()),
        }
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, init: Option<Value>) -> R<Obj> {
        let list = match &init {
            Some(v) => header_init(cx, v)?,
            None => Vec::new(),
        };
        Ok(cx.alloc(
            Iface::Headers,
            Slot::Headers(HeaderList {
                list,
                guard: Guard::None,
            }),
        ))
    }

    pub fn append(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String, value: String) -> R<()> {
        let (n, v) = (header_name(&name)?, normalize_value(&value)?);
        with(cx, this, |h| {
            if !ignored(h.guard, &n)? {
                h.list.push((n, v));
            }
            Ok(())
        })
    }

    pub fn delete(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<()> {
        let n = header_name(&name)?;
        with(cx, this, |h| {
            if !ignored(h.guard, &n)? {
                h.list.retain(|(k, _)| *k != n);
            }
            Ok(())
        })
    }

    pub fn get(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<Option<String>> {
        let n = header_name(&name)?;
        with(cx, this, |h| {
            let vals: Vec<&str> = h
                .list
                .iter()
                .filter(|(k, _)| *k == n)
                .map(|(_, v)| v.as_str())
                .collect();
            Ok((!vals.is_empty()).then(|| vals.join(", ")))
        })
    }

    pub fn get_set_cookie(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<String>> {
        with(cx, this, |h| {
            Ok(h.list
                .iter()
                .filter(|(k, _)| k == "set-cookie")
                .map(|(_, v)| v.clone())
                .collect())
        })
    }

    pub fn has(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String) -> R<bool> {
        let n = header_name(&name)?;
        with(cx, this, |h| Ok(h.list.iter().any(|(k, _)| *k == n)))
    }

    pub fn set(cx: &mut Cx<'_, '_, '_>, this: Obj, name: String, value: String) -> R<()> {
        let (n, v) = (header_name(&name)?, normalize_value(&value)?);
        with(cx, this, |h| {
            if ignored(h.guard, &n)? {
                return Ok(());
            }
            match h.list.iter().position(|(k, _)| *k == n) {
                Some(i) => {
                    h.list[i].1 = v;
                    let mut j = i + 1;
                    while j < h.list.len() {
                        if h.list[j].0 == n {
                            h.list.remove(j);
                        } else {
                            j += 1;
                        }
                    }
                }
                None => h.list.push((n, v)),
            }
            Ok(())
        })
    }

    /// Sorted by name, values of a name combined (`Set-Cookie` kept apart).
    pub fn pairs(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Vec<(String, String)>> {
        with(cx, this, |h| {
            let mut names: Vec<&String> = h.list.iter().map(|(k, _)| k).collect();
            names.sort();
            names.dedup();
            let mut out = Vec::new();
            for n in names {
                let vals = h
                    .list
                    .iter()
                    .filter(|(k, _)| k == n)
                    .map(|(_, v)| v.clone());
                if n == "set-cookie" {
                    out.extend(vals.map(|v| (n.clone(), v)));
                } else {
                    out.push((n.clone(), vals.collect::<Vec<_>>().join(", ")));
                }
            }
            Ok(out)
        })
    }
}

/// The `Body` mixin of `Request` and `Response`.
pub mod body {
    use super::*;

    fn take(cx: &Cx<'_, '_, '_>, this: Obj) -> R<R<Vec<u8>>> {
        let id = this.slab()?;
        let mut st = cx.realm.st.borrow_mut();
        let (used, body) = match st.slot_mut(id) {
            Some(Slot::Request(r)) => (&mut r.body_used, &r.body),
            Some(Slot::Response(r)) => (&mut r.body_used, &r.body),
            _ => return Err(illegal()),
        };
        if *used {
            return Ok(Err(Error::Type("body stream already read".into())));
        }
        *used = true;
        Ok(Ok(body.clone()))
    }

    pub fn get_body_used(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        slot(cx, this.slab()?, |s| match s {
            Slot::Request(r) => Some(r.body_used),
            Slot::Response(r) => Some(r.body_used),
            _ => None,
        })
    }

    pub fn text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        match take(cx, this)? {
            Ok(b) => {
                let s = cx.str(&utf8(&b));
                settled(cx, Ok(s.into()))
            }
            Err(e) => settled(cx, Err(e)),
        }
    }

    /// `arrayBuffer()`: the body's bytes.
    pub fn array_buffer(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        match take(cx, this)? {
            Ok(b) => {
                let ab = crate::dom::blobs::array_buffer(cx, &b);
                let ab = cx.local(&ab);
                settled(cx, Ok(ab))
            }
            Err(e) => settled(cx, Err(e)),
        }
    }

    /// `bytes()`: the body's bytes as a `Uint8Array`.
    pub fn bytes(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        match take(cx, this)? {
            Ok(b) => {
                let ab = crate::dom::blobs::array_buffer(cx, &b);
                let ab = cx.local(&ab);
                let ab =
                    v8::Local::<v8::ArrayBuffer>::try_from(ab).map_err(|_| Error::Terminated)?;
                let u8a = v8::Uint8Array::new(cx.scope, ab, 0, b.len()).ok_or(Error::Terminated)?;
                settled(cx, Ok(u8a.into()))
            }
            Err(e) => settled(cx, Err(e)),
        }
    }

    /// `blob()`: the body as a `Blob` typed by the `Content-Type` header.
    pub fn blob(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let headers = {
            let st = cx.realm.st.borrow();
            match this.slab().ok().and_then(|id| st.slot(id)) {
                Some(Slot::Request(r)) => Some(r.headers),
                Some(Slot::Response(r)) => Some(r.headers),
                _ => None,
            }
        };
        let ct = match headers {
            Some(h) => headers::get(cx, Obj::Slab(h), "content-type".into())?.unwrap_or_default(),
            None => String::new(),
        };
        match take(cx, this)? {
            Ok(b) => {
                let data = crate::dom::blobs::BlobData {
                    bytes: b.into(),
                    content_type: ct.to_ascii_lowercase(),
                    file: None,
                };
                let o = crate::dom::blobs::wrap(cx, std::rc::Rc::new(data));
                let w: v8::Local<v8::Value> = cx.wrap(o).into();
                settled(cx, Ok(w))
            }
            Err(e) => settled(cx, Err(e)),
        }
    }

    pub fn json(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let bytes = match take(cx, this)? {
            Ok(b) => b,
            Err(e) => return settled(cx, Err(e)),
        };
        let text = cx.str(&utf8(&bytes));
        let parsed = {
            let scope = &mut *cx.scope;
            v8::tc_scope!(let tc, scope);
            match v8::json::parse(tc, text) {
                Some(v) => Ok(v8::Global::new(tc, v)),
                None => Err(tc.exception().map(|e| v8::Global::new(tc, e))),
            }
        };
        match parsed {
            Ok(v) => {
                let v = cx.local(&v);
                settled(cx, Ok(v))
            }
            Err(Some(e)) => settled(cx, Err(Error::Thrown(e))),
            Err(None) => Err(Error::Terminated),
        }
    }
}

/// `Request`.
pub mod request {
    use super::*;

    fn get<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&RequestData) -> T) -> R<T> {
        slot(cx, this.slab()?, |s| match s {
            Slot::Request(r) => Some(f(r)),
            _ => None,
        })
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, input: Value, init: RequestInit) -> R<Obj> {
        let data = request_data(cx, &input, init)?;
        Ok(cx.alloc(Iface::Request, Slot::Request(Box::new(data))))
    }

    pub fn get_method(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |r| r.method.clone())
    }

    pub fn get_url(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |r| r.url.to_string())
    }

    pub fn get_headers(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        get(cx, this, |r| Obj::Slab(r.headers))
    }

    pub fn get_destination(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(String::new())
    }

    pub fn get_referrer(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok("about:client".into())
    }

    pub fn get_mode(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |r| {
            match r.mode {
                Mode::Navigate => "navigate",
                Mode::SameOrigin => "same-origin",
                Mode::Cors => "cors",
                Mode::NoCors => "no-cors",
            }
            .into()
        })
    }

    pub fn get_credentials(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |r| {
            match r.credentials {
                Credentials::Omit => "omit",
                Credentials::SameOrigin => "same-origin",
                Credentials::Include => "include",
            }
            .into()
        })
    }

    pub fn get_redirect(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |r| {
            match r.redirect {
                Redirect::Follow => "follow",
                Redirect::Error => "error",
                Redirect::Manual => "manual",
            }
            .into()
        })
    }

    pub fn get_signal(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        get(cx, this, |r| Obj::Slab(r.signal))
    }

    pub fn clone(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let mut r = get(cx, this, Clone::clone)?;
        if r.body_used {
            return Err(Error::Type("Request body is already used".into()));
        }
        let list = header_list(cx, r.headers);
        r.headers = new_headers(cx, list, Guard::Request);
        Ok(cx.alloc(Iface::Request, Slot::Request(Box::new(r))))
    }
}

/// `Response`.
pub mod response {
    use super::*;

    fn get<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&ResponseData) -> T) -> R<T> {
        slot(cx, this.slab()?, |s| match s {
            Slot::Response(r) => Some(f(r)),
            _ => None,
        })
    }

    fn make(
        cx: &mut Cx<'_, '_, '_>,
        body: Vec<u8>,
        content_type: Option<String>,
        init: &ResponseInit,
    ) -> R<Obj> {
        if !(200..=599).contains(&init.status) {
            return Err(Error::Range(format!(
                "The status provided ({}) is outside the range [200, 599].",
                init.status
            )));
        }
        let mut list = match &init.headers {
            Some(h) => header_init(cx, h)?,
            None => Vec::new(),
        };
        if let Some(ct) = content_type
            && !list.iter().any(|(k, _)| k == "content-type")
        {
            list.push(("content-type".into(), ct));
        }
        let headers = new_headers(cx, list, Guard::Response);
        Ok(cx.alloc(
            Iface::Response,
            Slot::Response(Box::new(ResponseData {
                kind: "default",
                url: String::new(),
                redirected: false,
                status: init.status,
                status_text: init.status_text.clone(),
                headers,
                body,
                body_used: false,
            })),
        ))
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, body: Option<Value>, init: ResponseInit) -> R<Obj> {
        let (bytes, ct) = match &body {
            Some(b) => {
                let v = cx.local(b);
                body_init(cx, v)?
            }
            None => (Vec::new(), None),
        };
        if body.is_some() && matches!(init.status, 101 | 103 | 204 | 205 | 304) {
            return Err(Error::Type(
                "Response with null body status cannot have body".into(),
            ));
        }
        make(cx, bytes, ct, &init)
    }

    pub fn error(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        let headers = new_headers(cx, Vec::new(), Guard::Immutable);
        Ok(cx.alloc(
            Iface::Response,
            Slot::Response(Box::new(ResponseData {
                kind: "error",
                url: String::new(),
                redirected: false,
                status: 0,
                status_text: String::new(),
                headers,
                body: Vec::new(),
                body_used: false,
            })),
        ))
    }

    pub fn redirect(cx: &mut Cx<'_, '_, '_>, url: String, status: u16) -> R<Obj> {
        let u = parse_url(cx, &url)
            .ok_or_else(|| Error::Type(format!("Failed to parse URL from {url}")))?;
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Err(Error::Range(format!("Invalid status code {status}")));
        }
        let headers = new_headers(
            cx,
            vec![("location".into(), u.to_string())],
            Guard::Immutable,
        );
        Ok(cx.alloc(
            Iface::Response,
            Slot::Response(Box::new(ResponseData {
                kind: "default",
                url: String::new(),
                redirected: false,
                status,
                status_text: String::new(),
                headers,
                body: Vec::new(),
                body_used: false,
            })),
        ))
    }

    pub fn json(cx: &mut Cx<'_, '_, '_>, data: Value, init: ResponseInit) -> R<Obj> {
        let v = cx.local(&data);
        let s = v8::json::stringify(cx.scope, v)
            .ok_or_else(|| Error::Type("The data is not JSON-serializable".into()))?;
        let s = s.to_rust_string_lossy(cx.scope);
        make(cx, s.into_bytes(), Some("application/json".into()), &init)
    }

    pub fn get_type_(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |r| r.kind.to_string())
    }

    pub fn get_url(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |r| r.url.clone())
    }

    pub fn get_redirected(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        get(cx, this, |r| r.redirected)
    }

    pub fn get_status(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        get(cx, this, |r| r.status)
    }

    pub fn get_ok(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        get(cx, this, |r| (200..300).contains(&r.status))
    }

    pub fn get_status_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        get(cx, this, |r| r.status_text.clone())
    }

    pub fn get_headers(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        get(cx, this, |r| Obj::Slab(r.headers))
    }

    pub fn clone(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        let mut r = get(cx, this, Clone::clone)?;
        if r.body_used {
            return Err(Error::Type("Response body is already used".into()));
        }
        let guard = slot(cx, r.headers, |s| match s {
            Slot::Headers(h) => Some(h.guard),
            _ => None,
        })?;
        let list = header_list(cx, r.headers);
        r.headers = new_headers(cx, list, guard);
        Ok(cx.alloc(Iface::Response, Slot::Response(Box::new(r))))
    }
}

/// Aborts a signal: rejects its queued fetches, fires `abort`, then aborts its dependents.
fn signal_abort(cx: &mut Cx<'_, '_, '_>, id: u32, reason: v8::Local<'_, v8::Value>) -> R<()> {
    let kept = cx.keep(reason);
    let dependents = slot_mut(cx, id, |s| match s {
        Slot::Signal(d) if !d.aborted => {
            d.aborted = true;
            d.reason = Some(kept.clone());
            Some(Some(std::mem::take(&mut d.dependents)))
        }
        Slot::Signal(_) => Some(None),
        _ => None,
    })?;
    let Some(dependents) = dependents else {
        return Ok(());
    };
    let cancelled: Vec<Pending> = {
        let mut sh = cx.realm.shared.borrow_mut();
        let realm = cx.realm.id;
        let (out, keep): (Vec<Pending>, Vec<Pending>) = sh.requests.drain(..).partition(|p| {
            p.realm == realm && matches!(p.done, Done::Fetch { signal: Some(s), .. } if s == id)
        });
        sh.requests.extend(keep);
        out
    };
    for p in cancelled {
        if let Done::Fetch { resolver, .. } = p.done {
            let r = cx.local(&kept);
            settle_promise(cx, &resolver, Err(r));
        }
    }
    event_target::fire(cx, Target::Object(id), "abort", false, false)?;
    // Aborting a registerTool signal unregisters its tools.
    super::webmcp::signal_aborted(cx, id)?;
    for d in dependents {
        let r = cx.local(&kept);
        signal_abort(cx, d, r)?;
    }
    Ok(())
}

/// `AbortController`.
pub mod abort_controller {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        let s = new_signal(cx);
        Ok(cx.alloc(Iface::AbortController, Slot::Controller(s)))
    }

    pub fn get_signal(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Obj> {
        slot(cx, this.slab()?, |s| match s {
            Slot::Controller(id) => Some(Obj::Slab(*id)),
            _ => None,
        })
    }

    pub fn abort(cx: &mut Cx<'_, '_, '_>, this: Obj, reason: Option<Value>) -> R<()> {
        let Obj::Slab(id) = get_signal(cx, this)? else {
            return Err(illegal());
        };
        let r = match &reason {
            Some(v) if !cx.local(v).is_undefined() => cx.local(v),
            _ => dom_exception(cx, "AbortError", "signal is aborted without reason"),
        };
        signal_abort(cx, id, r)
    }
}

fn timeout_fired(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    cv::call(scope, &args, &mut rv, |cx, args| {
        let id = args.data().uint32_value(cx.scope).unwrap_or(u32::MAX);
        let reason = dom_exception(cx, "TimeoutError", "signal timed out");
        signal_abort(cx, id, reason)?;
        cx.ret(())
    });
}

/// `AbortSignal`.
pub mod abort_signal {
    use super::*;

    fn get<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&SignalData) -> T) -> R<T> {
        slot(cx, this.slab()?, |s| match s {
            Slot::Signal(d) => Some(f(d)),
            _ => None,
        })
    }

    pub fn abort(cx: &mut Cx<'_, '_, '_>, reason: Option<Value>) -> R<Obj> {
        let id = new_signal(cx);
        let r = match &reason {
            Some(v) if !cx.local(v).is_undefined() => cx.local(v),
            _ => dom_exception(cx, "AbortError", "signal is aborted without reason"),
        };
        signal_abort(cx, id, r)?;
        Ok(Obj::Slab(id))
    }

    pub fn timeout(cx: &mut Cx<'_, '_, '_>, ms: u64) -> R<Obj> {
        let id = new_signal(cx);
        let data = v8::Integer::new_from_unsigned(cx.scope, id);
        let f = v8::Function::builder(timeout_fired)
            .data(data.into())
            .build(cx.scope)
            .ok_or(Error::Terminated)?;
        let cb = cx.keep(f.into());
        let realm = cx.realm.id;
        cx.realm.shared.borrow_mut().schedule(
            Timer {
                id: 0,
                realm,
                cb,
                args: Vec::new(),
                interval: None,
                frame: false,
                idle: false,
                caused: false,
            },
            ms as f64,
        );
        Ok(Obj::Slab(id))
    }

    pub fn any(cx: &mut Cx<'_, '_, '_>, signals: Vec<Obj>) -> R<Obj> {
        let id = new_signal(cx);
        let sources: Vec<u32> = signals.iter().map(|s| s.slab()).collect::<R<_>>()?;
        for &sid in &sources {
            if let Some(reason) = signal_aborted(cx, sid) {
                let r = cx.local(&reason);
                signal_abort(cx, id, r)?;
                return Ok(Obj::Slab(id));
            }
        }
        for sid in sources {
            slot_mut(cx, sid, |s| match s {
                Slot::Signal(d) => {
                    d.dependents.push(id);
                    Some(())
                }
                _ => None,
            })?;
        }
        Ok(Obj::Slab(id))
    }

    pub fn get_aborted(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        get(cx, this, |d| d.aborted)
    }

    pub fn get_reason(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        match get(cx, this, |d| d.reason.clone())? {
            Some(r) => Ok(r),
            None => Ok(cv::undefined(cx)),
        }
    }

    pub fn throw_if_aborted(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        match get(cx, this, |d| d.reason.clone())? {
            Some(r) => Err(Error::Thrown(r)),
            None => Ok(()),
        }
    }
}

/// Fires a `ProgressEvent` at a non-node event target (an XHR, a `FileReader`).
pub(crate) fn progress_event_at(
    cx: &mut Cx<'_, '_, '_>,
    id: u32,
    ty: &str,
    loaded: f64,
    total: f64,
) -> R<()> {
    progress(cx, id, ty, loaded, total)
}

fn progress(cx: &mut Cx<'_, '_, '_>, id: u32, ty: &str, loaded: f64, total: f64) -> R<()> {
    let now = {
        let sh = cx.realm.shared.borrow();
        sh.now - sh.origin
    };
    let data = EventData {
        ty: ty.into(),
        initialized: true,
        trusted: true,
        time: now,
        progress: (total > 0.0, loaded, total),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::ProgressEvent, Slot::Event(Box::new(data)));
    event_target::dispatch(cx, Target::Object(id), e).map(|_| ())
}

/// `ProgressEvent`.
pub mod progress_event {
    use super::*;

    fn get<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&EventData) -> T) -> R<T> {
        slot(cx, this.slab()?, |s| match s {
            Slot::Event(e) => Some(f(e)),
            _ => None,
        })
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: ProgressEventInit) -> R<Obj> {
        let now = {
            let sh = cx.realm.shared.borrow();
            sh.now - sh.origin
        };
        Ok(cx.alloc(
            Iface::ProgressEvent,
            Slot::Event(Box::new(EventData {
                ty,
                bubbles: init.bubbles,
                cancelable: init.cancelable,
                composed: init.composed,
                initialized: true,
                time: now,
                progress: (init.length_computable, init.loaded, init.total),
                ..EventData::default()
            })),
        ))
    }

    pub fn get_length_computable(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        get(cx, this, |e| e.progress.0)
    }

    pub fn get_loaded(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        get(cx, this, |e| e.progress.1)
    }

    pub fn get_total(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<f64> {
        get(cx, this, |e| e.progress.2)
    }
}

/// `XMLHttpRequest` (asynchronous; a synchronous `open` is refused and reported).
pub mod xml_http_request {
    use super::*;

    fn with<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&mut XhrData) -> R<T>) -> R<T> {
        with_id(cx, this.slab()?, f)
    }

    fn with_id<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&mut XhrData) -> R<T>) -> R<T> {
        let mut st = cx.realm.st.borrow_mut();
        match st.slot_mut(id) {
            Some(Slot::Xhr(x)) => f(x),
            _ => Err(illegal()),
        }
    }

    fn state_change(cx: &mut Cx<'_, '_, '_>, id: u32, state: u16) -> R<()> {
        with_id(cx, id, |x| {
            x.state = state;
            Ok(())
        })?;
        event_target::fire(cx, Target::Object(id), "readystatechange", false, false).map(|_| ())
    }

    /// Removes this request's queued send.
    fn unqueue(cx: &Cx<'_, '_, '_>, id: u32) {
        let realm = cx.realm.id;
        cx.realm
            .shared
            .borrow_mut()
            .requests
            .retain(|p| !(p.realm == realm && matches!(p.done, Done::Xhr(x) if x == id)));
    }

    pub fn constructor(cx: &mut Cx<'_, '_, '_>) -> R<Obj> {
        Ok(cx.alloc(Iface::XMLHttpRequest, Slot::Xhr(Box::default())))
    }

    pub fn get_ready_state(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        with(cx, this, |x| Ok(x.state))
    }

    pub fn open(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        method: String,
        url: String,
        is_async: Option<bool>,
        _user: Option<String>,
        _password: Option<String>,
    ) -> R<()> {
        let method = normalize_method(&method).map_err(|_| {
            Error::dom(
                "SyntaxError",
                format!("'{method}' is not a valid HTTP method."),
            )
        })?;
        let u = parse_url(cx, &url)
            .ok_or_else(|| Error::dom("SyntaxError", format!("Invalid URL {url:?}")))?;
        if is_async == Some(false) {
            cx.realm.shared.borrow_mut().error(
                crate::PageErrorKind::Unsupported,
                "synchronous XMLHttpRequest is not supported",
                u.as_str(),
            );
            return Err(Error::dom(
                "InvalidAccessError",
                "Synchronous XMLHttpRequest is not supported.",
            ));
        }
        let id = this.slab()?;
        unqueue(cx, id);
        with(cx, this, |x| {
            let keep = (x.response_type.clone(), x.with_credentials, x.timeout);
            *x = XhrData {
                method,
                url: Some(u),
                response_type: keep.0,
                with_credentials: keep.1,
                timeout: keep.2,
                ..XhrData::default()
            };
            Ok(())
        })?;
        state_change(cx, id, 1)
    }

    pub fn set_request_header(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        name: String,
        value: String,
    ) -> R<()> {
        let n = header_name(&name)
            .map_err(|_| Error::dom("SyntaxError", format!("Invalid header name {name:?}")))?;
        let v = normalize_value(&value)
            .map_err(|_| Error::dom("SyntaxError", "Invalid header value"))?;
        with(cx, this, |x| {
            if x.state != 1 || x.sent {
                return Err(Error::dom(
                    "InvalidStateError",
                    "The object's state must be OPENED.",
                ));
            }
            if forbidden_request_header(&n) {
                return Ok(());
            }
            match x.headers.iter_mut().find(|(k, _)| *k == n) {
                Some(h) => h.1 = format!("{}, {v}", h.1),
                None => x.headers.push((n, v)),
            }
            Ok(())
        })
    }

    pub fn get_timeout(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        with(cx, this, |x| Ok(x.timeout))
    }

    pub fn set_timeout(cx: &mut Cx<'_, '_, '_>, this: Obj, v: u32) -> R<()> {
        with(cx, this, |x| {
            x.timeout = v;
            Ok(())
        })
    }

    pub fn get_with_credentials(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<bool> {
        with(cx, this, |x| Ok(x.with_credentials))
    }

    pub fn set_with_credentials(cx: &mut Cx<'_, '_, '_>, this: Obj, v: bool) -> R<()> {
        with(cx, this, |x| {
            if x.state > 1 || x.sent {
                return Err(Error::dom(
                    "InvalidStateError",
                    "The value may only be set before send().",
                ));
            }
            x.with_credentials = v;
            Ok(())
        })
    }

    pub fn send(cx: &mut Cx<'_, '_, '_>, this: Obj, body: Option<Value>) -> R<()> {
        let id = this.slab()?;
        let (method, url, mut headers, creds) = with(cx, this, |x| {
            if x.state != 1 || x.sent {
                return Err(Error::dom(
                    "InvalidStateError",
                    "The object's state must be OPENED.",
                ));
            }
            x.sent = true;
            Ok((
                x.method.clone(),
                x.url.clone(),
                x.headers.clone(),
                x.with_credentials,
            ))
        })?;
        let url = url.ok_or_else(illegal)?;
        if !count_request(cx) {
            return Err(Error::dom(
                "NetworkError",
                "Failed to execute 'send' on 'XMLHttpRequest': this document made too many requests.",
            ));
        }
        let mut bytes = Vec::new();
        if let Some(b) = &body
            && !matches!(method.as_str(), "GET" | "HEAD")
        {
            let v = cx.local(b);
            let (data, ct) = body_init(cx, v)?;
            bytes = data;
            if let Some(ct) = ct
                && !headers.iter().any(|(k, _)| k == "content-type")
            {
                headers.push(("content-type".into(), ct));
            }
        }
        let client = cx.realm.st.borrow().client.clone();
        let req = FetchRequest {
            method,
            url,
            headers,
            body: bytes,
            dest: Dest::Xhr,
            mode: Mode::Cors,
            credentials: if creds {
                Credentials::Include
            } else {
                Credentials::SameOrigin
            },
            redirect: Redirect::Follow,
            max_redirects: 20,
            client: Some(client),
        };
        let realm = cx.realm.id;
        cx.realm.shared.borrow_mut().requests.push_back(Pending {
            realm,
            req,
            done: Done::Xhr(id),
        });
        progress(cx, id, "loadstart", 0.0, 0.0)
    }

    /// The completion task: `readystatechange` 2, 3 and 4, `progress`, `load`/`error`,
    /// `loadend`.
    pub(crate) fn finish(
        cx: &mut Cx<'_, '_, '_>,
        id: u32,
        result: Result<Fetched, pageon_net::FetchError>,
    ) -> R<()> {
        let ok = result.is_ok();
        let len = result.as_ref().map_or(0, |f| f.body.len()) as f64;
        with_id(cx, id, |x| {
            match result {
                Ok(f) => x.response = Some(f),
                Err(_) => x.failed = true,
            }
            Ok(())
        })?;
        if ok {
            state_change(cx, id, 2)?;
            state_change(cx, id, 3)?;
            progress(cx, id, "progress", len, len)?;
            state_change(cx, id, 4)?;
            progress(cx, id, "load", len, len)?;
            progress(cx, id, "loadend", len, len)
        } else {
            state_change(cx, id, 4)?;
            progress(cx, id, "error", 0.0, 0.0)?;
            progress(cx, id, "loadend", 0.0, 0.0)
        }
    }

    pub fn abort(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = this.slab()?;
        unqueue(cx, id);
        let active = with(cx, this, |x| {
            let active = (x.state == 1 && x.sent) || x.state == 2 || x.state == 3;
            x.sent = false;
            x.response = None;
            x.failed = true;
            Ok(active)
        })?;
        if active {
            state_change(cx, id, 4)?;
            progress(cx, id, "abort", 0.0, 0.0)?;
            progress(cx, id, "loadend", 0.0, 0.0)?;
        }
        with(cx, this, |x| {
            if x.state == 4 {
                x.state = 0;
            }
            Ok(())
        })
    }

    pub fn get_response_url(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |x| {
            Ok(x.response
                .as_ref()
                .map(|f| f.url.to_string())
                .unwrap_or_default())
        })
    }

    pub fn get_status(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u16> {
        with(cx, this, |x| {
            Ok(x.response.as_ref().map_or(0, |f| f.status))
        })
    }

    pub fn get_status_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |x| {
            Ok(x.response
                .as_ref()
                .map(|f| f.status_text.clone())
                .unwrap_or_default())
        })
    }

    pub fn get_response_header(
        cx: &mut Cx<'_, '_, '_>,
        this: Obj,
        name: String,
    ) -> R<Option<String>> {
        let n = name.to_ascii_lowercase();
        with(cx, this, |x| {
            let Some(f) = &x.response else {
                return Ok(None);
            };
            let vals: Vec<&str> = f
                .headers
                .iter()
                .filter(|(k, _)| *k == n)
                .map(|(_, v)| v.as_str())
                .collect();
            Ok((!vals.is_empty()).then(|| vals.join(", ")))
        })
    }

    pub fn get_all_response_headers(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |x| {
            let Some(f) = &x.response else {
                return Ok(String::new());
            };
            let mut names: Vec<&String> = f.headers.iter().map(|(k, _)| k).collect();
            names.sort();
            names.dedup();
            Ok(names
                .into_iter()
                .map(|n| {
                    let v: Vec<&str> = f
                        .headers
                        .iter()
                        .filter(|(k, _)| k == n)
                        .map(|(_, v)| v.as_str())
                        .collect();
                    format!("{n}: {}\r\n", v.join(", "))
                })
                .collect())
        })
    }

    pub fn override_mime_type(_cx: &mut Cx<'_, '_, '_>, _this: Obj, _mime: String) -> R<()> {
        // Bodies are decoded as UTF-8 text or parsed as JSON; the MIME type changes neither.
        Ok(())
    }

    pub fn get_response_type(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |x| Ok(x.response_type.clone()))
    }

    pub fn set_response_type(cx: &mut Cx<'_, '_, '_>, this: Obj, v: String) -> R<()> {
        if !matches!(v.as_str(), "" | "text" | "json" | "arraybuffer") {
            // `blob` and `document` responses are not supported (absent `Blob`, no XML).
            let url = with(cx, this, |x| Ok(x.url.as_ref().map(ToString::to_string)))?;
            cx.realm.shared.borrow_mut().error(
                crate::PageErrorKind::Unsupported,
                format!("XMLHttpRequest responseType {v:?} is not supported"),
                url.unwrap_or_default(),
            );
            return Ok(());
        }
        with(cx, this, |x| {
            if x.state >= 3 {
                return Err(Error::dom(
                    "InvalidStateError",
                    "The response type cannot be changed now.",
                ));
            }
            x.response_type = v;
            Ok(())
        })
    }

    pub fn get_response_text(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        with(cx, this, |x| {
            if !matches!(x.response_type.as_str(), "" | "text") {
                return Err(Error::dom(
                    "InvalidStateError",
                    "responseText is only available if responseType is '' or 'text'.",
                ));
            }
            Ok(match &x.response {
                Some(f) if x.state >= 3 => utf8(&f.body),
                _ => String::new(),
            })
        })
    }

    pub fn get_response(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let (ty, body, state) = with(cx, this, |x| {
            Ok((
                x.response_type.clone(),
                x.response.as_ref().map(|f| f.body.clone()),
                x.state,
            ))
        })?;
        match ty.as_str() {
            "" | "text" => {
                let s = if state >= 3 {
                    body.map(|b| utf8(&b))
                } else {
                    None
                };
                Ok(cv::string_value(cx, &s.unwrap_or_default()))
            }
            _ if state < 4 || body.is_none() => Ok(cv::null(cx)),
            "json" => {
                let text = cx.str(&utf8(&body.unwrap_or_default()));
                let scope = &mut *cx.scope;
                v8::tc_scope!(let tc, scope);
                Ok(match v8::json::parse(tc, text) {
                    Some(v) => v8::Global::new(tc, v),
                    None => {
                        let n = v8::null(tc);
                        v8::Global::new(tc, v8::Local::<v8::Value>::from(n))
                    }
                })
            }
            _ => {
                let bytes = body.unwrap_or_default();
                let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
                let ab = v8::ArrayBuffer::with_backing_store(cx.scope, &store);
                Ok(cx.keep(ab.into()))
            }
        }
    }
}
