//! Quoting of page-authored text.
//!
//! Every string that comes from the page is written as a JSON-style double-quoted string, so
//! page text can never be read as engine vocabulary.

/// Quotes page text: `"` → `\"`, `\` → `\\`, newline → `\n`, other control characters →
/// `\uXXXX`. Runs of whitespace are kept as they are (callers normalise first if wanted).
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || c == '\u{2028}' || c == '\u{2029}' => {
                for unit in c.encode_utf16(&mut [0; 2]) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Collapses whitespace runs to single spaces and trims.
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalises and truncates to at most `max` characters, ending with `…` when cut.
pub fn clip(text: &str, max: usize) -> String {
    let text = normalize(text);
    if text.chars().count() <= max {
        return text;
    }
    let mut s: String = text.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_round_trip_through_json() {
        for s in [
            "plain",
            "a \"b\" c",
            "back\\slash",
            "line\nbreak",
            "ctl\u{1}x",
            "ls\u{2028}x",
            "emoji 🎉 ‏עברית",
        ] {
            let q = quote(s);
            assert!(q.starts_with('"') && q.ends_with('"'));
            let back: String = serde_json::from_str(&q).unwrap_or_else(|e| panic!("{q}: {e}"));
            assert_eq!(back, s);
        }
    }

    #[test]
    fn engine_vocabulary_stays_inside_quotes() {
        assert_eq!(quote(r#"button e99 "Pay""#), r#""button e99 \"Pay\"""#);
    }

    #[test]
    fn clip_marks_cuts() {
        assert_eq!(clip("  a   b ", 10), "a b");
        assert_eq!(clip("abcdef", 4), "abc…");
    }
}
