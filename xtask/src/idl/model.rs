//! A small WebIDL model built from weedle2's AST: interfaces with partials and mixins merged,
//! dictionaries with inheritance, typedefs, enums and callbacks.

use std::collections::{BTreeMap, BTreeSet};

use weedle::Definition;
use weedle::argument::Argument;
use weedle::attribute::{ExtendedAttribute, ExtendedAttributeList, IdentifierOrString};
use weedle::interface::InterfaceMember;
use weedle::literal::{ConstValue, DefaultValue, IntegerLit};
use weedle::mixin::MixinMember;
use weedle::namespace::NamespaceMember;
use weedle::types::{
    AttributedNonAnyType, FloatingPointType, IntegerType, NonAnyType, ReturnType, SingleType, Type,
    UnionMemberType,
};

/// An IDL type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    /// `undefined`.
    Undefined,
    /// `any`.
    Any,
    /// `boolean`.
    Bool,
    /// An integer type: (bits, signed).
    Int(u8, bool),
    /// A floating-point type: restricted (`double`) or not (`unrestricted double`).
    Float(bool),
    /// `DOMString`, `USVString`, `ByteString`, `CSSOMString`.
    Str,
    /// `object`.
    Object,
    /// A named type: interface, dictionary, enum, callback.
    Named(String),
    /// `T?`.
    Nullable(Box<Ty>),
    /// `sequence<T>` / `FrozenArray<T>` / `ObservableArray<T>`.
    Seq(Box<Ty>),
    /// `(A or B)`.
    Union(Vec<Ty>),
    /// A buffer type (`ArrayBuffer`, a typed array, `DataView`): converted by hand.
    Buffer,
    /// `Promise<T>`, typed arrays, records, symbols: types pageon's bindings do not convert.
    Other(String),
}

impl Ty {
    fn nullable(self, q: bool) -> Ty {
        if q {
            Ty::Nullable(Box::new(self))
        } else {
            self
        }
    }
}

/// Extended attributes pageon reads.
#[derive(Debug, Clone, Default)]
pub struct Ext {
    /// `[Reflect]`, `[Reflect=name]`: the content attribute name.
    pub reflect: Option<String>,
    /// `[ReflectURL]`.
    pub reflect_url: bool,
    /// `[ReflectSetter]` (getter hand-written, setter reflects).
    pub reflect_setter: bool,
    /// `[ReflectDefault=n]` (integer only).
    pub reflect_default: Option<String>,
    /// `[ReflectNonNegative]`.
    pub reflect_non_negative: bool,
    /// An `ObservableArray<T>` attribute (the getter returns a JavaScript value).
    pub observable: bool,
    /// `[PutForwards=name]`.
    pub put_forwards: Option<String>,
    /// `[LegacyNullToEmptyString]`.
    pub null_to_empty: bool,
    /// `[Global]`.
    pub global: bool,
    /// `[Exposed=…]` names Window (or `*`).
    pub window: bool,
    /// `[LegacyFactoryFunction=Name(args)]` (`Image`, `Option`): the function's name and
    /// arguments.
    pub factory: Option<(String, Vec<Arg>)>,
    /// `[LegacyUnforgeable]`.
    pub unforgeable: bool,
    /// A `stringifier` attribute: `toString` returns it.
    pub stringifier: bool,
}

/// An operation or constructor argument.
#[derive(Debug, Clone)]
pub struct Arg {
    /// Name.
    pub name: String,
    /// Type.
    pub ty: Ty,
    /// `optional`.
    pub optional: bool,
    /// Default value, as IDL text.
    pub default: Option<String>,
    /// `T... name`.
    pub variadic: bool,
    /// `[LegacyNullToEmptyString]` on the argument type.
    pub null_to_empty: bool,
}

/// An interface member.
#[derive(Debug, Clone)]
pub enum Member {
    /// `attribute`.
    Attr {
        /// Name.
        name: String,
        /// Type.
        ty: Ty,
        /// `readonly`.
        readonly: bool,
        /// `static`.
        is_static: bool,
        /// Extended attributes.
        ext: Ext,
    },
    /// An operation.
    Op {
        /// Name.
        name: String,
        /// Arguments.
        args: Vec<Arg>,
        /// Return type.
        ret: Ty,
        /// `static`.
        is_static: bool,
    },
    /// `const`.
    Const {
        /// Name.
        name: String,
        /// Rust literal of the value.
        value: String,
    },
    /// `constructor(...)`.
    Ctor {
        /// Arguments.
        args: Vec<Arg>,
        /// `[HTMLConstructor]`: HTML's element constructors (custom elements).
        html: bool,
    },
    /// `iterable<K, V>`: `entries`, `keys`, `values`, `forEach` and `@@iterator` over a
    /// hand-written `pairs`.
    Iterable,
    /// `setlike<T>`: `size`, `has`, the iteration methods and, unless read-only, `add`,
    /// `delete` and `clear`, over hand-written `set_*` functions and `pairs`.
    Setlike {
        /// `readonly setlike`.
        readonly: bool,
    },
}

impl Member {
    /// The member's name (`constructor` for constructors).
    pub fn name(&self) -> &str {
        match self {
            Member::Attr { name, .. } | Member::Op { name, .. } | Member::Const { name, .. } => {
                name
            }
            Member::Ctor { .. } => "constructor",
            Member::Iterable => "iterable",
            Member::Setlike { .. } => "setlike",
        }
    }
}

/// A member with the interface or mixin that declares it (for the implementation module).
#[derive(Debug, Clone)]
pub struct Owned {
    /// Declaring interface or mixin.
    pub owner: String,
    /// The member.
    pub member: Member,
}

/// An interface or namespace.
#[derive(Debug, Clone, Default)]
pub struct Interface {
    /// Name.
    pub name: String,
    /// Parent interface.
    pub parent: Option<String>,
    /// Members, own and from included mixins.
    pub members: Vec<Owned>,
    /// Extended attributes.
    pub ext: Ext,
    /// A `namespace` (plain object, no prototype).
    pub namespace: bool,
    /// A callback interface with constants (`NodeFilter`): an interface object that holds the
    /// constants, with no prototype and no constructor (Web IDL §3.7.6).
    pub callback: bool,
}

/// A dictionary member: name, type, default (IDL text).
pub type DictMember = (String, Ty, Option<String>);

/// A dictionary.
#[derive(Debug, Clone, Default)]
pub struct Dictionary {
    /// Parent dictionary.
    pub parent: Option<String>,
    /// Members: (name, type, default).
    pub members: Vec<(String, Ty, Option<String>)>,
}

/// The whole model.
#[derive(Debug, Default)]
pub struct Model {
    /// Interfaces and namespaces.
    pub interfaces: BTreeMap<String, Interface>,
    /// Dictionaries.
    pub dictionaries: BTreeMap<String, Dictionary>,
    /// Typedefs.
    pub typedefs: BTreeMap<String, Ty>,
    /// Enums.
    pub enums: BTreeSet<String>,
    /// Callback functions and callback interfaces.
    pub callbacks: BTreeSet<String>,
}

/// Rewrites syntax newer than weedle2's grammar into equivalent forms it parses.
pub fn normalize(text: &str) -> String {
    let mut s = text.replace("Exposed=*", "Exposed=Any");
    // `ObservableArray<T>` attributes parse as sequences marked `[PageonObservable]`: their
    // getters return the page's own array (hand-written), their setters take a sequence.
    while let Some(i) = s.find("attribute ObservableArray<") {
        let head = s[..i].trim_end();
        let marked = if head.ends_with(']') {
            let open = head.rfind('[').unwrap_or(0);
            format!("{}[PageonObservable, {}", &s[..open], &s[open + 1..i])
        } else {
            format!("{}[PageonObservable] ", &s[..i])
        };
        let rest = s[i..].replacen("ObservableArray<", "sequence<", 1);
        s = marked + &rest;
    }
    // Float literals in extended attributes, e.g. `ReflectDefault=1.0` → `ReflectDefault=1`.
    while let Some(i) = s.find("ReflectDefault=") {
        let start = i + "ReflectDefault=".len();
        let end = s[start..].find([',', ']']).map_or(s.len(), |e| start + e);
        let v = s[start..end].trim();
        let int = v.split('.').next().unwrap_or("0").to_string();
        s.replace_range(i..end, &format!("ReflectDefaultValue={int}"));
    }
    let mut s = s.replace("ReflectDefaultValue=", "ReflectDefault=");
    // `required [EnforceRange] T name` (current grammar) → `[EnforceRange] required T name`.
    let mut from = 0;
    while let Some(i) = s[from..].find("required [").map(|i| from + i) {
        let Some(close) = s[i..].find(']').map(|c| i + c) else {
            break;
        };
        let attrs = s[i + "required ".len()..=close].to_string();
        s.replace_range(i..=close, &format!("{attrs} required"));
        from = close + 1;
    }
    s
}

fn ext(list: &Option<ExtendedAttributeList<'_>>) -> Ext {
    let mut e = Ext::default();
    let Some(list) = list else { return e };
    for a in &list.body.list {
        match a {
            ExtendedAttribute::NoArgs(n) => match n.0.0 {
                "Reflect" => e.reflect = Some(String::new()),
                "ReflectURL" => e.reflect_url = true,
                "ReflectSetter" => e.reflect_setter = true,
                "ReflectNonNegative" => e.reflect_non_negative = true,
                "PageonObservable" => e.observable = true,
                "LegacyNullToEmptyString" => e.null_to_empty = true,
                "Global" => e.global = true,
                "LegacyUnforgeable" => e.unforgeable = true,
                _ => {}
            },
            ExtendedAttribute::Ident(i) => {
                let rhs = match i.rhs {
                    IdentifierOrString::Identifier(id) => id.0.to_string(),
                    IdentifierOrString::String(s) => s.0.to_string(),
                };
                match i.lhs_identifier.0 {
                    "Reflect" => e.reflect = Some(rhs),
                    "ReflectDefault" => e.reflect_default = Some(rhs),
                    "PutForwards" => e.put_forwards = Some(rhs),
                    "Global" => e.global = true,
                    "Exposed" => e.window = rhs == "Window" || rhs == "Any",
                    _ => {}
                }
            }
            ExtendedAttribute::NamedArgList(n) if n.lhs_identifier.0 == "LegacyFactoryFunction" => {
                e.factory = Some((n.rhs_identifier.0.to_string(), args(&n.args.body)));
            }
            ExtendedAttribute::IdentList(l) => {
                if l.identifier.0 == "Exposed" {
                    e.window = l.list.body.list.iter().any(|i| i.0 == "Window");
                }
                if l.identifier.0 == "Global" {
                    e.global = true;
                }
            }
            _ => {}
        }
    }
    // `[Reflect]` with `ReflectURL` only: the URL flag implies reflection.
    if (e.reflect_url || e.reflect_non_negative) && e.reflect.is_none() {
        e.reflect = Some(String::new());
    }
    e
}

fn int_ty(t: &IntegerType) -> Ty {
    match t {
        IntegerType::LongLong(l) => Ty::Int(64, l.unsigned.is_none()),
        IntegerType::Long(l) => Ty::Int(32, l.unsigned.is_none()),
        IntegerType::Short(s) => Ty::Int(16, s.unsigned.is_none()),
    }
}

fn float_ty(t: &FloatingPointType) -> Ty {
    match t {
        FloatingPointType::Float(f) => Ty::Float(f.unrestricted.is_none()),
        FloatingPointType::Double(d) => Ty::Float(d.unrestricted.is_none()),
    }
}

fn non_any(t: &NonAnyType<'_>) -> Ty {
    match t {
        NonAnyType::Integer(m) => int_ty(&m.type_).nullable(m.q_mark.is_some()),
        NonAnyType::FloatingPoint(m) => float_ty(&m.type_).nullable(m.q_mark.is_some()),
        NonAnyType::Boolean(m) => Ty::Bool.nullable(m.q_mark.is_some()),
        NonAnyType::Byte(m) => Ty::Int(8, true).nullable(m.q_mark.is_some()),
        NonAnyType::Octet(m) => Ty::Int(8, false).nullable(m.q_mark.is_some()),
        NonAnyType::ByteString(m) => Ty::Str.nullable(m.q_mark.is_some()),
        NonAnyType::DOMString(m) => Ty::Str.nullable(m.q_mark.is_some()),
        NonAnyType::USVString(m) => Ty::Str.nullable(m.q_mark.is_some()),
        NonAnyType::Sequence(m) => {
            Ty::Seq(Box::new(ty(&m.type_.generics.body))).nullable(m.q_mark.is_some())
        }
        NonAnyType::FrozenArrayType(m) => {
            Ty::Seq(Box::new(ty(&m.type_.generics.body))).nullable(m.q_mark.is_some())
        }
        NonAnyType::Object(m) => Ty::Object.nullable(m.q_mark.is_some()),
        NonAnyType::Identifier(m) => {
            let name = m.type_.0;
            let base = if name == "CSSOMString" {
                Ty::Str
            } else {
                Ty::Named(name.to_string())
            };
            base.nullable(m.q_mark.is_some())
        }
        other => {
            let name: String = format!("{other:?}").chars().take(40).collect();
            const BUFFERS: [&str; 13] = [
                "ArrayBuffer",
                "SharedArrayBuffer",
                "DataView",
                "Int8Array",
                "Int16Array",
                "Int32Array",
                "Uint8Array",
                "Uint16Array",
                "Uint32Array",
                "Uint8Clamped",
                "Float32Array",
                "Float64Array",
                "BigInt64Array",
            ];
            if BUFFERS.iter().any(|b| name.starts_with(b)) || name.starts_with("BigUint64Array") {
                Ty::Buffer
            } else {
                Ty::Other(name)
            }
        }
    }
}

fn union_member(m: &UnionMemberType<'_>) -> Ty {
    match m {
        UnionMemberType::Single(AttributedNonAnyType { type_, .. }) => non_any(type_),
        UnionMemberType::Union(u) => {
            Ty::Union(u.type_.body.list.iter().map(union_member).collect())
                .nullable(u.q_mark.is_some())
        }
    }
}

fn ty(t: &Type<'_>) -> Ty {
    match t {
        Type::Single(SingleType::Any(_)) => Ty::Any,
        Type::Single(SingleType::NonAny(n)) => non_any(n),
        Type::Union(u) => Ty::Union(u.type_.body.list.iter().map(union_member).collect())
            .nullable(u.q_mark.is_some()),
    }
}

fn ret(t: &ReturnType<'_>) -> Ty {
    match t {
        ReturnType::Undefined(_) => Ty::Undefined,
        ReturnType::Type(t) => ty(t),
    }
}

fn int_lit(l: &IntegerLit<'_>) -> String {
    match l {
        IntegerLit::Dec(d) => d.0.to_string(),
        IntegerLit::Hex(h) => h.0.to_string(),
        IntegerLit::Oct(o) => {
            let digits = o.0.trim_start_matches('-').trim_start_matches('0');
            if digits.is_empty() {
                "0".into()
            } else {
                format!("0o{digits}")
            }
        }
    }
}

fn default(d: &DefaultValue<'_>) -> String {
    match d {
        DefaultValue::Boolean(b) => b.0.to_string(),
        DefaultValue::EmptyArray(_) => "[]".into(),
        DefaultValue::EmptyDictionary(_) => "{}".into(),
        DefaultValue::Float(f) => format!("{f:?}")
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect(),
        DefaultValue::Integer(i) => int_lit(i),
        DefaultValue::Null(_) => "null".into(),
        DefaultValue::String(s) => format!("\"{}\"", s.0),
    }
}

fn args(list: &weedle::argument::ArgumentList<'_>) -> Vec<Arg> {
    list.list
        .iter()
        .map(|a| match a {
            Argument::Single(s) => Arg {
                name: s.identifier.0.to_string(),
                ty: ty(&s.type_.type_),
                optional: s.optional.is_some(),
                default: s.default.map(|d| default(&d.value)),
                variadic: false,
                null_to_empty: ext(&s.type_.attributes).null_to_empty,
            },
            Argument::Variadic(v) => Arg {
                name: v.identifier.0.to_string(),
                ty: ty(&v.type_),
                optional: true,
                default: None,
                variadic: true,
                null_to_empty: false,
            },
        })
        .collect()
}

fn attr(
    name: &str,
    t: &weedle::types::AttributedType<'_>,
    readonly: bool,
    is_static: bool,
    e: &Option<ExtendedAttributeList<'_>>,
) -> Member {
    let mut x = ext(e);
    x.null_to_empty |= ext(&t.attributes).null_to_empty;
    Member::Attr {
        name: name.to_string(),
        ty: ty(&t.type_),
        readonly,
        is_static,
        ext: x,
    }
}

fn const_value(v: &ConstValue<'_>) -> String {
    match v {
        ConstValue::Boolean(b) => b.0.to_string(),
        ConstValue::Integer(i) => int_lit(i),
        ConstValue::Float(f) => format!("{f:?}")
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect(),
        ConstValue::Null(_) => "null".into(),
    }
}

/// An escaped identifier (`_any`) without its underscore.
fn unescape(name: &str) -> String {
    name.strip_prefix('_').unwrap_or(name).to_string()
}

fn iface_members(owner: &str, members: &[InterfaceMember<'_>]) -> Vec<Owned> {
    let mut out = Vec::new();
    for m in members {
        let member = match m {
            InterfaceMember::Const(c) => Member::Const {
                name: c.identifier.0.to_string(),
                value: const_value(&c.const_value),
            },
            InterfaceMember::Attribute(a) => {
                let is_static = matches!(
                    a.modifier,
                    Some(weedle::interface::StringifierOrInheritOrStatic::Static(_))
                );
                let mut m = attr(
                    a.identifier.0,
                    &a.type_,
                    a.readonly.is_some(),
                    is_static,
                    &a.attributes,
                );
                if matches!(
                    a.modifier,
                    Some(weedle::interface::StringifierOrInheritOrStatic::Stringifier(_))
                ) && let Member::Attr { ext, .. } = &mut m
                {
                    ext.stringifier = true;
                }
                m
            }
            InterfaceMember::Constructor(c) => Member::Ctor {
                args: args(&c.args.body),
                html: c.attributes.as_ref().is_some_and(|l| {
                    l.body.list.iter().any(
                        |a| matches!(a, ExtendedAttribute::NoArgs(n) if n.0.0 == "HTMLConstructor"),
                    )
                }),
            },
            InterfaceMember::Operation(o) => {
                let Some(id) = o.identifier else { continue }; // special (getter/setter) operations
                let is_static = matches!(
                    o.modifier,
                    Some(weedle::interface::StringifierOrStatic::Static(_))
                );
                Member::Op {
                    name: unescape(id.0),
                    args: args(&o.args.body),
                    ret: ret(&o.return_type),
                    is_static,
                }
            }
            InterfaceMember::Iterable(_) => Member::Iterable,
            InterfaceMember::Setlike(s) => Member::Setlike {
                readonly: s.readonly.is_some(),
            },
            InterfaceMember::Stringifier(_) => Member::Op {
                name: "toString".into(),
                args: Vec::new(),
                ret: Ty::Str,
                is_static: false,
            },
            _ => continue,
        };
        out.push(Owned {
            owner: owner.to_string(),
            member,
        });
    }
    out
}

fn mixin_members(owner: &str, members: &[MixinMember<'_>]) -> Vec<Owned> {
    let mut out = Vec::new();
    for m in members {
        let member = match m {
            MixinMember::Const(c) => Member::Const {
                name: c.identifier.0.to_string(),
                value: const_value(&c.const_value),
            },
            MixinMember::Attribute(a) => attr(
                a.identifier.0,
                &a.type_,
                a.readonly.is_some(),
                false,
                &a.attributes,
            ),
            MixinMember::Operation(o) => {
                let Some(id) = o.identifier else { continue };
                Member::Op {
                    name: id.0.to_string(),
                    args: args(&o.args.body),
                    ret: ret(&o.return_type),
                    is_static: false,
                }
            }
            MixinMember::Stringifier(_) => Member::Op {
                name: "toString".into(),
                args: Vec::new(),
                ret: Ty::Str,
                is_static: false,
            },
        };
        out.push(Owned {
            owner: owner.to_string(),
            member,
        });
    }
    out
}

/// Loads every `.idl` file of `dir` (sorted by name) into one model.
pub fn load(dir: &std::path::Path) -> Result<Model, String> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "idl"))
        .collect();
    files.sort();
    let mut model = Model::default();
    let mut mixins: BTreeMap<String, Vec<Owned>> = BTreeMap::new();
    let mut includes: Vec<(String, String)> = Vec::new();
    let mut partials: Vec<(String, Vec<Owned>)> = Vec::new();
    let mut partial_dicts: Vec<(String, Vec<DictMember>)> = Vec::new();
    let texts: Vec<(String, String)> = files
        .iter()
        .map(|p| {
            Ok((
                p.display().to_string(),
                normalize(&std::fs::read_to_string(p).map_err(|e| e.to_string())?),
            ))
        })
        .collect::<Result<_, String>>()?;
    for (file, text) in &texts {
        let parsed = std::panic::catch_unwind(|| weedle::parse(text))
            .map_err(|_| format!("{file}: weedle2 cannot parse this file"))?
            .map_err(|e| {
                format!("{file}: {e:?}")
                    .chars()
                    .take(400)
                    .collect::<String>()
            })?;
        for d in parsed {
            match d {
                Definition::Interface(i) => {
                    let name = i.identifier.0.to_string();
                    let entry = model.interfaces.entry(name.clone()).or_default();
                    entry.name = name.clone();
                    entry.parent = i.inheritance.map(|p| p.identifier.0.to_string());
                    entry.ext = ext(&i.attributes);
                    entry
                        .members
                        .splice(0..0, iface_members(&name, &i.members.body));
                }
                Definition::PartialInterface(p) => {
                    partials.push((
                        p.identifier.0.to_string(),
                        iface_members(p.identifier.0, &p.members.body),
                    ));
                }
                Definition::InterfaceMixin(m) => {
                    mixins
                        .entry(m.identifier.0.to_string())
                        .or_default()
                        .extend(mixin_members(m.identifier.0, &m.members.body));
                }
                Definition::PartialInterfaceMixin(m) => {
                    mixins
                        .entry(m.identifier.0.to_string())
                        .or_default()
                        .extend(mixin_members(m.identifier.0, &m.members.body));
                }
                Definition::IncludesStatement(s) => includes.push((
                    s.lhs_identifier.0.to_string(),
                    s.rhs_identifier.0.to_string(),
                )),
                Definition::Namespace(n) => {
                    let name = n.identifier.0.to_string();
                    let entry = model.interfaces.entry(name.clone()).or_default();
                    entry.name = name.clone();
                    entry.namespace = true;
                    entry.ext = ext(&n.attributes);
                    for m in &n.members.body {
                        if let NamespaceMember::Operation(o) = m
                            && let Some(id) = o.identifier
                        {
                            entry.members.push(Owned {
                                owner: name.clone(),
                                member: Member::Op {
                                    name: id.0.to_string(),
                                    args: args(&o.args.body),
                                    ret: ret(&o.return_type),
                                    is_static: false,
                                },
                            });
                        }
                    }
                }
                Definition::PartialNamespace(n) => {
                    let name = n.identifier.0.to_string();
                    let entry = model.interfaces.entry(name.clone()).or_default();
                    entry.name = name.clone();
                    entry.namespace = true;
                    for m in &n.members.body {
                        if let NamespaceMember::Operation(o) = m
                            && let Some(id) = o.identifier
                        {
                            entry.members.push(Owned {
                                owner: name.clone(),
                                member: Member::Op {
                                    name: id.0.to_string(),
                                    args: args(&o.args.body),
                                    ret: ret(&o.return_type),
                                    is_static: false,
                                },
                            });
                        }
                    }
                }
                Definition::Dictionary(d) => {
                    let entry = model
                        .dictionaries
                        .entry(d.identifier.0.to_string())
                        .or_default();
                    entry.parent = d.inheritance.map(|p| p.identifier.0.to_string());
                    for m in &d.members.body {
                        entry.members.push((
                            m.identifier.0.to_string(),
                            ty(&m.type_),
                            m.default.map(|x| default(&x.value)),
                        ));
                    }
                }
                Definition::PartialDictionary(d) => partial_dicts.push((
                    d.identifier.0.to_string(),
                    d.members
                        .body
                        .iter()
                        .map(|m| {
                            (
                                m.identifier.0.to_string(),
                                ty(&m.type_),
                                m.default.map(|x| default(&x.value)),
                            )
                        })
                        .collect(),
                )),
                Definition::Typedef(t) => {
                    model
                        .typedefs
                        .insert(t.identifier.0.to_string(), ty(&t.type_.type_));
                }
                Definition::Enum(e) => {
                    model.enums.insert(e.identifier.0.to_string());
                }
                Definition::Callback(c) => {
                    model.callbacks.insert(c.identifier.0.to_string());
                }
                Definition::CallbackInterface(c) => {
                    let name = c.identifier.0.to_string();
                    model.callbacks.insert(name.clone());
                    let consts: Vec<Owned> = iface_members(&name, &c.members.body)
                        .into_iter()
                        .filter(|m| matches!(m.member, Member::Const { .. }))
                        .collect();
                    if !consts.is_empty() {
                        let entry = model.interfaces.entry(name.clone()).or_default();
                        entry.name = name;
                        entry.callback = true;
                        entry.ext = ext(&c.attributes);
                        entry.members = consts;
                    }
                }
                _ => {}
            }
        }
    }
    // `WindowProxy` is the window as scripts see it.
    model
        .typedefs
        .insert("WindowProxy".into(), Ty::Named("Window".into()));
    // Geometry's legacy SVG aliases.
    model
        .typedefs
        .entry("SVGRect".into())
        .or_insert(Ty::Named("DOMRect".into()));
    model
        .typedefs
        .entry("SVGMatrix".into())
        .or_insert(Ty::Named("DOMMatrix".into()));
    for (name, members) in partials {
        if let Some(i) = model.interfaces.get_mut(&name) {
            i.members.extend(members);
        }
    }
    for (name, members) in partial_dicts {
        if let Some(d) = model.dictionaries.get_mut(&name) {
            d.members.extend(members);
        }
    }
    for (iface, mixin) in includes {
        if let (Some(i), Some(m)) = (model.interfaces.get_mut(&iface), mixins.get(&mixin)) {
            i.members.extend(m.iter().cloned());
        }
    }
    Ok(model)
}

impl Model {
    /// Resolves typedefs (recursively).
    pub fn resolve(&self, t: &Ty) -> Ty {
        match t {
            Ty::Named(n) => match self.typedefs.get(n) {
                Some(inner) => self.resolve(inner),
                None => t.clone(),
            },
            Ty::Nullable(i) => Ty::Nullable(Box::new(self.resolve(i))),
            Ty::Seq(i) => Ty::Seq(Box::new(self.resolve(i))),
            Ty::Union(v) => Ty::Union(v.iter().map(|x| self.resolve(x)).collect()),
            other => other.clone(),
        }
    }

    /// The interface and its ancestors, nearest first.
    pub fn chain(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = Some(name.to_string());
        while let Some(c) = cur {
            cur = self.interfaces.get(&c).and_then(|i| i.parent.clone());
            out.push(c);
        }
        out
    }

    /// All members of a dictionary including inherited ones (ancestors first).
    pub fn dict_members(&self, name: &str) -> Vec<(String, Ty, Option<String>)> {
        let mut chain = Vec::new();
        let mut cur = Some(name.to_string());
        while let Some(c) = cur {
            cur = self.dictionaries.get(&c).and_then(|d| d.parent.clone());
            chain.push(c);
        }
        chain
            .iter()
            .rev()
            .filter_map(|c| self.dictionaries.get(c))
            .flat_map(|d| d.members.clone())
            .collect()
    }
}
