//! pageon's agent observations.
//!
//! `pageon-agent` turns a captured page state into the `ax` view: a compact outline of what
//! an agent can read and act on, cut to a token budget that names its tokenizer, with short
//! element refs (`e12`) that stay stable across steps and are never written into the page.
//! All page-authored text is quoted, so page content can never forge engine markup.
//!
//! ```no_run
//! use pageon_agent::{Budget, Capture, Episode, Mode};
//!
//! let capture = Capture::load("page.capture.json.gz")?;
//! println!("{}", pageon_agent::ax(&capture, Budget::o200k(4000)));
//!
//! let mut episode = Episode::new(Budget::o200k(4000), Mode::Diff);
//! let first = episode.observe(&capture);
//! let target = episode.resolve("e3", &capture)?; // the element to act on
//! # let _ = (first, target);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! The text format (v1) is documented in the crate README.

pub mod ax;
pub mod browsergym;
pub mod budget;
pub mod capture;
pub mod diff;
pub mod page;
pub mod quote;
pub mod refs;
pub mod visibility;

pub use browsergym::browsergym;
pub use budget::{Budget, Tokenizer, count_tokens, cut_head_tail};
pub use capture::{Capture, CaptureError};
pub use refs::RefError;

use ax::Line;
use page::Page;
use refs::{RefTable, parse_ref, ref_name};

/// How follow-up observations are sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Every observation is complete.
    Full,
    /// After the first step, only changes are sent (falls back to full when large).
    Diff,
}

/// Share of the full observation above which a diff is replaced by the full observation.
pub const DIFF_FALLBACK: f64 = 0.4;

/// The stateless `ax` view of one capture (fresh refs `e1…` in document order).
pub fn ax(capture: &Capture, budget: Budget) -> String {
    let page = Page::build(capture);
    let mut table = RefTable::default();
    table.reconcile(&ax::collect(&page), page.nav, capture.meta.step);
    let lines = ax::build(&page, &mut table);
    let header = ax::header(capture, 1, budget, false);
    let fitted = ax::fit_lines(&lines, &header, budget);
    ax::render_lines(&header, &fitted)
}

/// The refs the stateless [`ax`] view assigns, by node: lets other views of the same page (such
/// as Markdown links) name elements with the same refs.
pub fn ref_map(capture: &Capture) -> Vec<(page::NodeKey, String)> {
    let page = Page::build(capture);
    let mut table = RefTable::default();
    let present = ax::collect(&page);
    table.reconcile(&present, page.nav, capture.meta.step);
    let _ = ax::build(&page, &mut table);
    present
        .into_iter()
        .filter_map(|(key, _)| table.get(&key).map(|r| (key, ref_name(r))))
        .collect()
}

/// Which elements [`Episode::read`] shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    /// What is in the viewport.
    #[default]
    Visible,
    /// The interactive elements in the viewport.
    Interactive,
    /// The whole page.
    All,
}

/// Words a search ignores.
const STOP_WORDS: &[&str] = &[
    "the", "an", "of", "to", "in", "on", "for", "and", "or", "with", "at", "by", "is", "it",
    "this", "that",
];

/// Each accessibility node's level below the start (the frames' roots, or one node), frames
/// counting from their owner `<iframe>`; nodes outside a start's subtree are absent.
fn ax_levels(
    page: &Page,
    start: Option<(usize, usize)>,
) -> std::collections::HashMap<(usize, usize), u32> {
    let mut levels = std::collections::HashMap::new();
    let mut queue = std::collections::VecDeque::new();
    match start {
        Some(s) => queue.push_back((s, 0)),
        None => {
            if let Some(r) = page.frames.first().and_then(|f| f.ax_root) {
                queue.push_back(((0, r), 0));
            }
        }
    }
    while let Some(((f, i), level)) = queue.pop_front() {
        if levels.contains_key(&(f, i)) || levels.len() > 200_000 {
            continue;
        }
        levels.insert((f, i), level);
        let frame = &page.frames[f];
        let node = &frame.ax[i];
        // Ignored nodes (wrappers a reader never sees) add no level.
        let below = level + u32::from(!node.ignored);
        for c in &node.child_ids {
            if let Some(&ci) = frame.ax_by_id.get(c) {
                queue.push_back(((f, ci), below));
            }
        }
        // An <iframe>'s document continues below it.
        if let Some(b) = node.backend_dom_node_id
            && let Some(cf) = page.child_frame(f, b)
            && let Some(r) = page.frames[cf].ax_root
        {
            queue.push_back(((cf, r), below));
        }
    }
    levels
}

/// The element an action targets.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// Backend DOM node id.
    pub backend_node_id: i64,
    /// CDP frame id.
    pub frame_id: String,
    /// BrowserGym's `bid` attribute on the node, if present.
    pub bid: Option<String>,
    /// Relocation confidence if the ref was moved to a replacement element at this step.
    pub relocated: Option<f64>,
}

/// An element with a ref, as [`Episode::elements`] lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    /// Its ref (`e14`).
    pub reference: String,
    /// Its `ax` line (`button e14 "Pay now"`).
    pub line: String,
    /// The node it names.
    pub target: Target,
}

/// One task played once: owns the ref table and the last observation the agent saw.
#[derive(Debug)]
pub struct Episode {
    budget: Budget,
    mode: Mode,
    table: RefTable,
    step: u32,
    version: u64,
    last: Option<Vec<Line>>,
    pending: Vec<String>,
}

impl Episode {
    /// A new episode.
    pub fn new(budget: Budget, mode: Mode) -> Self {
        Self {
            budget,
            mode,
            table: RefTable::default(),
            step: 0,
            version: 0,
            last: None,
            pending: Vec::new(),
        }
    }

    /// The current observation version.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Records an action just taken; its outcome line prefixes the next observation (several
    /// actions between two observations give several lines, in order).
    /// `error` is the failure text, if the action failed.
    pub fn note_action(&mut self, verb: &str, reference: &str, error: Option<&str>) {
        let line = match error {
            None => format!("ok {verb} {reference}"),
            Some(e) => format!("err {verb} {reference}: {e}"),
        };
        self.pending.push(line);
    }

    /// Makes the next observation complete again (refs stay as they are): for an agent that
    /// lost track of the page.
    pub fn reset_view(&mut self) {
        self.last = None;
    }

    /// Records a ready-made outcome line (for example a tab action's outcome); it prefixes the
    /// next observation.
    pub fn note(&mut self, line: impl Into<String>) {
        self.pending.push(line.into());
    }

    /// Observes a capture: a full observation on the first step, after navigation, or in
    /// [`Mode::Full`]; otherwise a diff against what the agent last saw.
    pub fn observe(&mut self, capture: &Capture) -> String {
        let page = Page::build(capture);
        self.table
            .reconcile(&ax::collect(&page), page.nav, self.step);
        let lines = ax::build(&page, &mut self.table);
        let prev_version = self.version;
        // Fit with a provisional header; versions have a fixed width impact negligible here.
        let probe = ax::header(capture, self.version + 1, self.budget, false);
        let fitted = ax::fit_lines(&lines, &probe, self.budget);
        let changed = self.last.as_ref().is_none_or(|l| *l != fitted);
        if changed {
            self.version += 1;
        }
        let pending = std::mem::take(&mut self.pending);
        let mut outcome = (!pending.is_empty()).then(|| {
            let mut lines = pending.join("\n");
            lines.push_str(&format!(" v{prev_version}→v{}", self.version));
            if self.table.refs_reset() {
                lines.push_str(" refs-reset");
            }
            lines
        });
        if outcome.is_none() && self.table.refs_reset() {
            outcome = Some("refs-reset".into());
        }
        let full = |fallback: bool| {
            let header = ax::header(capture, self.version, self.budget, fallback);
            ax::render_lines(&header, &fitted)
        };
        let body = match (&self.last, self.mode) {
            (Some(last), Mode::Diff) if !self.table.refs_reset() => {
                let ops = diff::diff(last, &fitted);
                let d = diff::render(prev_version, self.version, &ops, last);
                let full_text = full(false);
                if (self.budget.count(&d) as f64)
                    > DIFF_FALLBACK * self.budget.count(&full_text) as f64
                {
                    full(true)
                } else {
                    d
                }
            }
            _ => full(false),
        };
        self.last = Some(fitted);
        self.step += 1;
        match outcome {
            Some(o) => format!("{o}\n{body}"),
            None => body,
        }
    }

    /// The elements with refs on this capture, in document order, with their `ax` line: the
    /// same refs [`Episode::observe`] gives (refs are reconciled the same way, and the version
    /// does not change). For views of the same page, such as set-of-marks.
    pub fn elements(&mut self, capture: &Capture) -> Vec<Element> {
        let page = Page::build(capture);
        self.table
            .reconcile(&ax::collect(&page), page.nav, self.step);
        let lines = ax::build(&page, &mut self.table);
        lines
            .into_iter()
            .filter(|l| matches!(l.kind, ax::LineKind::Element | ax::LineKind::Container))
            .filter_map(|l| {
                let target = self.key(&l.key)?;
                Some(Element {
                    reference: l.key,
                    line: l.text,
                    target,
                })
            })
            .collect()
    }

    /// The complete `ax` view with this episode's refs (reconciled as [`Episode::observe`]
    /// does), without changing the version or the last view: the agent's next diff is
    /// unaffected. For other consumers of the same page, such as BrowserGym observations.
    pub fn full_view(&mut self, capture: &Capture) -> String {
        let page = Page::build(capture);
        self.table
            .reconcile(&ax::collect(&page), page.nav, self.step);
        let lines = ax::build(&page, &mut self.table);
        let header = ax::header(capture, self.version, self.budget, false);
        let fitted = ax::fit_lines(&lines, &header, self.budget);
        ax::render_lines(&header, &fitted)
    }

    /// A read of the page with this episode's refs (reconciled as [`Episode::observe`] does,
    /// without changing the version or the last view): the elements in the viewport
    /// ([`Filter::Visible`]), only its interactive ones, or the whole page; at most `depth`
    /// levels of the accessibility tree below the document or below `root` (a ref, whose
    /// subtree alone is read); fitted to `budget`. A `root` the episode does not know is an
    /// error.
    pub fn read(
        &mut self,
        capture: &Capture,
        filter: Filter,
        depth: u32,
        root: Option<&str>,
        budget: Budget,
    ) -> Result<String, RefError> {
        let page = Page::build(capture);
        self.table
            .reconcile(&ax::collect(&page), page.nav, self.step);
        let lines = ax::build(&page, &mut self.table);
        let start = match root {
            Some(r) => {
                let key = self
                    .table
                    .lookup(parse_ref(r).ok_or(RefError::Unknown)?)?
                    .clone();
                let f = page
                    .frames
                    .iter()
                    .position(|fr| fr.frame_id == key.frame_id)
                    .ok_or(RefError::Unknown)?;
                let ax = page.frames[f]
                    .ax
                    .iter()
                    .position(|n| n.backend_dom_node_id == Some(key.backend_id))
                    .ok_or(RefError::Stale {
                        since_step: self.step,
                    })?;
                Some((f, ax))
            }
            None => None,
        };
        let levels = ax_levels(&page, start);
        let kept: Vec<Line> = lines
            .into_iter()
            .filter(|l| {
                let Some(at) = l.at else { return false };
                let Some(&level) = levels.get(&at) else {
                    return false;
                };
                let shown = match filter {
                    Filter::Visible => l.in_view,
                    Filter::Interactive => {
                        l.in_view
                            && matches!(l.kind, ax::LineKind::Element | ax::LineKind::Container)
                    }
                    Filter::All => true,
                };
                shown && level < depth.max(1)
            })
            .collect();
        let header = ax::header(capture, self.version, budget, false);
        let fitted = ax::fit_lines(&kept, &header, budget);
        Ok(ax::render_lines(&header, &fitted))
    }

    /// The elements whose role, name, value or nearby text contain the words of `query`
    /// (case-insensitive), most words first, then those in the viewport, then document
    /// order; at most `limit`. A word match, not a model: the first line says so.
    pub fn find(&mut self, capture: &Capture, query: &str, limit: usize) -> String {
        let page = Page::build(capture);
        self.table
            .reconcile(&ax::collect(&page), page.nav, self.step);
        let lines = ax::build(&page, &mut self.table);
        let words: Vec<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 2 && !STOP_WORDS.contains(w))
            .map(str::to_string)
            .collect();
        let mut scored: Vec<(usize, bool, usize, &Line)> = Vec::new();
        let mut context = String::new();
        for (i, l) in lines.iter().enumerate() {
            match l.kind {
                ax::LineKind::Text | ax::LineKind::Heading => context = l.text.to_lowercase(),
                ax::LineKind::Element | ax::LineKind::Container => {
                    let own = l.text.to_lowercase();
                    // Own words count twice, words of the text just before once.
                    let score: usize = words
                        .iter()
                        .map(|w| {
                            if own.contains(w.as_str()) {
                                2
                            } else {
                                usize::from(context.contains(w.as_str()))
                            }
                        })
                        .sum();
                    if score > 0 {
                        scored.push((score, l.in_view, i, l));
                    }
                }
                _ => {}
            }
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
        let total = scored.len();
        let mut out = format!(
            "find {}: {} (by the words in each element's role, name, value and the text before it)",
            quote::quote(query),
            match total {
                0 => "no match".to_string(),
                1 => "1 match".to_string(),
                n if n > limit => format!("{n} matches, the best {limit}"),
                n => format!("{n} matches"),
            }
        );
        for (_, _, _, l) in scored.into_iter().take(limit) {
            out.push('\n');
            out.push_str(&l.text);
        }
        out
    }

    /// The lines the agent last saw (for tests of diff exactness).
    pub fn last_lines(&self) -> Option<&[Line]> {
        self.last.as_deref()
    }

    /// The ref the agent has for an element (frame id and backend node id), if any.
    pub fn ref_of(&self, frame_id: &str, backend_id: i64) -> Option<String> {
        self.table.find(frame_id, backend_id).map(refs::ref_name)
    }

    /// The element a ref names (frame and backend node), without checking that it is still
    /// there or uncovered.
    pub fn key(&self, reference: &str) -> Option<Target> {
        let r = parse_ref(reference)?;
        let key = self.table.lookup(r).ok()?;
        Some(Target {
            backend_node_id: key.backend_id,
            frame_id: key.frame_id.clone(),
            bid: None,
            relocated: self.table.relocation(r),
        })
    }

    /// Resolves a ref against the capture the agent just observed.
    pub fn resolve(&self, reference: &str, capture: &Capture) -> Result<Target, RefError> {
        let r = parse_ref(reference).ok_or(RefError::Unknown)?;
        let key = self.table.lookup(r)?.clone();
        let page = Page::build(capture);
        let f = page
            .frames
            .iter()
            .position(|fr| fr.frame_id == key.frame_id)
            .ok_or(RefError::Unknown)?;
        let frame = &page.frames[f];
        let d = *frame
            .by_backend
            .get(&key.backend_id)
            .ok_or(RefError::Stale {
                since_step: self.step,
            })?;
        let hits = visibility::HitIndex::build(frame);
        if let Some(top) = visibility::covered_by(&page, frame, &hits, d) {
            let mut i = Some(top);
            let mut by = None;
            while let Some(k) = i {
                if let Some(rf) = self.table.get(&page.key(f, frame.dom[k].backend_id)) {
                    by = Some(ref_name(rf));
                    break;
                }
                i = frame.dom[k].parent;
            }
            return Err(RefError::Covered { covered_by: by });
        }
        Ok(Target {
            backend_node_id: key.backend_id,
            frame_id: key.frame_id.clone(),
            bid: frame.dom[d]
                .attr("bid")
                .or_else(|| frame.dom[d].attr("browsergym_id"))
                .map(str::to_string),
            relocated: self.table.relocation(r),
        })
    }
}
