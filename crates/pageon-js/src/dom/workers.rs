//! Dedicated workers. A worker runs in the page's engine, on its thread and virtual clock, in a
//! context of its own (its own global and security token): only messages cross, as structured
//! clones. `new Worker` queues the start; the engine loads the script (`Engine::start_worker`)
//! and delivers messages and errors in posting order (`Engine::deliver_worker_mail`).

use std::cell::RefCell;

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{ErrorEventInit, Iface, WorkerOptions};
use crate::modules::ScriptJob;
use crate::realm::{ErrorInfo, EventData, Slot, Target};
use pageon_net::{Dest, FetchRequest};

/// Workers one engine runs at once.
pub const MAX_WORKERS: usize = 16;

/// Messages waiting between workers and their owners; beyond this, messages are dropped.
const MAX_MAIL: usize = 4096;

/// Scripts `importScripts` can load, found when a worker starts.
const MAX_IMPORTS: usize = 64;

/// A worker realm's link to its `Worker` object.
#[derive(Debug, Clone, Copy)]
pub struct Link {
    /// The realm that created the worker.
    pub owner: usize,
    /// The `Worker` object there (slab id).
    pub handle: u32,
}

/// A `Worker` object's state.
#[derive(Debug, Default, Clone)]
pub struct Handle {
    /// The script URL.
    pub url: String,
    /// `name`.
    pub name: String,
    /// `type: "module"`.
    pub module: bool,
    /// The worker's realm, once started.
    pub realm: Option<usize>,
    /// Messages posted before it started.
    pub waiting: Vec<Vec<u8>>,
    /// Terminated, closed or failed to start.
    pub ended: bool,
}

/// What crosses between a worker and its owner.
#[derive(Debug)]
pub enum Mail {
    /// A message for the worker's global.
    ToWorker {
        /// The worker realm.
        realm: usize,
        /// Serialized message.
        data: Vec<u8>,
    },
    /// A message for the `Worker` object.
    ToOwner {
        /// Where the worker came from.
        link: Link,
        /// Serialized message.
        data: Vec<u8>,
    },
    /// An uncaught error, or a script that failed to load: an `ErrorEvent` at the `Worker`.
    Error {
        /// Where the worker came from.
        link: Link,
        /// Message, filename, line, column.
        error: (String, String, u32, u32),
    },
}

fn handle<T>(cx: &Cx<'_, '_, '_>, id: u32, f: impl FnOnce(&mut Handle) -> T) -> R<T> {
    match cx.st().slot_mut(id) {
        Some(Slot::Worker(h)) => Ok(f(h)),
        _ => Err(Error::Type("Illegal invocation".into())),
    }
}

/// Queues mail (dropped, with one page error, when too much waits).
pub(crate) fn send(shared: &RefCell<crate::realm::Shared>, mail: Mail) {
    let mut sh = shared.borrow_mut();
    if sh.worker_mail.len() >= MAX_MAIL {
        if sh.worker_mail.len() == MAX_MAIL {
            sh.error(
                crate::PageErrorKind::Exception,
                format!("postMessage: {MAX_MAIL} worker messages are already waiting; further messages are dropped"),
                "postMessage",
            );
        }
        return;
    }
    sh.worker_mail.push_back(mail);
}

/// Ends a worker realm: no task runs in it again, and what it had queued is dropped.
fn end(shared: &RefCell<crate::realm::Shared>, realm: usize) {
    let r = shared
        .borrow()
        .realms
        .get(realm)
        .and_then(std::rc::Weak::upgrade);
    if let Some(r) = r {
        let mut st = r.st.borrow_mut();
        if st.closed {
            return;
        }
        st.closed = true;
    }
    let mut sh = shared.borrow_mut();
    sh.workers = sh.workers.saturating_sub(1);
    sh.timers.retain(|_, t| t.realm != realm);
    sh.requests.retain(|p| p.realm != realm);
    sh.worker_mail
        .retain(|m| !matches!(m, Mail::ToWorker { realm: r, .. } if *r == realm));
}

/// `postMessage(message, transfer)` or `postMessage(message, {transfer})`: the message and
/// the transfer list.
fn message_args(
    cx: &mut Cx<'_, '_, '_>,
    args: &v8::FunctionCallbackArguments,
    ctx: &str,
) -> R<(Value, Vec<Value>)> {
    if args.length() == 0 {
        return Err(Error::args(ctx, 0));
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
    Ok((message, transfer))
}

/// `Worker.prototype.postMessage` (both overloads).
pub(crate) fn worker_post_message(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, args| {
        let this = cx.this(args, Iface::Worker)?;
        let id = this.slab()?;
        let (message, transfer) = message_args(cx, args, "Worker.postMessage")?;
        let data = crate::dom::session::serialize_transfer(cx, &message, &transfer)?;
        let (realm, ended) = handle(cx, id, |h| (h.realm, h.ended))?;
        if !ended {
            match realm {
                Some(realm) => send(&cx.realm.shared, Mail::ToWorker { realm, data }),
                None => handle(cx, id, |h| h.waiting.push(data))?,
            }
        }
        cx.ret(())
    });
}

/// `DedicatedWorkerGlobalScope.postMessage` (both overloads).
pub(crate) fn global_post_message(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    crate::cv::call(scope, &args, &mut rv, |cx, args| {
        let (message, transfer) = message_args(cx, args, "DedicatedWorkerGlobalScope.postMessage")?;
        let data = crate::dom::session::serialize_transfer(cx, &message, &transfer)?;
        if let Some(link) = cx.realm.worker {
            send(&cx.realm.shared, Mail::ToOwner { link, data });
        }
        cx.ret(())
    });
}

pub mod worker {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, url: String, options: WorkerOptions) -> R<Obj> {
        let Some(u) = crate::dom::resolve_url(cx, &url).and_then(|u| ::url::Url::parse(&u).ok())
        else {
            return Err(Error::dom(
                "SyntaxError",
                format!("Failed to construct 'Worker': The URL '{url}' is invalid."),
            ));
        };
        let origin = cx.st().origin.clone();
        let same = u.origin().ascii_serialization() == origin;
        if !matches!(u.scheme(), "data" | "blob") && !same {
            return Err(Error::dom(
                "SecurityError",
                format!(
                    "Failed to construct 'Worker': Script at '{u}' cannot be accessed from origin '{origin}'."
                ),
            ));
        }
        {
            let mut sh = cx.realm.shared.borrow_mut();
            if sh.workers >= MAX_WORKERS {
                return Err(Error::dom(
                    "QuotaExceededError",
                    format!(
                        "Failed to construct 'Worker': a page runs at most {MAX_WORKERS} workers."
                    ),
                ));
            }
            sh.workers += 1;
        }
        let obj = cx.alloc(
            Iface::Worker,
            Slot::Worker(Box::new(Handle {
                url: u.to_string(),
                name: options.name,
                module: options.type_ == "module",
                ..Handle::default()
            })),
        );
        let owner = cx.realm.id;
        cx.realm
            .shared
            .borrow_mut()
            .scripts
            .push_back(ScriptJob::Worker {
                owner,
                handle: obj.slab()?,
            });
        Ok(obj)
    }

    pub fn terminate(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        let id = this.slab()?;
        let (realm, ended) = handle(cx, id, |h| {
            let was = h.ended;
            h.ended = true;
            h.waiting.clear();
            (h.realm, was)
        })?;
        match realm {
            Some(r) => end(&cx.realm.shared, r),
            // Not started yet: the start finds it ended.
            None if !ended => {
                let mut sh = cx.realm.shared.borrow_mut();
                sh.workers = sh.workers.saturating_sub(1);
            }
            None => {}
        }
        Ok(())
    }
}

pub mod worker_global_scope {
    use super::*;

    pub fn get_self_(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(Obj::Window)
    }

    pub fn get_location(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(crate::dom::window_same(
            cx,
            "location",
            Iface::WorkerLocation,
            Slot::Location,
        ))
    }

    pub fn get_navigator(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Obj> {
        Ok(crate::dom::window_same(
            cx,
            "navigator",
            Iface::WorkerNavigator,
            Slot::Navigator,
        ))
    }

    /// `importScripts(...urls)`: runs each script now, in order. `blob:` and `data:` URLs are
    /// read directly; other scripts must have been loaded when the worker started (the URLs
    /// its scripts name as string literals).
    pub fn import_scripts(cx: &mut Cx<'_, '_, '_>, _this: Obj, urls: Vec<String>) -> R<()> {
        let base = cx.st().url.clone();
        let mut sources = Vec::new();
        for url in urls {
            let resolved = ::url::Url::parse(&base)
                .and_then(|b| b.join(url.trim()))
                .map_err(|_| {
                    Error::dom(
                        "SyntaxError",
                        format!("Failed to execute 'importScripts': The URL '{url}' is invalid."),
                    )
                })?
                .to_string();
            let text = source_now(cx, &resolved).ok_or_else(|| {
                Error::dom(
                    "NetworkError",
                    format!(
                        "Failed to execute 'importScripts': The script at '{resolved}' failed to load (pageon loads the scripts a worker names as string literals when it starts)."
                    ),
                )
            })?;
            sources.push((resolved, text));
        }
        for (url, text) in sources {
            let scope = &mut *cx.scope;
            v8::tc_scope!(let tc, scope);
            let Some(code) = v8::String::new(tc, &text) else {
                return Err(Error::Terminated);
            };
            let name = v8::String::new(tc, &url).map(Into::into);
            let origin = name.map(|n| {
                v8::ScriptOrigin::new(tc, n, 0, 0, false, 0, None, false, false, false, None)
            });
            let ran = v8::Script::compile(tc, code, origin.as_ref()).and_then(|s| s.run(tc));
            if ran.is_none() {
                if tc.is_execution_terminating() {
                    return Err(Error::Terminated);
                }
                if let Some(e) = tc.exception() {
                    return Err(Error::Thrown(v8::Global::new(tc, e)));
                }
            }
        }
        Ok(())
    }
}

/// A script's text available without the network: a `blob:` URL, a `data:` URL, or one the
/// worker loaded when it started.
fn source_now(cx: &Cx<'_, '_, '_>, url: &str) -> Option<String> {
    if url.starts_with("blob:") {
        let sh = cx.realm.shared.borrow();
        return sh
            .blob_urls
            .get(url)
            .map(|(b, _)| String::from_utf8_lossy(b).into_owned());
    }
    if url.starts_with("data:") {
        return data_url(url).map(|b| String::from_utf8_lossy(&b).into_owned());
    }
    cx.st().imports.get(url).cloned()
}

/// The bytes of a `data:` URL (`data:[<mime>][;base64],<data>`).
pub(crate) fn data_url(url: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let bytes = percent_decode(data);
    if meta.ends_with(";base64") {
        let clean: Vec<u8> = bytes
            .into_iter()
            .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
            .collect();
        base64::engine::general_purpose::STANDARD_NO_PAD
            .decode(clean)
            .ok()
    } else {
        Some(bytes)
    }
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// The URLs a classic script passes to `importScripts` as string literals.
fn literal_imports(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("importScripts(") {
        rest = &rest[i + "importScripts(".len()..];
        let args = rest.split(')').next().unwrap_or_default();
        for a in args.split(',') {
            let a = a.trim();
            let q = a.chars().next();
            if let Some(q) = q.filter(|q| matches!(q, '\'' | '"' | '`'))
                && a.len() >= 2
                && a.ends_with(q)
                && !a.contains("${")
            {
                out.push(a[1..a.len() - 1].to_string());
            }
        }
    }
    out
}

pub mod dedicated_worker_global_scope {
    use super::*;

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().window_name.clone())
    }

    /// `close()`: the worker ends once the current task is done; what it posted in that task
    /// still reaches its owner.
    pub fn close(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<()> {
        end(&cx.realm.shared, cx.realm.id);
        Ok(())
    }
}

pub mod worker_location {
    use super::*;

    fn url(cx: &Cx<'_, '_, '_>) -> ::url::Url {
        ::url::Url::parse(&cx.st().url)
            .unwrap_or_else(|_| ::url::Url::parse("about:blank").unwrap_or_else(|_| unreachable!()))
    }

    pub fn get_href(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(url(cx).to_string())
    }

    pub fn get_origin(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(cx.st().origin.clone())
    }

    pub fn get_protocol(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(format!("{}:", url(cx).scheme()))
    }

    pub fn get_host(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        let u = url(cx);
        Ok(match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => String::new(),
        })
    }

    pub fn get_hostname(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(url(cx).host_str().unwrap_or_default().to_string())
    }

    pub fn get_port(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(url(cx).port().map(|p| p.to_string()).unwrap_or_default())
    }

    pub fn get_pathname(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(url(cx).path().to_string())
    }

    pub fn get_search(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(url(cx)
            .query()
            .filter(|q| !q.is_empty())
            .map(|q| format!("?{q}"))
            .unwrap_or_default())
    }

    pub fn get_hash(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(url(cx)
            .fragment()
            .filter(|f| !f.is_empty())
            .map(|f| format!("#{f}"))
            .unwrap_or_default())
    }
}

pub mod error_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: ErrorEventInit) -> R<Obj> {
        let ev = EventData {
            ty,
            bubbles: init.bubbles,
            cancelable: init.cancelable,
            composed: init.composed,
            initialized: true,
            error: Some(Box::new((
                init.message,
                init.filename,
                init.lineno,
                init.colno,
                init.error,
            ))),
            ..EventData::default()
        };
        Ok(cx.alloc(Iface::ErrorEvent, Slot::Event(Box::new(ev))))
    }

    fn field<T>(cx: &Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&ErrorInfo) -> T) -> R<T> {
        match cx.st().slot(this.slab()?) {
            Some(Slot::Event(e)) => e
                .error
                .as_deref()
                .map(f)
                .ok_or_else(|| Error::Type("Illegal invocation".into())),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_message(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        field(cx, this, |e| e.0.clone())
    }

    pub fn get_filename(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        field(cx, this, |e| e.1.clone())
    }

    pub fn get_lineno(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        field(cx, this, |e| e.2)
    }

    pub fn get_colno(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<u32> {
        field(cx, this, |e| e.3)
    }

    pub fn get_error(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let v = field(cx, this, |e| e.4.clone())?;
        Ok(v.unwrap_or_else(|| {
            let u: v8::Local<'_, v8::Value> = v8::undefined(cx.scope).into();
            v8::Global::new(cx.scope, u)
        }))
    }
}

/// Fires mail at its target: a `message` event, or an `ErrorEvent` at the `Worker`.
pub(crate) fn deliver(cx: &mut Cx<'_, '_, '_>, mail: Mail) -> R<()> {
    let time = {
        let sh = cx.realm.shared.borrow();
        sh.now - sh.origin
    };
    let (target, ev, iface) = match mail {
        Mail::ToWorker { data, .. } => {
            let data = crate::dom::session::deserialize(cx, &data)?;
            (
                Target::Window,
                message_event(data, time),
                Iface::MessageEvent,
            )
        }
        Mail::ToOwner { link, data } => {
            if handle(cx, link.handle, |h| h.ended)? {
                return Ok(());
            }
            let data = crate::dom::session::deserialize(cx, &data)?;
            (
                Target::Object(link.handle),
                message_event(data, time),
                Iface::MessageEvent,
            )
        }
        Mail::Error { link, error } => {
            let ev = EventData {
                ty: "error".into(),
                cancelable: true,
                initialized: true,
                trusted: true,
                time,
                error: Some(Box::new((error.0, error.1, error.2, error.3, None))),
                ..EventData::default()
            };
            (Target::Object(link.handle), ev, Iface::ErrorEvent)
        }
    };
    let e = cx.alloc(iface, Slot::Event(Box::new(ev)));
    crate::dom::event_target::dispatch(cx, target, e).map(|_| ())
}

fn message_event(data: Value, time: f64) -> EventData {
    EventData {
        ty: "message".into(),
        initialized: true,
        trusted: true,
        time,
        detail: Some(data),
        message: Some(Box::new((String::new(), String::new(), None))),
        ..EventData::default()
    }
}

/// `URL.createObjectURL(blob)`: a `blob:` URL of this origin for the blob's bytes.
pub(crate) fn create_object_url(cx: &mut Cx<'_, '_, '_>, blob: &Value) -> R<String> {
    let v = cx.local(blob);
    let obj = v8::Local::<v8::Object>::try_from(v)
        .ok()
        .and_then(|o| crate::cv::unwrap_obj(cx, o.into(), Iface::Blob).ok())
        .ok_or_else(|| {
            Error::Type(
                "Failed to execute 'createObjectURL' on 'URL': Overload resolution failed.".into(),
            )
        })?;
    let b = crate::dom::blobs::blob_of(cx, obj)?;
    let mut id = [0u8; 16];
    cx.st().crypto.fill(&mut id);
    id[6] = (id[6] & 0x0f) | 0x40;
    id[8] = (id[8] & 0x3f) | 0x80;
    let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
    let uuid = format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    );
    let origin = cx.st().origin.clone();
    let url = format!("blob:{origin}/{uuid}");
    cx.realm
        .shared
        .borrow_mut()
        .blob_urls
        .insert(url.clone(), (b.bytes.to_vec(), b.content_type.clone()));
    Ok(url)
}

impl crate::Engine {
    /// Starts a worker queued by `new Worker`: its realm, its script, then the messages that
    /// waited. A script that cannot load fires `error` at the `Worker`.
    pub(crate) fn start_worker(&mut self, owner: usize, id: u32, env: &dyn pageon_env::Env) {
        let Some(o) = self.realm(owner).cloned() else {
            return;
        };
        let (url, name, module) = match o.st.borrow().slot(id) {
            Some(Slot::Worker(h)) if !h.ended => (h.url.clone(), h.name.clone(), h.module),
            _ => return,
        };
        let link = Link { owner, handle: id };
        let fail = |e: &mut Self, msg: String| {
            if let Some(Slot::Worker(h)) = o.st.borrow_mut().slot_mut(id) {
                h.ended = true;
                h.waiting.clear();
            }
            {
                let mut sh = e.shared.borrow_mut();
                sh.workers = sh.workers.saturating_sub(1);
                sh.error(crate::PageErrorKind::Load, msg.clone(), url.as_str());
            }
            send(
                &e.shared,
                Mail::Error {
                    link,
                    error: (msg, url.clone(), 0, 0),
                },
            );
        };
        let Ok(parsed) = ::url::Url::parse(&url) else {
            return fail(self, format!("invalid worker URL {url}"));
        };
        // The script, unless a module loads it itself.
        let inline = if url.starts_with("blob:") {
            let found = self.shared.borrow().blob_urls.get(&url).cloned();
            match found {
                Some((b, _)) => Some(String::from_utf8_lossy(&b).into_owned()),
                None => {
                    return fail(
                        self,
                        format!("the worker script {url} was revoked or never created"),
                    );
                }
            }
        } else if url.starts_with("data:") {
            match data_url(&url) {
                Some(b) => Some(String::from_utf8_lossy(&b).into_owned()),
                None => return fail(self, "the worker's data: URL is malformed".into()),
            }
        } else {
            None
        };
        // A `data:` worker has an opaque origin; others share the page's.
        let mut key = [0u8; 32];
        o.st.borrow_mut().crypto.fill(&mut key);
        let cfg = {
            let st = o.st.borrow();
            let mut client = st.client.clone();
            client.url = parsed.clone();
            if url.starts_with("data:") {
                client = client.sandboxed(Some("allow-scripts"));
            }
            crate::RealmConfig {
                url: url.clone(),
                origin: if client.opaque() {
                    "null".to_string()
                } else {
                    st.origin.clone()
                },
                navigator: st.navigator.clone(),
                viewport: st.viewport,
                stylesheets: Vec::new(),
                client,
                net: st.net.clone(),
                accept_dialogs: false,
                crypto_key: key,
            }
        };
        let realm = self.add_worker_realm(cfg, link);
        if let Some(r) = self.realm(realm) {
            r.st.borrow_mut().window_name = name;
        }
        let waiting = match o.st.borrow_mut().slot_mut(id) {
            Some(Slot::Worker(h)) => {
                h.realm = Some(realm);
                std::mem::take(&mut h.waiting)
            }
            _ => Vec::new(),
        };
        let before = self.shared.borrow().errors.len();
        if module {
            if !self.run_module(realm, &parsed, inline.as_deref(), None, env) {
                return fail(self, format!("the worker module {url} failed to load"));
            }
        } else {
            let text = match inline {
                Some(t) => t,
                None => match self.worker_script(realm, &parsed, env) {
                    Ok(t) => t,
                    Err(e) => return fail(self, e),
                },
            };
            self.preload_imports(realm, &parsed, &text, env);
            self.run_script(realm, &text, url.as_str(), None);
        }
        self.forward_errors(realm, before);
        for data in waiting {
            send(&self.shared, Mail::ToWorker { realm, data });
        }
    }

    /// Fetches a classic worker script (same origin, a script response).
    fn worker_script(
        &mut self,
        realm: usize,
        url: &::url::Url,
        env: &dyn pageon_env::Env,
    ) -> Result<String, String> {
        let r = self.realm(realm).cloned().ok_or("the worker is gone")?;
        let (net, client) = {
            let st = r.st.borrow();
            (st.net.clone(), st.client.clone())
        };
        let req = FetchRequest::subresource(url.clone(), Dest::Worker, client);
        let now = self.shared.borrow().now as u64;
        match net.borrow_mut().fetch(env, req, now) {
            Ok(f) if (200..300).contains(&f.status) => {
                Ok(String::from_utf8_lossy(&f.body).into_owned())
            }
            Ok(f) => Err(format!("the worker script {url} answered {}", f.status)),
            Err(e) => Err(format!("the worker script {url} failed to load: {e}")),
        }
    }

    /// Loads the scripts a worker's code names in `importScripts("…")`, and theirs, so the
    /// synchronous call can run them.
    fn preload_imports(
        &mut self,
        realm: usize,
        base: &::url::Url,
        text: &str,
        env: &dyn pageon_env::Env,
    ) {
        let mut queue: Vec<(::url::Url, String)> = literal_imports(text)
            .into_iter()
            .filter_map(|u| base.join(&u).ok().map(|u| (u, String::new())))
            .collect();
        let mut seen = 0;
        while let Some((u, _)) = queue.pop() {
            if seen >= MAX_IMPORTS || !matches!(u.scheme(), "http" | "https" | "file") {
                continue;
            }
            let known = self
                .realm(realm)
                .is_some_and(|r| r.st.borrow().imports.contains_key(u.as_str()));
            if known {
                continue;
            }
            seen += 1;
            if let Ok(t) = self.worker_script(realm, &u, env) {
                queue.extend(
                    literal_imports(&t)
                        .into_iter()
                        .filter_map(|n| u.join(&n).ok().map(|n| (n, String::new()))),
                );
                if let Some(r) = self.realm(realm) {
                    r.st.borrow_mut().imports.insert(u.to_string(), t);
                }
            }
        }
    }

    /// Turns the errors a worker realm recorded since `before` into `ErrorEvent`s at its
    /// `Worker`.
    pub(crate) fn forward_errors(&self, realm: usize, before: usize) {
        let Some(link) = self.realm(realm).and_then(|r| r.worker) else {
            return;
        };
        let new: Vec<(String, String, u32, u32)> = {
            let sh = self.shared.borrow();
            sh.errors
                .get(before..)
                .unwrap_or_default()
                .iter()
                .filter(|e| e.kind == crate::PageErrorKind::Exception)
                .map(|e| (e.message.clone(), e.source.clone(), e.line, e.column))
                .collect()
        };
        for error in new {
            send(&self.shared, Mail::Error { link, error });
        }
    }

    /// Delivers one waiting message or error; returns whether there was one.
    pub(crate) fn deliver_worker_mail(&mut self) -> bool {
        let Some(mail) = self.shared.borrow_mut().worker_mail.pop_front() else {
            return false;
        };
        let realm = match &mail {
            Mail::ToWorker { realm, .. } => *realm,
            Mail::ToOwner { link, .. } | Mail::Error { link, .. } => link.owner,
        };
        let (_, fired) = self.guarded(|e| {
            e.with_cx(realm, |cx| deliver(cx, mail));
        });
        self.terminated(fired, "message");
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_imports_are_found() {
        let src = "importScripts('a.js', \"b.js\");\nif (x) importScripts(`c.js`, url, `${d}.js`);";
        assert_eq!(literal_imports(src), ["a.js", "b.js", "c.js"]);
    }

    #[test]
    fn data_urls_decode() {
        assert_eq!(
            data_url("data:text/javascript,a%20b").as_deref(),
            Some(&b"a b"[..])
        );
        assert_eq!(
            data_url("data:text/javascript;base64,YWI=").as_deref(),
            Some(&b"ab"[..])
        );
    }
}
