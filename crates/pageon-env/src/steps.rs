//! Steps: what the agent did and what the page was after each action, kept in a
//! recording next to the streams they index. They make a replay provable, a reset exact, and a
//! divergence explainable.

use serde::{Deserialize, Serialize};

/// Where a step ended in each recorded stream.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Exchanges served so far.
    pub exchanges: usize,
    /// Clock readings so far.
    pub clock: usize,
    /// Random draws so far.
    pub random: usize,
    /// Realtime entries so far.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub realtime: usize,
}

/// One step of an episode: opening the page (step 0) or an action that ran.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// `open https://…`, or the action text (`click e5`, `type e3 "ada"`), for people.
    pub action: String,
    /// The same, for the engine: `{"open": {"url", "options"}}` or the serialized action.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub input: serde_json::Value,
    /// Where the step ended in the streams (filled by the recording).
    #[serde(default)]
    pub at: Cursor,
    /// Observations the agent made before this step (they assign refs; a re-run repeats
    /// them, so refs come out the same).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub observed: u32,
    /// The state hash after the step (16 hex digits).
    pub hash: String,
    /// The agent view after the step (stateless, unbudgeted, secrets redacted).
    pub view: String,
    /// The action's target as the agent saw it when it acted: what a workflow
    /// locates it by. Absent for actions without a target, and in older recordings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Target>,
    /// Expectations checked after the step (`see "Thanks"`): a workflow checks them on every
    /// run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
    /// What the call that ended with this step cost (absent in older recordings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
}

/// What one call cost: the tokens of the reply the model read, and the time it took.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// o200k tokens of the reply text.
    pub tokens: u32,
    /// Wall time of the call (ms).
    pub wall_ms: u64,
    /// Time on the page's clock while it settled (ms).
    pub virtual_ms: u64,
}

/// An element as the agent's view names it: role, accessible name, and its index among the
/// elements with the same role and name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// Role (`button`).
    pub role: String,
    /// Accessible name (`Sign in`; empty when it has none).
    pub name: String,
    /// Position among the elements with this role and name (0 for the first).
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub index: usize,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's signature
fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's signature
fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What kind of difference a replay found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The page made a request the recording does not have.
    Request,
    /// The action failed, or ran, unlike in the recording.
    Action,
    /// The page differs after the step.
    Page,
}

impl Kind {
    /// `request`, `action` or `page`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Action => "action",
            Self::Page => "page",
        }
    }
}

/// The first place a re-run differs from its recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Divergence {
    /// The step.
    pub step: usize,
    /// The recorded action of that step.
    pub action: String,
    /// What differs.
    pub kind: Kind,
    /// The request, the two outcomes, or "the page differs".
    pub detail: String,
    /// The recorded view (for `page`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// The view now (for `page`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "step {} {}: {}", self.step, self.action, self.detail)
    }
}

impl Divergence {
    /// Up to 20 changed lines between the recorded and the current view (`- ` / `+ `).
    pub fn diff(&self) -> Vec<String> {
        match (&self.expected, &self.actual) {
            (Some(a), Some(b)) => diff_lines(a, b, 20),
            _ => Vec::new(),
        }
    }
}

/// A line diff by longest common subsequence: removed lines as `- …`, added as `+ …`.
pub fn diff_lines(a: &str, b: &str, max: usize) -> Vec<String> {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let (n, m) = (a.len(), b.len());
    // Views are a few hundred lines; a quadratic table is fine. Larger inputs are cut.
    if n * m > 4_000_000 {
        return vec!["- (views too large to diff)".into()];
    }
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while (i < n || j < m) && out.len() < max {
        if i < n && j < m && a[i] == b[j] {
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            out.push(format!("- {}", a[i].trim_start()));
            i += 1;
        } else {
            out.push(format!("+ {}", b[j].trim_start()));
            j += 1;
        }
    }
    out
}

/// FNV-1a 64 of a text, as 16 hex digits: stable everywhere (no seeded hasher).
pub fn fnv64(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A recording that keeps steps (reached through [`crate::Env::recorder`]).
pub trait Recorder {
    /// Records the step that just ended, or, while resuming, checks it against the recording.
    fn step(&self, step: Step);
    /// The steps so far.
    fn steps(&self) -> Vec<Step>;
    /// The number of steps so far.
    fn len(&self) -> usize;
    /// Whether there are no steps.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Cuts the recording after step `k` and serves it again: the next `k + 1` steps are
    /// re-run from it (no egress) and checked, then recording goes on live.
    fn rewind(&self, k: usize) -> Result<(), String>;
    /// Ends a resume early (after a divergence): the steps checked so far stay, and it
    /// records on from there.
    fn cancel(&self);
    /// The first divergence found while resuming.
    fn divergence(&self) -> Option<Divergence>;
    /// Whether requests are answered from the recording (no egress) right now.
    fn replaying(&self) -> bool;
    /// Pauses recording: while paused, requests, clock and randomness pass through
    /// unrecorded and steps are ignored (a lookahead branch must leave no trace).
    fn pause(&self, paused: bool) {
        let _ = paused;
    }
    /// Adds expectations checked after the last step to it (ignored while resuming).
    fn checks(&self, checks: Vec<String>) {
        let _ = checks;
    }
    /// Sets what the call that ended with the last step cost (ignored while resuming).
    fn cost(&self, cost: Cost) {
        let _ = cost;
    }
    /// Whether the recording stopped at a size limit (later steps are not in it).
    fn truncated(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv64(""), "cbf29ce484222325");
        assert_eq!(fnv64("a"), "af63dc4c8601ec8c");
    }

    #[test]
    fn diffs_show_changes() {
        let d = diff_lines("a\nbutton \"Pay\"\nc", "a\nbutton \"Sold out\"\nc", 20);
        assert_eq!(d, ["- button \"Pay\"", "+ button \"Sold out\""]);
        assert!(diff_lines("x", "x", 20).is_empty());
    }
}
