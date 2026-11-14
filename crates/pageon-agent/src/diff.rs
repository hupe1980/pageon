//! Structured diffs between observations. Applying a diff to the
//! previous observation reproduces the next one exactly.

use std::collections::{HashMap, HashSet};

use crate::ax::Line;

/// One diff operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Insert `line` after the line keyed `after` (`None` = at the start).
    Add {
        /// The new line.
        line: Line,
        /// Key of the preceding line.
        after: Option<String>,
    },
    /// Remove the line with this key.
    Remove {
        /// Key.
        key: String,
    },
    /// Replace the line with the same key.
    Change {
        /// The new line.
        line: Line,
    },
}

/// Computes the operations turning `old` into `new`. Keys must be unique within each list.
pub fn diff(old: &[Line], new: &[Line]) -> Vec<Op> {
    let new_keys: HashSet<&str> = new.iter().map(|l| l.key.as_str()).collect();
    let old_pos: HashMap<&str, usize> = old
        .iter()
        .enumerate()
        .map(|(i, l)| (l.key.as_str(), i))
        .collect();
    // Common lines in new order, and the longest run that keeps old order (kept in place).
    let common: Vec<(usize, usize)> = new
        .iter()
        .enumerate()
        .filter_map(|(j, l)| old_pos.get(l.key.as_str()).map(|&i| (j, i)))
        .collect();
    let stay: HashSet<usize> = lis(&common.iter().map(|&(_, i)| i).collect::<Vec<_>>())
        .into_iter()
        .map(|k| common[k].0)
        .collect();
    let mut ops = Vec::new();
    for l in old {
        let moved = old_pos.contains_key(l.key.as_str())
            && new_keys.contains(l.key.as_str())
            && !new
                .iter()
                .enumerate()
                .any(|(j, n)| n.key == l.key && stay.contains(&j));
        if !new_keys.contains(l.key.as_str()) || moved {
            ops.push(Op::Remove { key: l.key.clone() });
        }
    }
    for (j, l) in new.iter().enumerate() {
        let after = (j > 0).then(|| new[j - 1].key.clone());
        if stay.contains(&j) {
            let o = &old[old_pos[l.key.as_str()]];
            if o.text != l.text || o.depth != l.depth || o.kind != l.kind {
                ops.push(Op::Change { line: l.clone() });
            }
        } else {
            ops.push(Op::Add {
                line: l.clone(),
                after,
            });
        }
    }
    ops
}

/// Applies `ops` to `old`.
pub fn apply(old: &[Line], ops: &[Op]) -> Vec<Line> {
    let mut out: Vec<Line> = old.to_vec();
    for op in ops {
        match op {
            Op::Remove { key } => out.retain(|l| &l.key != key),
            Op::Change { line } => {
                if let Some(l) = out.iter_mut().find(|l| l.key == line.key) {
                    *l = line.clone();
                }
            }
            Op::Add { line, after } => {
                let at = match after {
                    None => 0,
                    Some(k) => out
                        .iter()
                        .position(|l| &l.key == k)
                        .map_or(out.len(), |p| p + 1),
                };
                out.insert(at, line.clone());
            }
        }
    }
    out
}

/// Renders a diff for the agent (`@diff v<a>→v<b>` with `+`, `-` and `~` lines).
pub fn render(from: u64, to: u64, ops: &[Op], old: &[Line]) -> String {
    let mut out = format!("@diff v{from}→v{to}");
    if ops.is_empty() {
        out.push_str(" no change");
        return out;
    }
    let anchor = |after: &Option<String>| match after {
        None => "^".to_string(),
        Some(k) if k.starts_with('e') => k.clone(),
        Some(_) => "·".to_string(),
    };
    for op in ops {
        out.push('\n');
        match op {
            Op::Add { line, after } => {
                out.push_str(&format!("+ {} after:{}", line.text, anchor(after)))
            }
            Op::Remove { key } => {
                let text = old
                    .iter()
                    .find(|l| &l.key == key)
                    .map(|l| l.text.as_str())
                    .unwrap_or(key);
                if key.starts_with('e') {
                    out.push_str(&format!("- {key}"));
                } else {
                    out.push_str(&format!("- {text}"));
                }
            }
            Op::Change { line } => out.push_str(&format!("~ {}", line.text)),
        }
    }
    out
}

/// Longest increasing subsequence; returns indexes into `xs`.
fn lis(xs: &[usize]) -> Vec<usize> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; xs.len()];
    for (i, &x) in xs.iter().enumerate() {
        let p = tails.partition_point(|&t| xs[t] < x);
        if p > 0 {
            prev[i] = tails[p - 1];
        }
        if p == tails.len() {
            tails.push(i);
        } else {
            tails[p] = i;
        }
    }
    let mut out = Vec::new();
    let mut k = tails.last().copied().unwrap_or(usize::MAX);
    while k != usize::MAX {
        out.push(k);
        k = prev[k];
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::LineKind;

    fn l(key: &str, text: &str) -> Line {
        let mut line = Line::test_default();
        line.key = key.into();
        line.kind = LineKind::Element;
        line.role = "button".into();
        line.priority = 2;
        line.text = text.into();
        line
    }

    #[test]
    fn apply_reproduces_new() {
        let old = vec![l("e1", "a"), l("e2", "b"), l("e3", "c"), l("t:x", "t")];
        let new = vec![l("e3", "c"), l("e1", "a!"), l("e4", "d"), l("t:x", "t")];
        let ops = diff(&old, &new);
        assert_eq!(apply(&old, &ops), new);
        assert!(!render(1, 2, &ops, &old).is_empty());
    }

    #[test]
    fn no_change_is_empty() {
        let old = vec![l("e1", "a")];
        assert!(diff(&old, &old).is_empty());
        assert_eq!(render(3, 3, &[], &old), "@diff v3→v3 no change");
    }
}
