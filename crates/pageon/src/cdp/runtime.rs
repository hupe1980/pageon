//! `Runtime` and `DOM`: execution contexts, remote objects, evaluation, node handles.

use pageon_dom::{NodeData, NodeId};
use pageon_env::Env;
use pageon_js::{Arg, Handle, PromiseState};
use serde_json::{Value as Json, json};

use super::{AWAIT_BUDGET_MS, CdpError, Ctx, Pending, Reply, Ser, Session};

/// What a call produced, computed before the page settles (a navigation may follow).
enum Done {
    Value(Handle, Result<Json, String>),
    Thrown(pageon_js::Thrown, Option<Json>),
    Waiting(Handle),
}

impl<E: Env> Session<'_, E> {
    /// Fresh contexts for a target's current documents: for each frame with a document, its
    /// main world, then each isolated world.
    pub(super) fn new_contexts(&mut self, ti: usize) {
        let worlds = self.targets[ti].worlds.clone();
        let loaded: Vec<usize> = (0..self.targets[ti].frames.len())
            .filter(|&f| self.targets[ti].frames[f].loaded)
            .collect();
        let mut contexts = Vec::new();
        for &frame in &loaded {
            for name in std::iter::once(String::new()).chain(worlds.iter().cloned()) {
                self.next_context += 1;
                let id = self.next_context;
                contexts.push(Ctx {
                    id,
                    unique: format!("{}.{id}", self.targets[ti].epoch),
                    name,
                    frame,
                });
            }
        }
        self.targets[ti].contexts = contexts;
        // World init scripts run in each new document's world (main-world ones ran before the
        // document's own scripts, in the loader).
        let scripts: Vec<crate::load::InitScript> = self.targets[ti]
            .tab
            .init_scripts_mut()
            .iter()
            .filter(|s| s.world.is_some())
            .cloned()
            .collect();
        for frame in &loaded {
            for script in &scripts {
                self.run_init_in(ti, *frame, script);
            }
        }
        // Bindings live on each document's global: install them again.
        let names = self.targets[ti].bindings.clone();
        if !names.is_empty() {
            for frame in loaded {
                if let Ok(live) = self.targets[ti].tab.live_in(frame) {
                    let mut e = live.engine.borrow_mut();
                    for n in &names {
                        e.add_binding(live.realm, n);
                    }
                }
            }
        }
    }

    /// Runs an init script now in the target's current documents (`runImmediately`), in its
    /// world or the main world.
    pub(super) fn run_init_now(&mut self, ti: usize, script: &crate::load::InitScript) {
        let frames: Vec<usize> = (0..self.targets[ti].frames.len())
            .filter(|&f| self.targets[ti].frames[f].loaded)
            .collect();
        for frame in frames {
            self.run_init_in(ti, frame, script);
        }
    }

    /// Runs an init script in one frame's document (its world, created if needed).
    fn run_init_in(&mut self, ti: usize, frame: usize, script: &crate::load::InitScript) {
        let Ok(live) = self.targets[ti].tab.live_in(frame) else {
            return;
        };
        let mut e = live.engine.borrow_mut();
        let name = format!("pageon-init-script-{}", script.id);
        match &script.world {
            // A world's script: evaluated in it (its errors stay the client's, as for
            // `Runtime.evaluate` there).
            Some(w) => {
                if let Ok(r) = e.world(live.realm, w) {
                    let _ = e.evaluate(r, &format!("{}\n//# sourceURL={name}", script.source));
                }
            }
            None => e.run_script(live.realm, &script.source, &name, None),
        }
    }

    /// The live document of a frame, as a CDP error when it has none.
    fn live(&mut self, ti: usize, frame: usize) -> Result<std::rc::Rc<crate::LiveDoc>, CdpError> {
        self.targets[ti]
            .tab
            .live_in(frame)
            .map_err(|e| CdpError::server(e.to_string()))
    }

    /// A context's frame, live document and engine context (its isolated world, created on
    /// first use, or the document's main world).
    pub(super) fn live_ctx(
        &mut self,
        ti: usize,
        ctx: i64,
    ) -> Result<(usize, std::rc::Rc<crate::LiveDoc>, usize), CdpError> {
        let frame = self.frame_of(ti, ctx)?;
        let name = self.ctx_name(ti, ctx);
        let live = self.live(ti, frame)?;
        let index = if name.is_empty() {
            live.realm
        } else {
            live.engine
                .borrow_mut()
                .world(live.realm, &name)
                .map_err(CdpError::server)?
        };
        Ok((frame, live, index))
    }

    /// A context's world name (empty for the main world).
    pub(super) fn ctx_name(&self, ti: usize, ctx: i64) -> String {
        self.targets[ti]
            .contexts
            .iter()
            .find(|c| c.id == ctx)
            .map(|c| c.name.clone())
            .unwrap_or_default()
    }

    /// The frame of a context (an error if it is gone).
    fn frame_of(&self, ti: usize, ctx: i64) -> Result<usize, CdpError> {
        self.ctx_frame(ti, ctx)
            .ok_or_else(|| CdpError::server("Cannot find context with specified id"))
    }

    pub(super) fn context_json(&self, ti: usize, c: &Ctx) -> Json {
        let t = &self.targets[ti];
        let frame = &t.frames[c.frame];
        json!({
            "id": c.id,
            "origin": crate::frames::origin_of(&frame.url),
            "name": c.name,
            "uniqueId": c.unique,
            "auxData": {
                "isDefault": c.name.is_empty(),
                "type": if c.name.is_empty() { "default" } else { "isolated" },
                "frameId": frame.id,
            },
        })
    }

    pub(super) fn announce_contexts(&mut self, ti: usize) {
        if !self.targets[ti].runtime {
            return;
        }
        let contexts = self.targets[ti].contexts.clone();
        for c in &contexts {
            let ctx = self.context_json(ti, c);
            self.target_event(
                ti,
                "Runtime.executionContextCreated",
                json!({"context": ctx}),
            );
        }
    }

    fn context(&self, ti: usize, params: &Json) -> Result<i64, CdpError> {
        let t = &self.targets[ti];
        let main = || t.contexts.first().map(|c| c.id);
        let id = if let Some(id) = params.get("executionContextId").and_then(Json::as_i64) {
            Some(id)
        } else if let Some(id) = params.get("contextId").and_then(Json::as_i64) {
            Some(id)
        } else if let Some(u) = params.get("uniqueContextId").and_then(Json::as_str) {
            t.contexts.iter().find(|c| c.unique == u).map(|c| c.id)
        } else {
            main()
        };
        id.filter(|id| t.contexts.iter().any(|c| c.id == *id))
            .ok_or_else(|| CdpError::server("Cannot find context with specified id"))
    }

    fn object(&self, ti: usize, object_id: &str) -> Result<(i64, Handle), CdpError> {
        self.targets[ti]
            .objects
            .get(object_id)
            .copied()
            .ok_or_else(|| CdpError::server("Could not find object with given id"))
    }

    /// A CDP `RemoteObject`, registering an `objectId` for objects held by reference.
    fn remote(
        &mut self,
        ti: usize,
        ctx: i64,
        h: Handle,
        by_value: bool,
        group: Option<&str>,
    ) -> Result<Json, CdpError> {
        let (_, live, at) = self.live_ctx(ti, ctx)?;
        let described = live.engine.borrow_mut().describe(at, h, by_value);
        let r = described.map_err(CdpError::server)?;
        Ok(self.register(ti, ctx, h, r, group))
    }

    fn register(
        &mut self,
        ti: usize,
        ctx: i64,
        h: Handle,
        mut r: Json,
        group: Option<&str>,
    ) -> Json {
        let by_ref = matches!(r["type"].as_str(), Some("object" | "function" | "symbol"))
            && r.get("value").is_none()
            && r["subtype"] != "null";
        let frame = self.ctx_frame(ti, ctx).unwrap_or(0);
        let t = &mut self.targets[ti];
        if by_ref {
            t.next_object += 1;
            let id = format!("{}.{ctx}.{}", t.epoch, t.next_object);
            t.objects.insert(id.clone(), (ctx, h));
            if let Some(g) = group {
                t.groups.entry(g.to_string()).or_default().push(id.clone());
            }
            r["objectId"] = json!(id);
        } else if let Ok(live) = t.tab.live_in(frame) {
            live.engine.borrow_mut().release(h);
        }
        r
    }

    pub(super) fn value_reply(
        &mut self,
        ti: usize,
        ctx: i64,
        h: Handle,
        ser: Ser,
        group: Option<&str>,
    ) -> Reply {
        let deep = match ser {
            Ser::Deep(d) => Some(self.deep_value(ti, ctx, h, d)?),
            Ser::Cdp { .. } => None,
        };
        let mut r = self.remote(ti, ctx, h, ser.by_value(), group)?;
        if let Some(d) = deep {
            r["deepSerializedValue"] = d;
        }
        Ok(json!({"result": r}))
    }

    /// Chrome's `deepSerializedValue` of a held value: the engine's serialization with its node
    /// and window keys turned into this session's ids (`backendNodeId`, `loaderId`, frame ids).
    pub(super) fn deep_value(
        &mut self,
        ti: usize,
        ctx: i64,
        h: Handle,
        depth: pageon_js::Depth,
    ) -> Result<Json, CdpError> {
        let (frame, live, at) = self.live_ctx(ti, ctx)?;
        let mut v = live
            .engine
            .borrow_mut()
            .deep(at, h, depth)
            .map_err(CdpError::server)?;
        self.deep_ids(ti, frame, &mut v);
        Ok(v)
    }

    /// The frame whose document runs in engine realm `realm` of the caller's engine (the
    /// caller's frame if none). Realm numbers are per engine (cross-origin frames have their
    /// own), and an isolated world's index names its document's realm.
    fn realm_frame(&self, ti: usize, realm: usize, fallback: usize) -> usize {
        let tab = &self.targets[ti].tab;
        let Some(own) = tab.live_if_started(fallback) else {
            return fallback;
        };
        let realm = own.engine.borrow().realm(realm).map_or(realm, |r| r.id);
        (0..self.targets[ti].frames.len())
            .find(|&fi| {
                tab.live_if_started(fi).is_some_and(|l| {
                    std::rc::Rc::ptr_eq(&l.engine, &own.engine) && l.realm == realm
                })
            })
            .unwrap_or(fallback)
    }

    fn deep_ids(&mut self, ti: usize, frame: usize, v: &mut Json) {
        match v {
            Json::Object(m) => {
                if let Some(Json::Array(k)) = m.remove("node") {
                    let num = |i: usize| k.get(i).and_then(Json::as_u64).unwrap_or(0) as u32;
                    let fi = self.realm_frame(ti, num(0) as usize, frame);
                    let n = NodeId::from_parts(num(1), num(2));
                    let backend = self.backend_id(ti, fi, n);
                    let f = &self.targets[ti].frames[fi];
                    let (loader, id) = (f.loader.clone(), f.id.clone());
                    if let Some(Json::Object(val)) = m.get_mut("value") {
                        val.insert("backendNodeId".into(), json!(backend));
                        val.insert("loaderId".into(), json!(loader));
                        val.insert("frameId".into(), json!(id));
                    }
                }
                if let Some(r) = m.remove("realm").and_then(|r| r.as_u64()) {
                    let fi = self.realm_frame(ti, r as usize, frame);
                    let id = self.targets[ti].frames[fi].id.clone();
                    m.insert("value".into(), json!({"context": id}));
                }
                // Another origin's window, by its context in the page's frame tree.
                if let Some(c) = m.remove("frame").and_then(|c| c.as_u64()) {
                    let fi = u32::try_from(c)
                        .ok()
                        .and_then(|c| self.targets[ti].tab.frame_of_context(c))
                        .unwrap_or(frame);
                    let id = self.targets[ti].frames[fi].id.clone();
                    m.insert("value".into(), json!({"context": id}));
                }
                for (_, c) in m.iter_mut() {
                    self.deep_ids(ti, frame, c);
                }
            }
            Json::Array(a) => a.iter_mut().for_each(|c| self.deep_ids(ti, frame, c)),
            _ => {}
        }
    }

    /// Builds a BiDi `LocalValue` in a context and holds it as an object id.
    pub(crate) fn materialize_in(
        &mut self,
        ti: usize,
        ctx: i64,
        v: &pageon_js::LocalValue,
    ) -> Result<String, CdpError> {
        let (_, live, at) = self.live_ctx(ti, ctx)?;
        let h = live
            .engine
            .borrow_mut()
            .materialize(at, v)
            .map_err(CdpError::params)?;
        let t = &mut self.targets[ti];
        t.next_object += 1;
        let id = format!("{}.{ctx}.{}", t.epoch, t.next_object);
        t.objects.insert(id.clone(), (ctx, h));
        Ok(id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn exception_reply(
        &mut self,
        ti: usize,
        ctx: i64,
        h: Handle,
        text: &str,
        line: u32,
        column: u32,
        group: Option<&str>,
        ser: Ser,
    ) -> Reply {
        let deep = match ser {
            Ser::Deep(depth) => self.deep_value(ti, ctx, h, depth).ok(),
            Ser::Cdp { .. } => None,
        };
        let mut ex = self.remote(ti, ctx, h, false, group)?;
        if let Some(d) = deep {
            ex["deepSerializedValue"] = d;
        }
        Ok(exception(ex, text, line, column))
    }

    /// The node behind `objectId`, `backendNodeId` or `nodeId`, with its frame.
    fn node_param(&mut self, ti: usize, params: &Json) -> Result<(usize, NodeId), CdpError> {
        if let Some(oid) = params.get("objectId").and_then(Json::as_str) {
            let (ctx, h) = self.object(ti, oid)?;
            let (frame, live, at) = self.live_ctx(ti, ctx)?;
            let n = live.engine.borrow_mut().node_of(at, h);
            return n
                .map(|n| (frame, n))
                .ok_or_else(|| CdpError::server("Object id doesn't reference a Node"));
        }
        let id = params
            .get("backendNodeId")
            .or_else(|| params.get("nodeId"))
            .and_then(Json::as_u64)
            .ok_or_else(|| {
                CdpError::params("Either nodeId, backendNodeId or objectId must be specified")
            })?;
        self.targets[ti]
            .backends
            .get(&id)
            .copied()
            .ok_or_else(|| CdpError::server("No node with given id found"))
    }

    pub(super) fn backend_id(&mut self, ti: usize, frame: usize, n: NodeId) -> u64 {
        if let Some(&b) = self.targets[ti].nodes.get(&(frame, n)) {
            return b;
        }
        self.next_backend += 1;
        let b = self.next_backend;
        let t = &mut self.targets[ti];
        t.nodes.insert((frame, n), b);
        t.backends.insert(b, (frame, n));
        b
    }

    /// A CDP `DOM.Node` for a node of a frame's live document; an `<iframe>` also gets its
    /// frame's id and document.
    fn node_json(&mut self, ti: usize, frame: usize, n: NodeId) -> Result<Json, CdpError> {
        let backend = self.backend_id(ti, frame, n);
        let url = self.targets[ti].frames[frame].url.clone();
        let child = self.targets[ti]
            .tab
            .frames_meta()
            .iter()
            .position(|m| m.parent == Some(frame) && m.owner == Some(n));
        let live = self.live(ti, frame)?;
        let dom = live.dom.borrow();
        let node = dom
            .get(n)
            .ok_or_else(|| CdpError::server("No node with given id found"))?;
        let children = dom.children(n).count();
        let (ty, name, local, value) = match &node.data {
            NodeData::Document => (9, "#document".to_string(), String::new(), String::new()),
            NodeData::DocumentFragment | NodeData::ShadowRoot { .. } => (
                11,
                "#document-fragment".into(),
                String::new(),
                String::new(),
            ),
            NodeData::Doctype { name, .. } => (10, name.clone(), String::new(), String::new()),
            NodeData::Text(t) => (3, "#text".into(), String::new(), t.clone()),
            NodeData::Comment(t) => (8, "#comment".into(), String::new(), t.clone()),
            NodeData::ProcessingInstruction { target, data } => {
                (7, target.clone(), String::new(), data.clone())
            }
            NodeData::Element { .. } => {
                let local = node.local_name().unwrap_or_default().to_string();
                let html = node.namespace() == Some(pageon_dom::Namespace::Html);
                let name = if html {
                    local.to_ascii_uppercase()
                } else {
                    local.clone()
                };
                (1, name, local, String::new())
            }
        };
        let mut j = json!({
            "nodeId": 0,
            "backendNodeId": backend,
            "nodeType": ty,
            "nodeName": name,
            "localName": local,
            "nodeValue": value,
            "childNodeCount": children,
        });
        if ty == 1 {
            let attrs: Vec<String> = node
                .attrs()
                .iter()
                .flat_map(|a| [a.qualified_name(), a.value.clone()])
                .collect();
            j["attributes"] = json!(attrs);
        }
        if ty == 9 {
            j["documentURL"] = json!(url);
            j["baseURL"] = json!(url);
            j["xmlVersion"] = json!("");
        }
        drop(dom);
        if let Some(c) = child {
            j["frameId"] = json!(self.targets[ti].frames[c].id);
            if self.targets[ti].frames[c].loaded
                && let Ok(l) = self.live(ti, c)
            {
                let root = l.dom.borrow().root();
                j["contentDocument"] = self.node_json(ti, c, root)?;
            }
        }
        Ok(j)
    }

    /// Runs a call in the page and answers it (or defers it while its promise is pending).
    #[allow(clippy::too_many_arguments)]
    fn run_call(
        &mut self,
        ti: usize,
        id: &Json,
        ctx: i64,
        ser: Ser,
        await_promise: bool,
        group: Option<String>,
        call: impl FnOnce(&mut pageon_js::Engine, usize) -> Result<Handle, pageon_js::Thrown>,
    ) -> Reply {
        let by_value = ser.by_value();
        let frame = self.frame_of(ti, ctx)?;
        let name = self.ctx_name(ti, ctx);
        let (done, out) = self.targets[ti]
            .tab
            .script_in(frame, "evaluate", |e, main| {
                // The context's world (an isolated world is created on first use).
                let realm = if name.is_empty() {
                    main
                } else {
                    match e.world(main, &name) {
                        Ok(w) => w,
                        Err(text) => {
                            return Done::Thrown(
                                pageon_js::Thrown {
                                    text,
                                    line: 0,
                                    column: 0,
                                    exception: None,
                                },
                                None,
                            );
                        }
                    }
                };
                match call(e, realm) {
                    Err(t) => {
                        let ex = t.exception.map(|h| e.describe(realm, h, false));
                        Done::Thrown(t, ex.and_then(Result::ok))
                    }
                    Ok(h) if await_promise => match e.promise_state(realm, h) {
                        PromiseState::Pending => Done::Waiting(h),
                        PromiseState::Fulfilled(v) => {
                            Done::Value(v, e.describe(realm, v, by_value))
                        }
                        PromiseState::Rejected(v) => {
                            let d = e.describe(realm, v, false).ok();
                            Done::Thrown(
                                pageon_js::Thrown {
                                    text: "Uncaught (in promise)".into(),
                                    line: 0,
                                    column: 0,
                                    exception: Some(v),
                                },
                                d,
                            )
                        }
                        PromiseState::NotPromise => Done::Value(h, e.describe(realm, h, by_value)),
                    },
                    Ok(h) => Done::Value(h, e.describe(realm, h, by_value)),
                }
            })
            .map_err(|e| CdpError::server(e.to_string()))?;
        let epoch = self.targets[ti].epoch;
        let reply = match done {
            Done::Value(h, d) => {
                let deep = match ser {
                    Ser::Deep(depth) => Some(self.deep_value(ti, ctx, h, depth)?),
                    Ser::Cdp { .. } => None,
                };
                let mut r =
                    self.register(ti, ctx, h, d.map_err(CdpError::server)?, group.as_deref());
                if let Some(d) = deep {
                    r["deepSerializedValue"] = d;
                }
                Ok(json!({"result": r}))
            }
            Done::Thrown(t, d) => {
                let ex = match (t.exception, d) {
                    (Some(h), Some(r)) => {
                        let deep = match ser {
                            Ser::Deep(depth) => self.deep_value(ti, ctx, h, depth).ok(),
                            Ser::Cdp { .. } => None,
                        };
                        let mut ex = self.register(ti, ctx, h, r, group.as_deref());
                        if let Some(d) = deep {
                            ex["deepSerializedValue"] = d;
                        }
                        ex
                    }
                    _ => json!({"type": "undefined"}),
                };
                Ok(exception(ex, &t.text, t.line, t.column))
            }
            Done::Waiting(h) => {
                let session_id = self.targets[ti].session_id.clone().unwrap_or_default();
                self.pending.push(Pending {
                    id: id.clone(),
                    session_id,
                    target: self.targets[ti].id.clone(),
                    epoch,
                    ctx,
                    promise: h,
                    ser,
                    group,
                    budget_ms: AWAIT_BUDGET_MS,
                });
                Ok(json!({}))
            }
        };
        self.after_outcome(ti, &out);
        reply
    }

    /// `Runtime.*` and `DOM.*`.
    pub(super) fn runtime_method(
        &mut self,
        ti: usize,
        id: &Json,
        method: &str,
        params: &Json,
    ) -> Reply {
        let flag = |k: &str| params.get(k).and_then(Json::as_bool).unwrap_or(false);
        let group = params
            .get("objectGroup")
            .and_then(Json::as_str)
            .map(str::to_string);
        match method {
            "Runtime.enable" => {
                if !self.targets[ti].runtime {
                    self.targets[ti].runtime = true;
                    self.announce_contexts(ti);
                }
                Ok(json!({}))
            }
            "Runtime.disable" => {
                self.targets[ti].runtime = false;
                Ok(json!({}))
            }
            "Runtime.runIfWaitingForDebugger" | "DOM.enable" | "DOM.disable" => Ok(json!({})),
            "DOM.scrollIntoViewIfNeeded" => {
                let (frame, n) = self.node_param(ti, params)?;
                let tab = &mut self.targets[ti].tab;
                tab.scroll_into_view(frame, n, &mut crate::Outcome::default())
                    .map_err(|e| CdpError::server(e.to_string()))?;
                Ok(json!({}))
            }
            "Runtime.evaluate" => {
                let ctx = self.context(ti, params)?;
                let source = params
                    .get("expression")
                    .and_then(Json::as_str)
                    .ok_or_else(|| CdpError::params("expression: string value expected"))?
                    .to_string();
                self.run_call(
                    ti,
                    id,
                    ctx,
                    Ser::of(params)?,
                    flag("awaitPromise"),
                    group,
                    |e, realm| e.evaluate(realm, &source),
                )
            }
            "Runtime.callFunctionOn" => {
                let declaration = params
                    .get("functionDeclaration")
                    .and_then(Json::as_str)
                    .ok_or_else(|| CdpError::params("functionDeclaration: string value expected"))?
                    .to_string();
                let (ctx, this) = match params.get("objectId").and_then(Json::as_str) {
                    Some(oid) => {
                        let (c, h) = self.object(ti, oid)?;
                        (c, Some(h))
                    }
                    None => (self.context(ti, params)?, None),
                };
                let mut args = Vec::new();
                for a in params
                    .get("arguments")
                    .and_then(Json::as_array)
                    .into_iter()
                    .flatten()
                {
                    args.push(
                        if let Some(oid) = a.get("objectId").and_then(Json::as_str) {
                            // Objects never cross worlds (as in Chrome): a main-world object
                            // passed into an isolated world would bring the page's prototypes.
                            let (c, h) = self.object(ti, oid)?;
                            if self.ctx_name(ti, c) != self.ctx_name(ti, ctx) {
                                return Err(CdpError::server(
                                    "Argument should belong to the same JavaScript world as target object",
                                ));
                            }
                            Arg::Handle(h)
                        } else if let Some(u) = a.get("unserializableValue").and_then(Json::as_str)
                        {
                            if !matches!(u, "NaN" | "Infinity" | "-Infinity" | "-0")
                                && !(u.ends_with('n')
                                    && u[..u.len() - 1]
                                        .trim_start_matches('-')
                                        .bytes()
                                        .all(|b| b.is_ascii_digit()))
                            {
                                return Err(CdpError::params(format!("unserializableValue {u:?}")));
                            }
                            Arg::Unserializable(u.to_string())
                        } else if let Some(v) = a.get("value") {
                            Arg::Json(v.clone())
                        } else {
                            Arg::Undefined
                        },
                    );
                }
                self.run_call(
                    ti,
                    id,
                    ctx,
                    Ser::of(params)?,
                    flag("awaitPromise"),
                    group,
                    |e, realm| e.call_function(realm, &declaration, this, &args),
                )
            }
            "Runtime.getProperties" => {
                if flag("accessorPropertiesOnly") {
                    return Ok(json!({"result": []}));
                }
                let oid = params
                    .get("objectId")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let (ctx, h) = self.object(ti, oid)?;
                let (_, live, at) = self.live_ctx(ti, ctx)?;
                let props = live.engine.borrow_mut().properties(at, h);
                let mut result = Vec::with_capacity(props.len());
                for (name, v) in props {
                    let value = self.remote(ti, ctx, v, false, group.as_deref())?;
                    result.push(json!({
                        "name": name, "value": value, "writable": true,
                        "configurable": true, "enumerable": true, "isOwn": true,
                    }));
                }
                Ok(json!({"result": result}))
            }
            "Runtime.releaseObject" => {
                let oid = params
                    .get("objectId")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                self.release(ti, oid);
                Ok(json!({}))
            }
            "Runtime.releaseObjectGroup" => {
                let g = group.unwrap_or_default();
                let ids = self.targets[ti].groups.remove(&g).unwrap_or_default();
                for oid in ids {
                    self.release(ti, &oid);
                }
                Ok(json!({}))
            }
            "Runtime.addBinding" => {
                let name = params
                    .get("name")
                    .and_then(Json::as_str)
                    .ok_or_else(|| CdpError::params("name: string value expected"))?
                    .to_string();
                if !self.targets[ti].bindings.contains(&name) {
                    self.targets[ti].bindings.push(name.clone());
                    for frame in 0..self.targets[ti].frames.len() {
                        if self.targets[ti].frames[frame].loaded
                            && let Ok(live) = self.targets[ti].tab.live_in(frame)
                        {
                            live.engine.borrow_mut().add_binding(live.realm, &name);
                        }
                    }
                }
                Ok(json!({}))
            }
            "Runtime.removeBinding" => {
                let name = params
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.targets[ti].bindings.retain(|b| *b != name);
                for frame in 0..self.targets[ti].frames.len() {
                    if let Some(live) = self.targets[ti].tab.live_if_started(frame) {
                        live.engine.borrow_mut().remove_binding(live.realm, &name);
                    }
                }
                Ok(json!({}))
            }
            "DOM.getDocument" => {
                let root = self.live(ti, 0)?.dom.borrow().root();
                let mut node = self.node_json(ti, 0, root)?;
                node["nodeId"] = node["backendNodeId"].clone();
                Ok(json!({"root": node}))
            }
            "DOM.getContentQuads" | "DOM.getBoxModel" => {
                let (frame, n) = self.node_param(ti, params)?;
                let tab = &self.targets[ti].tab;
                let b = tab
                    .frame_document(frame)
                    .and_then(|d| d.layout(crate::Viewport::default()).client_rect(n))
                    .ok_or_else(|| CdpError::server("Could not compute content quads."))?;
                // In top-level viewport coordinates: add the iframes' content-box origins.
                let (ox, oy) = tab.frame_offset(frame);
                let (x, y, w, h) = (
                    f64::from(b.x) + ox,
                    f64::from(b.y) + oy,
                    f64::from(b.width),
                    f64::from(b.height),
                );
                let quad = json!([x, y, x + w, y, x + w, y + h, x, y + h]);
                if method == "DOM.getContentQuads" {
                    return Ok(json!({"quads": [quad]}));
                }
                // pageon's boxes are border boxes; padding, border and margin are not split out.
                Ok(json!({"model": {
                    "content": quad, "padding": quad, "border": quad, "margin": quad,
                    "width": w, "height": h,
                }}))
            }
            "DOM.getFrameOwner" => {
                let id = params
                    .get("frameId")
                    .and_then(Json::as_str)
                    .unwrap_or_default();
                let frame = self
                    .frame_index(ti, id)
                    .ok_or_else(|| CdpError::server("Frame with the given id was not found."))?;
                let metas = self.targets[ti].tab.frames_meta();
                let (Some(parent), Some(owner)) = (metas[frame].parent, metas[frame].owner) else {
                    return Err(CdpError::server(
                        "Frame with the given id does not belong to the target.",
                    ));
                };
                let b = self.backend_id(ti, parent, owner);
                Ok(json!({"backendNodeId": b, "nodeId": 0}))
            }
            "DOM.setFileInputFiles" => {
                let (frame, n) = self.node_param(ti, params)?;
                let files: Vec<String> = params
                    .get("files")
                    .and_then(Json::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| f.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let tab = &mut self.targets[ti].tab;
                if files.is_empty() {
                    return Err(CdpError::params("files: staged file paths or names"));
                }
                tab.upload_node(frame, n, &files)
                    .map_err(|e| CdpError::server(format!("{e}: {}", e.guidance())))?;
                Ok(json!({}))
            }
            "DOM.describeNode" => {
                let (frame, n) = self.node_param(ti, params)?;
                Ok(json!({"node": self.node_json(ti, frame, n)?}))
            }
            "DOM.resolveNode" => {
                let (frame, n) = self.node_param(ti, params)?;
                // A context of the node's frame (its main world unless one is named).
                let ctx = if params.get("executionContextId").is_some() {
                    let c = self.context(ti, params)?;
                    if self.ctx_frame(ti, c) != Some(frame) {
                        return Err(CdpError::server(
                            "Node with given id does not belong to the document",
                        ));
                    }
                    c
                } else {
                    self.targets[ti]
                        .contexts
                        .iter()
                        .find(|c| c.frame == frame && c.name.is_empty())
                        .map(|c| c.id)
                        .ok_or_else(|| CdpError::server("Cannot find context with specified id"))?
                };
                let (_, live, at) = self.live_ctx(ti, ctx)?;
                let h = live
                    .engine
                    .borrow_mut()
                    .handle_for_node(at, n)
                    .ok_or_else(|| CdpError::server("No node with given id found"))?;
                let r = self.remote(ti, ctx, h, false, group.as_deref())?;
                Ok(json!({"object": r}))
            }
            _ => Err(super::unknown(method)),
        }
    }

    fn release(&mut self, ti: usize, oid: &str) {
        if let Some((ctx, h)) = self.targets[ti].objects.remove(oid)
            && let Some(frame) = self.ctx_frame(ti, ctx)
            && let Some(live) = self.targets[ti].tab.live_if_started(frame)
        {
            live.engine.borrow_mut().release(h);
        }
    }
}

fn exception(ex: Json, text: &str, line: u32, column: u32) -> Json {
    json!({
        "result": ex,
        "exceptionDetails": {
            "exceptionId": 1,
            "text": text,
            "lineNumber": line,
            "columnNumber": column,
            "exception": ex,
        },
    })
}
