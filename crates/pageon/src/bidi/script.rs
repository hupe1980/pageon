//! `script.*`: targets (contexts, sandboxes, realms), LocalValue arguments, RemoteValue
//! results with ownership, and awaited promises answered when they settle.

use pageon_env::Env;
use pageon_js::{LocalKey, LocalValue};
use serde_json::{Value as Json, json};

use super::{Answer, BidiError, Cdp, Realm, Session, Waiter, parse_shared_id};

impl<E: Env> Session<'_, E> {
    /// The realm a `script.Target` names, creating a sandbox on first use.
    pub(super) fn target_realm(&mut self, t: &Json) -> Result<Realm, BidiError> {
        if let Some(r) = t.get("realm").and_then(Json::as_str) {
            return self
                .realms
                .iter()
                .find(|x| x.id == r)
                .cloned()
                .ok_or_else(|| BidiError::new("no such frame", format!("no realm {r:?}")));
        }
        let context = t
            .get("context")
            .and_then(Json::as_str)
            .ok_or_else(|| BidiError::invalid("target: {context, sandbox?} or {realm}"))?
            .to_string();
        let session = self.context_of(&context)?.session.clone();
        let sandbox = t.get("sandbox").and_then(Json::as_str).map(str::to_string);
        let find = |s: &Self| {
            s.realms
                .iter()
                .find(|r| r.context == context && r.sandbox == sandbox)
                .cloned()
        };
        if let Some(r) = find(self) {
            return Ok(r);
        }
        if let Some(name) = &sandbox {
            self.call(
                "Page.createIsolatedWorld",
                json!({"frameId": context, "worldName": name}),
                Some(&session),
            )?;
            if let Some(r) = find(self) {
                return Ok(r);
            }
        }
        Err(BidiError::new(
            "no such frame",
            format!("browsing context {context:?} has no document to run scripts in yet"),
        ))
    }

    pub(super) fn run_script(
        &mut self,
        id: &Json,
        method: &str,
        p: &Json,
    ) -> Result<Option<Json>, BidiError> {
        let realm = self.target_realm(&p["target"])?;
        let await_promise = p
            .get("awaitPromise")
            .and_then(Json::as_bool)
            .ok_or_else(|| BidiError::invalid("awaitPromise: a boolean is required"))?;
        let root = match p.get("resultOwnership").and_then(Json::as_str) {
            None | Some("none") => false,
            Some("root") => true,
            Some(o) => return Err(BidiError::invalid(format!("resultOwnership {o:?}"))),
        };
        let so = &p["serializationOptions"];
        let mut ser = json!({"serialization": "deep", "additionalParameters": {
            "maxNodeDepth": so["maxDomDepth"].as_u64().unwrap_or(0),
        }});
        if let Some(d) = so["maxObjectDepth"].as_u64() {
            ser["maxDepth"] = json!(d);
        }
        let gesture = p["userActivation"].as_bool().unwrap_or(false);
        let cdp = if method == "script.evaluate" {
            let expression = p["expression"]
                .as_str()
                .ok_or_else(|| BidiError::invalid("expression: a string is required"))?;
            self.cdp(
                "Runtime.evaluate",
                json!({
                    "expression": expression, "contextId": realm.ctx, "awaitPromise": await_promise,
                    "userGesture": gesture, "serializationOptions": ser,
                }),
                Some(&realm.session),
            )
        } else {
            let decl = p["functionDeclaration"]
                .as_str()
                .ok_or_else(|| BidiError::invalid("functionDeclaration: a string is required"))?
                .to_string();
            let mut args = Vec::new();
            for a in p["arguments"].as_array().into_iter().flatten() {
                args.push(self.argument(&realm, a)?);
            }
            let mut call = json!({
                "functionDeclaration": decl, "arguments": args, "awaitPromise": await_promise,
                "userGesture": gesture, "serializationOptions": ser,
            });
            match p.get("this").filter(|t| t["type"] != "undefined") {
                Some(t) => {
                    let a = self.argument(&realm, t)?;
                    let oid = match a.get("objectId").and_then(Json::as_str) {
                        Some(o) => o.to_string(),
                        None => {
                            let lv = local_value(self, &realm, t)?;
                            self.materialize(&realm, &lv)?
                        }
                    };
                    call["objectId"] = json!(oid);
                }
                None => call["executionContextId"] = json!(realm.ctx),
            }
            self.cdp("Runtime.callFunctionOn", call, Some(&realm.session))
        };
        let w = Waiter {
            id: id.clone(),
            realm: realm.id.clone(),
            root,
            session: realm.session.clone(),
        };
        match cdp {
            Cdp::Ok(r) => self.script_result(&w, Ok(r)).map(Some),
            Cdp::Err(e) => Err(BidiError::from_cdp(&e)),
            Cdp::Deferred(cid) => {
                self.waiting.insert(cid, w);
                Ok(None)
            }
        }
    }

    /// Answers a deferred script command.
    pub(super) fn finish_script(&mut self, w: Waiter, r: Result<Json, Json>) {
        match self.script_result(&w, r) {
            Ok(v) => self.success(&w.id, v),
            Err(e) => self.error(&w.id, e),
        }
    }

    fn script_result(&mut self, w: &Waiter, r: Result<Json, Json>) -> Answer {
        let r = r.map_err(|e| BidiError::from_cdp(&e))?;
        if let Some(d) = r.get("exceptionDetails") {
            let ex = &d["exception"];
            let value = self.owned(w, ex);
            let text = ex["description"]
                .as_str()
                .and_then(|s| s.lines().next())
                .map(str::to_string)
                .or_else(|| {
                    ex.get("value")
                        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                })
                .unwrap_or_else(|| {
                    d["text"]
                        .as_str()
                        .unwrap_or("Uncaught")
                        .trim_start_matches("Uncaught ")
                        .to_string()
                });
            return Ok(json!({
                "type": "exception", "realm": w.realm,
                "exceptionDetails": {
                    "columnNumber": d["columnNumber"].as_u64().unwrap_or(0),
                    "lineNumber": d["lineNumber"].as_u64().unwrap_or(0),
                    "text": text, "exception": value,
                    "stackTrace": {"callFrames": super::stack(&d["stackTrace"])},
                },
            }));
        }
        let value = self.owned(w, &r["result"]);
        Ok(json!({"type": "success", "result": value, "realm": w.realm}))
    }

    /// The RemoteValue of a CDP result, with a handle if the client owns it (else released).
    fn owned(&mut self, w: &Waiter, r: &Json) -> Json {
        let mut v = match r.get("deepSerializedValue") {
            Some(d) => self.bidi_value(d),
            None => json!({"type": r["type"].as_str().unwrap_or("undefined")}),
        };
        if let Some(oid) = r["objectId"].as_str() {
            if w.root {
                v["handle"] = json!(oid);
            } else {
                let _ = self.cdp(
                    "Runtime.releaseObject",
                    json!({"objectId": oid}),
                    Some(&w.session),
                );
            }
        }
        v
    }

    /// A CDP `CallArgument` for a BiDi LocalValue.
    fn argument(&mut self, realm: &Realm, a: &Json) -> Result<Json, BidiError> {
        if let Some(h) = self.own_handle(realm, a)? {
            return Ok(json!({"objectId": h}));
        }
        if let Some(s) = a.get("sharedId").and_then(Json::as_str) {
            return Ok(json!({"objectId": self.resolve_shared(realm, s)?}));
        }
        let ty = a["type"]
            .as_str()
            .ok_or_else(|| BidiError::invalid(format!("not a LocalValue: {a}")))?;
        Ok(match ty {
            "undefined" => json!({}),
            "null" => json!({"value": null}),
            "string" | "boolean" => json!({"value": a["value"]}),
            "number" => match &a["value"] {
                Json::String(s)
                    if matches!(s.as_str(), "NaN" | "-0" | "Infinity" | "-Infinity") =>
                {
                    json!({"unserializableValue": s})
                }
                v @ Json::Number(_) => json!({"value": v}),
                v => return Err(BidiError::invalid(format!("number value {v}"))),
            },
            "bigint" => {
                let s = a["value"]
                    .as_str()
                    .ok_or_else(|| BidiError::invalid("bigint value: a string"))?;
                json!({"unserializableValue": format!("{s}n")})
            }
            "channel" => {
                return Err(BidiError::unsupported(
                    "channel arguments (script.message) are not supported",
                ));
            }
            _ => {
                let lv = local_value(self, realm, a)?;
                json!({"objectId": self.materialize(realm, &lv)?})
            }
        })
    }

    fn materialize(&mut self, realm: &Realm, v: &LocalValue) -> Result<String, BidiError> {
        let ti = self
            .cdp
            .target_of(&realm.session)
            .ok_or_else(|| BidiError::new("no such frame", "the realm's page is gone"))?;
        self.cdp
            .materialize_in(ti, realm.ctx, v)
            .map_err(|e| BidiError::invalid(e.message()))
    }

    /// A reference's `handle` if it is `realm`'s. Handles belong to one realm; a node's handle
    /// from another realm (Puppeteer sends both) is resolved by its `sharedId` instead, so
    /// sandboxes get their own world's object.
    fn own_handle(&self, realm: &Realm, a: &Json) -> Result<Option<String>, BidiError> {
        let Some(h) = a.get("handle").and_then(Json::as_str) else {
            return Ok(None);
        };
        let ours = self
            .cdp
            .target_of(&realm.session)
            .and_then(|ti| self.cdp.object_context(ti, h))
            .is_some_and(|c| c == realm.ctx);
        if ours {
            Ok(Some(h.to_string()))
        } else if a.get("sharedId").is_some() {
            Ok(None)
        } else {
            Err(BidiError::new(
                "no such handle",
                format!("handle {h:?} belongs to another realm"),
            ))
        }
    }

    /// The object id of the node a `sharedId` names, in `realm`, while its document lives.
    pub(super) fn resolve_shared(&mut self, realm: &Realm, s: &str) -> Result<String, BidiError> {
        let no = || BidiError::new("no such node", format!("no node {s:?}"));
        let (frame, loader, backend) = parse_shared_id(s).ok_or_else(no)?;
        let c = self.context_of(frame).map_err(|_| no())?;
        if c.navigation.as_deref() != Some(loader) {
            return Err(BidiError::new(
                "no such node",
                format!("node {s:?} belongs to a document that is gone"),
            ));
        }
        let session = c.session.clone();
        // The node's frame, in the same world as `realm` (its sandbox, or the main world).
        let ctx = if realm.context == frame {
            realm.ctx
        } else {
            let mut target = json!({"context": frame});
            if let Some(sb) = &realm.sandbox {
                target["sandbox"] = json!(sb);
            }
            self.target_realm(&target).map_err(|_| no())?.ctx
        };
        let r = self
            .call(
                "DOM.resolveNode",
                json!({"backendNodeId": backend, "executionContextId": ctx}),
                Some(&session),
            )
            .map_err(|_| no())?;
        r["object"]["objectId"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(no)
    }

    pub(super) fn disown(&mut self, p: &Json) -> Answer {
        let realm = self.target_realm(&p["target"])?;
        for h in p["handles"].as_array().into_iter().flatten() {
            if let Some(h) = h.as_str() {
                let _ = self.cdp(
                    "Runtime.releaseObject",
                    json!({"objectId": h}),
                    Some(&realm.session),
                );
            }
        }
        Ok(json!({}))
    }

    pub(super) fn get_realms(&mut self, p: &Json) -> Answer {
        let context = p.get("context").and_then(Json::as_str);
        if let Some(c) = context {
            self.context_of(c)?;
        }
        if let Some(t) = p.get("type").and_then(Json::as_str)
            && t != "window"
        {
            return Ok(json!({"realms": []}));
        }
        let realms: Vec<Json> = self
            .realms
            .iter()
            .filter(|r| context.is_none_or(|c| r.context == c))
            .map(|r| {
                let mut j = json!({"realm": r.id, "origin": r.origin, "type": "window", "context": r.context});
                if let Some(s) = &r.sandbox {
                    j["sandbox"] = json!(s);
                }
                j
            })
            .collect();
        Ok(json!({"realms": realms}))
    }
}

/// A typed LocalValue, with references resolved to held values of `realm`.
fn local_value<E: Env>(
    s: &mut Session<'_, E>,
    realm: &Realm,
    a: &Json,
) -> Result<LocalValue, BidiError> {
    if let Some(h) = s.own_handle(realm, a)? {
        return held(s, realm, &h);
    }
    if let Some(sh) = a.get("sharedId").and_then(Json::as_str) {
        let oid = s.resolve_shared(realm, sh)?;
        return held(s, realm, &oid);
    }
    let v = &a["value"];
    let list = |s: &mut Session<'_, E>| -> Result<Vec<LocalValue>, BidiError> {
        v.as_array()
            .ok_or_else(|| BidiError::invalid(format!("{} value: a list", a["type"])))?
            .iter()
            .map(|x| local_value(s, realm, x))
            .collect()
    };
    let pairs = |s: &mut Session<'_, E>| -> Result<Vec<(LocalKey, LocalValue)>, BidiError> {
        let mut out = Vec::new();
        for kv in v
            .as_array()
            .ok_or_else(|| BidiError::invalid(format!("{} value: a list of pairs", a["type"])))?
        {
            let (k, val) = (&kv[0], &kv[1]);
            let k = match k {
                Json::String(k) => LocalKey::String(k.clone()),
                k => LocalKey::Value(local_value(s, realm, k)?),
            };
            out.push((k, local_value(s, realm, val)?));
        }
        Ok(out)
    };
    Ok(match a["type"].as_str().unwrap_or_default() {
        "undefined" => LocalValue::Undefined,
        "null" => LocalValue::Null,
        "string" => LocalValue::String(v.as_str().unwrap_or_default().to_string()),
        "boolean" => LocalValue::Bool(v.as_bool().unwrap_or(false)),
        "number" => LocalValue::Number(match v {
            Json::String(n) => match n.as_str() {
                "NaN" => f64::NAN,
                "-0" => -0.0,
                "Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                n => return Err(BidiError::invalid(format!("number value {n:?}"))),
            },
            n => n
                .as_f64()
                .ok_or_else(|| BidiError::invalid(format!("number value {n}")))?,
        }),
        "bigint" => LocalValue::BigInt(v.as_str().unwrap_or_default().to_string()),
        "array" => LocalValue::Array(list(s)?),
        "set" => LocalValue::Set(list(s)?),
        "object" => LocalValue::Object(pairs(s)?),
        "map" => LocalValue::Map(pairs(s)?),
        "date" => LocalValue::Date(v.as_str().unwrap_or_default().to_string()),
        "regexp" => LocalValue::RegExp {
            pattern: v["pattern"].as_str().unwrap_or_default().to_string(),
            flags: v["flags"].as_str().unwrap_or_default().to_string(),
        },
        t => {
            return Err(BidiError::invalid(format!(
                "{t:?} is not a LocalValue type"
            )));
        }
    })
}

fn held<E: Env>(s: &Session<'_, E>, realm: &Realm, oid: &str) -> Result<LocalValue, BidiError> {
    let ti = s
        .cdp
        .target_of(&realm.session)
        .ok_or_else(|| BidiError::new("no such handle", format!("no handle {oid:?}")))?;
    s.cdp
        .handle_of(ti, oid)
        .map(LocalValue::Handle)
        .ok_or_else(|| BidiError::new("no such handle", format!("no handle {oid:?}")))
}
