//! The cookie jar: RFC 6265bis as Chromium ships it. `SameSite` defaults to `Lax`, `None`
//! requires `Secure`, the `__Secure-`/`__Host-` prefixes are enforced, public suffixes are
//! rejected, and `Partitioned` cookies (CHIPS) are keyed by the top-level site.
//!
//! Time is passed in (ms since the Unix epoch), so expiry follows the load's virtual clock.

use serde::{Deserialize, Serialize};
use url::Url;

use crate::site;

/// Maximum bytes of name plus value.
pub const MAX_NAME_VALUE: usize = 4096;
/// Maximum bytes of one attribute value.
const MAX_ATTR: usize = 1024;
/// Maximum cookies per registrable domain.
pub const MAX_PER_DOMAIN: usize = 180;
/// Maximum cookies in a jar.
pub const MAX_TOTAL: usize = 3000;
/// Expiry is capped at 400 days.
const MAX_AGE_MS: u64 = 400 * 24 * 3600 * 1000;

/// `SameSite`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SameSite {
    /// Only same-site requests.
    Strict,
    /// Same-site requests and top-level navigations with a safe method (the default).
    Lax,
    /// Every request (requires `Secure`).
    None,
}

/// A stored cookie.
#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    /// Name (may be empty).
    pub name: String,
    /// Value.
    pub value: String,
    /// Domain, lower-case, no leading dot.
    pub domain: String,
    /// Sent only to `domain` itself (no `Domain` attribute).
    pub host_only: bool,
    /// Path.
    pub path: String,
    /// Expiry in ms since the Unix epoch; `None` for a session cookie.
    pub expires: Option<u64>,
    /// `Secure`.
    pub secure: bool,
    /// `HttpOnly`.
    pub http_only: bool,
    /// `SameSite`.
    pub same_site: SameSite,
    /// The top-level site of a partitioned cookie.
    pub partition_key: Option<String>,
    /// Creation time (ms).
    pub created: u64,
}

/// Where a cookie is set or read: the request URL and how it relates to the page.
#[derive(Debug, Clone)]
pub struct Context<'a> {
    /// The request (or document) URL.
    pub url: &'a Url,
    /// The top-level site (`https://example.com`).
    pub top_site: &'a str,
    /// Whether the request is same-site with the top-level site and every ancestor frame.
    pub same_site: bool,
    /// Whether it is a top-level navigation with a safe method (`Lax` cookies go along).
    pub top_level_safe_nav: bool,
    /// Whether it comes from script (`document.cookie`): `HttpOnly` cookies are invisible and
    /// cannot be set.
    pub script: bool,
}

/// Why a `Set-Cookie` was not stored.
pub type Rejection = String;

fn has_ctl(s: &str) -> bool {
    s.bytes().any(|b| (b < 0x20 && b != b'\t') || b == 0x7f)
}

fn wsp_trim(s: &str) -> &str {
    s.trim_matches(|c| c == ' ' || c == '\t')
}

/// The default path of a URL (RFC 6265bis 5.1.4).
pub fn default_path(url: &Url) -> String {
    let p = url.path();
    if !p.starts_with('/') {
        return "/".into();
    }
    match p.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

/// Domain match (RFC 6265bis 5.1.3).
pub fn domain_match(host: &str, domain: &str) -> bool {
    if host == domain {
        return true;
    }
    host.ends_with(domain)
        && host.as_bytes().get(host.len() - domain.len() - 1) == Some(&b'.')
        && host.parse::<std::net::IpAddr>().is_err()
}

/// Path match (RFC 6265bis 5.1.4).
pub fn path_match(request: &str, cookie: &str) -> bool {
    request == cookie
        || (request.starts_with(cookie)
            && (cookie.ends_with('/') || request.as_bytes().get(cookie.len()) == Some(&b'/')))
}

fn host_of(url: &Url) -> Option<String> {
    let h = url.host_str()?;
    // IPv6 literals keep their brackets in `host_str`; cookies compare the bare address.
    Some(
        h.trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase(),
    )
}

/// Parses a cookie date (RFC 6265bis 5.1.1) into ms since the Unix epoch.
pub fn parse_date(s: &str) -> Option<u64> {
    let delim = |c: char| {
        let b = c as u32;
        b == 0x09
            || (0x20..=0x2f).contains(&b)
            || (0x3b..=0x40).contains(&b)
            || (0x5b..=0x60).contains(&b)
            || (0x7b..=0x7e).contains(&b)
    };
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for tok in s.split(delim).filter(|t| !t.is_empty()) {
        if time.is_none()
            && let Some(t) = parse_time(tok)
        {
            time = Some(t);
            continue;
        }
        if day.is_none()
            && let Some(d) = leading_digits(tok, 1, 2)
        {
            day = Some(d);
            continue;
        }
        if month.is_none()
            && let Some(m) = parse_month(tok)
        {
            month = Some(m);
            continue;
        }
        if year.is_none()
            && let Some(y) = leading_digits(tok, 2, 4)
        {
            year = Some(y);
            continue;
        }
    }
    let (h, mi, se) = time?;
    let (day, month, mut year) = (day?, month?, year?);
    if (70..=99).contains(&year) {
        year += 1900;
    } else if year <= 69 {
        year += 2000;
    }
    if !(1..=31).contains(&day) || year < 1601 || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let mdays = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day > mdays[(month - 1) as usize] {
        return None;
    }
    let days = days_from_civil(i64::from(year), month, day);
    let secs = days * 86_400 + i64::from(h) * 3600 + i64::from(mi) * 60 + i64::from(se);
    Some(if secs < 0 { 0 } else { secs as u64 * 1000 })
}

/// Digits at the start of `tok` (between `min` and `max` of them), followed by a non-digit or
/// the end.
fn leading_digits(tok: &str, min: usize, max: usize) -> Option<u32> {
    let n = tok.bytes().take_while(u8::is_ascii_digit).count();
    if n < min || n > max {
        return None;
    }
    tok[..n].parse().ok()
}

fn parse_time(tok: &str) -> Option<(u32, u32, u32)> {
    let mut parts = tok.splitn(3, ':');
    let h = parts.next()?;
    let m = parts.next()?;
    let rest = parts.next()?;
    let digits = |s: &str| -> Option<u32> {
        if s.is_empty() || s.len() > 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let s_len = rest.bytes().take_while(u8::is_ascii_digit).count();
    if s_len == 0 || s_len > 2 {
        return None;
    }
    Some((digits(h)?, digits(m)?, rest[..s_len].parse().ok()?))
}

fn parse_month(tok: &str) -> Option<u32> {
    let t = tok.get(..3)?.to_ascii_lowercase();
    let i = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| *m == t)?;
    Some(i as u32 + 1)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parsed attributes of one `Set-Cookie` string.
struct Parsed {
    name: String,
    value: String,
    expires: Option<u64>,
    max_age: Option<i64>,
    domain: Option<String>,
    path: Option<String>,
    secure: bool,
    http_only: bool,
    same_site: Option<SameSite>,
    partitioned: bool,
}

fn parse(header: &str) -> Result<Parsed, Rejection> {
    if has_ctl(header) {
        return Err("control character".into());
    }
    let (pair, attrs) = header.split_once(';').unwrap_or((header, ""));
    let (name, value) = match pair.split_once('=') {
        Some((n, v)) => (wsp_trim(n), wsp_trim(v)),
        None => ("", wsp_trim(pair)),
    };
    if name.is_empty() && value.is_empty() {
        return Err("empty name and value".into());
    }
    if name.len() + value.len() > MAX_NAME_VALUE {
        return Err(format!("name and value longer than {MAX_NAME_VALUE} bytes"));
    }
    let mut p = Parsed {
        name: name.into(),
        value: value.into(),
        expires: None,
        max_age: None,
        domain: None,
        path: None,
        secure: false,
        http_only: false,
        same_site: None,
        partitioned: false,
    };
    for av in attrs.split(';') {
        let (k, v) = match av.split_once('=') {
            Some((k, v)) => (wsp_trim(k), wsp_trim(v)),
            None => (wsp_trim(av), ""),
        };
        if v.len() > MAX_ATTR {
            continue;
        }
        match k.to_ascii_lowercase().as_str() {
            "expires" => {
                if let Some(t) = parse_date(v) {
                    p.expires = Some(t);
                }
            }
            "max-age" => {
                let ok = v
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_digit() || b == b'-')
                    && v.bytes().skip(1).all(|b| b.is_ascii_digit());
                if ok {
                    p.max_age = Some(v.parse::<i64>().unwrap_or(if v.starts_with('-') {
                        i64::MIN
                    } else {
                        i64::MAX
                    }));
                }
            }
            "domain" => {
                if !v.is_empty() {
                    p.domain = Some(v.strip_prefix('.').unwrap_or(v).to_ascii_lowercase());
                }
            }
            "path" => {
                p.path = (v.starts_with('/')).then(|| v.to_string());
            }
            "secure" => p.secure = true,
            "httponly" => p.http_only = true,
            "samesite" => {
                p.same_site = match v.to_ascii_lowercase().as_str() {
                    "strict" => Some(SameSite::Strict),
                    "lax" => Some(SameSite::Lax),
                    "none" => Some(SameSite::None),
                    _ => None,
                }
            }
            "partitioned" => p.partitioned = true,
            _ => {}
        }
    }
    Ok(p)
}

/// The cookies of a session.
#[derive(Debug, Clone, Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
}

impl Jar {
    /// All cookies still valid at `now`.
    pub fn all(&self, now: u64) -> Vec<Cookie> {
        self.cookies
            .iter()
            .filter(|c| c.expires.is_none_or(|e| e > now))
            .cloned()
            .collect()
    }

    /// Adds cookies as they are (an import: no context checks, replacing equal ones).
    pub fn add(&mut self, cookies: &[Cookie]) {
        for c in cookies {
            self.cookies.retain(|o| !same_key(o, c));
            self.cookies.push(c.clone());
        }
    }

    /// Stores one `Set-Cookie` header received in `cx` at `now`.
    pub fn set(&mut self, header: &str, cx: &Context<'_>, now: u64) -> Result<(), Rejection> {
        let p = parse(header)?;
        let host = host_of(cx.url).ok_or("no host")?;
        let is_ip = host.parse::<std::net::IpAddr>().is_ok();
        let secure_origin = site::trustworthy(cx.url);
        let (domain, host_only) = match p.domain {
            Some(d) if is_ip && d != host => return Err(format!("Domain={d} on an IP address")),
            Some(d) if site::is_public_suffix(&d) => {
                if d == host {
                    (host.clone(), true)
                } else {
                    return Err(format!("Domain={d} is a public suffix"));
                }
            }
            Some(d) if !domain_match(&host, &d) => {
                return Err(format!("Domain={d} does not match {host}"));
            }
            Some(d) if is_ip => (d, true),
            Some(d) => (d, false),
            None => (host.clone(), true),
        };
        let path = p.path.clone().unwrap_or_else(|| default_path(cx.url));
        if p.secure && !secure_origin {
            return Err("Secure from an insecure URL".into());
        }
        if p.http_only && cx.script {
            return Err("HttpOnly from script".into());
        }
        let lower = p.name.to_ascii_lowercase();
        if lower.starts_with("__secure-") && !(p.secure && secure_origin) {
            return Err("__Secure- prefix without Secure".into());
        }
        if lower.starts_with("__host-")
            && !(p.secure && secure_origin && host_only && p.path.as_deref() == Some("/"))
        {
            return Err("__Host- prefix needs Secure, no Domain and Path=/".into());
        }
        let same_site = p.same_site.unwrap_or(SameSite::Lax);
        if same_site == SameSite::None && !p.secure {
            return Err("SameSite=None without Secure".into());
        }
        if same_site != SameSite::None && !cx.same_site && !cx.top_level_safe_nav {
            return Err(format!(
                "SameSite={same_site:?} set from a cross-site context"
            ));
        }
        if p.partitioned && !p.secure {
            return Err("Partitioned without Secure".into());
        }
        let partition_key = p.partitioned.then(|| cx.top_site.to_string());
        // Leave secure cookies alone: an insecure origin cannot shadow a secure cookie.
        if !secure_origin
            && self.cookies.iter().any(|o| {
                o.secure
                    && o.name == p.name
                    && (domain_match(&domain, &o.domain) || domain_match(&o.domain, &domain))
                    && path_match(&path, &o.path)
            })
        {
            return Err("would overwrite a Secure cookie from an insecure URL".into());
        }
        let expires = match (p.max_age, p.expires) {
            (Some(ma), _) if ma <= 0 => Some(0),
            (Some(ma), _) => {
                Some(now.saturating_add((ma as u64).saturating_mul(1000).min(MAX_AGE_MS)))
            }
            (None, Some(e)) => Some(e.min(now.saturating_add(MAX_AGE_MS))),
            (None, None) => None,
        };
        let mut cookie = Cookie {
            name: p.name,
            value: p.value,
            domain,
            host_only,
            path,
            expires,
            secure: p.secure,
            http_only: p.http_only,
            same_site,
            partition_key,
            created: now,
        };
        if let Some(i) = self.cookies.iter().position(|o| same_key(o, &cookie)) {
            if cx.script && self.cookies[i].http_only {
                return Err("would overwrite an HttpOnly cookie from script".into());
            }
            cookie.created = self.cookies[i].created;
            self.cookies.remove(i);
        }
        if cookie.expires.is_some_and(|e| e <= now) {
            return Ok(()); // a deletion
        }
        self.cookies.push(cookie);
        self.evict(now);
        Ok(())
    }

    fn evict(&mut self, now: u64) {
        self.cookies.retain(|c| c.expires.is_none_or(|e| e > now));
        let reg = |c: &Cookie| {
            site::registrable(&c.domain)
                .unwrap_or(&c.domain)
                .to_string()
        };
        if let Some(last) = self.cookies.last() {
            let key = reg(last);
            let mut same: Vec<usize> = (0..self.cookies.len())
                .filter(|&i| reg(&self.cookies[i]) == key)
                .collect();
            while same.len() > MAX_PER_DOMAIN {
                let oldest = same
                    .iter()
                    .copied()
                    .min_by_key(|&i| self.cookies[i].created)
                    .unwrap_or(0);
                self.cookies.remove(oldest);
                same = (0..self.cookies.len())
                    .filter(|&i| reg(&self.cookies[i]) == key)
                    .collect();
            }
        }
        while self.cookies.len() > MAX_TOTAL {
            let oldest = (0..self.cookies.len())
                .min_by_key(|&i| self.cookies[i].created)
                .unwrap_or(0);
            self.cookies.remove(oldest);
        }
    }

    /// The cookies sent for a request in `cx` at `now`, in order (longest path first, then
    /// oldest).
    pub fn matching(&self, cx: &Context<'_>, now: u64) -> Vec<&Cookie> {
        let Some(host) = host_of(cx.url) else {
            return Vec::new();
        };
        let path = cx.url.path();
        let secure = site::trustworthy(cx.url);
        let mut out: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                c.expires.is_none_or(|e| e > now)
                    && if c.host_only {
                        host == c.domain
                    } else {
                        domain_match(&host, &c.domain)
                    }
                    && path_match(path, &c.path)
                    && (!c.secure || secure)
                    && !(c.http_only && cx.script)
                    && match c.same_site {
                        SameSite::None => true,
                        SameSite::Lax => cx.same_site || cx.top_level_safe_nav,
                        SameSite::Strict => cx.same_site,
                    }
                    && c.partition_key.as_deref().is_none_or(|k| k == cx.top_site)
            })
            .collect();
        out.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.created.cmp(&b.created))
        });
        out
    }

    /// The `Cookie` header (or `document.cookie` string) for `cx` at `now`.
    pub fn header(&self, cx: &Context<'_>, now: u64) -> String {
        self.matching(cx, now)
            .iter()
            .map(|c| {
                if c.name.is_empty() {
                    c.value.clone()
                } else {
                    format!("{}={}", c.name, c.value)
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Keeps only the cookies `keep` says yes to.
    pub fn retain(&mut self, keep: impl FnMut(&Cookie) -> bool) {
        self.cookies.retain(keep);
    }

    /// Removes every cookie.
    pub fn clear(&mut self) {
        self.cookies.clear();
    }
}

fn same_key(a: &Cookie, b: &Cookie) -> bool {
    a.name == b.name
        && a.domain == b.domain
        && a.host_only == b.host_only
        && a.path == b.path
        && a.partition_key == b.partition_key
}

/// A cookie in Playwright's shape (`storageState().cookies`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pw {
    name: String,
    value: String,
    domain: String,
    #[serde(default = "root")]
    path: String,
    #[serde(default = "session")]
    expires: f64,
    #[serde(default)]
    http_only: bool,
    #[serde(default)]
    secure: bool,
    #[serde(default = "lax")]
    same_site: SameSite,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    partition_key: Option<String>,
}

fn root() -> String {
    "/".into()
}
fn session() -> f64 {
    -1.0
}
fn lax() -> SameSite {
    SameSite::Lax
}

impl Serialize for Cookie {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        Pw {
            name: self.name.clone(),
            value: self.value.clone(),
            domain: if self.host_only {
                self.domain.clone()
            } else {
                format!(".{}", self.domain)
            },
            path: self.path.clone(),
            expires: self.expires.map_or(-1.0, |e| e as f64 / 1000.0),
            http_only: self.http_only,
            secure: self.secure,
            same_site: self.same_site,
            partition_key: self.partition_key.clone(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for Cookie {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let p = Pw::deserialize(d)?;
        let host_only = !p.domain.starts_with('.');
        Ok(Self {
            name: p.name,
            value: p.value,
            domain: p.domain.trim_start_matches('.').to_ascii_lowercase(),
            host_only,
            path: p.path,
            expires: (p.expires >= 0.0).then_some((p.expires * 1000.0) as u64),
            secure: p.secure,
            http_only: p.http_only,
            same_site: p.same_site,
            partition_key: p.partition_key,
            created: 0,
        })
    }
}

/// Reads cookies from JSON: a Playwright `storageState` object (`{"cookies": [...]}`) or a
/// cookie array.
pub fn from_json(text: &str) -> Result<Vec<Cookie>, String> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum File {
        State { cookies: Vec<Cookie> },
        List(Vec<Cookie>),
    }
    match serde_json::from_str::<File>(text).map_err(|e| e.to_string())? {
        File::State { cookies } | File::List(cookies) => Ok(cookies),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn cx<'a>(url: &'a Url, top: &'a str, same_site: bool) -> Context<'a> {
        Context {
            url,
            top_site: top,
            same_site,
            top_level_safe_nav: false,
            script: false,
        }
    }

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_date("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(1_445_412_480_000)
        );
        assert_eq!(
            parse_date("Sun, 06 Nov 94 08:49:37 GMT"),
            Some(784_111_777_000)
        );
        assert_eq!(parse_date("Feb 31 2020 00:00:00"), None);
        assert_eq!(parse_date("garbage"), None);
    }

    #[test]
    fn domain_path_and_public_suffix() {
        let mut j = Jar::default();
        let a = u("https://www.example.co.uk/a/b");
        let top = "https://example.co.uk";
        assert!(j.set("x=1; Domain=co.uk", &cx(&a, top, true), NOW).is_err());
        assert!(
            j.set("x=1; Domain=other.com", &cx(&a, top, true), NOW)
                .is_err()
        );
        j.set("d=1; Domain=example.co.uk", &cx(&a, top, true), NOW)
            .unwrap();
        j.set("h=2", &cx(&a, top, true), NOW).unwrap();
        let sub = u("https://api.example.co.uk/a/x");
        assert_eq!(j.header(&cx(&sub, top, true), NOW), "d=1");
        assert_eq!(j.header(&cx(&a, top, true), NOW), "d=1; h=2");
        assert_eq!(
            j.header(&cx(&u("https://www.example.co.uk/"), top, true), NOW),
            ""
        );
    }

    #[test]
    fn same_site_secure_prefixes_http_only() {
        let mut j = Jar::default();
        let a = u("https://a.test/");
        let top = "https://a.test";
        j.set("lax=1", &cx(&a, top, true), NOW).unwrap();
        j.set("strict=1; SameSite=Strict", &cx(&a, top, true), NOW)
            .unwrap();
        j.set("none=1; SameSite=None; Secure", &cx(&a, top, true), NOW)
            .unwrap();
        assert!(
            j.set("bad=1; SameSite=None", &cx(&a, top, true), NOW)
                .is_err()
        );
        j.set("ho=1; HttpOnly", &cx(&a, top, true), NOW).unwrap();
        // Cross-site subresource: only None.
        assert_eq!(j.header(&cx(&a, "https://b.test", false), NOW), "none=1");
        // Top-level navigation from another site: Lax and None.
        let mut nav = cx(&a, top, false);
        nav.top_level_safe_nav = true;
        assert_eq!(j.header(&nav, NOW), "lax=1; none=1; ho=1");
        // Script: no HttpOnly; cannot set or overwrite HttpOnly.
        let mut script = cx(&a, top, true);
        script.script = true;
        assert_eq!(j.header(&script, NOW), "lax=1; strict=1; none=1");
        assert!(j.set("ho=2", &script, NOW).is_err());
        assert!(j.set("x=1; HttpOnly", &script, NOW).is_err());
        // Prefixes.
        assert!(j.set("__Secure-a=1", &cx(&a, top, true), NOW).is_err());
        assert!(
            j.set("__Secure-a=1; Secure", &cx(&a, top, true), NOW)
                .is_ok()
        );
        assert!(
            j.set(
                "__Host-a=1; Secure; Path=/; Domain=a.test",
                &cx(&a, top, true),
                NOW
            )
            .is_err()
        );
        assert!(
            j.set("__Host-a=1; Secure; Path=/", &cx(&a, top, true), NOW)
                .is_ok()
        );
        // Insecure origin: no Secure.
        let h = u("http://a.test/");
        assert!(
            j.set("s=1; Secure", &cx(&h, "http://a.test", true), NOW)
                .is_err()
        );
        // Cross-site context cannot set Lax cookies.
        assert!(j.set("c=1", &cx(&a, "https://b.test", false), NOW).is_err());
    }

    #[test]
    fn partitioned_cookies_stay_under_their_top_level_site() {
        let mut j = Jar::default();
        let embed = u("https://widget.test/");
        j.set(
            "p=1; Secure; SameSite=None; Partitioned",
            &cx(&embed, "https://a.test", false),
            NOW,
        )
        .unwrap();
        j.set(
            "u=1; Secure; SameSite=None",
            &cx(&embed, "https://a.test", false),
            NOW,
        )
        .unwrap();
        assert_eq!(
            j.header(&cx(&embed, "https://a.test", false), NOW),
            "p=1; u=1"
        );
        assert_eq!(j.header(&cx(&embed, "https://b.test", false), NOW), "u=1");
    }

    #[test]
    fn expiry_deletion_and_order() {
        let mut j = Jar::default();
        let a = u("https://a.test/x/y");
        let top = "https://a.test";
        j.set("a=1; Path=/", &cx(&a, top, true), NOW).unwrap();
        j.set("a=2; Path=/x", &cx(&a, top, true), NOW + 1).unwrap();
        j.set("t=1; Max-Age=10; Path=/", &cx(&a, top, true), NOW)
            .unwrap();
        assert_eq!(j.header(&cx(&a, top, true), NOW + 5), "a=2; a=1; t=1");
        assert_eq!(j.header(&cx(&a, top, true), NOW + 10_000), "a=2; a=1");
        j.set("a=; Path=/x; Max-Age=0", &cx(&a, top, true), NOW)
            .unwrap();
        assert_eq!(j.header(&cx(&a, top, true), NOW), "a=1; t=1");
    }

    #[test]
    fn limits() {
        let mut j = Jar::default();
        let a = u("https://a.test/");
        for i in 0..200 {
            j.set(&format!("c{i}=1"), &cx(&a, "https://a.test", true), NOW + i)
                .unwrap();
        }
        assert_eq!(j.all(NOW).len(), MAX_PER_DOMAIN);
        assert!(
            j.header(&cx(&a, "https://a.test", true), NOW)
                .starts_with("c20=1")
        );
        assert!(
            j.set(
                &format!("big={}", "x".repeat(5000)),
                &cx(&a, "https://a.test", true),
                NOW
            )
            .is_err()
        );
    }

    #[test]
    fn playwright_shape_round_trips() {
        let text = r#"{"cookies":[{"name":"sid","value":"1","domain":".a.test","path":"/","expires":-1,"httpOnly":true,"secure":true,"sameSite":"Lax"}],"origins":[]}"#;
        let c = from_json(text).unwrap();
        assert!(!c[0].host_only && c[0].http_only && c[0].expires.is_none());
        let back = serde_json::to_string(&c).unwrap();
        assert!(back.contains("\"domain\":\".a.test\""), "{back}");
        assert_eq!(from_json(&back).unwrap()[0].domain, "a.test");
    }
}
