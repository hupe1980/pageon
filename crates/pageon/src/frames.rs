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

/// Loads the frames of `root` and of its frames, level by level.
pub(crate) fn load_all(
    root: &mut Document,
    env: Option<&dyn Env>,
    net: &Rc<RefCell<Net>>,
    options: LoadOptions,
    keep: bool,
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
            let mut frames = Vec::new();
            for owner in iframes(doc) {
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
                    frames.push(fail(url, "frame nesting limit"));
                    continue;
                }
                if count >= MAX_FRAMES {
                    frames.push(fail(url, "frame limit"));
                    continue;
                }
                count += 1;
                let loaded = if let Some(html) = srcdoc {
                    Ok(crate::parse_without_frames(&html, "about:srcdoc"))
                } else if url == "about:blank" {
                    Ok(crate::parse_without_frames("", "about:blank"))
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
                        keep,
                        delegate_tools,
                        ..Default::default()
                    };
                    match crate::load::fetch(&url, env, net, options, start) {
                        Ok(crate::load::Loaded::Page(d)) => Ok(*d),
                        Ok(crate::load::Loaded::Download(_)) => Err("a download".to_string()),
                        Err(e) => Err(e.to_string()),
                    }
                } else {
                    Err("no environment to load frames".to_string())
                };
                frames.push(match loaded {
                    Ok(child) => Frame {
                        owner,
                        origin: if url.starts_with("about:") {
                            parent_origin.clone()
                        } else {
                            origin_of(child.final_url())
                        },
                        url: if url.starts_with("about:") {
                            url
                        } else {
                            child.final_url().to_string()
                        },
                        state: FrameState::Loaded,
                        document: Some(Box::new(child)),
                    },
                    Err(why) => fail(url, &why),
                });
            }
            doc.frames = frames;
            for f in &mut doc.frames {
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
                };
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
