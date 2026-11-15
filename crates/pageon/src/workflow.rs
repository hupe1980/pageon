//! Workflows: a solved task compiled into a straight line of actions on elements named
//! as the agent saw them (role and name, never refs), with parameters for what changes. A run
//! goes live through the session, without a model; when the site has moved on it stops with a
//! precise drift report. Repair is compiling the session's steps again.
//!
//! ```no_run
//! use std::collections::HashMap;
//!
//! use pageon::replay::InProcess;
//! use pageon::{Live, Session, workflow};
//!
//! let log: pageon_env::Log = serde_json::from_str(&std::fs::read_to_string("ep.json")?)?;
//! let wf = workflow::compile(&log.steps)?;
//! let session = Session::new(Live::default());
//! let mut driver = InProcess::new(&session);
//! let params = HashMap::from([("user".to_string(), "bob".to_string())]);
//! let report = workflow::run(&mut driver, &wf, &params, Default::default(), &mut |line| eprintln!("{line}"))?;
//! println!("{}", report.status);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::HashMap;
use std::fmt;

use pageon_env::{Step, Target};
use serde::{Deserialize, Serialize};

use crate::replay::Driver;
use crate::{Action, ActionError, Dump, LoadOptions};

/// The format version.
pub const VERSION: u32 = 1;

/// A compiled workflow (a file the user owns).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workflow {
    /// Format version.
    pub workflow: u32,
    /// The URL the task starts at.
    pub start: String,
    /// What a run fills in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    /// The steps, in order.
    pub steps: Vec<WStep>,
}

/// A parameter: `{{name}}` in a step's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Param {
    /// Name (`user`).
    pub name: String,
    /// The value recorded when compiled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// How a step finds its element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Locator {
    /// As the agent saw it: role, name, index among equal ones.
    Element(Target),
    /// A ref of the recorded page (recordings without target lines): valid only there.
    Ref {
        /// The ref.
        #[serde(rename = "ref")]
        reference: String,
    },
}

/// One step: pageon's action, with a locator where the ref was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "lowercase")]
pub enum WStep {
    /// Click.
    Click {
        /// The element.
        target: Locator,
        /// Where the step leads (scheme, host, path).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<String>,
    },
    /// Hover.
    Hover {
        /// The element.
        target: Locator,
    },
    /// Type text (a literal, `{{param}}` or `secret://name`).
    Type {
        /// The element.
        target: Locator,
        /// The text.
        text: String,
        /// Where the step leads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<String>,
    },
    /// Upload staged files by name.
    Upload {
        /// The file input, or what opens it.
        target: Locator,
        /// Staged file names.
        files: Vec<String>,
    },
    /// Select options.
    Select {
        /// The `<select>`.
        target: Locator,
        /// Values or labels.
        options: Vec<String>,
    },
    /// Press a key on the focused element.
    Press {
        /// The key.
        key: String,
        /// Where the step leads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<String>,
    },
    /// Let time pass.
    Wait {
        /// Milliseconds.
        ms: u64,
    },
    /// Load a URL.
    Goto {
        /// The URL.
        url: String,
    },
    /// Scroll: the viewport, a container, or an element into view.
    Scroll {
        /// The element, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<Locator>,
        /// How far.
        by: crate::ScrollBy,
    },
    /// History back.
    Back,
    /// History forward.
    Forward,
    /// Call a tool the page registered (WebMCP).
    Tool {
        /// The tool's name.
        name: String,
        /// The registering origin, when it was chosen explicitly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        /// The input object.
        input: serde_json::Value,
        /// Where the step leads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<String>,
    },
}

impl WStep {
    fn target(&self) -> Option<&Locator> {
        match self {
            Self::Click { target, .. }
            | Self::Hover { target }
            | Self::Type { target, .. }
            | Self::Upload { target, .. }
            | Self::Select { target, .. } => Some(target),
            Self::Scroll { target, .. } => target.as_ref(),
            _ => None,
        }
    }

    fn expect(&self) -> Option<&str> {
        match self {
            Self::Click { expect, .. }
            | Self::Type { expect, .. }
            | Self::Press { expect, .. }
            | Self::Tool { expect, .. } => expect.as_deref(),
            _ => None,
        }
    }

    fn set_expect(&mut self, url: String) {
        if let Self::Click { expect, .. }
        | Self::Type { expect, .. }
        | Self::Press { expect, .. }
        | Self::Tool { expect, .. } = self
        {
            *expect = Some(url);
        }
    }
}

impl fmt::Display for Locator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Element(t) => {
                write!(f, "{} {}", t.role, pageon_agent::quote::quote(&t.name))?;
                if t.index > 0 {
                    write!(f, " #{}", t.index + 1)?;
                }
                Ok(())
            }
            Self::Ref { reference } => write!(f, "{reference} (recorded ref)"),
        }
    }
}

impl fmt::Display for WStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let q = pageon_agent::quote::quote;
        match self {
            Self::Click { target, .. } => write!(f, "click {target}"),
            Self::Hover { target } => write!(f, "hover {target}"),
            Self::Type { target, text, .. } => write!(f, "type {target} {}", q(text)),
            Self::Upload { target, files } => write!(f, "upload {target} {}", files.join(",")),
            Self::Select { target, options } => {
                write!(f, "select {target}")?;
                for o in options {
                    write!(f, " {}", q(o))?;
                }
                Ok(())
            }
            Self::Press { key, .. } => write!(f, "press {key}"),
            Self::Wait { ms } => write!(f, "wait {ms}"),
            Self::Goto { url } => write!(f, "goto {url}"),
            Self::Back => f.write_str("back"),
            Self::Forward => f.write_str("forward"),
            Self::Scroll { target, by } => {
                // As the action reads, with the locator in place of a ref.
                let text = Action::Scroll {
                    target: target.as_ref().map(|_| "\u{1}".to_string()),
                    by: *by,
                    point: None,
                }
                .to_string();
                match target {
                    Some(t) => f.write_str(&text.replace('\u{1}', &t.to_string())),
                    None => f.write_str(&text),
                }
            }
            Self::Tool { name, input, .. } => write!(f, "tool {name} {input}"),
        }
    }
}

/// An element of an agent view line (`button e5 "Sign in" focused`): role, ref, name.
pub(crate) fn element(line: &str) -> Option<(String, String, String)> {
    let line = line.trim_start();
    let (role, rest) = line.split_once(' ')?;
    let (r, rest) = rest.split_once(' ').unwrap_or((rest, ""));
    r.strip_prefix('e')?.parse::<u32>().ok()?;
    let name = rest
        .strip_prefix('"')
        .and_then(|_| unquote(rest))
        .unwrap_or_default();
    Some((role.to_string(), r.to_string(), name))
}

/// The first JSON-style quoted string at the start of `s`.
fn unquote(s: &str) -> Option<String> {
    let mut end = None;
    let mut escaped = false;
    for (i, c) in s.char_indices().skip(1) {
        match (escaped, c) {
            (true, _) => escaped = false,
            (false, '\\') => escaped = true,
            (false, '"') => {
                end = Some(i);
                break;
            }
            _ => {}
        }
    }
    serde_json::from_str(&s[..=end?]).ok()
}

/// The elements with refs in a view, in order: `(role, ref, name)`.
fn elements(view: &str) -> Vec<(String, String, String)> {
    view.lines().filter_map(element).collect()
}

/// The target of `reference` in the agent's view `lines`, with its index among equal ones.
pub(crate) fn target_in<'a>(
    lines: impl Iterator<Item = &'a str>,
    reference: &str,
) -> Option<Target> {
    let all: Vec<(String, String, String)> = lines.filter_map(element).collect();
    let (role, _, name) = all.iter().find(|(_, r, _)| r == reference)?.clone();
    let index = all
        .iter()
        .take_while(|(_, r, _)| r != reference)
        .filter(|(ro, _, n)| *ro == role && *n == name)
        .count();
    Some(Target { role, name, index })
}

/// The URL of a view's header (`@page v2 "Title" https://… …`).
fn view_url(view: &str) -> Option<String> {
    let header = view.lines().find(|l| l.starts_with("@page"))?;
    let rest = header.splitn(3, ' ').nth(2)?;
    let after_title = match rest.strip_prefix('"') {
        Some(_) => {
            let title = unquote(rest)?;
            let len = pageon_agent::quote::quote(&title).len();
            rest.get(len..)?.trim_start()
        }
        None => rest,
    };
    after_title.split_whitespace().next().map(str::to_string)
}

/// A URL as an expectation compares it: scheme, host and path.
fn place(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(u) => format!(
            "{}://{}{}",
            u.scheme(),
            u.host_str().unwrap_or_default(),
            u.path()
        ),
        Err(_) => url.to_string(),
    }
}

fn slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let s = s
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if s.is_empty() { "text".into() } else { s }
}

/// Compiles recorded steps into a workflow. Typed literals become parameters named
/// after their fields; `secret://` handles stay handles.
pub fn compile(steps: &[Step]) -> Result<Workflow, String> {
    let first = steps.first().ok_or("the recording has no steps")?;
    let start = first
        .input
        .get("open")
        .and_then(|o| o.get("url"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| first.action.strip_prefix("open ").map(str::to_string))
        .ok_or("the first step is not an `open`")?;
    let mut wf = Workflow {
        workflow: VERSION,
        start,
        params: Vec::new(),
        steps: Vec::new(),
    };
    let mut at = view_url(&first.view).map(|u| place(&u));
    for step in &steps[1..] {
        let action: Action = if step.input.is_null() {
            step.action.parse()?
        } else {
            serde_json::from_value(step.input.clone())
                .map_err(|e| format!("step {}: {e}", step.action))?
        };
        let locator = |r: &str| match &step.target {
            Some(t) => Locator::Element(t.clone()),
            None => Locator::Ref {
                reference: r.to_string(),
            },
        };
        let mut w = match action {
            Action::Click(r) => WStep::Click {
                target: locator(&r),
                expect: None,
            },
            Action::Hover(r) => WStep::Hover {
                target: locator(&r),
            },
            Action::Type(r, text) => {
                let text = if text.starts_with("secret://") || text.is_empty() {
                    text
                } else {
                    let base = slug(step.target.as_ref().map_or("", |t| t.name.as_str()));
                    let mut name = base.clone();
                    let mut n = 1;
                    while wf.params.iter().any(|p| p.name == name) {
                        n += 1;
                        name = format!("{base}_{n}");
                    }
                    wf.params.push(Param {
                        name: name.clone(),
                        default: Some(text),
                    });
                    format!("{{{{{name}}}}}")
                };
                WStep::Type {
                    target: locator(&r),
                    text,
                    expect: None,
                }
            }
            Action::Select(r, options) => WStep::Select {
                target: locator(&r),
                options,
            },
            Action::Press(key) => WStep::Press { key, expect: None },
            Action::Wait(ms) => WStep::Wait { ms },
            Action::Goto(url) => WStep::Goto { url },
            Action::Back => WStep::Back,
            Action::Forward => WStep::Forward,
            Action::Scroll {
                target,
                by,
                point: None,
            } => WStep::Scroll {
                target: target.as_deref().map(locator),
                by,
            },
            Action::Tool {
                name,
                origin,
                input,
            } => WStep::Tool {
                name,
                origin,
                input,
                expect: None,
            },
            Action::Upload(r, files) => {
                if files.iter().any(|f| f.starts_with("download:")) {
                    return Err(format!(
                        "step `{}` uploads a download of this session; workflows upload staged files (--file)",
                        step.action
                    ));
                }
                WStep::Upload {
                    target: locator(&r),
                    files,
                }
            }
            Action::ClickAt(..)
            | Action::ClickWith { .. }
            | Action::Hold(..)
            | Action::TypeFocused(_)
            | Action::Eval(_)
            | Action::Scroll { point: Some(_), .. }
            | Action::HoverAt(..)
            | Action::PointerDown(..)
            | Action::PointerUp(..)
            | Action::DragEvent { .. } => {
                return Err(format!(
                    "step `{}` acts at a point; workflows name elements (record with refs)",
                    step.action
                ));
            }
            Action::Drag(..) => {
                return Err(format!(
                    "step `{}` drags; workflows do not drag yet (finish this part by hand)",
                    step.action
                ));
            }
        };
        let now = view_url(&step.view).map(|u| place(&u));
        if now.is_some() && now != at {
            w.set_expect(now.clone().unwrap_or_default());
        }
        at = now;
        wf.steps.push(w);
    }
    Ok(wf)
}

/// A run's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// `done` or `drifted`.
    pub status: Status,
    /// Steps completed (the opening included).
    pub ran: usize,
    /// Renamed targets the run followed (soft drift).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drifts: Vec<Drift>,
    /// Where and why a drifted run stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Every step ran.
    Done,
    /// The site moved on: a step could not be done as compiled.
    Drifted,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Done => "done",
            Self::Drifted => "drifted",
        })
    }
}

/// A renamed target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drift {
    /// The step (1 is the first after opening).
    pub step: usize,
    /// The compiled name.
    pub from: String,
    /// The name on the page.
    pub to: String,
}

/// Where a drifted run stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    /// The step.
    pub step: usize,
    /// What could not be done.
    pub reason: String,
    /// Elements of the same role on the page (`button e9 "Log in"`), most similar first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// The agent view at that point.
    pub view: String,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.stop {
            None => write!(
                f,
                "done: {} steps{}",
                self.ran.saturating_sub(1),
                match self.drifts.len() {
                    0 => String::new(),
                    1 => ", 1 drift".into(),
                    n => format!(", {n} drifts"),
                }
            ),
            Some(s) => {
                write!(f, "drifted at step {}: {}", s.step, s.reason)?;
                if !s.candidates.is_empty() {
                    write!(f, "\n  candidates: {}", s.candidates.join(", "))?;
                }
                Ok(())
            }
        }
    }
}

/// Dice similarity of the character bigrams of two names (case and spaces ignored).
fn similarity(a: &str, b: &str) -> f64 {
    let norm = |s: &str| -> Vec<char> {
        s.to_lowercase()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    };
    let (a, b) = (norm(a), norm(b));
    if a == b {
        return 1.0;
    }
    let grams = |v: &[char]| -> Vec<(char, char)> { v.windows(2).map(|w| (w[0], w[1])).collect() };
    let (ga, mut gb) = (grams(&a), grams(&b));
    if ga.is_empty() || gb.is_empty() {
        return 0.0;
    }
    let total = ga.len() + gb.len();
    let mut common = 0;
    for g in ga {
        if let Some(i) = gb.iter().position(|x| *x == g) {
            gb.swap_remove(i);
            common += 1;
        }
    }
    2.0 * common as f64 / total as f64
}

/// The similarity at which a differently named element of the same role is taken as renamed.
pub const RENAMED: f64 = 0.75;

enum Found {
    Exact(String),
    Renamed(String, String),
    Missing(String, Vec<String>),
}

fn resolve(view: &str, locator: &Locator) -> Found {
    let els = elements(view);
    let t = match locator {
        Locator::Ref { reference } => {
            return if els.iter().any(|(_, r, _)| r == reference) {
                Found::Exact(reference.clone())
            } else {
                Found::Missing(
                    format!("{reference} (a recorded ref) is not on the page"),
                    Vec::new(),
                )
            };
        }
        Locator::Element(t) => t,
    };
    let same: Vec<&(String, String, String)> =
        els.iter().filter(|(ro, _, _)| *ro == t.role).collect();
    let exact: Vec<&&(String, String, String)> =
        same.iter().filter(|(_, _, n)| *n == t.name).collect();
    if let Some((_, r, _)) = exact.get(t.index) {
        return Found::Exact(r.clone());
    }
    let line = |(ro, r, n): &(String, String, String)| {
        format!("{ro} {r} {}", pageon_agent::quote::quote(n))
    };
    let mut ranked: Vec<(f64, &(String, String, String))> = same
        .iter()
        .map(|e| (similarity(&e.2, &t.name), *e))
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    let close: Vec<&(f64, &(String, String, String))> =
        ranked.iter().filter(|(s, _)| *s >= RENAMED).collect();
    if exact.is_empty() && close.len() == 1 {
        let (_, (_, r, n)) = close[0];
        return Found::Renamed(r.clone(), n.clone());
    }
    let reason = if exact.is_empty() {
        format!("no {} on the page", Locator::Element(t.clone()))
    } else {
        format!(
            "{} {} appears {} time(s), not {}",
            t.role,
            pageon_agent::quote::quote(&t.name),
            exact.len(),
            t.index + 1
        )
    };
    Found::Missing(
        reason,
        ranked.iter().take(5).map(|(_, e)| line(e)).collect(),
    )
}

fn substitute(text: &str, params: &HashMap<String, String>) -> String {
    let mut out = text.to_string();
    for (k, v) in params {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

/// Runs a workflow through `driver` (in-process, caged or MCP): opens `start`, then each step
/// on the element its locator finds, loading with `options`. `progress` hears one line per
/// step (`✓ 2 type …`).
pub fn run(
    driver: &mut dyn Driver,
    wf: &Workflow,
    params: &HashMap<String, String>,
    options: LoadOptions,
    progress: &mut dyn FnMut(&str),
) -> Result<Report, String> {
    if wf.workflow != VERSION {
        return Err(format!(
            "workflow format {} (this pageon reads {VERSION})",
            wf.workflow
        ));
    }
    let mut values = HashMap::new();
    for p in &wf.params {
        match params.get(&p.name).or(p.default.as_ref()) {
            Some(v) => {
                values.insert(p.name.clone(), v.clone());
            }
            None => return Err(format!("the workflow needs --param {}=VALUE", p.name)),
        }
    }
    if let Some(unknown) = params
        .keys()
        .find(|k| !wf.params.iter().any(|p| &p.name == *k))
    {
        return Err(format!("the workflow has no parameter {unknown:?}"));
    }
    if let Err(e) = driver.open(&wf.start, options) {
        let reason = format!("the start page did not open: {e}");
        progress(&format!("✗ 0 open {}: {e}", wf.start));
        return Ok(Report {
            status: Status::Drifted,
            ran: 0,
            drifts: Vec::new(),
            stop: Some(Stop {
                step: 0,
                reason,
                candidates: Vec::new(),
                view: String::new(),
            }),
        });
    }
    progress(&format!("✓ 0 open {}", wf.start));
    let mut report = Report {
        status: Status::Done,
        ran: 1,
        drifts: Vec::new(),
        stop: None,
    };
    let mut view = driver.view(Dump::Ax)?;
    for (i, step) in wf.steps.iter().enumerate() {
        let n = i + 1;
        let stop = |drifts: &[Drift], reason: String, candidates: Vec<String>, view: &str| Report {
            status: Status::Drifted,
            ran: n,
            drifts: drifts.to_vec(),
            stop: Some(Stop {
                step: n,
                reason,
                candidates,
                view: view.to_string(),
            }),
        };
        let reference = match step.target() {
            None => None,
            Some(loc) => match resolve(&view, loc) {
                Found::Exact(r) => Some(r),
                Found::Renamed(r, to) => {
                    if let Locator::Element(t) = loc {
                        report.drifts.push(Drift {
                            step: n,
                            from: t.name.clone(),
                            to: to.clone(),
                        });
                        progress(&format!(
                            "~ {n} {step}  (now {})",
                            pageon_agent::quote::quote(&to)
                        ));
                    }
                    Some(r)
                }
                Found::Missing(reason, candidates) => {
                    progress(&format!("✗ {n} {step}: {reason}"));
                    return Ok(stop(&report.drifts, reason, candidates, &view));
                }
            },
        };
        let r = reference.clone().unwrap_or_default();
        let action = match step {
            WStep::Click { .. } => Action::Click(r),
            WStep::Hover { .. } => Action::Hover(r),
            WStep::Type { text, .. } => Action::Type(r, substitute(text, &values)),
            WStep::Upload { files, .. } => Action::Upload(r, files.clone()),
            WStep::Select { options, .. } => Action::Select(r, options.clone()),
            WStep::Press { key, .. } => Action::Press(key.clone()),
            WStep::Wait { ms } => Action::Wait(*ms),
            WStep::Goto { url } => Action::Goto(substitute(url, &values)),
            WStep::Back => Action::Back,
            WStep::Forward => Action::Forward,
            WStep::Scroll { by, .. } => Action::Scroll {
                point: None,
                target: reference.clone(),
                by: *by,
            },
            WStep::Tool {
                name,
                origin,
                input,
                ..
            } => Action::Tool {
                name: name.clone(),
                origin: origin.clone(),
                input: input.clone(),
            },
        };
        if let Err(e) = driver.act(action) {
            let reason = match &e {
                ActionError::Navigation(_) => e.to_string(),
                _ => format!("{e} ({})", e.guidance()),
            };
            progress(&format!("✗ {n} {step}: {reason}"));
            let view = driver.view(Dump::Ax).unwrap_or_default();
            return Ok(stop(&report.drifts, reason, Vec::new(), &view));
        }
        view = driver.view(Dump::Ax)?;
        if let Some(want) = step.expect() {
            let now = view_url(&view).map(|u| place(&u)).unwrap_or_default();
            if now != want {
                let reason = format!("expected {want}, at {now}");
                progress(&format!("✗ {n} {step}: {reason}"));
                return Ok(stop(&report.drifts, reason, Vec::new(), &view));
            }
        }
        if !report.drifts.iter().any(|d| d.step == n) {
            progress(&format!("✓ {n} {step}"));
        }
        report.ran = n + 1;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_lines_and_similarity() {
        assert_eq!(
            element(r#"  button e5 "Sign in" focused"#),
            Some(("button".into(), "e5".into(), "Sign in".into()))
        );
        assert_eq!(
            element(r#"textbox e3 ="ada""#),
            Some(("textbox".into(), "e3".into(), String::new()))
        );
        assert_eq!(element(r#"text "hello""#), None);
        assert_eq!(
            element(r#"link e2 "Say \"hi\"""#).map(|e| e.2),
            Some("Say \"hi\"".into())
        );
        assert!(similarity("Sign in", "Sign in now") >= RENAMED);
        assert!(similarity("Report", "Reports") >= RENAMED);
        assert!(similarity("Sign in", "Login") < RENAMED);
        assert_eq!(
            view_url(r#"@page v2 "Shop \"x\"" https://shop.test/a?b=1 load:complete"#).as_deref(),
            Some("https://shop.test/a?b=1")
        );
        assert_eq!(place("https://shop.test/a?b=1#c"), "https://shop.test/a");
        assert_eq!(slug("Email address"), "email_address");
        assert_eq!(slug(""), "text");
    }
}
