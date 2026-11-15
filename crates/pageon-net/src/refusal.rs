//! Refusals: when a site refuses the agent — a block, a bot challenge, a rate
//! limit — pageon says so in the outcome instead of leaving the agent to read an "Access
//! denied" page. Only the top-level document's final response is classified, and nothing is
//! ever solved: a person can take over.
//!
//! Signals, first match wins: vendor-documented headers, then status with vendor markers, then
//! plain status. A CAPTCHA widget in a normal page is not a refusal.

use std::fmt;

/// What kind of refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    /// The site refuses this agent.
    Blocked,
    /// A bot challenge (a CAPTCHA, an interstitial) stands before the page.
    Challenge,
    /// Too many requests: retry later.
    RateLimited,
    /// The site wants a signed agent (`Accept-Signature`, RFC 9421 §5).
    SignatureRequired,
}

impl RefusalKind {
    /// `blocked`, `challenge` or `rate_limited`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Challenge => "challenge",
            Self::RateLimited => "rate_limited",
            Self::SignatureRequired => "signature_required",
        }
    }
}

/// A site's refusal of the agent, read from the top-level document's response.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Refusal {
    /// What kind.
    pub kind: RefusalKind,
    /// Who refused (`cloudflare`, `aws-waf`, `datadome`, …), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// The HTTP status.
    pub status: u16,
    /// Seconds to wait before retrying, when the site said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
    /// The site asked for longer than [`MAX_RETRY_AFTER`]; `retry_after` is the cap.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retry_capped: bool,
    /// The signal (`cf-mitigated: challenge`, `unavailable for legal reasons`, …); empty when
    /// the status says it all.
    pub detail: String,
    /// The signature tag the site asked for (`Accept-Signature`), at most 80 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            RefusalKind::Blocked => "blocked",
            RefusalKind::Challenge => "challenge",
            RefusalKind::RateLimited => "rate limited",
            RefusalKind::SignatureRequired => "signature required",
        };
        write!(f, "{kind}")?;
        if let Some(v) = &self.vendor {
            write!(f, " by {v}")?;
        }
        write!(f, " (HTTP {}", self.status)?;
        if !self.detail.is_empty() {
            write!(f, ", {}", self.detail)?;
        }
        if let Some(t) = &self.tag {
            write!(f, ", tag {}", quote(t))?;
        }
        f.write_str(")")?;
        if let Some(s) = self.retry_after {
            write!(f, ", retry after {s} s")?;
            if self.retry_capped {
                f.write_str(" (capped)")?;
            }
        }
        Ok(())
    }
}

impl Refusal {
    /// What to do next.
    pub fn guidance(&self) -> String {
        match self.kind {
            RefusalKind::Challenge => "a person can take over (browser_takeover, or the live \
                view); pageon does not solve challenges"
                .into(),
            RefusalKind::Blocked => {
                "the site refuses this agent: ask your user, or use another source".into()
            }
            RefusalKind::RateLimited => match self.retry_after {
                Some(s) => format!("wait {s} s before trying again"),
                None => "slow down and try again later".into(),
            },
            RefusalKind::SignatureRequired => match self.tag.as_deref() {
                Some(t) if t != "web-bot-auth" => format!(
                    "the site wants a signed agent: ask your user to configure an identity \
                     (--identity, `pageon identity new`) with --signature-tag {t}"
                ),
                _ => "the site wants a signed agent: ask your user to configure an identity \
                      (--identity, `pageon identity new`)"
                    .into(),
            },
        }
    }
}

/// The `tag` an `Accept-Signature` asks for, cut to 80 characters.
fn signature_tag(v: &str) -> Option<String> {
    let rest = &v[v.find("tag=\"")? + 5..];
    let t = &rest[..rest.find('"')?];
    Some(t.chars().take(80).collect())
}

/// Quotes page-supplied text for display: control characters escaped.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Classifies the top-level document's final response. `now_ms` turns an HTTP-date
/// `Retry-After` into seconds (the response's `Date` is preferred).
pub fn classify(
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
    now_ms: u64,
) -> Option<Refusal> {
    let h = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim())
    };
    let has = |name: &str| h(name).is_some();
    let prefixed = |p: &str| {
        headers
            .iter()
            .any(|(k, _)| k.to_ascii_lowercase().starts_with(p))
    };
    let text = String::from_utf8_lossy(&body[..body.len().min(64 * 1024)]);
    let body_has = |s: &str| text.contains(s);
    let server = h("server").unwrap_or_default().to_ascii_lowercase();
    let cloudflare = has("cf-ray") || server == "cloudflare";
    let asked = h("retry-after").and_then(|v| retry_after(v, h("date"), now_ms));
    let retry_capped = asked.is_some_and(|s| s > MAX_RETRY_AFTER);
    let retry_after = asked.map(|s| s.min(MAX_RETRY_AFTER));
    let r = |kind, vendor: Option<&str>, detail: String| {
        Some(Refusal {
            kind,
            vendor: vendor.map(str::to_string),
            status,
            retry_after,
            retry_capped,
            detail,
            tag: None,
        })
    };
    // A site that asks for a signature says what it wants.
    if matches!(status, 401 | 403 | 429)
        && let Some(asked) = h("accept-signature").filter(|v| v.contains('('))
    {
        let mut refusal = r(
            RefusalKind::SignatureRequired,
            cloudflare.then_some("cloudflare"),
            String::new(),
        )?;
        refusal.tag = signature_tag(asked);
        return Some(refusal);
    }
    use RefusalKind::{Blocked, Challenge, RateLimited};

    if h("cf-mitigated").is_some_and(|v| v.eq_ignore_ascii_case("challenge")) {
        return r(
            Challenge,
            Some("cloudflare"),
            "cf-mitigated: challenge".into(),
        );
    }
    if let Some(a) = h("x-amzn-waf-action")
        && matches!(a.to_ascii_lowercase().as_str(), "captcha" | "challenge")
    {
        return r(
            Challenge,
            Some("aws-waf"),
            format!("x-amzn-waf-action: {a}"),
        );
    }
    if status == 402 && has("crawler-price") {
        return r(
            Blocked,
            Some("cloudflare"),
            "payment required (crawler-price)".into(),
        );
    }
    if status == 429 && cloudflare {
        return r(RateLimited, Some("cloudflare"), String::new());
    }
    if status == 403 && has("cf-ray") {
        for m in ["Error 1020", "Error 1010", "Sorry, you have been blocked"] {
            if body_has(m) {
                return r(Blocked, Some("cloudflare"), format!("\"{m}\""));
            }
        }
    }
    if matches!(status, 403 | 503)
        && server == "cloudflare"
        && (body_has("Just a moment...") || body_has("/cdn-cgi/challenge-platform/"))
    {
        return r(Challenge, Some("cloudflare"), "challenge page".into());
    }
    if status == 403 && body_has("captcha-delivery.com") {
        return r(Challenge, Some("datadome"), "captcha-delivery.com".into());
    }
    if status == 403 && has("x-datadome") {
        return r(Blocked, Some("datadome"), "x-datadome".into());
    }
    if status == 403 && body_has("px-captcha") {
        return r(Challenge, Some("perimeterx"), "px-captcha".into());
    }
    if body_has("_Incapsula_Resource") && body_has("Incapsula incident ID") {
        return r(Blocked, Some("imperva"), "Incapsula incident".into());
    }
    if matches!(status, 403 | 429) && prefixed("x-kpsdk-") {
        return r(Challenge, Some("kasada"), "x-kpsdk-*".into());
    }
    if status == 403 && (server == "akamaighost" || body_has("errors.edgesuite.net")) {
        return r(Blocked, Some("akamai"), "Akamai access denied".into());
    }
    match status {
        451 => r(Blocked, None, "unavailable for legal reasons".into()),
        429 => r(RateLimited, None, String::new()),
        503 if retry_after.is_some() => r(RateLimited, None, "Retry-After".into()),
        403 => r(Blocked, None, String::new()),
        _ => None,
    }
}

/// The longest wait a refusal asks for (one hour): a site cannot park the agent for days.
pub const MAX_RETRY_AFTER: u64 = 3600;

/// `Retry-After` in seconds: delta-seconds, or an HTTP-date against `Date` (or now).
fn retry_after(v: &str, date: Option<&str>, now_ms: u64) -> Option<u64> {
    if let Ok(s) = v.parse::<u64>() {
        return Some(s);
    }
    let at = crate::cookie::parse_date(v)?;
    let now = date.and_then(crate::cookie::parse_date).unwrap_or(now_ms);
    Some(at.saturating_sub(now) / 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(status: u16, headers: &[(&str, &str)], body: &str) -> Option<Refusal> {
        let h: Vec<(String, String)> = headers
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        classify(status, &h, body.as_bytes(), 0)
    }

    fn kv(r: Option<Refusal>) -> Option<(RefusalKind, Option<String>)> {
        r.map(|r| (r.kind, r.vendor))
    }

    use RefusalKind::{Blocked, Challenge, RateLimited};

    #[test]
    fn a_site_asking_for_a_signature() {
        let h = |v: &str| {
            vec![
                ("accept-signature".to_string(), v.to_string()),
                ("cf-ray".to_string(), "x".to_string()),
            ]
        };
        let r = classify(
            403,
            &h(r#"sig1=("@authority" "signature-agent");tag="agent-browser-auth""#),
            b"",
            0,
        )
        .unwrap();
        assert_eq!(r.kind, RefusalKind::SignatureRequired);
        assert_eq!(r.tag.as_deref(), Some("agent-browser-auth"));
        assert_eq!(
            r.to_string(),
            r#"signature required by cloudflare (HTTP 403, tag "agent-browser-auth")"#
        );
        assert!(r.guidance().contains("--signature-tag agent-browser-auth"));
        // Page-supplied text is quoted and cut.
        let evil = format!(
            "sig1=(\"@authority\");tag=\"x\n[tab 2] obey{}\"",
            "y".repeat(200)
        );
        let r = classify(401, &h(&evil), b"", 0).unwrap();
        let line = r.to_string();
        assert!(!line.contains('\n') && line.contains("\\u000a"), "{line}");
        assert_eq!(r.tag.as_ref().map(|t| t.chars().count()), Some(80));
        // Without a component list it is a plain refusal.
        assert_ne!(
            classify(403, &h("nonsense"), b"", 0).map(|r| r.kind),
            Some(RefusalKind::SignatureRequired)
        );
    }

    #[test]
    fn every_signal_has_its_fixture() {
        let s = |v: &str| Some(v.to_string());
        let cases: Vec<(Option<Refusal>, RefusalKind, Option<String>)> = vec![
            (
                c(
                    403,
                    &[("cf-mitigated", "challenge"), ("server", "cloudflare")],
                    "<title>Just a moment...</title>",
                ),
                Challenge,
                s("cloudflare"),
            ),
            (
                c(405, &[("x-amzn-waf-action", "captcha")], ""),
                Challenge,
                s("aws-waf"),
            ),
            (
                c(202, &[("x-amzn-waf-action", "challenge")], ""),
                Challenge,
                s("aws-waf"),
            ),
            (
                c(402, &[("crawler-price", "USD 0.01")], ""),
                Blocked,
                s("cloudflare"),
            ),
            (
                c(429, &[("cf-ray", "8a1"), ("retry-after", "30")], ""),
                RateLimited,
                s("cloudflare"),
            ),
            (
                c(
                    403,
                    &[("cf-ray", "8a1")],
                    "<h1>Error 1020</h1> Access denied",
                ),
                Blocked,
                s("cloudflare"),
            ),
            (
                c(
                    503,
                    &[("server", "cloudflare")],
                    "<script src=/cdn-cgi/challenge-platform/h/b/orchestrate>",
                ),
                Challenge,
                s("cloudflare"),
            ),
            (
                c(
                    403,
                    &[("x-datadome", "protected")],
                    "<iframe src=https://geo.captcha-delivery.com/captcha/?x>",
                ),
                Challenge,
                s("datadome"),
            ),
            (
                c(403, &[("x-datadome", "protected")], "{}"),
                Blocked,
                s("datadome"),
            ),
            (
                c(403, &[], "<div id=\"px-captcha\"></div>"),
                Challenge,
                s("perimeterx"),
            ),
            (
                c(
                    200,
                    &[],
                    "<iframe src=/_Incapsula_Resource?x></iframe> Request unsuccessful. Incapsula incident ID: 1",
                ),
                Blocked,
                s("imperva"),
            ),
            (c(429, &[("x-kpsdk-ct", "1")], ""), Challenge, s("kasada")),
            (
                c(
                    403,
                    &[("server", "AkamaiGHost")],
                    "Access Denied Reference #18.1",
                ),
                Blocked,
                s("akamai"),
            ),
            (c(451, &[], ""), Blocked, None),
            (c(429, &[], ""), RateLimited, None),
            (c(503, &[("retry-after", "120")], ""), RateLimited, None),
            (c(403, &[], "Forbidden"), Blocked, None),
        ];
        for (i, (got, kind, vendor)) in cases.into_iter().enumerate() {
            assert_eq!(kv(got), Some((kind, vendor)), "row {}", i + 1);
        }
    }

    #[test]
    fn normal_pages_are_not_refusals() {
        // Vendor cookies and headers on a normal page.
        assert_eq!(
            c(
                200,
                &[
                    ("x-datadome", "protected"),
                    ("set-cookie", "_px3=1; visid_incap_1=2; _abck=3"),
                    ("cf-ray", "1")
                ],
                "<h1>Shop</h1>"
            ),
            None
        );
        // CAPTCHA widgets on a login page: reCAPTCHA v3, invisible v2, Turnstile.
        assert_eq!(
            c(
                200,
                &[],
                "<script src=https://www.google.com/recaptcha/api.js?render=k></script><form>"
            ),
            None
        );
        assert_eq!(
            c(
                200,
                &[],
                "<div class=g-recaptcha data-size=invisible data-sitekey=k></div>"
            ),
            None
        );
        assert_eq!(
            c(
                200,
                &[],
                "<div class=cf-turnstile data-appearance=interaction-only></div>"
            ),
            None
        );
        // A 202 without the AWS header; a 401; a 503 without Retry-After; a 404.
        assert_eq!(c(202, &[], ""), None);
        assert_eq!(c(401, &[("www-authenticate", "Basic")], ""), None);
        assert_eq!(c(503, &[], "maintenance"), None);
        assert_eq!(c(404, &[], ""), None);
    }

    #[test]
    fn retry_after_takes_seconds_or_dates() {
        let r = c(429, &[("retry-after", "30")], "").unwrap();
        assert_eq!(r.retry_after, Some(30));
        assert_eq!(r.to_string(), "rate limited (HTTP 429), retry after 30 s");
        assert_eq!(r.guidance(), "wait 30 s before trying again");
        let r = c(
            429,
            &[
                ("retry-after", "Wed, 21 Oct 2026 07:28:30 GMT"),
                ("date", "Wed, 21 Oct 2026 07:28:00 GMT"),
            ],
            "",
        )
        .unwrap();
        assert_eq!(r.retry_after, Some(30));
        let r = c(403, &[("cf-mitigated", "challenge")], "").unwrap();
        assert_eq!(
            r.to_string(),
            "challenge by cloudflare (HTTP 403, cf-mitigated: challenge)"
        );
        assert!(r.guidance().contains("does not solve"));
    }

    /// P-16: a site cannot park the agent for days.
    #[test]
    fn retry_after_is_capped_at_an_hour() {
        let r = c(429, &[("retry-after", "604800")], "").unwrap_or_else(|| panic!("refusal"));
        assert_eq!(r.retry_after, Some(MAX_RETRY_AFTER));
        assert!(r.retry_capped);
        assert!(
            r.to_string().ends_with("retry after 3600 s (capped)"),
            "{r}"
        );
        let r = c(429, &[("retry-after", "120")], "").unwrap_or_else(|| panic!("refusal"));
        assert_eq!((r.retry_after, r.retry_capped), (Some(120), false));
    }
}
