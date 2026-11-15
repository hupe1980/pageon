//! The egress policy: origin sets and an approver, applied where requests leave (an
//! `Env` wrapper, so it runs in the broker when pages are sandboxed).

use std::fmt;

use pageon_env::{Env, NetError, Request, Response};
use url::Url;

/// What a request does, decided at egress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectClass {
    /// `GET`, `HEAD` or `OPTIONS` to a task origin.
    Read,
    /// Any other method to a task origin (it may change something: irreversible).
    Write,
    /// A request to an origin outside the task's origin sets (it may leak data).
    Outside,
}

/// A request as the guard and the approver see it: structured metadata, never page text
///.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Effect {
    /// Method.
    pub method: String,
    /// URL.
    pub url: String,
    /// Origin (`https://shop.test`).
    pub origin: String,
    /// Class (`read`, `write`, `outside`; `read`/`write` when there are no origin sets).
    pub class: EffectClass,
    /// Certain data labels: `secret:<name>` (it carries that secret, to an origin it is bound
    /// to), `first-contact` (no request went to this origin before in the session).
    #[serde(default)]
    pub labels: Vec<String>,
    /// The agent action that caused it, as the engine reports it (advisory); `None` for
    /// requests the page makes by itself.
    #[serde(default)]
    pub cause: Option<pageon_env::Cause>,
}

/// The longest URL a guard sees (longer ones are cut, marked `…`).
const MAX_URL: usize = 2048;
/// The longest page-authored target line a guard sees.
const MAX_TARGET: usize = 300;

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// A guard's answer about an effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Let it go.
    Allow,
    /// Refuse it, with a reason the agent sees.
    Deny(String),
    /// Let the approver decide (refused when there is none).
    Ask,
}

/// Decides about effects from their metadata (a critic, a guard model, rules).
type Guard = Box<dyn Fn(&Effect) -> Verdict>;

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.method, self.url)
    }
}

/// An origin pattern: `https://shop.test`, or `https://*.shop.test` for its subdomains too.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    scheme: String,
    host: String,
    subdomains: bool,
    port: Option<u16>,
}

impl Pattern {
    fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_end_matches('/');
        let (scheme, rest) = s.split_once("://")?;
        let (subdomains, rest) = match rest.strip_prefix("*.") {
            Some(r) => (true, r),
            None => (false, rest),
        };
        let u = Url::parse(&format!("{scheme}://{rest}/")).ok()?;
        Some(Self {
            scheme: u.scheme().to_string(),
            host: u.host_str()?.to_string(),
            subdomains,
            port: u.port_or_known_default(),
        })
    }

    fn matches(&self, u: &Url) -> bool {
        let Some(host) = u.host_str() else {
            return false;
        };
        u.scheme() == self.scheme
            && u.port_or_known_default() == self.port
            && (host == self.host
                || (self.subdomains && host.ends_with(&format!(".{}", self.host))))
    }
}

/// Whether `origin` is a valid origin pattern (`https://shop.test`, `https://*.shop.test`),
/// or what is wrong with it, with the corrected form when only the scheme is missing. Check
/// origins that come from users with it: [`Policy::read_write`] and [`Policy::read_only`]
/// ignore invalid patterns.
pub fn check_origin(origin: &str) -> Result<(), String> {
    if Pattern::parse(origin).is_some() {
        return Ok(());
    }
    let bare = origin.trim().trim_end_matches('/');
    if !bare.contains("://") && Pattern::parse(&format!("https://{bare}")).is_some() {
        return Err(format!(
            "{origin} needs a scheme: https://{bare} (or http://{bare})"
        ));
    }
    Err(format!(
        "{origin} is not an origin: write it as https://host, https://host:port or https://*.host"
    ))
}

/// Whether `url` is in the origins of `patterns` (`https://*.a.test` patterns allowed).
pub(crate) fn origin_allowed(patterns: &[String], url: &str) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    patterns
        .iter()
        .filter_map(|p| Pattern::parse(p))
        .any(|p| p.matches(&u))
}

/// Decides whether a write may leave.
type Approver = Box<dyn Fn(&Effect) -> bool>;

/// An `Env` that lets requests leave only within the task's origins, and asks before writes.
///
/// ```no_run
/// use pageon::{Live, Policy};
///
/// let env = Policy::new(Live::default())
///     .read_write(["https://shop.example"])
///     .read_only(["https://cdn.example"])
///     .approver(|effect| {
///         eprintln!("allow {effect}?");
///         false
///     });
/// ```
///
/// Without origin sets, it lets everything through. A refused request fails as
/// `NetError::Blocked("policy: …")`: the page sees a network error, the request log and the
/// outcome say why.
pub struct Policy<E: Env> {
    inner: E,
    read_write: Vec<Pattern>,
    read_only: Vec<Pattern>,
    approver: Option<Approver>,
    guard: Option<Guard>,
    /// Refuse every write unless the approver approves it, with or without origin sets.
    hold_writes: bool,
    /// Origins requests went to (and were allowed) in this session.
    contacted: std::cell::RefCell<std::collections::HashSet<String>>,
}

impl<E: Env> fmt::Debug for Policy<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Policy")
            .field("read_write", &self.read_write)
            .field("read_only", &self.read_only)
            .field("approver", &self.approver.is_some())
            .finish_non_exhaustive()
    }
}

impl<E: Env> Policy<E> {
    /// A policy with no origin sets (everything allowed).
    pub fn new(inner: E) -> Self {
        Self {
            inner,
            read_write: Vec::new(),
            read_only: Vec::new(),
            approver: None,
            hold_writes: false,
            guard: None,
            contacted: std::cell::RefCell::default(),
        }
    }

    /// Consults `guard` about every request except reads of origins already contacted: writes,
    /// requests outside the origin sets and first contacts. It sees structured
    /// metadata only. `Ask` defers to the approver (refused without one); a panic refuses.
    pub fn guard(mut self, guard: impl Fn(&Effect) -> Verdict + 'static) -> Self {
        self.guard = Some(Box::new(guard));
        self
    }

    /// Origins the task may read and write (`https://shop.test`, `https://*.shop.test`).
    /// Invalid patterns are ignored (validate them with [`check_origin`]).
    pub fn read_write<S: AsRef<str>>(mut self, origins: impl IntoIterator<Item = S>) -> Self {
        self.read_write.extend(
            origins
                .into_iter()
                .filter_map(|o| Pattern::parse(o.as_ref())),
        );
        self
    }

    /// Origins the task may only read.
    pub fn read_only<S: AsRef<str>>(mut self, origins: impl IntoIterator<Item = S>) -> Self {
        self.read_only.extend(
            origins
                .into_iter()
                .filter_map(|o| Pattern::parse(o.as_ref())),
        );
        self
    }

    /// Holds every write (any method but `GET`, `HEAD` and `OPTIONS`), with or without origin
    /// sets: it is refused unless the approver approves it. `pageon replay --live` uses this so
    /// recorded writes are not sent to the live site again by default.
    pub fn hold_writes(mut self) -> Self {
        self.hold_writes = true;
        self
    }

    /// Asks `approve` before every write to a read-write origin (`false`, or a panic, refuses).
    pub fn approver(mut self, approve: impl Fn(&Effect) -> bool + 'static) -> Self {
        self.approver = Some(Box::new(approve));
        self
    }

    /// The environment inside.
    pub fn inner(&self) -> &E {
        &self.inner
    }

    /// What the policy decides for a request (`method`, `url`, no labels): `None` to allow,
    /// or why it refuses.
    pub fn decide(&self, method: &str, url: &str) -> Option<String> {
        let mut req = Request::get(url);
        req.method = method.to_string();
        self.judge(&req)
    }

    /// What the policy decides for `req`: `None` to allow, or why it refuses. The order:
    /// origin sets, then the guard, then the approver.
    fn judge(&self, req: &Request) -> Option<String> {
        let sets = !(self.read_write.is_empty() && self.read_only.is_empty());
        if !sets && self.guard.is_none() && !self.hold_writes {
            return None;
        }
        let Ok(u) = Url::parse(&req.url) else {
            return sets.then(|| format!("policy: {} is not a URL", req.url));
        };
        if u.scheme() == "file" {
            return None;
        }
        let origin = u.origin().ascii_serialization();
        let write = !matches!(req.method.as_str(), "GET" | "HEAD" | "OPTIONS");
        let held = self.hold_writes && write;
        let rw = self.read_write.iter().any(|p| p.matches(&u));
        let ro = self.read_only.iter().any(|p| p.matches(&u));
        let class = match (sets && !(rw || ro), write) {
            (true, _) => EffectClass::Outside,
            (false, true) => EffectClass::Write,
            (false, false) => EffectClass::Read,
        };
        if sets {
            match class {
                EffectClass::Outside => {
                    return Some(format!("policy: {origin} is outside the task's origins"));
                }
                EffectClass::Write if !rw => {
                    return Some(format!("policy: {} to read-only {origin}", req.method));
                }
                _ => {}
            }
        }
        let first = !self.contacted.borrow().contains(&origin);
        let mut labels = req.labels.clone();
        if first {
            labels.push("first-contact".into());
        }
        // What the guard and the approver see: the URL with vault secrets as handles,
        // page-authored text clipped and marked.
        let effect = Effect {
            method: req.method.clone(),
            url: clip(req.shown_url.as_deref().unwrap_or(&req.url), MAX_URL),
            origin: origin.clone(),
            class,
            labels,
            cause: req.cause.clone().map(|mut c| {
                if let Some(t) = &c.target {
                    c.target = Some(clip(t, MAX_TARGET));
                    c.target_untrusted = true;
                }
                c
            }),
        };
        let mut ask = false;
        if let Some(g) = &self.guard
            && (write
                || first
                || class == EffectClass::Outside
                || effect.labels.iter().any(|l| l == "consequential"))
        {
            let verdict = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| g(&effect)))
                .unwrap_or_else(|_| Verdict::Deny("guard failed: it panicked".into()));
            match verdict {
                Verdict::Allow => {}
                Verdict::Deny(why) if why.starts_with("guard failed") => return Some(why),
                Verdict::Deny(why) => return Some(format!("guard: {why}")),
                Verdict::Ask => ask = true,
            }
        }
        if ask || held || (sets && class == EffectClass::Write) {
            match &self.approver {
                Some(approve) => {
                    let ok =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| approve(&effect)))
                            .unwrap_or(false);
                    if !ok {
                        return Some(format!(
                            "policy: {} {} was not approved",
                            req.method, req.url
                        ));
                    }
                }
                None if ask => {
                    return Some(format!(
                        "guard: {} {} needs approval, and no approver is configured",
                        req.method, req.url
                    ));
                }
                None if held => {
                    return Some(format!(
                        "held: {} {} is a write, and writes are held (pass --approve to be asked)",
                        req.method, req.url
                    ));
                }
                None => {}
            }
        }
        // Only an allowed request makes its origin known: a refused first contact stays one,
        // and a lookahead's reads leave no trace.
        if !req.labels.iter().any(|l| l == "lookahead") {
            self.contacted.borrow_mut().insert(origin);
        }
        None
    }
}

impl<E: Env> Env for Policy<E> {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        // Re-running recorded steps (a reset, a replay): answers come from the recording and
        // nothing leaves the machine, so there is nothing to decide or approve.
        if self.inner.recorder().is_some_and(|r| r.replaying()) {
            return self.inner.fetch(req);
        }
        match self.judge(req) {
            Some(why) => Err(NetError::Blocked(why)),
            None => self.inner.fetch(req),
        }
    }

    /// Opening a connection is judged as its `GET` (a read); every frame as its `SEND` (a
    /// write): origin sets, guard, approver and held writes apply to frames as to requests.
    fn stream(&self, call: &pageon_env::StreamCall) -> Result<pageon_env::StreamReply, NetError> {
        use pageon_env::StreamCall;
        if !self.inner.recorder().is_some_and(|r| r.replaying())
            && let StreamCall::Open { req, .. } | StreamCall::Send { req, .. } = call
            && let Some(why) = self.judge(req)
        {
            return Err(NetError::Blocked(why));
        }
        self.inner.stream(call)
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        self.inner.now_ms()
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        self.inner.random_u64()
    }

    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        self.inner.secret(name, origin)
    }

    fn file(&self, name: &str, origin: &str) -> Result<pageon_env::StagedFile, NetError> {
        self.inner.file(name, origin)
    }

    fn recorder(&self) -> Option<&dyn pageon_env::Recorder> {
        self.inner.recorder()
    }

    fn profile(&self) -> pageon_env::Profile {
        self.inner.profile()
    }
}
