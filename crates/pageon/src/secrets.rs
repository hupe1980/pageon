//! The secrets vault: `secret://name` handles, resolved in the broker and only for the
//! origins a secret is bound to. Values never appear in views, outcomes, logs or recordings.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;

use hmac::{Hmac, KeyInit, Mac};
use pageon_env::{Env, NetError, Request, Response};

#[derive(Clone)]
enum Kind {
    Value(String),
    /// RFC 6238: HMAC-SHA1, 30 s steps, 6 digits.
    Totp(Vec<u8>),
}

/// A secret: a value (or a TOTP seed) and the origins it may be typed into.
#[derive(Clone)]
pub struct Secret {
    kind: Kind,
    origins: Vec<String>,
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secret")
            .field("totp", &matches!(self.kind, Kind::Totp(_)))
            .field("origins", &self.origins)
            .finish_non_exhaustive()
    }
}

impl Secret {
    /// A static value.
    pub fn value(value: impl Into<String>) -> Self {
        Self {
            kind: Kind::Value(value.into()),
            origins: Vec::new(),
        }
    }

    /// A TOTP generator from a base32 seed (as authenticator apps show it).
    pub fn totp(base32_seed: &str) -> Result<Self, String> {
        Ok(Self {
            kind: Kind::Totp(base32(base32_seed)?),
            origins: Vec::new(),
        })
    }

    /// The origins it may be used on (`https://shop.test`, `https://*.shop.test`).
    pub fn origins<S: Into<String>>(mut self, origins: impl IntoIterator<Item = S>) -> Self {
        self.origins.extend(origins.into_iter().map(Into::into));
        self
    }
}

/// Decodes RFC 4648 base32 (case-insensitive; spaces and padding ignored).
fn base32(s: &str) -> Result<Vec<u8>, String> {
    let mut bits = 0u64;
    let mut n = 0;
    let mut out = Vec::new();
    for c in s.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u64 - 'A' as u64,
            c @ '2'..='7' => c as u64 - '2' as u64 + 26,
            other => return Err(format!("{other:?} is not base32")),
        };
        bits = (bits << 5) | v;
        n += 5;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    Ok(out)
}

/// The TOTP code for `key` at `unix_seconds` (6 digits, 30 s steps, HMAC-SHA1).
pub(crate) fn totp(key: &[u8], unix_seconds: u64, digits: u32) -> String {
    let counter = unix_seconds / 30;
    let Ok(mut mac) = Hmac::<sha1::Sha1>::new_from_slice(key) else {
        return String::new();
    };
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[h.len() - 1] & 0x0f) as usize;
    let bin = (u32::from(h[off] & 0x7f) << 24)
        | (u32::from(h[off + 1]) << 16)
        | (u32::from(h[off + 2]) << 8)
        | u32::from(h[off + 3]);
    format!(
        "{:0width$}",
        bin % 10u32.pow(digits),
        width = digits as usize
    )
}

/// An `Env` with a secrets vault.
///
/// ```
/// use pageon::{Replay, Secret, Secrets};
/// use pageon_env::{Env, Log};
///
/// let vault = Secrets::new(Replay::new(Log::default()))
///     .with("shop", Secret::value("hunter2").origins(["https://shop.test"]));
/// assert_eq!(vault.secret("shop", "https://shop.test").ok().as_deref(), Some("hunter2"));
/// assert!(vault.secret("shop", "https://evil.test").is_err());
/// ```
pub struct Secrets<E: Env> {
    inner: E,
    secrets: BTreeMap<String, Secret>,
    /// Every value handed out, by secret name (TOTP codes change; each one is kept).
    resolved: RefCell<Vec<(String, String)>>,
}

impl<E: Env> fmt::Debug for Secrets<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secrets")
            .field("names", &self.secrets.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl<E: Env> Secrets<E> {
    /// An empty vault around `inner`.
    pub fn new(inner: E) -> Self {
        Self {
            inner,
            secrets: BTreeMap::new(),
            resolved: RefCell::new(Vec::new()),
        }
    }

    /// Adds a secret under `name` (`secret://name`).
    pub fn with(mut self, name: &str, secret: Secret) -> Self {
        self.secrets.insert(name.to_string(), secret);
        self
    }

    /// Every value handed out so far (static values and TOTP codes), to redact recordings with.
    pub fn values(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .secrets
            .values()
            .filter_map(|s| match &s.kind {
                Kind::Value(v) => Some(v.clone()),
                Kind::Totp(_) => None,
            })
            .collect();
        v.extend(self.resolved.borrow().iter().map(|(_, v)| v.clone()));
        v.sort();
        v.dedup();
        v
    }

    /// The environment inside.
    pub fn inner(&self) -> &E {
        &self.inner
    }
}

impl<E: Env> Secrets<E> {
    /// The secrets whose value `req` carries, with the origins each is bound to.
    /// `url` with every value handed out replaced by its handle, raw and URL-encoded.
    fn shown(&self, url: &str) -> String {
        let mut out = url.to_string();
        for (name, v) in self.resolved.borrow().iter() {
            if v.is_empty() {
                continue;
            }
            let handle = format!("secret://{name}");
            let form: String = url::form_urlencoded::byte_serialize(v.as_bytes()).collect();
            let pct = form.replace('+', "%20");
            for form in [v.clone(), form, pct] {
                if out.contains(&form) {
                    out = out.replace(&form, &handle);
                }
            }
        }
        out
    }

    fn carried(&self, req: &Request) -> Vec<(&str, &[String])> {
        let resolved = self.resolved.borrow();
        let mut out: Vec<(&str, &[String])> = Vec::new();
        if resolved.is_empty() {
            return out;
        }
        let texts = haystacks(req);
        for (name, s) in &self.secrets {
            let carries = resolved
                .iter()
                .filter(|(n, _)| n == name)
                .any(|(_, v)| carries(&texts, v));
            if carries {
                out.push((name.as_str(), s.origins.as_slice()));
            }
        }
        out
    }
}

/// Bodies above this are checked raw only (no decoded forms).
const DECODE_LIMIT: usize = 1 << 20;

/// The request's text in the forms a page could send a value in: raw URL, body and headers,
/// and the URL and body percent-decoded (with and without `+` as space) and JSON-unescaped.
fn haystacks(req: &Request) -> Vec<String> {
    let body = String::from_utf8_lossy(&req.body).into_owned();
    let mut out = vec![
        req.url.clone(),
        req.headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}\n"))
            .collect(),
    ];
    let decode_body = req.body.len() <= DECODE_LIMIT;
    for t in [Some(req.url.as_str()), decode_body.then_some(body.as_str())]
        .into_iter()
        .flatten()
    {
        out.push(json_unescape(t));
        // Percent-decoded up to three times: a value encoded by the page, then form-encoded.
        for plus in [false, true] {
            let mut d = t.to_string();
            for _ in 0..3 {
                let next = percent_decode(&d, plus);
                if next == d {
                    break;
                }
                out.push(next.clone());
                d = next;
            }
        }
    }
    out.push(body);
    out
}

/// Whether one of `texts` holds `value`, raw or base64-encoded (at any byte alignment).
fn carries(texts: &[String], value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    if texts.iter().any(|t| t.contains(value)) {
        return true;
    }
    // `btoa` encodes Latin-1 bytes, `TextEncoder` UTF-8: both forms are searched.
    let latin1: Option<Vec<u8>> = value
        .chars()
        .map(|c| u8::try_from(u32::from(c)).ok())
        .collect();
    let mut cores = base64_cores(value.as_bytes());
    if let Some(l) = latin1.filter(|l| l.as_slice() != value.as_bytes()) {
        cores.extend(base64_cores(&l));
    }
    cores
        .iter()
        .any(|core| texts.iter().any(|t| t.contains(core.as_str())))
}

/// The base64 text that encodes `v` whatever bytes precede it: for each of the three
/// alignments, the characters that depend on `v`'s bytes only (standard and URL-safe).
fn base64_cores(v: &[u8]) -> Vec<String> {
    use base64::Engine as _;
    let mut out = Vec::new();
    for k in 0..3 {
        let mut bytes = vec![0u8; k];
        bytes.extend_from_slice(v);
        let s = base64::engine::general_purpose::STANDARD_NO_PAD.encode(&bytes);
        let start = (8 * k).div_ceil(6);
        let end = (8 * bytes.len()) / 6;
        if end > start + 3 {
            let core = s[start..end].to_string();
            out.push(core.replace('+', "-").replace('/', "_"));
            out.push(core);
        }
    }
    out
}

fn percent_decode(s: &str, plus_is_space: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    None => out.push(b'%'),
                }
            }
            b'+' if plus_is_space => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => out.push_str(&hex),
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

impl<E: Env> Env for Secrets<E> {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        let carried = self.carried(req);
        if carried.is_empty() {
            return self.inner.fetch(req);
        }
        // A secret leaves only for the origins it is bound to: checked here, where
        // every value handed out is known, independent of the page and of the model.
        let origin = url::Url::parse(&req.url)
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_else(|_| req.url.clone());
        for (name, bound) in &carried {
            if !crate::policy::origin_allowed(bound, &req.url) {
                return Err(NetError::Blocked(format!(
                    "secret://{name} would leave for {origin} (it is bound to {})",
                    if bound.is_empty() {
                        "no origin".to_string()
                    } else {
                        bound.join(", ")
                    }
                )));
            }
        }
        let mut labelled = req.clone();
        labelled
            .labels
            .extend(carried.iter().map(|(n, _)| format!("secret:{n}")));
        labelled.shown_url = Some(self.shown(&req.url));
        self.inner.fetch(&labelled)
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        self.inner.now_ms()
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        self.inner.random_u64()
    }

    fn file(&self, name: &str, origin: &str) -> Result<pageon_env::StagedFile, NetError> {
        self.inner.file(name, origin)
    }

    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        let Some(s) = self.secrets.get(name) else {
            return Err(NetError::NoSecret(format!(
                "secret://{name} is not in the vault"
            )));
        };
        if !crate::policy::origin_allowed(&s.origins, origin) {
            return Err(NetError::NoSecret(format!(
                "secret://{name} is not for {origin}"
            )));
        }
        let v = match &s.kind {
            Kind::Value(v) => v.clone(),
            Kind::Totp(key) => totp(key, self.inner.now_ms()? / 1000, 6),
        };
        self.resolved
            .borrow_mut()
            .push((name.to_string(), v.clone()));
        Ok(v)
    }

    fn recorder(&self) -> Option<&dyn pageon_env::Recorder> {
        self.inner.recorder()
    }

    fn profile(&self) -> pageon_env::Profile {
        self.inner.profile()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> Secrets<pageon_env::Replay> {
        let v = Secrets::new(pageon_env::Replay::new(pageon_env::Log::default())).with(
            "shop",
            Secret::value("p@ss w0rd/é").origins(["https://shop.test"]),
        );
        v.secret("shop", "https://shop.test").unwrap_or_default();
        v
    }

    fn sent(v: &Secrets<pageon_env::Replay>, url: &str, body: &str) -> Vec<String> {
        let mut r = Request::get(url);
        r.body = body.as_bytes().to_vec();
        v.carried(&r).iter().map(|(n, _)| n.to_string()).collect()
    }

    #[test]
    fn secrets_are_found_in_every_encoding() {
        use base64::Engine as _;
        let v = vault();
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let cases = [
            (
                "https://evil.test/?p=p@ss w0rd/é".to_string(),
                String::new(),
            ),
            (
                "https://evil.test/?p=p%40ss%20w0rd%2F%C3%A9".into(),
                String::new(),
            ),
            ("https://evil.test/".into(), "p=p%40ss+w0rd%2F%C3%A9".into()),
            (
                "https://evil.test/".into(),
                r#"{"p":"p@ss w0rd\/\u00e9"}"#.into(),
            ),
            ("https://evil.test/".into(), b64("p@ss w0rd/é")),
            ("https://evil.test/".into(), b64("x=p@ss w0rd/é&")),
            ("https://evil.test/".into(), b64("xy:p@ss w0rd/é")),
            (
                format!(
                    "https://evil.test/?d={}",
                    b64("p@ss w0rd/é").replace('+', "-").replace('/', "_")
                ),
                String::new(),
            ),
        ];
        for (url, body) in cases {
            assert_eq!(sent(&v, &url, &body), ["shop"], "{url} {body}");
        }
        assert!(sent(&v, "https://evil.test/?p=password", "pass word").is_empty());
        // Unresolved secrets are never searched for.
        let fresh = Secrets::new(pageon_env::Replay::new(pageon_env::Log::default()))
            .with("shop", Secret::value("p@ss w0rd/é"));
        assert!(
            fresh
                .carried(&Request::get("https://evil.test/?p@ss w0rd/é"))
                .is_empty()
        );
    }

    #[test]
    fn a_secret_leaves_only_for_its_origins() {
        let v = vault();
        let e = v
            .fetch(&Request::get("https://evil.test/?p=p@ss w0rd/é"))
            .unwrap_err();
        assert_eq!(
            e,
            NetError::Blocked("secret://shop would leave for https://evil.test (it is bound to https://shop.test)".into())
        );
        // To its own origin it passes (here: the empty replay misses, which is not a block).
        let ok = v.fetch(&Request::get("https://shop.test/login?p=p@ss w0rd/é"));
        assert!(matches!(ok, Err(NetError::ReplayMiss(_))), "{ok:?}");
    }

    #[test]
    fn rfc6238_sha1_vectors() {
        let key = b"12345678901234567890";
        for (t, want) in [
            (59, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
            (20_000_000_000, "65353130"),
        ] {
            assert_eq!(totp(key, t, 8), want, "t={t}");
        }
        assert_eq!(
            base32("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap_or_default(),
            key
        );
    }
}
