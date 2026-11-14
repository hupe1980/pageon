//! Running a document's scripts while it loads: classic scripts at their parse position, then
//! `defer`/`async` scripts in document order, `DOMContentLoaded`, `load`, and settling on the
//! virtual clock. Everything the scripts fetch goes through the same `Env`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use pageon_dom::{Dom, NodeData, NodeId, ParseOptions, Parser, Step, encoding};
use pageon_env::Env;
use pageon_js::realm::Target;
use pageon_js::{Engine, Limits, PageError, PageErrorKind, RealmConfig, ScriptKind};
use pageon_layout::{Stylesheet, Viewport};
use pageon_net::{Client, Dest, FetchRequest, Net};
use url::Url;

/// What running a document's scripts produced.
pub(crate) struct Ran {
    pub(crate) dom: Dom,
    pub(crate) stylesheets: Vec<Stylesheet>,
    pub(crate) stylesheets_failed: usize,
    pub(crate) errors: Vec<PageError>,
    pub(crate) scripts: usize,
    pub(crate) pending_timers: usize,
    pub(crate) virtual_ms: u64,
    pub(crate) modals: Vec<NodeId>,
    pub(crate) focused: Option<NodeId>,
    pub(crate) scroll: crate::Scroll,
    pub(crate) pending_requests: usize,
    pub(crate) navigation: Option<pageon_js::NavRequest>,
    pub(crate) dialogs: Vec<pageon_js::Dialog>,
    pub(crate) popups: Vec<String>,
    /// The running engine, when kept (tabs).
    pub(crate) live: Option<crate::LiveDoc>,
}

/// A script that runs after parsing, in document order (`defer` classic and module scripts).
enum Deferred {
    Classic(NodeId, Url),
    /// A module script: its URL (`src`, or the document URL for inline text) and inline text.
    Module(NodeId, Url, Option<String>),
}

struct Run<'e> {
    engine: Rc<RefCell<Engine>>,
    realm: usize,
    dom: Rc<RefCell<Dom>>,
    base: Url,
    client: Client,
    env: &'e dyn Env,
    net: Rc<RefCell<Net>>,
    now: u64,
    deferred: Vec<Deferred>,
    inline: usize,
    sheets_seen: HashSet<NodeId>,
    stylesheets: Vec<Stylesheet>,
    stylesheets_failed: usize,
}

impl Run<'_> {
    /// Fetches stylesheets parsed so far (scripts may read styles and geometry).
    fn sync_stylesheets(&mut self) {
        let links: Vec<(NodeId, String)> = {
            let dom = self.dom.borrow();
            dom.traverse(dom.root())
                .filter_map(|e| match e {
                    pageon_dom::Edge::Enter(id) if !self.sheets_seen.contains(&id) => {
                        let n = dom.get(id)?;
                        let rel = n.attr("rel").unwrap_or_default().to_ascii_lowercase();
                        (n.is_html("link")
                            && rel.split_ascii_whitespace().any(|t| t == "stylesheet")
                            && !rel.contains("alternate"))
                        .then(|| (id, n.attr("href").unwrap_or_default().to_string()))
                    }
                    _ => None,
                })
                .collect()
        };
        if links.is_empty() {
            return;
        }
        for (id, href) in links {
            self.sheets_seen.insert(id);
            let Ok(url) = self.base.join(href.trim()) else {
                self.stylesheets_failed += 1;
                continue;
            };
            let req = FetchRequest::subresource(url, Dest::Style, self.client.clone());
            let res = self.net.borrow_mut().fetch(self.env, req, self.now);
            match res {
                Ok(r) if (200..300).contains(&r.status) => {
                    let charset = r
                        .header("content-type")
                        .and_then(encoding::charset_param)
                        .unwrap_or("utf-8");
                    let (css, _) = encoding::decode(&r.body, Some(charset));
                    self.stylesheets.push(Stylesheet { link: id, css });
                }
                _ => self.stylesheets_failed += 1,
            }
        }
        if let Some(r) = self.engine.borrow().realm(self.realm).cloned() {
            let mut st = r.st.borrow_mut();
            st.stylesheets.clone_from(&self.stylesheets);
            st.mutations += 1;
        }
    }

    /// Fetches an external script's text.
    fn fetch_script(&mut self, url: &Url) -> Option<String> {
        let req = FetchRequest::subresource(url.clone(), Dest::Script, self.client.clone());
        let res = self.net.borrow_mut().fetch(self.env, req, self.now);
        match res {
            Ok(r) if (200..300).contains(&r.status) => {
                let charset = r
                    .header("content-type")
                    .and_then(encoding::charset_param)
                    .unwrap_or("utf-8");
                Some(encoding::decode(&r.body, Some(charset)).0)
            }
            Ok(r) => {
                self.engine.borrow_mut().report_in(
                    self.realm,
                    PageErrorKind::Load,
                    format!("script failed to load: HTTP {}", r.status),
                    url.as_str(),
                );
                None
            }
            Err(e) => {
                self.engine.borrow_mut().report_in(
                    self.realm,
                    PageErrorKind::Load,
                    format!("script failed to load: {e}"),
                    url.as_str(),
                );
                None
            }
        }
    }

    /// A script element reached by the parser.
    fn script(&mut self, node: NodeId) {
        let (k, src, deferred, text, started, nomodule) = {
            let dom = self.dom.borrow();
            let Some(n) = dom.get(node) else { return };
            let started = self
                .engine
                .borrow()
                .realm(self.realm)
                .is_some_and(|r| r.st.borrow().started_scripts.contains(&node));
            let text: String = dom
                .children(node)
                .filter_map(|c| match &dom.get(c)?.data {
                    NodeData::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect();
            let src = n.attr("src").map(str::to_string);
            let deferred =
                src.is_some() && (n.attr("defer").is_some() || n.attr("async").is_some());
            (
                pageon_js::script_kind(n),
                src,
                deferred,
                text,
                started,
                n.attr("nomodule").is_some(),
            )
        };
        if started {
            return;
        }
        if let Some(r) = self.engine.borrow().realm(self.realm).cloned() {
            r.st.borrow_mut().started_scripts.insert(node);
        }
        let joined = match src.as_deref().map(|s| self.base.join(s.trim())) {
            Some(Ok(u)) => Some(u),
            Some(Err(_)) => {
                self.engine.borrow_mut().report_in(
                    self.realm,
                    PageErrorKind::Load,
                    "invalid script URL",
                    src.unwrap_or_default(),
                );
                return;
            }
            None => None,
        };
        match k {
            ScriptKind::Data => {}
            // Module-supporting browsers skip `nomodule` scripts (the legacy bundle).
            ScriptKind::Classic if nomodule => {}
            // Module scripts run after parsing, in document order with `defer` scripts.
            ScriptKind::Module => {
                let inline = joined.is_none().then_some(text);
                let url = joined.unwrap_or_else(|| self.base.clone());
                self.deferred.push(Deferred::Module(node, url, inline));
            }
            ScriptKind::ImportMap => match joined {
                Some(url) => self.engine.borrow_mut().report_in(
                    self.realm,
                    PageErrorKind::Unsupported,
                    "External import maps are not supported",
                    url.as_str(),
                ),
                None => self
                    .engine
                    .borrow_mut()
                    .register_import_map(self.realm, &text),
            },
            ScriptKind::Classic => match joined {
                Some(url) => {
                    if deferred {
                        self.deferred.push(Deferred::Classic(node, url));
                    } else {
                        self.sync_stylesheets();
                        if let Some(code) = self.fetch_script(&url) {
                            self.engine.borrow_mut().run_script(
                                self.realm,
                                &code,
                                url.as_str(),
                                Some(node),
                            );
                        }
                    }
                }
                None => {
                    self.inline += 1;
                    self.sync_stylesheets();
                    let name = format!("<inline #{}>", self.inline);
                    self.engine
                        .borrow_mut()
                        .run_script(self.realm, &text, &name, Some(node));
                }
            },
        }
        // Scripts this one inserted, and imports it started, run before parsing goes on.
        self.engine.borrow_mut().run_scripts(self.env);
    }
}

/// The settings of a document's realm (its `crypto` key: 256 bits through the seam, the OS
/// live, the recording in replay, separate from `Math.random`'s seed).
fn config(
    client: &Client,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    (url, origin): (&str, &str),
    accept_dialogs: bool,
) -> Result<RealmConfig, pageon_env::NetError> {
    let mut crypto_key = [0u8; 32];
    for chunk in crypto_key.chunks_mut(8) {
        chunk.copy_from_slice(&env.random_u64()?.to_le_bytes());
    }
    Ok(RealmConfig {
        url: url.to_string(),
        origin: origin.to_string(),
        navigator: env.profile().navigator(),
        viewport: Viewport::default(),
        stylesheets: Vec::new(),
        client: client.clone(),
        net: net.clone(),
        accept_dialogs,
        crypto_key,
    })
}

/// Adds a document's realm: to the engine its origin has in the page, or to a new engine
/// (which the page then keeps for that origin). Places it in the page's frame tree.
fn realm(
    dom: &Rc<RefCell<Dom>>,
    client: &Client,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    accept_dialogs: bool,
    start: &crate::load::Start<'_>,
    (url, origin): (&str, &str),
) -> Result<(Rc<RefCell<Engine>>, usize), pageon_env::NetError> {
    let engine = match start.page.and_then(|(p, _)| p.engine(origin)) {
        Some(e) => e,
        None => {
            let now = env.now_ms()?;
            let seed = env.random_u64()?;
            let e = Rc::new(RefCell::new(Engine::new(now, seed, Limits::default())));
            if let Some((p, _)) = start.page {
                p.engines.borrow_mut().insert(origin.to_string(), e.clone());
            }
            e
        }
    };
    let cfg = config(client, env, net, (url, origin), accept_dialogs)?;
    let realm = engine.borrow_mut().add_realm(dom.clone(), cfg);
    if let Some((p, context)) = start.page {
        let id = engine.borrow().id();
        p.frames.borrow_mut().run_by(context, id, realm);
        engine
            .borrow_mut()
            .set_frame(realm, p.frames.clone(), context);
    }
    engine
        .borrow_mut()
        .set_history_state(realm, start.history.cloned());
    Ok((engine, realm))
}

/// Whether any element has an inline event handler (`onload="…"`).
fn has_inline_handlers(dom: &Dom) -> bool {
    dom.traverse(dom.root()).any(|e| match e {
        pageon_dom::Edge::Enter(n) => dom
            .get(n)
            .is_some_and(|x| x.attrs().iter().any(|a| a.local_name().starts_with("on"))),
        pageon_dom::Edge::Leave(_) => false,
    })
}

/// The URL and origin of a document: its client's, or what it inherits (`about:srcdoc`).
fn identity(client: &Client, start: &crate::load::Start<'_>) -> (String, String) {
    match start.inherit {
        Some((url, origin)) => (url.to_string(), origin.to_string()),
        None => (
            client.url.to_string(),
            crate::frames::origin_of(client.url.as_str()),
        ),
    }
}

/// Parses `text` as the document at `url`, running its scripts, then settles. A page without
/// scripts or inline handlers never starts the JavaScript engine, unless its origin already
/// runs one in the page (its frames and windows can then reach it). The running engine is
/// returned for the page's frames and tabs.
pub(crate) fn run(
    text: &str,
    client: &Client,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    now: u64,
    options: crate::LoadOptions,
    start: crate::load::Start<'_>,
) -> Result<Ran, pageon_env::NetError> {
    let settle_ms = options.settle_ms;
    let accept = options.dialogs == crate::DialogPolicy::Accept;
    let url = &client.url;
    let (doc_url, origin) = identity(client, &start);
    if let Some((p, context)) = start.page
        && let Some(e) = p.frames.borrow_mut().entries.get_mut(context as usize)
    {
        e.origin.clone_from(&origin);
    }
    let mut parser = Parser::new(ParseOptions { scripting: true });
    let dom = parser.dom();
    parser.feed(text);
    let begin = |dom: &Rc<RefCell<Dom>>| -> Result<Run<'_>, pageon_env::NetError> {
        let (engine, realm) = realm(dom, client, env, net, accept, &start, (&doc_url, &origin))?;
        Ok(Run {
            engine,
            realm,
            dom: dom.clone(),
            base: url.clone(),
            client: client.clone(),
            env,
            net: net.clone(),
            now,
            deferred: Vec::new(),
            inline: 0,
            sheets_seen: HashSet::new(),
            stylesheets: Vec::new(),
            stylesheets_failed: 0,
        })
    };
    let joins = start.page.is_some_and(|(p, _)| p.engine(&origin).is_some());
    let mut run: Option<Run<'_>> = if joins { Some(begin(&dom)?) } else { None };
    while let Step::Script(node) = parser.step() {
        if run.is_none() {
            run = Some(begin(&dom)?);
        }
        if let Some(r) = run.as_mut() {
            // Custom elements parsed since the last script are constructed now.
            r.engine.borrow_mut().upgrade_custom_elements(r.realm);
            r.script(node);
        }
    }
    drop(parser.finish());
    if let Some(r) = run.as_mut() {
        r.engine.borrow_mut().upgrade_custom_elements(r.realm);
    }
    if run.is_none() && has_inline_handlers(&dom.borrow()) {
        run = Some(begin(&dom)?);
    }
    let Some(mut r) = run else {
        let dom =
            Rc::try_unwrap(dom).map_or_else(|shared| shared.borrow().clone(), RefCell::into_inner);
        let (stylesheets, stylesheets_failed) =
            crate::load::fetch_stylesheets(&dom, client, env, net, now);
        return Ok(Ran {
            dom,
            stylesheets,
            stylesheets_failed,
            errors: Vec::new(),
            scripts: 0,
            pending_timers: 0,
            virtual_ms: 0,
            modals: Vec::new(),
            focused: None,
            scroll: crate::Scroll::default(),
            pending_requests: 0,
            navigation: None,
            dialogs: Vec::new(),
            popups: Vec::new(),
            live: None,
        });
    };
    let realm = r.realm;
    r.sync_stylesheets();
    let engine = r.engine.clone();
    engine.borrow_mut().set_ready_state(realm, "interactive");
    for d in std::mem::take(&mut r.deferred) {
        match d {
            Deferred::Classic(node, url) => {
                if let Some(code) = r.fetch_script(&url) {
                    engine
                        .borrow_mut()
                        .run_script(realm, &code, url.as_str(), Some(node));
                }
            }
            Deferred::Module(node, url, inline) => {
                r.sync_stylesheets();
                engine
                    .borrow_mut()
                    .run_module(realm, &url, inline.as_deref(), Some(node), env);
            }
        }
        engine.borrow_mut().run_scripts(env);
    }
    let root = dom.borrow().root();
    let mut e = engine.borrow_mut();
    e.fire(realm, Target::Node(root), "DOMContentLoaded", true);
    e.set_ready_state(realm, "complete");
    e.fire(realm, Target::Window, "load", false);
    // A URL with a fragment opens scrolled to its target.
    if let Some(f) = url.fragment() {
        e.scroll_to_fragment(realm, f);
    }
    let settled = e.settle(settle_ms as f64, env);
    let scroll = crate::Scroll::of(&e, realm);
    let (modals, focused) = e
        .realm(realm)
        .map(|x| {
            let st = x.st.borrow();
            (st.modals.clone(), st.focused)
        })
        .unwrap_or_default();
    let errors = e.errors_of(realm);
    let scripts = e.scripts_run();
    let (navigation, dialogs, popups) = e.take_requests(realm);
    drop(e);
    let Run {
        stylesheets,
        stylesheets_failed,
        ..
    } = r;
    let snapshot = dom.borrow().clone();
    Ok(Ran {
        dom: snapshot,
        stylesheets,
        stylesheets_failed,
        errors,
        scripts,
        pending_timers: settled.pending,
        virtual_ms: settled.virtual_ms,
        modals,
        focused,
        scroll,
        pending_requests: settled.pending_requests,
        navigation,
        dialogs,
        popups,
        live: Some(crate::LiveDoc { engine, realm, dom }),
    })
}

/// Starts a running engine for a document loaded without one (no scripts), so a tab can act
/// on it, or adds its realm to the engine its origin runs in the page.
pub(crate) fn start_live(
    doc: &crate::Document,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    options: crate::LoadOptions,
    page: Option<&crate::frames::Page>,
) -> Result<crate::LiveDoc, pageon_env::NetError> {
    let Some(client) = doc
        .client
        .clone()
        .or_else(|| Url::parse(&doc.final_url).ok().map(|u| Client::top(&u)))
    else {
        return Err(pageon_env::NetError::Io(format!(
            "{} is not a URL",
            doc.final_url
        )));
    };
    let dom = Rc::new(RefCell::new(doc.dom.clone()));
    let accept = options.dialogs == crate::DialogPolicy::Accept;
    let origin = page
        .and_then(|p| {
            p.frames
                .borrow()
                .entries
                .get(doc.context as usize)
                .map(|e| e.origin.clone())
        })
        .unwrap_or_else(|| crate::frames::origin_of(client.url.as_str()));
    let start = crate::load::Start {
        page: page.map(|p| (p, doc.context)),
        ..Default::default()
    };
    let (engine, realm) = realm(
        &dom,
        &client,
        env,
        net,
        accept,
        &start,
        (&doc.final_url, &origin),
    )?;
    if let Some(r) = engine.borrow().realm(realm) {
        let mut st = r.st.borrow_mut();
        st.stylesheets.clone_from(&doc.stylesheets);
        st.ready_state = "complete";
        st.mutations += 1;
    }
    Ok(crate::LiveDoc { engine, realm, dom })
}
