//! Sign-ins that renew: a saved sign-in workflow, and how a session's sign-out is recognised
//! from engine facts (the document requests of an action's outcome), never from page text.

use serde_json::Value;
use url::Url;

use crate::workflow::Workflow;

/// Renewals of one origin in a row (with no action in between that stayed signed in) before
/// pageon stops renewing it.
pub const MAX_IN_A_ROW: u32 = 3;

/// A sign-in workflow: its start page is the site's sign-in page.
#[derive(Debug, Clone)]
pub struct SignIn {
    /// The file as given (shown in replies).
    pub name: String,
    /// The workflow.
    pub workflow: Workflow,
    /// The origin it signs in to (`https://portal.example`).
    pub origin: String,
    /// The sign-in page's path (`/login`).
    pub path: String,
}

impl SignIn {
    /// A sign-in from a workflow; `name` is how replies name it.
    pub fn new(name: &str, workflow: Workflow) -> Result<Self, String> {
        let start = Url::parse(&workflow.start)
            .map_err(|e| format!("{name}: its start {:?} is not a URL: {e}", workflow.start))?;
        if !matches!(start.scheme(), "http" | "https") {
            return Err(format!(
                "{name}: a sign-in starts on an http(s) sign-in page, not {}",
                workflow.start
            ));
        }
        Ok(Self {
            name: name.to_string(),
            origin: start.origin().ascii_serialization(),
            path: start.path().to_string(),
            workflow,
        })
    }

    /// A sign-in from a workflow file.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let workflow: Workflow = serde_json::from_str(&text).map_err(|e| {
            format!(
                "{}: not a workflow ({e}); compile one with pageon workflow compile",
                path.display()
            )
        })?;
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        Self::new(&name, workflow)
    }

    /// Whether `url` is this sign-in's page.
    pub fn is_sign_in_page(&self, url: &Url) -> bool {
        url.origin().ascii_serialization() == self.origin && url.path() == self.path
    }
}

/// Checks that no two sign-ins share an origin.
pub fn check(sign_ins: &[SignIn]) -> Result<(), String> {
    for (i, a) in sign_ins.iter().enumerate() {
        if let Some(b) = sign_ins[i + 1..].iter().find(|b| b.origin == a.origin) {
            return Err(format!(
                "{} and {} both sign in to {}; keep one",
                a.name, b.name, a.origin
            ));
        }
    }
    Ok(())
}

/// A sign-out an action ran into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedOut {
    /// Which sign-in (index).
    pub sign_in: usize,
    /// `redirected to the sign-in page` or `401`.
    pub why: &'static str,
    /// The method of the request that met it.
    pub method: String,
    /// The URL it asked for.
    pub url: String,
}

/// The sign-out in an outcome (its `requests`, as MCP replies carry them), if any: a document
/// request to a sign-in's origin that was redirected from another path to the sign-in page,
/// or answered 401.
pub fn signed_out(outcome: &Value, sign_ins: &[SignIn]) -> Option<SignedOut> {
    for r in outcome["requests"].as_array()? {
        if r["dest"] != "Document" || r["outcome"] != "Ok" {
            continue;
        }
        let Some(asked) = r["url"].as_str().and_then(|u| Url::parse(u).ok()) else {
            continue;
        };
        let Some(i) = sign_ins
            .iter()
            .position(|s| asked.origin().ascii_serialization() == s.origin)
        else {
            continue;
        };
        let s = &sign_ins[i];
        let landed = r["final_url"].as_str().and_then(|u| Url::parse(u).ok());
        let why = if r["status"] == 401 {
            "401"
        } else if !s.is_sign_in_page(&asked) && landed.is_some_and(|l| s.is_sign_in_page(&l)) {
            "redirected to the sign-in page"
        } else {
            continue;
        };
        return Some(SignedOut {
            sign_in: i,
            why,
            method: r["method"].as_str().unwrap_or("GET").to_string(),
            url: asked.to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sign_in() -> SignIn {
        let wf: Workflow = serde_json::from_value(json!({
            "workflow": 1, "start": "https://portal.test/login", "steps": []
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        SignIn::new("login.flow.json", wf).unwrap_or_else(|e| panic!("{e}"))
    }

    fn req(method: &str, url: &str, status: u16, final_url: Option<&str>) -> Value {
        json!({"method": method, "url": url, "dest": "Document", "status": status,
               "final_url": final_url, "outcome": "Ok"})
    }

    #[test]
    fn redirects_to_the_sign_in_page_and_401s_are_sign_outs() {
        let s = [sign_in()];
        let redirected = json!({"requests": [req("GET", "https://portal.test/orders", 200, Some("https://portal.test/login?next=/orders"))]});
        let out = signed_out(&redirected, &s).unwrap_or_else(|| panic!("not seen"));
        assert_eq!(
            (out.why, out.method.as_str()),
            ("redirected to the sign-in page", "GET")
        );
        assert_eq!(out.url, "https://portal.test/orders");
        let refused = json!({"requests": [req("POST", "https://portal.test/orders", 401, None)]});
        assert_eq!(signed_out(&refused, &s).map(|o| o.why), Some("401"));
    }

    #[test]
    fn asking_for_the_sign_in_page_other_origins_and_subresources_are_not() {
        let s = [sign_in()];
        for outcome in [
            json!({"requests": [req("GET", "https://portal.test/login", 200, None)]}),
            json!({"requests": [req("GET", "https://other.test/orders", 401, None)]}),
            json!({"requests": [req("GET", "https://other.test/x", 200, Some("https://portal.test/login"))]}),
            json!({"requests": [{"method": "GET", "url": "https://portal.test/api", "dest": "Fetch", "status": 401, "outcome": "Ok"}]}),
            json!({"requests": [req("GET", "https://portal.test/orders", 200, None)]}),
        ] {
            assert_eq!(signed_out(&outcome, &s), None, "{outcome}");
        }
    }

    #[test]
    fn sign_ins_are_checked() {
        let wf = |start: &str| -> Workflow {
            serde_json::from_value(json!({"workflow": 1, "start": start, "steps": []}))
                .unwrap_or_else(|e| panic!("{e}"))
        };
        assert!(SignIn::new("a", wf("file:///login.html")).is_err());
        let two = [sign_in(), sign_in()];
        assert!(check(&two).is_err());
    }

    #[test]
    #[ignore = "timing: run with --release -- --ignored"]
    #[allow(clippy::disallowed_methods)] // a timing harness measures wall time on purpose
    fn detection_is_cheap() {
        let s = [sign_in()];
        let requests: Vec<Value> = (0..50)
            .map(|i| req("GET", &format!("https://cdn.test/{i}.js"), 200, None))
            .collect();
        let outcome = json!({"requests": requests});
        let t = std::time::Instant::now();
        for _ in 0..10_000 {
            assert!(signed_out(&outcome, &s).is_none());
        }
        let us = t.elapsed().as_secs_f64() * 1e6 / 10_000.0;
        println!("sign-out check over 50 requests: {us:.2} µs");
        assert!(us < 50.0, "{us:.2} µs");
    }
}
