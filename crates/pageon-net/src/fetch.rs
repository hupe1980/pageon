//! Main fetch: egress policy, request headers, cookies, redirects, CORS and response tainting
//! (the parts of the Fetch standard a browser engine without a cache needs).

use pageon_env::{Env, NetError, Request, private_host_of};
use url::Url;

use crate::cookie::Context;
use crate::{
    Block, Client, Credentials, Dest, FetchError, FetchRequest, Fetched, Kind, Mode, Net, Outcome,
    Redirect, RequestEntry, site,
};

/// Request headers a page may not set (Fetch's forbidden request-header names).
fn forbidden(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.starts_with("proxy-")
        || n.starts_with("sec-")
        || matches!(
            n.as_str(),
            "accept-charset"
                | "accept-encoding"
                | "access-control-request-headers"
                | "access-control-request-method"
                | "connection"
                | "content-length"
                | "cookie"
                | "cookie2"
                | "date"
                | "dnt"
                | "expect"
                | "host"
                | "keep-alive"
                | "origin"
                | "referer"
                | "set-cookie"
                | "signature"
                | "signature-agent"
                | "signature-input"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
                | "via"
        )
}

/// Whether a request header is CORS-safelisted.
fn safelisted(name: &str, value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    match name.to_ascii_lowercase().as_str() {
        "accept" => !value.bytes().any(unsafe_byte),
        "accept-language" | "content-language" => value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" *,-.;=".contains(&b)),
        "content-type" => {
            if value.bytes().any(unsafe_byte) {
                return false;
            }
            let essence = value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            matches!(
                essence.as_str(),
                "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"
            )
        }
        _ => false,
    }
}

fn unsafe_byte(b: u8) -> bool {
    (b < 0x20 && b != b'\t') || b"\"():<>?@[\\]{}\x7f".contains(&b)
}

fn simple_method(m: &str) -> bool {
    matches!(m, "GET" | "HEAD" | "POST")
}

/// The reason phrase for a status (HTTP/2 has none; this keeps `statusText` deterministic).
pub(crate) fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        410 => "Gone",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

fn accept(dest: Dest) -> &'static str {
    match dest {
        Dest::Document | Dest::Frame => {
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
        }
        Dest::Style => "text/css,*/*;q=0.1",
        Dest::Image => "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
        Dest::Script | Dest::Fetch | Dest::Xhr | Dest::Robots => "*/*",
    }
}

fn sec_dest(dest: Dest) -> &'static str {
    match dest {
        Dest::Document => "document",
        Dest::Frame => "iframe",
        Dest::Script => "script",
        Dest::Style => "style",
        Dest::Image => "image",
        Dest::Fetch | Dest::Xhr | Dest::Robots => "empty",
    }
}

fn sec_mode(mode: Mode) -> &'static str {
    match mode {
        Mode::Navigate => "navigate",
        Mode::SameOrigin => "same-origin",
        Mode::Cors => "cors",
        Mode::NoCors => "no-cors",
    }
}

/// `Sec-Fetch-Site` levels, worst last.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Relation {
    None,
    SameOrigin,
    SameSite,
    CrossSite,
}

fn relation(client: Option<&Client>, url: &Url) -> Relation {
    match client {
        None => Relation::None,
        Some(c) if site::same_origin(&c.url, url) => Relation::SameOrigin,
        Some(c) if site::site(&c.url) == site::site(url) => Relation::SameSite,
        Some(_) => Relation::CrossSite,
    }
}

/// `Referer` under `strict-origin-when-cross-origin`.
fn referrer(client: &Client, url: &Url) -> Option<String> {
    let from = &client.url;
    if !matches!(from.scheme(), "http" | "https") {
        return None;
    }
    if from.scheme() == "https" && !site::trustworthy(url) {
        return None;
    }
    if site::same_origin(from, url) {
        let mut r = from.clone();
        r.set_fragment(None);
        let _ = r.set_username("");
        let _ = r.set_password(None);
        Some(r.to_string())
    } else {
        Some(format!("{}/", site::origin(from)))
    }
}

/// The browser's rules for where a request may go. `from` is the previous URL of a redirect
/// chain (`None` for the first hop).
fn egress(net: &Net, req: &FetchRequest, url: &Url) -> Result<(), FetchError> {
    // A web page never reaches the disk: `file:` is for local pages and their own files.
    // (Redirects lead only to http and https.)
    if url.scheme() == "file"
        && req
            .client
            .as_ref()
            .is_some_and(|c| c.url.scheme() != "file")
    {
        return Err(FetchError::Blocked {
            url: url.to_string(),
            reason: Block::LocalFile,
        });
    }
    if !net.allow_private
        && let Some(host) = private_host_of(url.as_str())
    {
        return Err(FetchError::Blocked {
            url: url.to_string(),
            reason: Block::PrivateNetwork(format!("private network address {host}")),
        });
    }
    if req.dest != Dest::Document
        && let Some(c) = &req.client
        && c.url.scheme() == "https"
        && url.scheme() == "http"
        && !site::trustworthy(url)
    {
        return Err(FetchError::Blocked {
            url: url.to_string(),
            reason: Block::MixedContent,
        });
    }
    Ok(())
}

/// Refuses a top-level document that the site's robots.txt disallows for `pageon` (RFC 9309).
/// robots.txt is fetched once per origin per session, without cookies, and logged.
fn robots_check(
    run: &mut Run<'_>,
    url: &Url,
    entries: &mut Vec<RequestEntry>,
) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Ok(());
    }
    let origin = url.origin().ascii_serialization();
    if !run.net.robots_txt.contains_key(&origin) {
        let robots = fetch_robots(run, url, entries)?;
        run.net.robots_txt.insert(origin.clone(), robots);
    }
    let path = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_string(),
    };
    match run
        .net
        .robots_txt
        .get(&origin)
        .and_then(|r| r.disallowed(&path))
    {
        Some(why) => Err(FetchError::Blocked {
            url: url.to_string(),
            reason: Block::Robots(format!("robots.txt disallows {path} ({why})")),
        }),
        None => Ok(()),
    }
}

fn fetch_robots(
    run: &mut Run<'_>,
    page: &Url,
    entries: &mut Vec<RequestEntry>,
) -> Result<crate::Robots, FetchError> {
    let Ok(mut url) = page.join("/robots.txt") else {
        return Ok(crate::Robots::AllowAll);
    };
    // RFC 9309 2.3.1.2: follow at least five redirects.
    for _ in 0..=5 {
        if !run.net.allow_private
            && let Some(host) = private_host_of(url.as_str())
        {
            return Err(FetchError::Blocked {
                url: url.to_string(),
                reason: Block::PrivateNetwork(format!("private network address {host}")),
            });
        }
        let mut entry = RequestEntry {
            method: "GET".into(),
            url: url.to_string(),
            dest: Dest::Robots,
            status: None,
            final_url: None,
            outcome: Outcome::Ok,
            cookies_rejected: Vec::new(),
            from_cache: false,
        };
        let mut req = pageon_env::Request::get(url.as_str());
        req.allow_private = run.net.allow_private;
        req.headers.push(("accept".into(), "text/plain,*/*".into()));
        let res = match run.env.fetch(&req) {
            Ok(r) => r,
            // Not in the recording: the recorded session already decided (no egress anyway).
            Err(NetError::ReplayMiss(_)) => return Ok(crate::Robots::AllowAll),
            // Refused by the egress policy: the rules cannot be checked, so the page is not
            // loaded either (fail-closed), with the policy's own reason for it.
            Err(NetError::Blocked(m)) => {
                entry.outcome = Outcome::Failed(m.clone());
                entries.push(entry);
                return Err(net_error(page, NetError::Blocked(m)));
            }
            Err(e) => {
                entry.outcome = Outcome::Failed(e.to_string());
                entries.push(entry);
                return Ok(crate::Robots::DisallowAll(format!(
                    "robots.txt is unreachable ({e})"
                )));
            }
        };
        entry.status = Some(res.status);
        entries.push(entry);
        if (300..400).contains(&res.status)
            && let Some(next) = res.header("location").and_then(|l| url.join(l).ok())
        {
            url = next;
            continue;
        }
        return Ok(crate::Robots::from_status(res.status)
            .unwrap_or_else(|| crate::Robots::parse(&res.body, "pageon")));
    }
    // A redirect chain longer than RFC 9309's five: the rules cannot be read; fail closed.
    Ok(crate::Robots::DisallowAll(
        "robots.txt could not be checked: more than 5 redirects".into(),
    ))
}

fn net_error(url: &Url, e: NetError) -> FetchError {
    match e {
        NetError::Blocked(m) => FetchError::Blocked {
            url: url.to_string(),
            reason: Block::PrivateNetwork(m),
        },
        e => FetchError::Network(e),
    }
}

/// The cookie context of a request to `url`.
fn cookie_cx<'a>(req: &FetchRequest, url: &'a Url, top_site: &'a str) -> Context<'a> {
    match &req.client {
        // A top-level navigation a page started: the new page is the top level; it is
        // same-site when the initiator is.
        Some(c) if req.dest == Dest::Document => Context {
            url,
            top_site,
            same_site: site::site(&c.url) == site::site(url),
            top_level_safe_nav: matches!(req.method.as_str(), "GET" | "HEAD"),
            script: false,
        },
        None => Context {
            url,
            top_site,
            same_site: true,
            top_level_safe_nav: matches!(req.method.as_str(), "GET" | "HEAD"),
            script: false,
        },
        Some(c) => Context {
            url,
            top_site,
            same_site: c.same_site_ancestors && site::site(url) == c.top_site,
            top_level_safe_nav: false,
            script: false,
        },
    }
}

/// Whether a CORS response allows `origin`.
fn cors_check(
    headers: &[(String, String)],
    origin: &str,
    credentials: bool,
) -> Result<(), FetchError> {
    let get = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.trim());
    let Some(acao) = get("access-control-allow-origin") else {
        return Err(FetchError::Cors(
            "no Access-Control-Allow-Origin header".into(),
        ));
    };
    if credentials {
        if acao != origin {
            return Err(FetchError::Cors(format!(
                "Access-Control-Allow-Origin {acao} does not allow {origin} with credentials"
            )));
        }
        if get("access-control-allow-credentials") != Some("true") {
            return Err(FetchError::Cors(
                "credentials without Access-Control-Allow-Credentials: true".into(),
            ));
        }
    } else if acao != "*" && acao != origin {
        return Err(FetchError::Cors(format!(
            "Access-Control-Allow-Origin {acao} does not allow {origin}"
        )));
    }
    Ok(())
}

fn list(headers: &[(String, String)], name: &str) -> Vec<String> {
    headers
        .iter()
        .filter(|(k, _)| k == name)
        .flat_map(|(_, v)| v.split(','))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// State of one fetch across its redirects.
struct Run<'a> {
    net: &'a mut Net,
    env: &'a dyn Env,
    now: u64,
    rejected: Vec<String>,
}

impl Run<'_> {
    fn send(
        &mut self,
        req: &FetchRequest,
        method: &str,
        url: &Url,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        credentials: bool,
    ) -> Result<pageon_env::Response, FetchError> {
        let top_site = match &req.client {
            Some(c) if req.dest != Dest::Document => c.top_site.clone(),
            _ => site::site(url),
        };
        let mut headers = headers;
        if credentials {
            let cookies = self
                .net
                .jar
                .header(&cookie_cx(req, url, &top_site), self.now);
            if !cookies.is_empty() {
                headers.push(("cookie".into(), cookies));
            }
        }
        let r = self
            .env
            .fetch(&Request {
                method: method.to_string(),
                url: url.to_string(),
                headers,
                body,
                allow_private: self.net.allow_private,
                labels: self.net.labels.clone(),
                cause: self.net.cause.clone(),
                shown_url: None,
                cookie_scope: credentials.then(|| {
                    let cx = cookie_cx(req, url, &top_site);
                    pageon_env::CookieScope {
                        top_site: top_site.clone(),
                        same_site: cx.same_site,
                        top_level_safe_nav: cx.top_level_safe_nav,
                    }
                }),
            })
            .map_err(|e| net_error(url, e))?;
        if credentials {
            for (k, v) in &r.headers {
                if k == "set-cookie"
                    && let Err(why) = self
                        .net
                        .jar
                        .set(v, &cookie_cx(req, url, &top_site), self.now)
                {
                    let name = v.split(['=', ';']).next().unwrap_or_default().trim();
                    self.rejected.push(format!("{name} ({why})"));
                }
            }
        }
        Ok(r)
    }
}

pub(crate) fn fetch(
    net: &mut Net,
    env: &dyn Env,
    mut req: FetchRequest,
    now: u64,
) -> Result<Fetched, FetchError> {
    if matches!(req.dest, Dest::Fetch | Dest::Xhr) {
        req.headers.retain(|(k, _)| !forbidden(k));
    }
    let cacheable = req.method == "GET"
        && matches!(req.dest, Dest::Script | Dest::Style | Dest::Image)
        && egress(net, &req, &req.url).is_ok();
    if cacheable && let Some(hit) = net.cache.get(req.url.as_str(), now).cloned() {
        net.log.push(RequestEntry {
            method: req.method.clone(),
            url: req.url.to_string(),
            dest: req.dest,
            status: Some(hit.status),
            final_url: hit.redirected.then(|| hit.url.to_string()),
            outcome: Outcome::Ok,
            cookies_rejected: Vec::new(),
            from_cache: true,
        });
        return Ok(hit);
    }
    let mut run = Run {
        net,
        env,
        now,
        rejected: Vec::new(),
    };
    let mut entries = Vec::new();
    let result = main(&mut run, &req, &mut entries);
    let mut entry = RequestEntry {
        method: req.method.clone(),
        url: req.url.to_string(),
        dest: req.dest,
        status: None,
        final_url: None,
        outcome: Outcome::Ok,
        cookies_rejected: std::mem::take(&mut run.rejected),
        from_cache: false,
    };
    match &result {
        Ok(f) => {
            entry.status = Some(f.status);
            if f.redirected {
                entry.final_url = Some(f.url.to_string());
            }
        }
        Err(FetchError::Blocked { reason, .. }) => {
            entry.outcome = Outcome::Blocked(reason.to_string());
        }
        Err(FetchError::Cors(m)) => entry.outcome = Outcome::Cors(m.clone()),
        Err(e) => entry.outcome = Outcome::Failed(e.to_string()),
    }
    if cacheable && let Ok(f) = &result {
        run.net.cache.put(req.url.as_str(), f, now);
    }
    run.net.log.extend(entries);
    run.net.log.push(entry);
    result
}

fn main(
    run: &mut Run<'_>,
    req: &FetchRequest,
    entries: &mut Vec<RequestEntry>,
) -> Result<Fetched, FetchError> {
    let client_origin = req.client.as_ref().map(|c| c.url.origin());
    let mut url = req.url.clone();
    let mut method = req.method.clone();
    let mut body = req.body.clone();
    let mut page_headers = req.headers.clone();
    let mut chain = vec![url.to_string()];
    let mut tainted = false;
    let mut worst = Relation::None;
    loop {
        if !matches!(url.scheme(), "http" | "https" | "file") {
            return Err(FetchError::Network(NetError::Network(format!(
                "unsupported scheme {}",
                url.scheme()
            ))));
        }
        egress(run.net, req, &url)?;
        if req.dest == Dest::Document && run.net.robots {
            robots_check(run, &url, entries)?;
        }
        let same_origin = client_origin.as_ref().is_none_or(|o| *o == url.origin()) && !tainted;
        if req.mode == Mode::SameOrigin && !same_origin {
            return Err(FetchError::Cors(format!(
                "mode \"same-origin\" forbids the cross-origin request to {url}"
            )));
        }
        let cors = req.mode == Mode::Cors && !same_origin;
        let origin = match &req.client {
            Some(c) if !tainted => site::origin(&c.url),
            Some(_) => "null".into(),
            None => String::new(),
        };
        let include = req.credentials == Credentials::Include;
        let credentials = match req.credentials {
            Credentials::Include => true,
            Credentials::SameOrigin => same_origin,
            Credentials::Omit => false,
        };
        worst = worst.max(relation(req.client.as_ref(), &url));

        // Request headers: the page's, then the browser's. Under the browser-compatible
        // profile, Chrome's: client hints first, its document `accept`.
        let profile = run.env.profile();
        let chrome = profile == pageon_env::Profile::BrowserCompatible;
        let mut headers: Vec<(String, String)> = Vec::new();
        let has =
            |h: &[(String, String)], n: &str| h.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
        if site::trustworthy(&url) && url.scheme() != "file" {
            for (k, v) in profile.client_hints() {
                headers.push((k.into(), v));
            }
        }
        if chrome && req.dest == Dest::Document {
            headers.push(("upgrade-insecure-requests".into(), "1".into()));
        }
        if !has(&page_headers, "accept") {
            let value = if chrome && matches!(req.dest, Dest::Document | Dest::Frame) {
                pageon_env::chrome::ACCEPT_DOCUMENT
            } else {
                accept(req.dest)
            };
            headers.push(("accept".into(), value.into()));
        }
        if !has(&page_headers, "accept-language") {
            headers.push(("accept-language".into(), "en-US,en;q=0.9".into()));
        }
        headers.extend(page_headers.iter().cloned());
        if req.client.is_some() && (cors || !matches!(method.as_str(), "GET" | "HEAD")) {
            headers.push(("origin".into(), origin.clone()));
        }
        if let Some(c) = &req.client
            && let Some(r) = referrer(c, &url)
        {
            headers.push(("referer".into(), r));
        }
        if site::trustworthy(&url) && url.scheme() != "file" {
            headers.push(("sec-fetch-dest".into(), sec_dest(req.dest).into()));
            headers.push(("sec-fetch-mode".into(), sec_mode(req.mode).into()));
            let s = match worst {
                Relation::None => "none",
                Relation::SameOrigin => "same-origin",
                Relation::SameSite => "same-site",
                Relation::CrossSite => "cross-site",
            };
            headers.push(("sec-fetch-site".into(), s.into()));
            if req.dest == Dest::Document {
                headers.push(("sec-fetch-user".into(), "?1".into()));
            }
        }

        if cors {
            let unsafe_names: Vec<String> = {
                let mut v: Vec<String> = page_headers
                    .iter()
                    .filter(|(k, v)| !safelisted(k, v))
                    .map(|(k, _)| k.to_ascii_lowercase())
                    .collect();
                v.sort();
                v.dedup();
                v
            };
            if !simple_method(&method) || !unsafe_names.is_empty() {
                preflight(
                    run,
                    req,
                    &url,
                    &origin,
                    &method,
                    &unsafe_names,
                    include,
                    entries,
                )?;
            }
        }

        let resp = run.send(req, &method, &url, headers, body.clone(), credentials)?;
        let location = resp
            .header("location")
            .filter(|_| matches!(resp.status, 301 | 302 | 303 | 307 | 308));
        if let Some(location) = location {
            match req.redirect {
                Redirect::Error => {
                    return Err(FetchError::Redirect(format!(
                        "{url} redirects, and the request's redirect mode is \"error\""
                    )));
                }
                Redirect::Manual => {
                    return Ok(Fetched {
                        kind: Kind::OpaqueRedirect,
                        status: 0,
                        status_text: String::new(),
                        headers: Vec::new(),
                        body: Vec::new(),
                        url,
                        redirected: false,
                        chain,
                    });
                }
                Redirect::Follow => {}
            }
            let next = url
                .join(location.trim())
                .map_err(|e| FetchError::Redirect(format!("bad Location {location:?}: {e}")))?;
            if !matches!(next.scheme(), "http" | "https") {
                return Err(FetchError::Redirect(format!(
                    "redirect to a {} URL",
                    next.scheme()
                )));
            }
            if chain.contains(&next.to_string()) {
                chain.push(next.to_string());
                return Err(FetchError::RedirectLoop(chain));
            }
            chain.push(next.to_string());
            if chain.len() > req.max_redirects + 1 {
                return Err(FetchError::TooManyRedirects(chain));
            }
            if cors && (!next.username().is_empty() || next.password().is_some()) {
                return Err(FetchError::Cors(
                    "redirect to a URL with credentials".into(),
                ));
            }
            if url.origin() != next.origin()
                && client_origin.as_ref().is_some_and(|o| *o != url.origin())
            {
                tainted = true;
            }
            if (matches!(resp.status, 301 | 302) && method == "POST")
                || (resp.status == 303 && !matches!(method.as_str(), "GET" | "HEAD"))
            {
                method = "GET".into();
                body.clear();
                page_headers.retain(|(k, _)| {
                    !matches!(
                        k.to_ascii_lowercase().as_str(),
                        "content-type"
                            | "content-encoding"
                            | "content-language"
                            | "content-location"
                    )
                });
            }
            url = next;
            continue;
        }

        let exposed = list(&resp.headers, "access-control-expose-headers");
        if cors {
            cors_check(&resp.headers, &origin, include)?;
        }
        let kind = if same_origin || !matches!(req.dest, Dest::Fetch | Dest::Xhr) {
            Kind::Basic
        } else if cors {
            Kind::Cors
        } else {
            Kind::Opaque
        };
        let headers = match kind {
            Kind::Basic => resp
                .headers
                .iter()
                .filter(|(k, _)| k != "set-cookie" && k != "set-cookie2")
                .cloned()
                .collect(),
            Kind::Cors => {
                let all = exposed.iter().any(|e| e == "*") && !include;
                resp.headers
                    .iter()
                    .filter(|(k, _)| {
                        k != "set-cookie"
                            && k != "set-cookie2"
                            && (all
                                || matches!(
                                    k.as_str(),
                                    "cache-control"
                                        | "content-language"
                                        | "content-length"
                                        | "content-type"
                                        | "expires"
                                        | "last-modified"
                                        | "pragma"
                                )
                                || exposed.iter().any(|e| e.eq_ignore_ascii_case(k)))
                    })
                    .cloned()
                    .collect()
            }
            Kind::Opaque | Kind::OpaqueRedirect => Vec::new(),
        };
        let opaque = kind == Kind::Opaque;
        return Ok(Fetched {
            kind,
            status: if opaque { 0 } else { resp.status },
            status_text: if opaque {
                String::new()
            } else {
                status_text(resp.status).into()
            },
            headers,
            body: if opaque { Vec::new() } else { resp.body },
            redirected: chain.len() > 1,
            url,
            chain,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn preflight(
    run: &mut Run<'_>,
    req: &FetchRequest,
    url: &Url,
    origin: &str,
    method: &str,
    unsafe_names: &[String],
    include: bool,
    entries: &mut Vec<RequestEntry>,
) -> Result<(), FetchError> {
    let mut headers = vec![
        ("accept".into(), "*/*".into()),
        ("access-control-request-method".into(), method.to_string()),
        ("origin".into(), origin.to_string()),
    ];
    if !unsafe_names.is_empty() {
        headers.push((
            "access-control-request-headers".into(),
            unsafe_names.join(","),
        ));
    }
    if let Some(c) = &req.client
        && let Some(r) = referrer(c, url)
    {
        headers.push(("referer".into(), r));
    }
    let mut entry = RequestEntry {
        method: "OPTIONS".into(),
        url: url.to_string(),
        dest: req.dest,
        status: None,
        final_url: None,
        outcome: Outcome::Ok,
        cookies_rejected: Vec::new(),
        from_cache: false,
    };
    let result = (|| {
        let r = run.send(req, "OPTIONS", url, headers, Vec::new(), false)?;
        entry.status = Some(r.status);
        if !(200..300).contains(&r.status) {
            return Err(FetchError::Cors(format!(
                "preflight answered {} (needs 2xx)",
                r.status
            )));
        }
        cors_check(&r.headers, origin, include)?;
        let methods = list(&r.headers, "access-control-allow-methods");
        if !simple_method(method)
            && !methods.iter().any(|m| m == method)
            && !(methods.iter().any(|m| m == "*") && !include)
        {
            return Err(FetchError::Cors(format!(
                "method {method} not allowed by Access-Control-Allow-Methods"
            )));
        }
        let allowed = list(&r.headers, "access-control-allow-headers");
        let star = allowed.iter().any(|h| h == "*") && !include;
        for name in unsafe_names {
            if !allowed.iter().any(|h| h.eq_ignore_ascii_case(name))
                && !(star && name != "authorization")
            {
                return Err(FetchError::Cors(format!(
                    "header {name} not allowed by Access-Control-Allow-Headers"
                )));
            }
        }
        Ok(())
    })();
    if let Err(e) = &result {
        entry.outcome = match e {
            FetchError::Cors(m) => Outcome::Cors(m.clone()),
            FetchError::Blocked { reason, .. } => Outcome::Blocked(reason.to_string()),
            e => Outcome::Failed(e.to_string()),
        };
    }
    entries.push(entry);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_rules() {
        assert!(forbidden("Cookie") && forbidden("sec-foo") && forbidden("Proxy-Authorization"));
        assert!(!forbidden("x-requested-with"));
        assert!(safelisted("content-type", "text/plain;charset=UTF-8"));
        assert!(!safelisted("content-type", "application/json"));
        assert!(!safelisted("x-token", "1"));
        assert_eq!(status_text(404), "Not Found");
    }

    #[test]
    fn referrer_policy() {
        let c = Client::top(&Url::parse("https://a.test/p?q#f").unwrap());
        let same = Url::parse("https://a.test/x").unwrap();
        let cross = Url::parse("https://b.test/x").unwrap();
        let down = Url::parse("http://b.test/x").unwrap();
        assert_eq!(referrer(&c, &same).as_deref(), Some("https://a.test/p?q"));
        assert_eq!(referrer(&c, &cross).as_deref(), Some("https://a.test/"));
        assert_eq!(referrer(&c, &down), None);
    }
}
