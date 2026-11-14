//! WebDriver BiDi values: deep serialization of a held value to a RemoteValue, in
//! the shape Chrome's CDP `deepSerializedValue` uses, and materialization of a LocalValue.
//! Both run in Rust against V8, so no helper code is visible to the page.
//!
//! Nodes are serialized with an engine key (`"node": [realm, index, generation]`) that the
//! protocol layer turns into its own ids (`backendNodeId`, `sharedId`); windows carry
//! `"realm": index` the same way.

use pageon_dom::{NodeData, NodeId};
use serde_json::{Value as Json, json};

use crate::cv::{self, Cx};
use crate::engine::Engine;
use crate::eval::Handle;
use crate::generated::Iface;

/// A BiDi `LocalValue`, with references already resolved to held values.
#[derive(Debug, Clone)]
pub enum LocalValue {
    /// `undefined`.
    Undefined,
    /// `null`.
    Null,
    /// A string.
    String(String),
    /// A number (any `f64`, including `NaN`, `-0` and infinities).
    Number(f64),
    /// A boolean.
    Bool(bool),
    /// A BigInt, as its decimal text.
    BigInt(String),
    /// An array.
    Array(Vec<LocalValue>),
    /// An object: string keys or values as keys.
    Object(Vec<(LocalKey, LocalValue)>),
    /// A map.
    Map(Vec<(LocalKey, LocalValue)>),
    /// A set.
    Set(Vec<LocalValue>),
    /// A date (ISO 8601 text).
    Date(String),
    /// A regular expression.
    RegExp {
        /// Source.
        pattern: String,
        /// Flags.
        flags: String,
    },
    /// A held value (a BiDi `handle` or `sharedId`).
    Handle(Handle),
}

/// A key of a [`LocalValue::Object`] or [`LocalValue::Map`].
#[derive(Debug, Clone)]
pub enum LocalKey {
    /// A string key.
    String(String),
    /// A value as key.
    Value(LocalValue),
}

/// Limits of a deep serialization.
#[derive(Debug, Clone, Copy)]
pub struct Depth {
    /// Object depth (`None`: unlimited).
    pub objects: Option<u32>,
    /// Node depth (children serialized below the top node).
    pub nodes: u32,
}

struct Walk {
    /// Objects already serialized: (identity hash, value, index in `ids`).
    seen: Vec<(i32, v8::Global<v8::Object>, usize)>,
    /// Which seen objects repeat (and so get a `weakLocalObjectReference`).
    repeated: Vec<bool>,
    realm: usize,
    budget: u32,
}

impl Engine {
    /// The deep serialization of a held value (Chrome's `deepSerializedValue`).
    pub fn deep(&mut self, realm: usize, h: Handle, depth: Depth) -> Result<Json, String> {
        self.with_held(realm, |cx, held| {
            let v = held
                .get(&h.0)
                .ok_or("Could not find object with given id")?;
            let mut w = Walk {
                seen: Vec::new(),
                repeated: Vec::new(),
                realm,
                budget: 100_000,
            };
            let l = cx.local(v);
            let mut out = value(cx, l, depth.objects, depth.nodes, &mut w);
            finish(&mut out, &w.repeated);
            Ok(out)
        })
        .unwrap_or_else(|| Err("Cannot find context with specified id".into()))
    }

    /// Builds a [`LocalValue`] in a realm and holds it.
    pub fn materialize(&mut self, realm: usize, v: &LocalValue) -> Result<Handle, String> {
        let r = self
            .with_held(realm, |cx, held| {
                let l = local(cx, v, held)?;
                Ok::<_, String>(cx.keep(l))
            })
            .unwrap_or_else(|| Err("Cannot find context with specified id".into()))?;
        Ok(self.keep_handle(r))
    }
}

/// Marks first occurrences of repeated objects and strips the walk's bookkeeping.
fn finish(v: &mut Json, repeated: &[bool]) {
    match v {
        Json::Object(m) => {
            if let Some(i) = m.remove("_seen").and_then(|i| i.as_u64())
                && repeated.get(i as usize).copied().unwrap_or(false)
            {
                m.insert("weakLocalObjectReference".into(), json!(i + 1));
            }
            for (_, c) in m.iter_mut() {
                finish(c, repeated);
            }
        }
        Json::Array(a) => a.iter_mut().for_each(|c| finish(c, repeated)),
        _ => {}
    }
}

fn text(scope: &mut v8::PinScope<'_, '_>, v: v8::Local<'_, v8::Value>) -> String {
    v.to_string(scope)
        .map(|s| s.to_rust_string_lossy(scope))
        .unwrap_or_default()
}

fn number(n: f64) -> Json {
    if n.is_nan() {
        json!("NaN")
    } else if n.is_infinite() {
        json!(if n > 0.0 { "Infinity" } else { "-Infinity" })
    } else if n == 0.0 && n.is_sign_negative() {
        json!("-0")
    } else if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

/// The ISO 8601 text of a time value (`Date.prototype.toISOString`), computed here so a page
/// that replaces the prototype cannot change it.
pub(crate) fn iso(ms: f64) -> Option<String> {
    if !ms.is_finite() || ms.abs() > 8.64e15 {
        return None;
    }
    let ms = ms as i64;
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, mi, s, milli) = (
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000,
    );
    let year = if (0..=9999).contains(&y) {
        format!("{y:04}")
    } else if y < 0 {
        format!("-{:06}", -y)
    } else {
        format!("+{y:06}")
    };
    Some(format!(
        "{year}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z"
    ))
}

/// Parses the ISO 8601 forms `Date` accepts (`YYYY-MM-DD`, `…THH:MM[:SS[.sss]]`, `Z` or
/// `±HH:MM`) to a time value. Date-only forms are UTC; date-time forms without an offset too
/// (pageon's time zone is UTC).
pub(crate) fn parse_iso(s: &str) -> Option<f64> {
    let s = s.trim();
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' if s.len() > 10 && s.as_bytes().get(7) == Some(&b'-') => (-1, &s[1..]),
        _ => (1, s),
    };
    let (date, time) = rest.split_once('T').unwrap_or((rest, ""));
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: i64 = dp.next().map_or(Some(1), |v| v.parse().ok())?;
    let d: i64 = dp.next().map_or(Some(1), |v| v.parse().ok())?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = sign * y;
    let (clock, offset) = if time.is_empty() {
        ("", 0)
    } else if let Some(c) = time.strip_suffix('Z') {
        (c, 0)
    } else if let Some(i) = time.rfind(['+', '-']) {
        let (c, o) = time.split_at(i);
        let neg = o.starts_with('-');
        let (oh, om) = o[1..]
            .split_once(':')
            .unwrap_or((&o[1..3.min(o.len())], "0"));
        let off = oh.parse::<i64>().ok()? * 60 + om.parse::<i64>().ok()?;
        (c, if neg { -off } else { off })
    } else {
        (time, 0)
    };
    let mut cp = clock.split(':');
    let h: i64 = cp
        .next()
        .filter(|v| !v.is_empty())
        .map_or(Some(0), |v| v.parse().ok())?;
    let mi: i64 = cp.next().map_or(Some(0), |v| v.parse().ok())?;
    let sec: f64 = cp.next().map_or(Some(0.0), |v| v.parse().ok())?;
    // days_from_civil.
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let ms = ((days * 24 + h) * 60 + mi - offset) as f64 * 60_000.0 + (sec * 1000.0).round();
    Some(ms)
}

fn key<'s>(
    cx: &mut Cx<'_, 's, '_>,
    k: v8::Local<'s, v8::Value>,
    depth: Option<u32>,
    nodes: u32,
    w: &mut Walk,
    strings: bool,
) -> Json {
    if strings && k.is_string() {
        json!(text(cx.scope, k))
    } else {
        value(cx, k, depth, nodes, w)
    }
}

fn entries<'s>(
    cx: &mut Cx<'_, 's, '_>,
    pairs: Vec<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>)>,
    depth: Option<u32>,
    nodes: u32,
    w: &mut Walk,
) -> Json {
    let mut out = Vec::with_capacity(pairs.len());
    for (k, v) in pairs {
        let k = key(cx, k, depth, nodes, w, true);
        let v = value(cx, v, depth, nodes, w);
        out.push(json!([k, v]));
    }
    Json::Array(out)
}

fn list<'s>(
    cx: &mut Cx<'_, 's, '_>,
    items: Vec<v8::Local<'s, v8::Value>>,
    depth: Option<u32>,
    nodes: u32,
    w: &mut Walk,
) -> Json {
    Json::Array(
        items
            .into_iter()
            .map(|v| value(cx, v, depth, nodes, w))
            .collect(),
    )
}

fn array_items<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    a: v8::Local<'s, v8::Array>,
) -> Vec<v8::Local<'s, v8::Value>> {
    (0..a.length())
        .map(|i| {
            a.get_index(scope, i)
                .unwrap_or_else(|| v8::undefined(scope).into())
        })
        .collect()
}

/// The indexed items of an array-like platform object (`length` and `[i]`).
fn indexed<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    o: v8::Local<'s, v8::Object>,
) -> Vec<v8::Local<'s, v8::Value>> {
    let Some(k) = v8::String::new(scope, "length") else {
        return Vec::new();
    };
    let n = o
        .get(scope, k.into())
        .and_then(|l| l.uint32_value(scope))
        .unwrap_or(0)
        .min(10_000);
    (0..n).filter_map(|i| o.get_index(scope, i)).collect()
}

fn own_entries<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    o: v8::Local<'s, v8::Object>,
) -> Vec<(v8::Local<'s, v8::Value>, v8::Local<'s, v8::Value>)> {
    let Some(names) = o.get_own_property_names(scope, Default::default()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for i in 0..names.length() {
        let Some(k) = names.get_index(scope, i) else {
            continue;
        };
        v8::tc_scope!(let tc, scope);
        let v = o.get(tc, k).unwrap_or_else(|| v8::undefined(tc).into());
        out.push((k, v));
    }
    out
}

fn value<'s>(
    cx: &mut Cx<'_, 's, '_>,
    v: v8::Local<'s, v8::Value>,
    depth: Option<u32>,
    nodes: u32,
    w: &mut Walk,
) -> Json {
    let scope = &mut *cx.scope;
    if v.is_undefined() {
        return json!({"type": "undefined"});
    }
    if v.is_null() {
        return json!({"type": "null"});
    }
    if v.is_boolean() {
        return json!({"type": "boolean", "value": v.boolean_value(scope)});
    }
    if v.is_number() {
        return json!({"type": "number", "value": number(v.number_value(scope).unwrap_or(f64::NAN))});
    }
    if v.is_string() {
        return json!({"type": "string", "value": text(scope, v)});
    }
    if v.is_big_int() {
        return json!({"type": "bigint", "value": text(scope, v)});
    }
    if v.is_symbol() {
        return json!({"type": "symbol"});
    }
    let Ok(o) = v8::Local::<v8::Object>::try_from(v) else {
        return json!({"type": "undefined"});
    };
    // Repeated objects (and cycles) are referenced, not walked again.
    let hash = o.get_identity_hash().get();
    if let Some(&(_, _, i)) = w
        .seen
        .iter()
        .find(|(h, g, _)| *h == hash && v8::Local::new(scope, g).strict_equals(v))
    {
        w.repeated[i] = true;
        let ty = kind(cx, v, o);
        return json!({"type": ty, "weakLocalObjectReference": i + 1});
    }
    let i = w.seen.len();
    w.seen.push((hash, v8::Global::new(scope, o), i));
    w.repeated.push(false);
    let ty = kind(cx, v, o);
    let mut out = json!({"type": ty, "_seen": i});
    if w.budget == 0 {
        return out;
    }
    w.budget -= 1;
    let deeper = depth.map(|d| d.saturating_sub(1));
    let room = depth != Some(0);
    let scope = &mut *cx.scope;
    match ty {
        "array" if room => {
            let a = v8::Local::<v8::Array>::try_from(v).ok();
            let items = a.map(|a| array_items(scope, a)).unwrap_or_default();
            out["value"] = list(cx, items, deeper, nodes, w);
        }
        "set" if room => {
            let a = v8::Local::<v8::Set>::try_from(v)
                .ok()
                .map(|s| s.as_array(scope));
            let items = a.map(|a| array_items(scope, a)).unwrap_or_default();
            out["value"] = list(cx, items, deeper, nodes, w);
        }
        "map" if room => {
            let a = v8::Local::<v8::Map>::try_from(v)
                .ok()
                .map(|m| m.as_array(scope));
            let flat = a.map(|a| array_items(scope, a)).unwrap_or_default();
            let pairs = flat.chunks(2).map(|c| (c[0], c[1])).collect();
            out["value"] = entries(cx, pairs, deeper, nodes, w);
        }
        "object" if room => {
            let pairs = own_entries(scope, o);
            out["value"] = entries(cx, pairs, deeper, nodes, w);
        }
        "nodelist" | "htmlcollection" if room => {
            let items = indexed(scope, o);
            out["value"] = list(cx, items, deeper, nodes, w);
        }
        "regexp" => {
            if let Ok(r) = v8::Local::<v8::RegExp>::try_from(v) {
                let pattern = r.get_source(scope).to_rust_string_lossy(scope);
                let flags = v8::String::new(scope, "flags")
                    .and_then(|k| o.get(scope, k.into()))
                    .map(|f| text(scope, f))
                    .unwrap_or_default();
                out["value"] = json!({"pattern": pattern, "flags": flags});
            }
        }
        "date" => {
            if let Ok(d) = v8::Local::<v8::Date>::try_from(v) {
                out["value"] = json!(iso(d.value_of()).unwrap_or_else(|| "Invalid Date".into()));
            }
        }
        "window" => {
            let r = crate::realm::of_global(scope, o).unwrap_or(w.realm);
            out["value"] = json!({});
            out["realm"] = json!(r);
        }
        "node" => {
            if let Some((cv::Obj::Node(n), _)) = cv::unwrap_any(cx, v) {
                let realm = w.realm;
                out["value"] = node(cx, n, nodes, w);
                out["node"] = json!([realm, n.index(), n.generation()]);
            }
        }
        _ => {}
    }
    out
}

/// The BiDi type of an object.
fn kind<'s>(
    cx: &mut Cx<'_, 's, '_>,
    v: v8::Local<'s, v8::Value>,
    o: v8::Local<'s, v8::Object>,
) -> &'static str {
    if v.is_proxy() {
        return "proxy";
    }
    if v.is_function() {
        return "function";
    }
    if crate::realm::of_global(cx.scope, o).is_some() {
        return "window";
    }
    if let Some((obj, iface)) = cv::unwrap_any(cx, v) {
        return match (obj, iface) {
            (_, Iface::Window) => "window",
            (cv::Obj::Node(_), _) => "node",
            (cv::Obj::Window, _) => "window",
            (_, Iface::NodeList) => "nodelist",
            (_, Iface::HTMLCollection) => "htmlcollection",
            _ => "object",
        };
    }
    if v.is_array() {
        "array"
    } else if v.is_reg_exp() {
        "regexp"
    } else if v.is_date() {
        "date"
    } else if v.is_map() {
        "map"
    } else if v.is_set() {
        "set"
    } else if v.is_weak_map() {
        "weakmap"
    } else if v.is_weak_set() {
        "weakset"
    } else if v.is_generator_object() {
        "generator"
    } else if v.is_native_error() {
        "error"
    } else if v.is_promise() {
        "promise"
    } else if v.is_typed_array() {
        "typedarray"
    } else if v.is_array_buffer() {
        "arraybuffer"
    } else {
        "object"
    }
}

/// A node's BiDi `NodeProperties`, with children to `depth`.
fn node(cx: &mut Cx<'_, '_, '_>, n: NodeId, depth: u32, w: &mut Walk) -> Json {
    let (mut props, kids, open_root) = {
        let dom = cx.dom();
        let Some(node) = dom.get(n) else {
            return json!({});
        };
        let kids: Vec<NodeId> = dom.children(n).collect();
        let (ty, value) = match &node.data {
            NodeData::Document => (9, None),
            NodeData::DocumentFragment | NodeData::ShadowRoot { .. } => (11, None),
            NodeData::Doctype { .. } => (10, None),
            NodeData::Text(t) => (3, Some(t.clone())),
            NodeData::Comment(t) => (8, Some(t.clone())),
            NodeData::ProcessingInstruction { data, .. } => (7, Some(data.clone())),
            NodeData::Element { .. } => (1, None),
        };
        let mut p = json!({"nodeType": ty, "childNodeCount": kids.len()});
        if let Some(v) = value {
            p["nodeValue"] = json!(v);
        }
        if ty == 1 {
            p["localName"] = json!(node.local_name().unwrap_or_default());
            p["namespaceURI"] = json!(node.namespace().and_then(ns_uri));
            let attrs: serde_json::Map<String, Json> = node
                .attrs()
                .iter()
                .map(|a| (a.qualified_name(), json!(a.value)))
                .collect();
            p["attributes"] = Json::Object(attrs);
            p["shadowRoot"] = Json::Null;
        }
        // An open shadow root, as a node reference (closed roots stay hidden).
        let open_root = dom.shadow_root(n).filter(|&r| {
            matches!(
                dom.get(r).map(|x| &x.data),
                Some(NodeData::ShadowRoot {
                    mode: pageon_dom::ShadowMode::Open,
                    ..
                })
            )
        });
        (p, kids, open_root)
    };
    if let Some(r) = open_root {
        let realm = w.realm;
        let mut c = json!({"type": "node", "value": node(cx, r, 0, w)});
        c["node"] = json!([realm, r.index(), r.generation()]);
        props["shadowRoot"] = c;
    }
    if depth > 0 {
        let mut children = Vec::with_capacity(kids.len());
        for k in kids {
            let realm = w.realm;
            let mut c = json!({"type": "node", "value": node(cx, k, depth - 1, w)});
            c["node"] = json!([realm, k.index(), k.generation()]);
            children.push(c);
        }
        props["children"] = Json::Array(children);
    }
    props
}

fn ns_uri(ns: pageon_dom::Namespace) -> Option<&'static str> {
    use pageon_dom::Namespace as N;
    Some(match ns {
        N::Html => "http://www.w3.org/1999/xhtml",
        N::Svg => "http://www.w3.org/2000/svg",
        N::MathMl => "http://www.w3.org/1998/Math/MathML",
        N::Xlink => "http://www.w3.org/1999/xlink",
        N::Xml => "http://www.w3.org/XML/1998/namespace",
        N::Xmlns => "http://www.w3.org/2000/xmlns/",
        N::None => return None,
    })
}

fn local<'s>(
    cx: &mut Cx<'_, 's, '_>,
    v: &LocalValue,
    held: &crate::eval::Held,
) -> Result<v8::Local<'s, v8::Value>, String> {
    let fail = || "cannot build the value".to_string();
    let scope = &mut *cx.scope;
    Ok(match v {
        LocalValue::Undefined => v8::undefined(scope).into(),
        LocalValue::Null => v8::null(scope).into(),
        LocalValue::Bool(b) => v8::Boolean::new(scope, *b).into(),
        LocalValue::Number(n) => v8::Number::new(scope, *n).into(),
        LocalValue::String(s) => v8::String::new(scope, s).ok_or_else(fail)?.into(),
        LocalValue::BigInt(s) => {
            bigint(scope, s).ok_or_else(|| format!("{s:?} is not a BigInt"))?
        }
        LocalValue::Date(s) => {
            let t = parse_iso(s).ok_or_else(|| format!("{s:?} is not an ISO 8601 date"))?;
            v8::Date::new(scope, t).ok_or_else(fail)?.into()
        }
        LocalValue::RegExp { pattern, flags } => {
            let mut f = v8::RegExpCreationFlags::empty();
            for c in flags.chars() {
                f |= match c {
                    'd' => v8::RegExpCreationFlags::HAS_INDICES,
                    'g' => v8::RegExpCreationFlags::GLOBAL,
                    'i' => v8::RegExpCreationFlags::IGNORE_CASE,
                    'm' => v8::RegExpCreationFlags::MULTILINE,
                    's' => v8::RegExpCreationFlags::DOT_ALL,
                    'u' => v8::RegExpCreationFlags::UNICODE,
                    'v' => v8::RegExpCreationFlags::UNICODE_SETS,
                    'y' => v8::RegExpCreationFlags::STICKY,
                    _ => return Err(format!("invalid regular expression flags {flags:?}")),
                };
            }
            let p = v8::String::new(scope, pattern).ok_or_else(fail)?;
            v8::tc_scope!(let tc, scope);
            match v8::RegExp::new(tc, p, f) {
                Some(r) => r.into(),
                None => return Err(format!("invalid regular expression /{pattern}/{flags}")),
            }
        }
        LocalValue::Handle(h) => {
            let g = held
                .get(&h.0)
                .ok_or("Could not find object with given id")?;
            v8::Local::new(scope, g)
        }
        LocalValue::Array(items) => {
            let mut vals = Vec::with_capacity(items.len());
            for i in items {
                vals.push(local(cx, i, held)?);
            }
            v8::Array::new_with_elements(cx.scope, &vals).into()
        }
        LocalValue::Set(items) => {
            let set = v8::Set::new(cx.scope);
            for i in items {
                let l = local(cx, i, held)?;
                set.add(cx.scope, l).ok_or_else(fail)?;
            }
            set.into()
        }
        LocalValue::Map(pairs) => {
            let map = v8::Map::new(cx.scope);
            for (k, val) in pairs {
                let k = local_key(cx, k, held)?;
                let val = local(cx, val, held)?;
                map.set(cx.scope, k, val).ok_or_else(fail)?;
            }
            map.into()
        }
        LocalValue::Object(pairs) => {
            let obj = v8::Object::new(cx.scope);
            for (k, val) in pairs {
                let k = local_key(cx, k, held)?;
                let val = local(cx, val, held)?;
                obj.create_data_property(cx.scope, k.try_into().map_err(|_| fail())?, val)
                    .ok_or_else(fail)?;
            }
            obj.into()
        }
    })
}

fn local_key<'s>(
    cx: &mut Cx<'_, 's, '_>,
    k: &LocalKey,
    held: &crate::eval::Held,
) -> Result<v8::Local<'s, v8::Value>, String> {
    match k {
        LocalKey::String(s) => Ok(v8::String::new(cx.scope, s)
            .ok_or("cannot build the key")?
            .into()),
        LocalKey::Value(v) => {
            let l = local(cx, v, held)?;
            // Object keys are property keys: strings or symbols.
            Ok(l)
        }
    }
}

/// A BigInt from decimal text (`-123`, `456`).
fn bigint<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> Option<v8::Local<'s, v8::Value>> {
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    // Little-endian 64-bit words, by repeated multiply-add.
    let mut words: Vec<u64> = vec![0];
    for b in digits.bytes() {
        let mut carry = u128::from(b - b'0');
        for w in &mut words {
            let x = u128::from(*w) * 10 + carry;
            *w = x as u64;
            carry = x >> 64;
        }
        if carry > 0 {
            words.push(carry as u64);
        }
    }
    v8::BigInt::new_from_words(scope, neg, &words).map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trips() {
        for ms in [
            0.0,
            1_700_000_000_123.0,
            -1.0,
            951_782_400_000.0,
            -62_198_755_200_000.0,
        ] {
            let s = iso(ms).unwrap();
            assert_eq!(parse_iso(&s), Some(ms), "{s}");
        }
        assert_eq!(iso(0.0).unwrap(), "1970-01-01T00:00:00.000Z");
        assert_eq!(parse_iso("2024-02-29"), Some(1_709_164_800_000.0));
        assert_eq!(
            parse_iso("2024-02-29T01:00:00+01:00"),
            Some(1_709_164_800_000.0)
        );
        assert_eq!(iso(f64::NAN), None);
    }
}
