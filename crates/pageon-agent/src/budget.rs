//! Token budgets. A budget always names its tokenizer, so a number of tokens means the same
//! thing to every model.

use std::fmt;
use std::sync::OnceLock;

/// A tokenizer a budget can be counted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Tokenizer {
    /// OpenAI's `o200k_base`.
    O200k,
}

impl Tokenizer {
    /// Parses a tokenizer name.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "o200k" | "o200k_base" => Some(Self::O200k),
            _ => None,
        }
    }

    /// Counts tokens of `text`.
    pub fn count(self, text: &str) -> usize {
        match self {
            Self::O200k => o200k().encode_ordinary(text).len(),
        }
    }
}

impl fmt::Display for Tokenizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::O200k => "o200k",
        })
    }
}

fn o200k() -> &'static tiktoken_rs::CoreBPE {
    static BPE: OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    BPE.get_or_init(|| {
        tiktoken_rs::o200k_base().unwrap_or_else(|e| panic!("o200k_base tables are bundled: {e}"))
    })
}

/// A token budget: a tokenizer plus a maximum count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Budget {
    /// The tokenizer the budget is counted in.
    pub tokenizer: Tokenizer,
    /// Maximum number of tokens.
    pub max_tokens: usize,
}

impl Budget {
    /// An o200k budget.
    pub fn o200k(max_tokens: usize) -> Self {
        Self {
            tokenizer: Tokenizer::O200k,
            max_tokens,
        }
    }

    /// Counts `text` in this budget's tokenizer.
    pub fn count(&self, text: &str) -> usize {
        self.tokenizer.count(text)
    }
}

impl fmt::Display for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.max_tokens, self.tokenizer)
    }
}

/// Counts o200k tokens (shared by every format so comparisons are fair).
pub fn count_tokens(text: &str) -> usize {
    Tokenizer::O200k.count(text)
}

/// Cuts a baseline's text to the budget by keeping head and tail lines and inserting one
/// omission line. Text that fits is returned unchanged.
pub fn cut_head_tail(text: &str, budget: Budget) -> String {
    if budget.count(text) <= budget.max_tokens {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    let render = |head: usize, tail: usize| {
        let omitted = lines.len() - head - tail;
        let mut out: Vec<String> = lines[..head].iter().map(|s| (*s).to_string()).collect();
        out.push(format!("… {omitted} lines omitted"));
        out.extend(lines[lines.len() - tail..].iter().map(|s| (*s).to_string()));
        out.join("\n")
    };
    // Largest k such that head = ceil(k/2), tail = floor(k/2) fits.
    let (mut lo, mut hi) = (0usize, lines.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let (h, t) = (mid.div_ceil(2), mid / 2);
        if budget.count(&render(h, t)) <= budget.max_tokens {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let rendered = render(lo.div_ceil(2), lo / 2);
    if budget.count(&rendered) <= budget.max_tokens {
        rendered
    } else {
        String::new()
    }
}

/// A line offered to [`fit`]: lower `priority` is kept first; ties prefer lines nearer the
/// start or end of their run (`edge_distance`), then earlier lines.
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    /// Priority class (0 = always first).
    pub priority: u8,
    /// Distance from the nearer edge of the line's priority run.
    pub edge_distance: usize,
    /// Token cost of the line including its newline.
    pub tokens: usize,
}

/// Chooses which lines to keep within `max_tokens` (already reduced by reserved overhead).
/// Returns a keep-flag per candidate.
pub fn fit(candidates: &[Candidate], max_tokens: usize) -> Vec<bool> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by_key(|&i| (candidates[i].priority, candidates[i].edge_distance, i));
    let mut keep = vec![false; candidates.len()];
    let mut used = 0usize;
    for i in order {
        let cost = candidates[i].tokens;
        // Priority 0 (header, outcome, omission) is always kept.
        if used + cost <= max_tokens || candidates[i].priority == 0 {
            keep[i] = true;
            used += cost;
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_o200k() {
        assert!(count_tokens("hello world") >= 2);
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn head_tail_fits_and_marks_omission() {
        let text: String = (0..500).map(|i| format!("- item number {i}\n")).collect();
        let b = Budget::o200k(200);
        let cut = cut_head_tail(&text, b);
        assert!(b.count(&cut) <= 200);
        assert!(cut.contains("lines omitted"));
        assert!(cut.starts_with("- item number 0"));
        assert!(cut.trim_end().ends_with("- item number 499"));
    }

    #[test]
    fn fit_prefers_priority_then_edges() {
        let c = |p, e| Candidate {
            priority: p,
            edge_distance: e,
            tokens: 10,
        };
        let keep = fit(&[c(0, 0), c(4, 0), c(4, 1), c(4, 0), c(2, 0)], 40);
        assert_eq!(keep, vec![true, true, false, true, true]);
    }
}
