//! `Page`, `Input`, `Network`, `Emulation`, `Pageon`: navigation and its events, input at
//! viewport points, cookies, and the agent view.

use pageon_env::Env;
use pageon_net::{Cookie, Dest, SameSite};
use serde_json::{Value as Json, json};

use super::{CdpError, Reply, Session};
use crate::{Action, ActionError, Outcome};

/// Virtual time a keystroke lets pass (debounced handlers, `setTimeout(0)` effects).
const KEY_SETTLE_MS: u64 = 50;

/// Methods that change nothing in pageon (state Chrome keeps for its DevTools front end).
const HARMLESS: &[&str] = &[
    "Page.enable",
    "Page.disable",
    "Page.bringToFront",
    "Page.setBypassCSP",
    "Page.stopLoading",
    "Page.setInterceptFileChooserDialog",
    "Page.setAdBlockingEnabled",
    "Network.disable",
    "Network.setCacheDisabled",
    "Network.setBypassServiceWorker",
    "Emulation.setDeviceMetricsOverride",
    "Emulation.clearDeviceMetricsOverride",
    "Emulation.setTouchEmulationEnabled",
    "Emulation.setFocusEmulationEnabled",
    "Emulation.setDefaultBackgroundColorOverride",
    "Audits.enable",
    "Audits.disable",
    "Performance.enable",
    "Performance.disable",
    "Log.enable",
    "Log.disable",
    "WebMCP.enable",
    "Inspector.enable",
    "Page.setFontFamilies",
    "Emulation.setEmulatedMedia",
    "Browser.setDownloadBehavior",
    "Security.enable",
    "ServiceWorker.enable",
    "ServiceWorker.disable",
    "Target.setAutoAttach",
    "Target.setDiscoverTargets",
];

/// Methods pageon does not have yet, and what to use instead.
const UNSUPPORTED: &[(&str, &str)] = &[
    (
        "Page.printToPDF",
        "pageon does not print to PDF; take a screenshot (Page.captureScreenshot, fullPage), or read the page with page.content()",
    ),
    (
        "Page.startScreencast",
        "pageon paints on demand and does not stream; take screenshots (Page.captureScreenshot) when you need one",
    ),
    (
        "Fetch.enable",
        "request interception is not supported; replay a recorded session with `pageon cdp --replay`, or limit where requests go with --origin",
    ),
    (
        "Network.setRequestInterception",
        "request interception is not supported; replay a recorded session with `pageon cdp --replay`",
    ),
    (
        "Network.setUserAgentOverride",
        "pageon identifies itself as an agent; for a coherent Chrome identity start pageon with --profile browser-compatible",
    ),
    (
        "Emulation.setUserAgentOverride",
        "pageon identifies itself as an agent; for a coherent Chrome identity start pageon with --profile browser-compatible",
    ),
    (
        "Page.addScriptToEvaluateOnNewDocument",
        "scripts that run before the page's own scripts are not supported yet; call page.evaluate() after page.goto()",
    ),
    (
        "Network.setExtraHTTPHeaders",
        "extra request headers are not supported yet",
    ),
];

impl<E: Env> Session<'_, E> {
    /// A lifecycle event for every frame with a document (each with its own loader).
    fn lifecycle(&mut self, ti: usize, name: &str) {
        for fi in 0..self.targets[ti].frames.len() {
            if self.targets[ti].frames[fi].loaded {
                self.lifecycle_of(ti, fi, name);
            }
        }
    }

    fn lifecycle_of(&mut self, ti: usize, fi: usize, name: &str) {
        if self.targets[ti].lifecycle {
            let f = &self.targets[ti].frames[fi];
            let (frame, loader) = (f.id.clone(), f.loader.clone());
            let ts = self.timestamp();
            self.target_event(
                ti,
                "Page.lifecycleEvent",
                json!({"frameId": frame, "loaderId": loader, "name": name, "timestamp": ts}),
            );
        }
    }

    /// A new document replaced the old one: its frames, contexts and objects are gone.
    pub(super) fn begin_document(&mut self, ti: usize) {
        self.detach_frames(ti);
        let n = self.next_id();
        let t = &mut self.targets[ti];
        t.epoch = n;
        t.loader = super::hex(0x10_AD00, n);
        t.objects.clear();
        t.groups.clear();
        t.nodes.clear();
        t.backends.clear();
        let frame = t.id.clone();
        if t.page {
            self.target_event(ti, "Page.frameStartedLoading", json!({"frameId": frame}));
        }
    }

    /// The new document's events, in Chrome's order.
    pub(super) fn finish_document(&mut self, ti: usize) {
        if self.targets[ti].runtime {
            self.target_event(ti, "Runtime.executionContextsCleared", json!({}));
        }
        self.rebuild_frames(ti);
        self.lifecycle_of(ti, 0, "init");
        if self.targets[ti].page {
            let frame = self.frame_json(ti, 0);
            self.target_event(
                ti,
                "Page.frameNavigated",
                json!({"frame": frame, "type": "Navigation"}),
            );
        }
        self.attach_frames(ti);
        if self.discover {
            let info = self.target_info(ti);
            self.event(
                "Target.targetInfoChanged",
                json!({"targetInfo": info}),
                None,
            );
        }
        self.new_contexts(ti);
        self.announce_contexts(ti);
        let ts = self.timestamp();
        self.lifecycle(ti, "commit");
        let page = self.targets[ti].page;
        if page {
            self.target_event(ti, "Page.domContentEventFired", json!({"timestamp": ts}));
        }
        self.lifecycle(ti, "DOMContentLoaded");
        if page {
            self.target_event(ti, "Page.loadEventFired", json!({"timestamp": ts}));
        }
        self.lifecycle(ti, "load");
        if page {
            let frame = self.targets[ti].id.clone();
            self.target_event(ti, "Page.frameStoppedLoading", json!({"frameId": frame}));
        }
        self.lifecycle(ti, "networkAlmostIdle");
        self.lifecycle(ti, "networkIdle");
    }

    /// `Network.*` events for the requests an action or a load made.
    pub(super) fn network_events(&mut self, ti: usize, out: &Outcome, navigated: bool) {
        if !self.targets[ti].network {
            return;
        }
        let ts = self.timestamp();
        let (frame, loader, document) = {
            let t = &self.targets[ti];
            (t.id.clone(), t.loader.clone(), t.tab.url().to_string())
        };
        let mut document_seen = false;
        for e in &out.requests {
            // WebSockets are not requests in Chrome's Network domain (they have their own
            // `webSocket*` events, not served yet).
            if e.dest == Dest::WebSocket {
                continue;
            }
            // The navigation's own request shares the loader's id (Puppeteer ties `goto`'s
            // response to it).
            let is_doc = navigated && !document_seen && e.dest == Dest::Document;
            let request_id = if is_doc {
                document_seen = true;
                loader.clone()
            } else {
                self.next_request += 1;
                format!("{}.{}", ti + 1, self.next_request)
            };
            let ty = match e.dest {
                Dest::Document | Dest::Frame => "Document",
                Dest::Script => "Script",
                Dest::Style => "Stylesheet",
                Dest::Image => "Image",
                Dest::Fetch => "Fetch",
                Dest::Xhr => "XHR",
                Dest::EventSource => "EventSource",
                Dest::Robots | Dest::WebSocket => "Other",
            };
            self.target_event(
                ti,
                "Network.requestWillBeSent",
                json!({
                    "requestId": request_id, "loaderId": loader, "documentURL": document,
                    "request": {
                        "url": e.url, "method": e.method, "headers": {},
                        "mixedContentType": "none", "initialPriority": "High",
                        "referrerPolicy": "strict-origin-when-cross-origin",
                    },
                    "timestamp": ts, "wallTime": ts, "initiator": {"type": "other"},
                    "redirectHasExtraInfo": false, "type": ty, "frameId": frame,
                    "hasUserGesture": false,
                }),
            );
            match (&e.outcome, e.status) {
                (pageon_net::Outcome::Ok, Some(status)) => {
                    let url = e.final_url.clone().unwrap_or_else(|| e.url.clone());
                    self.target_event(
                        ti,
                        "Network.responseReceived",
                        json!({
                            "requestId": request_id, "loaderId": loader, "timestamp": ts, "type": ty,
                            "response": {
                                "url": url, "status": status, "statusText": "", "headers": {},
                                "mimeType": if ty == "Document" { "text/html" } else { "" },
                                "connectionReused": false, "connectionId": 0,
                                "remoteIPAddress": "", "remotePort": 0,
                                "fromDiskCache": e.from_cache, "fromServiceWorker": false,
                                "fromPrefetchCache": false, "encodedDataLength": 0,
                                "responseTime": ts * 1000.0, "protocol": "http/1.1",
                                "securityState": "unknown",
                            },
                            "hasExtraInfo": false, "frameId": frame,
                        }),
                    );
                    self.target_event(
                        ti,
                        "Network.loadingFinished",
                        json!({"requestId": request_id, "timestamp": ts, "encodedDataLength": 0}),
                    );
                }
                (outcome, _) => {
                    let blocked = matches!(outcome, pageon_net::Outcome::Blocked(_));
                    let mut ev = json!({
                        "requestId": request_id, "timestamp": ts, "type": ty,
                        "errorText": if blocked { "net::ERR_BLOCKED_BY_CLIENT" } else { "net::ERR_FAILED" },
                        "canceled": false,
                    });
                    if blocked {
                        ev["blockedReason"] = json!("other");
                    }
                    self.target_event(ti, "Network.loadingFailed", ev);
                }
            }
        }
    }

    /// Runs an action on a target and sends what it caused. Low-level input settles briefly
    /// (a keystroke takes no agent step): pointer moves only run what is due now, keys run
    /// what falls due within 50 ms, and clicks, navigation and `Pageon.act` settle like an
    /// action.
    fn act(&mut self, ti: usize, action: Action) -> Result<Outcome, ActionError> {
        let window = match action {
            Action::HoverAt(..) => Some(0),
            Action::Press(_) => Some(KEY_SETTLE_MS),
            _ => None,
        };
        let tab = &mut self.targets[ti].tab;
        let r = match window {
            Some(w) => tab.act_within(action, w),
            None => tab.act(action),
        };
        if let Ok(out) = &r {
            self.after_outcome(ti, out);
        }
        r
    }

    /// An agent's step (`Pageon.act`): settles like any action.
    fn agent_act(&mut self, ti: usize, action: Action) -> Result<Outcome, ActionError> {
        let r = self.targets[ti].tab.act(action);
        if let Ok(out) = &r {
            self.after_outcome(ti, out);
        }
        r
    }

    /// `Page`, `Input`, `Network`, `Emulation`, `Pageon` and the rest.
    pub(super) fn page_domain(&mut self, ti: usize, method: &str, params: &Json) -> Reply {
        let s = |k: &str| {
            params
                .get(k)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let num = |k: &str| params.get(k).and_then(Json::as_f64).unwrap_or(0.0);
        match method {
            "Page.enable" => {
                self.targets[ti].page = true;
                Ok(json!({}))
            }
            "Network.enable" => {
                self.targets[ti].network = true;
                Ok(json!({}))
            }
            "Page.setLifecycleEventsEnabled" => {
                let on = params
                    .get("enabled")
                    .and_then(Json::as_bool)
                    .unwrap_or(false);
                self.targets[ti].lifecycle = on;
                if on {
                    // The current document is loaded: report its lifecycle so far.
                    for name in [
                        "init",
                        "commit",
                        "DOMContentLoaded",
                        "load",
                        "networkAlmostIdle",
                        "networkIdle",
                    ] {
                        self.lifecycle(ti, name);
                    }
                }
                Ok(json!({}))
            }
            "Page.getLayoutMetrics" => {
                // The viewport at its scroll position; the content size is the document's
                // scrollable size.
                let v = crate::Viewport::default();
                let (w, h) = (f64::from(v.width), f64::from(v.height));
                let l = self.targets[ti].tab.document().layout(v);
                let (cw, ch) = l.document_size();
                let (x, y) = l.viewport_scroll();
                let layout = json!({"pageX": x, "pageY": y, "clientWidth": w, "clientHeight": h});
                let visual = json!({"offsetX": 0, "offsetY": 0, "pageX": x, "pageY": y,
                    "clientWidth": w, "clientHeight": h, "scale": 1, "zoom": 1});
                let content = json!({"x": 0, "y": 0, "width": cw, "height": ch});
                Ok(json!({
                    "layoutViewport": layout, "visualViewport": visual, "contentSize": content,
                    "cssLayoutViewport": layout, "cssVisualViewport": visual, "cssContentSize": content,
                }))
            }
            "Page.captureScreenshot" => {
                let format = match params.get("format").and_then(Json::as_str) {
                    None | Some("png") => crate::Format::Png,
                    Some("jpeg") => crate::Format::Jpeg {
                        quality: params
                            .get("quality")
                            .and_then(Json::as_u64)
                            .map_or(80, |q| q.min(100) as u8),
                    },
                    Some(other) => {
                        let mut err = CdpError::server(format!(
                            "pageon does not encode {other}; use format png or jpeg"
                        ));
                        err.data =
                            Some(json!({"kind": "unsupported", "guidance": "use png or jpeg"}));
                        return Err(err);
                    }
                };
                let (area, scale) = match params.get("clip") {
                    Some(c) => {
                        let f = |k: &str| c.get(k).and_then(Json::as_f64).unwrap_or(0.0) as f32;
                        let scale = c.get("scale").and_then(Json::as_f64).unwrap_or(1.0) as f32;
                        (
                            crate::Area::Clip(crate::Rect {
                                x: f("x"),
                                y: f("y"),
                                width: f("width"),
                                height: f("height"),
                            }),
                            scale,
                        )
                    }
                    // Without a clip, Chrome captures the whole page when asked to go beyond the
                    // viewport (Puppeteer's `fullPage`).
                    None if params
                        .get("captureBeyondViewport")
                        .and_then(Json::as_bool)
                        .unwrap_or(false) =>
                    {
                        (crate::Area::FullPage, 1.0)
                    }
                    None => (crate::Area::Viewport, 1.0),
                };
                let o = crate::ScreenshotOptions {
                    area,
                    scale,
                    format,
                    ..crate::ScreenshotOptions::default()
                };
                let img = self.targets[ti].tab.screenshot(&o).map_err(paint_error)?;
                Ok(json!({"data": b64(&img.bytes)}))
            }
            "Pageon.screenshot" => {
                let flag = |k: &str| params.get(k).and_then(Json::as_bool).unwrap_or(false);
                let format = match params.get("format").and_then(Json::as_str) {
                    Some("jpeg") => crate::Format::Jpeg { quality: 80 },
                    _ => crate::Format::Png,
                };
                let t = &mut self.targets[ti].tab;
                let img = match params.get("ref").and_then(Json::as_str) {
                    Some(r) => t.crop(r, 8.0, format, 1.0),
                    None => t.screenshot(&crate::ScreenshotOptions {
                        area: if flag("fullPage") {
                            crate::Area::FullPage
                        } else {
                            crate::Area::Viewport
                        },
                        format,
                        marks: flag("marks"),
                        mask: flag("mask"),
                        ..crate::ScreenshotOptions::default()
                    }),
                }
                .map_err(paint_error)?;
                Ok(json!({
                    "data": b64(&img.bytes), "width": img.width, "height": img.height,
                    "tokens": img.tokens, "notes": img.notes, "legend": img.legend,
                    "provenance": img.provenance,
                }))
            }
            "Page.getFrameTree" => Ok(json!({"frameTree": self.frame_tree(ti, 0)})),
            "Page.navigate" => {
                let url = s("url");
                if url.is_empty() {
                    return Err(CdpError::params("url: string value expected"));
                }
                let frame = self.targets[ti].id.clone();
                match self.act(ti, Action::Goto(url)) {
                    Ok(out) if out.downloads.is_empty() => {
                        Ok(json!({"frameId": frame, "loaderId": self.targets[ti].loader}))
                    }
                    Ok(_) => Ok(
                        json!({"frameId": frame, "loaderId": self.targets[ti].loader, "isDownload": true}),
                    ),
                    Err(e) => {
                        Ok(json!({"frameId": frame, "errorText": format!("net::ERR_FAILED ({e})")}))
                    }
                }
            }
            "Page.reload" => {
                let url = self.targets[ti].tab.url().to_string();
                self.act(ti, Action::Goto(url))
                    .map_err(|e| CdpError::server(e.to_string()))?;
                Ok(json!({}))
            }
            "Page.getNavigationHistory" => {
                let (urls, index) = self.targets[ti].tab.history();
                let entries: Vec<Json> = urls
                    .iter()
                    .enumerate()
                    .map(|(i, u)| json!({"id": i, "url": u, "userTypedURL": u, "title": "", "transitionType": "typed"}))
                    .collect();
                Ok(json!({"currentIndex": index, "entries": entries}))
            }
            "Page.navigateToHistoryEntry" => {
                let want = params.get("entryId").and_then(Json::as_u64).unwrap_or(0) as usize;
                let (urls, index) = self.targets[ti].tab.history();
                if want >= urls.len() {
                    return Err(CdpError::server("No entry with passed id"));
                }
                let action = if want < index {
                    Action::Back
                } else {
                    Action::Forward
                };
                for _ in 0..want.abs_diff(index) {
                    self.act(ti, action.clone())
                        .map_err(|e| CdpError::server(e.to_string()))?;
                }
                Ok(json!({}))
            }
            "Page.createIsolatedWorld" => {
                let name = s("worldName");
                let frame = match params.get("frameId").and_then(Json::as_str) {
                    Some(id) => self
                        .frame_index(ti, id)
                        .ok_or_else(|| CdpError::server("No frame for given id found"))?,
                    None => 0,
                };
                if !self.targets[ti].frames[frame].loaded {
                    return Err(CdpError::server("Frame has no document"));
                }
                if !self.targets[ti].worlds.contains(&name) {
                    self.targets[ti].worlds.push(name.clone());
                }
                if let Some(c) = self.targets[ti]
                    .contexts
                    .iter()
                    .find(|c| c.name == name && c.frame == frame)
                {
                    return Ok(json!({"executionContextId": c.id}));
                }
                self.next_context += 1;
                let id = self.next_context;
                let c = super::Ctx {
                    id,
                    unique: format!("{}.{id}", self.targets[ti].epoch),
                    name,
                    frame,
                };
                self.targets[ti].contexts.push(c.clone());
                if self.targets[ti].runtime {
                    let ctx = self.context_json(ti, &c);
                    self.target_event(
                        ti,
                        "Runtime.executionContextCreated",
                        json!({"context": ctx}),
                    );
                }
                Ok(json!({"executionContextId": id}))
            }
            "Page.addScriptToEvaluateOnNewDocument" => {
                let source = s("source");
                let world = s("worldName");
                if !world.is_empty() && !self.targets[ti].worlds.contains(&world) {
                    self.targets[ti].worlds.push(world.clone());
                }
                let n = self.next_id().to_string();
                // Puppeteer registers its utility world with an empty script: nothing to run.
                let code = source
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .any(|l| !l.trim().is_empty());
                if code {
                    let script = crate::load::InitScript {
                        id: n.clone(),
                        source,
                        world: (!world.is_empty()).then_some(world),
                    };
                    if params.get("runImmediately").and_then(Json::as_bool) == Some(true) {
                        self.run_init_now(ti, &script);
                    }
                    self.targets[ti].tab.init_scripts_mut().push(script);
                }
                Ok(json!({"identifier": n}))
            }
            "Page.removeScriptToEvaluateOnNewDocument" => {
                let id = s("identifier");
                let scripts = self.targets[ti].tab.init_scripts_mut();
                let before = scripts.len();
                scripts.retain(|x| x.id != id);
                // Puppeteer's world registrations have no script to remove.
                let _ = before;
                Ok(json!({}))
            }
            "Network.setExtraHTTPHeaders" => {
                let empty = params
                    .get("headers")
                    .and_then(Json::as_object)
                    .is_none_or(serde_json::Map::is_empty);
                if empty {
                    Ok(json!({}))
                } else {
                    Err(unsupported(method))
                }
            }
            "Network.getCookies" => {
                let urls: Vec<String> = match params.get("urls").and_then(Json::as_array) {
                    Some(a) => a
                        .iter()
                        .filter_map(Json::as_str)
                        .map(str::to_string)
                        .collect(),
                    None => vec![self.targets[ti].tab.url().to_string()],
                };
                Ok(json!({"cookies": cookies_json(self.session, Some(&urls))}))
            }
            "Network.setCookies" => {
                set_cookies(self.session, params, Some(self.targets[ti].tab.url()))
            }
            "Network.setCookie" => {
                let wrapped = json!({"cookies": [params]});
                set_cookies(self.session, &wrapped, Some(self.targets[ti].tab.url()))?;
                Ok(json!({"success": true}))
            }
            "Network.deleteCookies" => {
                let name = s("name");
                let url = params
                    .get("url")
                    .and_then(Json::as_str)
                    .and_then(|u| url::Url::parse(u).ok());
                let domain = params
                    .get("domain")
                    .and_then(Json::as_str)
                    .map(|d| d.trim_start_matches('.').to_string());
                let path = params
                    .get("path")
                    .and_then(Json::as_str)
                    .map(str::to_string);
                let all = self.session.cookies();
                let keep: Vec<Cookie> = all
                    .into_iter()
                    .filter(|c| {
                        let hit = c.name == name
                            && url.as_ref().is_none_or(|u| cookie_matches(c, u))
                            && domain
                                .as_ref()
                                .is_none_or(|d| c.domain.eq_ignore_ascii_case(d))
                            && path.as_ref().is_none_or(|p| c.path == *p);
                        !hit
                    })
                    .collect();
                let mut net = self.session.net().borrow_mut();
                net.clear_cookies();
                net.add_cookies(&keep);
                Ok(json!({}))
            }
            "Network.clearBrowserCookies" => {
                self.session.net().borrow_mut().clear_cookies();
                Ok(json!({}))
            }
            "Input.dispatchMouseEvent" => {
                let (x, y) = (num("x"), num("y"));
                match s("type").as_str() {
                    "mouseMoved" => match self.targets[ti].pressed {
                        None => {
                            let _ = self.act(ti, Action::HoverAt(x, y));
                        }
                        // A move with the button held: the press happens now, then the
                        // move is a drag step (HTML drag and drop on a draggable element).
                        Some((p, moved)) => {
                            if !moved {
                                self.targets[ti].pressed = Some((p, true));
                                let _ = self.act(ti, Action::PointerDown(p.0, p.1));
                            }
                            if let Ok(out) = self.act(ti, Action::HoverAt(x, y))
                                && let Some(items) = out.drag.and_then(|d| d.intercepted)
                            {
                                let items: Vec<Json> = items
                                    .into_iter()
                                    .map(|(t, d)| json!({"mimeType": t, "data": d}))
                                    .collect();
                                self.target_event(
                                    ti,
                                    "Input.dragIntercepted",
                                    json!({"data": {"items": items, "dragOperationsMask": 19}}),
                                );
                            }
                        }
                    },
                    "mousePressed" => self.targets[ti].pressed = Some(((x, y), false)),
                    "mouseWheel" => {
                        // The wheel lands where the pointer is: move it there first.
                        let _ = self.act(ti, Action::HoverAt(x, y));
                        let _ = self.act(
                            ti,
                            Action::Scroll {
                                target: None,
                                by: crate::ScrollBy::Pixels(num("deltaX"), num("deltaY")),
                            },
                        );
                    }
                    "mouseReleased" => {
                        let was = self.targets[ti].pressed.take();
                        let left = matches!(s("button").as_str(), "left" | "");
                        match was {
                            // Pressed and released without a move: a click, as before.
                            Some((_, false)) if left => {
                                let _ = self.act(ti, Action::ClickAt(x, y));
                            }
                            Some((_, true)) => {
                                let _ = self.act(ti, Action::PointerUp(x, y));
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                Ok(json!({}))
            }
            "Input.setInterceptDrags" => {
                let on = params
                    .get("enabled")
                    .and_then(Json::as_bool)
                    .unwrap_or(false);
                self.targets[ti].tab.set_intercept_drags(on);
                Ok(json!({}))
            }
            "Input.dispatchDragEvent" => {
                let kind = match s("type").as_str() {
                    "dragEnter" => crate::DragKind::Enter,
                    "dragOver" => crate::DragKind::Over,
                    "drop" => crate::DragKind::Drop,
                    "dragCancel" => crate::DragKind::Cancel,
                    other => {
                        return Err(CdpError::params(format!(
                            "Input.dispatchDragEvent: unknown type {other:?} (dragEnter, dragOver, drop or dragCancel)"
                        )));
                    }
                };
                let items: Vec<(String, String)> = params
                    .pointer("/data/items")
                    .and_then(Json::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|i| {
                                (
                                    i["mimeType"].as_str().unwrap_or_default().to_string(),
                                    i["data"].as_str().unwrap_or_default().to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let (x, y) = (num("x"), num("y"));
                let _ = self.act(ti, Action::DragEvent { kind, x, y, items });
                if matches!(kind, crate::DragKind::Drop | crate::DragKind::Cancel) {
                    self.targets[ti].pressed = None;
                }
                Ok(json!({}))
            }
            "Input.dispatchKeyEvent" => {
                let ty = s("type");
                if matches!(ty.as_str(), "keyDown" | "rawKeyDown") {
                    let key = s("key");
                    let text = s("text");
                    let key = match key.as_str() {
                        "Shift" | "Control" | "Alt" | "Meta" | "CapsLock" => return Ok(json!({})),
                        "" | "Unidentified" if !text.is_empty() => text,
                        _ => key,
                    };
                    if !key.is_empty() {
                        let _ = self.act(ti, Action::Press(key));
                    }
                } else if ty == "char" && params.get("key").is_none() {
                    // A bare character (keyboard.sendCharacter).
                    for c in s("text").chars() {
                        let _ = self.act(ti, Action::Press(c.to_string()));
                    }
                }
                Ok(json!({}))
            }
            "Input.insertText" => {
                for c in s("text").chars() {
                    let key = if c == '\n' {
                        "Enter".to_string()
                    } else {
                        c.to_string()
                    };
                    let _ = self.act(ti, Action::Press(key));
                }
                Ok(json!({}))
            }
            "Pageon.observe" => {
                let full = params.get("full").and_then(Json::as_bool).unwrap_or(false);
                let t = &mut self.targets[ti].tab;
                let observation = if full { t.observe_full() } else { t.observe() };
                Ok(json!({"observation": observation}))
            }
            "Pageon.browsergym" => Ok(self.targets[ti].tab.browsergym()),
            "Pageon.act" => {
                // The action text (`click e5`), or its structured form (`{"Type": ["e3", "a \"b\""]}`,
                // lossless for values with quotes or several options).
                let parsed: Result<Action, String> = match params.get("action") {
                    Some(Json::String(t)) => t.parse(),
                    Some(v) => {
                        serde_json::from_value(v.clone()).map_err(|e| format!("not an action: {e}"))
                    }
                    None => {
                        Err("Pageon.act needs an action, e.g. {\"action\": \"click e5\"}".into())
                    }
                };
                let action = parsed.map_err(|e: String| {
                    let mut err = CdpError::server(e.clone());
                    err.data = Some(json!({"kind": "invalid_action", "guidance": e}));
                    err
                })?;
                match self.agent_act(ti, action) {
                    Ok(out) => Ok(json!({"outcome": out.to_string(), "data": out})),
                    Err(e) => {
                        // The guidance is in the message too: some clients drop error data.
                        let mut err = CdpError::server(format!("{e}; {}", e.guidance()));
                        err.data = Some(
                            json!({"kind": crate::mcp::kind_of(&e), "guidance": e.guidance()}),
                        );
                        Err(err)
                    }
                }
            }
            m if HARMLESS.contains(&m) => Ok(json!({})),
            m if UNSUPPORTED.iter().any(|(u, _)| *u == m) => Err(unsupported(m)),
            m => Err(super::unknown(m)),
        }
    }
}

fn paint_error(e: crate::PaintError) -> CdpError {
    let mut err = CdpError::server(e.to_string());
    err.data = Some(json!({"kind": e.kind(), "guidance": e.guidance()}));
    err
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unsupported(method: &str) -> CdpError {
    let instead = UNSUPPORTED
        .iter()
        .find(|(m, _)| *m == method)
        .map_or("not available in pageon", |(_, i)| i);
    CdpError::unsupported(method, instead)
}

fn cookie_matches(c: &Cookie, u: &url::Url) -> bool {
    let host = u.host_str().unwrap_or_default();
    let domain_ok = if c.host_only {
        host.eq_ignore_ascii_case(&c.domain)
    } else {
        host.eq_ignore_ascii_case(&c.domain)
            || host
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", c.domain.to_ascii_lowercase()))
    };
    let path = u.path();
    let path_ok = path == c.path
        || (path.starts_with(&c.path)
            && (c.path.ends_with('/') || path[c.path.len()..].starts_with('/')));
    domain_ok && path_ok && (!c.secure || u.scheme() == "https")
}

/// A cookie as a CDP `Network.Cookie`.
pub(crate) fn cookie_to_cdp(c: &Cookie) -> Json {
    json!({
        "name": c.name,
        "value": c.value,
        "domain": if c.host_only { c.domain.clone() } else { format!(".{}", c.domain) },
        "path": c.path,
        "expires": c.expires.map_or(-1.0, |ms| ms as f64 / 1000.0),
        "size": c.name.len() + c.value.len(),
        "httpOnly": c.http_only,
        "secure": c.secure,
        "session": c.expires.is_none(),
        "sameSite": match c.same_site { SameSite::Strict => "Strict", SameSite::Lax => "Lax", SameSite::None => "None" },
        "priority": "Medium",
        "sameParty": false,
        "sourceScheme": if c.secure { "Secure" } else { "NonSecure" },
        "sourcePort": if c.secure { 443 } else { 80 },
    })
}

/// Whether a cookie applies to a URL (domain, path, `Secure`).
pub(crate) fn applies(c: &Cookie, u: &url::Url) -> bool {
    cookie_matches(c, u)
}

/// The session's cookies as CDP `Network.Cookie`s (those that apply to `urls`, if given).
pub(super) fn cookies_json<E: Env>(
    session: &crate::Session<E>,
    urls: Option<&[String]>,
) -> Vec<Json> {
    let parsed: Option<Vec<url::Url>> =
        urls.map(|u| u.iter().filter_map(|x| url::Url::parse(x).ok()).collect());
    session
        .cookies()
        .into_iter()
        .filter(|c| {
            parsed
                .as_ref()
                .is_none_or(|us| us.iter().any(|u| cookie_matches(c, u)))
        })
        .map(|c| cookie_to_cdp(&c))
        .collect()
}

/// A CDP `Network.CookieParam` as a cookie: it needs a `url` (or the page's) or a `domain`.
pub(crate) fn cdp_to_cookie(c: &Json, page: Option<&str>, now: u64) -> Result<Cookie, String> {
    let s = |k: &str| c.get(k).and_then(Json::as_str).map(str::to_string);
    let url = s("url")
        .or_else(|| page.map(str::to_string))
        .and_then(|u| url::Url::parse(&u).ok());
    let (domain, host_only) = match (s("domain"), &url) {
        (Some(d), _) => (d.trim_start_matches('.').to_ascii_lowercase(), false),
        (None, Some(u)) => (u.host_str().unwrap_or_default().to_ascii_lowercase(), true),
        (None, None) => return Err("a cookie needs a url or a domain".into()),
    };
    let secure = c
        .get("secure")
        .and_then(Json::as_bool)
        .unwrap_or_else(|| url.as_ref().is_some_and(|u| u.scheme() == "https"));
    Ok(Cookie {
        name: s("name").unwrap_or_default(),
        value: s("value").unwrap_or_default(),
        domain,
        host_only,
        path: s("path").unwrap_or_else(|| "/".into()),
        expires: c
            .get("expires")
            .and_then(Json::as_f64)
            .filter(|e| *e > 0.0)
            .map(|e| (e * 1000.0) as u64),
        secure,
        http_only: c.get("httpOnly").and_then(Json::as_bool).unwrap_or(false),
        same_site: match s("sameSite").as_deref() {
            Some("Strict") => SameSite::Strict,
            Some("None") => SameSite::None,
            _ => SameSite::Lax,
        },
        partition_key: None,
        created: now,
    })
}

/// `Network.setCookies` / `Storage.setCookies`: each cookie needs a `url` or a `domain`.
pub(super) fn set_cookies<E: Env>(
    session: &crate::Session<E>,
    params: &Json,
    page: Option<&str>,
) -> Reply {
    let mut out = Vec::new();
    for c in params
        .get("cookies")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        out.push(cdp_to_cookie(c, page, session.now_ms()).map_err(|e| CdpError::params(&e))?);
    }
    session.add_cookies(&out);
    Ok(json!({}))
}
