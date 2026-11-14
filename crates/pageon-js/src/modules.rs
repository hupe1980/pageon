//! HTML's script model beyond classic parser scripts: module scripts (graphs, the
//! module map, import maps, `import.meta`, dynamic `import()`, top-level await, JSON
//! modules), `nomodule`, and scripts inserted by DOM methods.
//!
//! V8 links and evaluates; this module does HTML's half: fetching (through the realm's network
//! state and the caller's `Env`, so policy, recording and replay apply), resolution, and the
//! hooks. Fetches are synchronous, so a graph is complete before it is instantiated.

/// Most modules one document fetches, over all its graphs and dynamic imports (SR-007).
pub const MAX_MODULES: u32 = 2_000;

use pageon_dom::NodeId;
use pageon_net::{Credentials, Dest, FetchRequest, Mode};
use url::Url;

use crate::PageErrorKind;
use crate::cv::{self, Cx};
use crate::engine::Engine;
use crate::realm::Target;

/// A module of a realm's module map.
#[derive(Debug)]
pub struct ModuleRecord {
    /// Its URL (for an inline module script: the document URL).
    pub url: String,
    /// Inline module scripts are not in the map (never found by URL).
    pub inline: bool,
    /// A JSON module.
    pub json: Option<cv::Value>,
    pub(crate) hash: i32,
    pub(crate) module: v8::Global<v8::Module>,
}

/// Specifier keys → URLs (`None`: blocked).
type Entries = Vec<(String, Option<String>)>;
/// A module's requests: specifier, and whether it is a JSON import (or the attribute error).
type Requests = Vec<(String, Result<bool, String>)>;

/// A document's import map.
#[derive(Debug, Default, Clone)]
pub struct ImportMap {
    /// Specifier keys → URLs (`None`: blocked), longest key first.
    imports: Vec<(String, Option<String>)>,
    /// Scope prefixes → their maps, longest prefix first.
    scopes: Vec<(String, Entries)>,
}

/// What a `<script>` is, by its `type` (HTML's "prepare the script element").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    /// A classic script.
    Classic,
    /// A module script.
    Module,
    /// An import map.
    ImportMap,
    /// A data block (never runs).
    Data,
}

/// The kind of a script element.
pub fn script_kind(node: &pageon_dom::Node) -> ScriptKind {
    let ty = node.attr("type").map(|t| t.trim().to_ascii_lowercase());
    match ty.as_deref() {
        None | Some("") => ScriptKind::Classic,
        Some("module") => ScriptKind::Module,
        Some("importmap") => ScriptKind::ImportMap,
        Some(t) if is_js_mime(t) => ScriptKind::Classic,
        _ => ScriptKind::Data,
    }
}

fn is_js_mime(t: &str) -> bool {
    let essence = t.split(';').next().unwrap_or_default().trim();
    matches!(
        essence,
        "text/javascript"
            | "application/javascript"
            | "application/ecmascript"
            | "text/ecmascript"
            | "text/jscript"
            | "application/x-javascript"
            | "text/x-javascript"
            | "text/livescript"
            | "application/x-ecmascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
    )
}

/// A job of the page's script queue: run where the `Env` is available (`Engine::run_scripts`).
#[derive(Debug)]
pub enum ScriptJob {
    /// A script element inserted by a DOM method.
    Element {
        /// Realm.
        realm: usize,
        /// The element.
        node: NodeId,
    },
    /// A dynamic `import()`.
    Import {
        /// Realm.
        realm: usize,
        /// The resolved URL.
        url: String,
        /// `with {type: "json"}`.
        json: bool,
        /// The promise's resolver.
        resolver: v8::Global<v8::PromiseResolver>,
    },
}

impl ImportMap {
    /// Parses an import map (`base`: the document URL); unusable entries are skipped and
    /// reported in `warnings`.
    fn parse(text: &str, base: &Url, warnings: &mut Vec<String>) -> Option<Self> {
        let Ok(serde_json::Value::Object(top)) = serde_json::from_str::<serde_json::Value>(text)
        else {
            warnings.push("the import map is not a JSON object".into());
            return None;
        };
        let map = |v: Option<&serde_json::Value>, base: &Url, warnings: &mut Vec<String>| {
            let mut out: Vec<(String, Option<String>)> = Vec::new();
            for (k, v) in v.and_then(|v| v.as_object()).into_iter().flatten() {
                if k.is_empty() {
                    continue;
                }
                let key = url_like(k, base).map_or_else(|| k.clone(), |u| u.to_string());
                let target = v.as_str().and_then(|s| url_like(s, base));
                let target = match target {
                    Some(t) if key.ends_with('/') && !t.as_str().ends_with('/') => {
                        warnings.push(format!(
                            "import map entry {k:?}: a key ending in \"/\" needs an address ending in \"/\""
                        ));
                        None
                    }
                    Some(t) => Some(t.to_string()),
                    None => {
                        warnings.push(format!("import map entry {k:?}: invalid address {v}"));
                        None
                    }
                };
                out.push((key, target));
            }
            out.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
            out
        };
        let imports = map(top.get("imports"), base, warnings);
        let mut scopes = Vec::new();
        for (prefix, m) in top
            .get("scopes")
            .and_then(|s| s.as_object())
            .into_iter()
            .flatten()
        {
            let Ok(p) = base.join(prefix) else {
                warnings.push(format!("import map scope {prefix:?} is not a URL"));
                continue;
            };
            scopes.push((p.to_string(), map(Some(m), base, warnings)));
        }
        scopes.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
        Some(Self { imports, scopes })
    }

    /// Merges a later map (HTML's multiple import maps): existing entries win.
    fn merge(&mut self, other: Self) {
        for (k, v) in other.imports {
            if !self.imports.iter().any(|(x, _)| *x == k) {
                self.imports.push((k, v));
            }
        }
        self.imports.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
        for (p, m) in other.scopes {
            if !self.scopes.iter().any(|(x, _)| *x == p) {
                self.scopes.push((p, m));
            }
        }
        self.scopes.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
    }
}

/// A URL-like specifier (`/`, `./`, `../`, or absolute), parsed against `base`.
fn url_like(s: &str, base: &Url) -> Option<Url> {
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") {
        return base.join(s).ok();
    }
    Url::parse(s).ok()
}

/// One map's match for `key` (exact, or the longest trailing-slash prefix).
fn lookup(map: &[(String, Option<String>)], key: &str) -> Option<Result<String, String>> {
    for (k, v) in map {
        if k == key {
            return Some(
                v.clone()
                    .ok_or_else(|| format!("{key:?} is blocked by the import map")),
            );
        }
        if k.ends_with('/') && key.starts_with(k.as_str()) {
            return Some(match v {
                Some(v) => Url::parse(v)
                    .and_then(|b| b.join(&key[k.len()..]))
                    .map(|u| u.to_string())
                    .map_err(|_| format!("{key:?} does not resolve through the import map")),
                None => Err(format!("{key:?} is blocked by the import map")),
            });
        }
    }
    None
}

/// HTML's "resolve a module specifier".
pub(crate) fn resolve(map: &ImportMap, specifier: &str, base: &Url) -> Result<Url, String> {
    let as_url = url_like(specifier, base);
    let key = as_url
        .as_ref()
        .map_or_else(|| specifier.to_string(), Url::to_string);
    let referrer = base.as_str();
    for (prefix, m) in &map.scopes {
        let applies =
            prefix == referrer || (prefix.ends_with('/') && referrer.starts_with(prefix.as_str()));
        if applies && let Some(r) = lookup(m, &key) {
            return r.and_then(|u| Url::parse(&u).map_err(|e| e.to_string()));
        }
    }
    if let Some(r) = lookup(&map.imports, &key) {
        return r.and_then(|u| Url::parse(&u).map_err(|e| e.to_string()));
    }
    as_url.ok_or_else(|| {
        format!(
            "Failed to resolve module specifier \"{specifier}\". Relative references must start with either \"/\", \"./\", or \"../\"."
        )
    })
}

/// A JavaScript or JSON MIME type check for module responses.
fn mime_ok(content_type: &str, json: bool) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if json {
        essence == "application/json" || essence == "text/json" || essence.ends_with("+json")
    } else {
        is_js_mime(&essence)
    }
}

/// The `type` import attribute of a module request (`None`: JavaScript).
fn attr_type(
    scope: &v8::PinScope<'_, '_>,
    attrs: v8::Local<'_, v8::FixedArray>,
    stride: usize,
) -> Result<Option<String>, String> {
    let n = attrs.length();
    let mut i = 0;
    let mut ty = None;
    while i + 1 < n {
        let k = attrs
            .get(scope, i)
            .and_then(|d| v8::Local::<v8::Value>::try_from(d).ok())
            .map(|v| v.to_rust_string_lossy(scope));
        let v = attrs
            .get(scope, i + 1)
            .and_then(|d| v8::Local::<v8::Value>::try_from(d).ok())
            .map(|v| v.to_rust_string_lossy(scope));
        if k.as_deref() == Some("type") {
            ty = v;
        }
        i += stride;
    }
    match ty.as_deref() {
        None => Ok(None),
        Some("json") => Ok(Some("json".into())),
        Some(t) => Err(format!(
            "Import attribute type \"{t}\" is not supported (pageon supports \"json\")"
        )),
    }
}

/// The base URL of the script or module that is running (`resource` is its name).
fn base_of(resource: &str, document: &str) -> Url {
    Url::parse(resource)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https" | "file" | "data" | "blob"))
        .or_else(|| Url::parse(document).ok())
        .unwrap_or_else(|| Url::parse("about:blank").unwrap_or_else(|_| unreachable!()))
}

fn record_of<'a>(
    records: &'a [ModuleRecord],
    scope: &v8::PinScope<'_, '_>,
    m: v8::Local<'_, v8::Module>,
) -> Option<&'a ModuleRecord> {
    let hash = m.get_identity_hash().get();
    records
        .iter()
        .find(|r| r.hash == hash && v8::Local::new(scope, &r.module) == m)
}

fn throw_type(scope: &mut v8::PinScope<'_, '_>, msg: &str) {
    if let Some(m) = v8::String::new(scope, msg) {
        let e = v8::Exception::type_error(scope, m);
        scope.throw_exception(e);
    }
}

/// V8's resolve hook: the requested module is already in the realm's module map.
pub(crate) fn resolve_hook<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    attrs: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    v8::callback_scope!(unsafe scope, context);
    let realm = crate::realm::current(scope)?;
    let spec = specifier.to_rust_string_lossy(scope);
    let json = match attr_type(scope, attrs, 3) {
        Ok(t) => t.is_some(),
        Err(e) => {
            throw_type(scope, &e);
            return None;
        }
    };
    let st = realm.st.borrow();
    let base = record_of(&st.modules, scope, referrer)
        .map(|r| base_of(&r.url, &st.url))
        .unwrap_or_else(|| base_of("", &st.url));
    let url = match resolve(&st.import_map, &spec, &base) {
        Ok(u) => u.to_string(),
        Err(e) => {
            drop(st);
            throw_type(scope, &e);
            return None;
        }
    };
    let found = st
        .modules
        .iter()
        .find(|r| !r.inline && r.url == url && r.json.is_some() == json)
        .map(|r| v8::Local::new(scope, &r.module));
    drop(st);
    if found.is_none() {
        throw_type(scope, &format!("Failed to load module script {url}"));
    }
    found
}

/// The `default` export of a JSON module.
fn json_steps<'s>(
    context: v8::Local<'s, v8::Context>,
    module: v8::Local<'_, v8::Module>,
) -> Option<v8::Local<'s, v8::Value>> {
    v8::callback_scope!(unsafe scope, context);
    let realm = crate::realm::current(scope)?;
    let value = {
        let st = realm.st.borrow();
        let r = record_of(&st.modules, scope, module)?;
        v8::Local::new(scope, r.json.as_ref()?)
    };
    let name = v8::String::new(scope, "default")?;
    module.set_synthetic_module_export(scope, name, value)?;
    let resolver = v8::PromiseResolver::new(scope)?;
    let undefined = v8::undefined(scope);
    resolver.resolve(scope, undefined.into());
    Some(resolver.get_promise(scope).into())
}

/// V8's `import.meta` hook: `url` and `resolve()`.
pub(crate) extern "C" fn import_meta_hook(
    context: v8::Local<v8::Context>,
    module: v8::Local<v8::Module>,
    meta: v8::Local<v8::Object>,
) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        v8::callback_scope!(unsafe scope, context);
        v8::scope!(let scope, scope);
        let Some(realm) = crate::realm::current(scope) else {
            return;
        };
        let url = {
            let st = realm.st.borrow();
            record_of(&st.modules, scope, module).map_or_else(|| st.url.clone(), |r| r.url.clone())
        };
        let (Some(k), Some(v)) = (v8::String::new(scope, "url"), v8::String::new(scope, &url))
        else {
            return;
        };
        meta.create_data_property(scope, k.into(), v.into());
        let Some(resolve_fn) = v8::Function::builder(meta_resolve)
            .data(v.into())
            .build(scope)
        else {
            return;
        };
        if let Some(k) = v8::String::new(scope, "resolve") {
            meta.create_data_property(scope, k.into(), resolve_fn.into());
        }
    }));
}

fn meta_resolve(
    scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let Some(realm) = crate::realm::current(scope) else {
        return;
    };
    let module_url = args.data().to_rust_string_lossy(scope);
    let spec = args.get(0).to_rust_string_lossy(scope);
    let r = {
        let st = realm.st.borrow();
        resolve(&st.import_map, &spec, &base_of(&module_url, &st.url))
    };
    match r {
        Ok(u) => {
            if let Some(s) = v8::String::new(scope, u.as_str()) {
                rv.set(s.into());
            }
        }
        Err(e) => throw_type(scope, &e),
    }
}

/// V8's dynamic `import()` hook: queues an import job and returns its promise.
pub(crate) fn dynamic_import_hook<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    _options: v8::Local<'s, v8::Data>,
    resource: v8::Local<'s, v8::Value>,
    specifier: v8::Local<'s, v8::String>,
    attrs: v8::Local<'s, v8::FixedArray>,
) -> Option<v8::Local<'s, v8::Promise>> {
    let realm = crate::realm::current(scope)?;
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    let spec = specifier.to_rust_string_lossy(scope);
    let resource = resource.to_rust_string_lossy(scope);
    let resolved = match attr_type(scope, attrs, 2) {
        Err(e) => Err(e),
        Ok(t) => {
            let st = realm.st.borrow();
            let base = base_of(&resource, &st.url);
            resolve(&st.import_map, &spec, &base).map(|u| (u.to_string(), t.is_some()))
        }
    };
    match resolved {
        Ok((url, json)) => {
            let resolver = v8::Global::new(scope, resolver);
            realm
                .shared
                .borrow_mut()
                .scripts
                .push_back(ScriptJob::Import {
                    realm: realm.id,
                    url,
                    json,
                    resolver,
                });
        }
        Err(e) => {
            let m = v8::String::new(scope, &e)?;
            let ex = v8::Exception::type_error(scope, m);
            resolver.reject(scope, ex);
        }
    }
    Some(promise)
}

/// Marks scripts that `nodes` connect to the document as started and queues them (HTML's
/// "prepare the script element" for scripts inserted by DOM methods).
pub(crate) fn inserted(cx: &Cx<'_, '_, '_>, nodes: &[NodeId]) {
    let dom = cx.dom();
    let mut found = Vec::new();
    for &n in nodes {
        // Connected, shadow-including: scripts in shadow trees run too.
        if !dom.is_connected(n) {
            continue;
        }
        for e in dom.traverse(n) {
            if let pageon_dom::Edge::Enter(x) = e
                && let Some(el) = dom.get(x)
                && el.is_html("script")
            {
                let has_text = dom.children(x).any(|c| {
                    matches!(dom.get(c).map(|c| &c.data), Some(pageon_dom::NodeData::Text(t)) if !t.is_empty())
                });
                if el.attr("src").is_some() || has_text {
                    found.push(x);
                }
            }
        }
    }
    drop(dom);
    if found.is_empty() {
        return;
    }
    let mut st = cx.st();
    let fresh: Vec<NodeId> = found
        .into_iter()
        .filter(|n| st.started_scripts.insert(*n))
        .collect();
    drop(st);
    let mut sh = cx.realm.shared.borrow_mut();
    for node in fresh {
        sh.scripts.push_back(ScriptJob::Element {
            realm: cx.realm.id,
            node,
        });
    }
}

/// A fetched script body.
struct Body {
    text: String,
    content_type: String,
    url: Url,
}

impl Engine {
    /// Fetches a script through the realm's network state and `env`.
    fn fetch_script_body(
        &mut self,
        realm: usize,
        url: &Url,
        module: bool,
        env: &dyn pageon_env::Env,
    ) -> Result<Body, String> {
        let r = self.realm(realm).cloned().ok_or("the document is gone")?;
        if module {
            let first_over = {
                let mut st = r.st.borrow_mut();
                st.modules_loaded = st.modules_loaded.saturating_add(1);
                st.modules_loaded > MAX_MODULES && st.refused.insert("modules")
            };
            if first_over {
                r.shared.borrow_mut().error(
                    PageErrorKind::Load,
                    format!("this document has loaded {MAX_MODULES} modules; no more are loaded"),
                    url.as_str(),
                );
            }
            if r.st.borrow().modules_loaded > MAX_MODULES {
                return Err(format!(
                    "this document has loaded {MAX_MODULES} modules; no more are loaded"
                ));
            }
        }
        let (net, client) = {
            let st = r.st.borrow();
            (st.net.clone(), st.client.clone())
        };
        let mut req = FetchRequest::subresource(url.clone(), Dest::Script, client);
        if module {
            req.mode = Mode::Cors;
            req.credentials = Credentials::SameOrigin;
        }
        let now = self.shared.borrow().now as u64;
        let res = net.borrow_mut().fetch(env, req, now);
        match res {
            Ok(f) if (200..300).contains(&f.status) => {
                let content_type = f
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                let charset = pageon_dom::encoding::charset_param(&content_type).unwrap_or("utf-8");
                let text = pageon_dom::encoding::decode(&f.body, Some(charset)).0;
                Ok(Body {
                    text,
                    content_type,
                    url: f.url,
                })
            }
            Ok(f) => Err(format!("HTTP {}", f.status)),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Registers an import map's text (parsed or inserted `<script type=importmap>`).
    pub fn register_import_map(&mut self, realm: usize, text: &str) {
        let Some(r) = self.realm(realm).cloned() else {
            return;
        };
        let url = r.st.borrow().url.clone();
        let base = Url::parse(&url).unwrap_or_else(|_| base_of("", "about:blank"));
        let mut warnings = Vec::new();
        let map = ImportMap::parse(text, &base, &mut warnings);
        for w in warnings {
            self.report(PageErrorKind::Exception, w, url.clone());
        }
        if let Some(m) = map {
            r.st.borrow_mut().import_map.merge(m);
        }
    }

    /// Compiles a module (or builds a JSON module) and adds it to the realm's records;
    /// returns its requests (specifier, JSON) for the graph walk.
    fn compile_module(
        &mut self,
        realm: usize,
        url: &str,
        text: &str,
        json: bool,
        inline: bool,
    ) -> Result<Requests, (PageErrorKind, String)> {
        let out = self.with_cx(realm, |cx| {
            let scope = &mut *cx.scope;
            v8::tc_scope!(let tc, scope);
            let name = v8::String::new(tc, url).ok_or((PageErrorKind::Load, "bad URL".into()))?;
            let (module, json_value) = if json {
                let src = v8::String::new(tc, text)
                    .ok_or((PageErrorKind::Load, "the module is too large".into()))?;
                let Some(v) = v8::json::parse(tc, src) else {
                    let msg = tc
                        .exception()
                        .map(|e| e.to_rust_string_lossy(tc))
                        .unwrap_or_else(|| "invalid JSON".into());
                    tc.reset();
                    return Err((PageErrorKind::Exception, format!("{msg} (JSON module)")));
                };
                let export = v8::String::new(tc, "default")
                    .ok_or((PageErrorKind::Load, "no string".into()))?;
                let m = v8::Module::create_synthetic_module(tc, name, &[export], json_steps);
                (m, Some(v8::Global::new(tc, v)))
            } else {
                let src = v8::String::new(tc, text)
                    .ok_or((PageErrorKind::Load, "the module is too large".into()))?;
                let origin = v8::ScriptOrigin::new(
                    tc,
                    name.into(),
                    0,
                    0,
                    false,
                    0,
                    None,
                    false,
                    false,
                    true,
                    None,
                );
                let mut source = v8::script_compiler::Source::new(src, Some(&origin));
                match v8::script_compiler::compile_module(tc, &mut source) {
                    Some(m) => (m, None),
                    None => {
                        let err = tc.exception().map(|ex| {
                            let msg = tc.message();
                            crate::engine::page_error(tc, ex, msg)
                        });
                        tc.reset();
                        let e = err.map_or_else(
                            || "SyntaxError".to_string(),
                            |e| format!("{} ({}:{})", e.message, e.line, e.column),
                        );
                        return Err((PageErrorKind::Exception, e));
                    }
                }
            };
            let mut requests = Vec::new();
            if json_value.is_none() {
                let reqs = module.get_module_requests();
                for i in 0..reqs.length() {
                    let Some(r) = reqs
                        .get(tc, i)
                        .and_then(|d| v8::Local::<v8::ModuleRequest>::try_from(d).ok())
                    else {
                        continue;
                    };
                    let spec = r.get_specifier().to_rust_string_lossy(tc);
                    let ty = attr_type(tc, r.get_import_attributes(), 3).map(|t| t.is_some());
                    requests.push((spec, ty));
                }
            }
            let hash = module.get_identity_hash().get();
            cx_records(&cx.realm).push(ModuleRecord {
                url: url.to_string(),
                inline,
                json: json_value,
                hash,
                module: v8::Global::new(tc, module),
            });
            Ok(requests)
        });
        out.unwrap_or_else(|| Err((PageErrorKind::Load, "the document is gone".into())))
    }

    /// Fetches and compiles every module the graph from `root` needs, depth first.
    fn load_graph(
        &mut self,
        realm: usize,
        root: &str,
        requests: Vec<(String, Result<bool, String>)>,
        env: &dyn pageon_env::Env,
    ) -> Result<(), (PageErrorKind, String, String)> {
        let mut work: Vec<(String, String, Result<bool, String>)> = requests
            .into_iter()
            .map(|(s, t)| (root.to_string(), s, t))
            .collect();
        let mut fetched = 0;
        while let Some((referrer, spec, ty)) = work.pop() {
            let json = ty.map_err(|e| (PageErrorKind::Exception, e, referrer.clone()))?;
            let Some(r) = self.realm(realm).cloned() else {
                return Ok(());
            };
            let url = {
                let st = r.st.borrow();
                let base = base_of(&referrer, &st.url);
                resolve(&st.import_map, &spec, &base)
                    .map_err(|e| (PageErrorKind::Exception, e, referrer.clone()))?
            };
            let key = url.to_string();
            let known =
                r.st.borrow()
                    .modules
                    .iter()
                    .any(|m| !m.inline && m.url == key && m.json.is_some() == json);
            if known {
                continue;
            }
            fetched += 1;
            if fetched > MAX_MODULES {
                return Err((
                    PageErrorKind::Load,
                    format!("the module graph has more than {MAX_MODULES} modules"),
                    root.to_string(),
                ));
            }
            let body = self
                .fetch_script_body(realm, &url, true, env)
                .map_err(|e| {
                    (
                        PageErrorKind::Load,
                        format!("Failed to load module script: {e}"),
                        key.clone(),
                    )
                })?;
            if !mime_ok(&body.content_type, json) {
                let what = if json {
                    "Expected a JSON module script"
                } else {
                    "Expected a JavaScript-or-Wasm module script"
                };
                return Err((
                    PageErrorKind::Load,
                    format!(
                        "Failed to load module script: {what} but the server responded with a MIME type of \"{}\". Strict MIME type checking is enforced for module scripts per HTML spec.",
                        body.content_type
                    ),
                    key,
                ));
            }
            let _ = body.url;
            let reqs = self
                .compile_module(realm, &key, &body.text, json, false)
                .map_err(|(k, e)| (k, e, key.clone()))?;
            work.extend(reqs.into_iter().map(|(s, t)| (key.clone(), s, t)));
        }
        Ok(())
    }

    /// Instantiates and evaluates a loaded module; `Ok(Some(promise))` while top-level await
    /// is pending, the namespace when asked.
    fn evaluate_module(
        &mut self,
        realm: usize,
        url: &str,
        inline: bool,
        json: bool,
    ) -> Result<(), PageErrorKind> {
        self.scripts += 1;
        let (r, fired) = self.guarded(|e| {
            e.with_cx(realm, |cx| {
                let module = {
                    let st = cx.st();
                    st.modules
                        .iter()
                        .rev()
                        .find(|m| m.url == url && m.inline == inline && m.json.is_some() == json)
                        .map(|m| m.module.clone())
                };
                let Some(module) = module else {
                    return Err(PageErrorKind::Load);
                };
                let scope = &mut *cx.scope;
                v8::tc_scope!(let tc, scope);
                let m = v8::Local::new(tc, &module);
                let evaluated = if m.instantiate_module(tc, resolve_hook).is_some() {
                    m.evaluate(tc)
                } else {
                    None
                };
                let ok = evaluated.is_some();
                let mut err = None;
                if !ok && !tc.is_execution_terminating() {
                    if let Some(ex) = tc.exception() {
                        let msg = tc.message();
                        err = Some(crate::engine::page_error(tc, ex, msg));
                    }
                } else if m.get_status() == v8::ModuleStatus::Errored {
                    let ex = m.get_exception();
                    let msg = v8::Exception::create_message(tc, ex);
                    err = Some(crate::engine::page_error(tc, ex, Some(msg)));
                    // Reported here, with the module's URL: not again as an unhandled
                    // rejection of the evaluation promise.
                    if let Some(p) =
                        evaluated.and_then(|v| v8::Local::<v8::Promise>::try_from(v).ok())
                        && let Some(noop) = v8::Function::new(tc, return_data)
                    {
                        let _ = p.catch(tc, noop);
                    }
                }
                match err {
                    Some(mut e) => {
                        if e.source.is_empty() {
                            e.source = url.to_string();
                        }
                        cx.realm.shared.borrow_mut().errors.push(e);
                        Err(PageErrorKind::Exception)
                    }
                    None => Ok(()),
                }
            })
        });
        crate::engine::report_rejections(&self.shared);
        self.terminated(fired, url);
        r.unwrap_or(Err(PageErrorKind::Load))
    }

    /// Runs a module script: `url` for `src`, or `inline` text (with the document URL).
    /// Fires `error` on the element when the graph cannot load; `load` after an external one
    /// ran. Returns whether it ran.
    pub fn run_module(
        &mut self,
        realm: usize,
        url: &Url,
        inline: Option<&str>,
        node: Option<NodeId>,
        env: &dyn pageon_env::Env,
    ) -> bool {
        if self.exhausted() {
            return false;
        }
        let key = url.to_string();
        let loaded: Result<(), (PageErrorKind, String, String)> = (|| {
            let requests = match inline {
                Some(text) => self
                    .compile_module(realm, &key, text, false, true)
                    .map_err(|(k, e)| (k, e, key.clone()))?,
                None => {
                    let known = self.realm(realm).is_some_and(|r| {
                        r.st.borrow()
                            .modules
                            .iter()
                            .any(|m| !m.inline && m.url == key && m.json.is_none())
                    });
                    if known {
                        Vec::new()
                    } else {
                        let body = self.fetch_script_body(realm, url, true, env).map_err(|e| {
                            (
                                PageErrorKind::Load,
                                format!("Failed to load module script: {e}"),
                                key.clone(),
                            )
                        })?;
                        if !mime_ok(&body.content_type, false) {
                            return Err((
                                PageErrorKind::Load,
                                format!(
                                    "Failed to load module script: Expected a JavaScript-or-Wasm module script but the server responded with a MIME type of \"{}\". Strict MIME type checking is enforced for module scripts per HTML spec.",
                                    body.content_type
                                ),
                                key.clone(),
                            ));
                        }
                        self.compile_module(realm, &key, &body.text, false, false)
                            .map_err(|(k, e)| (k, e, key.clone()))?
                    }
                }
            };
            self.load_graph(realm, &key, requests, env)
        })();
        if let Err((kind, message, source)) = loaded {
            self.report(kind, message, source);
            if let Some(n) = node {
                self.fire(realm, Target::Node(n), "error", false);
            }
            return false;
        }
        let ran = self
            .evaluate_module(realm, &key, inline.is_some(), false)
            .is_ok();
        if inline.is_none()
            && let Some(n) = node
        {
            self.fire(realm, Target::Node(n), "load", false);
        }
        ran
    }

    /// Runs the page's script queue (inserted scripts, dynamic imports) until it is empty;
    /// returns how many jobs ran.
    pub fn run_scripts(&mut self, env: &dyn pageon_env::Env) -> usize {
        let mut n = 0;
        while n < 10_000 && !self.exhausted() {
            let job = self.shared.borrow_mut().scripts.pop_front();
            let Some(job) = job else { break };
            n += 1;
            match job {
                ScriptJob::Element { realm, node } => self.run_inserted(realm, node, env),
                ScriptJob::Import {
                    realm,
                    url,
                    json,
                    resolver,
                } => self.run_import(realm, &url, json, resolver, env),
            }
        }
        n
    }

    fn run_inserted(&mut self, realm: usize, node: NodeId, env: &dyn pageon_env::Env) {
        let Some(r) = self.realm(realm).cloned() else {
            return;
        };
        let (kind, src, text, nomodule, base) = {
            let dom = r.dom.borrow();
            let Some(el) = dom.get(node) else { return };
            let text: String = dom
                .children(node)
                .filter_map(|c| match &dom.get(c)?.data {
                    pageon_dom::NodeData::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect();
            (
                script_kind(el),
                el.attr("src").map(str::to_string),
                text,
                el.attr("nomodule").is_some(),
                r.st.borrow().url.clone(),
            )
        };
        let base = Url::parse(&base).unwrap_or_else(|_| base_of("", "about:blank"));
        let src_url = match src.as_deref().map(|s| base.join(s.trim())) {
            Some(Ok(u)) => Some(u),
            Some(Err(_)) => {
                self.report(
                    PageErrorKind::Load,
                    "invalid script URL",
                    src.unwrap_or_default(),
                );
                self.fire(realm, Target::Node(node), "error", false);
                return;
            }
            None => None,
        };
        match kind {
            ScriptKind::Data => {}
            ScriptKind::Classic if nomodule => {}
            ScriptKind::Classic => match src_url {
                Some(url) => match self.fetch_script_body(realm, &url, false, env) {
                    Ok(b) => {
                        self.run_script(realm, &b.text, url.as_str(), Some(node));
                        self.fire(realm, Target::Node(node), "load", false);
                    }
                    Err(e) => {
                        self.report(
                            PageErrorKind::Load,
                            format!("script failed to load: {e}"),
                            url.as_str(),
                        );
                        self.fire(realm, Target::Node(node), "error", false);
                    }
                },
                None => {
                    self.run_script(realm, &text, "<inserted inline script>", Some(node));
                }
            },
            ScriptKind::Module => {
                match src_url {
                    Some(url) => self.run_module(realm, &url, None, Some(node), env),
                    None => self.run_module(realm, &base, Some(&text), Some(node), env),
                };
            }
            ScriptKind::ImportMap => match src_url {
                Some(url) => {
                    self.report(
                        PageErrorKind::Unsupported,
                        "External import maps are not supported",
                        url.as_str(),
                    );
                    self.fire(realm, Target::Node(node), "error", false);
                }
                None => self.register_import_map(realm, &text),
            },
        }
    }

    fn run_import(
        &mut self,
        realm: usize,
        url: &str,
        json: bool,
        resolver: v8::Global<v8::PromiseResolver>,
        env: &dyn pageon_env::Env,
    ) {
        let Ok(u) = Url::parse(url) else { return };
        let loaded: Result<(), String> = (|| {
            let known = self.realm(realm).is_some_and(|r| {
                r.st.borrow()
                    .modules
                    .iter()
                    .any(|m| !m.inline && m.url == url && m.json.is_some() == json)
            });
            if known {
                return Ok(());
            }
            let body = self
                .fetch_script_body(realm, &u, true, env)
                .map_err(|e| e.to_string())?;
            if !mime_ok(&body.content_type, json) {
                return Err(format!(
                    "the server responded with a MIME type of \"{}\"",
                    body.content_type
                ));
            }
            let reqs = self
                .compile_module(realm, url, &body.text, json, false)
                .map_err(|(_, e)| e)?;
            self.load_graph(realm, url, reqs, env)
                .map_err(|(_, e, _)| e)
        })();
        if let Err(e) = &loaded {
            self.report(
                PageErrorKind::Load,
                format!("Failed to fetch dynamically imported module: {url} ({e})"),
                url,
            );
        }
        let (_, fired) = self.guarded(|eng| {
            eng.with_cx(realm, |cx| {
                let scope = &mut *cx.scope;
                v8::tc_scope!(let tc, scope);
                let resolver = v8::Local::new(tc, &resolver);
                if loaded.is_err() {
                    if let Some(m) = v8::String::new(
                        tc,
                        &format!("Failed to fetch dynamically imported module: {url}"),
                    ) {
                        let ex = v8::Exception::type_error(tc, m);
                        resolver.reject(tc, ex);
                    }
                    return;
                }
                let module = {
                    let st = cx.realm.st.borrow();
                    st.modules
                        .iter()
                        .find(|m| !m.inline && m.url == url && m.json.is_some() == json)
                        .map(|m| m.module.clone())
                };
                let Some(module) = module else { return };
                let m = v8::Local::new(tc, &module);
                let evaluated = if m.instantiate_module(tc, resolve_hook).is_some() {
                    m.evaluate(tc)
                } else {
                    None
                };
                let Some(promise) = evaluated else {
                    if let Some(ex) = tc.exception() {
                        tc.reset();
                        resolver.reject(tc, ex);
                    }
                    return;
                };
                let ns = m.get_module_namespace();
                match v8::Local::<v8::Promise>::try_from(promise) {
                    Ok(p) if p.state() == v8::PromiseState::Pending => {
                        // Top-level await: resolve with the namespace once evaluation settles.
                        let Some(then) = v8::Function::builder(return_data).data(ns).build(tc)
                        else {
                            return;
                        };
                        if let Some(chained) = p.then(tc, then) {
                            resolver.resolve(tc, chained.into());
                        }
                    }
                    Ok(p) if p.state() == v8::PromiseState::Rejected => {
                        let reason = p.result(tc);
                        resolver.reject(tc, reason);
                    }
                    _ => {
                        resolver.resolve(tc, ns);
                    }
                }
            });
        });
        self.terminated(fired, url);
    }
}

/// Returns the function's data (the namespace, after top-level await).
fn return_data(
    _scope: &mut v8::PinScope,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    rv.set(args.data());
}

fn cx_records(realm: &crate::realm::Realm) -> std::cell::RefMut<'_, Vec<ModuleRecord>> {
    std::cell::RefMut::map(realm.st.borrow_mut(), |st| &mut st.modules)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(json: &str) -> ImportMap {
        let base = Url::parse("https://app.test/index.html").unwrap();
        let mut w = Vec::new();
        ImportMap::parse(json, &base, &mut w).unwrap()
    }

    #[test]
    fn resolution_follows_html() {
        let base = Url::parse("https://app.test/js/main.js").unwrap();
        let m = map(
            r#"{"imports": {"lib": "/vendor/lib.js", "lib/": "/vendor/lib/", "@x/": "https://cdn.test/x/"},
                "scopes": {"/js/": {"lib": "/js/lib-scoped.js"}}}"#,
        );
        let r = |s: &str, b: &Url| resolve(&m, s, b).map(|u| u.to_string());
        assert_eq!(r("./a.js", &base).unwrap(), "https://app.test/js/a.js");
        assert_eq!(r("/b.js", &base).unwrap(), "https://app.test/b.js");
        assert_eq!(
            r("lib", &base).unwrap(),
            "https://app.test/js/lib-scoped.js"
        );
        let other = Url::parse("https://app.test/other.js").unwrap();
        assert_eq!(r("lib", &other).unwrap(), "https://app.test/vendor/lib.js");
        assert_eq!(
            r("lib/util.js", &other).unwrap(),
            "https://app.test/vendor/lib/util.js"
        );
        assert_eq!(r("@x/y.js", &other).unwrap(), "https://cdn.test/x/y.js");
        let e = r("nope", &other).unwrap_err();
        assert!(
            e.starts_with("Failed to resolve module specifier \"nope\""),
            "{e}"
        );
    }

    #[test]
    fn kinds_and_mime() {
        assert!(mime_ok("text/javascript; charset=utf-8", false));
        assert!(!mime_ok("text/html", false));
        assert!(mime_ok("application/json", true));
        assert!(!mime_ok("text/javascript", true));
    }
}
