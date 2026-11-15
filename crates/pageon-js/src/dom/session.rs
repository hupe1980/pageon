//! The History API (HTML "session history"). The tab's history is the one source of truth:
//! a page's `pushState`/`replaceState`/`back`/`forward`/`go` (and fragment navigations) are
//! queued as [`HistoryOp`]s, which the tab applies after the task; it moves through entries of
//! the same document by [`restore`] (`popstate`, `hashchange`), and loads other documents.
//! State is serialized with V8's serializer (`structuredClone`'s), at most [`MAX_STATE`].

use std::cell::RefCell;
use std::rc::Rc;

use v8::{ValueDeserializerHelper as _, ValueSerializerHelper as _};

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::{Iface, PopStateEventInit};
use crate::realm::{EventData, Slot, Target};

/// The largest serialized state (Firefox's figure).
pub const MAX_STATE: usize = 16 << 20;
/// The most entries a tab keeps (Chrome's figure).
pub const MAX_ENTRIES: u32 = 50;
/// How long a user activation lasts (page time, ms; Chrome's transient activation).
pub const ACTIVATION_MS: f64 = 5000.0;

/// Serialized state, shared between the tab's entry and the document.
pub type State = Rc<Vec<u8>>;

/// What a page asked of the tab's history during a task.
#[derive(Debug, Clone)]
pub enum HistoryOp {
    /// A new entry of this document (`pushState`, a fragment navigation).
    Push {
        /// The entry's URL.
        url: String,
        /// Its state.
        state: Option<State>,
        /// Whether the page had a user activation (otherwise the entry it left is skippable
        /// for the agent's back and forward).
        activated: bool,
    },
    /// The current entry changed (`replaceState`, `location.replace` of a fragment).
    Replace {
        /// The entry's URL.
        url: String,
        /// Its state.
        state: Option<State>,
    },
    /// `history.go(delta)` (`back`: -1, `forward`: 1).
    Traverse(i32),
}

/// A document's view of the session history.
#[derive(Debug)]
pub struct HistoryState {
    /// Entries in the tab (set by the tab, updated by this document's pushes).
    pub length: u32,
    /// The current entry's index (likewise).
    pub index: u32,
    /// The current entry's serialized state.
    pub state: Option<State>,
    /// `history.state` per world, deserialized once per state.
    pub cache: Vec<(u32, usize, Value)>,
    /// `scrollRestoration` is `manual`.
    pub manual_scroll: bool,
    /// Requests for the tab, in order.
    pub ops: Vec<HistoryOp>,
    /// Page time of the last user activation (trusted click, key or selection).
    pub activated_at: Option<f64>,
}

impl Default for HistoryState {
    fn default() -> Self {
        Self {
            length: 1,
            index: 0,
            state: None,
            cache: Vec::new(),
            manual_scroll: false,
            ops: Vec::new(),
            activated_at: None,
        }
    }
}

impl HistoryState {
    /// Whether the page has a user activation now.
    pub fn activated(&self, now: f64) -> bool {
        self.activated_at.is_some_and(|t| now - t <= ACTIVATION_MS)
    }
}

/// The serializer's delegate: records why a value cannot be cloned.
struct Delegate {
    error: Rc<RefCell<Option<String>>>,
}

impl v8::ValueSerializerImpl for Delegate {
    fn throw_data_clone_error<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        message: v8::Local<'s, v8::String>,
    ) {
        *self.error.borrow_mut() = Some(message.to_rust_string_lossy(scope));
        let e = v8::Exception::error(scope, message);
        scope.throw_exception(e);
    }

    // DOM objects (wrappers with internal fields) are not cloneable here.
    fn write_host_object<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        _object: v8::Local<'s, v8::Object>,
        _value_serializer: &dyn v8::ValueSerializerHelper,
    ) -> Option<bool> {
        let message = "a platform object could not be cloned";
        *self.error.borrow_mut() = Some(message.into());
        let m = v8::String::new(scope, message)?;
        let e = v8::Exception::error(scope, m);
        scope.throw_exception(e);
        None
    }
}

struct Reader;

impl v8::ValueDeserializerImpl for Reader {}

/// Serializes a value as `structuredClone` does (`DataCloneError` for functions, symbols, DOM
/// objects; exceptions of getters are rethrown).
pub(crate) fn serialize(cx: &mut Cx<'_, '_, '_>, v: &Value) -> R<Vec<u8>> {
    let error = Rc::new(RefCell::new(None));
    let local = cx.local(v);
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    let context = tc.get_current_context();
    let ser = v8::ValueSerializer::new(
        tc,
        Box::new(Delegate {
            error: error.clone(),
        }),
    );
    ser.write_header();
    let ok = ser.write_value(context, local) == Some(true);
    let bytes = ser.release();
    drop(ser);
    if ok {
        return Ok(bytes);
    }
    if tc.is_execution_terminating() {
        return Err(Error::Terminated);
    }
    let why = error.borrow_mut().take();
    match (why, tc.exception()) {
        (Some(why), _) => Err(Error::dom(
            "DataCloneError",
            format!("Failed to execute 'structuredClone': {why}."),
        )),
        (None, Some(ex)) => Err(Error::Thrown(v8::Global::new(tc, ex))),
        (None, None) => Err(Error::dom(
            "DataCloneError",
            "the value could not be cloned",
        )),
    }
}

/// Deserializes a value in the current context.
pub(crate) fn deserialize(cx: &mut Cx<'_, '_, '_>, bytes: &[u8]) -> R<Value> {
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    let context = tc.get_current_context();
    let de = v8::ValueDeserializer::new(tc, Box::new(Reader), bytes);
    let v = de
        .read_header(context)
        .filter(|ok| *ok)
        .and_then(|_| de.read_value(context));
    match v {
        Some(v) => Ok(v8::Global::new(tc, v)),
        None if tc.is_execution_terminating() => Err(Error::Terminated),
        None => Err(Error::dom("DataCloneError", "the value could not be read")),
    }
}

/// `history.state` for this world: the current state, deserialized once per state and world.
fn current_state(cx: &mut Cx<'_, '_, '_>) -> R<Value> {
    let (state, world) = (cx.st().history.state.clone(), cx.world);
    let Some(state) = state else {
        let n = v8::null(cx.scope);
        return Ok(cx.keep(n.into()));
    };
    let key = Rc::as_ptr(&state) as usize;
    let hit = cx
        .st()
        .history
        .cache
        .iter()
        .find(|(w, k, _)| *w == world && *k == key)
        .map(|(_, _, v)| v.clone());
    if let Some(v) = hit {
        return Ok(v);
    }
    let v = deserialize(cx, &state)?;
    let mut st = cx.st();
    st.history
        .cache
        .retain(|(w, k, _)| *w != world && *k == key);
    st.history.cache.push((world, key, v.clone()));
    Ok(v)
}

/// Whether the document's URL may be rewritten to `target` (HTML: same origin, credentials and
/// port; for `file:` the same path; for other schemes only the query and fragment may change).
fn rewritable(current: &url::Url, target: &url::Url) -> bool {
    if current.scheme() != target.scheme()
        || current.username() != target.username()
        || current.password() != target.password()
        || current.host_str() != target.host_str()
        || current.port_or_known_default() != target.port_or_known_default()
    {
        return false;
    }
    match target.scheme() {
        "http" | "https" => true,
        "file" => current.path() == target.path(),
        _ => current[..url::Position::AfterPath] == target[..url::Position::AfterPath],
    }
}

fn now(cx: &Cx<'_, '_, '_>) -> f64 {
    let sh = cx.realm.shared.borrow();
    sh.now - sh.origin
}

/// `pushState`/`replaceState`.
fn change(cx: &mut Cx<'_, '_, '_>, data: &Value, url: Option<String>, push: bool) -> R<()> {
    let method = if push { "pushState" } else { "replaceState" };
    let current = cx.st().url.clone();
    let base = url::Url::parse(&current)
        .map_err(|_| Error::dom("SecurityError", "the document has no URL"))?;
    let target = match url {
        None => base.clone(),
        Some(u) => base.join(&u).map_err(|_| {
            Error::dom(
                "SecurityError",
                format!("Failed to execute '{method}' on 'History': '{u}' is not a valid URL."),
            )
        })?,
    };
    if !rewritable(&base, &target) {
        return Err(Error::dom(
            "SecurityError",
            format!(
                "Failed to execute '{method}' on 'History': A history state object with URL '{target}' cannot be created in a document with origin '{}' and URL '{current}'.",
                base.origin().ascii_serialization()
            ),
        ));
    }
    let bytes = serialize(cx, data)?;
    if bytes.len() > MAX_STATE {
        return Err(Error::dom(
            "DataCloneError",
            format!("Failed to execute '{method}' on 'History': the state is larger than 16 MiB."),
        ));
    }
    let state = Some(Rc::new(bytes));
    let activated = {
        let t = now(cx);
        cx.st().history.activated(t)
    };
    let mut st = cx.st();
    st.url = target.to_string();
    st.history.state.clone_from(&state);
    st.history.cache.clear();
    if push {
        let next = (st.history.index + 1).min(MAX_ENTRIES - 1);
        st.history.length = next + 1;
        st.history.index = next;
        st.history.ops.push(HistoryOp::Push {
            url: target.to_string(),
            state,
            activated,
        });
    } else {
        st.history.ops.push(HistoryOp::Replace {
            url: target.to_string(),
            state,
        });
    }
    Ok(())
}

/// A fragment navigation: a new entry of this document, with no state.
pub(crate) fn fragment(cx: &mut Cx<'_, '_, '_>, url: &str, replace: bool) {
    let t = now(cx);
    let mut st = cx.st();
    let activated = st.history.activated(t);
    st.history.state = None;
    st.history.cache.clear();
    if replace {
        st.history.ops.push(HistoryOp::Replace {
            url: url.into(),
            state: None,
        });
    } else {
        let next = (st.history.index + 1).min(MAX_ENTRIES - 1);
        st.history.length = next + 1;
        st.history.index = next;
        st.history.ops.push(HistoryOp::Push {
            url: url.into(),
            state: None,
            activated,
        });
    }
}

/// Moves this document to one of its entries (the tab's same-document traversal): its URL and
/// state, then `popstate` with the state, and `hashchange` if only the fragment changed.
pub(crate) fn restore(
    cx: &mut Cx<'_, '_, '_>,
    url: &str,
    state: Option<State>,
    index: u32,
    length: u32,
) -> R<()> {
    let old = std::mem::replace(&mut cx.st().url, url.to_string());
    {
        let mut st = cx.st();
        st.history.state = state;
        st.history.cache.clear();
        st.history.index = index;
        st.history.length = length;
    }
    let data = EventData {
        ty: "popstate".into(),
        initialized: true,
        trusted: true,
        time: now(cx),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::PopStateEvent, Slot::Event(Box::new(data)));
    crate::dom::event_target::dispatch(cx, Target::Window, e)?;
    let fragment = |u: &str| {
        url::Url::parse(u)
            .ok()
            .map(|u| u.fragment().map(str::to_string))
    };
    if fragment(&old) != fragment(url) {
        crate::dom::event_target::fire(cx, Target::Window, "hashchange", false, false)?;
    }
    Ok(())
}

pub mod history {
    use super::*;

    pub fn get_length(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<u32> {
        Ok(cx.st().history.length)
    }

    pub fn get_scroll_restoration(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<String> {
        Ok(if cx.st().history.manual_scroll {
            "manual"
        } else {
            "auto"
        }
        .into())
    }

    pub fn set_scroll_restoration(cx: &mut Cx<'_, '_, '_>, _this: Obj, v: String) -> R<()> {
        match v.as_str() {
            "auto" => cx.st().history.manual_scroll = false,
            "manual" => cx.st().history.manual_scroll = true,
            _ => {} // an invalid enum value is ignored
        }
        Ok(())
    }

    pub fn get_state(cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<Value> {
        current_state(cx)
    }

    /// `go(delta)`: `0` reloads; otherwise the tab traverses after this task.
    pub fn go(cx: &mut Cx<'_, '_, '_>, _this: Obj, delta: i32) -> R<()> {
        if delta == 0 {
            let url = cx.st().url.clone();
            cx.st().navigation = Some(crate::realm::NavRequest {
                url,
                method: "GET".into(),
                body: Vec::new(),
                content_type: None,
                replace: true,
            });
        } else {
            cx.st().history.ops.push(HistoryOp::Traverse(delta));
        }
        Ok(())
    }

    pub fn back(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        go(cx, this, -1)
    }

    pub fn forward(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<()> {
        go(cx, this, 1)
    }

    pub fn push_state(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        data: Value,
        _unused: String,
        url: Option<String>,
    ) -> R<()> {
        change(cx, &data, url, true)
    }

    pub fn replace_state(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        data: Value,
        _unused: String,
        url: Option<String>,
    ) -> R<()> {
        change(cx, &data, url, false)
    }
}

pub mod pop_state_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: PopStateEventInit) -> R<Obj> {
        let data = EventData {
            ty,
            bubbles: init.bubbles,
            cancelable: init.cancelable,
            composed: init.composed,
            initialized: true,
            time: now(cx),
            detail: init.state,
            ..EventData::default()
        };
        Ok(cx.alloc(Iface::PopStateEvent, Slot::Event(Box::new(data))))
    }

    /// A constructed event's `state`; a fired one's is `history.state` (in this world).
    pub fn get_state(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        let detail = match cx.realm.st.borrow().slot(this.slab()?) {
            Some(Slot::Event(e)) => e.detail.clone(),
            _ => return Err(Error::Type("Illegal invocation".into())),
        };
        match detail {
            Some(d) => Ok(d),
            None => current_state(cx),
        }
    }

    pub fn get_has_ua_visual_transition(_cx: &mut Cx<'_, '_, '_>, _this: Obj) -> R<bool> {
        Ok(false)
    }
}

/// `structuredClone(value, {transfer})`.
pub(crate) fn structured_clone(
    cx: &mut Cx<'_, '_, '_>,
    value: &Value,
    transfer: &[Value],
) -> R<Value> {
    let bytes = serialize_transfer(cx, value, transfer)?;
    deserialize(cx, &bytes)
}

/// Serializes `value` for another context, moving the `ArrayBuffer`s in `transfer`: their
/// bytes travel with the message and the sender's buffers are detached. Anything else in the
/// list is a `DataCloneError`.
pub(crate) fn serialize_transfer(
    cx: &mut Cx<'_, '_, '_>,
    value: &Value,
    transfer: &[Value],
) -> R<Vec<u8>> {
    let mut buffers: Vec<v8::Local<'_, v8::ArrayBuffer>> = Vec::with_capacity(transfer.len());
    for t in transfer {
        let v = cx.local(t);
        let Ok(b) = v8::Local::<v8::ArrayBuffer>::try_from(v) else {
            return Err(Error::dom(
                "DataCloneError",
                "Failed to execute 'postMessage': pageon transfers only ArrayBuffers.",
            ));
        };
        if !b.is_detachable() || buffers.iter().any(|o| o.strict_equals(b.into())) {
            return Err(Error::dom(
                "DataCloneError",
                "Failed to execute 'postMessage': an ArrayBuffer is detached or listed twice.",
            ));
        }
        buffers.push(b);
    }
    let bytes = serialize(cx, value)?;
    for b in buffers {
        let _ = b.detach(None);
    }
    Ok(bytes)
}
