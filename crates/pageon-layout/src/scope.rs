//! Shadow-tree style scoping by selector rewriting.
//!
//! blitz styles one flat tree and does not implement shadow trees, so pageon lays out the
//! composed tree and marks it with mirror-only attributes: `pageon-scope=N` on the elements
//! of shadow tree N, `pageon-host=N` on its host, `pageon-slot-in=N` on the nodes slotted into
//! it. Selectors are then rewritten so that:
//!
//! - a shadow tree's rules match only inside it (every compound gets `[pageon-scope="N"]`),
//!   with `:host`, `:host(…)` and `::slotted(…)` mapped to the markers;
//! - document rules do not reach into shadow trees (the subject compound gets
//!   `:not([pageon-scope])`), and `::part(p)` reaches exposed parts.
//!
//! Inheritance and custom properties follow the flat tree, as the standard has them. Only
//! pages with shadow trees are rewritten.

/// Which rules a stylesheet holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The document's own stylesheets.
    Document,
    /// The stylesheets of shadow tree `N`.
    Shadow(u32),
}

/// Rewrites a stylesheet's selectors for `scope`.
pub fn rewrite(css: &str, scope: Scope) -> String {
    let mut out = String::with_capacity(css.len() + css.len() / 4);
    rules(css, &|p| selector_list(p, scope), &mut out);
    out
}

/// Pseudo-classes Blitz does not match, and what the mirror marks instead (mirror-only
/// attributes the page never sees).
const PSEUDO: &[(&str, &str)] = &[
    (":user-invalid", "[pageon-user-invalid]"),
    (":user-valid", "[pageon-user-valid]"),
    (":invalid", "[pageon-invalid]"),
    (":valid", "[pageon-valid]"),
    (":required", ":is(input,select,textarea)[required]"),
    (":optional", ":is(input,select,textarea):not([required])"),
];

/// Whether a stylesheet uses a pseudo-class the mirror marks (then [`pseudo`] rewrites it).
pub fn uses_pseudo(css: &str) -> bool {
    let lower = css.contains(":state(") || PSEUDO.iter().any(|(p, _)| css.contains(p));
    lower
        || (css.bytes().any(|b| b.is_ascii_uppercase()) && {
            let l = css.to_ascii_lowercase();
            l.contains(":state(") || PSEUDO.iter().any(|(p, _)| l.contains(p))
        })
}

/// Rewrites `:valid`, `:invalid`, `:user-valid`, `:user-invalid`, `:required`, `:optional` and
/// `:state(x)` in selectors to the mirror's markers; `None` when the sheet uses none.
pub fn pseudo(css: &str) -> Option<String> {
    if !uses_pseudo(css) {
        return None;
    }
    let mut out = String::with_capacity(css.len() + 64);
    rules(css, &pseudo_selector, &mut out);
    Some(out)
}

fn pseudo_selector(sel: &str) -> String {
    let b = sel.as_bytes();
    let mut out = String::with_capacity(sel.len() + 32);
    let mut i = 0;
    'scan: while i < b.len() {
        let c = b[i];
        if c == b':' && (i == 0 || b[i - 1] != b':') && b.get(i + 1) != Some(&b':') {
            let rest = &sel[i..];
            if rest.len() >= 7
                && rest[..7].eq_ignore_ascii_case(":state(")
                && let Some(close) = rest.find(')')
            {
                let name = rest[7..close].trim().trim_matches(['"', '\'']);
                out.push_str("[pageon-state~=\"");
                out.extend(name.chars().filter(|c| *c != '"' && *c != '\\'));
                out.push_str("\"]");
                i += close + 1;
                continue;
            }
            for (p, to) in PSEUDO {
                let n = p.len();
                if rest.len() >= n
                    && rest[..n].eq_ignore_ascii_case(p)
                    && !rest[n..].bytes().next().is_some_and(|x| {
                        x.is_ascii_alphanumeric() || x == b'-' || x == b'_' || x == b'('
                    })
                {
                    out.push_str(to);
                    i += n;
                    continue 'scan;
                }
            }
        }
        if c == b'"' || c == b'\'' {
            let end = css_string_end(sel, i);
            out.push_str(&sel[i..end]);
            i = end;
            continue;
        }
        let ch = sel[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// At-rules whose blocks hold rules (rewritten); other at-rule blocks are kept as they are.
const GROUPING: &[&str] = &[
    "media",
    "supports",
    "layer",
    "container",
    "document",
    "scope",
    "starting-style",
];

fn rules(css: &str, prelude_of: &dyn Fn(&str) -> String, out: &mut String) {
    let b = css.as_bytes();
    let mut i = 0;
    while i < b.len() {
        // The prelude: up to `{` or `;` at depth 0.
        let start = i;
        let (end, term) = scan_to(css, i, b"{;");
        let prelude = &css[start..end];
        match term {
            Some(b';') => {
                out.push_str(&css[start..=end]);
                i = end + 1;
            }
            Some(_) => {
                let close = block_end(css, end);
                let body = &css[end + 1..close.min(b.len())];
                let trimmed = prelude.trim_start();
                if let Some(at) = trimmed.strip_prefix('@') {
                    let name: String = at
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .collect::<String>()
                        .to_ascii_lowercase();
                    out.push_str(prelude);
                    out.push('{');
                    if GROUPING.contains(&name.as_str()) {
                        rules(body, prelude_of, out);
                    } else {
                        out.push_str(body);
                    }
                    out.push('}');
                } else {
                    out.push_str(&prelude_of(prelude));
                    out.push('{');
                    out.push_str(body);
                    out.push('}');
                }
                i = close + 1;
            }
            None => {
                out.push_str(&css[start..]);
                break;
            }
        }
    }
}

/// The index of the first byte in `stops` at bracket depth 0 from `i` (skipping strings and
/// comments), and which byte it was.
fn scan_to(css: &str, mut i: usize, stops: &[u8]) -> (usize, Option<u8>) {
    let b = css.as_bytes();
    let mut depth = 0i32;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = css[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                continue;
            }
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            c if depth <= 0 && stops.contains(&c) => return (i, Some(c)),
            _ => {}
        }
        i += 1;
    }
    (b.len(), None)
}

/// The index of the `}` closing the block opened at `open`.
fn block_end(css: &str, open: usize) -> usize {
    let b = css.as_bytes();
    let mut depth = 0i32;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = css[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
                continue;
            }
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Splits at `sep` bytes at depth 0.
fn split_top(s: &str, sep: u8) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    loop {
        let (at, hit) = scan_to(s, start, &[sep]);
        parts.push(&s[start..at]);
        if hit.is_none() {
            return parts;
        }
        start = at + 1;
    }
}

/// The text without comments (outside strings).
fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i = s[i + 2..].find("*/").map_or(b.len(), |e| i + 2 + e + 2);
            continue;
        }
        if b[i] == b'"' || b[i] == b'\'' {
            let end = css_string_end(s, i);
            out.push_str(&s[i..end]);
            i = end;
            continue;
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn selector_list(list: &str, scope: Scope) -> String {
    let list = strip_comments(list);
    split_top(&list, b',')
        .into_iter()
        .map(|s| complex(s.trim(), scope))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A complex selector as compounds and the combinators between them.
fn compounds(sel: &str) -> Vec<(String, String)> {
    // (combinator before, compound)
    let b = sel.as_bytes();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut comb = String::new();
    let mut cur = String::new();
    let mut i = 0;
    let mut depth = 0i32;
    while i < b.len() {
        let c = b[i];
        match c {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            _ => {}
        }
        if depth == 0 && (c.is_ascii_whitespace() || matches!(c, b'>' | b'+' | b'~')) {
            if !cur.is_empty() {
                out.push((std::mem::take(&mut comb), std::mem::take(&mut cur)));
            }
            if !c.is_ascii_whitespace() {
                comb = (c as char).to_string();
            } else if comb.is_empty() {
                comb = " ".into();
            }
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' {
            let end = css_string_end(sel, i);
            cur.push_str(&sel[i..end]);
            i = end;
            continue;
        }
        let ch = sel[i..].chars().next().unwrap_or(' ');
        cur.push(ch);
        i += ch.len_utf8();
    }
    if !cur.is_empty() {
        out.push((comb, cur));
    }
    out
}

fn css_string_end(s: &str, i: usize) -> usize {
    let b = s.as_bytes();
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() && b[j] != q {
        if b[j] == b'\\' {
            j += 1;
        }
        j += 1;
    }
    (j + 1).min(b.len())
}

/// `(base, pseudo-element)`: the compound split at its first `::` at depth 0.
fn split_pseudo_element(c: &str) -> (&str, &str) {
    let b = c.as_bytes();
    let mut depth = 0i32;
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b':' if depth == 0 && b.get(i + 1) == Some(&b':') => return (&c[..i], &c[i..]),
            _ => {}
        }
    }
    (c, "")
}

/// `(…)` at the start of `s`: its contents and what follows the closing parenthesis.
fn paren(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix('(')?;
    let mut depth = 0i32;
    for (i, c) in rest.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ']' => depth -= 1,
            ')' if depth == 0 => return Some((&rest[..i], &rest[i + 1..])),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn complex(sel: &str, scope: Scope) -> String {
    if sel.is_empty() {
        return String::new();
    }
    let parts = compounds(sel);
    let last = parts.len().saturating_sub(1);
    let mut out = String::new();
    for (i, (comb, comp)) in parts.iter().enumerate() {
        match comb.as_str() {
            "" => {}
            " " => out.push(' '),
            c => {
                out.push(' ');
                out.push_str(c);
                out.push(' ');
            }
        }
        out.push_str(&match scope {
            Scope::Shadow(n) => shadow_compound(comp, n),
            Scope::Document if i == last => document_subject(comp),
            Scope::Document => comp.clone(),
        });
    }
    out
}

fn shadow_compound(comp: &str, n: u32) -> String {
    let mark = format!("[pageon-scope=\"{n}\"]");
    let host = format!("[pageon-host=\"{n}\"]");
    let (base, pe) = split_pseudo_element(comp);
    if let Some(rest) = base.strip_prefix(":host-context") {
        // Approximated as `:host` (the context is ignored).
        let after = paren(rest).map_or(rest, |(_, a)| a);
        return format!("{host}{after}{pe}");
    }
    if let Some(rest) = base.strip_prefix(":host") {
        if let Some((arg, after)) = paren(rest) {
            return format!("{host}{arg}{after}{pe}");
        }
        return format!("{host}{rest}{pe}");
    }
    if let Some(p) = pe.strip_prefix("::slotted")
        && let Some((arg, after)) = paren(p)
    {
        let slotted = format!("{}[pageon-slot-in=\"{n}\"]{after}", arg.trim());
        return if base.is_empty() {
            slotted
        } else {
            format!("{base}{mark} > {slotted}")
        };
    }
    format!("{base}{mark}{pe}")
}

fn document_subject(comp: &str) -> String {
    let (base, pe) = split_pseudo_element(comp);
    if let Some(p) = pe.strip_prefix("::part")
        && let Some((arg, after)) = paren(p)
    {
        let names: String = arg
            .split_whitespace()
            .map(|n| format!("[part~=\"{n}\"]"))
            .collect();
        return format!("{base} {names}{after}");
    }
    format!("{base}:not([pageon-scope]){pe}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_rules() {
        let s = Scope::Shadow(2);
        assert_eq!(
            rewrite("button{color:red}", s),
            "button[pageon-scope=\"2\"]{color:red}"
        );
        assert_eq!(
            rewrite(".a > .b, p::before{x:y}", s),
            ".a[pageon-scope=\"2\"] > .b[pageon-scope=\"2\"], p[pageon-scope=\"2\"]::before{x:y}"
        );
        assert_eq!(
            rewrite(":host{display:block}", s),
            "[pageon-host=\"2\"]{display:block}"
        );
        assert_eq!(
            rewrite(":host(.big:hover) span{a:b}", s),
            "[pageon-host=\"2\"].big:hover span[pageon-scope=\"2\"]{a:b}"
        );
        assert_eq!(
            rewrite("::slotted(h2){a:b}", s),
            "h2[pageon-slot-in=\"2\"]{a:b}"
        );
        assert_eq!(
            rewrite("slot[name=t]::slotted(*){a:b}", s),
            "slot[name=t][pageon-scope=\"2\"] > *[pageon-slot-in=\"2\"]{a:b}"
        );
        assert_eq!(
            rewrite("@media (min-width: 1px){p{a:b}}@keyframes k{from{a:b}}", s),
            "@media (min-width: 1px){p[pageon-scope=\"2\"]{a:b}}@keyframes k{from{a:b}}"
        );
        assert_eq!(
            rewrite("/* c{ */ a[title=\"x,y{\"]{content:\"}\"}", s),
            "a[title=\"x,y{\"][pageon-scope=\"2\"]{content:\"}\"}"
        );
    }

    #[test]
    fn document_rules() {
        let d = Scope::Document;
        assert_eq!(
            rewrite("main button:hover{c:d}", d),
            "main button:hover:not([pageon-scope]){c:d}"
        );
        assert_eq!(
            rewrite("x-tab::part(label){c:d}", d),
            "x-tab [part~=\"label\"]{c:d}"
        );
        assert_eq!(
            rewrite("@import url(a.css);p{c:d}", d),
            "@import url(a.css);p:not([pageon-scope]){c:d}"
        );
        assert_eq!(
            rewrite(":root{--x:1}", d),
            ":root:not([pageon-scope]){--x:1}"
        );
    }
}
