//! robots.txt (RFC 9309), for the pages an agent opens: groups selected by product
//! token, the longest matching rule wins, `allow` wins ties, `*` and `$` patterns.

/// The rules that apply to one product token, for one origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Robots {
    /// No robots.txt (4xx), or no rules for the token: everything is allowed.
    AllowAll,
    /// robots.txt was unreachable (5xx or a network error): nothing is allowed (RFC 9309 2.3.1.4).
    DisallowAll(String),
    /// The rules of the token's groups (or of `*`).
    Rules(Vec<Rule>),
}

/// One `allow`, `disallow` or `content-usage` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Allow (true) or disallow.
    pub allow: bool,
    /// The path pattern, percent-encoding normalized.
    pub pattern: String,
    /// A `Content-Usage` line's preferences (IETF aipref), as written; `None` for
    /// allow and disallow lines.
    pub usage: Option<String>,
}

/// The parse limit (RFC 9309 2.5: at least 500 KiB).
pub const MAX_BYTES: usize = 500 * 1024;

impl Robots {
    /// What a robots.txt fetch that ended with `status` means, when not 2xx.
    pub fn from_status(status: u16) -> Option<Self> {
        match status {
            200..=299 => None,
            400..=499 => Some(Self::AllowAll),
            _ => Some(Self::DisallowAll(format!(
                "robots.txt is unreachable (HTTP {status})"
            ))),
        }
    }

    /// The rules of `body` for `token` (case-insensitive; `pageon`).
    pub fn parse(body: &[u8], token: &str) -> Self {
        let body = &body[..body.len().min(MAX_BYTES)];
        let text = String::from_utf8_lossy(body);
        // (agents, rules) per group; consecutive user-agent lines share a group.
        let mut groups: Vec<(Vec<String>, Vec<Rule>)> = Vec::new();
        let mut in_agents = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or_default();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    if !in_agents {
                        groups.push((Vec::new(), Vec::new()));
                        in_agents = true;
                    }
                    let agent = value
                        .split(|c: char| c == '/' || c.is_whitespace())
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if let Some(g) = groups.last_mut() {
                        g.0.push(agent);
                    }
                }
                "allow" | "disallow" => {
                    in_agents = false;
                    if value.is_empty() {
                        continue;
                    }
                    if let Some(g) = groups.last_mut() {
                        g.1.push(Rule {
                            allow: key == "allow",
                            pattern: normalize(value),
                            usage: None,
                        });
                    }
                }
                // `Content-Usage: [path] prefs` (the aipref attachment draft).
                "content-usage" => {
                    in_agents = false;
                    let (pattern, prefs) = match value.split_once(char::is_whitespace) {
                        Some((p, rest)) if p.starts_with('/') => (normalize(p), rest.trim()),
                        _ => ("/".to_string(), value),
                    };
                    if let Some(g) = groups.last_mut()
                        && !prefs.is_empty()
                    {
                        g.1.push(Rule {
                            allow: true,
                            pattern,
                            usage: Some(prefs.chars().take(200).collect()),
                        });
                    }
                }
                _ => in_agents = false,
            }
        }
        let token = token.to_ascii_lowercase();
        let pick = |name: &str| -> Vec<Rule> {
            groups
                .iter()
                .filter(|(agents, _)| agents.iter().any(|a| a == name))
                .flat_map(|(_, rules)| rules.iter().cloned())
                .collect()
        };
        let mine = pick(&token);
        let has_mine = groups.iter().any(|(a, _)| a.contains(&token));
        let rules = if has_mine { mine } else { pick("*") };
        if rules.is_empty() {
            Self::AllowAll
        } else {
            Self::Rules(rules)
        }
    }

    /// Why `path` (path and query, as in the URL) is disallowed: the deciding rule, or `None`
    /// when it is allowed.
    pub fn disallowed(&self, path: &str) -> Option<String> {
        let rules = match self {
            Self::AllowAll => return None,
            Self::DisallowAll(why) => return Some(why.clone()),
            Self::Rules(r) => r,
        };
        if path == "/robots.txt" {
            return None;
        }
        let path = normalize(path);
        let best = rules
            .iter()
            .filter(|r| r.usage.is_none() && matches(r.pattern.as_bytes(), path.as_bytes()))
            .max_by_key(|r| (r.pattern.len(), r.allow))?;
        (!best.allow).then(|| format!("rule \"Disallow: {}\"", best.pattern))
    }

    /// The `Content-Usage` preferences for `path`: the longest matching rule's.
    pub fn usage(&self, path: &str) -> Option<&str> {
        let Self::Rules(rules) = self else {
            return None;
        };
        let path = normalize(path);
        rules
            .iter()
            .filter(|r| r.usage.is_some() && matches(r.pattern.as_bytes(), path.as_bytes()))
            .max_by_key(|r| r.pattern.len())
            .and_then(|r| r.usage.as_deref())
    }
}

/// Normalizes a pattern or path for comparison (RFC 9309 2.2.2): escapes of unreserved
/// characters are decoded (`/%70rivate` is `/private`), other escapes are upper-cased, and
/// non-ASCII is percent-encoded, so both compare as octets.
fn normalize(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            if v.is_ascii_alphanumeric() || b"-._~".contains(&v) {
                out.push(v as char);
            } else {
                out.push('%');
                out.push_str(&hex.to_ascii_uppercase());
            }
            i += 3;
            continue;
        }
        if c >= 0x80 {
            out.push_str(&format!("%{c:02X}"));
        } else {
            out.push(c as char);
        }
        i += 1;
    }
    out
}

/// Whether a pattern (`*` any run, a trailing `$` the end) matches the start of `path`.
fn matches(pattern: &[u8], path: &[u8]) -> bool {
    let (pat, anchored) = match pattern.split_last() {
        Some((b'$', rest)) => (rest, true),
        _ => (pattern, false),
    };
    // The path positions a prefix of the pattern can reach.
    let mut pos = vec![0usize];
    for &c in pat {
        let next: Vec<usize> = if c == b'*' {
            let from = pos.iter().copied().min().unwrap_or(0);
            (from..=path.len()).collect()
        } else {
            let mut n: Vec<usize> = pos
                .iter()
                .filter(|&&i| i < path.len() && path[i] == c)
                .map(|&i| i + 1)
                .collect();
            n.dedup();
            n
        };
        if next.is_empty() {
            return false;
        }
        pos = next;
    }
    !anchored || pos.contains(&path.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_usage_rules_are_read_by_path() {
        let r = Robots::parse(
            b"User-Agent: *\nContent-Usage: train-ai=n\nContent-Usage: /blog/ train-ai=y, search=y\nDisallow: /admin\n",
            "pageon",
        );
        assert_eq!(r.usage("/"), Some("train-ai=n"));
        assert_eq!(r.usage("/blog/post"), Some("train-ai=y, search=y"));
        assert!(r.disallowed("/blog/post").is_none());
        assert!(r.disallowed("/admin/x").is_some());
        let only = Robots::parse(b"User-Agent: *\nContent-Usage: train-ai=n\n", "pageon");
        assert!(only.disallowed("/x").is_none());
        assert_eq!(only.usage("/x"), Some("train-ai=n"));
    }

    fn r(body: &str) -> Robots {
        Robots::parse(body.as_bytes(), "pageon")
    }

    #[test]
    fn rfc9309_examples() {
        // RFC 9309 5.1 (simple) and 2.2.2/2.2.3 (special characters).
        let robots = r(
            "User-Agent: *\nDisallow: *.gif$\nDisallow: /example/\nAllow: /publications/\n\n\
                        User-Agent: foobot\nDisallow:/\nAllow:/example/page.html\nAllow:/example/allowed.gif\n\n\
                        User-Agent: barbot\nUser-Agent: bazbot\nDisallow: /example/page.html\n\n\
                        User-Agent: quxbot\n",
        );
        assert!(robots.disallowed("/example/x").is_some());
        assert!(robots.disallowed("/a/b.gif").is_some());
        assert!(robots.disallowed("/a/b.gifx").is_none());
        assert!(robots.disallowed("/publications/").is_none());
        assert!(robots.disallowed("/").is_none());
        let foobot = Robots::parse(
            b"User-Agent: foobot\nDisallow:/\nAllow:/example/page.html\n",
            "FooBot",
        );
        assert!(foobot.disallowed("/example/page.html").is_none());
        assert!(foobot.disallowed("/other").is_some());
        // Groups sharing user-agent lines; an empty group allows all.
        let baz = Robots::parse(
            b"User-Agent: barbot\nUser-Agent: bazbot\nDisallow: /example/page.html\n",
            "bazbot",
        );
        assert!(baz.disallowed("/example/page.html").is_some());
        assert_eq!(
            Robots::parse(b"User-Agent: quxbot\n", "quxbot"),
            Robots::AllowAll
        );
    }

    #[test]
    fn longest_match_wins_and_allow_wins_ties() {
        let robots = r(
            "User-agent: pageon\nDisallow: /shop\nAllow: /shop/public\nAllow: /x\nDisallow: /x\n",
        );
        assert!(robots.disallowed("/shop/cart").is_some());
        assert!(robots.disallowed("/shop/public/a").is_none());
        assert!(robots.disallowed("/x").is_none());
        assert_eq!(
            robots.disallowed("/shop/cart"),
            Some("rule \"Disallow: /shop\"".into())
        );
    }

    #[test]
    fn groups_are_merged_and_star_is_the_fallback() {
        let robots = r(
            "User-agent: pageon\nDisallow: /a\n\nUser-agent: *\nDisallow: /b\n\nuser-agent: PAGEON/2.0\nDisallow: /c\n",
        );
        assert!(robots.disallowed("/a").is_some());
        assert!(robots.disallowed("/c").is_some());
        assert!(
            robots.disallowed("/b").is_none(),
            "the * group does not apply when ours exists"
        );
        let star = r("User-agent: *\nDisallow: /b # comment\n");
        assert!(star.disallowed("/b/x").is_some());
        assert_eq!(r("User-agent: otherbot\nDisallow: /\n"), Robots::AllowAll);
        assert_eq!(r("garbage\n\u{0}\n"), Robots::AllowAll);
    }

    #[test]
    fn patterns_and_encoding() {
        let robots = r(
            "User-agent: *\nDisallow: /*/private$\nDisallow: /fish*.php\nDisallow: /ツ\nDisallow: /a%3cd\n",
        );
        assert!(robots.disallowed("/x/private").is_some());
        assert!(robots.disallowed("/x/private/y").is_none());
        assert!(robots.disallowed("/fish/salmon.php?id=1").is_some());
        assert!(
            robots.disallowed("/Fish.php").is_none(),
            "paths are case-sensitive"
        );
        assert!(robots.disallowed("/%E3%83%84").is_some());
        assert!(robots.disallowed("/a%3Cd").is_some());
        assert!(robots.disallowed("/robots.txt").is_none());
        let all = r("User-agent: *\nDisallow: /\n");
        assert!(all.disallowed("/robots.txt").is_none());
        assert!(all.disallowed("/index.html").is_some());
        assert_eq!(r("User-agent: *\nDisallow:\n"), Robots::AllowAll);
    }

    #[test]
    fn statuses_and_size() {
        assert_eq!(Robots::from_status(404), Some(Robots::AllowAll));
        assert!(Robots::from_status(503).unwrap().disallowed("/").is_some());
        assert_eq!(Robots::from_status(200), None);
        let mut big = "User-agent: *\n".to_string();
        big.push_str(&"# padding\n".repeat(MAX_BYTES / 10));
        big.push_str("Disallow: /late\n");
        assert!(
            r(&big).disallowed("/late").is_none(),
            "rules after 500 KiB are ignored"
        );
    }

    /// P-16: percent-encoded paths cannot bypass a rule, and encoded rules match.
    #[test]
    fn percent_escapes_of_unreserved_characters_are_decoded() {
        let r = Robots::parse(
            b"User-agent: *\nDisallow: /private\nDisallow: /a%2Fb\nDisallow: /%7Euser\n",
            "pageon",
        );
        for p in [
            "/private",
            "/%70rivate",
            "/%70%72ivate/x",
            "/~user",
            "/%7euser",
        ] {
            assert!(r.disallowed(p).is_some(), "{p}");
        }
        // A reserved character stays encoded: `/a%2Fb` is not `/a/b`.
        assert!(r.disallowed("/a%2fb").is_some());
        assert!(r.disallowed("/a/b").is_none());
        // An escape at the very end is read too.
        assert!(r.disallowed("/privat%65").is_some());
        assert!(r.disallowed("/public").is_none());
    }
}
