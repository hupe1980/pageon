//! Evaluation for protocol clients (CDP): run code in a realm, hold results as handles, and
//! describe values the way Chrome's DevTools protocol does.

use pageon_dom::{NodeData, NodeId};
use serde_json::{Value as Json, json};

use crate::cv::{self, Cx};
use crate::engine::Engine;

/// A JavaScript value held for a client until released (engine-wide ids).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Handle(pub u32);

/// An argument of [`Engine::call_function`].
#[derive(Debug, Clone)]
pub enum Arg {
    /// A JSON value.
    Json(Json),
    /// `NaN`, `Infinity`, `-Infinity`, `-0` or a BigInt literal (`12n`).
    Unserializable(String),
    /// A held value.
    Handle(Handle),
    /// `undefined`.
    Undefined,
}

/// A script that threw.
#[derive(Debug, Clone)]
pub struct Thrown {
    /// Message text (`Uncaught Error: …`).
    pub text: String,
    /// 0-based line.
    pub line: u32,
    /// 0-based column.
    pub column: u32,
    /// The thrown value.
    pub exception: Option<Handle>,
}

/// A promise's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromiseState {
    /// Not a promise (the value itself is the result).
    NotPromise,
    /// Still pending.
    Pending,
    /// Fulfilled with a value.
    Fulfilled(Handle),
    /// Rejected with a reason.
    Rejected(Handle),
}

type Raw = Result<cv::Value, (String, u32, u32, Option<cv::Value>)>;

/// Runs `f` (compiling or calling) inside a try-catch scope and returns its value or exception.
fn caught<'s>(
    cx: &mut Cx<'_, 's, '_>,
    f: impl FnOnce(&mut v8::PinScope<'s, '_>) -> Option<v8::Local<'s, v8::Value>>,
) -> Raw {
    let scope = &mut *cx.scope;
    v8::tc_scope!(let tc, scope);
    let r = f(tc);
    if tc.has_terminated() || tc.is_execution_terminating() {
        return Err(("script terminated: time limit".into(), 0, 0, None));
    }
    if let Some(ex) = tc.exception() {
        let msg = tc.message();
        let text = msg
            .map(|m| m.get(tc).to_rust_string_lossy(tc))
            .unwrap_or_else(|| "Uncaught".into());
        let (line, column) = msg
            .map(|m| {
                (
                    m.get_line_number(tc).unwrap_or(1).saturating_sub(1) as u32,
                    m.get_start_column() as u32,
                )
            })
            .unwrap_or((0, 0));
        return Err((text, line, column, Some(v8::Global::new(tc, ex))));
    }
    r.map(|v| v8::Global::new(tc, v))
        .ok_or_else(|| ("Uncaught".into(), 0, 0, None))
}

fn compile_run<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source: &str,
) -> Option<v8::Local<'s, v8::Value>> {
    let code = v8::String::new(scope, source)?;
    v8::Script::compile(scope, code, None)?.run(scope)
}

pub(crate) type Held = std::collections::HashMap<u32, cv::Value>;

impl Engine {
    /// Runs `f` with a call context and the held values, which a shared isolate lets us read
    /// only while it is locked (so they are lent to the closure, never cloned outside).
    pub(crate) fn with_held<T>(
        &mut self,
        realm: usize,
        f: impl FnOnce(&mut Cx<'_, '_, '_>, &Held) -> T,
    ) -> Option<T> {
        let held = std::mem::take(&mut self.handles);
        let r = self.with_cx(realm, |cx| f(cx, &held));
        self.handles = held;
        r
    }

    pub(crate) fn keep_handle(&mut self, v: cv::Value) -> Handle {
        self.next_handle += 1;
        let h = Handle(self.next_handle);
        self.handles.insert(h.0, v);
        h
    }

    fn finish(&mut self, out: Option<Raw>) -> Result<Handle, Thrown> {
        match out {
            Some(Ok(v)) => Ok(self.keep_handle(v)),
            Some(Err((text, line, column, ex))) => Err(Thrown {
                text,
                line,
                column,
                exception: ex.map(|e| self.keep_handle(e)),
            }),
            None => Err(Thrown {
                text: "Cannot find context with specified id".into(),
                line: 0,
                column: 0,
                exception: None,
            }),
        }
    }

    /// Evaluates an expression (a classic script) in a realm.
    pub fn evaluate(&mut self, realm: usize, source: &str) -> Result<Handle, Thrown> {
        if self.exhausted() || self.entry(realm).is_none() {
            return self.finish(None);
        }
        let (out, fired) =
            self.guarded(|e| e.with_cx(realm, |cx| caught(cx, |tc| compile_run(tc, source))));
        self.terminated(fired, "evaluate");
        self.finish(out)
    }

    /// Calls a function declaration (`(a, b) => …`, `function f() {…}`) with `this` and
    /// arguments, in a realm.
    pub fn call_function(
        &mut self,
        realm: usize,
        declaration: &str,
        this: Option<Handle>,
        args: &[Arg],
    ) -> Result<Handle, Thrown> {
        if self.exhausted() || self.entry(realm).is_none() {
            return self.finish(None);
        }
        let source = format!("({declaration}\n)");
        let (out, fired) = self.guarded(|e| {
            e.with_held(realm, |cx, held| {
                caught(cx, |tc| {
                    let f = compile_run(tc, &source)?;
                    let Ok(f) = v8::Local::<v8::Function>::try_from(f) else {
                        let msg = v8::String::new(
                            tc,
                            "Given expression does not evaluate to a function",
                        )?;
                        let ex = v8::Exception::type_error(tc, msg);
                        tc.throw_exception(ex);
                        return None;
                    };
                    let recv: v8::Local<v8::Value> = match this.and_then(|t| held.get(&t.0)) {
                        Some(t) => v8::Local::new(tc, t),
                        None => tc.get_current_context().global(tc).into(),
                    };
                    let mut values = Vec::with_capacity(args.len());
                    for a in args {
                        values.push(match a {
                            Arg::Json(j) => {
                                let s = v8::String::new(tc, &j.to_string())?;
                                v8::json::parse(tc, s)?
                            }
                            Arg::Unserializable(u) => compile_run(tc, u)?,
                            Arg::Handle(h) => match held.get(&h.0) {
                                Some(g) => v8::Local::new(tc, g),
                                None => {
                                    let msg =
                                        v8::String::new(tc, "Could not find object with given id")?;
                                    let ex = v8::Exception::error(tc, msg);
                                    tc.throw_exception(ex);
                                    return None;
                                }
                            },
                            Arg::Undefined => v8::undefined(tc).into(),
                        });
                    }
                    f.call(tc, recv, &values)
                })
            })
        });
        self.terminated(fired, "callFunctionOn");
        self.finish(out)
    }

    /// The tools a realm's page registered (WebMCP), with their provenance.
    pub fn tools(&self, realm: usize) -> Vec<crate::ToolInfo> {
        self.realm(realm)
            .map(|r| r.st.borrow().tools.iter().map(|t| t.info.clone()).collect())
            .unwrap_or_default()
    }

    /// Calls the page's tool `name`: its `execute(input, {signal})`, with `input`
    /// parsed in the realm (it must be a JSON object). Returns what `execute` returned (a
    /// promise, usually); the caller settles and reads it.
    pub fn call_tool(&mut self, realm: usize, name: &str, input: &Json) -> Result<Handle, Thrown> {
        if self.exhausted() || self.entry(realm).is_none() {
            return self.finish(None);
        }
        let text = input.to_string();
        let (out, fired) = self.guarded(|e| {
            e.with_cx(realm, |cx| {
                let exec = cx
                    .st()
                    .tools
                    .iter()
                    .find(|t| t.info.name == name)
                    .map(|t| t.execute.clone());
                let Some(exec) = exec else {
                    return Err((format!("no tool {name:?} on the page"), 0, 0, None));
                };
                let signal = crate::dom::net::fresh_signal(cx);
                let signal = cx.wrap(signal);
                let signal = cx.keep(signal.into());
                caught(cx, |tc| {
                    let f = v8::Local::<v8::Function>::try_from(v8::Local::new(tc, &exec)).ok()?;
                    let s = v8::String::new(tc, &text)?;
                    let parsed = v8::json::parse(tc, s)?;
                    if !parsed.is_object() || parsed.is_array() || parsed.is_null() {
                        let msg = v8::String::new(tc, "the input must be a JSON object")?;
                        let ex = v8::Exception::type_error(tc, msg);
                        tc.throw_exception(ex);
                        return None;
                    }
                    let options = v8::Object::new(tc);
                    let key = v8::String::new(tc, "signal")?;
                    let sv = v8::Local::new(tc, &signal);
                    options.set(tc, key.into(), sv)?;
                    let recv = v8::undefined(tc).into();
                    f.call(tc, recv, &[parsed, options.into()])
                })
            })
        });
        self.terminated(fired, "a tool");
        self.finish(out)
    }

    /// Describes a held value as a CDP `RemoteObject` (without `objectId`); by value, objects
    /// are serialized as JSON (`Err` when they cannot be).
    pub fn describe(&mut self, realm: usize, h: Handle, by_value: bool) -> Result<Json, String> {
        self.with_held(realm, |cx, held| {
            let v = held
                .get(&h.0)
                .ok_or("Could not find object with given id")?;
            let l = cx.local(v);
            remote(cx, l, by_value)
        })
        .unwrap_or_else(|| Err("Cannot find context with specified id".into()))
    }

    /// The own enumerable properties of a held object, as new handles.
    pub fn properties(&mut self, realm: usize, h: Handle) -> Vec<(String, Handle)> {
        let found = self
            .with_held(realm, |cx, held| {
                let Some(v) = held.get(&h.0) else {
                    return Vec::new();
                };
                let l = cx.local(v);
                let Ok(o) = v8::Local::<v8::Object>::try_from(l) else {
                    return Vec::new();
                };
                let Some(names) = o.get_own_property_names(cx.scope, Default::default()) else {
                    return Vec::new();
                };
                let mut out = Vec::new();
                for i in 0..names.length() {
                    let Some(k) = names.get_index(cx.scope, i) else {
                        continue;
                    };
                    let Some(val) = o.get(cx.scope, k) else {
                        continue;
                    };
                    out.push((k.to_rust_string_lossy(cx.scope), cx.keep(val)));
                }
                out
            })
            .unwrap_or_default();
        found
            .into_iter()
            .map(|(k, v)| (k, self.keep_handle(v)))
            .collect()
    }

    /// The state of a held promise.
    pub fn promise_state(&mut self, realm: usize, h: Handle) -> PromiseState {
        let r = self.with_held(realm, |cx, held| {
            let l = cx.local(held.get(&h.0)?);
            let Ok(p) = v8::Local::<v8::Promise>::try_from(l) else {
                return None;
            };
            Some(match p.state() {
                v8::PromiseState::Pending => (0, None),
                v8::PromiseState::Fulfilled => (1, Some(cx.keep(p.result(cx.scope)))),
                v8::PromiseState::Rejected => (2, Some(cx.keep(p.result(cx.scope)))),
            })
        });
        match r.flatten() {
            None => PromiseState::NotPromise,
            Some((0, _)) => PromiseState::Pending,
            Some((1, Some(v))) => PromiseState::Fulfilled(self.keep_handle(v)),
            Some((_, Some(v))) => PromiseState::Rejected(self.keep_handle(v)),
            Some(_) => PromiseState::Pending,
        }
    }

    /// The DOM node a held value wraps.
    pub fn node_of(&mut self, realm: usize, h: Handle) -> Option<NodeId> {
        self.with_held(realm, |cx, held| {
            let l = cx.local(held.get(&h.0)?);
            match cv::unwrap_any(cx, l) {
                Some((cv::Obj::Node(n), _)) => Some(n),
                _ => None,
            }
        })
        .flatten()
    }

    /// A handle to a node's wrapper in a realm (`None` if the node is gone).
    pub fn handle_for_node(&mut self, realm: usize, node: NodeId) -> Option<Handle> {
        let v = self
            .with_cx(realm, |cx| {
                cx.dom().get(node)?;
                let w = cx.wrap(cv::Obj::Node(node));
                Some(cx.keep(w.into()))
            })
            .flatten()?;
        Some(self.keep_handle(v))
    }

    /// Installs `globalThis[name](payload)`, which queues a call for
    /// [`take_binding_calls`](Self::take_binding_calls).
    pub fn add_binding(&mut self, realm: usize, name: &str) {
        self.with_cx(realm, |cx| {
            let scope = &mut *cx.scope;
            let (Some(key), Some(data)) =
                (v8::String::new(scope, name), v8::String::new(scope, name))
            else {
                return;
            };
            if let Some(f) = v8::Function::builder(binding_called)
                .data(data.into())
                .build(scope)
            {
                let global = scope.get_current_context().global(scope);
                global.set(scope, key.into(), f.into());
            }
        });
    }

    /// Removes a binding installed with [`add_binding`](Self::add_binding).
    pub fn remove_binding(&mut self, realm: usize, name: &str) {
        self.with_cx(realm, |cx| {
            let key = cx.str(name);
            let global = cx.scope.get_current_context().global(cx.scope);
            global.delete(cx.scope, key.into());
        });
    }

    /// Binding calls since the last take: (realm, name, payload).
    pub fn take_binding_calls(&mut self) -> Vec<(usize, String, String)> {
        std::mem::take(&mut self.shared.borrow_mut().bindings)
    }

    /// Releases a held value.
    pub fn release(&mut self, h: Handle) {
        self.handles.remove(&h.0);
    }
}

fn binding_called(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let Some(r) = crate::realm::current(scope) else {
        return;
    };
    let name = args.data().to_rust_string_lossy(scope);
    let payload = args
        .get(0)
        .to_string(scope)
        .map(|s| s.to_rust_string_lossy(scope))
        .unwrap_or_default();
    let mut sh = r.shared.borrow_mut();
    if sh.bindings.len() < 10_000 {
        sh.bindings.push((r.id, name, payload));
    }
}

/// Chrome's short description of a node (`input#q.a.b`, `#document`, `#text`).
fn node_description(cx: &Cx<'_, '_, '_>, n: NodeId) -> String {
    let dom = cx.dom();
    let Some(node) = dom.get(n) else {
        return "#node".into();
    };
    match &node.data {
        NodeData::Document => "#document".into(),
        NodeData::DocumentFragment | NodeData::ShadowRoot { .. } => "#document-fragment".into(),
        NodeData::Doctype { name, .. } => name.clone(),
        NodeData::Text(_) => "#text".into(),
        NodeData::Comment(_) => "#comment".into(),
        NodeData::ProcessingInstruction { target, .. } => target.clone(),
        NodeData::Element { .. } => {
            let mut d = node.local_name().unwrap_or_default().to_string();
            if let Some(id) = node.attr("id").filter(|i| !i.is_empty()) {
                d.push('#');
                d.push_str(id);
            }
            if let Some(c) = node.attr("class") {
                for c in c.split_ascii_whitespace() {
                    d.push('.');
                    d.push_str(c);
                }
            }
            d
        }
    }
}

fn number(n: f64, desc: String) -> Json {
    if n.is_nan() {
        json!({"type": "number", "unserializableValue": "NaN", "description": "NaN"})
    } else if n.is_infinite() {
        let s = if n > 0.0 { "Infinity" } else { "-Infinity" };
        json!({"type": "number", "unserializableValue": s, "description": s})
    } else if n == 0.0 && n.is_sign_negative() {
        json!({"type": "number", "unserializableValue": "-0", "description": "-0"})
    } else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        // Integral values go on the wire as integers (`2`, not `2.0`), as Chrome sends them.
        json!({"type": "number", "value": n as i64, "description": desc})
    } else {
        json!({"type": "number", "value": n, "description": desc})
    }
}

/// A CDP `RemoteObject` for a value.
fn remote(
    cx: &mut Cx<'_, '_, '_>,
    v: v8::Local<'_, v8::Value>,
    by_value: bool,
) -> Result<Json, String> {
    let scope = &mut *cx.scope;
    let text = |scope: &mut v8::PinScope<'_, '_>, v: v8::Local<'_, v8::Value>| {
        v.to_string(scope)
            .map(|s| s.to_rust_string_lossy(scope))
            .unwrap_or_default()
    };
    if v.is_undefined() {
        return Ok(json!({"type": "undefined"}));
    }
    if v.is_null() {
        return Ok(json!({"type": "object", "subtype": "null", "value": null}));
    }
    if v.is_boolean() {
        return Ok(json!({"type": "boolean", "value": v.boolean_value(scope)}));
    }
    if v.is_number() {
        let n = v.number_value(scope).unwrap_or(f64::NAN);
        let d = text(scope, v);
        return Ok(number(n, d));
    }
    if v.is_string() {
        return Ok(json!({"type": "string", "value": text(scope, v)}));
    }
    if v.is_big_int() {
        let d = format!("{}n", text(scope, v));
        return Ok(json!({"type": "bigint", "unserializableValue": d, "description": d}));
    }
    if v.is_symbol() {
        let d = v8::Local::<v8::Symbol>::try_from(v)
            .ok()
            .map(|s| {
                let desc = s.description(scope);
                let inner = if desc.is_undefined() {
                    String::new()
                } else {
                    text(scope, desc)
                };
                format!("Symbol({inner})")
            })
            .unwrap_or_else(|| "Symbol()".into());
        return Ok(json!({"type": "symbol", "description": d}));
    }
    if v.is_function() {
        if by_value {
            return Ok(json!({"type": "function", "value": {}}));
        }
        return Ok(
            json!({"type": "function", "className": "Function", "description": text(scope, v)}),
        );
    }
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
        return Ok(json!({"type": "undefined"}));
    };
    if by_value {
        // DOM wrappers serialize as `{}` (as in Chrome); other objects through JSON.
        if cv::unwrap_any(cx, v).is_some() {
            return Ok(json!({"type": "object", "value": {}}));
        }
        let scope = &mut *cx.scope;
        v8::tc_scope!(let tc, scope);
        let s = v8::json::stringify(tc, v);
        if tc.has_caught() {
            return Err("Object couldn't be returned by value".into());
        }
        let value: Json = s
            .map(|s| s.to_rust_string_lossy(tc))
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(json!({}));
        return Ok(json!({"type": "object", "value": value}));
    }
    if let Some((obj, iface)) = cv::unwrap_any(cx, v) {
        return Ok(match obj {
            cv::Obj::Node(n) => json!({
                "type": "object",
                "subtype": "node",
                "className": iface.name(),
                "description": node_description(cx, n),
            }),
            _ => json!({"type": "object", "className": iface.name(), "description": iface.name()}),
        });
    }
    let scope = &mut *cx.scope;
    let global = scope.get_current_context().global(scope);
    if o.strict_equals(global.into()) {
        return Ok(json!({"type": "object", "className": "Window", "description": "Window"}));
    }
    let class = o.get_constructor_name().to_rust_string_lossy(scope);
    let (subtype, description) = if v.is_array() {
        let n = v8::Local::<v8::Array>::try_from(v)
            .map(|a| a.length())
            .unwrap_or(0);
        (Some("array"), format!("Array({n})"))
    } else if v.is_native_error() {
        let stack_key = v8::String::new(scope, "stack");
        let stack = stack_key
            .and_then(|k| o.get(scope, k.into()))
            .filter(|s| s.is_string())
            .map(|s| text(scope, s));
        (Some("error"), stack.unwrap_or_else(|| text(scope, v)))
    } else if v.is_promise() {
        (Some("promise"), "Promise".into())
    } else if v.is_reg_exp() {
        (Some("regexp"), text(scope, v))
    } else if v.is_date() {
        (Some("date"), text(scope, v))
    } else if v.is_map() {
        (Some("map"), "Map".into())
    } else if v.is_set() {
        (Some("set"), "Set".into())
    } else if v.is_proxy() {
        (Some("proxy"), "Proxy".into())
    } else {
        (None, class.clone())
    };
    let mut r = json!({"type": "object", "className": class, "description": description});
    if let Some(s) = subtype {
        r["subtype"] = json!(s);
    }
    Ok(r)
}
