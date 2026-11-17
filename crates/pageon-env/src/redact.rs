//! Credentials in URL parameters: reset and magic-link tokens, OAuth codes, signed-URL
//! signatures, session ids. Their values are replaced wherever a URL or URL-encoded text
//! appears, so views and recordings never carry them.

use std::borrow::Cow;

/// What replaces a value.
pub const REDACTED: &str = "[redacted]";

/// Values shorter than this are left alone: credentials are long, and a script's
/// `a?key=b:c` stays intact.
const MIN_VALUE: usize = 8;

/// Parts of a parameter name that make it a credential.
const CONTAINS: &[&str] = &[
    "token",
    "secret",
    "passw",
    "signature",
    "credential",
    "session",
    "apikey",
    "api_key",
    "api-key",
];

/// Whole parameter names that are credentials.
const EXACT: &[&str] = &[
    "code",
    "key",
    "sig",
    "sid",
    "auth",
    "authorization",
    "jwt",
    "ticket",
    "otp",
    "pwd",
    "phpsessid",
];

fn is_credential(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    let Ok(lower) = std::str::from_utf8(&lower) else {
        return false;
    };
    EXACT.contains(&lower) || CONTAINS.iter().any(|c| lower.contains(c))
}

fn name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'[' | b']')
}

fn value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'%' | b'+' | b'/' | b'=')
}

/// `text` with the value of every credential parameter (`?token=…`, `&code=…`, `;jsessionid=…`,
/// or a parameter at the start, as in a form body) replaced by `[redacted]`. A value counts
/// when it has at least 8 URL characters. Idempotent.
pub fn redact_params(text: &str) -> Cow<'_, str> {
    let b = text.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut i = 0;
    while let Some(off) = memchr(b'=', &b[i..]) {
        let eq = i + off;
        let mut start = eq;
        while start > 0 && name_byte(b[start - 1]) {
            start -= 1;
        }
        // A parameter starts the text or follows `?`, `&` or `;` (a path parameter). Code is
        // not a URL: `a && x=…`, `;this.x=…` (a dotted name) and `x=f(…)` are left alone.
        let boundary = start == 0
            || match b[start - 1] {
                b'?' => true,
                b'&' => start < 2 || b[start - 2] != b'&',
                b';' => !b[start..eq].contains(&b'.'),
                _ => false,
            };
        let mut end = eq + 1;
        while end < b.len() && value_byte(b[end]) {
            end += 1;
        }
        let call = b.get(end) == Some(&b'(');
        if boundary && !call && end - (eq + 1) >= MIN_VALUE && is_credential(&b[start..eq]) {
            let o = out.get_or_insert_with(|| String::with_capacity(text.len()));
            o.push_str(&text[copied..=eq]);
            o.push_str(REDACTED);
            copied = end;
        }
        i = end.max(eq + 1);
    }
    match out {
        None => Cow::Borrowed(text),
        Some(mut o) => {
            o.push_str(&text[copied..]);
            Cow::Owned(o)
        }
    }
}

/// Whether a recorded URL answers a request: equal, or equal once both are redacted (the
/// recording keeps the redacted form; the page may rebuild the real one).
pub(crate) fn same_url(recorded: &str, requested: &str) -> bool {
    recorded == requested || redact_params(recorded) == redact_params(requested)
}

fn memchr(needle: u8, hay: &[u8]) -> Option<usize> {
    hay.iter().position(|&b| b == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_values_are_redacted() {
        let r = |s| redact_params(s).into_owned();
        assert_eq!(
            r("https://a.test/reset?token=abc123def456&lang=en"),
            "https://a.test/reset?token=[redacted]&lang=en"
        );
        assert_eq!(
            r("/cb?state=xyz12345678&code=4/0AX4XfWh-Q#top"),
            "/cb?state=xyz12345678&code=[redacted]#top"
        );
        assert_eq!(
            r("<a href=\"/f?X-Amz-Signature=ab12cd34ef56&amp;X-Amz-Credential=AKIA1234%2F2026\">"),
            "<a href=\"/f?X-Amz-Signature=[redacted]&amp;X-Amz-Credential=[redacted]\">"
        );
        assert_eq!(
            r("/x;jsessionid=0123456789ABCDEF?a=1"),
            "/x;jsessionid=[redacted]?a=1"
        );
        assert_eq!(
            r("password=hunter2hunter2&user=ada"),
            "password=[redacted]&user=ada"
        );
    }

    #[test]
    fn other_text_is_left_alone() {
        for s in [
            "/search?q=tokenization+guide&page=2",
            "/x?code=12&key=en",
            "n?key=r:i",
            "author=someone-famous",
            "monkey=bananas123",
            "a=b",
            "",
            "token=",
            "?token=[redacted]",
            "e7=1;this.initializeSessionAttributes=function(e){}",
            "a&&sessionToken=somefunction",
            "?x=1&sessionToken=makeToken(x)",
        ] {
            assert!(matches!(redact_params(s), Cow::Borrowed(_)), "{s}");
        }
    }

    #[test]
    fn it_is_idempotent() {
        let once =
            redact_params("?access_token=eyJhbGciOiJIUzI1NiJ9.e30.x&sig=abcdefgh123").into_owned();
        assert_eq!(redact_params(&once), once.as_str());
        assert_eq!(once, "?access_token=[redacted]&sig=[redacted]");
    }
}
