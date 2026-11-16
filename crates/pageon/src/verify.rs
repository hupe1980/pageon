//! Expectations: what an action should have done, checked against engine facts, each answer
//! with its evidence.
//!
//! ```text
//! url /orders                  the page is there (a path or a URL; `*` matches anything)
//! see "Order confirmed"        the page's text says it (case and spacing ignored)
//! see button "Pay"             an element with that role and name
//! no "Error"                   the opposite of see
//! request POST /orders 2xx     the action sent it, and it answered so (status optional)
//! download *.pdf               the action downloaded a file whose name matches
//! ```

use std::fmt;

use crate::Outcome;

/// Expectations per action.
pub const MAX_EXPECTATIONS: usize = 16;
/// The longest expectation.
pub const MAX_LEN: usize = 300;

/// What an action should have done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// The page's URL matches.
    Url(String),
    /// The page shows it.
    See(Text),
    /// The page does not show it.
    No(Text),
    /// The action sent this request (and it answered with this status).
    Request {
        /// The method (`POST`).
        method: String,
        /// The URL or path pattern.
        pattern: String,
        /// `201`, or a class (`2xx`).
        status: Option<String>,
    },
    /// The action downloaded a file whose name matches.
    Download(String),
}

/// What `see` and `no` look for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text {
    /// Words in the page's text.
    Words(String),
    /// An element by role and name.
    Element {
        /// The role (`button`).
        role: String,
        /// The accessible name.
        name: String,
    },
}

/// One expectation, answered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Check {
    /// The expectation as written.
    pub expect: String,
    /// It held.
    pub ok: bool,
    /// What pageon saw.
    pub evidence: String,
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mark = if self.ok { '✓' } else { '✗' };
        write!(f, "{mark} {}: {}", self.expect, self.evidence)
    }
}

/// The page facts checks read.
pub trait Facts {
    /// The page's URL.
    fn url(&mut self) -> Option<String>;
    /// The whole page's agent view (every element and text run; text a person cannot see is
    /// marked `hidden-text` and does not count).
    fn text(&mut self) -> Result<String, String>;
    /// View lines of elements whose name contains `name`'s words, from the whole page.
    fn elements(&mut self, name: &str) -> Result<Vec<String>, String>;
}

const GRAMMAR: &str = "write url PATH, see \"TEXT\", see ROLE \"NAME\", no \"TEXT\", request METHOD PATH [STATUS], or download NAME";

fn quoted(s: &str) -> Option<String> {
    let s = s.trim();
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\"", "\""))
}

fn text(rest: &str) -> Option<Text> {
    if let Some(w) = quoted(rest) {
        return (!w.is_empty()).then_some(Text::Words(w));
    }
    let (role, name) = rest.trim().split_once(' ')?;
    let name = quoted(name)?;
    let role_ok = !role.is_empty() && role.bytes().all(|b| b.is_ascii_lowercase() || b == b'-');
    role_ok.then(|| Text::Element {
        role: role.to_string(),
        name,
    })
}

impl std::str::FromStr for Expect {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.len() > MAX_LEN {
            return Err(format!("an expectation is at most {MAX_LEN} characters"));
        }
        let bad = || format!("{s:?} is not an expectation: {GRAMMAR}");
        let (word, rest) = s.split_once(' ').unwrap_or((s, ""));
        let rest = rest.trim();
        match word {
            "url" if !rest.is_empty() => Ok(Self::Url(rest.to_string())),
            "see" => text(rest).map(Self::See).ok_or_else(bad),
            "no" => text(rest).map(Self::No).ok_or_else(bad),
            "download" if !rest.is_empty() => Ok(Self::Download(rest.to_string())),
            "request" => {
                let mut parts = rest.split_whitespace();
                let method = parts.next().ok_or_else(bad)?.to_ascii_uppercase();
                let pattern = parts.next().ok_or_else(bad)?.to_string();
                let status = parts.next().map(str::to_string);
                if parts.next().is_some()
                    || !method.bytes().all(|b| b.is_ascii_uppercase())
                    || status.as_deref().is_some_and(|st| {
                        st.len() != 3
                            || !st
                                .bytes()
                                .enumerate()
                                .all(|(i, b)| b.is_ascii_digit() || (i > 0 && b == b'x'))
                    })
                {
                    return Err(bad());
                }
                Ok(Self::Request {
                    method,
                    pattern,
                    status,
                })
            }
            _ => Err(bad()),
        }
    }
}

/// Parses expectations, at most [`MAX_EXPECTATIONS`].
pub fn parse_all<S: AsRef<str>>(list: &[S]) -> Result<Vec<Expect>, String> {
    if list.len() > MAX_EXPECTATIONS {
        return Err(format!(
            "at most {MAX_EXPECTATIONS} expectations per action"
        ));
    }
    list.iter().map(|s| s.as_ref().parse()).collect()
}

/// `*` matches any run of characters; everything else literally (linear time).
pub fn glob(pattern: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), s.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Whether `url` matches a URL pattern: a path pattern against the path, else against scheme,
/// host and path (query and fragment ignored either way).
fn url_matches(pattern: &str, url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return glob(pattern, url);
    };
    if pattern.starts_with('/') {
        glob(pattern, u.path())
    } else {
        glob(pattern, &u[..url::Position::AfterPath])
    }
}

fn normal(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// What the page says first (its first text runs, quoted), for evidence.
fn says(view: &str) -> String {
    let runs: Vec<String> = view
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('@') && !l.starts_with("hidden-text"))
        .filter_map(|l| {
            let (a, b) = (l.find('"')?, l.rfind('"')?);
            (b > a).then(|| l[a + 1..b].replace("\\\"", "\""))
        })
        .filter(|t| !t.is_empty())
        .take(3)
        .collect();
    pageon_agent::quote::quote(&pageon_agent::quote::clip(&runs.join(" · "), 80))
}

fn redact(url: &str) -> String {
    pageon_env::redact_params(url).into_owned()
}

/// Checks one expectation after an action (`outcome`: what it did, if it was an action).
pub fn check(
    expect: &Expect,
    written: &str,
    outcome: Option<&Outcome>,
    facts: &mut dyn Facts,
) -> Check {
    let (ok, evidence) = match expect {
        Expect::Url(pattern) => match facts.url() {
            Some(u) => (url_matches(pattern, &u), format!("at {}", redact(&u))),
            None => (false, "no page is open".into()),
        },
        Expect::See(t) | Expect::No(t) => {
            let want = matches!(expect, Expect::See(_));
            let (found, evidence) = match see(t, facts) {
                Ok(x) => x,
                Err(e) => (false, e),
            };
            (found == want, evidence)
        }
        Expect::Request {
            method,
            pattern,
            status,
        } => match outcome {
            None => (false, "not an action: nothing was sent".into()),
            Some(o) => request(o, method, pattern, status.as_deref()),
        },
        Expect::Download(pattern) => match outcome {
            None => (false, "not an action: nothing was downloaded".into()),
            Some(o) => match o.downloads.iter().find(|d| glob(pattern, &d.filename)) {
                Some(d) => (
                    true,
                    format!("downloaded {}", pageon_agent::quote::quote(&d.filename)),
                ),
                None if o.downloads.is_empty() => (false, "nothing was downloaded".into()),
                None => (
                    false,
                    format!(
                        "downloaded {}",
                        o.downloads
                            .iter()
                            .map(|d| pageon_agent::quote::quote(&d.filename))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
            },
        },
    };
    Check {
        expect: written.trim().to_string(),
        ok,
        evidence,
    }
}

/// Whether the page shows `t`, and the evidence either way.
fn see(t: &Text, facts: &mut dyn Facts) -> Result<(bool, String), String> {
    match t {
        Text::Words(w) => {
            let text: String = facts
                .text()?
                .lines()
                .filter(|l| !l.trim_start().starts_with("hidden-text"))
                .collect::<Vec<_>>()
                .join("\n");
            if text.trim().is_empty() {
                return Ok((false, "the page has no text".into()));
            }
            let found = normal(&text).contains(&normal(w));
            Ok(if found {
                (
                    true,
                    format!("{} is on the page", pageon_agent::quote::quote(w)),
                )
            } else {
                (false, format!("not on the page (it says {})", says(&text)))
            })
        }
        Text::Element { role, name } => {
            let lines = facts.elements(name)?;
            let hit = lines.iter().find(|l| {
                crate::workflow::element(l)
                    .is_some_and(|(r, _, n)| r == *role && normal(&n) == normal(name))
            });
            Ok(match hit {
                Some(l) => (true, l.trim().to_string()),
                None => {
                    let near: Vec<&str> = lines.iter().map(|l| l.trim()).take(3).collect();
                    let target = format!("{role} {}", pageon_agent::quote::quote(name));
                    if near.is_empty() {
                        (false, format!("no {target} on the page"))
                    } else {
                        (
                            false,
                            format!("no {target}; the page has {}", near.join(", ")),
                        )
                    }
                }
            })
        }
    }
}

fn status_matches(want: &str, got: u16) -> bool {
    let got = got.to_string();
    want.len() == 3
        && want
            .bytes()
            .zip(got.bytes())
            .all(|(w, g)| w == b'x' || w == g)
}

fn request(o: &Outcome, method: &str, pattern: &str, status: Option<&str>) -> (bool, String) {
    let shown = |r: &pageon_net::RequestEntry| {
        let status = r
            .status
            .map_or_else(|| "no answer".to_string(), |s| s.to_string());
        format!("{} {} → {status}", r.method, redact(&r.url))
    };
    let sent: Vec<&pageon_net::RequestEntry> = o
        .requests
        .iter()
        .filter(|r| r.method == method && url_matches(pattern, &r.url))
        .collect();
    if let Some(r) = sent
        .iter()
        .find(|r| status.is_none_or(|s| r.status.is_some_and(|got| status_matches(s, got))))
    {
        return (true, shown(r));
    }
    if let Some(r) = sent.first() {
        return (false, shown(r));
    }
    let others: Vec<String> = o
        .requests
        .iter()
        .filter(|r| r.method != "GET" || matches!(r.dest, pageon_net::Dest::Document))
        .take(3)
        .map(shown)
        .collect();
    if others.is_empty() {
        (false, format!("no {method} request was sent"))
    } else {
        (
            false,
            format!("not sent; the action sent {}", others.join(", ")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grammar() {
        assert_eq!("url /orders".parse(), Ok(Expect::Url("/orders".into())));
        assert_eq!(
            r#"see "Order confirmed""#.parse(),
            Ok(Expect::See(Text::Words("Order confirmed".into())))
        );
        assert_eq!(
            r#"no button "Pay""#.parse(),
            Ok(Expect::No(Text::Element {
                role: "button".into(),
                name: "Pay".into()
            }))
        );
        assert_eq!(
            "request post /orders 2xx".parse(),
            Ok(Expect::Request {
                method: "POST".into(),
                pattern: "/orders".into(),
                status: Some("2xx".into())
            })
        );
        assert_eq!(
            "download *.pdf".parse(),
            Ok(Expect::Download("*.pdf".into()))
        );
        for bad in [
            "",
            "see",
            "see Order",
            "url",
            "request POST",
            "request POST /a 2x",
            "request POST /a 200 x",
            "click e5",
        ] {
            assert!(bad.parse::<Expect>().is_err(), "{bad:?}");
        }
        assert!(parse_all(&vec!["url /a"; 17]).is_err());
        assert!("url /".to_string().repeat(100).parse::<Expect>().is_err());
    }

    #[test]
    fn globs_and_urls() {
        assert!(glob("*.pdf", "report.pdf"));
        assert!(!glob("*.pdf", "report.pdf.exe"));
        assert!(glob("/orders/*", "/orders/42"));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b*c", "aXXbYY"));
        let stars = "*".repeat(100);
        assert!(glob(&stars, &"x".repeat(10_000)));
        assert!(url_matches("/orders", "https://shop.test/orders?x=1#y"));
        assert!(!url_matches("/orders", "https://shop.test/orders/42"));
        assert!(url_matches("https://shop.test/*", "https://shop.test/a/b"));
        assert!(
            status_matches("2xx", 201) && !status_matches("2xx", 500) && status_matches("404", 404)
        );
    }
}
