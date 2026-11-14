//! The session's HTTP cache, freshness only: `GET` scripts and stylesheets with explicit
//! freshness (`Cache-Control: max-age`, or `Expires`), never `no-store`/`no-cache`, no `Vary`
//! beyond `Accept-Encoding`. No revalidation, no heuristic freshness. Time is the load clock.

use std::collections::HashMap;

use crate::Fetched;

/// Cached responses by URL.
#[derive(Debug, Clone, Default)]
pub struct Cache {
    entries: HashMap<String, (Fetched, u64)>,
}

/// How long a response stays fresh (ms), if it may be stored at all.
pub fn freshness(f: &Fetched, now: u64) -> Option<u64> {
    if f.status != 200 {
        return None;
    }
    let cc = f
        .headers
        .iter()
        .filter(|(k, _)| k == "cache-control")
        .flat_map(|(_, v)| v.split(','))
        .map(|d| d.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    if cc.iter().any(|d| d == "no-store" || d == "no-cache") {
        return None;
    }
    if f.headers
        .iter()
        .filter(|(k, _)| k == "vary")
        .flat_map(|(_, v)| v.split(','))
        .any(|v| !v.trim().eq_ignore_ascii_case("accept-encoding"))
    {
        return None;
    }
    if let Some(age) = cc
        .iter()
        .find_map(|d| d.strip_prefix("max-age="))
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return (age > 0).then(|| age.saturating_mul(1000));
    }
    let date = |name: &str| f.header(name).and_then(crate::cookie::parse_date);
    let expires = date("expires")?;
    let base = date("date").unwrap_or(now);
    expires.checked_sub(base).filter(|&d| d > 0)
}

impl Cache {
    /// A fresh response for `url` at `now`.
    pub fn get(&self, url: &str, now: u64) -> Option<&Fetched> {
        self.entries
            .get(url)
            .filter(|(_, until)| now < *until)
            .map(|(f, _)| f)
    }

    /// Stores a response if it has explicit freshness.
    pub fn put(&mut self, url: &str, f: &Fetched, now: u64) {
        if let Some(ms) = freshness(f, now) {
            self.entries
                .insert(url.to_string(), (f.clone(), now.saturating_add(ms)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(headers: &[(&str, &str)]) -> Fetched {
        Fetched {
            kind: crate::Kind::Basic,
            status: 200,
            status_text: "OK".into(),
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            body: b"x".to_vec(),
            url: url::Url::parse("https://a.test/app.js").unwrap_or_else(|e| panic!("{e}")),
            redirected: false,
            chain: Vec::new(),
        }
    }

    #[test]
    fn freshness_rules() {
        assert_eq!(
            freshness(&res(&[("cache-control", "public, max-age=600")]), 0),
            Some(600_000)
        );
        assert_eq!(
            freshness(&res(&[("cache-control", "max-age=600, no-store")]), 0),
            None
        );
        assert_eq!(freshness(&res(&[("cache-control", "no-cache")]), 0), None);
        assert_eq!(
            freshness(
                &res(&[("cache-control", "max-age=60"), ("vary", "Cookie")]),
                0
            ),
            None
        );
        assert_eq!(
            freshness(
                &res(&[("cache-control", "max-age=60"), ("vary", "Accept-Encoding")]),
                0
            ),
            Some(60_000)
        );
        assert_eq!(
            freshness(
                &res(&[
                    ("date", "Wed, 21 Oct 2015 07:28:00 GMT"),
                    ("expires", "Wed, 21 Oct 2015 07:38:00 GMT")
                ]),
                0
            ),
            Some(600_000)
        );
        assert_eq!(freshness(&res(&[]), 0), None);
        let mut c = Cache::default();
        c.put("u", &res(&[("cache-control", "max-age=1")]), 1000);
        assert!(c.get("u", 1999).is_some());
        assert!(c.get("u", 2000).is_none());
    }
}
