//! Character encoding of a document, per the HTML standard's *encoding sniffing algorithm*:
//! byte-order mark, then the transport-layer charset, then the *prescan* of the first
//! 1024 bytes for `<meta>` declarations, then the default (`windows-1252`).

use encoding_rs::{Encoding as Enc, UTF_8, WINDOWS_1252};

/// Where the encoding came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A byte-order mark.
    Bom,
    /// The `charset` parameter of the HTTP `Content-Type`.
    Http,
    /// A `<meta>` declaration found by the prescan.
    Meta,
    /// Nothing declared: the default.
    Default,
}

impl Source {
    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bom => "bom",
            Self::Http => "http",
            Self::Meta => "meta",
            Self::Default => "default",
        }
    }
}

/// The encoding used to decode a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoding {
    /// WHATWG encoding name, e.g. `UTF-8`, `Shift_JIS`, `windows-1252`.
    pub name: &'static str,
    /// Where it came from.
    pub source: Source,
}

/// Decodes `bytes` to text and reports the encoding used. `http_charset` is the `charset`
/// parameter of the response's `Content-Type`, if any.
pub fn decode(bytes: &[u8], http_charset: Option<&str>) -> (String, Encoding) {
    let (enc, source, skip) = sniff(bytes, http_charset);
    let (text, _had_errors) = enc.decode_without_bom_handling(&bytes[skip..]);
    (
        text.into_owned(),
        Encoding {
            name: enc.name(),
            source,
        },
    )
}

fn sniff(bytes: &[u8], http_charset: Option<&str>) -> (&'static Enc, Source, usize) {
    if let Some((enc, len)) = Enc::for_bom(bytes) {
        return (enc, Source::Bom, len);
    }
    if let Some(enc) =
        http_charset.and_then(|c| Enc::for_label(c.trim().trim_matches('"').as_bytes()))
    {
        return (enc, Source::Http, 0);
    }
    if let Some(enc) = prescan(&bytes[..bytes.len().min(1024)]) {
        return (enc, Source::Meta, 0);
    }
    (WINDOWS_1252, Source::Default, 0)
}

/// The `charset` parameter of a `Content-Type` header value.
pub fn charset_param(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| v.trim().trim_matches('"'))
    })
}

const WS: [u8; 5] = [0x09, 0x0A, 0x0C, 0x0D, 0x20];

fn is_ws(b: u8) -> bool {
    WS.contains(&b)
}

fn starts_with_ci(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && hay[..needle.len()].eq_ignore_ascii_case(needle)
}

/// *Prescan a byte stream to determine its encoding* (HTML standard).
pub fn prescan(input: &[u8]) -> Option<&'static Enc> {
    let mut pos = 0;
    while pos < input.len() {
        let rest = &input[pos..];
        if rest.starts_with(b"<!--") {
            // Advance to the '>' of the first "-->" after the '<'.
            {
                let i = find(&input[pos + 2..], b"-->")?;
                pos = pos + 2 + i + 3
            }
            continue;
        }
        if starts_with_ci(rest, b"<meta") && rest.get(5).is_some_and(|&b| is_ws(b) || b == b'/') {
            pos += 6;
            if let Some(enc) = meta(input, &mut pos) {
                return Some(enc);
            }
            continue;
        }
        if rest.len() >= 2
            && rest[0] == b'<'
            && (rest[1].is_ascii_alphabetic()
                || (rest[1] == b'/' && rest.get(2).is_some_and(u8::is_ascii_alphabetic)))
        {
            pos += 2;
            while pos < input.len() && !is_ws(input[pos]) && input[pos] != b'>' {
                pos += 1;
            }
            while get_attribute(input, &mut pos).is_some() {}
            pos += 1;
            continue;
        }
        if rest.starts_with(b"<!") || rest.starts_with(b"</") || rest.starts_with(b"<?") {
            {
                let i = input[pos..].iter().position(|&b| b == b'>')?;
                pos += i + 1
            }
            continue;
        }
        pos += 1;
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn meta(input: &[u8], pos: &mut usize) -> Option<&'static Enc> {
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let mut got_pragma = false;
    let mut need_pragma: Option<bool> = None;
    let mut charset: Option<&'static Enc> = None;
    let mut charset_failed = false;
    while let Some((name, value)) = get_attribute(input, pos) {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        match name.as_slice() {
            b"http-equiv" => {
                if value == b"content-type" {
                    got_pragma = true;
                }
            }
            b"content" => {
                if charset.is_none()
                    && !charset_failed
                    && let Some(label) = charset_from_content(&value)
                {
                    charset = Enc::for_label(&label);
                    charset_failed = charset.is_none();
                    need_pragma = Some(true);
                }
            }
            b"charset" => {
                charset = Enc::for_label(&value);
                charset_failed = charset.is_none();
                need_pragma = Some(false);
            }
            _ => {}
        }
    }
    match need_pragma {
        None => return None,
        Some(true) if !got_pragma => return None,
        _ => {}
    }
    let enc = charset?;
    if enc.name().starts_with("UTF-16") {
        return Some(UTF_8);
    }
    if enc.name() == "x-user-defined" {
        return Some(WINDOWS_1252);
    }
    Some(enc)
}

/// *Get an attribute* (HTML standard, prescan); name and value are ASCII-lowercased.
fn get_attribute(input: &[u8], pos: &mut usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let at = |p: usize| input.get(p).copied();
    while at(*pos).is_some_and(|b| is_ws(b) || b == b'/') {
        *pos += 1;
    }
    let b = at(*pos)?;
    if b == b'>' {
        return None;
    }
    let mut name = Vec::new();
    let mut value = Vec::new();
    // Attribute name.
    loop {
        let b = at(*pos)?;
        if b == b'=' && !name.is_empty() {
            *pos += 1;
            break;
        }
        if is_ws(b) {
            while at(*pos).is_some_and(is_ws) {
                *pos += 1;
            }
            if at(*pos) != Some(b'=') {
                return Some((name, value));
            }
            *pos += 1;
            break;
        }
        if b == b'/' || b == b'>' {
            return Some((name, value));
        }
        name.push(b.to_ascii_lowercase());
        *pos += 1;
    }
    // Value.
    while at(*pos).is_some_and(is_ws) {
        *pos += 1;
    }
    let b = at(*pos)?;
    if b == b'"' || b == b'\'' {
        *pos += 1;
        loop {
            let c = at(*pos)?;
            *pos += 1;
            if c == b {
                return Some((name, value));
            }
            value.push(c.to_ascii_lowercase());
        }
    }
    if b == b'>' {
        return Some((name, value));
    }
    value.push(b.to_ascii_lowercase());
    *pos += 1;
    loop {
        let c = at(*pos)?;
        if is_ws(c) || c == b'>' {
            return Some((name, value));
        }
        value.push(c.to_ascii_lowercase());
        *pos += 1;
    }
}

/// *Extract a character encoding from a meta element*: the label after `charset=`.
fn charset_from_content(s: &[u8]) -> Option<Vec<u8>> {
    let mut i = 0;
    loop {
        let start = find(&s[i..], b"charset")? + i;
        i = start + 7;
        while s.get(i).is_some_and(|&b| is_ws(b)) {
            i += 1;
        }
        if s.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while s.get(i).is_some_and(|&b| is_ws(b)) {
            i += 1;
        }
        return match s.get(i) {
            Some(&q @ (b'"' | b'\'')) => {
                let end = s[i + 1..].iter().position(|&b| b == q)?;
                Some(s[i + 1..i + 1 + end].to_vec())
            }
            Some(_) => {
                let end = s[i..]
                    .iter()
                    .position(|&b| is_ws(b) || b == b';')
                    .unwrap_or(s.len() - i);
                Some(s[i..i + end].to_vec())
            }
            None => None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prescan_cases() {
        let p = |s: &str| prescan(s.as_bytes()).map(Enc::name);
        assert_eq!(p(r#"<meta charset="shift_jis">"#), Some("Shift_JIS"));
        assert_eq!(
            p(r#"<meta http-equiv="Content-Type" content="text/html; charset=windows-1251">"#),
            Some("windows-1251")
        );
        assert_eq!(
            p(r#"<meta content="text/html; charset=windows-1251">"#),
            None,
            "content without pragma"
        );
        assert_eq!(
            p(r#"<!-- <meta charset="koi8-r"> --><meta charset=utf-16le>"#),
            Some("UTF-8")
        );
        assert_eq!(
            p(r#"<html><head><title>x</title><meta charset='iso-8859-2'>"#),
            Some("ISO-8859-2")
        );
        assert_eq!(p("<p>no declaration</p>"), None);
    }

    #[test]
    fn charset_param_parses() {
        assert_eq!(charset_param("text/html; charset=\"UTF-8\""), Some("UTF-8"));
        assert_eq!(charset_param("text/html"), None);
    }
}
