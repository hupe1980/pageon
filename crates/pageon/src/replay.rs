//! Re-running recorded steps: replay validation, `--live` divergence checks and
//! reset-to-step all re-run a recording's steps through a [`Driver`] while the session's
//! [`Recording`](pageon_env::Recording) resumes: it serves (or checks) each step and keeps the
//! first [`Divergence`].

use pageon_env::{Divergence, Env, Kind, Step};

use crate::{Action, ActionError, Budget, Dump, LoadOptions, Sandbox, Session, Tab, Viewport};

/// What re-runs steps: a tab in-process, a caged tab, or an MCP backend.
pub trait Driver {
    /// Opens a page (step 0, or an `open` later in the episode).
    fn open(&mut self, url: &str, options: LoadOptions) -> Result<(), String>;
    /// Performs an action.
    fn act(&mut self, action: Action) -> Result<crate::Outcome, ActionError>;
    /// The agent's observation (it assigns refs).
    fn observe(&mut self) -> Result<String, String>;
    /// A view of the page now.
    fn view(&mut self, kind: Dump) -> Result<String, String>;
    /// The first divergence the session's recording found.
    fn divergence(&self) -> Option<Divergence>;
    /// The viewport as a JPEG: bytes, width, height (for traces).
    fn screenshot(&mut self) -> Result<(Vec<u8>, u32, u32), String> {
        Err("this driver does not paint".into())
    }
}

/// What a trace's screencast frames are painted with.
fn frame_options() -> crate::ScreenshotOptions {
    crate::ScreenshotOptions {
        format: crate::Format::Jpeg { quality: 60 },
        ..crate::ScreenshotOptions::default()
    }
}

/// A driver whose pages open under the operator's safety options rather than the ones a
/// recording carries: the private network, robots.txt and local files. `pageon replay --live`
/// wraps its driver in this, so a recording cannot widen what reaches the live web.
pub struct Operator<'a> {
    inner: &'a mut dyn Driver,
    options: LoadOptions,
}

impl<'a> Operator<'a> {
    /// Wraps `inner`; `options` are the operator's.
    pub fn new(inner: &'a mut dyn Driver, options: LoadOptions) -> Self {
        Self { inner, options }
    }
}

impl Driver for Operator<'_> {
    fn open(&mut self, url: &str, recorded: LoadOptions) -> Result<(), String> {
        let options = LoadOptions {
            allow_private_network: self.options.allow_private_network,
            robots: self.options.robots,
            allow_files: self.options.allow_files,
            ..recorded
        };
        self.inner.open(url, options)
    }

    fn act(&mut self, action: Action) -> Result<crate::Outcome, ActionError> {
        self.inner.act(action)
    }

    fn observe(&mut self) -> Result<String, String> {
        self.inner.observe()
    }

    fn view(&mut self, kind: Dump) -> Result<String, String> {
        self.inner.view(kind)
    }

    fn divergence(&self) -> Option<Divergence> {
        self.inner.divergence()
    }

    fn screenshot(&mut self) -> Result<(Vec<u8>, u32, u32), String> {
        self.inner.screenshot()
    }
}

/// What a step does, from its recorded input (or, for hand-written steps, its text).
enum Input {
    Open(String, LoadOptions),
    Act(Action),
}

fn input(step: &Step) -> Result<Input, String> {
    if let Some(open) = step.input.get("open") {
        let url = open
            .get("url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let options = open
            .get("options")
            .and_then(|o| serde_json::from_value(o.clone()).ok())
            .unwrap_or_default();
        return Ok(Input::Open(url, options));
    }
    if !step.input.is_null() {
        return serde_json::from_value(step.input.clone())
            .map(Input::Act)
            .map_err(|e| format!("unreadable step: {e}"));
    }
    match step.action.strip_prefix("open ") {
        Some(url) => Ok(Input::Open(url.to_string(), LoadOptions::default())),
        None => step.action.parse().map(Input::Act),
    }
}

/// Re-runs steps `0..=upto` through `driver` while its session's recording resumes;
/// `progress` hears of each step that matched. Returns the first divergence.
pub fn rerun(
    driver: &mut dyn Driver,
    steps: &[Step],
    upto: usize,
    progress: &mut dyn FnMut(usize, &Step),
) -> Result<(), Divergence> {
    for (i, step) in steps.iter().enumerate().take(upto + 1) {
        let failed = |detail: String| Divergence {
            step: i,
            action: step.action.clone(),
            kind: Kind::Action,
            detail,
            expected: None,
            actual: None,
        };
        for _ in 0..step.observed {
            driver.observe().map_err(failed)?;
        }
        let mut blocked = Vec::new();
        let ran = match input(step).map_err(failed)? {
            Input::Open(url, options) => driver
                .open(&url, options)
                .map_err(|e| format!("the page did not open: {e}")),
            Input::Act(a) => match driver.act(a) {
                Ok(o) => {
                    blocked = o
                        .requests
                        .iter()
                        .filter(|r| matches!(r.outcome, crate::RequestOutcome::Blocked(_)))
                        .map(ToString::to_string)
                        .collect();
                    Ok(())
                }
                Err(ActionError::Navigation(e)) => {
                    blocked.push(e.to_string());
                    Ok(())
                }
                Err(e) => Err(format!("the action failed: {e}")),
            },
        };
        // A missing request explains a failure better than the failure itself; a request
        // refused this time (a held write, a policy) explains a page that differs.
        if let Some(mut d) = driver.divergence() {
            for b in blocked {
                d.detail.push_str(&format!("\n    {b}"));
            }
            return Err(d);
        }
        ran.map_err(failed)?;
        progress(i, step);
    }
    Ok(())
}

/// One line per step for people: `✓ 2 click e5`, or the divergence with its diff.
pub fn report(d: &Divergence) -> String {
    let mut out = format!("✗ {} {}: {}", d.step, d.action, d.detail);
    let diff = d.diff();
    if diff.is_empty() && d.kind == Kind::Page {
        out.push_str("\n    (the agent view is the same; the DOM, a frame or storage differs)");
    }
    for l in diff {
        out.push_str("\n    ");
        out.push_str(&l);
    }
    out
}

/// Runs steps in this process.
pub struct InProcess<'s, E: Env> {
    session: &'s Session<E>,
    tab: Option<Tab<'s, E>>,
}

impl<'s, E: Env> InProcess<'s, E> {
    /// A driver over `session`.
    pub fn new(session: &'s Session<E>) -> Self {
        Self { session, tab: None }
    }

    /// The tab, once a step opened one.
    pub fn tab(&mut self) -> Option<&mut Tab<'s, E>> {
        self.tab.as_mut()
    }
}

impl<E: Env> Driver for InProcess<'_, E> {
    fn open(&mut self, url: &str, options: LoadOptions) -> Result<(), String> {
        self.tab = None;
        self.tab = Some(self.session.open(url, options).map_err(|e| e.to_string())?);
        Ok(())
    }

    fn act(&mut self, action: Action) -> Result<crate::Outcome, ActionError> {
        let t = self
            .tab
            .as_mut()
            .ok_or_else(|| ActionError::Unsupported("no page is open".into()))?;
        t.act(action)
    }

    fn observe(&mut self) -> Result<String, String> {
        let t = self.tab.as_mut().ok_or("no page is open")?;
        Ok(t.observe())
    }

    fn divergence(&self) -> Option<Divergence> {
        self.session.env().recorder()?.divergence()
    }

    fn view(&mut self, kind: Dump) -> Result<String, String> {
        let t = self.tab.as_mut().ok_or("no page is open")?;
        Ok(match kind {
            Dump::Ax => t.observe_full(),
            k => t.view(k, Viewport::default(), Budget::o200k(100_000)),
        })
    }

    fn screenshot(&mut self) -> Result<(Vec<u8>, u32, u32), String> {
        let t = self.tab.as_mut().ok_or("no page is open")?;
        let i = t.screenshot(&frame_options()).map_err(|e| e.to_string())?;
        Ok((i.bytes, i.width, i.height))
    }
}

/// Runs steps in a caged content process.
pub struct Caged<'a, E: Env> {
    sandbox: &'a mut Sandbox<E>,
    tab: Option<u32>,
}

impl<'a, E: Env> Caged<'a, E> {
    /// A driver over `sandbox`.
    pub fn new(sandbox: &'a mut Sandbox<E>) -> Self {
        Self { sandbox, tab: None }
    }
}

impl<E: Env> Driver for Caged<'_, E> {
    fn open(&mut self, url: &str, options: LoadOptions) -> Result<(), String> {
        if let Some(t) = self.tab.take() {
            self.sandbox.close_tab(t);
        }
        let (id, _) = self
            .sandbox
            .open_tab(url, options)
            .map_err(|e| e.to_string())?;
        self.tab = Some(id);
        Ok(())
    }

    fn act(&mut self, action: Action) -> Result<crate::Outcome, ActionError> {
        let t = self
            .tab
            .ok_or_else(|| ActionError::Unsupported("no page is open".into()))?;
        self.sandbox.act_tab(t, action)
    }

    fn divergence(&self) -> Option<Divergence> {
        self.sandbox.env().recorder()?.divergence()
    }

    fn observe(&mut self) -> Result<String, String> {
        let t = self.tab.ok_or("no page is open")?;
        self.sandbox
            .observe_tab(t, false)
            .map_err(|e| e.to_string())
    }

    fn view(&mut self, kind: Dump) -> Result<String, String> {
        let t = self.tab.ok_or("no page is open")?;
        match kind {
            Dump::Ax => self.sandbox.observe_tab(t, true),
            k => self
                .sandbox
                .view_tab(t, k, Viewport::default(), Budget::o200k(100_000)),
        }
        .map_err(|e| e.to_string())
    }

    fn screenshot(&mut self) -> Result<(Vec<u8>, u32, u32), String> {
        let t = self.tab.ok_or("no page is open")?;
        let i = self
            .sandbox
            .screenshot_tab(t, &frame_options())
            .map_err(|e| e.to_string())?;
        Ok((i.bytes, i.width, i.height))
    }
}
