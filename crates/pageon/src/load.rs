//! Loading a page: URL → (redirects) → bytes → encoding → document, all through one `Env`.

use std::cell::RefCell;
use std::rc::Rc;

use pageon_dom::{NodeData, ParseOptions, encoding};
use pageon_env::Env;
use pageon_net::{Client, Dest, FetchRequest, Net};
use url::Url;

use crate::{Document, Encoding, LoadError, Summary};

/// Options of a load.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct LoadOptions {
    /// Maximum redirects followed.
    pub max_redirects: usize,
    /// Run the page's scripts.
    pub scripts: bool,
    /// Virtual time (ms) a page may settle for after `load`: timers due later stay pending.
    pub settle_ms: u64,
    /// Allow requests to the private network (loopback, `localhost`, private and link-local
    /// addresses). Off by default: a page cannot reach the services next to the agent.
    pub allow_private_network: bool,
    /// How `alert`/`confirm`/`prompt` are answered (they never block).
    pub dialogs: DialogPolicy,
    /// Respect robots.txt (RFC 9309, product token `pageon`) for the tab's top-level
    /// documents. On by default; turn it off for a session a person directs.
    pub robots: bool,
    /// Let the caller open `file:` URLs as top-level pages. On by default (an operator naming a
    /// file); the agent-facing servers turn it off unless started with `--allow-files`, so a
    /// hijacked agent cannot open the disk. Web pages never load `file:` URLs either way.
    pub allow_files: bool,
    /// Client code run in every new document before its own scripts (CDP
    /// `Page.addScriptToEvaluateOnNewDocument`, BiDi `script.addPreloadScript`), in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub init_scripts: Vec<InitScript>,
}

/// A script a protocol client registered to run in every new document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InitScript {
    /// The registration's identifier.
    pub id: String,
    /// JavaScript source.
    pub source: String,
    /// The isolated world it runs in (`None`: the page's main world, before its scripts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub world: Option<String>,
}

/// How page dialogs are answered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DialogPolicy {
    /// `confirm` → `false`, `prompt` → `null` (as Playwright does by default).
    #[default]
    Dismiss,
    /// `confirm` → `true`, `prompt` → its default value.
    Accept,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            max_redirects: 20,
            scripts: true,
            settle_ms: 1000,
            allow_private_network: false,
            dialogs: DialogPolicy::Dismiss,
            robots: true,
            allow_files: true,
            init_scripts: Vec::new(),
        }
    }
}

fn to_url(input: &str) -> Result<Url, LoadError> {
    let invalid = |reason: String| LoadError::InvalidUrl {
        input: input.to_string(),
        reason,
    };
    if let Ok(u) = Url::parse(input) {
        // A Windows drive letter or a scheme-looking path is not a URL scheme.
        if u.scheme().len() > 1 {
            return match u.scheme() {
                "http" | "https" | "file" => Ok(u),
                "about" if u.as_str() == "about:blank" => Ok(u),
                s => Err(LoadError::UnsupportedScheme(s.to_string())),
            };
        }
    }
    if input.contains("://") {
        return Err(invalid("malformed URL".into()));
    }
    let path = std::path::absolute(input).map_err(|e| invalid(e.to_string()))?;
    Url::from_file_path(&path).map_err(|()| invalid("not an absolute path".into()))
}

/// Loads a page from a URL (`http`, `https`, `file`) or a local path, through `env`, with its
/// stylesheets, scripts and frames (see [`crate::Frame`]), in a fresh [`crate::Session`].
pub fn load(input: &str, env: &dyn Env, options: LoadOptions) -> Result<Document, LoadError> {
    crate::Session::new(env).load(input, options)
}

/// How a document is requested.
#[derive(Clone, Copy, Default)]
pub(crate) struct Start<'a> {
    /// The embedding document (frames).
    pub(crate) parent: Option<&'a Client>,
    /// The page that navigated (link, form, `location`) and what it asked for.
    pub(crate) initiator: Option<(&'a Client, &'a pageon_js::NavRequest)>,
    /// Keep the scripts running afterwards (tabs).
    pub(crate) keep: bool,
    /// The frame's `<iframe allow>` delegates "tools" (WebMCP).
    pub(crate) delegate_tools: bool,
    /// The history state of the entry being restored (a traversal to another document).
    pub(crate) history: Option<&'a pageon_js::SavedState>,
    /// The page the document belongs to, and its context in the page's frame tree.
    pub(crate) page: Option<(&'a crate::frames::Page, u32)>,
    /// The URL and origin of a document that inherits its origin (`about:srcdoc`).
    pub(crate) inherit: Option<(&'a str, &'a str)>,
}

/// A navigation's result: a page, or a file to download.
pub(crate) enum Loaded {
    Page(Box<Document>),
    Download(crate::Download),
}

/// Script navigations followed while one page loads (client-side redirects).
pub(crate) const MAX_SCRIPT_REDIRECTS: usize = 5;

/// Loads a top-level page with its frames, following navigations its scripts ask for while
/// loading (up to [`MAX_SCRIPT_REDIRECTS`]).
pub(crate) fn open(
    input: &str,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    options: LoadOptions,
    start: Start<'_>,
) -> Result<Loaded, LoadError> {
    let mut page = crate::frames::Page::new();
    let first = Start {
        page: Some((&page, 0)),
        ..start
    };
    let mut loaded = fetch(input, env, net, options.clone(), first)?;
    let mut chain = vec![input.to_string()];
    loop {
        let Loaded::Page(doc) = &mut loaded else {
            return Ok(loaded);
        };
        let Some(nav) = doc.js.navigation.take() else {
            break;
        };
        chain.push(nav.url.clone());
        if chain.len() > MAX_SCRIPT_REDIRECTS + 1 {
            return Err(LoadError::TooManyRedirects(chain));
        }
        let Some(client) = doc.client.clone() else {
            break;
        };
        page = crate::frames::Page::new();
        let next = Start {
            initiator: Some((&client, &nav)),
            page: Some((&page, 0)),
            ..start
        };
        loaded = fetch(&nav.url, env, net, options.clone(), next)?;
    }
    if let Loaded::Page(doc) = &mut loaded {
        crate::frames::load_all(doc, Some(env), net, options.clone(), Some(&page));
        crate::frames::connect(doc, &page, env, net, options);
        if start.keep {
            doc.page = Some(Rc::new(page));
        } else {
            // A load that is not kept: the documents stay as they are now, without engines.
            strip(doc);
        }
    }
    Ok(loaded)
}

fn strip(doc: &mut Document) {
    doc.live = None;
    for f in &mut doc.frames {
        if let Some(d) = f.document.as_deref_mut() {
            strip(d);
        }
    }
}

/// A frame document given inline (`srcdoc`, `about:blank`): no request; its scripts run with
/// its parent's origin, as one more realm of that origin's engine when the page runs one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn inline(
    html: &str,
    url: &str,
    origin: &str,
    parent: &Client,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    options: LoadOptions,
    start: Start<'_>,
) -> Result<Document, LoadError> {
    let started = env.now_ms()?;
    let ran = crate::scripts::run(
        html,
        parent,
        env,
        net,
        started,
        options,
        Start {
            inherit: Some((url, origin)),
            ..start
        },
    )?;
    let end = env.now_ms()?;
    let mut doc = crate::parse_without_frames("", url);
    doc.summary = Summary {
        bytes: html.len(),
        ms: end.saturating_sub(started),
        stylesheets: ran.stylesheets.len(),
        stylesheets_failed: ran.stylesheets_failed,
        scripts: ran.scripts,
        errors: ran.errors.len(),
        pending_timers: ran.pending_timers,
        virtual_ms: ran.virtual_ms,
        pending_requests: ran.pending_requests,
        ..Summary::default()
    };
    doc.js = crate::JsState {
        errors: ran.errors,
        scripts: ran.scripts,
        pending_timers: ran.pending_timers,
        virtual_ms: ran.virtual_ms,
        modals: ran.modals,
        drop_targets: ran.drop_targets,
        focused: ran.focused,
        scroll: ran.scroll,
        pending_requests: ran.pending_requests,
        navigation: ran.navigation,
        dialogs: ran.dialogs,
        popups: ran.popups,
    };
    doc.dom = ran.dom;
    doc.stylesheets = ran.stylesheets;
    doc.loaded_at = end;
    doc.client = Some(parent.clone());
    doc.live = ran.live.map(Rc::new);
    doc.context = start.page.map_or(0, |(_, c)| c);
    Ok(doc)
}

/// The file name of a download: `Content-Disposition`'s `filename*`/`filename`, else the URL's
/// last path segment; without control characters or path separators, at most 120 characters.
fn download_name(disposition: Option<&str>, url: &Url) -> String {
    let name: String = raw_download_name(disposition, url)
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\'))
        .take(120)
        .collect();
    match name.trim() {
        "" | "." | ".." => "download".into(),
        n => n.into(),
    }
}

fn raw_download_name(disposition: Option<&str>, url: &Url) -> String {
    let param = |name: &str| {
        disposition?.split(';').find_map(|p| {
            let (k, v) = p.split_once('=')?;
            k.trim()
                .eq_ignore_ascii_case(name)
                .then(|| v.trim().trim_matches('"').to_string())
        })
    };
    if let Some(v) = param("filename*") {
        let raw = v.rsplit("''").next().unwrap_or(&v);
        return url::form_urlencoded::parse(format!("x={}", raw.replace('+', "%2B")).as_bytes())
            .next()
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
    }
    if let Some(v) = param("filename").filter(|v| !v.is_empty()) {
        return v;
    }
    url.path_segments()
        .and_then(|mut s| s.next_back().map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "download".into())
}

/// Loads one document with its stylesheets and scripts, without frames.
pub(crate) fn fetch(
    input: &str,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    options: LoadOptions,
    start: Start<'_>,
) -> Result<Loaded, LoadError> {
    let started = env.now_ms()?;
    let url = to_url(input)?;
    if url.scheme() == "file"
        && !options.allow_files
        && start.parent.is_none()
        && start.initiator.is_none()
    {
        return Err(LoadError::LocalFilesOff(url.to_string()));
    }
    let mut req = match (start.parent, start.initiator) {
        (Some(p), _) => FetchRequest::subresource(url, Dest::Frame, p.clone()),
        (None, Some((client, nav))) => {
            let mut r = FetchRequest::navigate(url);
            r.method.clone_from(&nav.method);
            r.body.clone_from(&nav.body);
            if let Some(ct) = &nav.content_type {
                r.headers.push(("content-type".into(), ct.clone()));
            }
            r.client = Some(client.clone());
            r
        }
        (None, None) => FetchRequest::navigate(url),
    };
    req.max_redirects = options.max_redirects;
    // `about:blank` is an empty document, made without a request.
    let response = if req.url.as_str() == "about:blank" {
        pageon_net::Fetched {
            kind: pageon_net::Kind::Basic,
            status: 200,
            status_text: "OK".into(),
            headers: vec![("content-type".into(), "text/html".into())],
            body: Vec::new(),
            url: req.url.clone(),
            redirected: false,
            chain: vec![req.url.to_string()],
        }
    } else {
        net.borrow_mut().fetch(env, req, started)?
    };
    let url = response.url.clone();
    let chain = response.chain.clone();
    let client = match start.parent {
        None => Client::top(&url),
        Some(p) => {
            let mut c = p.frame(&url);
            c.tools |= p.tools && start.delegate_tools;
            c
        }
    };
    let content_type = response
        .header("content-type")
        .unwrap_or("text/html")
        .to_string();
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let html = matches!(essence.as_str(), "text/html" | "application/xhtml+xml" | "");
    let disposition = response.header("content-disposition");
    let attachment = disposition.is_some_and(|d| {
        d.trim_start()
            .to_ascii_lowercase()
            .starts_with("attachment")
    });
    if attachment || (!html && essence != "text/plain") {
        if start.parent.is_none() && url.scheme() != "file" {
            return Ok(Loaded::Download(crate::Download {
                handle: String::new(),
                filename: download_name(disposition, &url),
                url: url.to_string(),
                content_type: essence,
                bytes: response.body,
            }));
        }
        return Err(LoadError::UnsupportedContentType(essence));
    }
    let charset = encoding::charset_param(&content_type);
    let (text, enc) = encoding::decode(&response.body, charset);
    let mut js = crate::JsState::default();
    let mut live = None;
    let (dom, stylesheets, failed) = if html && options.scripts {
        let ran = crate::scripts::run(&text, &client, env, net, started, options, start)?;
        js = crate::JsState {
            errors: ran.errors,
            scripts: ran.scripts,
            pending_timers: ran.pending_timers,
            virtual_ms: ran.virtual_ms,
            modals: ran.modals,
            drop_targets: ran.drop_targets,
            focused: ran.focused,
            scroll: ran.scroll,
            pending_requests: ran.pending_requests,
            navigation: ran.navigation,
            dialogs: ran.dialogs,
            popups: ran.popups,
        };
        live = ran.live.map(Rc::new);
        (ran.dom, ran.stylesheets, ran.stylesheets_failed)
    } else {
        let dom = if html {
            pageon_dom::parse_document(&text, ParseOptions::default())
        } else {
            plain_text(&text)
        };
        let (sheets, failed) = fetch_stylesheets(&dom, &client, env, net, started);
        (dom, sheets, failed)
    };
    let end = env.now_ms()?;
    let refusal = if start.parent.is_none() {
        pageon_net::refusal::classify(response.status, &response.headers, &response.body, end)
    } else {
        None
    };
    Ok(Loaded::Page(Box::new(Document {
        dom,
        final_url: url.to_string(),
        status: response.status,
        content_type: if essence.is_empty() {
            "text/html".into()
        } else {
            essence
        },
        encoding: Encoding {
            name: enc.name,
            source: enc.source.as_str(),
        },
        summary: Summary {
            bytes: response.body.len(),
            ms: end.saturating_sub(started),
            redirects: chain.len() - 1,
            stylesheets: stylesheets.len(),
            stylesheets_failed: failed,
            scripts: js.scripts,
            errors: js.errors.len(),
            pending_timers: js.pending_timers,
            virtual_ms: js.virtual_ms,
            pending_requests: js.pending_requests,
            ..Summary::default()
        },
        stylesheets,
        frames: Vec::new(),
        nav: crate::next_nav(),
        js,
        requests: Vec::new(),
        refusal,
        loaded_at: end,
        client: Some(client),
        live,
        images: None,
        page: None,
        context: start.page.map_or(0, |(_, c)| c),
    })))
}

/// Fetches `<link rel=stylesheet href>` in document order through `net` (recorded and
/// replayed with the page). Failures are counted, never fatal.
pub(crate) fn fetch_stylesheets(
    dom: &pageon_dom::Dom,
    client: &Client,
    env: &dyn Env,
    net: &Rc<RefCell<Net>>,
    now: u64,
) -> (Vec<pageon_layout::Stylesheet>, usize) {
    let base = &client.url;
    let mut sheets = Vec::new();
    let mut failed = 0;
    for edge in dom.traverse(dom.root()) {
        let pageon_dom::Edge::Enter(id) = edge else {
            continue;
        };
        let Some(node) = dom.get(id) else { continue };
        if !node.is_html("link") {
            continue;
        }
        let rel = node.attr("rel").unwrap_or_default().to_ascii_lowercase();
        if !rel.split_ascii_whitespace().any(|t| t == "stylesheet") || rel.contains("alternate") {
            continue;
        }
        let Some(href) = node.attr("href").and_then(|h| base.join(h.trim()).ok()) else {
            failed += 1;
            continue;
        };
        let req = FetchRequest::subresource(href, Dest::Style, client.clone());
        match net.borrow_mut().fetch(env, req, now) {
            Ok(r) if (200..300).contains(&r.status) => {
                let charset = r
                    .header("content-type")
                    .and_then(encoding::charset_param)
                    .unwrap_or("utf-8");
                let (css, _) = encoding::decode(&r.body, Some(charset));
                sheets.push(pageon_layout::Stylesheet { link: id, css });
            }
            _ => failed += 1,
        }
    }
    (sheets, failed)
}

/// A plain-text document: the text in one `<pre>`, as browsers show it.
fn plain_text(text: &str) -> pageon_dom::Dom {
    let mut dom = pageon_dom::parse_document("<!DOCTYPE html><pre></pre>", ParseOptions::default());
    let pre = dom.traverse(dom.root()).find_map(|e| match e {
        pageon_dom::Edge::Enter(id) if dom.get(id).is_some_and(|n| n.is_html("pre")) => Some(id),
        _ => None,
    });
    if let Some(pre) = pre {
        let t = dom.create(NodeData::Text(text.to_string()));
        let _ = dom.append(pre, t);
    }
    dom
}
