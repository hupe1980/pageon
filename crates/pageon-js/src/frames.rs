//! Frames of a page: the tree its engines share, `postMessage`, and the windows of other frames.
//!
//! Documents of one origin are realms of one engine and reach each other's windows, documents
//! and nodes. Documents of other origins live in other engines: their windows are *remote
//! windows*, HTML's cross-origin `WindowProxy`. These offer `postMessage`, `window`, `self`,
//! `frames`, `length`, `top`, `parent`, `opener`, `closed`, `close`, `focus`, `blur`,
//! `location` (writes refused) and indexed child windows. Any other access throws
//! `SecurityError`. A remote window holds a context id, never an object of the other engine.
//!
//! Messages wait in the tree's mailbox. The engine running the target delivers them in its
//! event loop, checking the target's origin at that point.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use pageon_dom::NodeId;

use crate::cv::{self, Cx, Error, Obj, Part, R, Value};
use crate::generated::{Iface, MessageEventInit};
use crate::realm::{EventData, Realm, Slot, Target};

/// Most messages waiting at once in a page; further posts are dropped with a page error.
pub const MAX_MAIL: usize = 1024;

/// A document in a page's frame tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameEntry {
    /// The embedding document's context (`None` for the top-level document).
    pub parent: Option<u32>,
    /// The `<iframe>` in the parent's document.
    pub owner: Option<NodeId>,
    /// Serialized origin (`about:` documents have their parent's).
    pub origin: String,
    /// The engine running it (0: none: it runs no scripts).
    pub engine: u64,
    /// Its realm in that engine.
    pub realm: Option<usize>,
}

/// A message on its way.
#[derive(Debug, Clone)]
pub struct Message {
    /// The sender's context.
    pub from: u32,
    /// The target's context.
    pub to: u32,
    /// The sender's origin (`event.origin`).
    pub origin: String,
    /// The origin the target must have (`None`: any).
    pub target_origin: Option<String>,
    /// The structured clone.
    pub data: Vec<u8>,
}

/// A page's frame tree: every document by context id (0 is the top-level document), and the
/// messages waiting for delivery.
#[derive(Debug, Default)]
pub struct Frames {
    /// Documents by context id.
    pub entries: Vec<FrameEntry>,
    /// Messages in posting order.
    pub mailbox: VecDeque<Message>,
    /// The mailbox overflowed (one page error per overflow).
    full: bool,
}

impl Frames {
    /// A tree holding the top-level document.
    pub fn new(origin: &str) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            entries: vec![FrameEntry {
                parent: None,
                owner: None,
                origin: origin.to_string(),
                engine: 0,
                realm: None,
            }],
            mailbox: VecDeque::new(),
            full: false,
        }))
    }

    /// Adds a document; returns its context id.
    pub fn add(&mut self, entry: FrameEntry) -> u32 {
        self.entries.push(entry);
        u32::try_from(self.entries.len() - 1).unwrap_or(u32::MAX)
    }

    /// Records the engine and realm that run a document.
    pub fn run_by(&mut self, context: u32, engine: u64, realm: usize) {
        if let Some(e) = self.entries.get_mut(context as usize) {
            e.engine = engine;
            e.realm = Some(realm);
        }
    }

    /// The child documents of a context, in the document order of their `<iframe>`s.
    pub fn children(&self, context: u32) -> Vec<u32> {
        (0..self.entries.len())
            .filter(|&i| self.entries[i].parent == Some(context))
            .filter_map(|i| u32::try_from(i).ok())
            .collect()
    }

    /// The top-level document's context, seen from `context`.
    pub fn top(&self, mut context: u32) -> u32 {
        while let Some(p) = self.entries.get(context as usize).and_then(|e| e.parent) {
            context = p;
        }
        context
    }

    /// The engine that runs a context, if one does.
    pub fn engine_of(&self, context: u32) -> Option<u64> {
        self.entries
            .get(context as usize)
            .filter(|e| e.realm.is_some())
            .map(|e| e.engine)
    }

    /// Drops messages no engine will deliver (their target runs no scripts).
    pub fn drop_undeliverable(&mut self) {
        let entries = &self.entries;
        self.mailbox.retain(|m| {
            entries
                .get(m.to as usize)
                .is_some_and(|e| e.realm.is_some())
        });
    }

    /// Takes the messages for documents of `engine`, in order.
    fn take_for(&mut self, engine: u64) -> Vec<Message> {
        let (mine, rest): (VecDeque<Message>, VecDeque<Message>) =
            std::mem::take(&mut self.mailbox)
                .into_iter()
                .partition(|m| self.engine_of(m.to) == Some(engine));
        self.mailbox = rest;
        self.full &= self.mailbox.len() >= MAX_MAIL;
        mine.into()
    }
}

/// The tree and this document's context; a document alone gets a tree of its own.
fn frame_of(cx: &mut Cx<'_, '_, '_>) -> (Rc<RefCell<Frames>>, u32) {
    if let Some(f) = cx.st().frame.clone() {
        return f;
    }
    let origin = cx.st().origin.clone();
    let frames = Frames::new(&origin);
    let engine = cx.realm.shared.borrow().engine;
    frames.borrow_mut().run_by(0, engine, cx.realm.id);
    cx.st().frame = Some((frames.clone(), 0));
    (frames, 0)
}

/// The realm of `context`, if this engine runs it and the call is in the main world (isolated
/// worlds reach other frames only as remote windows).
fn local_realm(cx: &Cx<'_, '_, '_>, frames: &Frames, context: u32) -> Option<Rc<Realm>> {
    if cx.world != 0 {
        return None;
    }
    let e = frames.entries.get(context as usize)?;
    let sh = cx.realm.shared.borrow();
    if e.engine != sh.engine {
        return None;
    }
    sh.realms.get(e.realm?)?.upgrade()
}

/// The context of the frame an `<iframe>` element of this document holds.
fn child_of_owner(cx: &mut Cx<'_, '_, '_>, owner: NodeId) -> Option<u32> {
    let (frames, own) = frame_of(cx);
    let f = frames.borrow();
    f.children(own)
        .into_iter()
        .find(|&c| f.entries[c as usize].owner == Some(owner))
}

/// Most frames a document's scripts give `about:blank` documents.
const MAX_BLANK: usize = 16;

/// Whether an `<iframe>` gets its `about:blank` window from this engine: connected, with no
/// `srcdoc` and no `src` other than `about:blank`.
pub(crate) fn blank(dom: &pageon_dom::Dom, owner: NodeId) -> bool {
    let Some(n) = dom.get(owner) else {
        return false;
    };
    n.is_html("iframe")
        && dom.is_connected(owner)
        && n.attr("srcdoc").is_none()
        && n.attr("src").is_none_or(|s| {
            let s = s.trim();
            s.is_empty() || s.eq_ignore_ascii_case("about:blank")
        })
}

/// The frame of an `<iframe>` a script inserted, made at once as browsers make it: an empty
/// `about:blank` document of the parent's origin, in this engine (same origin, same engine).
pub(crate) fn blank_frame(cx: &mut Cx<'_, '_, '_>, owner: NodeId) -> Option<u32> {
    if cx.world != 0 || cx.realm.worker.is_some() || !blank(&cx.dom(), owner) {
        return None;
    }
    if let Some(c) = child_of_owner(cx, owner) {
        return Some(c);
    }
    let (frames, own) = frame_of(cx);
    if frames.borrow().children(own).len() >= MAX_BLANK {
        return None;
    }
    // A sandbox without `allow-same-origin` makes the frame's origin opaque: a window the
    // parent cannot reach into, with no document of this engine (scripts could not reach it
    // to run anything there).
    let opaque = cx
        .dom()
        .get(owner)
        .and_then(|n| n.attr("sandbox"))
        .is_some_and(|v| {
            !v.split_ascii_whitespace()
                .any(|t| t.eq_ignore_ascii_case("allow-same-origin"))
        })
        || cx.st().origin == "null";
    if opaque {
        let context = frames.borrow_mut().add(FrameEntry {
            parent: Some(own),
            owner: Some(owner),
            origin: "null".into(),
            engine: 0,
            realm: None,
        });
        cx.st().frames_changed = true;
        return Some(context);
    }
    let (cfg, ua_data, secure) = {
        let mut st = cx.st();
        let mut crypto_key = [0u8; 32];
        st.crypto.fill(&mut crypto_key);
        let secure = pageon_net::site::trustworthy(&st.client.url);
        let ua_data = st.navigator.ua_data.is_some() && secure;
        (
            crate::engine::RealmConfig {
                url: "about:blank".into(),
                origin: st.origin.clone(),
                navigator: st.navigator.clone(),
                viewport: st.viewport,
                stylesheets: Vec::new(),
                client: st.client.clone(),
                net: st.net.clone(),
                accept_dialogs: st.accept_dialogs,
                crypto_key,
            },
            ua_data,
            secure,
        )
    };
    let origin = cfg.origin.clone();
    let dom = pageon_dom::parse_document("", pageon_dom::ParseOptions { scripting: true });
    let id = cx.realm.shared.borrow().realms.len();
    let realm = crate::engine::new_realm(
        id,
        Rc::new(RefCell::new(dom)),
        cfg,
        cx.realm.templates.clone(),
        cx.realm.shared.clone(),
        None,
    );
    realm.st.borrow_mut().ready_state = "complete";
    let current = cx.scope.get_current_context();
    let mut token = Some(v8::Global::new(
        cx.scope,
        current.get_security_token(cx.scope),
    ));
    let (g, kept, key) = crate::engine::build_context(
        cx.scope,
        &cx.realm.templates,
        &mut token,
        &realm,
        0,
        ua_data,
        secure,
    );
    realm.worlds.borrow_mut().push(crate::realm::World {
        name: String::new(),
        context: kept,
        index: id,
    });
    let engine = {
        let mut sh = cx.realm.shared.borrow_mut();
        sh.realms.push(Rc::downgrade(&realm));
        sh.adopted.push((g, realm.clone(), key));
        sh.engine
    };
    let context = frames.borrow_mut().add(FrameEntry {
        parent: Some(own),
        owner: Some(owner),
        origin,
        engine,
        realm: Some(id),
    });
    realm.st.borrow_mut().frame = Some((frames, context));
    cx.st().frames_changed = true;
    Some(context)
}

/// Gives the `<iframe>`s scripts inserted their `about:blank` documents (if no script asked
/// for one yet) and fires `load` at them, after the task; `true` if any was handled.
pub(crate) fn deliver_blank_frames(cx: &mut Cx<'_, '_, '_>) -> bool {
    let me = cx.realm.id;
    let mine: Vec<NodeId> = {
        let mut sh = cx.realm.shared.borrow_mut();
        let (mine, rest): (Vec<_>, Vec<_>) = sh.blank_frames.drain(..).partition(|(r, _)| *r == me);
        sh.blank_frames = rest;
        mine.into_iter().map(|(_, n)| n).collect()
    };
    let mut any = false;
    for owner in mine {
        if blank_frame(cx, owner).is_some() {
            any = true;
            let _ = crate::dom::event_target::fire(cx, Target::Node(owner), "load", false, false);
        }
    }
    any
}

/// The wrapper of `obj` in another realm of this engine (its main world).
fn in_realm<'s>(
    cx: &mut Cx<'_, 's, '_>,
    realm: Rc<Realm>,
    obj: Obj,
) -> Option<v8::Local<'s, v8::Object>> {
    let ctx = realm.worlds.borrow().first().map(|w| w.context.clone())?;
    let context = v8::Local::new(cx.scope, &ctx);
    let kept = {
        let scope = &mut v8::ContextScope::new(cx.scope, context);
        let mut other = Cx {
            scope,
            realm,
            world: 0,
        };
        let o = other.wrap(obj);
        v8::Global::new(other.scope, o)
    };
    Some(v8::Local::new(cx.scope, &kept))
}

/// A node of another realm of this engine, as that realm wraps it.
pub(crate) fn wrap_foreign<'s>(
    cx: &mut Cx<'_, 's, '_>,
    realm: u32,
    node: NodeId,
) -> v8::Local<'s, v8::Object> {
    let other = cx
        .realm
        .shared
        .borrow()
        .realms
        .get(realm as usize)
        .and_then(std::rc::Weak::upgrade);
    other
        .and_then(|r| in_realm(cx, r, Obj::Node(node)))
        .unwrap_or_else(|| v8::Object::new(cx.scope))
}

/// The document of another realm of this engine (for `importNode`).
pub(crate) fn foreign_dom(cx: &Cx<'_, '_, '_>, realm: u32) -> Option<Rc<RefCell<pageon_dom::Dom>>> {
    let r = cx
        .realm
        .shared
        .borrow()
        .realms
        .get(realm as usize)?
        .upgrade()?;
    Some(r.dom.clone())
}

/// What `Obj::Frame` stands for, as this realm and world see it.
pub(crate) fn wrap<'s>(
    cx: &mut Cx<'_, 's, '_>,
    context: u32,
    part: Part,
) -> v8::Local<'s, v8::Object> {
    let (frames, own) = frame_of(cx);
    let found = match part {
        Part::Window => Some(window(cx, &frames, own, context)),
        Part::Document => {
            let realm = local_realm(cx, &frames.borrow(), context);
            realm.and_then(|r| {
                let root = r.dom.borrow().root();
                in_realm(cx, r, Obj::Node(root))
            })
        }
        Part::Owner => {
            let (parent, owner) = {
                let f = frames.borrow();
                let e = f.entries.get(context as usize);
                (e.and_then(|e| e.parent), e.and_then(|e| e.owner))
            };
            let realm = parent.and_then(|p| local_realm(cx, &frames.borrow(), p));
            realm
                .zip(owner)
                .and_then(|(r, n)| in_realm(cx, r, Obj::Node(n)))
        }
    };
    found.unwrap_or_else(|| v8::Object::new(cx.scope))
}

/// A window: this one, another realm's of this engine, or a remote window.
fn window<'s>(
    cx: &mut Cx<'_, 's, '_>,
    frames: &Rc<RefCell<Frames>>,
    own: u32,
    context: u32,
) -> v8::Local<'s, v8::Object> {
    if context == own {
        return cx.scope.get_current_context().global(cx.scope);
    }
    let local = local_realm(cx, &frames.borrow(), context);
    if let Some(r) = local
        && let Some(ctx) = r.worlds.borrow().first().map(|w| w.context.clone())
    {
        let context = v8::Local::new(cx.scope, &ctx);
        return context.global(cx.scope);
    }
    remote(cx, context)
}

/// The cached object of a remote window (`member` `""`) or one of its members.
fn cached<'s>(
    cx: &mut Cx<'_, 's, '_>,
    context: u32,
    member: &'static str,
    make: impl FnOnce(&mut Cx<'_, 's, '_>) -> v8::Local<'s, v8::Object>,
) -> v8::Local<'s, v8::Object> {
    let key = (cx.world, context, member);
    let hit = cx.st().remote.get(&key).cloned();
    if let Some(g) = hit {
        return v8::Local::new(cx.scope, &g);
    }
    let o = make(cx);
    let g = v8::Global::new(cx.scope, o);
    cx.st().remote.insert(key, g);
    o
}

/// A remote window: a null-prototype object whose properties are intercepted.
fn remote<'s>(cx: &mut Cx<'_, 's, '_>, context: u32) -> v8::Local<'s, v8::Object> {
    cached(cx, context, "", |cx| {
        let t = v8::ObjectTemplate::new(cx.scope);
        t.set_internal_field_count(1);
        t.set_named_property_handler(
            v8::NamedPropertyHandlerConfiguration::new()
                .getter(remote_get)
                .setter(remote_set)
                .deleter(remote_delete)
                .definer(remote_define),
        );
        t.set_indexed_property_handler(
            v8::IndexedPropertyHandlerConfiguration::new()
                .getter(remote_index)
                .setter(remote_index_set)
                .deleter(remote_index_delete)
                .definer(remote_index_define),
        );
        holder_object(cx, &t, context)
    })
}

fn holder_object<'s>(
    cx: &mut Cx<'_, 's, '_>,
    t: &v8::Local<'_, v8::ObjectTemplate>,
    context: u32,
) -> v8::Local<'s, v8::Object> {
    let o = t
        .new_instance(cx.scope)
        .unwrap_or_else(|| v8::Object::new(cx.scope));
    let id = v8::Integer::new_from_unsigned(cx.scope, context);
    o.set_internal_field(0, id.into());
    let null = v8::null(cx.scope);
    o.set_prototype(cx.scope, null.into());
    o
}

/// The context a remote window (or its `location`) stands for, if `o` is one of this realm's.
pub(crate) fn remote_context(cx: &mut Cx<'_, '_, '_>, o: v8::Local<'_, v8::Object>) -> Option<u32> {
    let entries: Vec<(u32, v8::Global<v8::Object>)> = cx
        .st()
        .remote
        .iter()
        .filter(|((w, _, m), _)| *w == cx.world && m.is_empty())
        .map(|((_, c, _), g)| (*c, g.clone()))
        .collect();
    entries
        .into_iter()
        .find(|(_, g)| v8::Local::new(cx.scope, g).strict_equals(o.into()))
        .map(|(c, _)| c)
}

/// The context id in a remote object's internal field.
fn holder_context(
    scope: &mut v8::PinScope<'_, '_>,
    holder: v8::Local<'_, v8::Object>,
) -> Option<u32> {
    if holder.internal_field_count() < 1 {
        return None;
    }
    let d = holder.get_internal_field(scope, 0)?;
    let v = v8::Local::<v8::Value>::try_from(d).ok()?;
    v.uint32_value(scope)
}

/// Runs `f` in the accessing realm.
fn with_realm<T>(
    scope: &mut v8::PinScope<'_, '_>,
    f: impl FnOnce(&mut Cx<'_, '_, '_>) -> T,
) -> Option<T> {
    let realm = crate::realm::current(scope)?;
    let world = crate::realm::current_world(scope);
    let mut cx = Cx {
        scope,
        realm,
        world,
    };
    Some(f(&mut cx))
}

fn security_error(cx: &mut Cx<'_, '_, '_>) {
    let origin = cx.st().origin.clone();
    cv::throw(
        cx,
        Error::dom(
            "SecurityError",
            format!(
                "Blocked a frame with origin \"{origin}\" from accessing a cross-origin frame."
            ),
        ),
    );
}

fn navigation_refused(cx: &mut Cx<'_, '_, '_>) {
    cv::throw(
        cx,
        Error::dom("NotSupportedError", "navigating a frame is not supported"),
    );
}

fn key_name(scope: &mut v8::PinScope<'_, '_>, key: v8::Local<'_, v8::Name>) -> Option<String> {
    (!key.is_symbol()).then(|| key.to_rust_string_lossy(scope))
}

fn remote_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    // Symbols (`@@toStringTag`, `@@hasInstance`, …) read as undefined: the prototype is null.
    let Some(name) = key_name(scope, key) else {
        return v8::Intercepted::kNo;
    };
    // `then` reads as undefined, so a remote window can resolve a promise.
    if name == "then" {
        return v8::Intercepted::kNo;
    }
    let holder = args.holder();
    let Some(context) = holder_context(scope, holder) else {
        return v8::Intercepted::kNo;
    };
    with_realm(scope, |cx| {
        let v: v8::Local<'_, v8::Value> = match name.as_str() {
            "window" | "self" | "frames" => holder.into(),
            "length" => {
                let (frames, _) = frame_of(cx);
                let n = frames.borrow().children(context).len();
                v8::Integer::new_from_unsigned(cx.scope, u32::try_from(n).unwrap_or(0)).into()
            }
            "top" | "parent" => {
                let (frames, _) = frame_of(cx);
                let target = {
                    let f = frames.borrow();
                    if name == "top" {
                        f.top(context)
                    } else {
                        f.entries
                            .get(context as usize)
                            .and_then(|e| e.parent)
                            .unwrap_or(context)
                    }
                };
                wrap(cx, target, Part::Window).into()
            }
            "opener" => v8::null(cx.scope).into(),
            "closed" => v8::Boolean::new(cx.scope, false).into(),
            "postMessage" => member_fn(cx, context, "postMessage", remote_post).into(),
            "close" => member_fn(cx, context, "close", remote_noop).into(),
            "focus" => member_fn(cx, context, "focus", remote_noop).into(),
            "blur" => member_fn(cx, context, "blur", remote_noop).into(),
            "location" => remote_location(cx, context).into(),
            _ => {
                security_error(cx);
                return;
            }
        };
        rv.set(v8::Local::new(&*cx.scope, v));
    });
    v8::Intercepted::kYes
}

fn remote_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    _value: v8::Local<'s, v8::Value>,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let name = key_name(scope, key);
    with_realm(scope, |cx| {
        if name.as_deref() == Some("location") {
            navigation_refused(cx);
        } else {
            security_error(cx);
        }
    });
    v8::Intercepted::kYes
}

fn remote_delete<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _key: v8::Local<'s, v8::Name>,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    with_realm(scope, security_error);
    v8::Intercepted::kYes
}

fn remote_define<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _key: v8::Local<'s, v8::Name>,
    _desc: &v8::PropertyDescriptor,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    with_realm(scope, security_error);
    v8::Intercepted::kYes
}

fn remote_index<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let Some(context) = holder_context(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    let found = with_realm(scope, |cx| {
        let (frames, _) = frame_of(cx);
        let child = frames
            .borrow()
            .children(context)
            .get(index as usize)
            .copied()?;
        let w = wrap(cx, child, Part::Window);
        rv.set(v8::Local::new(&*cx.scope, w).into());
        Some(())
    })
    .flatten();
    if found.is_some() {
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn remote_index_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _index: u32,
    _value: v8::Local<'s, v8::Value>,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    with_realm(scope, security_error);
    v8::Intercepted::kYes
}

fn remote_index_delete<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _index: u32,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    with_realm(scope, security_error);
    v8::Intercepted::kYes
}

fn remote_index_define<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _index: u32,
    _desc: &v8::PropertyDescriptor,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    with_realm(scope, security_error);
    v8::Intercepted::kYes
}

/// A remote window's `location`: `href` and `replace` exist; writing either navigates, which
/// frames do not do here; reading is a cross-origin access.
fn remote_location<'s>(cx: &mut Cx<'_, 's, '_>, context: u32) -> v8::Local<'s, v8::Object> {
    cached(cx, context, "location", |cx| {
        let t = v8::ObjectTemplate::new(cx.scope);
        t.set_internal_field_count(1);
        t.set_named_property_handler(
            v8::NamedPropertyHandlerConfiguration::new()
                .getter(location_get)
                .setter(location_set)
                .deleter(remote_delete)
                .definer(remote_define),
        );
        holder_object(cx, &t, context)
    })
}

fn location_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let Some(name) = key_name(scope, key) else {
        return v8::Intercepted::kNo;
    };
    if name == "then" {
        return v8::Intercepted::kNo;
    }
    let Some(context) = holder_context(scope, args.holder()) else {
        return v8::Intercepted::kNo;
    };
    with_realm(scope, |cx| {
        if name == "replace" {
            let f = member_fn(cx, context, "location.replace", location_replace);
            rv.set(v8::Local::new(&*cx.scope, f).into());
        } else {
            security_error(cx);
        }
    });
    v8::Intercepted::kYes
}

fn location_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    _value: v8::Local<'s, v8::Value>,
    _args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let name = key_name(scope, key);
    with_realm(scope, |cx| {
        if name.as_deref() == Some("href") {
            navigation_refused(cx);
        } else {
            security_error(cx);
        }
    });
    v8::Intercepted::kYes
}

fn location_replace(
    scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    _rv: v8::ReturnValue<v8::Value>,
) {
    with_realm(scope, navigation_refused);
}

/// A member function of a remote window, made once per (world, window, member).
fn member_fn<'s>(
    cx: &mut Cx<'_, 's, '_>,
    context: u32,
    member: &'static str,
    callback: impl v8::MapFnTo<v8::FunctionCallback>,
) -> v8::Local<'s, v8::Object> {
    cached(cx, context, member, |cx| {
        let data = v8::Integer::new_from_unsigned(cx.scope, context);
        v8::Function::builder(callback)
            .data(data.into())
            .build(cx.scope)
            .map_or_else(|| v8::Object::new(cx.scope), Into::into)
    })
}

fn remote_noop(
    _scope: &mut v8::PinScope<'_, '_>,
    _args: v8::FunctionCallbackArguments<'_>,
    _rv: v8::ReturnValue<v8::Value>,
) {
}

/// `postMessage(message, targetOrigin | {targetOrigin}, transfer)` on a remote window.
fn remote_post(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    _rv: v8::ReturnValue<v8::Value>,
) {
    let Some(to) = args.data().uint32_value(scope) else {
        return;
    };
    with_realm(scope, |cx| {
        let r = (|| {
            if args.length() < 1 {
                return Err(Error::args("Window.postMessage", 0));
            }
            let message = cx.keep(args.get(0));
            let second = args.get(1);
            let (target, transfer) = if second.is_object() && !second.is_string() {
                let o = cv::object(cx, second, "WindowPostMessageOptions")?;
                let target = cv::get(cx, o, "targetOrigin")?;
                let target = if target.is_undefined() {
                    "/".to_string()
                } else {
                    cv::string(cx, target)?
                };
                let transfer = cv::get(cx, o, "transfer")?;
                (target, transfer)
            } else if second.is_undefined() {
                ("/".to_string(), args.get(2))
            } else {
                (cv::string(cx, second)?, args.get(2))
            };
            let transfer = if transfer.is_null_or_undefined() {
                0
            } else {
                cv::seq(cx, transfer, |cx, v| Ok(cv::value(cx, v)))?.len()
            };
            post(cx, to, &message, &target, transfer)
        })();
        if let Err(e) = r {
            cv::throw(cx, e);
        }
    });
}

/// Queues a message from this document to context `to`.
pub(crate) fn post(
    cx: &mut Cx<'_, '_, '_>,
    to: u32,
    message: &Value,
    target_origin: &str,
    transfer: usize,
) -> R<()> {
    let (frames, own) = frame_of(cx);
    // The sender is the realm whose script calls (another realm of this engine when a script
    // posts through a frame's window), as browsers take the incumbent realm.
    let caller = crate::realm::entered(cx.scope)
        .filter(|r| !Rc::ptr_eq(r, &cx.realm) && Rc::ptr_eq(&r.shared, &cx.realm.shared));
    let (own, origin) = match caller.and_then(|r| {
        let st = r.st.borrow();
        st.frame
            .as_ref()
            .filter(|(f, _)| Rc::ptr_eq(f, &frames))
            .map(|(_, c)| (*c, st.origin.clone()))
    }) {
        Some(sender) => sender,
        None => (own, cx.st().origin.clone()),
    };
    let target = match target_origin {
        "*" => None,
        "/" => Some(origin.clone()),
        s => match url::Url::parse(s) {
            Ok(u) => Some(u.origin().ascii_serialization()),
            Err(_) => {
                return Err(Error::dom(
                    "SyntaxError",
                    format!("Invalid target origin '{s}' in a call to 'postMessage'."),
                ));
            }
        },
    };
    if transfer > 0 {
        return Err(Error::dom(
            "DataCloneError",
            "Transferring objects is not supported",
        ));
    }
    let data = crate::dom::session::serialize(cx, message)?;
    if data.len() > crate::dom::session::MAX_STATE {
        return Err(Error::dom(
            "DataCloneError",
            "the message is larger than 16 MiB",
        ));
    }
    let mut f = frames.borrow_mut();
    if f.mailbox.len() >= MAX_MAIL {
        let first = !std::mem::replace(&mut f.full, true);
        drop(f);
        if !first {
            return Ok(());
        }
        cx.realm.shared.borrow_mut().error(
            crate::PageErrorKind::Exception,
            format!(
                "postMessage: {MAX_MAIL} messages are already waiting; further messages are dropped"
            ),
            "postMessage",
        );
        return Ok(());
    }
    f.mailbox.push_back(Message {
        from: own,
        to,
        origin,
        target_origin: target,
        data,
    });
    Ok(())
}

impl crate::Engine {
    /// Delivers the messages waiting for this engine's documents, in order, as `message`
    /// events; a message whose target's origin does not match is dropped. Returns how many
    /// were taken.
    pub(crate) fn deliver_mail(&mut self) -> usize {
        let Some(frames) = self.frame_tree() else {
            return 0;
        };
        let id = self.id();
        let mine = frames.borrow_mut().take_for(id);
        let n = mine.len();
        for m in mine {
            let target = {
                let f = frames.borrow();
                f.entries.get(m.to as usize).and_then(|e| {
                    let matches = m.target_origin.as_ref().is_none_or(|t| *t == e.origin);
                    e.realm.filter(|_| matches)
                })
            };
            let Some(realm) = target else { continue };
            let (_, fired) = self.guarded(|e| {
                e.with_cx(realm, |cx| deliver(cx, &m));
            });
            self.terminated(fired, "message");
        }
        n
    }
}

/// Fires one message at this document's window.
fn deliver(cx: &mut Cx<'_, '_, '_>, m: &Message) {
    let Ok(data) = crate::dom::session::deserialize(cx, &m.data) else {
        return;
    };
    let own = frame_of(cx).1;
    let source = if m.from == own {
        Obj::Window
    } else {
        Obj::Frame(m.from, Part::Window)
    };
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
        message: Some(Box::new((m.origin.clone(), String::new(), Some(source)))),
        ..EventData::default()
    };
    let e = cx.alloc(Iface::MessageEvent, Slot::Event(Box::new(ev)));
    let _ = crate::dom::event_target::dispatch(cx, Target::Window, e);
}

/// The window's members that cross frames.
pub(crate) mod window {
    use super::*;

    pub fn top(cx: &mut Cx<'_, '_, '_>) -> Obj {
        let (frames, own) = frame_of(cx);
        let top = frames.borrow().top(own);
        if top == own {
            Obj::Window
        } else {
            Obj::Frame(top, Part::Window)
        }
    }

    pub fn parent(cx: &mut Cx<'_, '_, '_>) -> Obj {
        let (frames, own) = frame_of(cx);
        let parent = frames
            .borrow()
            .entries
            .get(own as usize)
            .and_then(|e| e.parent);
        match parent {
            Some(p) => Obj::Frame(p, Part::Window),
            None => Obj::Window,
        }
    }

    /// The loaded child frames, if the document is in a tree.
    pub fn children(cx: &mut Cx<'_, '_, '_>) -> Option<Vec<u32>> {
        let (frames, own) = cx.st().frame.clone()?;
        Some(frames.borrow().children(own))
    }

    pub fn frame_element(cx: &mut Cx<'_, '_, '_>) -> Option<Obj> {
        let (frames, own) = frame_of(cx);
        let parent = frames.borrow().entries.get(own as usize)?.parent?;
        local_realm(cx, &frames.borrow(), parent)?;
        Some(Obj::Frame(own, Part::Owner))
    }

    pub fn post_message(
        cx: &mut Cx<'_, '_, '_>,
        message: &Value,
        target_origin: &str,
        transfer: usize,
    ) -> R<()> {
        let own = frame_of(cx).1;
        post(cx, own, message, target_origin, transfer)
    }

    /// `window[i]`.
    pub fn indexed<'s>(
        scope: &mut v8::PinScope<'s, '_>,
        index: u32,
        _args: v8::PropertyCallbackArguments<'s>,
        mut rv: v8::ReturnValue<v8::Value>,
    ) -> v8::Intercepted {
        let found = with_realm(scope, |cx| {
            let child = children(cx)?.get(index as usize).copied()?;
            let w = wrap(cx, child, Part::Window);
            rv.set(v8::Local::new(&*cx.scope, w).into());
            Some(())
        })
        .flatten();
        if found.is_some() {
            v8::Intercepted::kYes
        } else {
            v8::Intercepted::kNo
        }
    }
}

/// `HTMLIFrameElement`: the frame's window and, within an engine, its document.
#[allow(missing_docs)]
pub mod htmli_frame_element {
    use super::*;

    pub fn get_content_window(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let node = this.node()?;
        Ok(child_of_owner(cx, node)
            .or_else(|| blank_frame(cx, node))
            .map(|c| Obj::Frame(c, Part::Window)))
    }

    pub fn get_content_document(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Obj>> {
        let node = this.node()?;
        let Some(child) = child_of_owner(cx, node).or_else(|| blank_frame(cx, node)) else {
            return Ok(None);
        };
        let (frames, _) = frame_of(cx);
        let reachable = local_realm(cx, &frames.borrow(), child).is_some();
        Ok(reachable.then_some(Obj::Frame(child, Part::Document)))
    }
}

/// `MessageEvent`: what `postMessage` delivers.
#[allow(missing_docs)]
pub mod message_event {
    use super::*;

    pub fn constructor(cx: &mut Cx<'_, '_, '_>, ty: String, init: MessageEventInit) -> R<Obj> {
        let source = match &init.source {
            Some(v) => {
                let v = cx.local(v);
                let global = cx.scope.get_current_context().global(cx.scope);
                if v.strict_equals(global.into()) {
                    Some(Obj::Window)
                } else if let Ok(o) = v8::Local::<v8::Object>::try_from(v)
                    && let Some(c) = remote_context(cx, o)
                {
                    Some(Obj::Frame(c, Part::Window))
                } else if v.is_null_or_undefined() {
                    None
                } else {
                    return Err(Error::Type(
                        "Failed to construct 'MessageEvent': The provided value is not a window."
                            .into(),
                    ));
                }
            }
            None => None,
        };
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
            detail: init.data,
            message: Some(Box::new((init.origin, init.last_event_id, source))),
            ..EventData::default()
        };
        Ok(cx.alloc(Iface::MessageEvent, Slot::Event(Box::new(data))))
    }

    fn fields<T>(cx: &mut Cx<'_, '_, '_>, this: Obj, f: impl FnOnce(&EventData) -> T) -> R<T> {
        match cx.realm.st.borrow().slot(this.slab()?) {
            Some(Slot::Event(e)) => Ok(f(e)),
            _ => Err(Error::Type("Illegal invocation".into())),
        }
    }

    pub fn get_data(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Value> {
        match fields(cx, this, |e| e.detail.clone())? {
            Some(v) => Ok(v),
            None => {
                let n = v8::null(cx.scope);
                Ok(cx.keep(n.into()))
            }
        }
    }

    pub fn get_origin(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        fields(cx, this, |e| {
            e.message.as_ref().map(|m| m.0.clone()).unwrap_or_default()
        })
    }

    pub fn get_last_event_id(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<String> {
        fields(cx, this, |e| {
            e.message.as_ref().map(|m| m.1.clone()).unwrap_or_default()
        })
    }

    pub fn get_source(cx: &mut Cx<'_, '_, '_>, this: Obj) -> R<Option<Value>> {
        let source = fields(cx, this, |e| e.message.as_ref().and_then(|m| m.2))?;
        Ok(source.map(|o| {
            let w: v8::Local<v8::Value> = cx.wrap(o).into();
            cx.keep(w)
        }))
    }
}
