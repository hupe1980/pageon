//! `browsingContext.*`, `storage.*` and the `pageon:` extension module.

use pageon_env::Env;
use serde_json::{Value as Json, json};

use super::{Answer, BidiError, Session, parse_shared_id};

impl<E: Env> Session<'_, E> {
    /// A browsingContext, storage or pageon command; `None` if pageon has no such command.
    pub(super) fn context_command(&mut self, m: &str, p: &Json) -> Option<Answer> {
        Some(match m {
            "browsingContext.getTree" => self.get_tree(p),
            "browsingContext.create" => self.create(p),
            "browsingContext.close" => self.close(p),
            "browsingContext.activate" => self.top(p).and_then(|c| {
                let s = self.context_of(&c)?.session.clone();
                self.call("Target.activateTarget", json!({"targetId": c}), None)
                    .map(|_| json!({}))
                    .map_err(|e| BidiError::new("unknown error", format!("{} ({s})", e.message)))
            }),
            "browsingContext.navigate" => self.navigate(p),
            "browsingContext.reload" => self.reload(p),
            "browsingContext.traverseHistory" => self.traverse(p),
            "browsingContext.captureScreenshot" => self.screenshot(p),
            "browsingContext.setViewport" => self.set_viewport(p),
            "browsingContext.handleUserPrompt" => self.context_id(p).and_then(|_| {
                Err(BidiError::new(
                    "no such alert",
                    "no prompt is open: pageon answers dialogs itself (alert: ok; confirm, prompt: cancel; beforeunload: leave) and reports them in pageon:act outcomes",
                ))
            }),
            "browsingContext.locateNodes" => self.locate(p),
            "storage.getCookies" => self.get_cookies(p),
            "storage.setCookie" => self.set_cookie(p),
            "storage.deleteCookies" => self.delete_cookies(p),
            "pageon:observe" => self.pageon("Pageon.observe", p),
            "pageon:act" => self.pageon("Pageon.act", p),
            _ => return None,
        })
    }

    fn context_id(&self, p: &Json) -> Result<String, BidiError> {
        let c = p["context"]
            .as_str()
            .ok_or_else(|| BidiError::invalid("context: a browsing context id is required"))?;
        self.context_of(c).map(|c| c.id.clone())
    }

    /// A top-level context named by `context`.
    fn top(&self, p: &Json) -> Result<String, BidiError> {
        let c = self.context_id(p)?;
        if self.context_of(&c)?.parent.is_some() {
            return Err(BidiError::invalid(format!(
                "{c:?} is a frame; this command takes a top-level context"
            )));
        }
        Ok(c)
    }

    fn get_tree(&mut self, p: &Json) -> Answer {
        let depth = p["maxDepth"].as_u64();
        let roots: Vec<Json> = match p["root"].as_str() {
            Some(r) => vec![self.info(self.context_of(r)?, depth)],
            None => self
                .contexts
                .iter()
                .filter(|c| c.parent.is_none())
                .map(|c| self.info(c, depth))
                .collect(),
        };
        Ok(json!({"contexts": roots}))
    }

    fn create(&mut self, p: &Json) -> Answer {
        match p["type"].as_str() {
            Some("tab" | "window") => {}
            t => return Err(BidiError::invalid(format!("type {t:?}: tab or window"))),
        }
        if let Some(u) = p["userContext"].as_str()
            && u != "default"
        {
            return Err(BidiError::new(
                "no such user context",
                format!("no user context {u:?}: pageon has one, \"default\""),
            ));
        }
        if let Some(r) = p["referenceContext"].as_str() {
            self.context_of(r)?;
        }
        let r = self.call("Target.createTarget", json!({"url": "about:blank"}), None)?;
        let id = r["targetId"].as_str().unwrap_or_default().to_string();
        Ok(json!({"context": id, "userContext": "default"}))
    }

    fn close(&mut self, p: &Json) -> Answer {
        let c = self.top(p)?;
        self.call("Target.closeTarget", json!({"targetId": c}), None)?;
        Ok(json!({}))
    }

    fn navigate(&mut self, p: &Json) -> Answer {
        let c = self.context_id(p)?;
        let url = p["url"]
            .as_str()
            .ok_or_else(|| BidiError::invalid("url: a string is required"))?;
        if url::Url::parse(url).is_err() {
            return Err(BidiError::invalid(format!(
                "{url:?} is not an absolute URL"
            )));
        }
        let session = self.context_of(&c)?.session.clone();
        if self.context_of(&c)?.parent.is_some() {
            return Err(BidiError::unsupported(
                "navigating a frame by itself is not supported; navigate its top-level context, or set the frame's src with script.callFunction",
            ));
        }
        let r = self.call("Page.navigate", json!({"url": url}), Some(&session))?;
        if let Some(e) = r["errorText"].as_str() {
            self.next_nav += 1;
            let nav = format!("failed-{}", self.next_nav);
            let ts = self.now();
            self.emit(
                "browsingContext.navigationFailed",
                json!({"context": c, "navigation": nav, "timestamp": ts, "url": url}),
                Some(&c),
            );
            return Err(BidiError::new(
                "unknown error",
                format!("navigation to {url} failed: {e}"),
            ));
        }
        let nav = r["loaderId"].as_str().unwrap_or_default().to_string();
        let url = self.context_of(&c)?.url.clone();
        Ok(json!({"navigation": nav, "url": url}))
    }

    fn reload(&mut self, p: &Json) -> Answer {
        let c = self.top(p)?;
        let session = self.context_of(&c)?.session.clone();
        self.call("Page.reload", json!({}), Some(&session))?;
        let ctx = self.context_of(&c)?;
        Ok(json!({"navigation": ctx.navigation, "url": ctx.url}))
    }

    fn traverse(&mut self, p: &Json) -> Answer {
        let c = self.top(p)?;
        let delta = p["delta"]
            .as_i64()
            .ok_or_else(|| BidiError::invalid("delta: an integer is required"))?;
        let session = self.context_of(&c)?.session.clone();
        let h = self.call("Page.getNavigationHistory", json!({}), Some(&session))?;
        let index = h["currentIndex"].as_i64().unwrap_or(0) + delta;
        let len = h["entries"].as_array().map_or(0, Vec::len) as i64;
        if index < 0 || index >= len {
            return Err(BidiError::new(
                "no such history entry",
                format!("no history entry {delta:+} from here ({len} entries)"),
            ));
        }
        if delta != 0 {
            self.call(
                "Page.navigateToHistoryEntry",
                json!({"entryId": index}),
                Some(&session),
            )?;
        }
        Ok(json!({}))
    }

    fn screenshot(&mut self, p: &Json) -> Answer {
        let c = self.top(p).map_err(|_| {
            BidiError::unsupported(
                "screenshots are of top-level contexts; clip one to the frame's element",
            )
        })?;
        let session = self.context_of(&c)?.session.clone();
        let mut params = json!({"captureBeyondViewport": p["origin"] == "document"});
        match p["format"]["type"].as_str() {
            None | Some("image/png") => params["format"] = json!("png"),
            Some("image/jpeg") => {
                params["format"] = json!("jpeg");
                if let Some(q) = p["format"]["quality"].as_f64() {
                    params["quality"] = json!((q * 100.0).round() as u64);
                }
            }
            Some(t) => {
                return Err(BidiError::unsupported(format!(
                    "pageon does not encode {t}: it encodes png or jpeg (image/png, image/jpeg)"
                )));
            }
        }
        let clip = &p["clip"];
        match clip["type"].as_str() {
            None => {}
            Some("box") => {
                params["clip"] = json!({
                    "x": clip["x"], "y": clip["y"], "width": clip["width"],
                    "height": clip["height"], "scale": 1,
                });
            }
            Some("element") => {
                let s = clip["element"]["sharedId"]
                    .as_str()
                    .ok_or_else(|| BidiError::invalid("clip.element: a node with a sharedId"))?;
                let (frame, _, backend) = parse_shared_id(s)
                    .ok_or_else(|| BidiError::new("no such node", format!("no node {s:?}")))?;
                let fs = self.context_of(frame)?.session.clone();
                let q = self.call(
                    "DOM.getContentQuads",
                    json!({"backendNodeId": backend}),
                    Some(&fs),
                )?;
                let quad = q["quads"][0].as_array().cloned().unwrap_or_default();
                let xs: Vec<f64> = quad.iter().step_by(2).filter_map(Json::as_f64).collect();
                let ys: Vec<f64> = quad
                    .iter()
                    .skip(1)
                    .step_by(2)
                    .filter_map(Json::as_f64)
                    .collect();
                if xs.is_empty() {
                    return Err(BidiError::new(
                        "unable to capture screen",
                        "the element has no box (not rendered)",
                    ));
                }
                let (x0, x1) = (
                    xs.iter().cloned().fold(f64::MAX, f64::min),
                    xs.iter().cloned().fold(f64::MIN, f64::max),
                );
                let (y0, y1) = (
                    ys.iter().cloned().fold(f64::MAX, f64::min),
                    ys.iter().cloned().fold(f64::MIN, f64::max),
                );
                params["clip"] =
                    json!({"x": x0, "y": y0, "width": x1 - x0, "height": y1 - y0, "scale": 1});
            }
            Some(t) => {
                return Err(BidiError::invalid(format!(
                    "clip type {t:?}: box or element"
                )));
            }
        }
        let r = self.call("Page.captureScreenshot", params, Some(&session))?;
        Ok(json!({"data": r["data"]}))
    }

    fn set_viewport(&mut self, p: &Json) -> Answer {
        if let Some(c) = p["context"].as_str() {
            self.context_of(c)?;
        }
        // As CDP's Emulation.setDeviceMetricsOverride: accepted; layout keeps the session's
        // viewport, which screenshots and getClientWindows report.
        Ok(json!({}))
    }

    fn locate(&mut self, p: &Json) -> Answer {
        let c = self.context_id(p)?;
        let locator = &p["locator"];
        let max = p["maxNodeCount"].as_u64().unwrap_or(0);
        let realm = self.target_realm(&json!({"context": c}))?;
        let nodes_depth = p["serializationOptions"]["maxDomDepth"]
            .as_u64()
            .unwrap_or(0);
        let ser = json!({"serialization": "deep", "maxDepth": 1, "additionalParameters": {"maxNodeDepth": nodes_depth}});
        let r = match locator["type"].as_str() {
            Some("css") => {
                let sel = locator["value"]
                    .as_str()
                    .ok_or_else(|| BidiError::invalid("locator.value: a CSS selector"))?;
                let mut args = vec![json!({"value": sel}), json!({"value": max})];
                for s in p["startNodes"].as_array().into_iter().flatten() {
                    let id = s["sharedId"]
                        .as_str()
                        .ok_or_else(|| BidiError::invalid("startNodes: nodes with a sharedId"))?;
                    args.push(json!({"objectId": self.resolve_shared(&realm, id)?}));
                }
                self.call(
                    "Runtime.callFunctionOn",
                    json!({
                        "functionDeclaration": "function (sel, max, ...roots) { if (!roots.length) roots = [document]; const out = []; for (const r of roots) for (const n of r.querySelectorAll(sel)) { out.push(n); if (max && out.length >= max) return out; } return out; }",
                        "executionContextId": realm.ctx, "arguments": args,
                        "serializationOptions": ser,
                    }),
                    Some(&realm.session),
                )?
            }
            Some("context") => {
                let child = locator["value"]["context"]
                    .as_str()
                    .ok_or_else(|| BidiError::invalid("locator.value.context: a context id"))?;
                let ch = self.context_of(child)?;
                if ch.parent.as_deref() != Some(c.as_str()) {
                    return Err(BidiError::invalid(format!(
                        "{child:?} is not a child of {c:?}"
                    )));
                }
                let owner = self.call(
                    "DOM.getFrameOwner",
                    json!({"frameId": child}),
                    Some(&realm.session),
                )?;
                let obj = self.call(
                    "DOM.resolveNode",
                    json!({"backendNodeId": owner["backendNodeId"], "executionContextId": realm.ctx}),
                    Some(&realm.session),
                )?;
                self.call(
                    "Runtime.callFunctionOn",
                    json!({
                        "functionDeclaration": "function () { return [this] }",
                        "objectId": obj["object"]["objectId"], "serializationOptions": ser,
                    }),
                    Some(&realm.session),
                )?
            }
            Some(t @ ("accessibility" | "xpath" | "innerText")) => {
                return Err(BidiError::unsupported(format!(
                    "{t} locators are not supported; use css, or pageon:observe for the agent's accessibility view with refs"
                )));
            }
            t => return Err(BidiError::invalid(format!("locator type {t:?}"))),
        };
        if let Some(e) = r.get("exceptionDetails") {
            return Err(BidiError::new(
                "invalid selector",
                e["exception"]["description"]
                    .as_str()
                    .unwrap_or("invalid selector")
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            ));
        }
        if let Some(oid) = r["result"]["objectId"].as_str() {
            let _ = self.cdp(
                "Runtime.releaseObject",
                json!({"objectId": oid}),
                Some(&realm.session),
            );
        }
        let v = self.bidi_value(&r["result"]["deepSerializedValue"]);
        Ok(json!({"nodes": v["value"].clone()}))
    }

    /// The cookie partition's URLs and key: a context's page, a source origin, or all.
    fn partition(&self, p: &Json) -> Result<(Option<String>, Json), BidiError> {
        let part = &p["partition"];
        match part["type"].as_str() {
            Some("context") => {
                let c = part["context"]
                    .as_str()
                    .ok_or_else(|| BidiError::invalid("partition.context"))?;
                let url = self.context_of(c)?.url.clone();
                let origin = crate::frames::origin_of(&url);
                Ok((
                    Some(url),
                    json!({"userContext": "default", "sourceOrigin": origin}),
                ))
            }
            Some("storageKey") => {
                if let Some(u) = part["userContext"].as_str()
                    && u != "default"
                {
                    return Err(BidiError::new(
                        "no such user context",
                        format!("no user context {u:?}"),
                    ));
                }
                let origin = part["sourceOrigin"].as_str().map(str::to_string);
                let key = match &origin {
                    Some(o) => json!({"userContext": "default", "sourceOrigin": o}),
                    None => json!({"userContext": "default"}),
                };
                Ok((origin.map(|o| format!("{}/", o.trim_end_matches('/'))), key))
            }
            None => Ok((None, json!({"userContext": "default"}))),
            Some(t) => Err(BidiError::invalid(format!("partition type {t:?}"))),
        }
    }

    fn cookies(&mut self, url: Option<&str>) -> Result<Vec<Json>, BidiError> {
        let r = match url {
            Some(u) => {
                let s = self
                    .contexts
                    .first()
                    .map(|c| c.session.clone())
                    .ok_or_else(|| {
                        BidiError::new("unknown error", "no page to read cookies from")
                    })?;
                self.call("Network.getCookies", json!({"urls": [u]}), Some(&s))?
            }
            None => self.call("Storage.getCookies", json!({}), None)?,
        };
        Ok(r["cookies"]
            .as_array()
            .into_iter()
            .flatten()
            .map(cdp_to_bidi)
            .collect())
    }

    fn get_cookies(&mut self, p: &Json) -> Answer {
        let (url, key) = self.partition(p)?;
        let all = self.cookies(url.as_deref())?;
        let f = &p["filter"];
        let cookies: Vec<Json> = all.into_iter().filter(|c| cookie_matches(c, f)).collect();
        Ok(json!({"cookies": cookies, "partitionKey": key}))
    }

    fn set_cookie(&mut self, p: &Json) -> Answer {
        let (_, key) = self.partition(p)?;
        let c = &p["cookie"];
        let value = match c["value"]["type"].as_str() {
            Some("string") => c["value"]["value"].clone(),
            _ => {
                return Err(BidiError::unsupported(
                    "cookie values are strings ({type: \"string\", value})",
                ));
            }
        };
        let domain = c["domain"]
            .as_str()
            .ok_or_else(|| BidiError::invalid("cookie.domain is required"))?;
        let mut cdp = json!({
            "name": c["name"], "value": value, "domain": domain,
            "path": c["path"].as_str().unwrap_or("/"),
            "secure": c["secure"].as_bool().unwrap_or(false),
            "httpOnly": c["httpOnly"].as_bool().unwrap_or(false),
        });
        match c["sameSite"].as_str() {
            Some("strict") => cdp["sameSite"] = json!("Strict"),
            Some("lax") => cdp["sameSite"] = json!("Lax"),
            Some("none") => cdp["sameSite"] = json!("None"),
            _ => {}
        }
        if let Some(e) = c["expiry"].as_u64() {
            cdp["expires"] = json!(e);
        }
        let r = self.call("Storage.setCookies", json!({"cookies": [cdp]}), None);
        r.map_err(|e| BidiError::new("unable to set cookie", e.message))?;
        Ok(json!({"partitionKey": key}))
    }

    fn delete_cookies(&mut self, p: &Json) -> Answer {
        let (url, key) = self.partition(p)?;
        let f = &p["filter"];
        let gone: Vec<Json> = self
            .cookies(url.as_deref())?
            .into_iter()
            .filter(|c| cookie_matches(c, f))
            .collect();
        let s = self.contexts.first().map(|c| c.session.clone());
        for c in gone {
            self.call(
                "Network.deleteCookies",
                json!({"name": c["name"], "domain": c["domain"], "path": c["path"]}),
                s.as_deref(),
            )?;
        }
        Ok(json!({"partitionKey": key}))
    }

    /// `pageon:observe` / `pageon:act`: CDP's `Pageon` domain on a context's tab.
    fn pageon(&mut self, method: &str, p: &Json) -> Answer {
        let c = self.top(p)?;
        let session = self.context_of(&c)?.session.clone();
        let mut params = p.clone();
        if let Some(o) = params.as_object_mut() {
            o.remove("context");
        }
        self.call(method, params, Some(&session))
    }
}

/// A CDP `Network.Cookie` as a BiDi `network.Cookie`.
pub(crate) fn cdp_to_bidi(c: &Json) -> Json {
    let mut j = json!({
        "name": c["name"], "value": {"type": "string", "value": c["value"]},
        "domain": c["domain"], "path": c["path"], "size": c["size"],
        "httpOnly": c["httpOnly"], "secure": c["secure"],
        "sameSite": match c["sameSite"].as_str() {
            Some("Strict") => "strict",
            Some("Lax") => "lax",
            Some("None") => "none",
            _ => "default",
        },
    });
    if let Some(e) = c["expires"].as_f64().filter(|e| *e > 0.0) {
        j["expiry"] = json!(e as u64);
    }
    j
}

/// Whether a BiDi cookie matches a `storage.CookieFilter`.
pub(crate) fn cookie_matches(c: &Json, f: &Json) -> bool {
    let Some(f) = f.as_object() else { return true };
    let bare = |d: &Json| {
        d.as_str()
            .map(|d| d.trim_start_matches('.').to_ascii_lowercase())
    };
    f.iter().all(|(k, want)| match k.as_str() {
        "value" => c["value"]["value"] == want["value"],
        // `.shop.test` and `shop.test` name the same domain.
        "domain" => bare(&c["domain"]) == bare(want),
        k => &c[k] == want,
    })
}
