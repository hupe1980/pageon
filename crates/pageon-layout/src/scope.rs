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

/// Rewrites of stylesheet text, kept per thread: a page that measures itself as it changes
/// lays out again and again with the same sheets, and rewriting them dominated that.
mod memo {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    /// Most bytes of rewritten text kept; past it the cache starts over.
    const MAX_BYTES: usize = 64 << 20;

    #[derive(Default)]
    struct Memo {
        map: HashMap<(u64, usize, u64), Option<Rc<str>>>,
        bytes: usize,
    }

    thread_local! {
        static MEMO: RefCell<Memo> = RefCell::new(Memo::default());
    }

    /// A fast hash of a sheet's text (a word at a time, as FxHash): the key of every memo,
    /// computed at each layout for each sheet. Not keyed: the text is the page's own.
    pub(super) fn fast_hash(s: &str) -> u64 {
        const K: u64 = 0x517c_c1b7_2722_0a95;
        let b = s.as_bytes();
        let mut h: u64 = b.len() as u64;
        let (words, rest) = b.as_chunks::<8>();
        for w in words {
            h = (h.rotate_left(5) ^ u64::from_le_bytes(*w)).wrapping_mul(K);
        }
        for &x in rest {
            h = (h.rotate_left(5) ^ u64::from(x)).wrapping_mul(K);
        }
        h
    }

    /// `f(css)`, remembered by the text and `kind` (which rewrite, which scope).
    pub(super) fn get(
        css: &str,
        kind: u64,
        f: impl FnOnce(&str) -> Option<String>,
    ) -> Option<Rc<str>> {
        let key = (fast_hash(css), css.len(), kind);
        if let Some(v) = MEMO.with(|m| m.borrow().map.get(&key).cloned()) {
            return v;
        }
        let v: Option<Rc<str>> = f(css).map(Rc::from);
        MEMO.with(|m| {
            let mut m = m.borrow_mut();
            let size = v.as_ref().map_or(0, |s| s.len()) + 32;
            if m.bytes + size > MAX_BYTES {
                m.map.clear();
                m.bytes = 0;
            }
            m.bytes += size;
            m.map.insert(key, v.clone());
        });
        v
    }
}

/// [`pseudo`], remembered (the same sheets are rewritten at every layout).
pub fn pseudo_memo(css: &str) -> Option<std::rc::Rc<str>> {
    memo::get(css, 0, pseudo)
}

/// The classes and ids of the document being laid out, for [`prune_memo`].
pub struct Tokens {
    classes: std::collections::HashSet<String>,
    ids: std::collections::HashSet<String>,
    fingerprint: u64,
}

impl Tokens {
    /// The classes and ids of every element of `dom` (shadow trees included when `dom` is the
    /// composed tree); `None` in quirks mode, where they match without regard to case.
    pub fn of(dom: &pageon_dom::Dom) -> Option<Self> {
        if dom.quirks_mode == pageon_dom::QuirksMode::Quirks {
            return None;
        }
        let mut classes = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        for e in dom.traverse(dom.root()) {
            if let pageon_dom::Edge::Enter(id) = e
                && let Some(n) = dom.get(id)
            {
                if let Some(c) = n.attr("class") {
                    classes.extend(c.split_ascii_whitespace().map(str::to_string));
                }
                if let Some(i) = n.attr("id") {
                    ids.insert(i.to_string());
                }
            }
        }
        let mut all: Vec<&String> = classes.iter().chain(ids.iter()).collect();
        all.sort_unstable();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&(classes.len(), &all), &mut h);
        Some(Self {
            classes,
            ids,
            fingerprint: std::hash::Hasher::finish(&h) | 1 << 63,
        })
    }
}

/// [`prune`] for the document's tokens, remembered (`None`: nothing to drop).
pub fn prune_memo(css: &str, tokens: &Tokens) -> Option<std::rc::Rc<str>> {
    memo::get(css, tokens.fingerprint, |c| prune(c, tokens))
}

/// The stylesheet without the style rules that cannot match the document: rules whose every
/// selector requires a class or id no element has (outside `:is()`, `:not()` and the like,
/// which are kept). Such rules are most of a large design system's sheet; dropping them before
/// the style engine parses them makes a layout of a page with such sheets several times
/// cheaper. `None` when nothing is dropped.
pub fn prune(css: &str, tokens: &Tokens) -> Option<String> {
    plan_of(css).apply(css, tokens)
}

/// A sheet taken apart once: pieces that are always kept (at-rule preludes and braces,
/// statements, other blocks) and style rules with what each of their selectors requires.
#[derive(Default)]
struct Plan {
    pieces: Vec<Piece>,
}

enum Piece {
    Keep(std::ops::Range<usize>),
    /// A style rule, kept if any selector's requirements are all in the document.
    Rule(std::ops::Range<usize>, Vec<Vec<Token>>),
}

/// A class (`true`) or id the selector needs.
type Token = (bool, String);

impl Plan {
    fn apply(&self, css: &str, tokens: &Tokens) -> Option<String> {
        let mut out = String::with_capacity(css.len() / 4);
        let mut dropped = false;
        for p in &self.pieces {
            match p {
                Piece::Keep(r) => out.push_str(&css[r.clone()]),
                Piece::Rule(r, sels) => {
                    let possible = sels.iter().any(|need| {
                        need.iter().all(|(class, name)| {
                            if *class {
                                tokens.classes.contains(name)
                            } else {
                                tokens.ids.contains(name)
                            }
                        })
                    });
                    if possible {
                        out.push_str(&css[r.clone()]);
                    } else {
                        dropped = true;
                    }
                }
            }
        }
        dropped.then_some(out)
    }
}

/// Plans by sheet (hash and length), and the bytes of text they stand for.
type Plans = (
    std::collections::HashMap<(u64, usize), std::rc::Rc<Plan>>,
    usize,
);

/// The plan of a sheet, made once per text (per thread).
fn plan_of(css: &str) -> std::rc::Rc<Plan> {
    thread_local! {
        static PLANS: std::cell::RefCell<Plans> =
            std::cell::RefCell::new((std::collections::HashMap::new(), 0));
    }
    let key = (memo::fast_hash(css), css.len());
    if let Some(p) = PLANS.with(|p| p.borrow().0.get(&key).cloned()) {
        return p;
    }
    let mut plan = Plan::default();
    plan_rules(css, 0, &mut plan);
    let plan = std::rc::Rc::new(plan);
    PLANS.with(|p| {
        let mut p = p.borrow_mut();
        // Plans hold ranges, not text: a bound on the texts they stand for is enough.
        if p.1 + css.len() > 256 << 20 {
            p.0.clear();
            p.1 = 0;
        }
        p.1 += css.len();
        p.0.insert(key, plan.clone());
    });
    plan
}

fn plan_rules(css: &str, base: usize, plan: &mut Plan) {
    let b = css.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let (end, term) = scan_to(css, i, b"{;");
        let prelude = &css[start..end];
        match term {
            Some(b';') => {
                plan.pieces.push(Piece::Keep(base + start..base + end + 1));
                i = end + 1;
            }
            Some(_) => {
                let close = block_end(css, end).min(b.len().saturating_sub(1));
                let trimmed = prelude.trim_start();
                if let Some(at) = trimmed.strip_prefix('@') {
                    let name: String = at
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .collect::<String>()
                        .to_ascii_lowercase();
                    if GROUPING.contains(&name.as_str()) && close > end {
                        plan.pieces.push(Piece::Keep(base + start..base + end + 1));
                        plan_rules(&css[end + 1..close], base + end + 1, plan);
                        plan.pieces
                            .push(Piece::Keep(base + close..base + close + 1));
                    } else {
                        plan.pieces
                            .push(Piece::Keep(base + start..base + close + 1));
                    }
                } else {
                    let list = strip_comments(prelude);
                    let sels = split_top(&list, b',').into_iter().map(required).collect();
                    plan.pieces
                        .push(Piece::Rule(base + start..base + close + 1, sels));
                }
                i = close + 1;
            }
            None => {
                plan.pieces.push(Piece::Keep(base + start..base + b.len()));
                break;
            }
        }
    }
}

/// The classes and ids a selector requires at its top level (inside `(...)` and `[...]`
/// anything may be optional).
fn required(sel: &str) -> Vec<Token> {
    let b = sel.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'\\' => {
                i += 2;
                continue;
            }
            q @ (b'"' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            c @ (b'.' | b'#') if depth == 0 => {
                let (name, next) = ident(sel, i + 1);
                if !name.is_empty() {
                    out.push((c == b'.', name));
                }
                i = next;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// A CSS identifier starting at `i`, unescaped (`md\:flex` is `md:flex`), and where it ends.
fn ident(s: &str, mut i: usize) -> (String, usize) {
    let b = s.as_bytes();
    let mut out = String::new();
    while i < b.len() {
        let c = b[i];
        if c == b'\\' {
            let hex: String = s[i + 1..]
                .chars()
                .take_while(char::is_ascii_hexdigit)
                .take(6)
                .collect();
            if hex.is_empty() {
                if let Some(ch) = s[i + 1..].chars().next() {
                    out.push(ch);
                    i += 1 + ch.len_utf8();
                } else {
                    i += 1;
                }
            } else {
                let v = u32::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
                out.push(
                    char::from_u32(v)
                        .filter(|&c| c != '\0')
                        .unwrap_or('\u{FFFD}'),
                );
                i += 1 + hex.len();
                if b.get(i).is_some_and(u8::is_ascii_whitespace) {
                    i += 1;
                }
            }
        } else if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80 {
            let ch = s[i..].chars().next().unwrap_or('\u{FFFD}');
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    (out, i)
}

/// [`uses_pseudo`], remembered.
pub fn uses_pseudo_memo(css: &str) -> bool {
    memo::get(css, u64::MAX, |c| uses_pseudo(c).then(String::new)).is_some()
}

/// [`rewrite`], remembered.
pub fn rewrite_memo(css: &str, scope: Scope) -> std::rc::Rc<str> {
    let kind = match scope {
        Scope::Document => 1,
        Scope::Shadow(n) => 2 + u64::from(n),
    };
    memo::get(css, kind, |c| Some(rewrite(c, scope))).unwrap_or_else(|| css.into())
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
    (":popover-open", "[pageon-popover-open]"),
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

#[cfg(test)]
mod prune_tests {
    use super::*;

    fn tokens(html: &str) -> Tokens {
        let dom = pageon_dom::parse_document(html, pageon_dom::ParseOptions::default());
        Tokens::of(&dom).unwrap_or_else(|| panic!("quirks"))
    }

    #[test]
    fn rules_that_cannot_match_are_dropped() {
        let t = tokens(r#"<!doctype html><div id=main class="btn md:flex"></div>"#);
        let css = ".btn{a:1}.nope{b:2}#main .x{c:3}#gone{d:4}.nope,.btn{e:5}\
                   :not(.nope){f:6}div:is(.nope){g:7}.md\\:flex{h:8}@media (x){.nope{i:9}.btn{j:10}}\
                   @keyframes k{from{k:11}}[class~=nope]{l:12}";
        let out = prune(css, &t).unwrap_or_else(|| panic!("nothing dropped"));
        for kept in ["a:1", "e:5", "f:6", "g:7", "h:8", "j:10", "k:11", "l:12"] {
            assert!(out.contains(kept), "{kept} dropped: {out}");
        }
        for gone in ["b:2", "c:3", "d:4", "i:9"] {
            assert!(!out.contains(gone), "{gone} kept: {out}");
        }
        assert_eq!(prune(".btn{a:1}", &t), None);
    }

    #[test]
    fn quirks_documents_are_not_pruned() {
        let dom = pageon_dom::parse_document("<div class=a>", pageon_dom::ParseOptions::default());
        assert!(Tokens::of(&dom).is_none());
    }
}
