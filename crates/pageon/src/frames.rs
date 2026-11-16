//! Frames: `<iframe>` documents loaded with their page (`src` through the same `Env`, `srcdoc`
//! inline), breadth-first, within depth and count limits. A frame that cannot be loaded is kept
//! as failed, with the reason, never silently dropped.

use std::cell::RefCell;
use std::rc::Rc;

use pageon_dom::{Edge, NodeId};
use pageon_env::Env;
use pageon_net::{Client, Net};
use url::Url;

use crate::{Document, LoadOptions};

/// A page's frames: the tree its engines share (who embeds whom, origins, mailbox) and its
/// engines by origin, so documents of one origin become realms of one engine.
#[derive(Default)]
pub(crate) struct Page {
    pub(crate) frames: Rc<RefCell<pageon_js::frames::Frames>>,
    pub(crate) engines: RefCell<std::collections::HashMap<String, Rc<RefCell<pageon_js::Engine>>>>,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("frames", &self.frames.borrow().entries.len())
            .finish_non_exhaustive()
    }
}

/// Rounds of message delivery between engines after one action.
pub(crate) const MAX_PUMP: usize = 16;

impl Page {
    /// A page whose top-level document is being loaded.
    pub(crate) fn new() -> Self {
        Self {
            frames: pageon_js::frames::Frames::new("null"),
            engines: RefCell::default(),
        }
    }

    /// The engine of an origin, if one runs in the page.
    pub(crate) fn engine(&self, origin: &str) -> Option<Rc<RefCell<pageon_js::Engine>>> {
        self.engines.borrow().get(origin).cloned()
    }

    /// Delivers messages between engines: every engine with mail settles (within `window`
    /// ms), until none has mail or [`MAX_PUMP`] rounds have run.
    pub(crate) fn pump(&self, env: &dyn Env, window: f64) {
        for _ in 0..MAX_PUMP {
            self.frames.borrow_mut().drop_undeliverable();
            let engines: Vec<_> = self.engines.borrow().values().cloned().collect();
            let mut any = false;
            for e in engines {
                if e.borrow().has_mail() {
                    any = true;
                    e.borrow_mut().settle(window, env);
                }
            }
            if !any {
                break;
            }
        }
    }
}

/// Deepest frame nesting loaded (the top-level document is depth 0).
pub const MAX_DEPTH: usize = 3;
/// Most frames loaded per top-level page.
pub const MAX_FRAMES: usize = 16;

/// Whether a frame's document is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameState {
    /// Loaded.
    Loaded,
    /// Not loaded, and why.
    Failed(String),
}

/// An `<iframe>` and its document.
#[derive(Debug, Clone)]
pub struct Frame {
    pub(crate) owner: NodeId,
    pub(crate) url: String,
    pub(crate) origin: String,
    pub(crate) state: FrameState,
    pub(crate) document: Option<Box<Document>>,
}

impl Frame {
    /// The `<iframe>` element in the parent document.
    pub fn owner(&self) -> NodeId {
        self.owner
    }

    /// The frame's URL (`about:srcdoc` for `srcdoc`, `about:blank` without a source).
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The frame's origin (`scheme://host[:port]`); `srcdoc` and `about:blank` frames have
    /// their parent's origin.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Whether it loaded.
    pub fn state(&self) -> &FrameState {
        &self.state
    }

    /// The frame's document, if it loaded.
    pub fn document(&self) -> Option<&Document> {
        self.document.as_deref()
    }
}

/// The origin of a URL, as the views print it.
pub(crate) fn origin_of(url: &str) -> String {
    Url::parse(url)
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|_| "null".into())
}

/// The `<iframe>` elements of a document, in document order (not inside `<template>`).
fn iframes(doc: &Document) -> Vec<NodeId> {
    let dom = doc.dom();
    dom.traverse(dom.root())
        .filter_map(|e| match e {
            Edge::Enter(id) if dom.get(id).is_some_and(|n| n.is_html("iframe")) => Some(id),
            _ => None,
        })
        .collect()
}

/// Loads the frames of `root` and of its frames, level by level. With a `page` (a load whose
/// scripts run), each frame is placed in the page's frame tree and its scripts run in the
/// engine of its origin; afterwards scriptless documents of an origin that runs scripts join
/// its engine, `load` fires at each loaded `<iframe>`, and messages are delivered.
pub(crate) fn load_all(
    root: &mut Document,
    env: Option<&dyn Env>,
    net: &Rc<RefCell<Net>>,
    options: LoadOptions,
    page: Option<&Page>,
) {
    let mut count = 0;
    // Each document with its ancestor URLs (to refuse recursive frames), the origin and base
    // URL it has (inherited by `srcdoc` and `about:blank` documents), and the client its frames
    // are requested by.
    let origin = origin_of(root.final_url());
    let base = Url::parse(root.final_url()).ok();
    let client = base.as_ref().map(Client::top);
    type Level<'d> = Vec<(
        &'d mut Document,
        Vec<String>,
        String,
        Option<Url>,
        Option<Client>,
    )>;
    let mut level: Level<'_> = vec![(root, Vec::new(), origin, base, client)];
    for depth in 1..=MAX_DEPTH + 1 {
        let mut next = Vec::new();
        for (doc, ancestors, parent_origin, base, client) in level {
            let mut chain = ancestors.clone();
            chain.push(doc.final_url().to_string());
            let parent_context = doc.context;
            let mut frames = Vec::new();
            for owner in iframes(doc) {
                // A frame a script already gave its `about:blank` document (the engine made
                // it as the script asked) is not loaded again.
                if page.is_some_and(|p| {
                    p.frames
                        .borrow()
                        .entries
                        .iter()
                        .any(|e| e.owner == Some(owner) && e.parent == Some(parent_context))
                }) {
                    continue;
                }
                frames.push(load_one(
                    doc,
                    owner,
                    &Parent {
                        origin: &parent_origin,
                        base: base.as_ref(),
                        client: client.as_ref(),
                        chain: &chain,
                        context: parent_context,
                    },
                    depth,
                    &mut count,
                    Loading {
                        env,
                        net,
                        options: &options,
                        page,
                    },
                ));
            }
            doc.frames = frames;
            let sandboxes: Vec<Option<String>> = doc
                .frames
                .iter()
                .map(|f| {
                    doc.dom()
                        .get(f.owner)
                        .and_then(|n| n.attr("sandbox"))
                        .map(str::to_string)
                })
                .collect();
            for (f, sandbox) in doc.frames.iter_mut().zip(sandboxes) {
                let inherits = f.url.starts_with("about:");
                let child_base = if inherits {
                    base.clone()
                } else {
                    Url::parse(&f.url).ok()
                };
                let child_origin = f.origin.clone();
                let child_client = if inherits {
                    client.clone()
                } else {
                    match (&client, &child_base) {
                        (Some(c), Some(u)) => Some(c.frame(u)),
                        _ => None,
                    }
                }
                .map(|c| c.sandboxed(sandbox.as_deref()));
                if let Some(child) = f.document.as_deref_mut() {
                    next.push((child, chain.clone(), child_origin, child_base, child_client));
                }
            }
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
}

/// Where a frame is loaded from: its parent's origin, base URL, client, ancestor URLs and
/// context.
struct Parent<'a> {
    origin: &'a String,
    base: Option<&'a Url>,
    client: Option<&'a Client>,
    chain: &'a [String],
    context: u32,
}

/// What loading frames needs.
#[derive(Clone, Copy)]
struct Loading<'a> {
    env: Option<&'a dyn Env>,
    net: &'a Rc<RefCell<Net>>,
    options: &'a LoadOptions,
    page: Option<&'a Page>,
}

/// Loads the frame of one `<iframe>` of `doc` (`src` through the network, `srcdoc` and
/// `about:blank` inline), within the depth and count limits.
fn load_one(
    doc: &Document,
    owner: NodeId,
    p: &Parent<'_>,
    depth: usize,
    count: &mut usize,
    loading: Loading<'_>,
) -> Frame {
    let (parent_origin, base, client, chain, parent_context) =
        (p.origin, p.base, p.client, p.chain, p.context);
    let Loading {
        env,
        net,
        options,
        page,
    } = loading;
    let options = options.clone();
    let attr = |k: &str| {
        doc.dom()
            .get(owner)
            .and_then(|n| n.attr(k))
            .map(str::to_string)
    };
    let fail = |url: String, why: &str| Frame {
        owner,
        origin: if url.starts_with("about:") {
            parent_origin.clone()
        } else {
            origin_of(&url)
        },
        url,
        state: FrameState::Failed(why.to_string()),
        document: None,
    };
    let srcdoc = attr("srcdoc");
    let src = attr("src")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let resolved = src.as_deref().map(|s| {
        base.as_ref()
            .and_then(|b| b.join(s).ok())
            .map(|u| u.to_string())
    });
    let url = match (&srcdoc, &resolved) {
        (Some(_), _) => "about:srcdoc".to_string(),
        (None, Some(Some(u))) => u.clone(),
        (None, Some(None)) => src.clone().unwrap_or_default(),
        (None, None) => "about:blank".to_string(),
    };
    if depth > MAX_DEPTH {
        return fail(url, "frame nesting limit");
    }
    if *count >= MAX_FRAMES {
        return fail(url, "frame limit");
    }
    *count += 1;
    let inherits = srcdoc.is_some() || url == "about:blank";
    let sandbox = attr("sandbox");
    // A sandbox without `allow-same-origin` (its own or an ancestor's): opaque.
    let opaque = client.is_some_and(|c| c.clone().sandboxed(sandbox.as_deref()).opaque());
    // The frame's place in the page's tree, while it loads (removed if it fails).
    // A frame a script already gave a window keeps its place (its `about:blank` document
    // is replaced by the one loaded now).
    let mut added = false;
    let context = page.map(|p| {
        let entry = pageon_js::frames::FrameEntry {
            parent: Some(parent_context),
            owner: Some(owner),
            origin: if opaque {
                "null".into()
            } else if inherits {
                parent_origin.clone()
            } else {
                origin_of(&url)
            },
            engine: 0,
            realm: None,
        };
        let mut f = p.frames.borrow_mut();
        let at = f
            .entries
            .iter()
            .position(|e| e.owner == Some(owner) && e.parent == Some(parent_context));
        match at {
            Some(i) => {
                f.entries[i] = entry;
                u32::try_from(i).unwrap_or(u32::MAX)
            }
            None => {
                added = true;
                f.add(entry)
            }
        }
    });
    let start = crate::load::Start {
        parent: client,
        page: page.zip(context),
        sandbox: sandbox.as_deref(),
        ..Default::default()
    };
    let loaded = if inherits {
        let html = srcdoc.clone().unwrap_or_default();
        match (env, &client, page) {
            (Some(env), Some(parent), Some(_)) if options.scripts => crate::load::inline(
                &html,
                &url,
                parent_origin,
                parent,
                env,
                net,
                options.clone(),
                start,
            )
            .map_err(|e| e.to_string()),
            _ => Ok(crate::parse_without_frames(&html, &url)),
        }
    } else if chain.contains(&url) {
        Err("recursive frame".to_string())
    } else if !(url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("file:"))
    {
        Err(format!("unsupported frame URL {url}"))
    } else if let (Some(env), Some(client)) = (env, &client) {
        let delegate_tools = attr("allow").is_some_and(|a| {
            a.split(';')
                .any(|d| d.split_whitespace().next() == Some("tools"))
        });
        let start = crate::load::Start {
            parent: Some(client),
            delegate_tools,
            ..start
        };
        match crate::load::fetch(&url, env, net, options.clone(), start) {
            Ok(crate::load::Loaded::Page(d)) => Ok(*d),
            Ok(crate::load::Loaded::Download(_)) => Err("a download".to_string()),
            Err(e) => Err(e.to_string()),
        }
    } else {
        Err("no environment to load frames".to_string())
    };
    if loaded.is_err()
        && added
        && let Some(p) = page
    {
        p.frames.borrow_mut().entries.pop();
    }
    match loaded {
        Ok(mut child) => {
            child.context = context.unwrap_or(0);
            Frame {
                owner,
                origin: if opaque {
                    "null".into()
                } else if inherits {
                    parent_origin.clone()
                } else {
                    origin_of(child.final_url())
                },
                url: if inherits {
                    url
                } else {
                    child.final_url().to_string()
                },
                state: FrameState::Loaded,
                document: Some(Box::new(child)),
            }
        }
        Err(why) => fail(url, &why),
    }
}

/// After a page's frames have loaded: scriptless documents of an origin that runs scripts
/// join its engine (so its other documents reach them), `load` fires at every loaded
/// `<iframe>` (deepest first), and the engines settle and deliver their messages.
pub(crate) fn connect(
    root: &mut Document,
    page: &Page,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    options: LoadOptions,
) {
    if root.frames.is_empty() {
        return;
    }
    join(root, page, env, net, options.clone());
    fire_loads(root);
    let engines: Vec<_> = page.engines.borrow().values().cloned().collect();
    for e in engines {
        e.borrow_mut().settle(options.settle_ms as f64, env);
    }
    page.pump(env, options.settle_ms as f64);
    crate::tab::refresh(root);
}

/// Loads the frames of the `<iframe>`s scripts added to the top-level document after it loaded
/// (with a `src` or `srcdoc`; `about:blank` ones get their window from the engine), as
/// browsers load them after the script: each joins the page's frame tree (taking the place of
/// its `about:blank` window), its scripts run, and `load` fires at its `<iframe>`. `true` if
/// any was loaded.
pub(crate) fn load_inserted(
    root: &mut Document,
    page: &Page,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    options: &LoadOptions,
) -> bool {
    // The document as the scripts left it.
    crate::tab::refresh(root);
    let have: std::collections::HashSet<NodeId> = root.frames.iter().map(|f| f.owner).collect();
    let wanted: Vec<NodeId> = iframes(root)
        .into_iter()
        .filter(|o| !have.contains(o))
        .filter(|&o| {
            root.dom().get(o).is_some_and(|n| {
                n.attr("srcdoc").is_some()
                    || n.attr("src").is_some_and(|s| {
                        let s = s.trim();
                        !s.is_empty() && !s.eq_ignore_ascii_case("about:blank")
                    })
            })
        })
        .collect();
    if wanted.is_empty() {
        return false;
    }
    let origin = origin_of(root.final_url());
    let base = Url::parse(root.final_url()).ok();
    let client = base.as_ref().map(Client::top);
    let chain = vec![root.final_url().to_string()];
    let mut count = root.frames.len();
    let mut loaded = Vec::new();
    for owner in wanted {
        let frame = load_one(
            root,
            owner,
            &Parent {
                origin: &origin,
                base: base.as_ref(),
                client: client.as_ref(),
                chain: &chain,
                context: root.context,
            },
            1,
            &mut count,
            Loading {
                env: Some(env),
                net,
                options,
                page: Some(page),
            },
        );
        if frame.state == FrameState::Loaded {
            loaded.push(owner);
        }
        root.frames.push(frame);
    }
    join(root, page, env, net, options.clone());
    if let Some(live) = &root.live {
        for owner in &loaded {
            live.engine.borrow_mut().fire(
                live.realm,
                pageon_js::Target::Node(*owner),
                "load",
                false,
            );
        }
    }
    let engines: Vec<_> = page.engines.borrow().values().cloned().collect();
    for e in engines {
        e.borrow_mut().settle(options.settle_ms as f64, env);
    }
    page.pump(env, options.settle_ms as f64);
    crate::tab::refresh(root);
    true
}

fn join(
    doc: &mut Document,
    page: &Page,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    options: LoadOptions,
) {
    if doc.live.is_none() {
        let origin = page
            .frames
            .borrow()
            .entries
            .get(doc.context as usize)
            .map(|e| e.origin.clone());
        if origin.is_some_and(|o| page.engine(&o).is_some())
            && let Ok(live) = crate::scripts::start_live(doc, env, net, options.clone(), Some(page))
        {
            doc.live = Some(Rc::new(live));
        }
    }
    for f in &mut doc.frames {
        if let Some(d) = f.document.as_deref_mut() {
            join(d, page, env, net, options.clone());
        }
    }
}

/// Fires the window `load` each document held back for its frames, deepest first, once they
/// have loaded (or failed), with `readyState` complete.
pub(crate) fn finish_loads(doc: &mut Document) {
    for f in &mut doc.frames {
        if let Some(d) = f.document.as_deref_mut() {
            finish_loads(d);
        }
    }
    if let Some(live) = &doc.live
        && live.load_pending.replace(false)
    {
        let mut e = live.engine.borrow_mut();
        e.set_ready_state(live.realm, "complete");
        e.fire(live.realm, pageon_js::Target::Window, "load", false);
    }
}

fn fire_loads(doc: &mut Document) {
    for f in &mut doc.frames {
        if let Some(d) = f.document.as_deref_mut() {
            fire_loads(d);
        }
    }
    if let Some(live) = &doc.live {
        for f in &doc.frames {
            if f.state == FrameState::Loaded {
                live.engine.borrow_mut().fire(
                    live.realm,
                    pageon_js::Target::Node(f.owner),
                    "load",
                    false,
                );
            }
        }
    }
}
