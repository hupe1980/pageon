//! Episode-scoped element refs `e<n>`: never recycled, relocated with a confidence when the
//! page replaces an element.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use crate::page::NodeKey;

/// What identifies an element for relocation: role, accessible name, accessibility path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Signature {
    /// AX role.
    pub role: String,
    /// Accessible name (normalised).
    pub name: String,
    /// `(role, name)` of non-ignored ancestors, outermost first.
    pub path: Vec<(String, String)>,
}

/// Confidence of a relocation whose role, name and path all match.
pub const CONF_EXACT: f64 = 1.0;
/// Confidence of a relocation whose role and name match but whose path differs (low, so it is
/// always flagged).
pub const CONF_MOVED: f64 = 0.4;

/// Why a ref no longer resolves.
#[derive(Debug, Clone, PartialEq)]
pub enum Gone {
    /// The element disappeared at this step with no equivalent.
    Stale {
        /// Step at which it was last missing.
        since_step: u32,
    },
    /// Several equally good replacements exist.
    Ambiguous {
        /// The candidate elements.
        candidates: Vec<NodeKey>,
    },
    /// The document was replaced (navigation).
    Reset,
}

/// Errors resolving a ref.
#[derive(Debug, Clone, PartialEq)]
pub enum RefError {
    /// Gone with no equivalent.
    Stale {
        /// Step at which it disappeared.
        since_step: u32,
    },
    /// Several equally good replacements.
    Ambiguous {
        /// Refs of the candidates.
        candidates: Vec<String>,
    },
    /// Never issued in this episode, or malformed.
    Unknown,
    /// Retired by a navigation.
    Reset,
    /// Present but covered by another element.
    Covered {
        /// Ref of the covering element, if it has one.
        covered_by: Option<String>,
    },
}

impl RefError {
    /// Stable kind name (`stale`, `ambiguous`, `unknown`, `reset`, `covered`).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Stale { .. } => "stale",
            Self::Ambiguous { .. } => "ambiguous",
            Self::Unknown => "unknown",
            Self::Reset => "reset",
            Self::Covered { .. } => "covered",
        }
    }
}

impl fmt::Display for RefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stale { since_step } => {
                write!(
                    f,
                    "stale (gone since step {since_step}) — use a ref from the current page"
                )
            }
            Self::Ambiguous { candidates } => write!(
                f,
                "ambiguous ({} candidates: {}) — pick one of them",
                candidates.len(),
                candidates.join(" ")
            ),
            Self::Unknown => f.write_str("unknown-ref — use a ref shown in the current page"),
            Self::Reset => {
                f.write_str("stale (page navigated, refs reset) — use a ref from the current page")
            }
            Self::Covered {
                covered_by: Some(r),
            } => write!(f, "covered-by:{r} — close or act on {r} first"),
            Self::Covered { covered_by: None } => {
                f.write_str("covered — scroll or close what covers it first")
            }
        }
    }
}

impl std::error::Error for RefError {}

/// The ref table of one episode.
#[derive(Debug, Default)]
pub struct RefTable {
    next: u32,
    by_key: HashMap<NodeKey, u32>,
    by_ref: HashMap<u32, NodeKey>,
    sig: HashMap<u32, Signature>,
    gone: HashMap<u32, Gone>,
    relocated: BTreeMap<u32, f64>,
    nav: Option<u32>,
    refs_reset: bool,
}

/// Formats a ref number.
pub fn ref_name(n: u32) -> String {
    format!("e{n}")
}

/// Parses `e<n>`, `[ref=e<n>]` or `@e<n>`.
pub fn parse_ref(s: &str) -> Option<u32> {
    let s = s
        .trim()
        .trim_start_matches("[ref=")
        .trim_end_matches(']')
        .trim_start_matches('@');
    s.strip_prefix('e')?.parse().ok()
}

impl RefTable {
    /// Reconciles the table with the elements present at `step` before refs are handed out:
    /// retires refs on navigation and relocates refs whose element was replaced.
    pub fn reconcile(&mut self, present: &[(NodeKey, Signature)], nav: u32, step: u32) {
        self.relocated.clear();
        self.refs_reset = false;
        if self.nav.is_some_and(|n| n != nav) {
            for &r in self.by_ref.keys() {
                self.gone.insert(r, Gone::Reset);
            }
            self.by_ref.clear();
            self.by_key.clear();
            self.refs_reset = true;
        }
        self.nav = Some(nav);
        let live: HashSet<&NodeKey> = present.iter().map(|(k, _)| k).collect();
        let mut missing: Vec<u32> = self
            .by_ref
            .iter()
            .filter(|(_, k)| !live.contains(k))
            .map(|(&r, _)| r)
            .collect();
        missing.sort_unstable();
        let mut taken: HashSet<NodeKey> = self
            .by_key
            .keys()
            .filter(|k| live.contains(k))
            .cloned()
            .collect();
        for r in missing {
            let key = self
                .by_ref
                .remove(&r)
                .unwrap_or_else(|| unreachable!("missing ref came from by_ref"));
            self.by_key.remove(&key);
            let Some(sig) = self.sig.get(&r).cloned() else {
                continue;
            };
            let score = |s: &Signature| {
                if s.role != sig.role || s.name != sig.name || sig.name.is_empty() {
                    0.0
                } else if s.path == sig.path {
                    CONF_EXACT
                } else {
                    CONF_MOVED
                }
            };
            let free: Vec<&(NodeKey, Signature)> = present
                .iter()
                .filter(|(k, _)| !taken.contains(k) && !self.by_key.contains_key(k))
                .collect();
            let best = free.iter().map(|(_, s)| score(s)).fold(0.0_f64, f64::max);
            let winners: Vec<&NodeKey> = free
                .iter()
                .filter(|(_, s)| best > 0.0 && score(s) == best)
                .map(|(k, _)| k)
                .collect();
            match winners.as_slice() {
                [] => {
                    self.gone.insert(r, Gone::Stale { since_step: step });
                }
                [one] => {
                    let one = (*one).clone();
                    taken.insert(one.clone());
                    self.by_key.insert(one.clone(), r);
                    self.by_ref.insert(r, one);
                    self.relocated.insert(r, best);
                    self.gone.remove(&r);
                }
                many => {
                    self.gone.insert(
                        r,
                        Gone::Ambiguous {
                            candidates: many.iter().map(|k| (*k).clone()).collect(),
                        },
                    );
                }
            }
        }
    }

    /// The ref of an element, assigning a new one if unseen. Records its signature.
    pub fn ref_for(&mut self, key: &NodeKey, sig: Option<&Signature>) -> u32 {
        let r = match self.by_key.get(key) {
            Some(&r) => r,
            None => {
                self.next += 1;
                let r = self.next;
                self.by_key.insert(key.clone(), r);
                self.by_ref.insert(r, key.clone());
                r
            }
        };
        if let Some(sig) = sig {
            self.sig.insert(r, sig.clone());
        }
        r
    }

    /// The ref of an element in a frame if it already has one (any navigation).
    pub fn find(&self, frame_id: &str, backend_id: i64) -> Option<u32> {
        self.by_key
            .iter()
            .find(|(k, _)| k.backend_id == backend_id && k.frame_id == frame_id)
            .map(|(_, &r)| r)
    }

    /// The ref of an element if it already has one.
    pub fn get(&self, key: &NodeKey) -> Option<u32> {
        self.by_key.get(key).copied()
    }

    /// The element a live ref points to, or why it does not resolve.
    pub fn lookup(&self, r: u32) -> Result<&NodeKey, RefError> {
        if let Some(k) = self.by_ref.get(&r) {
            return Ok(k);
        }
        match self.gone.get(&r) {
            Some(Gone::Stale { since_step }) => Err(RefError::Stale {
                since_step: *since_step,
            }),
            Some(Gone::Reset) => Err(RefError::Reset),
            Some(Gone::Ambiguous { candidates }) => Err(RefError::Ambiguous {
                candidates: candidates
                    .iter()
                    .filter_map(|k| self.get(k))
                    .map(ref_name)
                    .collect(),
            }),
            None => Err(RefError::Unknown),
        }
    }

    /// Confidence of the relocation of `r` at the last reconcile, if it was relocated.
    pub fn relocation(&self, r: u32) -> Option<f64> {
        self.relocated.get(&r).copied()
    }

    /// Whether the last reconcile retired refs because of a navigation.
    pub fn refs_reset(&self) -> bool {
        self.refs_reset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: i64) -> NodeKey {
        NodeKey {
            frame_id: "F".into(),
            nav: 0,
            backend_id: b,
        }
    }
    fn sig(role: &str, name: &str, path: &[&str]) -> Signature {
        Signature {
            role: role.into(),
            name: name.into(),
            path: path
                .iter()
                .map(|p| ((*p).to_string(), String::new()))
                .collect(),
        }
    }

    #[test]
    fn refs_are_stable_and_never_recycled() {
        let mut t = RefTable::default();
        let s = sig("button", "Pay", &["main"]);
        t.reconcile(&[(key(1), s.clone())], 0, 0);
        assert_eq!(t.ref_for(&key(1), Some(&s)), 1);
        t.reconcile(
            &[(key(1), s.clone()), (key(2), sig("link", "x", &[]))],
            0,
            1,
        );
        assert_eq!(t.ref_for(&key(1), Some(&s)), 1);
        assert_eq!(t.ref_for(&key(2), None), 2);
    }

    #[test]
    fn relocates_replaced_element_exact_and_moved() {
        let mut t = RefTable::default();
        let s = sig("button", "Pay", &["main"]);
        t.reconcile(&[(key(1), s.clone())], 0, 0);
        t.ref_for(&key(1), Some(&s));
        t.reconcile(&[(key(9), s.clone())], 0, 1);
        assert_eq!(t.lookup(1), Ok(&key(9)));
        assert_eq!(t.relocation(1), Some(CONF_EXACT));
        let moved = sig("button", "Pay", &["dialog"]);
        t.reconcile(&[(key(10), moved)], 0, 2);
        assert_eq!(t.relocation(1), Some(CONF_MOVED));
    }

    #[test]
    fn stale_ambiguous_and_reset() {
        let mut t = RefTable::default();
        let s = sig("button", "Pay", &[]);
        t.reconcile(&[(key(1), s.clone())], 0, 0);
        t.ref_for(&key(1), Some(&s));
        t.reconcile(&[], 0, 3);
        assert_eq!(t.lookup(1), Err(RefError::Stale { since_step: 3 }));

        let mut t = RefTable::default();
        t.reconcile(&[(key(1), s.clone())], 0, 0);
        t.ref_for(&key(1), Some(&s));
        t.reconcile(&[(key(2), s.clone()), (key(3), s.clone())], 0, 1);
        t.ref_for(&key(2), Some(&s));
        t.ref_for(&key(3), Some(&s));
        assert_eq!(
            t.lookup(1),
            Err(RefError::Ambiguous {
                candidates: vec!["e2".into(), "e3".into()]
            })
        );

        let mut t = RefTable::default();
        t.reconcile(&[(key(1), s.clone())], 0, 0);
        t.ref_for(&key(1), Some(&s));
        t.reconcile(&[(NodeKey { nav: 1, ..key(1) }, s.clone())], 1, 1);
        assert!(t.refs_reset());
        assert_eq!(t.lookup(1), Err(RefError::Reset));
        assert_eq!(t.ref_for(&NodeKey { nav: 1, ..key(1) }, Some(&s)), 2);
    }

    #[test]
    fn parses_ref_styles() {
        assert_eq!(parse_ref("e12"), Some(12));
        assert_eq!(parse_ref("[ref=e12]"), Some(12));
        assert_eq!(parse_ref("@e3"), Some(3));
        assert_eq!(parse_ref("12"), None);
    }
}
