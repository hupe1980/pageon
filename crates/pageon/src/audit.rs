//! The agent-readiness audit: what an agent meets on a page (names, actions, page tools, what
//! the view costs, access and health), scored per area with the fixes that gain the most first.
//!
//! [`PageFacts`] are read from a loaded page (in the content process when caged),
//! [`SiteFacts`] from the site's files, and [`Report::new`] scores them; it is pure.

use std::collections::HashMap;
use std::fmt::Write as _;

use pageon_agent::budget::count_tokens;
use pageon_agent::quote::{clip, quote};
use pageon_dom::NodeId;
use pageon_env::Env;
use serde::{Deserialize, Serialize};

use crate::{Budget, Dump, Tab, Viewport};

/// The view budget an agent gets by default (`pageon mcp`, `--budget`).
pub const DEFAULT_BUDGET: usize = 4000;

/// WCAG 2.5.8 (Target Size, Minimum): 24 × 24 CSS px, or that much room around the target.
const TARGET: f32 = 24.0;

/// An element of the agent view and its box.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElementFact {
    /// Its view line (`button e2 "Pay"`).
    pub line: String,
    /// Its box in CSS px (`x, y, width, height`), for elements of the top-level document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<[f32; 4]>,
}

/// A page tool (WebMCP).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolFact {
    /// Name (page text).
    pub name: String,
    /// Description (page text).
    pub description: String,
    /// Parameters in its input schema.
    pub params: u32,
    /// Parameters with a description.
    pub described: u32,
}

/// What one load of a page shows an agent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PageFacts {
    /// The final URL.
    pub url: String,
    /// The HTTP status.
    pub status: u16,
    /// What a non-HTML document was read from (`text`, `markdown`, `server-markdown`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The site's refusal of the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<pageon_net::Refusal>,
    /// Elements with refs, in view order.
    pub elements: Vec<ElementFact>,
    /// For each form that is not a tool and has a field to type in: its first field's line.
    pub forms: Vec<String>,
    /// Page tools.
    pub tools: Vec<ToolFact>,
    /// Tokens (o200k) of the whole agent view, its Markdown and its HTML.
    pub view_tokens: usize,
    /// Tokens of the Markdown view.
    pub markdown_tokens: usize,
    /// Tokens of the HTML.
    pub html_tokens: usize,
    /// The view's landmarks (`## navigation`) and their tokens, largest first.
    pub sections: Vec<(String, usize)>,
    /// Script errors (quoted, clipped).
    pub errors: Vec<String>,
    /// The text of a non-HTML document (robots.txt, llms.txt), clipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Reads the facts of a tab's page, with secrets redacted.
pub(crate) fn facts<E: Env>(tab: &mut Tab<'_, E>) -> PageFacts {
    let viewport = Viewport::default();
    let all = Budget::o200k(1 << 24);
    let capture = tab.doc.capture(viewport);
    let found = tab.episode.elements(&capture);
    let layout = tab.doc.layout(viewport);
    let nodes: Vec<Option<(usize, NodeId)>> =
        found.iter().map(|e| tab.doc.node_for(&e.target)).collect();
    let elements: Vec<ElementFact> = found
        .iter()
        .zip(&nodes)
        .map(|(e, n)| ElementFact {
            line: tab.redact(e.line.clone()),
            rect: n
                .filter(|(f, _)| *f == 0)
                .and_then(|(_, id)| layout.box_of(id))
                .map(|b| [b.x, b.y, b.width, b.height]),
        })
        .collect();
    let forms = forms(&tab.doc, &elements, &nodes);
    let tools = tab
        .tools()
        .into_iter()
        .map(|(_, t)| {
            let (params, described) = params(t.input_schema.as_deref());
            ToolFact {
                name: tab.redact(t.name),
                description: tab.redact(t.description),
                params,
                described,
            }
        })
        .collect();
    let view = tab.view(Dump::Ax, viewport, all);
    let source = tab.doc.source().map(str::to_string);
    let text = source
        .is_some()
        .then(|| clip_bytes(tab.view(Dump::Text, viewport, all), 512 * 1024));
    let errors = tab
        .doc
        .errors()
        .iter()
        // The site's errors: pageon's own time and memory limits are not the site's.
        .filter(|e| {
            matches!(
                e.kind,
                pageon_js::PageErrorKind::Exception | pageon_js::PageErrorKind::Load
            )
        })
        .take(20)
        .map(|e| quote(&clip(&tab.redact(e.message.clone()), 120)))
        .collect();
    PageFacts {
        url: tab.url().to_string(),
        status: tab.status(),
        source,
        refusal: tab.doc.refusal().cloned(),
        elements,
        forms,
        tools,
        view_tokens: count_tokens(&view),
        markdown_tokens: count_tokens(&tab.view(Dump::Markdown, viewport, all)),
        html_tokens: count_tokens(&tab.view(Dump::Html, viewport, all)),
        sections: sections(&view),
        errors,
        text,
    }
}

fn clip_bytes(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// The forms of the top-level document that are not tools but take typed input: the line of
/// each one's first text field.
fn forms(
    doc: &crate::Document,
    elements: &[ElementFact],
    nodes: &[Option<(usize, NodeId)>],
) -> Vec<String> {
    let dom = doc.dom();
    let mut out = Vec::new();
    for edge in dom.traverse(dom.root()) {
        let pageon_dom::Edge::Enter(form) = edge else {
            continue;
        };
        let Some(n) = dom.get(form) else { continue };
        if n.local_name() != Some("form") || n.attr("toolname").is_some() {
            continue;
        }
        let first = elements.iter().zip(nodes).find(|(e, at)| {
            let typed = matches!(
                Line::parse(&e.line).role,
                "textbox" | "searchbox" | "combobox"
            );
            typed
                && at.is_some_and(|(f, id)| {
                    let mut cur = dom.parent(id);
                    f == 0
                        && std::iter::from_fn(|| {
                            let c = cur?;
                            cur = dom.parent(c);
                            Some(c)
                        })
                        .any(|a| a == form)
                })
        });
        if let Some((e, _)) = first {
            out.push(e.line.clone());
        }
    }
    out
}

/// Parameters of an input schema and how many have a description.
fn params(schema: Option<&str>) -> (u32, u32) {
    let Some(v) = schema.and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()) else {
        return (0, 0);
    };
    let Some(props) = v.get("properties").and_then(serde_json::Value::as_object) else {
        return (0, 0);
    };
    let described = props
        .values()
        .filter(|p| {
            p.get("description")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|d| !d.trim().is_empty())
        })
        .count();
    (
        u32::try_from(props.len()).unwrap_or(u32::MAX),
        u32::try_from(described).unwrap_or(u32::MAX),
    )
}

/// The tokens of each landmark section of a view, largest first; empty when the view has
/// fewer than two.
fn sections(view: &str) -> Vec<(String, usize)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for line in view.lines().skip(1) {
        if line.starts_with("## ") || out.is_empty() {
            let name = if line.starts_with("## ") {
                line.to_string()
            } else {
                "(outside landmarks)".to_string()
            };
            out.push((name, String::new()));
        }
        if let Some((_, text)) = out.last_mut() {
            text.push_str(line);
            text.push('\n');
        }
    }
    if out.len() < 2 {
        return Vec::new();
    }
    let mut merged: Vec<(String, usize)> = Vec::new();
    for (name, text) in out {
        let n = count_tokens(&text);
        match merged.iter_mut().find(|(m, _)| *m == name) {
            Some((_, t)) => *t += n,
            None => merged.push((name, n)),
        }
    }
    merged.sort_by_key(|m| std::cmp::Reverse(m.1));
    merged
}

/// What the site publishes for agents.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SiteFacts {
    /// Whether the site was checked (http and https pages only).
    pub checked: bool,
    /// `/robots.txt` answered with rules.
    pub robots: bool,
    /// Why robots.txt keeps pageon from the page, if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub robots_disallows: Option<String>,
    /// `/llms.txt` answered with text.
    pub llms_txt: bool,
    /// The page answered `Accept: text/markdown` with Markdown.
    pub markdown: bool,
    /// Tokens of the agent view of the page loaded without scripts, as agents that do not
    /// run scripts read it (`None`: not loaded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub without_scripts: Option<usize>,
}

impl SiteFacts {
    /// The facts from loads of `/robots.txt`, `/llms.txt` and the page with
    /// `Accept: text/markdown` (`None`: the load failed); `path` is the page's path and query.
    pub fn from_loads(
        path: &str,
        robots: Option<&PageFacts>,
        llms: Option<&PageFacts>,
        markdown: Option<&PageFacts>,
    ) -> Self {
        let text = |f: Option<&PageFacts>| {
            f.filter(|f| (200..300).contains(&f.status))
                .and_then(|f| f.text.as_deref())
                .filter(|t| !t.trim().is_empty())
                .map(str::to_string)
        };
        let robots_text = text(robots);
        Self {
            checked: true,
            robots: robots_text.is_some(),
            robots_disallows: robots_text
                .and_then(|t| pageon_net::Robots::parse(t.as_bytes(), "pageon").disallowed(path)),
            llms_txt: text(llms).is_some(),
            markdown: markdown.is_some_and(|f| f.source.as_deref() == Some("server-markdown")),
            without_scripts: markdown.map(|f| f.view_tokens),
        }
    }
}

/// One area of the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Area {
    /// `names`, `actions`, `tools`, `cost`, `access` or `health`.
    pub name: String,
    /// 0 to 100; `None` when it does not apply to the page.
    pub score: Option<u8>,
    /// Its weight in the overall score.
    pub weight: u8,
    /// What it found, in a few words.
    pub summary: String,
}

/// Something to fix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// The area.
    pub area: String,
    /// Points the overall score gains when it is fixed (0: a suggestion).
    pub gain: f32,
    /// What is wrong.
    pub problem: String,
    /// How to fix it.
    pub fix: String,
    /// The elements, as view lines (refs as in `pageon fetch URL --dump ax`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<String>,
}

/// What the agent's view costs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    /// Tokens of the whole agent view.
    pub view: usize,
    /// Tokens of the Markdown view.
    pub markdown: usize,
    /// Tokens of the HTML.
    pub html: usize,
    /// The largest landmark sections of the view.
    pub sections: Vec<(String, usize)>,
}

/// The audit of one page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// The page.
    pub url: String,
    /// 0 to 100: the weighted mean of the areas that apply.
    pub score: u8,
    /// The areas.
    pub areas: Vec<Area>,
    /// What to fix, the largest gain first.
    pub findings: Vec<Finding>,
    /// What the view costs.
    pub cost: Cost,
    /// The site's refusal, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<pageon_net::Refusal>,
}

/// A view line taken apart: `button e2 "Pay" covered`.
struct Line<'a> {
    role: &'a str,
    reference: &'a str,
    name: Option<String>,
    placeholder: bool,
    flags: Vec<&'a str>,
}

impl<'a> Line<'a> {
    fn parse(line: &'a str) -> Self {
        let line = line.trim();
        let (role, rest) = line.split_once(' ').unwrap_or((line, ""));
        let (reference, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let mut out = Line {
            role,
            reference,
            name: None,
            placeholder: false,
            flags: Vec::new(),
        };
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            // A token: up to a space outside quotes.
            let mut quoted = false;
            let mut escaped = false;
            let end = rest
                .char_indices()
                .find(|&(_, c)| {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == '"' {
                        quoted = !quoted;
                    } else if c == ' ' && !quoted {
                        return true;
                    }
                    false
                })
                .map_or(rest.len(), |(i, _)| i);
            let token = &rest[..end];
            rest = &rest[end..];
            if token.starts_with('"') && out.name.is_none() {
                out.name = serde_json::from_str::<String>(token).ok();
            } else if token.starts_with("ph:\"") {
                out.placeholder = true;
            } else if !token.contains('"') {
                out.flags.push(token);
            }
        }
        out
    }
}

/// Weights of the areas, in report order.
const AREAS: [(&str, u8); 6] = [
    ("names", 25),
    ("actions", 20),
    ("tools", 15),
    ("cost", 20),
    ("access", 10),
    ("health", 10),
];

/// A finding before its gain is in overall points: the area's gain in its own points.
struct Raw {
    area: &'static str,
    gain: f32,
    problem: String,
    fix: String,
    elements: Vec<String>,
}

fn refs(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| (*l).to_string()).collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl Report {
    /// Scores a page and its site.
    pub fn new(page: &PageFacts, site: &SiteFacts) -> Self {
        let mut raw = Vec::new();
        let mut scores: HashMap<&str, (Option<f32>, String)> = HashMap::new();
        let lines: Vec<Line<'_>> = page.elements.iter().map(|e| Line::parse(&e.line)).collect();
        scores.insert("names", names(page, &lines, &mut raw));
        scores.insert("actions", actions(page, &lines, &mut raw));
        scores.insert("tools", tools(page, &mut raw));
        scores.insert("cost", cost(page, &mut raw));
        scores.insert("access", access(page, site, &mut raw));
        scores.insert("health", health(page, &mut raw));
        let applies: u32 = AREAS
            .iter()
            .filter(|(n, _)| scores[n].0.is_some())
            .map(|(_, w)| u32::from(*w))
            .sum();
        let total: f32 = AREAS
            .iter()
            .filter_map(|(n, w)| scores[n].0.map(|s| s * f32::from(*w)))
            .sum();
        let score = if applies == 0 {
            0
        } else {
            (total / applies as f32).round() as u8
        };
        let areas = AREAS
            .iter()
            .map(|(n, w)| Area {
                name: (*n).to_string(),
                score: scores[n].0.map(|s| s.round() as u8),
                weight: *w,
                summary: scores[n].1.clone(),
            })
            .collect();
        let weight = |area: &str| {
            AREAS
                .iter()
                .find(|(n, _)| *n == area)
                .map_or(0.0, |(_, w)| f32::from(*w))
        };
        let mut findings: Vec<Finding> = raw
            .into_iter()
            .map(|r| Finding {
                area: r.area.to_string(),
                gain: if applies == 0 {
                    0.0
                } else {
                    (r.gain * weight(r.area) / applies as f32 * 10.0).round() / 10.0
                },
                problem: r.problem,
                fix: r.fix,
                elements: r.elements,
            })
            .collect();
        findings.sort_by(|a, b| b.gain.total_cmp(&a.gain));
        Self {
            url: page.url.clone(),
            score,
            areas,
            findings,
            cost: Cost {
                view: page.view_tokens,
                markdown: page.markdown_tokens,
                html: page.html_tokens,
                sections: page.sections.iter().take(3).cloned().collect(),
            },
            refusal: page.refusal.clone(),
        }
    }

    /// The score of an area, if it applies.
    pub fn area(&self, name: &str) -> Option<u8> {
        self.areas.iter().find(|a| a.name == name)?.score
    }
}

fn names(page: &PageFacts, lines: &[Line<'_>], raw: &mut Vec<Raw>) -> (Option<f32>, String) {
    if lines.is_empty() {
        return (None, "no controls".into());
    }
    let n = lines.len() as f32;
    let mut alike: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(name) = &l.name {
            alike.entry((l.role, name.as_str())).or_default().push(i);
        }
    }
    let mut groups: Vec<Vec<usize>> = alike.into_values().filter(|g| g.len() >= 3).collect();
    groups.sort_by_key(|g| g[0]);
    let unnamed: Vec<&str> = lines
        .iter()
        .zip(&page.elements)
        .filter(|(l, _)| l.name.is_none() && !l.placeholder)
        .map(|(_, e)| e.line.as_str())
        .collect();
    let placeholder: Vec<&str> = lines
        .iter()
        .zip(&page.elements)
        .filter(|(l, _)| l.name.is_none() && l.placeholder)
        .map(|(_, e)| e.line.as_str())
        .collect();
    let in_groups: usize = groups.iter().map(Vec::len).sum();
    let lost = unnamed.len() as f32 + 0.5 * (placeholder.len() + in_groups) as f32;
    if !unnamed.is_empty() {
        raw.push(Raw {
            area: "names",
            gain: 100.0 * unnamed.len() as f32 / n,
            problem: format!(
                "{} without a name: an agent cannot tell what {} for",
                plural(unnamed.len(), "control", "controls"),
                if unnamed.len() == 1 { "it is" } else { "they are" }
            ),
            fix: "Give icon buttons and links an aria-label, images in links an alt, and fields a <label>.".into(),
            elements: refs(&unnamed),
        });
    }
    if !placeholder.is_empty() {
        raw.push(Raw {
            area: "names",
            gain: 50.0 * placeholder.len() as f32 / n,
            problem: format!(
                "{} named only by a placeholder, which disappears once typed in",
                plural(placeholder.len(), "field", "fields")
            ),
            fix: "Label each field with a <label> (or aria-label); keep the placeholder as an example."
                .into(),
            elements: refs(&placeholder),
        });
    }
    if !groups.is_empty() {
        groups.sort_by_key(|g| std::cmp::Reverse(g.len()));
        let first = &lines[groups[0][0]];
        let name = first.name.as_deref().unwrap_or_default();
        raw.push(Raw {
            area: "names",
            gain: 50.0 * in_groups as f32 / n,
            problem: if groups.len() == 1 {
                format!(
                    "{} {}s {} share a name: an agent cannot tell them apart",
                    in_groups,
                    first.role,
                    quote(name)
                )
            } else {
                format!(
                    "{in_groups} controls share {} names: an agent cannot tell them apart",
                    groups.len()
                )
            },
            fix: format!(
                "Name each for what it acts on, e.g. aria-label={}, or add visually hidden text.",
                quote(&format!("{name} <item>"))
            ),
            elements: groups
                .iter()
                .map(|g| {
                    let l = &lines[g[0]];
                    let refs: Vec<&str> = g.iter().take(4).map(|&i| lines[i].reference).collect();
                    format!(
                        "{} × {} {}: {}{}",
                        g.len(),
                        l.role,
                        quote(l.name.as_deref().unwrap_or_default()),
                        refs.join(", "),
                        if g.len() > 4 { ", …" } else { "" }
                    )
                })
                .collect(),
        });
    }
    let mut summary = Vec::new();
    if !unnamed.is_empty() {
        summary.push(format!("{} unnamed", unnamed.len()));
    }
    if !placeholder.is_empty() {
        summary.push(format!("{} placeholder only", placeholder.len()));
    }
    if in_groups > 0 {
        summary.push(format!("{in_groups} alike"));
    }
    if summary.is_empty() {
        summary.push(format!(
            "{} named",
            plural(lines.len(), "control", "controls")
        ));
    }
    (Some(100.0 * (1.0 - lost / n)), summary.join(" · "))
}

/// Elements whose target is under 24 × 24 px with less than that room around it (WCAG 2.5.8);
/// links are not measured, as links in text are exempt.
fn small(page: &PageFacts, lines: &[Line<'_>]) -> Vec<usize> {
    let rects: Vec<Option<[f32; 4]>> = page
        .elements
        .iter()
        .map(|e| e.rect.filter(|r| r[2] > 0.0 && r[3] > 0.0))
        .collect();
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(r) = rects[i] else { continue };
        if l.role == "link" || (r[2] >= TARGET && r[3] >= TARGET) {
            continue;
        }
        // The 24 px square centred on the target must not touch another target.
        let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        let sq = [cx - TARGET / 2.0, cy - TARGET / 2.0, TARGET, TARGET];
        let crowded = rects.iter().enumerate().any(|(j, o)| {
            j != i
                && o.is_some_and(|o| {
                    sq[0] < o[0] + o[2]
                        && o[0] < sq[0] + sq[2]
                        && sq[1] < o[1] + o[3]
                        && o[1] < sq[1] + sq[3]
                })
        });
        if crowded {
            out.push(i);
        }
    }
    out
}

fn actions(page: &PageFacts, lines: &[Line<'_>], raw: &mut Vec<Raw>) -> (Option<f32>, String) {
    if lines.is_empty() {
        return (None, "no controls".into());
    }
    let n = lines.len() as f32;
    let covered: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].flags.contains(&"covered"))
        .collect();
    let fake: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].role == "clickable")
        .collect();
    let small = small(page, lines);
    let bad: std::collections::HashSet<usize> =
        covered.iter().chain(&fake).chain(&small).copied().collect();
    let pick = |ix: &[usize]| -> Vec<String> {
        ix.iter().map(|&i| page.elements[i].line.clone()).collect()
    };
    if !covered.is_empty() {
        raw.push(Raw {
            area: "actions",
            gain: 100.0 * covered.len() as f32 / n,
            problem: format!(
                "{} covered by another element: a click lands on what covers {}",
                plural(covered.len(), "control", "controls"),
                if covered.len() == 1 { "it" } else { "them" }
            ),
            fix: "Move overlays, banners and sticky bars off controls, or let clicks through (pointer-events: none).".into(),
            elements: pick(&covered),
        });
    }
    if !fake.is_empty() {
        raw.push(Raw {
            area: "actions",
            gain: 100.0 * fake.len() as f32 / n,
            problem: format!(
                "{} only {} for clicks: no button role, no keyboard",
                plural(fake.len(), "element", "elements"),
                if fake.len() == 1 { "listens" } else { "listen" }
            ),
            fix: "Use <button> (or <a href> to navigate) instead of a clickable <div>.".into(),
            elements: pick(&fake),
        });
    }
    if !small.is_empty() {
        raw.push(Raw {
            area: "actions",
            gain: 100.0 * small.len() as f32 / n,
            problem: format!(
                "{} smaller than 24 × 24 px with neighbours too close (WCAG 2.5.8)",
                plural(small.len(), "target", "targets")
            ),
            fix: "Make targets at least 24 × 24 px, or keep 24 px of room around them.".into(),
            elements: pick(&small),
        });
    }
    let mut summary = Vec::new();
    if !covered.is_empty() {
        summary.push(format!("{} covered", covered.len()));
    }
    if !fake.is_empty() {
        summary.push(plural(fake.len(), "fake button", "fake buttons"));
    }
    if !small.is_empty() {
        summary.push(format!("{} small", small.len()));
    }
    if summary.is_empty() {
        summary.push("every control can be acted on".into());
    }
    (
        Some(100.0 * (1.0 - bad.len() as f32 / n)),
        summary.join(" · "),
    )
}

fn tools(page: &PageFacts, raw: &mut Vec<Raw>) -> (Option<f32>, String) {
    for f in &page.forms {
        raw.push(Raw {
            area: "tools",
            gain: 0.0,
            problem: "a form an agent fills field by field".into(),
            fix: "Make it a page tool: <form toolname=\"…\" tooldescription=\"…\"> (WebMCP)."
                .into(),
            elements: vec![f.clone()],
        });
    }
    if page.tools.is_empty() {
        return (
            None,
            if page.forms.is_empty() {
                "no page tools".into()
            } else {
                format!(
                    "no page tools · {} could be",
                    plural(page.forms.len(), "form", "forms")
                )
            },
        );
    }
    let n = page.tools.len() as f32;
    let mut total = 0.0;
    for t in &page.tools {
        let described = t.description.trim().chars().count() >= 12
            && !t.description.trim().eq_ignore_ascii_case(&t.name);
        let params = if t.params == 0 {
            1.0
        } else {
            t.described as f32 / t.params as f32
        };
        let credit = 0.5 * f32::from(u8::from(described)) + 0.5 * params;
        total += credit;
        if credit < 1.0 {
            let mut what = Vec::new();
            if !described {
                what.push("a description of what it does".to_string());
            }
            if t.described < t.params {
                what.push(format!(
                    "descriptions of {} of its {}",
                    t.params - t.described,
                    plural(t.params as usize, "parameter", "parameters")
                ));
            }
            raw.push(Raw {
                area: "tools",
                gain: 100.0 * (1.0 - credit) / n,
                problem: format!("tool {} lacks {}", quote(&t.name), what.join(" and ")),
                fix: "Say what the tool does and when to use it; describe every parameter in its input schema.".into(),
                elements: Vec::new(),
            });
        }
    }
    (
        Some(100.0 * total / n),
        format!(
            "{} · {}",
            plural(page.tools.len(), "tool", "tools"),
            page.tools
                .iter()
                .map(|t| quote(&t.name))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )
}

fn cost(page: &PageFacts, raw: &mut Vec<Raw>) -> (Option<f32>, String) {
    let v = page.view_tokens.max(1) as f32;
    let score = if page.view_tokens <= DEFAULT_BUDGET {
        100.0
    } else {
        100.0 * (DEFAULT_BUDGET as f32 / v).sqrt()
    };
    if page.view_tokens > DEFAULT_BUDGET {
        let largest = page
            .sections
            .iter()
            .take(3)
            .map(|(s, t)| {
                format!(
                    "{} {}%",
                    s.trim_start_matches("## "),
                    t * 100 / page.view_tokens.max(1)
                )
            })
            .collect::<Vec<_>>();
        raw.push(Raw {
            area: "cost",
            gain: 100.0 - score,
            problem: format!(
                "the agent view needs {} tokens; an agent's {} token view shows a part",
                thousands(page.view_tokens),
                thousands(DEFAULT_BUDGET)
            ),
            fix: if largest.is_empty() {
                "Mark up landmarks (header, nav, main, footer) and keep repeated blocks short.".into()
            } else {
                format!(
                    "Trim the largest parts ({}): long menus behind a button, repeated links and footers.",
                    largest.join(", ")
                )
            },
            elements: Vec::new(),
        });
    }
    let ratio = (page.view_tokens * 100)
        .checked_div(page.html_tokens)
        .map(|p| format!(" ({}% of the HTML)", p.max(1)))
        .unwrap_or_default();
    (
        Some(score),
        format!("{} tokens{ratio}", thousands(page.view_tokens)),
    )
}

fn access(page: &PageFacts, site: &SiteFacts, raw: &mut Vec<Raw>) -> (Option<f32>, String) {
    if let Some(r) = &page.refusal {
        raw.push(Raw {
            area: "access",
            gain: 100.0,
            problem: format!("the site refuses an honest agent: {r}"),
            fix: "Let declared agents through: verify signed agents (Web Bot Auth) instead of challenging them, and say what you allow in robots.txt.".into(),
            elements: Vec::new(),
        });
        return (Some(0.0), format!("refused ({})", r.kind.as_str()));
    }
    if !site.checked {
        return (None, "not a web page".into());
    }
    // Content without scripts: 25 points for at least half of the view.
    let share = site.without_scripts.map_or(1.0, |t| {
        (t as f32 / page.view_tokens.max(1) as f32 / 0.5).min(1.0)
    });
    let mut score = 30.0 + 25.0 * share;
    let mut has = Vec::new();
    let mut lacks = Vec::new();
    if share < 1.0 {
        let t = site.without_scripts.unwrap_or_default();
        lacks.push("little without scripts".to_string());
        raw.push(Raw {
            area: "access",
            gain: 25.0 * (1.0 - share),
            problem: format!(
                "agents that do not run scripts see {}% of the page ({} of {} tokens)",
                (t * 100 / page.view_tokens.max(1)).min(100),
                thousands(t),
                thousands(page.view_tokens)
            ),
            fix: "Render the main content on the server: many AI crawlers do not run scripts."
                .into(),
            elements: Vec::new(),
        });
    }
    let mut check = |ok: bool, name: &str, problem: &str, fix: &str| {
        if ok {
            score += 15.0;
            has.push(name.to_string());
        } else {
            lacks.push(format!("no {name}"));
            raw.push(Raw {
                area: "access",
                gain: 15.0,
                problem: problem.into(),
                fix: fix.into(),
                elements: Vec::new(),
            });
        }
    };
    check(
        site.robots,
        "robots.txt",
        "no robots.txt: agents cannot tell what the site allows",
        "Publish /robots.txt with rules for agents (and Content-Usage for AI use).",
    );
    check(
        site.llms_txt,
        "llms.txt",
        "no llms.txt: no index of the site for agents",
        "Publish /llms.txt: a Markdown list of the site's key pages (llmstxt.org).",
    );
    check(
        site.markdown,
        "Markdown",
        "no Markdown on request: agents read the page through its HTML",
        "Answer Accept: text/markdown with the page as Markdown.",
    );
    if let Some(why) = &site.robots_disallows {
        raw.push(Raw {
            area: "access",
            gain: 0.0,
            problem: format!("robots.txt keeps agents named pageon off this page ({why})"),
            fix: "Allow the agents you want on the page in robots.txt.".into(),
            elements: Vec::new(),
        });
    }
    let summary = if lacks.is_empty() {
        has.join(" · ")
    } else {
        lacks.join(" · ")
    };
    (Some(score), summary)
}

fn health(page: &PageFacts, raw: &mut Vec<Raw>) -> (Option<f32>, String) {
    if page.status >= 400 && page.refusal.is_none() {
        raw.push(Raw {
            area: "health",
            gain: 100.0,
            problem: format!("the page answered HTTP {}", page.status),
            fix: "Serve the page with a 2xx status.".into(),
            elements: Vec::new(),
        });
        return (Some(0.0), format!("HTTP {}", page.status));
    }
    let n = page.errors.len();
    let mut score = (100.0 - 20.0 * n as f32).max(0.0);
    // A page that shows an agent next to nothing although it sent plenty.
    let empty = page.elements.is_empty() && page.view_tokens < 100 && page.html_tokens > 1000;
    if empty {
        raw.push(Raw {
            area: "health",
            gain: score.min(50.0),
            problem: format!(
                "an agent sees almost nothing: {} tokens of view from {} of HTML",
                page.view_tokens,
                thousands(page.html_tokens)
            ),
            fix: "Render the main content on the server; see what the page waits for with pageon fetch URL --dump requests.".into(),
            elements: Vec::new(),
        });
        score = score.min(50.0);
    }
    if n > 0 {
        raw.push(Raw {
            area: "health",
            gain: 100.0 - score,
            problem: format!(
                "{} while loading: {}",
                plural(n, "script error", "script errors"),
                page.errors.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            ),
            fix: "Fix them (pageon fetch URL --dump errors lists them): what they break, agents miss. An error only pageon shows may be a gap in pageon; report it.".into(),
            elements: Vec::new(),
        });
    }
    (
        Some(score),
        match (n, empty) {
            (_, true) => "almost nothing to read".into(),
            (0, _) => "no script errors".into(),
            _ => plural(n, "script error", "script errors"),
        },
    )
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// How a report is printed.
#[derive(Debug, Clone, Copy, Default)]
pub struct Style {
    /// ANSI colours.
    pub color: bool,
}

impl Report {
    /// The report for a terminal: the score, the areas, then what to fix first.
    pub fn render(&self, style: Style) -> String {
        let paint = |code: &str, s: &str| {
            if style.color {
                format!("\x1b[{code}m{s}\x1b[0m")
            } else {
                s.to_string()
            }
        };
        let tone = |score: u8| match score {
            90..=100 => "32",
            50..=89 => "33",
            _ => "31",
        };
        let mut out = String::new();
        let _ = writeln!(
            out,
            "\n  {}  agent readiness · {}\n",
            paint(
                &format!("1;{}", tone(self.score)),
                &format!("{:>3}", self.score)
            ),
            self.url
        );
        for a in &self.areas {
            let (bar, num) = match a.score {
                Some(s) => {
                    let full = (usize::from(s) + 5) / 10;
                    (
                        paint(
                            tone(s),
                            &format!("{}{}", "█".repeat(full), "░".repeat(10 - full)),
                        ),
                        format!("{s:>3}"),
                    )
                }
                None => (paint("2", &"·".repeat(10)), "  –".to_string()),
            };
            let _ = writeln!(
                out,
                "  {:<8} {bar} {num}  {}",
                title(&a.name),
                paint("2", &a.summary)
            );
        }
        let fixes: Vec<&Finding> = self.findings.iter().filter(|f| f.gain > 0.0).collect();
        let ideas: Vec<&Finding> = self.findings.iter().filter(|f| f.gain <= 0.0).collect();
        if !fixes.is_empty() {
            let _ = writeln!(out, "\n  {}", paint("1", "Fix first"));
            for f in fixes {
                finding(&mut out, f, &paint, true);
            }
        }
        if !ideas.is_empty() {
            let _ = writeln!(out, "\n  {}", paint("1", "Also"));
            for f in ideas {
                finding(&mut out, f, &paint, false);
            }
        }
        if self.findings.iter().any(|f| !f.elements.is_empty()) {
            let _ = writeln!(
                out,
                "\n  {}",
                paint("2", "Refs are those of `pageon fetch URL --dump ax`.")
            );
        }
        out
    }
}

fn title(area: &str) -> String {
    let mut c = area.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

fn finding(out: &mut String, f: &Finding, paint: &dyn Fn(&str, &str) -> String, gain: bool) {
    let head = if gain {
        paint(
            "32",
            &format!("{:>5}", format!("+{}", f.gain.round().max(1.0))),
        )
    } else {
        paint("2", "    ·")
    };
    let _ = writeln!(out, "{head}  {}", f.problem);
    const SHOWN: usize = 6;
    for e in f.elements.iter().take(SHOWN) {
        let _ = writeln!(out, "         {}", paint("36", e));
    }
    if f.elements.len() > SHOWN {
        let _ = writeln!(
            out,
            "         {}",
            paint("2", &format!("… and {} more", f.elements.len() - SHOWN))
        );
    }
    let _ = writeln!(out, "         {}", paint("2", &format!("→ {}", f.fix)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(line: &str, rect: [f32; 4]) -> ElementFact {
        ElementFact {
            line: line.into(),
            rect: Some(rect),
        }
    }

    #[test]
    fn lines_come_apart() {
        let l = Line::parse(r#"button e13 "Under \"x\" covered" covered"#);
        assert_eq!(l.role, "button");
        assert_eq!(l.name.as_deref(), Some("Under \"x\" covered"));
        assert_eq!(l.flags, vec!["covered"]);
        let l = Line::parse(r#"textbox e6 ph:"Email""#);
        assert!(l.name.is_none() && l.placeholder);
        let l = Line::parse("link e1");
        assert!(l.name.is_none() && !l.placeholder && l.flags.is_empty());
    }

    #[test]
    fn a_clean_page_scores_high_and_problems_are_found() {
        let clean = PageFacts {
            url: "https://shop.test/".into(),
            status: 200,
            elements: vec![
                el(r#"link e1 "Home""#, [0.0, 0.0, 40.0, 20.0]),
                el(r#"button e2 "Search""#, [100.0, 0.0, 60.0, 30.0]),
            ],
            view_tokens: 300,
            ..PageFacts::default()
        };
        let site = SiteFacts {
            checked: true,
            robots: true,
            llms_txt: true,
            markdown: true,
            ..SiteFacts::default()
        };
        let r = Report::new(&clean, &site);
        assert_eq!(r.score, 100, "{r:#?}");
        assert!(r.findings.is_empty());
        assert_eq!(r.area("tools"), None);

        let messy = PageFacts {
            elements: vec![
                el("link e1", [0.0, 0.0, 20.0, 20.0]),
                el(r#"textbox e2 ph:"Email""#, [0.0, 100.0, 150.0, 21.0]),
                el(r#"link e3 "Edit""#, [0.0, 200.0, 30.0, 18.0]),
                el(r#"link e4 "Edit""#, [0.0, 220.0, 30.0, 18.0]),
                el(r#"link e5 "Edit""#, [0.0, 240.0, 30.0, 18.0]),
                el(r#"clickable e6 "Fake""#, [0.0, 300.0, 100.0, 30.0]),
                el(r#"button e7 "x""#, [0.0, 400.0, 8.0, 8.0]),
                el(r#"button e8 "Under" covered"#, [0.0, 410.0, 50.0, 30.0]),
            ],
            errors: vec!["\"boom\"".into()],
            view_tokens: 16_000,
            forms: vec![r#"textbox e2 ph:"Email""#.into()],
            ..clean
        };
        let r = Report::new(
            &messy,
            &SiteFacts {
                checked: true,
                ..SiteFacts::default()
            },
        );
        assert!(r.score < 70, "{}", r.render(Style::default()));
        let problems: Vec<&str> = r.findings.iter().map(|f| f.problem.as_str()).collect();
        for want in [
            "1 control without a name",
            "1 field named only by a placeholder",
            "3 links \"Edit\" share a name",
            "1 control covered",
            "1 element only listens for clicks",
            "1 target smaller",
            "needs 16,000 tokens",
            "no llms.txt",
            "1 script error",
            "a form an agent fills",
        ] {
            assert!(
                problems.iter().any(|p| p.contains(want)),
                "{want}: {problems:#?}"
            );
        }
        // The largest gain first; suggestions last.
        assert!(r.findings.windows(2).all(|w| w[0].gain >= w[1].gain));
        assert_eq!(r.findings.last().map(|f| f.gain), Some(0.0));
        // The 8 px button is crowded by the covered one; the field is not crowded.
        let small = r.findings.iter().find(|f| f.problem.contains("smaller"));
        assert_eq!(
            small.map(|f| f.elements.clone()),
            Some(vec![r#"button e7 "x""#.to_string()])
        );
    }

    #[test]
    fn refusal_zeroes_access_and_tools_are_judged() {
        let page = PageFacts {
            status: 403,
            refusal: Some(pageon_net::Refusal {
                kind: pageon_net::RefusalKind::Challenge,
                vendor: Some("cloudflare".into()),
                status: 403,
                retry_after: None,
                retry_capped: false,
                detail: String::new(),
                tag: None,
            }),
            tools: vec![
                ToolFact {
                    name: "search".into(),
                    description: "Search the catalogue by keyword".into(),
                    params: 1,
                    described: 1,
                },
                ToolFact {
                    name: "buy".into(),
                    description: "buy".into(),
                    params: 2,
                    described: 0,
                },
            ],
            ..PageFacts::default()
        };
        let r = Report::new(&page, &SiteFacts::default());
        assert_eq!(r.area("access"), Some(0));
        assert_eq!(r.area("health"), Some(100));
        assert_eq!(r.area("tools"), Some(50));
        assert!(r.findings[0].problem.contains("refuses"));
    }

    #[test]
    fn content_without_scripts_and_empty_views() {
        let page = PageFacts {
            status: 200,
            view_tokens: 1000,
            html_tokens: 20_000,
            elements: vec![el(r#"link e1 "Home""#, [0.0, 0.0, 40.0, 30.0])],
            ..PageFacts::default()
        };
        let site = SiteFacts {
            checked: true,
            robots: true,
            llms_txt: true,
            markdown: true,
            without_scripts: Some(100),
            ..SiteFacts::default()
        };
        let r = Report::new(&page, &site);
        assert_eq!(r.area("access"), Some(80), "{r:#?}");
        assert!(r.findings[0].problem.contains("see 10% of the page"));
        let empty = PageFacts {
            elements: Vec::new(),
            view_tokens: 40,
            ..page
        };
        let r = Report::new(&empty, &SiteFacts::default());
        assert_eq!(r.area("health"), Some(50));
        assert!(
            r.findings
                .iter()
                .any(|f| f.problem.contains("almost nothing"))
        );
    }

    #[test]
    fn sections_of_a_view() {
        let view = "@page v1\n## banner\nlink e1 \"Home\"\n## main\n# \"Shoes\"\ntext \"a b c d e f g h\"\n";
        let s = sections(view);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].0, "## main");
        assert!(sections("@page v1\nlink e1\n").is_empty());
    }

    #[test]
    fn site_files() {
        let ok = |text: &str, source: &str| PageFacts {
            status: 200,
            source: Some(source.into()),
            text: Some(text.into()),
            ..PageFacts::default()
        };
        let robots = ok("User-agent: *\nDisallow: /private\n", "text");
        let s = SiteFacts::from_loads("/private/x", Some(&robots), None, None);
        assert!(s.robots && !s.llms_txt && !s.markdown);
        assert!(s.robots_disallows.is_some());
        let md = ok("# Shop", "server-markdown");
        let s = SiteFacts::from_loads("/", None, Some(&ok("# Shop\n", "markdown")), Some(&md));
        assert!(!s.robots && s.llms_txt && s.markdown && s.robots_disallows.is_none());
    }
}
