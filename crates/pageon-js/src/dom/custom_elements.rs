//! Custom elements (HTML Standard): the registry, the HTML element constructor, upgrades,
//! and the lifecycle callbacks.
//!
//! Reactions (CEReactions) run before the DOM call that caused them returns: every binding
//! call ends with [`react`], which reads the document's mutation log for connections,
//! disconnections and attribute changes of custom elements, upgrades newly connected
//! candidates, and runs the callbacks in order. Callbacks fire on actual transitions (a flag per
//! element), so the same change seen twice never calls twice.

use std::collections::HashMap;

use pageon_dom::{Edge, Mutation, NodeData, NodeId};

use crate::cv::{Cx, Error, Obj, R, Value};
use crate::generated::ElementDefinitionOptions;

/// One `customElements.define`.
#[derive(Debug, Clone)]
pub struct Definition {
    name: String,
    ctor: Value,
    proto: Value,
    observed: Vec<String>,
    connected: Option<Value>,
    disconnected: Option<Value>,
    attribute_changed: Option<Value>,
    /// `static formAssociated = true`.
    form_associated: bool,
    /// `static disabledFeatures` holds `"internals"`.
    disable_internals: bool,
    form_associated_cb: Option<Value>,
    form_reset_cb: Option<Value>,
    form_disabled_cb: Option<Value>,
}

/// What an element is to the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Upgraded (or constructed): definition index, and whether it was last seen connected.
    Custom(usize, bool),
    /// Its constructor threw.
    Failed,
}

/// The realm's registry.
#[derive(Debug, Default)]
pub struct Registry {
    defs: Vec<Definition>,
    when_defined: HashMap<String, Vec<v8::Global<v8::PromiseResolver>>>,
    /// Elements being upgraded (the HTML element constructor returns the top one).
    construction: Vec<NodeId>,
    state: HashMap<NodeId, State>,
    /// How far the document's mutation log has been read.
    pub(crate) cursor: usize,
    reacting: bool,
    /// Form-associated elements: the form owner and disabled state last told to them.
    faces: HashMap<NodeId, (Option<NodeId>, bool)>,
}

impl Registry {
    /// Whether any element is defined (then mutations are logged and reactions run).
    pub fn active(&self) -> bool {
        !self.defs.is_empty()
    }

    fn by_name(&self, name: &str) -> Option<usize> {
        self.defs.iter().position(|d| d.name == name)
    }
}

/// A reaction to run.
enum Reaction {
    Upgrade(NodeId, usize),
    Connected(NodeId, usize),
    Disconnected(NodeId, usize),
    Attribute(NodeId, usize, String, Option<String>, Option<String>),
}

fn def_for(cx: &Cx<'_, '_, '_>, el: NodeId) -> Option<usize> {
    let dom = cx.dom();
    let n = dom.get(el)?;
    if n.namespace() != Some(pageon_dom::Namespace::Html) {
        return None;
    }
    let name = n.local_name()?.to_string();
    drop(dom);
    cx.st().custom.by_name(&name)
}

/// Elements in `root`'s shadow-including subtree, in tree order.
fn shadow_including(cx: &Cx<'_, '_, '_>, root: NodeId) -> Vec<NodeId> {
    let dom = cx.dom();
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(r) = stack.pop() {
        let mut found = Vec::new();
        for e in dom.traverse(r) {
            if let Edge::Enter(n) = e {
                if matches!(dom.get(n).map(|x| &x.data), Some(NodeData::Element { .. })) {
                    out.push(n);
                }
                if let Some(s) = dom.shadow_root(n) {
                    found.push(s);
                }
            }
        }
        stack.extend(found.into_iter().rev());
    }
    out
}

/// The HTML element constructor (`super()` in a custom element class, `new X()`).
pub fn construct(cx: &mut Cx<'_, '_, '_>, new_target: Value) -> R<Obj> {
    let target = cx.local(&new_target);
    let def = {
        let st = cx.st();
        st.custom.defs.iter().position(|d| {
            let c = v8::Local::new(cx.scope, &d.ctor);
            c.strict_equals(target)
        })
    };
    let Some(i) = def else {
        return Err(Error::Type("Illegal constructor".into()));
    };
    // An upgrade: the element being upgraded (its wrapper already has the prototype).
    if let Some(&el) = cx.st().custom.construction.last() {
        return Ok(Obj::Node(el));
    }
    let name = cx.st().custom.defs[i].name.clone();
    let el = super::create_element(&mut cx.dom_mut(), html5ever::ns!(html), &name);
    set_prototype(cx, el, i);
    cx.st().custom.state.insert(el, State::Custom(i, false));
    mark_form_associated(cx, el, i);
    Ok(Obj::Node(el))
}

fn set_prototype(cx: &mut Cx<'_, '_, '_>, el: NodeId, def: usize) {
    let proto = cx.st().custom.defs[def].proto.clone();
    let w = cx.wrap(Obj::Node(el));
    let p = cx.local(&proto);
    let _ = w.set_prototype(cx.scope, p);
}

/// Upgrades `el` with definition `def`: the constructor runs on the existing element, then
/// `attributeChangedCallback` for its observed attributes and `connectedCallback` if connected.
fn upgrade(cx: &mut Cx<'_, '_, '_>, el: NodeId, def: usize) -> R<()> {
    // Already upgraded, or being constructed (a reaction its constructor's DOM calls run must
    // not construct it again).
    let busy = {
        let st = cx.st();
        st.custom.state.contains_key(&el) || st.custom.construction.contains(&el)
    };
    if busy {
        return Ok(());
    }
    set_prototype(cx, el, def);
    mark_form_associated(cx, el, def);
    let ctor = cx.st().custom.defs[def].ctor.clone();
    cx.st().custom.construction.push(el);
    let (ok, terminated, error) = {
        let f = cx.local(&ctor);
        let scope = &mut *cx.scope;
        v8::tc_scope!(let tc, scope);
        let r = v8::Local::<v8::Function>::try_from(f)
            .ok()
            .and_then(|f| f.new_instance(tc, &[]));
        let terminated = tc.has_terminated() || tc.is_execution_terminating();
        let error = match tc.exception() {
            Some(ex) if !terminated => {
                let msg = tc.message();
                Some(crate::engine::page_error(tc, ex, msg))
            }
            _ => None,
        };
        (r.is_some() && error.is_none(), terminated, error)
    };
    cx.st().custom.construction.pop();
    if terminated {
        return Err(Error::Terminated);
    }
    if let Some(e) = error {
        cx.realm.shared.borrow_mut().errors.push(e);
    }
    if !ok {
        cx.st().custom.state.insert(el, State::Failed);
        return Ok(());
    }
    cx.st().custom.state.insert(el, State::Custom(def, false));
    let observed = cx.st().custom.defs[def].observed.clone();
    for name in observed {
        let v = cx
            .dom()
            .get(el)
            .and_then(|n| n.attr(&name).map(str::to_string));
        if v.is_some() {
            call(cx, el, def, Callback::Attribute(name, None, v))?;
        }
    }
    if cx.dom().is_connected(el) {
        cx.st().custom.state.insert(el, State::Custom(def, true));
        call(cx, el, def, Callback::Connected)?;
    }
    if cx.st().custom.defs[def].form_associated {
        face_check(cx, el)?;
    }
    Ok(())
}

/// Marks an element of a form-associated definition (its form owner is tracked from now on).
fn mark_form_associated(cx: &mut Cx<'_, '_, '_>, el: NodeId, def: usize) {
    if cx.st().custom.defs[def].form_associated {
        cx.dom_mut().internals_mut(el).form_associated = true;
        cx.st().custom.faces.entry(el).or_insert((None, false));
    }
}

/// Tells a form-associated element of a changed form owner or disabled state
/// (`formAssociatedCallback`, `formDisabledCallback`).
fn face_check(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<()> {
    let Some(&(owner, disabled)) = cx.st().custom.faces.get(&el) else {
        return Ok(());
    };
    let Some(State::Custom(def, _)) = cx.st().custom.state.get(&el).copied() else {
        return Ok(());
    };
    let (now_owner, now_disabled) = {
        let dom = cx.dom();
        if dom.get(el).is_none() {
            drop(dom);
            cx.st().custom.faces.remove(&el);
            return Ok(());
        }
        (
            pageon_dom::forms::owner(&dom, el),
            pageon_dom::forms::disabled(&dom, el),
        )
    };
    cx.st().custom.faces.insert(el, (now_owner, now_disabled));
    cx.dom_mut().internals_mut(el).disabled = now_disabled;
    if now_owner != owner {
        call(cx, el, def, Callback::FormAssociated(now_owner))?;
    }
    if now_disabled != disabled {
        call(cx, el, def, Callback::FormDisabled(now_disabled))?;
    }
    Ok(())
}

/// `formResetCallback` of a form-associated element.
pub(crate) fn form_reset(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<()> {
    let state = cx.st().custom.state.get(&el).copied();
    if let Some(State::Custom(def, _)) = state {
        call(cx, el, def, Callback::FormReset)?;
    }
    Ok(())
}

/// What `attachInternals()` needs to know: whether `el` is a custom element (being constructed
/// or constructed), and whether its definition disables internals.
pub(crate) fn internals_allowed(cx: &Cx<'_, '_, '_>, el: NodeId) -> Option<bool> {
    let (state, constructing) = {
        let st = cx.st();
        (
            st.custom.state.get(&el).copied(),
            st.custom.construction.last() == Some(&el),
        )
    };
    let def = match state {
        Some(State::Custom(d, _)) => d,
        _ if constructing => def_for(cx, el)?,
        _ => return None,
    };
    let allowed = !cx.st().custom.defs[def].disable_internals;
    Some(allowed)
}

enum Callback {
    Connected,
    Disconnected,
    Attribute(String, Option<String>, Option<String>),
    FormAssociated(Option<NodeId>),
    FormReset,
    FormDisabled(bool),
}

fn call(cx: &mut Cx<'_, '_, '_>, el: NodeId, def: usize, c: Callback) -> R<()> {
    enum Arg {
        Str(Option<String>),
        Node(Option<NodeId>),
        Bool(bool),
    }
    let (f, args) = {
        let st = cx.st();
        let d = &st.custom.defs[def];
        match c {
            Callback::Connected => (d.connected.clone(), Vec::new()),
            Callback::Disconnected => (d.disconnected.clone(), Vec::new()),
            Callback::Attribute(n, old, new) => (
                d.attribute_changed.clone(),
                vec![Arg::Str(Some(n)), Arg::Str(old), Arg::Str(new)],
            ),
            Callback::FormAssociated(f) => (d.form_associated_cb.clone(), vec![Arg::Node(f)]),
            Callback::FormReset => (d.form_reset_cb.clone(), Vec::new()),
            Callback::FormDisabled(b) => (d.form_disabled_cb.clone(), vec![Arg::Bool(b)]),
        }
    };
    let Some(f) = f else { return Ok(()) };
    let this: v8::Local<v8::Value> = cx.wrap(Obj::Node(el)).into();
    let args: Vec<v8::Local<v8::Value>> = args
        .into_iter()
        .map(|a| match a {
            Arg::Str(Some(s)) => cx.str(&s).into(),
            Arg::Node(Some(n)) => cx.wrap(Obj::Node(n)).into(),
            Arg::Bool(b) => v8::Boolean::new(cx.scope, b).into(),
            Arg::Str(None) | Arg::Node(None) => v8::null(cx.scope).into(),
        })
        .collect();
    cx.invoke(&f, this, &args)?;
    Ok(())
}

/// Runs the reactions the DOM changes since the last call asked for. Cheap when no element is
/// defined.
pub(crate) fn react(cx: &mut Cx<'_, '_, '_>) -> R<()> {
    for _ in 0..1000 {
        let (active, reacting) = {
            let st = cx.st();
            (st.custom.active(), st.custom.reacting)
        };
        if !active || reacting {
            return Ok(());
        }
        let (reactions, changed) = collect(cx);
        if changed && !cx.st().custom.faces.is_empty() {
            let faces: Vec<NodeId> = cx.st().custom.faces.keys().copied().collect();
            cx.st().custom.reacting = true;
            let r = faces.into_iter().try_for_each(|el| face_check(cx, el));
            cx.st().custom.reacting = false;
            r?;
        }
        if reactions.is_empty() {
            return Ok(());
        }
        cx.st().custom.reacting = true;
        let r = run(cx, reactions);
        cx.st().custom.reacting = false;
        r?;
    }
    Ok(())
}

fn run(cx: &mut Cx<'_, '_, '_>, reactions: Vec<Reaction>) -> R<()> {
    for r in reactions {
        match r {
            Reaction::Upgrade(el, def) => {
                // Reactions it queues (attributes, connected) run inside `upgrade`; let the
                // nested ones through.
                cx.st().custom.reacting = false;
                upgrade(cx, el, def)?;
                cx.st().custom.reacting = true;
            }
            Reaction::Connected(el, def) => call(cx, el, def, Callback::Connected)?,
            Reaction::Disconnected(el, def) => call(cx, el, def, Callback::Disconnected)?,
            Reaction::Attribute(el, def, n, old, new) => {
                call(cx, el, def, Callback::Attribute(n, old, new))?;
            }
        }
    }
    Ok(())
}

/// Reads the mutation log from the cursor and turns it into reactions; and whether the log had
/// entries (form owners may have changed).
fn collect(cx: &mut Cx<'_, '_, '_>) -> (Vec<Reaction>, bool) {
    let log: Vec<Mutation> = {
        let cursor = cx.st().custom.cursor;
        let dom = cx.dom();
        dom.mutations().get(cursor..).unwrap_or_default().to_vec()
    };
    {
        let len = cx.dom().mutations().len();
        let observers = !cx.st().observers.is_empty();
        if observers {
            cx.st().custom.cursor = len;
        } else {
            // Nobody else reads the log: drop what was read.
            cx.realm.dom.borrow_mut().take_mutations();
            cx.st().custom.cursor = 0;
        }
    }
    let mut out = Vec::new();
    let changed = !log.is_empty();
    for m in log {
        match m {
            Mutation::ChildList { added, removed, .. } => {
                for n in added.into_iter().chain(removed) {
                    for el in shadow_including(cx, n) {
                        let state = cx.st().custom.state.get(&el).copied();
                        let connected = cx.dom().is_connected(el);
                        match state {
                            Some(State::Custom(d, was)) if was != connected => {
                                cx.st().custom.state.insert(el, State::Custom(d, connected));
                                out.push(if connected {
                                    Reaction::Connected(el, d)
                                } else {
                                    Reaction::Disconnected(el, d)
                                });
                            }
                            None if connected => {
                                if let Some(d) = def_for(cx, el)
                                    && !out
                                        .iter()
                                        .any(|r| matches!(r, Reaction::Upgrade(e, _) if *e == el))
                                {
                                    out.push(Reaction::Upgrade(el, d));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Mutation::Attribute {
                target, name, old, ..
            } => {
                let state = cx.st().custom.state.get(&target).copied();
                let observed = match state {
                    Some(State::Custom(d, _)) => {
                        cx.st().custom.defs[d].observed.contains(&name).then_some(d)
                    }
                    _ => None,
                };
                if let Some(d) = observed {
                    let new = cx
                        .dom()
                        .get(target)
                        .and_then(|n| n.attr(&name).map(str::to_string));
                    out.push(Reaction::Attribute(target, d, name, old, new));
                }
            }
            _ => {}
        }
    }
    (out, changed)
}

/// Upgrades every candidate in `root`'s shadow-including subtree (in tree order).
pub(crate) fn upgrade_tree(cx: &mut Cx<'_, '_, '_>, root: NodeId) -> R<()> {
    if !cx.st().custom.active() {
        return Ok(());
    }
    for el in shadow_including(cx, root) {
        if cx.st().custom.state.contains_key(&el) {
            continue;
        }
        if let Some(d) = def_for(cx, el) {
            upgrade(cx, el, d)?;
        }
    }
    Ok(())
}

/// Constructs a just-created element of a defined name (`document.createElement`).
pub(crate) fn created(cx: &mut Cx<'_, '_, '_>, el: NodeId) -> R<()> {
    if let Some(d) = def_for(cx, el) {
        upgrade(cx, el, d)?;
    }
    Ok(())
}

/// A property of an object; an exception its getter throws is rethrown.
fn prop(cx: &mut Cx<'_, '_, '_>, o: &Value, k: &str) -> R<Value> {
    let key = cx.str(k);
    let obj = cx.local(o);
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    let r = v8::Local::<v8::Object>::try_from(obj)
        .ok()
        .and_then(|o| o.get(tc, key.into()));
    if tc.has_terminated() || tc.is_execution_terminating() {
        return Err(Error::Terminated);
    }
    if let Some(ex) = tc.exception() {
        return Err(Error::Thrown(v8::Global::new(tc, ex)));
    }
    let v = r.unwrap_or_else(|| v8::undefined(tc).into());
    Ok(v8::Global::new(tc, v))
}

/// A lifecycle callback of a prototype: a function, or absent.
fn callback(cx: &mut Cx<'_, '_, '_>, proto: &Value, k: &str) -> R<Option<Value>> {
    let v = prop(cx, proto, k)?;
    let l = cx.local(&v);
    if l.is_function() {
        Ok(Some(v))
    } else if l.is_undefined() {
        Ok(None)
    } else {
        Err(Error::Type(format!(
            "The '{k}' callback is not a function."
        )))
    }
}

/// `CustomElementRegistry`.
pub mod custom_element_registry {
    use super::*;

    pub fn define(
        cx: &mut Cx<'_, '_, '_>,
        _this: Obj,
        name: String,
        ctor: Value,
        options: ElementDefinitionOptions,
    ) -> R<()> {
        // Definitions belong to the page's main world, as in Chromium's isolated worlds.
        if cx.world != 0 {
            return Err(Error::dom(
                "NotSupportedError",
                "Failed to execute 'define' on 'CustomElementRegistry': custom elements cannot be defined in an isolated world",
            ));
        }
        let f = cx.local(&ctor);
        let Ok(func) = v8::Local::<v8::Object>::try_from(f) else {
            return Err(Error::Type(
                "Failed to execute 'define' on 'CustomElementRegistry': The provided value is not of type 'Function'.".into(),
            ));
        };
        if !func.is_constructor() {
            return Err(Error::Type(
                "Failed to execute 'define' on 'CustomElementRegistry': The provided value is not a constructor.".into(),
            ));
        }
        if !pageon_dom::is_custom_name(&name) {
            return Err(Error::dom(
                "SyntaxError",
                format!(
                    "Failed to execute 'define' on 'CustomElementRegistry': \"{name}\" is not a valid custom element name"
                ),
            ));
        }
        let taken = {
            let st = cx.st();
            st.custom.by_name(&name).is_some()
                || st.custom.defs.iter().any(|d| {
                    let c = v8::Local::new(cx.scope, &d.ctor);
                    c.strict_equals(f)
                })
        };
        if taken {
            return Err(Error::dom(
                "NotSupportedError",
                format!(
                    "Failed to execute 'define' on 'CustomElementRegistry': the name \"{name}\" or this constructor has already been used with this registry"
                ),
            ));
        }
        if options.extends.is_some() {
            return Err(Error::dom(
                "NotSupportedError",
                "Failed to execute 'define' on 'CustomElementRegistry': customized built-in elements are not supported",
            ));
        }
        let proto = prop(cx, &ctor, "prototype")?;
        if !cx.local(&proto).is_object() {
            return Err(Error::Type(
                "The constructor's prototype is not an object.".into(),
            ));
        }
        let connected = callback(cx, &proto, "connectedCallback")?;
        let disconnected = callback(cx, &proto, "disconnectedCallback")?;
        let attribute_changed = callback(cx, &proto, "attributeChangedCallback")?;
        let mut observed = Vec::new();
        if attribute_changed.is_some() {
            let list = prop(cx, &ctor, "observedAttributes")?;
            let list = cx.local(&list);
            if let Ok(arr) = v8::Local::<v8::Array>::try_from(list) {
                for i in 0..arr.length() {
                    if let Some(s) = arr.get_index(cx.scope, i) {
                        observed.push(s.to_rust_string_lossy(cx.scope));
                    }
                }
            }
        }
        let mut disable_internals = false;
        let features = prop(cx, &ctor, "disabledFeatures")?;
        let features = cx.local(&features);
        if let Ok(arr) = v8::Local::<v8::Array>::try_from(features) {
            for i in 0..arr.length() {
                if let Some(s) = arr.get_index(cx.scope, i) {
                    disable_internals |= s.to_rust_string_lossy(cx.scope) == "internals";
                }
            }
        }
        let fa = prop(cx, &ctor, "formAssociated")?;
        let form_associated = cx.local(&fa).boolean_value(cx.scope);
        let (form_associated_cb, form_reset_cb, form_disabled_cb) = if form_associated {
            (
                callback(cx, &proto, "formAssociatedCallback")?,
                callback(cx, &proto, "formResetCallback")?,
                callback(cx, &proto, "formDisabledCallback")?,
            )
        } else {
            (None, None, None)
        };
        cx.st().custom.defs.push(Definition {
            name: name.clone(),
            ctor: ctor.clone(),
            proto,
            observed,
            connected,
            disconnected,
            attribute_changed,
            form_associated,
            disable_internals,
            form_associated_cb,
            form_reset_cb,
            form_disabled_cb,
        });
        super::super::observers::update_observing(cx);
        // Upgrade what is already in the document, in tree order.
        let root = cx.dom().root();
        upgrade_tree(cx, root)?;
        // Resolve `whenDefined`.
        let waiting = cx
            .st()
            .custom
            .when_defined
            .remove(&name)
            .unwrap_or_default();
        for r in waiting {
            let res = v8::Local::new(cx.scope, &r);
            let c = cx.local(&ctor);
            res.resolve(cx.scope, c);
        }
        Ok(())
    }

    pub fn get(cx: &mut Cx<'_, '_, '_>, _this: Obj, name: String) -> R<Value> {
        let st = cx.st();
        Ok(match st.custom.by_name(&name) {
            Some(i) => st.custom.defs[i].ctor.clone(),
            None => {
                drop(st);
                let u = v8::undefined(cx.scope).into();
                cx.keep(u)
            }
        })
    }

    pub fn get_name(cx: &mut Cx<'_, '_, '_>, _this: Obj, ctor: Value) -> R<Option<String>> {
        let f = cx.local(&ctor);
        let st = cx.st();
        Ok(st
            .custom
            .defs
            .iter()
            .find(|d| v8::Local::new(cx.scope, &d.ctor).strict_equals(f))
            .map(|d| d.name.clone()))
    }

    pub fn when_defined(cx: &mut Cx<'_, '_, '_>, _this: Obj, name: String) -> R<Value> {
        if !pageon_dom::is_custom_name(&name) {
            let r = v8::PromiseResolver::new(cx.scope).ok_or(Error::Terminated)?;
            let msg = cx.str(&format!("\"{name}\" is not a valid custom element name"));
            let e = v8::Exception::syntax_error(cx.scope, msg);
            r.reject(cx.scope, e);
            let p = r.get_promise(cx.scope);
            return Ok(cx.keep(p.into()));
        }
        let ctor = {
            let st = cx.st();
            st.custom
                .by_name(&name)
                .map(|i| st.custom.defs[i].ctor.clone())
        };
        let r = v8::PromiseResolver::new(cx.scope).ok_or(Error::Terminated)?;
        let p = r.get_promise(cx.scope);
        match ctor {
            Some(c) => {
                let c = cx.local(&c);
                r.resolve(cx.scope, c);
            }
            None => {
                let keep = v8::Global::new(cx.scope, r);
                cx.st()
                    .custom
                    .when_defined
                    .entry(name)
                    .or_default()
                    .push(keep);
            }
        }
        Ok(cx.keep(p.into()))
    }

    pub fn upgrade(cx: &mut Cx<'_, '_, '_>, _this: Obj, root: Obj) -> R<()> {
        let root = root.node()?;
        upgrade_tree(cx, root)
    }
}
