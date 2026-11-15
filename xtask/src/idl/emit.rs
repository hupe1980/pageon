//! Emits `generated.rs`: the interface enum, dictionaries, V8 templates and member callbacks.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use super::model::{Arg, Ext, Interface, Member, Model, Owned, Ty};

/// The allowlist: per interface or mixin, the members implemented by hand.
pub type Allow = BTreeMap<String, BTreeSet<String>>;

/// `HTMLElement` → `html_element`, `getElementById` → `get_element_by_id`.
pub fn snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() {
            let prev_lower =
                i > 0 && (chars[i - 1].is_ascii_lowercase() || chars[i - 1].is_ascii_digit());
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            let prev_upper = i > 0 && chars[i - 1].is_ascii_uppercase();
            if i > 0 && (prev_lower || (prev_upper && next_lower)) {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    match out.as_str() {
        "type" | "loop" | "match" | "self" | "move" | "ref" | "where" | "fn" | "mod" | "use"
        | "in" | "as" | "box" => {
            format!("{out}_")
        }
        _ => out,
    }
}

/// How a member is implemented.
#[derive(Debug, Clone, PartialEq)]
enum Impl {
    Hand,
    Reflect,
    Handler,
    Skip,
}

struct Gen<'m> {
    model: &'m Model,
    allow: &'m Allow,
    order: Vec<String>,
    included: BTreeSet<String>,
    dicts: BTreeSet<String>,
    /// Overloads told apart by argument count: (owner, operation) → the other overloads.
    /// Overload alternatives: arguments, and the (position, interface) that selects it when
    /// it has as many required arguments as the main one (`FormData.append(name, blob, …)`).
    alts: std::collections::BTreeMap<(String, String), Vec<Alt>>,
    out: String,
    errors: Vec<String>,
}

fn is_scalar_reflect(t: &Ty) -> bool {
    match t {
        Ty::Str | Ty::Bool | Ty::Int(..) | Ty::Float(_) => true,
        Ty::Nullable(i) => matches!(**i, Ty::Str),
        _ => false,
    }
}

/// An overload alternative: its arguments, and the (position, interface) that selects it when
/// it has as many required arguments as the main one.
type Alt = (Vec<Arg>, Option<(usize, String)>);

impl Gen<'_> {
    fn allowed(&self, owner: &str, name: &str) -> bool {
        self.allow.get(owner).is_some_and(|s| s.contains(name))
    }

    fn how(&self, iface: &Interface, m: &Owned) -> Impl {
        let name = m.member.name();
        if self.allowed(&m.owner, name) || self.allowed(&iface.name, name) {
            return Impl::Hand;
        }
        if let Member::Attr {
            ty,
            ext,
            is_static: false,
            ..
        } = &m.member
        {
            let t = self.model.resolve(ty);
            if ext.reflect.is_some() && !ext.reflect_setter && is_scalar_reflect(&t) {
                return Impl::Reflect;
            }
            if matches!(ty, Ty::Named(n) if n == "EventHandler" || n == "OnErrorEventHandler" || n == "OnBeforeUnloadEventHandler")
                || matches!(ty, Ty::Nullable(i) if matches!(&**i, Ty::Named(n) if n.ends_with("EventHandler")))
            {
                return Impl::Handler;
            }
        }
        Impl::Skip
    }

    fn select(&mut self) {
        let model = self.model;
        let mut want: BTreeSet<String> = BTreeSet::new();
        for k in self.allow.keys() {
            if model.interfaces.contains_key(k) {
                want.insert(k.clone());
            }
        }
        for (name, i) in &model.interfaces {
            if name.starts_with("HTML") && name.ends_with("Element") && i.ext.window && !i.namespace
            {
                want.insert(name.clone());
            }
        }
        // Ancestors must be present.
        for w in want.clone() {
            for a in model.chain(&w) {
                if !self.allow.contains_key(&a) && !want.contains(&a) {
                    self.errors
                        .push(format!("{w} needs ancestor {a}: add it to the allowlist"));
                }
                want.insert(a);
            }
        }
        // Topological order: parents first, then by name.
        let mut order: Vec<String> = Vec::new();
        let mut left: Vec<String> = want.iter().cloned().collect();
        while !left.is_empty() {
            let before = left.len();
            left.retain(|n| {
                let ready = model
                    .interfaces
                    .get(n)
                    .and_then(|i| i.parent.clone())
                    .is_none_or(|p| order.contains(&p));
                if ready {
                    order.push(n.clone());
                }
                !ready
            });
            if left.len() == before {
                self.errors
                    .push(format!("inheritance cycle among {left:?}"));
                break;
            }
        }
        self.included = want;
        self.order = order;
        // Allowlist entries must name real members.
        for (owner, members) in self.allow {
            for m in members {
                let found = model.interfaces.values().any(|i| {
                    i.members
                        .iter()
                        .any(|o| (o.owner == *owner || i.name == *owner) && o.member.name() == m)
                        || (i.name == *owner && i.ext.factory.as_ref().is_some_and(|(f, _)| f == m))
                });
                if !found {
                    self.errors
                        .push(format!("allowlist: {owner}.{m} is not in the IDL"));
                }
            }
            if !model.interfaces.contains_key(owner)
                && !model
                    .interfaces
                    .values()
                    .any(|i| i.members.iter().any(|o| o.owner == *owner))
            {
                self.errors.push(format!(
                    "allowlist: {owner} is neither an interface nor an included mixin"
                ));
            }
        }
    }

    /// Rust type and converter for an argument type.
    fn conv(&mut self, t: &Ty, nte: bool, ctx: &str) -> (String, String) {
        let t = self.model.resolve(t);
        match &t {
            Ty::Str if nte => ("String".into(), "cv::string_nte(cx, v)?".into()),
            Ty::Str => ("String".into(), "cv::string(cx, v)?".into()),
            Ty::Bool => ("bool".into(), "cv::boolean(cx, v)".into()),
            Ty::Int(bits, signed) => {
                let r = format!("{}{bits}", if *signed { "i" } else { "u" });
                (r.clone(), format!("cv::int::<{r}>(cx, v)?"))
            }
            Ty::Float(restricted) => ("f64".into(), format!("cv::float(cx, v, {restricted})?")),
            Ty::Any | Ty::Object | Ty::Buffer => ("Value".into(), "cv::value(cx, v)".into()),
            Ty::Union(m)
                if m.iter()
                    .all(|t| matches!(self.model.resolve(t), Ty::Buffer | Ty::Union(_))) =>
            {
                ("Value".into(), "cv::value(cx, v)".into())
            }
            Ty::Named(n) if self.included.contains(n) && !self.model.callbacks.contains(n) => {
                ("Obj".into(), format!("cv::obj(cx, v, Iface::{n})?"))
            }
            Ty::Named(n) if self.model.dictionaries.contains_key(n) => {
                self.dicts.insert(n.clone());
                (n.clone(), format!("dict_{}(cx, v)?", snake(n)))
            }
            Ty::Named(n) if self.model.enums.contains(n) => {
                ("String".into(), "cv::string(cx, v)?".into())
            }
            Ty::Named(n) if self.model.callbacks.contains(n) => {
                ("Value".into(), "cv::value(cx, v)".into())
            }
            Ty::Nullable(i) => {
                let inner = self.model.resolve(i);
                if matches!(inner, Ty::Named(ref n) if self.model.callbacks.contains(n)) {
                    return ("Option<Value>".into(), "cv::opt_value(cx, v)".into());
                }
                let (rt, c) = self.conv(&inner, nte, ctx);
                (
                    format!("Option<{rt}>"),
                    format!("if v.is_null_or_undefined() {{ None }} else {{ Some({c}) }}"),
                )
            }
            Ty::Seq(i) => {
                let (rt, c) = self.conv(i, false, ctx);
                (
                    format!("Vec<{rt}>"),
                    format!("cv::seq(cx, v, |cx, v| Ok({c}))?"),
                )
            }
            Ty::Union(members) => {
                let has_str = members
                    .iter()
                    .any(|m| matches!(self.model.resolve(m), Ty::Str));
                let others: Vec<Ty> = members
                    .iter()
                    .map(|m| self.model.resolve(m))
                    .filter(|m| {
                        !matches!(m, Ty::Str)
                            && !matches!(m, Ty::Named(n) if n.starts_with("Trusted"))
                    })
                    .collect();
                if has_str
                    && others.iter().all(
                        |o| matches!(o, Ty::Named(n) if self.model.dictionaries.contains_key(n)),
                    )
                {
                    // (DOMString or SomeOptions): the string; options are ignored.
                    return ("String".into(), "cv::string_nte(cx, v)?".into());
                }
                if has_str && others.len() == 1 && matches!(&others[0], Ty::Named(n) if n == "Node")
                {
                    return ("NodeOrString".into(), "cv::node_or_string(cx, v)?".into());
                }
                let dicts: Vec<String> = others
                    .iter()
                    .filter_map(|o| match o {
                        Ty::Named(n) if self.model.dictionaries.contains_key(n) => Some(n.clone()),
                        _ => None,
                    })
                    .collect();
                if others.len() == 2
                    && dicts.len() == 1
                    && others.iter().any(|o| matches!(o, Ty::Bool))
                    && let Some(d) = dicts.first().cloned()
                {
                    {
                        self.dicts.insert(d.clone());
                        return (
                            format!("cv::OrBool<{d}>"),
                            format!(
                                "if v.is_boolean() {{ cv::OrBool::Bool(v.is_true()) }} else {{ cv::OrBool::Dict(dict_{}(cx, v)?) }}",
                                snake(&d)
                            ),
                        );
                    }
                }
                ("Value".into(), "cv::value(cx, v)".into())
            }
            Ty::Other(o) if o.starts_with("RecordType") => {
                ("Value".into(), "cv::value(cx, v)".into())
            }
            other => {
                self.errors
                    .push(format!("{ctx}: unsupported argument type {other:?}"));
                ("Value".into(), "cv::value(cx, v)".into())
            }
        }
    }

    /// Rust return type for an IDL type.
    fn ret_ty(&mut self, t: &Ty, ctx: &str) -> String {
        let t = self.model.resolve(t);
        match &t {
            Ty::Undefined => "()".into(),
            Ty::Str => "String".into(),
            Ty::Bool => "bool".into(),
            Ty::Int(bits, signed) => format!("{}{bits}", if *signed { "i" } else { "u" }),
            Ty::Float(_) => "f64".into(),
            Ty::Union(members) => {
                // Interfaces pageon does not expose (`File`) cannot occur.
                let resolved: Vec<Ty> = members
                    .iter()
                    .map(|m| self.model.resolve(m))
                    .filter(|m| {
                        !matches!(m, Ty::Named(n) if !self.included.contains(n)
                            && !self.model.dictionaries.contains_key(n)
                            && !self.model.enums.contains(n)
                            && !self.model.callbacks.contains(n))
                    })
                    .collect();
                let strings = resolved.iter().all(|m| {
                    matches!(m, Ty::Str) || matches!(m, Ty::Named(n) if n.starts_with("Trusted"))
                });
                let ifaces: Vec<&Ty> = resolved
                    .iter()
                    .filter(|m| matches!(m, Ty::Named(n) if self.included.contains(n)))
                    .collect();
                if strings {
                    "String".into()
                } else if ifaces.len() == 1 && resolved.iter().all(|m| matches!(m, Ty::Named(_))) {
                    "Obj".into()
                } else {
                    "Value".into()
                }
            }
            Ty::Any | Ty::Object | Ty::Buffer => "Value".into(),
            // A callback interface with constants is also an interface object; its values
            // are still callbacks.
            Ty::Named(n) if self.included.contains(n) && !self.model.callbacks.contains(n) => {
                "Obj".into()
            }
            Ty::Named(n) if self.model.enums.contains(n) => "String".into(),
            Ty::Named(n)
                if self.model.callbacks.contains(n) || self.model.dictionaries.contains_key(n) =>
            {
                "Value".into()
            }
            Ty::Nullable(i) => format!("Option<{}>", self.ret_ty(i, ctx)),
            Ty::Seq(i) => format!("Vec<{}>", self.ret_ty(i, ctx)),
            // A promise the hand-written code creates.
            Ty::Other(o) if o.starts_with("Promise") => "Value".into(),
            other => {
                self.errors
                    .push(format!("{ctx}: unsupported return type {other:?}"));
                "Value".into()
            }
        }
    }

    fn default_expr(&mut self, a: &Arg, rust: &str) -> Option<String> {
        let d = a.default.as_ref()?;
        let t = self.model.resolve(&a.ty);
        if rust == "Value" {
            return Some(match d.as_str() {
                "null" => "cv::null(cx)".into(),
                "{}" => "cv::empty_object(cx)".into(),
                "[]" => "cv::empty_array(cx)".into(),
                "true" | "false" => format!("cv::boolean_value(cx, {d})"),
                // An IDL string literal keeps its quotes in the model.
                other if other.starts_with('"') => format!("cv::string_value(cx, {other})"),
                other => format!("cv::number_value(cx, {other} as f64)"),
            });
        }
        if rust.starts_with("cv::OrBool<") {
            return Some(match d.as_str() {
                "false" | "true" => format!("cv::OrBool::Bool({d})"),
                _ => "cv::OrBool::Dict(Default::default())".to_string(),
            });
        }
        Some(match (d.as_str(), &t) {
            ("null", _) => "None".into(),
            ("{}", Ty::Named(n)) => {
                self.dicts.insert(n.clone());
                format!("{n}::default()")
            }
            ("{}", _) => format!("<{rust}>::default()"),
            ("[]", _) => "Vec::new()".into(),
            (s, Ty::Str | Ty::Named(_)) if s.starts_with('"') => format!("{s}.to_string()"),
            (s, Ty::Float(_)) => {
                if s.contains('.') {
                    s.to_string()
                } else {
                    format!("{s}.0")
                }
            }
            (s, _) => s.to_string(),
        })
    }

    /// Statements converting arguments `a0…` from `args`.
    fn args_code(&mut self, args: &[Arg], ctx: &str) -> (String, Vec<String>) {
        let mut code = String::new();
        let mut names = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let var = format!("a{i}");
            let (rust, conv) = self.conv(&a.ty, a.null_to_empty, ctx);
            if a.variadic {
                let _ = writeln!(
                    code,
                    "        let mut {var}: Vec<{rust}> = Vec::new();\n        for k in {i}..args.length() {{ let v = args.get(k); {var}.push({conv}); }}"
                );
            } else if a.optional {
                match self.default_expr(a, &rust) {
                    Some(d) => {
                        let _ = writeln!(
                            code,
                            "        let {var}: {rust} = {{ let v = args.get({i}); if v.is_undefined() {{ {d} }} else {{ {conv} }} }};"
                        );
                    }
                    None => {
                        let _ = writeln!(
                            code,
                            "        let {var}: Option<{rust}> = {{ let v = args.get({i}); if v.is_undefined() {{ None }} else {{ Some({conv}) }} }};"
                        );
                    }
                }
            } else {
                let _ = writeln!(
                    code,
                    "        if args.length() <= {i} {{ return Err(cv::Error::args(\"{ctx}\", {i})); }}"
                );
                let _ = writeln!(
                    code,
                    "        let {var}: {rust} = {{ let v = args.get({i}); {conv} }};"
                );
            }
            names.push(var);
        }
        (code, names)
    }

    fn this_code(&self, iface: &Interface) -> String {
        if iface.ext.global {
            "let this = Obj::Window;".into()
        } else if iface.namespace {
            "let this = Obj::Window; let _ = this;".into()
        } else {
            format!("let this = cx.this(args, Iface::{})?;", iface.name)
        }
    }

    fn callback(&mut self, fname: &str, body: &str) {
        let _ = write!(
            self.out,
            "#[allow(non_snake_case, unused_variables, clippy::let_unit_value, clippy::unit_arg)]\nfn {fname}(scope: &mut v8::PinScope, args: v8::FunctionCallbackArguments, mut rv: v8::ReturnValue) {{\n    cv::call(scope, &args, &mut rv, |cx, args| {{\n{body}    }});\n}}\n\n"
        );
    }

    fn emit_member(
        &mut self,
        iface: &Interface,
        m: &Owned,
        how: &Impl,
    ) -> Option<(String, Option<String>, Option<String>, usize)> {
        // Returns (js name, getter/op fn, setter fn, length).
        let owner = snake(&m.owner);
        let ctx = format!("{}.{}", iface.name, m.member.name());
        let this = self.this_code(iface);
        let prefix = match &m.member {
            Member::Op {
                is_static: true, ..
            } => format!("{}__static_{}", iface.name, m.member.name()),
            _ => format!("{}__{}", iface.name, m.member.name()),
        };
        match (&m.member, how) {
            (
                Member::Op {
                    name,
                    args,
                    ret,
                    is_static,
                },
                Impl::Hand,
            ) => {
                let (code, names) = self.args_code(args, &ctx);
                let rt = self.ret_ty(ret, &ctx);
                let this_line = if *is_static {
                    String::new()
                } else {
                    format!("        {this}\n")
                };
                let pass = |names: Vec<String>| {
                    if *is_static {
                        names.join(", ")
                    } else {
                        std::iter::once("this".to_string())
                            .chain(names)
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                };
                // Overloads with more required arguments come first, by argument count:
                // `scrollTo(x, y)` is `scroll_to_x_y`, next to `scroll_to(options)`.
                let mut alts = self
                    .alts
                    .get(&(m.owner.clone(), name.clone()))
                    .cloned()
                    .unwrap_or_default();
                alts.sort_by_key(|(a, _)| std::cmp::Reverse(required(a)));
                let mut dispatch = String::new();
                for (alt, by) in &alts {
                    let (alt_code, alt_names) = self.args_code(alt, &ctx);
                    let f = std::iter::once(snake(name))
                        .chain(alt.iter().map(|a| snake(&a.name)))
                        .collect::<Vec<_>>()
                        .join("_");
                    let when = match by {
                        Some((k, iface)) => {
                            format!(" && cv::obj(cx, args.get({k}), Iface::{iface}).is_ok()")
                        }
                        None => String::new(),
                    };
                    let _ = write!(
                        dispatch,
                        "        if args.length() >= {}{when} {{\n{alt_code}        let r: {rt} = crate::dom::{owner}::{f}(cx, {})?;\n        return cx.ret(r);\n        }}\n",
                        required(alt),
                        pass(alt_names)
                    );
                }
                let body = format!(
                    "{this_line}{dispatch}{code}        let r: {rt} = crate::dom::{owner}::{}(cx, {})?;\n        cx.ret(r)\n",
                    snake(name),
                    pass(names)
                );
                let f = format!("op__{prefix}");
                self.callback(&f, &body);
                let length = args.iter().filter(|a| !a.optional && !a.variadic).count();
                Some((name.clone(), Some(f), None, length))
            }
            (
                Member::Attr {
                    name,
                    ty,
                    readonly,
                    ext,
                    is_static: false,
                },
                how,
            ) => {
                let getter = format!("get__{prefix}");
                let setter = format!("set__{prefix}");
                let rt = if ext.observable {
                    "Value".to_string()
                } else {
                    self.ret_ty(ty, &ctx)
                };
                let content = ext
                    .reflect
                    .clone()
                    .filter(|r| !r.is_empty())
                    .unwrap_or_else(|| name.to_ascii_lowercase());
                let event = name.strip_prefix("on").unwrap_or(name).to_string();
                let get_body = match how {
                    Impl::Hand => format!(
                        "        {this}\n        let r: {rt} = crate::dom::{owner}::get_{}(cx, this)?;\n        cx.ret(r)\n",
                        snake(name)
                    ),
                    Impl::Reflect => format!(
                        "        {this}\n        let r = cv::reflect_get(cx, this, \"{content}\", {})?;\n        cx.ret_value(r)\n",
                        reflect_kind(&self.model.resolve(ty), ext)
                    ),
                    Impl::Handler => format!(
                        "        {this}\n        let r = cv::handler_get(cx, this, \"{event}\")?;\n        cx.ret(r)\n"
                    ),
                    Impl::Skip => return None,
                };
                self.callback(&getter, &get_body);
                let set = if !*readonly {
                    let (rust, conv) = self.conv(ty, ext.null_to_empty, &ctx);
                    let body = match how {
                        Impl::Hand if ext.reflect.is_some() && ext.reflect_setter => {
                            format!(
                                "        {this}\n        let v = args.get(0);\n        let _: {rust} = {conv};\n        cv::reflect_set(cx, this, \"{content}\", args.get(0), {})?;\n        cx.ret(())\n",
                                reflect_kind(&self.model.resolve(ty), ext)
                            )
                        }
                        Impl::Hand => format!(
                            "        {this}\n        let v = args.get(0);\n        let a0: {rust} = {conv};\n        crate::dom::{owner}::set_{}(cx, this, a0)?;\n        cx.ret(())\n",
                            snake(name)
                        ),
                        Impl::Reflect => format!(
                            "        {this}\n        cv::reflect_set(cx, this, \"{content}\", args.get(0), {})?;\n        cx.ret(())\n",
                            reflect_kind(&self.model.resolve(ty), ext)
                        ),
                        Impl::Handler => format!(
                            "        {this}\n        let v = cv::opt_value(cx, args.get(0));\n        cv::handler_set(cx, this, \"{event}\", v)?;\n        cx.ret(())\n"
                        ),
                        Impl::Skip => unreachable!(),
                    };
                    self.callback(&setter, &body);
                    Some(setter)
                } else if let Some(fwd) = &ext.put_forwards {
                    let body = format!(
                        "        cv::put_forwards(cx, args, \"{name}\", \"{fwd}\")?;\n        cx.ret(())\n"
                    );
                    self.callback(&setter, &body);
                    Some(setter)
                } else {
                    None
                };
                Some((name.clone(), Some(getter), set, 0))
            }
            _ => None,
        }
    }

    fn emit_dicts(&mut self) {
        let mut done: BTreeSet<String> = BTreeSet::new();
        while let Some(d) = self.dicts.iter().find(|d| !done.contains(*d)).cloned() {
            done.insert(d.clone());
            let mut members = self.model.dict_members(&d);
            members.sort_by(|a, b| a.0.cmp(&b.0)); // WebIDL: lexicographic order
            let mut fields = String::new();
            let mut defaults = String::new();
            let mut reads = String::new();
            for (name, ty, def) in &members {
                // Members whose type pageon does not expose (e.g. `AbortSignal`) cannot be
                // passed by a page, so they are not read.
                let resolved = self.model.resolve(ty);
                let mut inner = resolved.clone();
                while let Ty::Nullable(i) | Ty::Seq(i) = inner {
                    inner = *i;
                }
                if let Ty::Named(n) = &inner {
                    let known = self.included.contains(n)
                        || self.model.dictionaries.contains_key(n)
                        || self.model.enums.contains(n)
                        || self.model.callbacks.contains(n);
                    if !known {
                        continue;
                    }
                }
                let field = snake(name);
                let ctx = format!("{d}.{name}");
                let (rust, conv) = self.conv(ty, false, &ctx);
                let arg = Arg {
                    name: name.clone(),
                    ty: ty.clone(),
                    optional: true,
                    default: def.clone(),
                    variadic: false,
                    null_to_empty: false,
                };
                let dflt_expr = if rust == "Value" {
                    None
                } else {
                    self.default_expr(&arg, &rust)
                };
                let (fty, dflt) = match dflt_expr {
                    Some(e) if e != "None" => (rust.clone(), e),
                    Some(_) => (
                        if rust.starts_with("Option<") {
                            rust.clone()
                        } else {
                            format!("Option<{rust}>")
                        },
                        "None".into(),
                    ),
                    None => (format!("Option<{rust}>"), "None".into()),
                };
                let wrap = if fty == rust {
                    conv.clone()
                } else {
                    format!("Some({conv})")
                };
                let _ = writeln!(fields, "    /// `{name}`.\n    pub {field}: {fty},");
                let _ = writeln!(defaults, "            {field}: {dflt},");
                let _ = writeln!(
                    reads,
                    "    {{ let v = cv::get(cx, obj, \"{name}\")?; if !v.is_undefined() {{ d.{field} = {wrap}; }} }}"
                );
            }
            let _ = write!(
                self.out,
                "/// The `{d}` dictionary.\n#[derive(Debug, Clone)]\n#[allow(missing_docs)]\npub struct {d} {{\n{fields}}}\n\nimpl Default for {d} {{\n    fn default() -> Self {{\n        Self {{\n{defaults}        }}\n    }}\n}}\n\n#[allow(unused_variables, unused_mut, clippy::all)]\nfn dict_{}(cx: &mut Cx<'_, '_, '_>, v: v8::Local<'_, v8::Value>) -> R<{d}> {{\n    let mut d = {d}::default();\n    if v.is_null_or_undefined() {{ return Ok(d); }}\n    let obj = cv::object(cx, v, \"{d}\")?;\n{reads}    Ok(d)\n}}\n\n",
                snake(&d)
            );
        }
    }

    fn run(&mut self) {
        self.select();
        let model = self.model;
        let _ = write!(
            self.out,
            "//! @generated by `cargo xtask idl` from `crates/pageon-js/idl` and `idl/allowlist.toml`. Do not edit.\n\n#![allow(clippy::all, clippy::pedantic, missing_docs, non_camel_case_types, unused_imports, unused_mut, unused_parens, unused_variables)]\n\nuse v8::MapFnTo;\n\nuse crate::cv::{{self, Cx, NodeOrString, Obj, R, Value}};\n\n"
        );
        // Interface enum.
        let _ = writeln!(
            self.out,
            "/// Every interface pageon exposes, parents before children.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]\n#[repr(u16)]\npub enum Iface {{"
        );
        for n in &self.order {
            let _ = writeln!(self.out, "    /// `{n}`.\n    {n},");
        }
        let _ = writeln!(
            self.out,
            "}}\n\nimpl Iface {{\n    /// All interfaces.\n    pub const ALL: &'static [Iface] = &[{}];\n",
            self.order
                .iter()
                .map(|n| format!("Iface::{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let _ = writeln!(
            self.out,
            "    /// The interface name.\n    pub fn name(self) -> &'static str {{\n        match self {{"
        );
        for n in &self.order {
            let _ = writeln!(self.out, "            Iface::{n} => \"{n}\",");
        }
        let _ = writeln!(
            self.out,
            "        }}\n    }}\n\n    /// The parent interface.\n    pub fn parent(self) -> Option<Iface> {{\n        match self {{"
        );
        for n in &self.order {
            let p = model.interfaces.get(n).and_then(|i| i.parent.clone());
            let _ = writeln!(
                self.out,
                "            Iface::{n} => {},",
                p.map_or("None".into(), |p| format!("Some(Iface::{p})"))
            );
        }
        let _ = writeln!(
            self.out,
            "        }}\n    }}\n\n    /// By name.\n    pub fn from_name(name: &str) -> Option<Iface> {{\n        Iface::ALL.iter().copied().find(|i| i.name() == name)\n    }}\n\n    /// By index.\n    pub fn from_index(i: u16) -> Option<Iface> {{\n        Iface::ALL.get(usize::from(i)).copied()\n    }}\n\n    /// Whether `self` is `other` or inherits from it.\n    pub fn is_a(self, other: Iface) -> bool {{\n        let mut cur = Some(self);\n        while let Some(c) = cur {{\n            if c == other {{ return true; }}\n            cur = c.parent();\n        }}\n        false\n    }}\n}}\n"
        );

        // Members and template code.
        let mut install = String::new();
        let mut expose = String::new();
        let mut ifaces: Vec<Interface> = Vec::new();
        for n in &self.order {
            let Some(i) = model.interfaces.get(n).cloned() else {
                continue;
            };
            let (i, alts) = merge_overloads(i, model, &self.included);
            for (owner, name, alt) in alts {
                self.alts.entry((owner, name)).or_default().push(alt);
            }
            ifaces.push(i);
        }
        for iface in &ifaces {
            let i = &iface.name;
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut props = String::new();
            let ctor = iface
                .members
                .iter()
                .find(|m| {
                    matches!(m.member, Member::Ctor { .. }) && self.how(iface, m) == Impl::Hand
                })
                .cloned();
            for m in iface.members.clone() {
                let how = self.how(iface, &m);
                if let Member::Setlike { readonly } = m.member {
                    if how == Impl::Hand && seen.insert("setlike".into()) {
                        let this = self.this_code(iface);
                        let owner = snake(&m.owner);
                        for kind in ["Entries", "Values"] {
                            let body = format!(
                                "        {this}\n        let p = crate::dom::{owner}::pairs(cx, this)?;\n        let r = cv::iterator(cx, p, cv::Iter::{kind})?;\n        cx.ret(r)\n"
                            );
                            self.callback(&format!("iter_{kind}__{i}"), &body);
                        }
                        let body = format!(
                            "        {this}\n        let p = crate::dom::{owner}::pairs(cx, this)?;\n        cv::for_each(cx, args, p)?;\n        cx.ret(())\n"
                        );
                        self.callback(&format!("iter_forEach__{i}"), &body);
                        let body = format!(
                            "        {this}\n        let r = crate::dom::{owner}::set_size(cx, this)?;\n        cx.ret(r)\n"
                        );
                        self.callback(&format!("set_size__{i}"), &body);
                        let mut ops = vec![(
                            "has",
                            "let r = crate::dom::OWNER::set_has(cx, this, v)?;\n        cx.ret(r)",
                        )];
                        if !readonly {
                            ops.push((
                                "add",
                                "crate::dom::OWNER::set_add(cx, this, v)?;\n        cx.ret(this)",
                            ));
                            ops.push(("delete", "let r = crate::dom::OWNER::set_delete(cx, this, v)?;\n        cx.ret(r)"));
                        }
                        for (name, call) in &ops {
                            let call = call.replace("OWNER", &owner);
                            let body = format!(
                                "        {this}\n        let v = cv::string(cx, args.get(0))?;\n        {call}\n"
                            );
                            self.callback(&format!("set_{name}__{i}"), &body);
                        }
                        if !readonly {
                            let body = format!(
                                "        {this}\n        crate::dom::{owner}::set_clear(cx, this)?;\n        cx.ret(())\n"
                            );
                            self.callback(&format!("set_clear__{i}"), &body);
                        }
                        let _ = writeln!(
                            props,
                            "        cv::setlike(scope, proto, iter_Entries__{i}, iter_Values__{i}, iter_forEach__{i}, set_size__{i}, set_has__{i});"
                        );
                        if !readonly {
                            let _ = writeln!(
                                props,
                                "        cv::method(scope, proto.into(), \"add\", set_add__{i}, 1);\n        cv::method(scope, proto.into(), \"delete\", set_delete__{i}, 1);\n        cv::method(scope, proto.into(), \"clear\", set_clear__{i}, 0);"
                            );
                        }
                    }
                    continue;
                }
                if matches!(m.member, Member::Iterable) {
                    if how == Impl::Hand && seen.insert("iterable".into()) {
                        let this = self.this_code(iface);
                        let owner = snake(&m.owner);
                        for kind in ["Entries", "Keys", "Values"] {
                            let body = format!(
                                "        {this}\n        let p = crate::dom::{owner}::pairs(cx, this)?;\n        let r = cv::iterator(cx, p, cv::Iter::{kind})?;\n        cx.ret(r)\n"
                            );
                            self.callback(&format!("iter_{kind}__{i}"), &body);
                        }
                        let body = format!(
                            "        {this}\n        let p = crate::dom::{owner}::pairs(cx, this)?;\n        cv::for_each(cx, args, p)?;\n        cx.ret(())\n"
                        );
                        self.callback(&format!("iter_forEach__{i}"), &body);
                        let _ = writeln!(
                            props,
                            "        cv::iterable(scope, proto, iter_Entries__{i}, iter_Keys__{i}, iter_Values__{i}, iter_forEach__{i});"
                        );
                    }
                    continue;
                }
                let key = match &m.member {
                    Member::Op {
                        is_static: true,
                        name,
                        ..
                    } => format!("static {name}"),
                    other => other.name().to_string(),
                };
                if how == Impl::Skip || !seen.insert(key) {
                    if let Member::Const { name, value } = &m.member
                        && seen.insert(name.clone())
                    {
                        // Hex constants (`0xFFFFFFFF`) are unsigned in IDL.
                        let suffix = if value.starts_with("0x") { "_u64" } else { "" };
                        let _ = writeln!(
                            props,
                            "        cv::constant(scope, f, proto, \"{name}\", {value}{suffix} as f64);"
                        );
                    }
                    continue;
                }
                if let Some((js, getter, setter, length)) = self.emit_member(iface, &m, &how) {
                    let target = if iface.ext.global || iface.namespace {
                        "inst"
                    } else {
                        "proto"
                    };
                    match &m.member {
                        Member::Op {
                            is_static: true, ..
                        } => {
                            let _ = writeln!(
                                props,
                                "        cv::method(scope, f.into(), \"{js}\", {}, {length});",
                                getter.unwrap_or_default()
                            );
                        }
                        Member::Op { .. } => {
                            let _ = writeln!(
                                props,
                                "        cv::method(scope, {target}.into(), \"{js}\", {}, {length});",
                                getter.unwrap_or_default()
                            );
                        }
                        _ => {
                            let g = getter.unwrap_or_default();
                            let _ = writeln!(
                                props,
                                "        cv::accessor(scope, {target}, \"{js}\", {g}, {});",
                                setter.map_or("None".into(), |s| format!("Some({s}.map_fn_to())"))
                            );
                            if matches!(&m.member, Member::Attr { ext, .. } if ext.stringifier) {
                                let _ = writeln!(
                                    props,
                                    "        cv::method(scope, {target}.into(), \"toString\", {g}, 0);"
                                );
                            }
                        }
                    }
                }
            }
            let ctor_fn = match &ctor {
                Some(c) => {
                    if let Member::Ctor { html: true, .. } = &c.member {
                        // HTML element constructors: `new.target` picks the custom element.
                        let body = format!(
                            "        if args.new_target().is_undefined() {{ return Err(cv::Error::Type(\"Failed to construct '{i}': Please use the 'new' operator.\".into())); }}\n        let nt = cx.keep(args.new_target());\n        let r: Obj = crate::dom::custom_elements::construct(cx, nt)?;\n        cx.ret(r)\n"
                        );
                        self.callback(&format!("ctor__{i}"), &body);
                    } else if let Member::Ctor { args, .. } = &c.member {
                        let ctx = format!("{i} constructor");
                        let (code, names) = self.args_code(args, &ctx);
                        let body = format!(
                            "        if args.new_target().is_undefined() {{ return Err(cv::Error::Type(\"Failed to construct '{i}': Please use the 'new' operator.\".into())); }}\n{code}        let r: Obj = crate::dom::{}::constructor(cx, {})?;\n        cx.ret(r)\n",
                            snake(&c.owner),
                            names.join(", ")
                        );
                        self.callback(&format!("ctor__{i}"), &body);
                    }
                    format!("ctor__{i}")
                }
                None => "cv::illegal_constructor".into(),
            };
            if iface.callback {
                let _ = writeln!(
                    install,
                    "    {{ // {i}\n        let f = cv::interface(scope, \"{i}\", cv::illegal_constructor);\n        f.remove_prototype();\n        let proto = v8::ObjectTemplate::new(scope);\n{props}        out.push(v8::Global::new(scope, f));\n    }}"
                );
                let _ = writeln!(
                    expose,
                    "    cv::expose(scope, global, &t[Iface::{i} as usize], \"{i}\");"
                );
                continue;
            }
            if iface.namespace {
                let _ = writeln!(
                    install,
                    "    {{ // {i}\n        let f = v8::FunctionTemplate::new(scope, cv::illegal_constructor);\n        let inst = f.instance_template(scope);\n        let proto = f.prototype_template(scope);\n{props}        out.push(v8::Global::new(scope, f));\n    }}"
                );
                let _ = writeln!(
                    expose,
                    "    cv::expose_namespace(scope, global, &t[Iface::{i} as usize], \"{}\");",
                    i
                );
                continue;
            }
            let parent = iface.parent.as_ref().map_or(String::new(), |p| {
                format!("        f.inherit(v8::Local::new(scope, &out[Iface::{p} as usize]));\n")
            });
            let _ = writeln!(
                install,
                "    {{ // {i}\n        let f = cv::interface(scope, \"{i}\", {ctor_fn});\n{parent}        let inst = f.instance_template(scope);\n        inst.set_internal_field_count(cv::FIELDS);\n        let proto = f.prototype_template(scope);\n{props}        crate::dom::hooks::template(scope, Iface::{i}, f);\n        out.push(v8::Global::new(scope, f));\n    }}"
            );
            if iface.ext.window && !iface.ext.global {
                let _ = writeln!(
                    expose,
                    "    cv::expose(scope, global, &t[Iface::{i} as usize], \"{i}\");"
                );
            }
            // `[LegacyFactoryFunction]` (`new Image()`), when allowlisted under its name.
            if let Some((fname, fargs)) = iface.ext.factory.clone()
                && self.allowed(i, &fname)
            {
                let ctx = format!("{fname} factory");
                let (code, names) = self.args_code(&fargs, &ctx);
                let body = format!(
                    "        if args.new_target().is_undefined() {{ return Err(cv::Error::Type(\"Failed to construct '{fname}': Please use the 'new' operator.\".into())); }}\n{code}        let r: Obj = crate::dom::{}::legacy_factory(cx, {})?;\n        cx.ret(r)\n",
                    snake(i),
                    names.join(", ")
                );
                self.callback(&format!("factory__{fname}"), &body);
                let _ = writeln!(
                    expose,
                    "    cv::expose_factory(scope, global, \"{i}\", \"{fname}\", {}, factory__{fname});",
                    required(&fargs)
                );
            }
        }
        self.emit_dicts();
        let _ = write!(
            self.out,
            "/// Builds every interface template, in `Iface` order.\npub fn templates(scope: &mut v8::PinScope<'_, '_, ()>) -> Vec<v8::Global<v8::FunctionTemplate>> {{\n    let mut out: Vec<v8::Global<v8::FunctionTemplate>> = Vec::with_capacity(Iface::ALL.len());\n{install}    out\n}}\n\n/// Puts interface objects and namespaces on a new global.\npub fn expose(scope: &mut v8::PinScope<'_, '_>, global: v8::Local<'_, v8::Object>, t: &[v8::Global<v8::FunctionTemplate>]) {{\n{expose}}}\n"
        );
    }
}

fn required(args: &[Arg]) -> usize {
    args.iter().filter(|a| !a.optional && !a.variadic).count()
}

/// Overloaded operations: when one overload's arguments are a prefix of another's, the longer
/// one is used, its extra arguments optional (`open(method, url)` / `open(method, url, async)`).
/// Overloads that need more arguments than the first are returned as alternatives, chosen by
/// argument count (`scrollTo(options)` / `scrollTo(x, y)`); any others are ignored.
fn merge_overloads(
    mut iface: Interface,
    model: &Model,
    included: &BTreeSet<String>,
) -> (Interface, Vec<(String, String, Alt)>) {
    let mut alts = Vec::new();
    let absent = |t: &Ty| {
        let mut t = model.resolve(t);
        while let Ty::Nullable(i) = t {
            t = *i;
        }
        matches!(&t, Ty::Named(n) if model.interfaces.contains_key(n) && !included.contains(n))
    };
    let mut i = 0;
    while i < iface.members.len() {
        let (owner, name) = (
            iface.members[i].owner.clone(),
            iface.members[i].member.name().to_string(),
        );
        let Member::Op { args: a, .. } = &iface.members[i].member else {
            i += 1;
            continue;
        };
        let a = a.clone();
        let later = (i + 1..iface.members.len()).find(|&j| {
            iface.members[j].owner == owner
                && matches!(&iface.members[j].member, Member::Op { name: n, .. } if *n == name)
        });
        if let Some(j) = later
            && let Member::Op { args: b, .. } = &iface.members[j].member
        {
            let (short, long) = if a.len() <= b.len() { (&a, b) } else { (b, &a) };
            let prefix = short
                .iter()
                .zip(long.iter())
                .all(|(x, y)| format!("{:?}", x.ty) == format!("{:?}", y.ty))
                && !long.iter().skip(short.len()).any(|x| absent(&x.ty));
            if prefix {
                let mut merged = long.clone();
                for arg in merged.iter_mut().skip(short.len()) {
                    arg.optional = true;
                }
                if let Member::Op { args, .. } = &mut iface.members[i].member {
                    *args = merged;
                }
                iface.members.remove(j);
                continue;
            }
            if required(b) > a.iter().filter(|x| !x.variadic).count()
                && !b.iter().any(|x| absent(&x.ty))
            {
                alts.push((owner, name, (b.clone(), None)));
                iface.members.remove(j);
                continue;
            }
            // Same arity, told apart by an interface-typed argument where the other takes
            // something else (`append(name, value)` / `append(name, blob, filename)`).
            let by = b
                .iter()
                .enumerate()
                .find_map(|(k, x)| match model.resolve(&x.ty) {
                    Ty::Named(n)
                        if model.interfaces.contains_key(&n)
                            && !absent(&x.ty)
                            && a.get(k).is_some_and(|y| {
                                format!("{:?}", y.ty) != format!("{:?}", x.ty)
                            }) =>
                    {
                        Some((k, n))
                    }
                    _ => None,
                });
            if let Some(by) = by
                && required(b) == required(&a)
                && !b.iter().any(|x| absent(&x.ty))
            {
                alts.push((owner, name, (b.clone(), Some(by))));
                iface.members.remove(j);
                continue;
            }
        }
        i += 1;
    }
    (iface, alts)
}

fn reflect_kind(t: &Ty, ext: &Ext) -> String {
    let default = ext.reflect_default.clone().unwrap_or_else(|| "0".into());
    match t {
        _ if ext.reflect_url => "cv::Reflect::Url".into(),
        Ty::Str => "cv::Reflect::Str".into(),
        Ty::Nullable(_) => "cv::Reflect::NullableStr".into(),
        Ty::Bool => "cv::Reflect::Bool".into(),
        Ty::Int(_, true) if ext.reflect_non_negative => "cv::Reflect::NonNegative".into(),
        Ty::Int(_, false) => format!("cv::Reflect::Unsigned({default})"),
        Ty::Int(..) => format!("cv::Reflect::Long({default})"),
        Ty::Float(_) => format!("cv::Reflect::Double({default} as f64)"),
        _ => "cv::Reflect::Str".into(),
    }
}

/// Generates the bindings, or the list of problems.
pub fn generate(model: &Model, allow: &Allow) -> Result<String, Vec<String>> {
    let mut g = Gen {
        model,
        allow,
        order: Vec::new(),
        included: BTreeSet::new(),
        dicts: BTreeSet::new(),
        alts: std::collections::BTreeMap::new(),
        out: String::new(),
        errors: Vec::new(),
    };
    g.run();
    if g.errors.is_empty() {
        Ok(g.out)
    } else {
        Err(g.errors)
    }
}
