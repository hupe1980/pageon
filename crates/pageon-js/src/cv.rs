//! The call context and the WebIDL ↔ V8 conversions used by the generated bindings.

use std::cell::{Ref, RefMut};
use std::rc::Rc;

use pageon_dom::{Dom, NodeId};

use crate::generated::Iface;
use crate::realm::{Realm, Slot, State, Target};

/// A JavaScript value kept beyond a callback.
pub type Value = v8::Global<v8::Value>;
/// A binding result.
pub type R<T> = Result<T, Error>;
/// Internal fields of every wrapper: interface, index (node or slab), generation.
pub const FIELDS: usize = 3;

/// What a wrapper stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Obj {
    /// A DOM node of the realm's document.
    Node(NodeId),
    /// The realm's global object.
    Window,
    /// A non-node object (event, collection, style declaration, …) in the realm's slab.
    Slab(u32),
    /// Another frame's window, document or `<iframe>`, by context id (see [`crate::frames`]).
    Frame(u32, Part),
    /// A node of another document of this engine (realm, node). Only `importNode` takes it;
    /// everything else refuses it.
    Foreign(u32, NodeId),
}

/// What of another frame an [`Obj::Frame`] stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Part {
    /// Its window (the realm's global, or a remote window).
    Window,
    /// Its document (same engine only).
    Document,
    /// Its `<iframe>` element in the parent (same engine only).
    Owner,
}

impl Obj {
    /// The node, or a `TypeError`.
    pub fn node(self) -> R<NodeId> {
        match self {
            Obj::Node(n) => Ok(n),
            Obj::Foreign(..) => Err(Error::dom("NotSupportedError", FOREIGN)),
            _ => Err(Error::Type("not a node".into())),
        }
    }

    /// The slab id, or a `TypeError`.
    pub fn slab(self) -> R<u32> {
        match self {
            Obj::Slab(s) => Ok(s),
            _ => Err(Error::Type("illegal invocation".into())),
        }
    }
}

/// Why a node of another document is refused.
pub const FOREIGN: &str =
    "The node belongs to another document; use document.importNode to copy it.";

/// A binding failure, thrown into JavaScript.
#[derive(Debug)]
pub enum Error {
    /// `TypeError`.
    Type(String),
    /// `RangeError`.
    Range(String),
    /// A `DOMException` with its name.
    Dom(&'static str, String),
    /// A JavaScript exception to rethrow.
    Thrown(Value),
    /// Execution is being terminated (time or memory limit): unwind silently.
    Terminated,
}

impl Error {
    /// Missing required argument `i` of `ctx` (`Interface.member`).
    pub fn args(ctx: &str, i: i32) -> Self {
        let (iface, member) = ctx.split_once('.').unwrap_or((ctx, ""));
        let n = i + 1;
        Error::Type(format!(
            "Failed to execute '{member}' on '{iface}': {n} argument{} required, but only {i} present.",
            if n == 1 { "" } else { "s" }
        ))
    }

    /// A `DOMException`.
    pub fn dom(name: &'static str, msg: impl Into<String>) -> Self {
        Error::Dom(name, msg.into())
    }
}

/// A value returned to JavaScript, built without V8 lifetimes.
#[derive(Debug)]
pub enum Out {
    /// `undefined`.
    Undefined,
    /// `null`.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number.
    Num(f64),
    /// A string.
    Str(String),
    /// A wrapped object.
    Obj(Obj),
    /// A JavaScript value.
    Value(Value),
    /// An array.
    List(Vec<Out>),
}

/// Rust values that convert to [`Out`].
pub trait IntoOut {
    /// The conversion.
    fn into_out(self) -> Out;
}

macro_rules! num_out {
    ($($t:ty),*) => {$(impl IntoOut for $t { fn into_out(self) -> Out { Out::Num(self as f64) } })*};
}
num_out!(i8, u8, i16, u16, i32, u32, i64, u64, f64);

impl IntoOut for () {
    fn into_out(self) -> Out {
        Out::Undefined
    }
}
impl IntoOut for bool {
    fn into_out(self) -> Out {
        Out::Bool(self)
    }
}
impl IntoOut for String {
    fn into_out(self) -> Out {
        Out::Str(self)
    }
}
impl IntoOut for Obj {
    fn into_out(self) -> Out {
        Out::Obj(self)
    }
}
impl IntoOut for Value {
    fn into_out(self) -> Out {
        Out::Value(self)
    }
}
impl IntoOut for Out {
    fn into_out(self) -> Out {
        self
    }
}
impl<T: IntoOut> IntoOut for Option<T> {
    fn into_out(self) -> Out {
        self.map_or(Out::Null, IntoOut::into_out)
    }
}
impl<T: IntoOut> IntoOut for Vec<T> {
    fn into_out(self) -> Out {
        Out::List(self.into_iter().map(IntoOut::into_out).collect())
    }
}

/// `(Node or DOMString)` arguments.
#[derive(Debug)]
pub enum NodeOrString {
    /// A node.
    Node(Obj),
    /// A string (becomes a text node).
    Str(String),
}

/// `(SomeOptions or boolean)` arguments.
#[derive(Debug)]
pub enum OrBool<D> {
    /// The dictionary.
    Dict(D),
    /// The boolean.
    Bool(bool),
}

/// How a `[Reflect]` attribute maps its content attribute.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)] // all kinds the generator can emit
pub enum Reflect {
    /// `DOMString`: the value, or `""`.
    Str,
    /// `DOMString?`: the value, or `null`.
    NullableStr,
    /// `USVString` URL: resolved against the document URL.
    Url,
    /// `boolean`: presence.
    Bool,
    /// `long` with a default.
    Long(i64),
    /// `long` limited to non-negative numbers (`maxLength`): -1 when absent or negative.
    NonNegative,
    /// `unsigned long` with a default.
    Unsigned(u64),
    /// `double` with a default.
    Double(f64),
}

/// The context of one call from JavaScript into the engine.
pub struct Cx<'a, 's, 'i> {
    /// The V8 scope.
    pub scope: &'a mut v8::PinScope<'s, 'i>,
    /// The realm (document, wrappers, listeners).
    pub realm: Rc<Realm>,
    /// The world the call runs in (0: the page's main world; others are isolated worlds of
    /// protocol clients, with their own wrappers).
    pub world: u32,
}

impl<'s> Cx<'_, 's, '_> {
    /// The realm's document.
    pub fn dom(&self) -> Ref<'_, Dom> {
        self.realm.dom.borrow()
    }

    /// The realm's document, for changes (counts a mutation, so layout is recomputed).
    pub fn dom_mut(&self) -> RefMut<'_, Dom> {
        self.realm.st.borrow_mut().mutations += 1;
        self.realm.dom.borrow_mut()
    }

    /// The realm's document, for changes that do not affect layout (live ranges, the
    /// selection, a text field's caret): no relayout follows.
    pub fn dom_quiet(&self) -> RefMut<'_, Dom> {
        self.realm.dom.borrow_mut()
    }

    /// The realm's state.
    pub fn st(&self) -> RefMut<'_, State> {
        self.realm.st.borrow_mut()
    }

    /// Converts and returns a value.
    pub fn ret<T: IntoOut>(&mut self, v: T) -> R<Out> {
        Ok(v.into_out())
    }

    /// Returns what a constructor made, with the prototype of `new.target` when a class
    /// extends the interface (`class Bus extends EventTarget`).
    pub fn constructed(&mut self, args: &v8::FunctionCallbackArguments, obj: Obj) -> R<Out> {
        if let Ok(nt) = v8::Local::<v8::Object>::try_from(args.new_target()) {
            let o = self.wrap(obj);
            let key = self.str("prototype");
            if let Some(proto) = nt.get(self.scope, key.into()).filter(|p| p.is_object())
                && o.get_prototype(self.scope)
                    .is_none_or(|current| !current.strict_equals(proto))
            {
                let _ = o.set_prototype(self.scope, proto);
            }
        }
        self.ret(obj)
    }

    /// Returns an already converted value.
    pub fn ret_value(&mut self, v: Out) -> R<Out> {
        Ok(v)
    }

    /// A V8 string.
    pub fn str(&self, s: &str) -> v8::Local<'s, v8::String> {
        v8::String::new(self.scope, s).unwrap_or_else(|| v8::String::empty(self.scope))
    }

    /// A local handle for a kept value.
    pub fn local(&self, v: &Value) -> v8::Local<'s, v8::Value> {
        v8::Local::new(self.scope, v)
    }

    /// Keeps a value beyond this call.
    pub fn keep(&self, v: v8::Local<'_, v8::Value>) -> Value {
        v8::Global::new(self.scope, v)
    }

    /// The receiver of a call, checked against the expected interface.
    pub fn this(&mut self, args: &v8::FunctionCallbackArguments, iface: Iface) -> R<Obj> {
        let this = args.this();
        let global = self.scope.get_current_context().global(self.scope);
        if this.strict_equals(global.into()) {
            let own = if self.realm.worker.is_some() {
                Iface::DedicatedWorkerGlobalScope
            } else {
                Iface::Window
            };
            return if own.is_a(iface) {
                Ok(Obj::Window)
            } else {
                Err(illegal())
            };
        }
        unwrap_obj(self, this.into(), iface)
    }

    /// The wrapper of an object (created once, then the same object every time).
    pub fn wrap(&mut self, obj: Obj) -> v8::Local<'s, v8::Object> {
        match obj {
            Obj::Window => self.scope.get_current_context().global(self.scope),
            Obj::Frame(context, part) => crate::frames::wrap(self, context, part),
            Obj::Foreign(realm, node) => crate::frames::wrap_foreign(self, realm, node),
            Obj::Node(id) => {
                if let Some(w) = self.realm.st.borrow().wrappers.get(&(self.world, id)) {
                    return v8::Local::new(self.scope, w);
                }
                let iface = crate::dom::node::iface_of(&self.dom(), id);
                let o = self.instance(iface, id.index(), id.generation());
                let g = v8::Global::new(self.scope, o);
                self.realm
                    .st
                    .borrow_mut()
                    .wrappers
                    .insert((self.world, id), g);
                o
            }
            Obj::Slab(i) => {
                let (iface, existing) = {
                    let st = self.realm.st.borrow();
                    let e = &st.slab[i as usize];
                    let w = e
                        .wrappers
                        .iter()
                        .find(|(w, _)| *w == self.world)
                        .map(|(_, g)| g.clone());
                    (e.iface, w)
                };
                if let Some(w) = existing {
                    return v8::Local::new(self.scope, &w);
                }
                let o = self.instance(iface, i, 0);
                let g = v8::Global::new(self.scope, o);
                self.realm.st.borrow_mut().slab[i as usize]
                    .wrappers
                    .push((self.world, g));
                o
            }
        }
    }

    /// Runs `f` in another world of this realm: its context is entered, so wrappers, arrays and
    /// errors made by `f` are that world's.
    pub fn in_world<T>(&mut self, world: u32, f: impl FnOnce(&mut Cx<'_, '_, '_>) -> T) -> T {
        if world == self.world {
            return f(self);
        }
        let ctx = self
            .realm
            .worlds
            .borrow()
            .get(world as usize)
            .map(|w| w.context.clone());
        let Some(ctx) = ctx else {
            return f(self);
        };
        let context = v8::Local::new(self.scope, &ctx);
        let scope = &mut v8::ContextScope::new(self.scope, context);
        let mut cx = Cx {
            scope,
            realm: self.realm.clone(),
            world,
        };
        f(&mut cx)
    }

    /// The world a function or object was created in (0 if it is not a realm's).
    pub fn world_of(&mut self, v: &Value) -> u32 {
        let v = self.local(v);
        match v8::Local::<v8::Object>::try_from(v) {
            Ok(o) => self.world_of_object(o),
            Err(_) => self.world,
        }
    }

    /// The world an object was created in.
    pub fn world_of_object(&mut self, o: v8::Local<'_, v8::Object>) -> u32 {
        match o.get_creation_context(self.scope) {
            Some(c) => {
                let g = c.global(self.scope);
                crate::realm::world_of_global(self.scope, g)
            }
            None => self.world,
        }
    }

    fn instance(&mut self, iface: Iface, a: u32, b: u32) -> v8::Local<'s, v8::Object> {
        let t = v8::Local::new(self.scope, &self.realm.templates[iface as usize]);
        let inst = t.instance_template(self.scope);
        let o = inst
            .new_instance(self.scope)
            .unwrap_or_else(|| v8::Object::new(self.scope));
        let tag = v8::Integer::new(self.scope, i32::from(iface as u16));
        o.set_internal_field(0, tag.into());
        let ia = v8::Integer::new_from_unsigned(self.scope, a);
        o.set_internal_field(1, ia.into());
        let ib = v8::Integer::new_from_unsigned(self.scope, b);
        o.set_internal_field(2, ib.into());
        o
    }

    /// Adds a non-node object to the realm and returns its handle.
    pub fn alloc(&mut self, iface: Iface, slot: Slot) -> Obj {
        Obj::Slab(self.realm.st.borrow_mut().alloc(iface, slot))
    }

    /// Converts a returned value to JavaScript.
    pub fn js_value(&mut self, out: Out) -> v8::Local<'s, v8::Value> {
        match out {
            Out::Undefined => v8::undefined(self.scope).into(),
            Out::Null => v8::null(self.scope).into(),
            Out::Bool(b) => v8::Boolean::new(self.scope, b).into(),
            Out::Num(n) => v8::Number::new(self.scope, n).into(),
            Out::Str(s) => self.str(&s).into(),
            Out::Obj(o) => self.wrap(o).into(),
            Out::Value(v) => self.local(&v),
            Out::List(items) => {
                let values: Vec<v8::Local<v8::Value>> =
                    items.into_iter().map(|i| self.js_value(i)).collect();
                v8::Array::new_with_elements(self.scope, &values).into()
            }
        }
    }

    /// Calls a function value (or an object's `handleEvent`) with `this`; an exception is
    /// reported as a page error and the call returns `None`.
    pub fn invoke(
        &mut self,
        f: &Value,
        this: v8::Local<'s, v8::Value>,
        args: &[v8::Local<'s, v8::Value>],
    ) -> R<Option<Value>> {
        let fv = self.local(f);
        let (func, recv) = if fv.is_function() {
            (v8::Local::<v8::Function>::try_from(fv).ok(), this)
        } else if let Ok(o) = v8::Local::<v8::Object>::try_from(fv) {
            let key = self.str("handleEvent");
            let h = o.get(self.scope, key.into());
            (
                h.and_then(|h| v8::Local::<v8::Function>::try_from(h).ok()),
                o.into(),
            )
        } else {
            (None, this)
        };
        let Some(func) = func else { return Ok(None) };
        let scope = &mut *self.scope;
        v8::tc_scope!(let tc, scope);
        let r = func.call(tc, recv, args);
        if tc.has_terminated() || tc.is_execution_terminating() {
            return Err(Error::Terminated);
        }
        if let Some(ex) = tc.exception() {
            let msg = tc.message();
            let err = crate::engine::page_error(tc, ex, msg);
            self.realm.shared.borrow_mut().errors.push(err);
            return Ok(None);
        }
        let out = r.map(|v| v8::Global::new(tc, v));
        Ok(out)
    }
}

fn illegal() -> Error {
    Error::Type("Illegal invocation".into())
}

/// The object behind a wrapper, checked against an interface.
pub fn unwrap(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>, iface: Iface) -> R<Obj> {
    unwrap_obj(cx, v, iface)
}

/// The object behind any wrapper, with its interface.
pub fn unwrap_any(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> Option<(Obj, Iface)> {
    let o = v8::Local::<v8::Object>::try_from(v).ok()?;
    if o.internal_field_count() < FIELDS {
        return None;
    }
    let d = o.get_internal_field(cx.scope, 0)?;
    let tag = v8::Local::<v8::Value>::try_from(d)
        .ok()?
        .uint32_value(cx.scope)?;
    let iface = Iface::from_index(u16::try_from(tag).ok()?)?;
    unwrap_obj(cx, v, iface).ok().map(|obj| (obj, iface))
}

pub(crate) fn unwrap_obj(
    cx: &mut Cx<'_, '_, '_>,
    v: v8::Local<'_, v8::Value>,
    iface: Iface,
) -> R<Obj> {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
        return Err(illegal());
    };
    if o.internal_field_count() < FIELDS {
        let global = cx.scope.get_current_context().global(cx.scope);
        if o.strict_equals(global.into()) && Iface::Window.is_a(iface) {
            return Ok(Obj::Window);
        }
        return Err(illegal());
    }
    let field = |i: usize| -> Option<u32> {
        let d = o.get_internal_field(cx.scope, i)?;
        let v = v8::Local::<v8::Value>::try_from(d).ok()?;
        v.uint32_value(cx.scope)
    };
    let (Some(tag), Some(a), Some(b)) = (field(0), field(1), field(2)) else {
        return Err(illegal());
    };
    let Some(actual) = u16::try_from(tag).ok().and_then(Iface::from_index) else {
        return Err(illegal());
    };
    if !actual.is_a(iface) {
        return Err(illegal());
    }
    // A wrapper of another document of this engine: its slot means nothing here.
    if let Some(c) = o.get_creation_context(cx.scope)
        && c != cx.scope.get_current_context()
        && let Some(other) = crate::realm::of_global(cx.scope, c.global(cx.scope))
        && other != cx.realm.id
    {
        if actual.is_a(Iface::Node) && actual != Iface::Attr {
            return Ok(Obj::Foreign(
                u32::try_from(other).unwrap_or(u32::MAX),
                NodeId::from_parts(a, b),
            ));
        }
        return Err(Error::dom("NotSupportedError", FOREIGN));
    }
    // `Attr` is a `Node` in the IDL, but attributes are not nodes of pageon's document model.
    if actual.is_a(Iface::Node) && actual != Iface::Attr {
        Ok(Obj::Node(NodeId::from_parts(a, b)))
    } else {
        Ok(Obj::Slab(a))
    }
}

/// Throws a binding error into JavaScript.
pub fn throw(cx: &mut Cx<'_, '_, '_>, e: Error) {
    if let Some(ex) = error_value(cx, e) {
        cx.scope.throw_exception(ex);
    }
}

/// The JavaScript value of a binding error (to reject a promise with); `None` when execution
/// is terminating.
pub fn error_value<'s>(cx: &mut Cx<'_, 's, '_>, e: Error) -> Option<v8::Local<'s, v8::Value>> {
    Some(match e {
        Error::Terminated => return None,
        Error::Thrown(v) => cx.local(&v),
        Error::Type(m) => {
            let s = cx.str(&m);
            v8::Exception::type_error(cx.scope, s)
        }
        Error::Range(m) => {
            let s = cx.str(&m);
            v8::Exception::range_error(cx.scope, s)
        }
        Error::Dom(name, m) => {
            let o = cx.alloc(
                Iface::DOMException,
                Slot::Exception {
                    name: name.to_string(),
                    message: m,
                },
            );
            cx.wrap(o).into()
        }
    })
}

thread_local! {
    /// Host functions running (called from script): script is on the stack.
    static HOST_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Whether script is on the stack (a host function called from it is running).
pub(crate) fn script_running() -> bool {
    HOST_DEPTH.with(|d| d.get() > 0)
}

/// HTML's "clean up after running script": with no script on the stack, the microtasks run
/// and mutation observers hear of changes (between the listeners of an event the engine
/// dispatches, as between a person's keystrokes).
pub(crate) fn clean_up(cx: &mut Cx<'_, '_, '_>) {
    if script_running() || cx.scope.is_execution_terminating() {
        return;
    }
    cx.scope.perform_microtask_checkpoint();
    if matches!(crate::dom::observers::deliver(cx), Ok(true)) {
        cx.scope.perform_microtask_checkpoint();
    }
}

/// Runs a generated callback body: builds the context, converts the result or throws.
pub fn call<F>(
    scope: &mut v8::PinScope,
    args: &v8::FunctionCallbackArguments,
    rv: &mut v8::ReturnValue,
    f: F,
) where
    F: FnOnce(&mut Cx<'_, '_, '_>, &v8::FunctionCallbackArguments) -> R<Out>,
{
    let Some(realm) = crate::realm::current(scope) else {
        return;
    };
    let world = crate::realm::current_world(scope);
    let mut cx = Cx {
        scope,
        realm,
        world,
    };
    // A bug in a binding must never unwind into V8 (that aborts the process): it becomes a
    // `TypeError` for the page and a recorded page error.
    HOST_DEPTH.with(|d| d.set(d.get() + 1));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut cx, args)));
    HOST_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    let result = result.unwrap_or_else(|p| {
        let why = p
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown".into());
        cx.realm.shared.borrow_mut().error(
            crate::PageErrorKind::Exception,
            format!("internal error in pageon: {why}"),
            "pageon",
        );
        Err(Error::Type(
            "Internal error in the engine (reported).".into(),
        ))
    });
    // CEReactions: custom element reactions run before the call returns to script.
    if cx.realm.st.borrow().custom.active()
        && let Err(Error::Terminated) = crate::dom::custom_elements::react(&mut cx)
    {
        return;
    }
    match result {
        Ok(Out::Undefined) => {}
        Ok(out) => {
            let v = cx.js_value(out);
            rv.set(v);
        }
        Err(e) => throw(&mut cx, e),
    }
}

/// The constructor of interfaces that have none.
pub fn illegal_constructor(
    scope: &mut v8::PinScope,
    _args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let msg =
        v8::String::new(scope, "Illegal constructor").unwrap_or_else(|| v8::String::empty(scope));
    let ex = v8::Exception::type_error(scope, msg);
    scope.throw_exception(ex);
}

/// A function template for an interface.
pub fn interface<'s>(
    scope: &mut v8::PinScope<'s, '_, ()>,
    name: &str,
    ctor: impl v8::MapFnTo<v8::FunctionCallback>,
) -> v8::Local<'s, v8::FunctionTemplate> {
    let f = v8::FunctionTemplate::new(scope, ctor);
    let n = v8::String::new(scope, name).unwrap_or_else(|| v8::String::empty(scope));
    f.set_class_name(n);
    // `Object.prototype.toString` names the interface (`[object Event]`), as WebIDL says.
    let tag = v8::Symbol::get_to_string_tag(scope);
    f.prototype_template(scope).set_with_attr(
        tag.into(),
        n.into(),
        v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_ENUM,
    );
    f
}

/// Adds a method.
pub fn method(
    scope: &mut v8::PinScope<'_, '_, ()>,
    target: v8::Local<'_, v8::Template>,
    name: &str,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
    length: usize,
) {
    let f = v8::FunctionTemplate::builder(cb)
        .length(i32::try_from(length).unwrap_or(0))
        .build(scope);
    let n = v8::String::new(scope, name).unwrap_or_else(|| v8::String::empty(scope));
    target.set(n.into(), f.into());
}

/// What an iterable's iterator yields.
#[derive(Debug, Clone, Copy)]
pub enum Iter {
    /// `[key, value]` pairs.
    Entries,
    /// Keys.
    Keys,
    /// Values.
    Values,
}

/// An iterator over a snapshot of `pairs` (an array iterator: `next`, and iterable itself).
pub fn iterator(cx: &mut Cx<'_, '_, '_>, pairs: Vec<(String, String)>, kind: Iter) -> R<Value> {
    let items: Vec<v8::Local<v8::Value>> = pairs
        .into_iter()
        .map(|(k, v)| match kind {
            Iter::Entries => {
                let pair = [cx.str(&k).into(), cx.str(&v).into()];
                v8::Array::new_with_elements(cx.scope, &pair).into()
            }
            Iter::Keys => cx.str(&k).into(),
            Iter::Values => cx.str(&v).into(),
        })
        .collect();
    let arr = v8::Array::new_with_elements(cx.scope, &items);
    let key = cx.str("values");
    let f = arr
        .get(cx.scope, key.into())
        .and_then(|f| v8::Local::<v8::Function>::try_from(f).ok())
        .ok_or_else(|| Error::Type("no array iterator".into()))?;
    let it = f.call(cx.scope, arr.into(), &[]).ok_or(Error::Terminated)?;
    Ok(cx.keep(it))
}

/// `forEach(callback, thisArg)` over a snapshot of `pairs`: `callback(value, key, object)`.
pub fn for_each(
    cx: &mut Cx<'_, '_, '_>,
    args: &v8::FunctionCallbackArguments,
    pairs: Vec<(String, String)>,
) -> R<()> {
    let Ok(f) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        return Err(Error::Type(
            "forEach: the callback is not a function".into(),
        ));
    };
    let this_arg = args.get(1);
    let obj: v8::Local<v8::Value> = args.this().into();
    for (k, v) in pairs {
        let a = [cx.str(&v).into(), cx.str(&k).into(), obj];
        // An exception propagates to the caller as is.
        f.call(cx.scope, this_arg, &a).ok_or(Error::Terminated)?;
    }
    Ok(())
}

/// Installs `entries`, `keys`, `values`, `forEach` and `@@iterator` on a prototype.
pub fn iterable(
    scope: &mut v8::PinScope<'_, '_, ()>,
    proto: v8::Local<'_, v8::ObjectTemplate>,
    entries: impl v8::MapFnTo<v8::FunctionCallback> + Copy,
    keys: impl v8::MapFnTo<v8::FunctionCallback>,
    values: impl v8::MapFnTo<v8::FunctionCallback>,
    for_each: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    method(scope, proto.into(), "entries", entries, 0);
    method(scope, proto.into(), "keys", keys, 0);
    method(scope, proto.into(), "values", values, 0);
    method(scope, proto.into(), "forEach", for_each, 1);
    let f = v8::FunctionTemplate::new(scope, entries);
    let sym = v8::Symbol::get_iterator(scope);
    proto.set(sym.into(), f.into());
}

/// Installs a set-like's `size`, `has`, `entries`, `keys` and `values` (the same function),
/// `forEach` and `@@iterator` (`values`) on a prototype.
pub fn setlike(
    scope: &mut v8::PinScope<'_, '_, ()>,
    proto: v8::Local<'_, v8::ObjectTemplate>,
    entries: impl v8::MapFnTo<v8::FunctionCallback>,
    values: impl v8::MapFnTo<v8::FunctionCallback> + Copy,
    for_each: impl v8::MapFnTo<v8::FunctionCallback>,
    size: impl v8::MapFnTo<v8::FunctionCallback>,
    has: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    method(scope, proto.into(), "entries", entries, 0);
    let f = v8::FunctionTemplate::new(scope, values);
    for name in ["keys", "values"] {
        let n = v8::String::new(scope, name).unwrap_or_else(|| v8::String::empty(scope));
        proto.set(n.into(), f.into());
    }
    let sym = v8::Symbol::get_iterator(scope);
    proto.set(sym.into(), f.into());
    method(scope, proto.into(), "forEach", for_each, 1);
    method(scope, proto.into(), "has", has, 1);
    accessor(scope, proto, "size", size, None);
}

/// Adds an accessor property.
pub fn accessor(
    scope: &mut v8::PinScope<'_, '_, ()>,
    target: v8::Local<'_, v8::ObjectTemplate>,
    name: &str,
    getter: impl v8::MapFnTo<v8::FunctionCallback>,
    setter: Option<v8::FunctionCallback>,
) {
    let g = v8::FunctionTemplate::new(scope, getter);
    let s = setter.map(|s| v8::FunctionTemplate::new_raw(scope, s));
    let n = v8::String::new(scope, name).unwrap_or_else(|| v8::String::empty(scope));
    target.set_accessor_property(n.into(), Some(g), s, v8::PropertyAttribute::NONE);
}

/// Adds a constant to the interface object and its prototype.
pub fn constant(
    scope: &mut v8::PinScope<'_, '_, ()>,
    f: v8::Local<'_, v8::FunctionTemplate>,
    proto: v8::Local<'_, v8::ObjectTemplate>,
    name: &str,
    value: f64,
) {
    let n = v8::String::new(scope, name).unwrap_or_else(|| v8::String::empty(scope));
    let v = v8::Number::new(scope, value);
    let attr = || v8::PropertyAttribute::READ_ONLY | v8::PropertyAttribute::DONT_DELETE;
    f.set_with_attr(n.into(), v.into(), attr());
    proto.set_with_attr(n.into(), v.into(), attr());
}

/// Puts an interface object on the global.
pub fn expose(
    scope: &mut v8::PinScope<'_, '_>,
    global: v8::Local<'_, v8::Object>,
    t: &v8::Global<v8::FunctionTemplate>,
    name: &str,
) {
    let t = v8::Local::new(scope, t);
    if let (Some(f), Some(n)) = (t.get_function(scope), v8::String::new(scope, name)) {
        let _ =
            global.define_own_property(scope, n.into(), f.into(), v8::PropertyAttribute::DONT_ENUM);
    }
}

/// Puts a `[LegacyFactoryFunction]` (`Image`, `Option`) on the global: a constructor function
/// whose `prototype` is the interface's, as WebIDL specifies.
pub fn expose_factory(
    scope: &mut v8::PinScope<'_, '_>,
    global: v8::Local<'_, v8::Object>,
    iface: &str,
    name: &str,
    length: usize,
    callback: impl v8::MapFnTo<v8::FunctionCallback>,
) {
    let (Some(iname), Some(n), Some(p)) = (
        v8::String::new(scope, iface),
        v8::String::new(scope, name),
        v8::String::new(scope, "prototype"),
    ) else {
        return;
    };
    let proto = global
        .get(scope, iname.into())
        .and_then(|c| c.to_object(scope))
        .and_then(|c| c.get(scope, p.into()));
    let Some(f) = v8::Function::builder(callback)
        .length(i32::try_from(length).unwrap_or(0))
        .build(scope)
    else {
        return;
    };
    f.set_name(n);
    if let Some(proto) = proto {
        let fixed = v8::PropertyAttribute::READ_ONLY
            | v8::PropertyAttribute::DONT_ENUM
            | v8::PropertyAttribute::DONT_DELETE;
        let _ = f.define_own_property(scope, p.into(), proto, fixed);
    }
    let _ = global.define_own_property(scope, n.into(), f.into(), v8::PropertyAttribute::DONT_ENUM);
}

/// Puts a namespace object (e.g. `console`) on the global.
pub fn expose_namespace(
    scope: &mut v8::PinScope<'_, '_>,
    global: v8::Local<'_, v8::Object>,
    t: &v8::Global<v8::FunctionTemplate>,
    name: &str,
) {
    let t = v8::Local::new(scope, t);
    let inst = t.instance_template(scope);
    if let (Some(o), Some(n)) = (inst.new_instance(scope), v8::String::new(scope, name)) {
        let _ =
            global.define_own_property(scope, n.into(), o.into(), v8::PropertyAttribute::DONT_ENUM);
    }
}

// ---- argument conversions -------------------------------------------------------------------

/// `DOMString` (ToString; symbols throw).
pub fn string(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> R<String> {
    if v.is_symbol() {
        return Err(Error::Type(
            "Cannot convert a Symbol value to a string".into(),
        ));
    }
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    match v.to_string(tc) {
        Some(s) => Ok(s.to_rust_string_lossy(tc)),
        None => Err(rethrow(tc)),
    }
}

fn rethrow(tc: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>) -> Error {
    if tc.is_execution_terminating() {
        return Error::Terminated;
    }
    match tc.exception() {
        Some(e) => Error::Thrown(v8::Global::new(tc, e)),
        None => Error::Terminated,
    }
}

/// `[LegacyNullToEmptyString] DOMString`.
pub fn string_nte(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> R<String> {
    if v.is_null() {
        Ok(String::new())
    } else {
        string(cx, v)
    }
}

/// `boolean`.
pub fn boolean(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> bool {
    v.boolean_value(cx.scope)
}

fn number(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> R<f64> {
    if v.is_number() {
        return Ok(v.number_value(cx.scope).unwrap_or(f64::NAN));
    }
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    match v.to_number(tc) {
        Some(n) => Ok(n.value()),
        None => Err(rethrow(tc)),
    }
}

/// Integer types, converted with WebIDL's modulo rules.
pub trait IdlInt: Sized {
    /// From an already truncated finite number.
    fn wrap(x: f64) -> Self;
}

macro_rules! idl_int {
    ($($t:ty),*) => {$(impl IdlInt for $t {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        fn wrap(x: f64) -> Self { (x as i128) as $t }
    })*};
}
idl_int!(i8, u8, i16, u16, i32, u32, i64, u64);

/// `long`, `unsigned short`, … .
pub fn int<T: IdlInt>(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> R<T> {
    let n = number(cx, v)?;
    Ok(T::wrap(if n.is_finite() { n.trunc() } else { 0.0 }))
}

/// `double` (restricted: non-finite values throw) and `unrestricted double`.
pub fn float(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>, restricted: bool) -> R<f64> {
    let n = number(cx, v)?;
    if restricted && !n.is_finite() {
        return Err(Error::Type(
            "The provided double value is non-finite.".into(),
        ));
    }
    Ok(n)
}

/// `any`, `object`, callbacks.
pub fn value(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> Value {
    v8::Global::new(cx.scope, v)
}

/// A nullable callback: `null`/`undefined` or a non-object become `None`.
pub fn opt_value(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> Option<Value> {
    (v.is_function() || v.is_object()).then(|| v8::Global::new(cx.scope, v))
}

/// An interface-typed argument.
pub fn obj(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>, iface: Iface) -> R<Obj> {
    unwrap_obj(cx, v, iface)
        .map_err(|_| Error::Type(format!("parameter is not of type '{}'.", iface.name())))
}

/// `sequence<T>`.
pub fn seq<T>(
    cx: &mut Cx<'_, '_, '_>,
    v: v8::Local<'_, v8::Value>,
    mut each: impl FnMut(&mut Cx<'_, '_, '_>, v8::Local<'_, v8::Value>) -> R<T>,
) -> R<Vec<T>> {
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
        return Err(Error::Type(
            "The provided value cannot be converted to a sequence.".into(),
        ));
    };
    let len_key = cx.str("length");
    let len = o
        .get(cx.scope, len_key.into())
        .and_then(|l| l.uint32_value(cx.scope))
        .unwrap_or(0)
        .min(100_000);
    let mut out = Vec::new();
    for i in 0..len {
        let item = o
            .get_index(cx.scope, i)
            .unwrap_or_else(|| v8::undefined(cx.scope).into());
        out.push(each(cx, item)?);
    }
    Ok(out)
}

/// `(Node or DOMString)`.
pub fn node_or_string(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> R<NodeOrString> {
    match unwrap_obj(cx, v, Iface::Node) {
        Ok(o) => Ok(NodeOrString::Node(o)),
        Err(_) => Ok(NodeOrString::Str(string(cx, v)?)),
    }
}

/// An object argument (dictionaries).
pub fn object<'s>(
    cx: &mut Cx<'_, 's, '_>,
    v: v8::Local<'_, v8::Value>,
    what: &str,
) -> R<v8::Local<'s, v8::Object>> {
    let v = v8::Local::new(&*cx.scope, v);
    v8::Local::<v8::Object>::try_from(v)
        .map_err(|_| Error::Type(format!("The provided value is not of type '{what}'.")))
}

/// A property of an object (getters may throw).
pub fn get<'s>(
    cx: &mut Cx<'_, 's, '_>,
    o: v8::Local<'_, v8::Object>,
    key: &str,
) -> R<v8::Local<'s, v8::Value>> {
    let o = v8::Local::new(&*cx.scope, o);
    let k = cx.str(key);
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    match o.get(tc, k.into()) {
        Some(v) => Ok(v8::Local::new(tc, v)),
        None => Err(rethrow(tc)),
    }
}

// ---- generated attribute families ---------------------------------------------------------

/// Reads a `[Reflect]` attribute.
pub fn reflect_get(cx: &mut Cx<'_, '_, '_>, this: Obj, name: &str, kind: Reflect) -> R<Out> {
    let id = this.node()?;
    let value = cx
        .dom()
        .get(id)
        .and_then(|n| n.attr(name).map(str::to_string));
    Ok(match kind {
        Reflect::Str => Out::Str(value.unwrap_or_default()),
        Reflect::NullableStr => value.map_or(Out::Null, Out::Str),
        Reflect::Bool => Out::Bool(value.is_some()),
        Reflect::Url => Out::Str(match value {
            Some(v) => crate::dom::resolve_url(cx, &v).unwrap_or(v),
            None => String::new(),
        }),
        Reflect::Long(d) => Out::Num(value.and_then(|v| parse_int(&v)).unwrap_or(d) as f64),
        Reflect::NonNegative => Out::Num(
            value
                .and_then(|v| parse_int(&v))
                .filter(|n| (0..=i64::from(i32::MAX)).contains(n))
                .unwrap_or(-1) as f64,
        ),
        Reflect::Unsigned(d) => Out::Num(
            value
                .and_then(|v| parse_int(&v))
                .filter(|n| *n >= 0 && *n <= i64::from(i32::MAX))
                .map_or(d as f64, |n| n as f64),
        ),
        Reflect::Double(d) => Out::Num(
            value
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|n| n.is_finite())
                .unwrap_or(d),
        ),
    })
}

/// HTML's rules for parsing integers (leading whitespace, sign, digits).
pub fn parse_int(s: &str) -> Option<i64> {
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0c', '\r']);
    let (neg, rest) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let digits: String = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .take(18)
        .collect();
    let n: i64 = digits.parse().ok()?;
    Some(if neg { -n } else { n })
}

/// Writes a `[Reflect]` attribute.
pub fn reflect_set(
    cx: &mut Cx<'_, '_, '_>,
    this: Obj,
    name: &str,
    v: v8::Local<'_, v8::Value>,
    kind: Reflect,
) -> R<()> {
    let id = this.node()?;
    let value = match kind {
        Reflect::Bool => {
            let on = boolean(cx, v);
            let mut dom = cx.dom_mut();
            let r = if on {
                dom.set_attr(id, name, "")
            } else {
                dom.remove_attr(id, name)
            };
            return r.map_err(|e| Error::dom("NotFoundError", e.to_string()));
        }
        Reflect::NullableStr if v.is_null() => {
            return cx
                .dom_mut()
                .remove_attr(id, name)
                .map_err(|e| Error::dom("NotFoundError", e.to_string()));
        }
        Reflect::Long(_) => int::<i32>(cx, v)?.to_string(),
        Reflect::NonNegative => {
            let n = int::<i32>(cx, v)?;
            if n < 0 {
                return Err(Error::dom(
                    "IndexSizeError",
                    format!("The value provided ({n}) is not non-negative."),
                ));
            }
            n.to_string()
        }
        Reflect::Unsigned(_) => {
            let n = int::<u32>(cx, v)?;
            if n > i32::MAX as u32 {
                "0".to_string()
            } else {
                n.to_string()
            }
        }
        Reflect::Double(_) => {
            let n = float(cx, v, true)?;
            crate::dom::number_to_string(n)
        }
        _ => string(cx, v)?,
    };
    cx.dom_mut()
        .set_attr(id, name, &value)
        .map_err(|e| Error::dom("NotFoundError", e.to_string()))
}

fn target(this: Obj) -> R<Target> {
    Ok(match this {
        Obj::Window => Target::Window,
        Obj::Node(n) => Target::Node(n),
        Obj::Slab(i) => Target::Object(i),
        Obj::Frame(..) | Obj::Foreign(..) => {
            return Err(Error::dom("NotSupportedError", crate::cv::FOREIGN));
        }
    })
}

/// Reads an event handler IDL attribute (`onclick`).
pub fn handler_get(cx: &mut Cx<'_, '_, '_>, this: Obj, event: &str) -> R<Option<Value>> {
    let t = target(this)?;
    let w = cx.world;
    Ok(cx.st().handlers.get(&(w, t, event.to_string())).cloned())
}

/// Writes an event handler IDL attribute.
pub fn handler_set(cx: &mut Cx<'_, '_, '_>, this: Obj, event: &str, v: Option<Value>) -> R<()> {
    let t = target(this)?;
    let w = cx.world;
    let mut st = cx.st();
    match v {
        Some(f) => {
            st.handlers.insert((w, t, event.to_string()), f);
        }
        None => {
            st.handlers.remove(&(w, t, event.to_string()));
        }
    }
    Ok(())
}

/// `[PutForwards]`: `el.classList = "a b"` sets `el.classList.value`.
pub fn put_forwards(
    cx: &mut Cx<'_, '_, '_>,
    args: &v8::FunctionCallbackArguments,
    attr: &str,
    forward: &str,
) -> R<()> {
    let this = args.this();
    let target = get(cx, this, attr)?;
    let Ok(o) = v8::Local::<v8::Object>::try_from(target) else {
        return Ok(());
    };
    let key = cx.str(forward);
    let v = args.get(0);
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    if o.set(tc, key.into(), v).is_none() {
        return Err(rethrow(tc));
    }
    Ok(())
}

/// `null` as a kept value.
pub fn null(cx: &mut Cx<'_, '_, '_>) -> Value {
    let n = v8::null(cx.scope);
    v8::Global::new(cx.scope, v8::Local::<v8::Value>::from(n))
}

/// `{}` as a kept value.
#[allow(dead_code)] // emitted by the generator for IDL defaults
pub fn empty_object(cx: &mut Cx<'_, '_, '_>) -> Value {
    let o = v8::Object::new(cx.scope);
    v8::Global::new(cx.scope, v8::Local::<v8::Value>::from(o))
}

/// `[]` as a kept value.
#[allow(dead_code)] // emitted by the generator for IDL defaults
pub fn empty_array(cx: &mut Cx<'_, '_, '_>) -> Value {
    let a = v8::Array::new(cx.scope, 0);
    v8::Global::new(cx.scope, v8::Local::<v8::Value>::from(a))
}

/// A boolean as a kept value.
#[allow(dead_code)] // emitted by the generator for IDL defaults
pub fn boolean_value(cx: &mut Cx<'_, '_, '_>, b: bool) -> Value {
    let v = v8::Boolean::new(cx.scope, b);
    v8::Global::new(cx.scope, v8::Local::<v8::Value>::from(v))
}

/// A string as a kept value.
#[allow(dead_code)] // emitted by the generator for IDL defaults
pub fn number_value(cx: &mut Cx<'_, '_, '_>, n: f64) -> Value {
    let v = v8::Number::new(cx.scope, n);
    v8::Global::new(cx.scope, v8::Local::<v8::Value>::from(v))
}

/// `undefined`, kept.
pub fn undefined(cx: &mut Cx<'_, '_, '_>) -> Value {
    let v = v8::undefined(cx.scope);
    v8::Global::new(cx.scope, v8::Local::<v8::Value>::from(v))
}

/// A string, kept.
pub fn string_value(cx: &mut Cx<'_, '_, '_>, s: &str) -> Value {
    let v = cx.str(s);
    v8::Global::new(cx.scope, v8::Local::<v8::Value>::from(v))
}

impl<D: Default> Default for OrBool<D> {
    fn default() -> Self {
        OrBool::Dict(D::default())
    }
}
