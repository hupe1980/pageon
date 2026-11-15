//! Interceptors the IDL describes in prose: indexed access on lists and collections, CSS
//! properties on `style` objects (`el.style.display`), and `dataset` (`el.dataset.fooBar`).

use crate::cv::{self, Cx, Obj, Out};
use crate::generated::Iface;
use crate::realm::Slot;

/// Adds the interceptors of an interface to its template.
pub fn template(
    scope: &mut v8::PinScope<'_, '_, ()>,
    iface: Iface,
    f: v8::Local<'_, v8::FunctionTemplate>,
) {
    let inst = f.instance_template(scope);
    // Web IDL: a value iterator (`iterable<Node>`) takes `entries`, `keys`, `values`, `forEach`
    // and `@@iterator` from %Array.prototype%; any interface with an indexed getter and a
    // `length` gets `@@iterator` = %Array.prototype.values%.
    let proto = f.prototype_template(scope);
    let iterator = v8::Symbol::get_iterator(scope);
    match iface {
        Iface::NodeList | Iface::DOMTokenList => {
            for (name, intrinsic) in [
                ("entries", v8::Intrinsic::ArrayProtoEntries),
                ("keys", v8::Intrinsic::ArrayProtoKeys),
                ("values", v8::Intrinsic::ArrayProtoValues),
                ("forEach", v8::Intrinsic::ArrayProtoForEach),
            ] {
                let key = v8::String::new(scope, name).unwrap_or_else(|| v8::String::empty(scope));
                proto.set_intrinsic_data_property(
                    key.into(),
                    intrinsic,
                    v8::PropertyAttribute::DONT_ENUM,
                );
            }
            proto.set_intrinsic_data_property(
                iterator.into(),
                v8::Intrinsic::ArrayProtoValues,
                v8::PropertyAttribute::DONT_ENUM,
            );
        }
        Iface::HTMLCollection
        | Iface::HTMLFormControlsCollection
        | Iface::HTMLOptionsCollection
        | Iface::DOMRectList
        | Iface::CSSStyleDeclaration
        | Iface::FileList
        | Iface::DataTransferItemList
        | Iface::NamedNodeMap => {
            proto.set_intrinsic_data_property(
                iterator.into(),
                v8::Intrinsic::ArrayProtoValues,
                v8::PropertyAttribute::DONT_ENUM,
            );
        }
        _ => {}
    }
    match iface {
        Iface::NodeList
        | Iface::HTMLCollection
        | Iface::HTMLFormControlsCollection
        | Iface::HTMLOptionsCollection
        | Iface::DOMTokenList
        | Iface::DOMRectList
        | Iface::FileList
        | Iface::CSSStyleDeclaration => {
            inst.set_indexed_property_handler(
                v8::IndexedPropertyHandlerConfiguration::new().getter(indexed_get),
            );
        }
        Iface::Worker => {
            // `postMessage(message, transfer)` and `postMessage(message, options)`.
            cv::method(
                scope,
                proto.into(),
                "postMessage",
                crate::dom::workers::worker_post_message,
                1,
            );
        }
        Iface::DedicatedWorkerGlobalScope => {
            cv::method(
                scope,
                inst.into(),
                "postMessage",
                crate::dom::workers::global_post_message,
                1,
            );
        }
        Iface::MessagePort => {
            // `postMessage(message, transfer)` and `postMessage(message, options)`.
            cv::method(
                scope,
                proto.into(),
                "postMessage",
                crate::dom::message_port::post_message_any,
                1,
            );
        }
        Iface::HTMLInputElement | Iface::HTMLTextAreaElement => {
            cv::method(
                scope,
                proto.into(),
                "setRangeText",
                crate::dom::editing::set_range_text_any,
                1,
            );
        }
        Iface::Performance => {
            // `measure` takes a mark name or an options object: one method reads both.
            cv::method(
                scope,
                proto.into(),
                "measure",
                crate::dom::timing::measure_any,
                1,
            );
        }
        Iface::DataTransferItemList => {
            inst.set_indexed_property_handler(
                v8::IndexedPropertyHandlerConfiguration::new().getter(indexed_get),
            );
            // `add` has overloads of different lengths: one method takes both.
            cv::method(
                scope,
                proto.into(),
                "add",
                crate::dom::data_transfer_item_list::add_any,
                1,
            );
        }
        Iface::CSSStyleProperties => {
            inst.set_indexed_property_handler(
                v8::IndexedPropertyHandlerConfiguration::new().getter(indexed_get),
            );
            inst.set_named_property_handler(
                v8::NamedPropertyHandlerConfiguration::new()
                    .getter(style_get)
                    .setter(style_set)
                    .query(style_query),
            );
        }
        Iface::NamedNodeMap => {
            inst.set_indexed_property_handler(
                v8::IndexedPropertyHandlerConfiguration::new().getter(indexed_get),
            );
            inst.set_named_property_handler(
                v8::NamedPropertyHandlerConfiguration::new().getter(attr_get),
            );
        }
        Iface::Window => {
            // `window[i]`: the child frames' windows.
            inst.set_indexed_property_handler(
                v8::IndexedPropertyHandlerConfiguration::new()
                    .getter(crate::frames::window::indexed),
            );
            // Named access on the window (`window.out` for `<div id=out>`), only when no real
            // property of that name exists.
            inst.set_named_property_handler(
                v8::NamedPropertyHandlerConfiguration::new()
                    .getter(window_named)
                    .flags(
                        v8::PropertyHandlerFlags::NON_MASKING
                            | v8::PropertyHandlerFlags::ONLY_INTERCEPT_STRINGS,
                    ),
            );
        }
        Iface::Storage => {
            // `localStorage.key`: items as properties, never shadowing the methods.
            inst.set_named_property_handler(
                v8::NamedPropertyHandlerConfiguration::new()
                    .getter(storage_get)
                    .setter(storage_set)
                    .query(storage_query)
                    .deleter(storage_delete)
                    .enumerator(storage_keys)
                    .flags(
                        v8::PropertyHandlerFlags::NON_MASKING
                            | v8::PropertyHandlerFlags::ONLY_INTERCEPT_STRINGS,
                    ),
            );
        }
        Iface::DOMStringMap => {
            inst.set_named_property_handler(
                v8::NamedPropertyHandlerConfiguration::new()
                    .getter(data_get)
                    .setter(data_set)
                    .query(data_query)
                    .deleter(data_delete)
                    .enumerator(data_keys),
            );
        }
        _ => {}
    }
}

fn with_cx<T>(
    scope: &mut v8::PinScope<'_, '_>,
    this: v8::Local<'_, v8::Object>,
    iface: Iface,
    f: impl FnOnce(&mut Cx<'_, '_, '_>, Obj) -> Option<T>,
) -> Option<T> {
    let realm = crate::realm::current(scope)?;
    let world = crate::realm::current_world(scope);
    let mut cx = Cx {
        scope,
        realm,
        world,
    };
    let obj = cv::unwrap(&mut cx, this.into(), iface).ok()?;
    f(&mut cx, obj)
}

fn indexed_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    index: u32,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let r = with_cx_any(scope, args.holder(), |cx, obj, iface| {
        let i = index as usize;
        match iface {
            Iface::DOMTokenList => crate::dom::dom_token_list::item(cx, obj, index)
                .ok()
                .flatten()
                .map(Out::Str),
            Iface::DOMRectList => crate::dom::dom_rect_list::item(cx, obj, index)
                .ok()
                .flatten()
                .map(Out::Obj),
            Iface::FileList => crate::dom::file_list::item(cx, obj, index)
                .ok()
                .flatten()
                .map(Out::Obj),
            Iface::DataTransferItemList => {
                crate::dom::data_transfer_item_list::item(cx, obj, index)
                    .ok()
                    .flatten()
                    .map(Out::Obj)
            }
            Iface::NamedNodeMap => crate::dom::named_node_map::item(cx, obj, index)
                .ok()
                .flatten()
                .map(Out::Obj),
            Iface::CSSStyleDeclaration | Iface::CSSStyleProperties => {
                crate::dom::css_style_declaration::item(cx, obj, index)
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(Out::Str)
            }
            _ => super::collections::items(cx, obj)
                .ok()
                .and_then(|v| v.get(i).copied())
                .map(|n| Out::Obj(Obj::Node(n))),
        }
        .map(|o| cx.js_value(o))
        .map(|v| {
            rv.set(v8::Local::new(&*cx.scope, v));
        })
    });
    if r.is_some() {
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn with_cx_any<T>(
    scope: &mut v8::PinScope<'_, '_>,
    this: v8::Local<'_, v8::Object>,
    f: impl FnOnce(&mut Cx<'_, '_, '_>, Obj, Iface) -> Option<T>,
) -> Option<T> {
    let realm = crate::realm::current(scope)?;
    let world = crate::realm::current_world(scope);
    let mut cx = Cx {
        scope,
        realm,
        world,
    };
    let (obj, iface) = cv::unwrap_any(&mut cx, this.into())?;
    f(&mut cx, obj, iface)
}

fn key_string(scope: &mut v8::PinScope<'_, '_>, key: v8::Local<'_, v8::Name>) -> Option<String> {
    if key.is_symbol() {
        return None;
    }
    Some(key.to_rust_string_lossy(scope))
}

fn style_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else {
        return v8::Intercepted::kNo;
    };
    let Some(css) = super::collections::css_name(&name)
        .filter(|_| !name.contains('-') || name.starts_with("--"))
    else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(
        scope,
        args.holder(),
        Iface::CSSStyleDeclaration,
        |cx, obj| {
            let d = super::collections::decl(cx, obj).ok()?;
            let v = super::collections::property(cx, d, &css);
            let s = cx.str(&v);
            rv.set(v8::Local::new(&*cx.scope, s).into());
            Some(())
        },
    );
    if r.is_some() {
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn style_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    value: v8::Local<'s, v8::Value>,
    args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else {
        return v8::Intercepted::kNo;
    };
    let Some(css) = super::collections::css_name(&name) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(
        scope,
        args.holder(),
        Iface::CSSStyleDeclaration,
        |cx, obj| {
            let v = if value.is_null() {
                String::new()
            } else {
                cv::string(cx, value).ok()?
            };
            let d = super::collections::decl(cx, obj).ok()?;
            if let Err(e) = super::collections::set_property_value(cx, d, &css, &v, false) {
                cv::throw(cx, e);
            }
            Some(())
        },
    );
    if r.is_some() {
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn style_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Integer>,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else {
        return v8::Intercepted::kNo;
    };
    if super::collections::css_name(&name).is_some() {
        rv.set_int32(0);
        return v8::Intercepted::kYes;
    }
    v8::Intercepted::kNo
}

/// `fooBar` → `data-foo-bar` (`None` if the name is not a valid dataset key).
fn data_attr(name: &str) -> Option<String> {
    if name
        .chars()
        .zip(name.chars().skip(1))
        .any(|(a, b)| a == '-' && b.is_ascii_lowercase())
    {
        return None;
    }
    let mut s = String::from("data-");
    for c in name.chars() {
        if c.is_ascii_uppercase() {
            s.push('-');
            s.push(c.to_ascii_lowercase());
        } else {
            s.push(c);
        }
    }
    Some(s)
}

fn data_key(attr: &str) -> Option<String> {
    let rest = attr.strip_prefix("data-")?;
    let mut out = String::new();
    let mut upper = false;
    for c in rest.chars() {
        if c == '-' {
            upper = true;
        } else if upper && c.is_ascii_lowercase() {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            if upper {
                out.push('-');
            }
            out.push(c);
            upper = false;
        }
    }
    Some(out)
}

fn dataset_node(cx: &Cx<'_, '_, '_>, obj: Obj) -> Option<pageon_dom::NodeId> {
    match cx.realm.st.borrow().slot(obj.slab().ok()?) {
        Some(Slot::DataSet(n)) => Some(*n),
        _ => None,
    }
}

fn data_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let Some(attr) = key_string(scope, key).and_then(|k| data_attr(&k)) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(scope, args.holder(), Iface::DOMStringMap, |cx, obj| {
        let n = dataset_node(cx, obj)?;
        let v = cx
            .dom()
            .get(n)
            .and_then(|x| x.attr(&attr).map(str::to_string))?;
        let s = cx.str(&v);
        rv.set(v8::Local::new(&*cx.scope, s).into());
        Some(())
    });
    if r.is_some() {
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn data_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    value: v8::Local<'s, v8::Value>,
    args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let Some(attr) = key_string(scope, key).and_then(|k| data_attr(&k)) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(scope, args.holder(), Iface::DOMStringMap, |cx, obj| {
        let n = dataset_node(cx, obj)?;
        let v = cv::string(cx, value).ok()?;
        cx.dom_mut().set_attr(n, &attr, &v).ok()
    });
    if r.is_some() {
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn data_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Integer>,
) -> v8::Intercepted {
    let Some(attr) = key_string(scope, key).and_then(|k| data_attr(&k)) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(scope, args.holder(), Iface::DOMStringMap, |cx, obj| {
        let n = dataset_node(cx, obj)?;
        cx.dom().get(n).and_then(|x| x.attr(&attr)).map(|_| ())
    });
    if r.is_some() {
        rv.set_int32(0);
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn data_delete<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let Some(attr) = key_string(scope, key).and_then(|k| data_attr(&k)) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(scope, args.holder(), Iface::DOMStringMap, |cx, obj| {
        let n = dataset_node(cx, obj)?;
        cx.dom_mut().remove_attr(n, &attr).ok()
    });
    if r.is_some() {
        rv.set_bool(true);
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn data_keys<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Array>,
) {
    let _ = with_cx(scope, args.holder(), Iface::DOMStringMap, |cx, obj| {
        let n = dataset_node(cx, obj)?;
        let keys: Vec<String> = cx
            .dom()
            .get(n)
            .map(|x| {
                x.attrs()
                    .iter()
                    .filter_map(|a| data_key(&a.qualified_name()))
                    .collect()
            })
            .unwrap_or_default();
        let values: Vec<v8::Local<v8::Value>> = keys.iter().map(|k| cx.str(k).into()).collect();
        let arr = v8::Array::new_with_elements(cx.scope, &values);
        rv.set(v8::Local::new(&*cx.scope, arr));
        Some(())
    });
}

fn attr_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(scope, args.holder(), Iface::NamedNodeMap, |cx, obj| {
        let attr = crate::dom::named_node_map::get_named_item(cx, obj, name)
            .ok()
            .flatten()?;
        let w = cx.wrap(attr);
        rv.set(v8::Local::new(&*cx.scope, w).into());
        Some(())
    });
    if r.is_some() {
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

/// `window[name]`: the first element with that `id`, or a named `a`/`embed`/`form`/`iframe`/
/// `img`/`object` element.
fn window_named<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    _args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let Some(name) = key_string(scope, key).filter(|n| !n.is_empty()) else {
        return v8::Intercepted::kNo;
    };
    let Some(realm) = crate::realm::current(scope) else {
        return v8::Intercepted::kNo;
    };
    let found = {
        let dom = realm.dom.borrow();
        let by_id = dom.traverse(dom.root()).find_map(|e| match e {
            pageon_dom::Edge::Enter(n)
                if dom.get(n).and_then(|x| x.attr("id")) == Some(name.as_str()) =>
            {
                Some(n)
            }
            _ => None,
        });
        by_id.or_else(|| {
            dom.traverse(dom.root()).find_map(|e| match e {
                pageon_dom::Edge::Enter(n)
                    if dom.get(n).is_some_and(|x| {
                        x.attr("name") == Some(name.as_str())
                            && ["a", "embed", "form", "iframe", "img", "object"]
                                .iter()
                                .any(|t| x.is_html(t))
                    }) =>
                {
                    Some(n)
                }
                _ => None,
            })
        })
    };
    let Some(node) = found else {
        return v8::Intercepted::kNo;
    };
    let world = crate::realm::current_world(scope);
    let mut cx = Cx {
        scope,
        realm,
        world,
    };
    let w = cx.wrap(Obj::Node(node));
    rv.set(v8::Local::new(&*cx.scope, w).into());
    v8::Intercepted::kYes
}

fn storage_get<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Value>,
) -> v8::Intercepted {
    let Some(k) = key_string(scope, key) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(scope, args.holder(), Iface::Storage, |cx, obj| {
        let v = super::webstorage::area(cx, obj, |a| a.get(&k).map(str::to_string)).ok()??;
        let s = cx.str(&v);
        rv.set(v8::Local::new(&*cx.scope, s).into());
        Some(())
    });
    if r.is_some() {
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn storage_set<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    value: v8::Local<'s, v8::Value>,
    args: v8::PropertyCallbackArguments<'s>,
    _rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let Some(k) = key_string(scope, key) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(scope, args.holder(), Iface::Storage, |cx, obj| {
        let v = cv::string(cx, value).ok()?;
        if let Err(e) = super::webstorage::set(cx, obj, &k, &v) {
            cv::throw(cx, e);
        }
        Some(())
    });
    if r.is_some() {
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn storage_query<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Integer>,
) -> v8::Intercepted {
    let Some(k) = key_string(scope, key) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(scope, args.holder(), Iface::Storage, |cx, obj| {
        super::webstorage::area(cx, obj, |a| a.get(&k).map(|_| ())).ok()?
    });
    if r.is_some() {
        rv.set_int32(0);
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn storage_delete<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    key: v8::Local<'s, v8::Name>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Boolean>,
) -> v8::Intercepted {
    let Some(k) = key_string(scope, key) else {
        return v8::Intercepted::kNo;
    };
    let r = with_cx(scope, args.holder(), Iface::Storage, |cx, obj| {
        super::webstorage::area(cx, obj, |a| {
            let had = a.get(&k).is_some();
            a.remove(&k);
            had.then_some(())
        })
        .ok()?
    });
    if r.is_some() {
        rv.set_bool(true);
        v8::Intercepted::kYes
    } else {
        v8::Intercepted::kNo
    }
}

fn storage_keys<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    args: v8::PropertyCallbackArguments<'s>,
    mut rv: v8::ReturnValue<v8::Array>,
) {
    let _ = with_cx(scope, args.holder(), Iface::Storage, |cx, obj| {
        let keys: Vec<String> =
            super::webstorage::area(cx, obj, |a| a.keys().map(str::to_string).collect()).ok()?;
        let values: Vec<v8::Local<v8::Value>> = keys.iter().map(|k| cx.str(k).into()).collect();
        let arr = v8::Array::new_with_elements(cx.scope, &values);
        rv.set(v8::Local::new(&*cx.scope, arr));
        Some(())
    });
}
