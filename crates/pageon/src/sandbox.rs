//! The broker: pages run in a caged content process. The broker keeps the network, the
//! clock, randomness, secrets and policy (its `Env`), and serves them to the content process
//! over the content process's standard input and output.

use std::fmt;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};

use pageon_env::Env;
use procage::native::{Cage, CageOptions, Child, Grants};
use procage::policy::{AbsPath, Access, Base, Exec, FsRule, FsTarget, Network, Policy};
use serde::de::DeserializeOwned;

use crate::protocol::{self, EnvCall, EnvReply, FromContent, Op, ToContent};
use crate::{
    Action, ActionError, Budget, Dump, LoadError, LoadOptions, Outcome, StorageState, Viewport,
};

/// Where the content process comes from.
#[derive(Debug, Clone, Default)]
pub struct SandboxOptions {
    /// The program to start as `<program> content`; default: this binary. Binaries that are
    /// not `pageon` call [`crate::content::serve`] when started with the argument `content`,
    /// or point here at an installed `pageon`.
    pub program: Option<PathBuf>,
}

/// Why the sandbox could not start or broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxError {
    /// The host cannot cage the content process.
    Cage(String),
    /// The content process could not be started.
    Spawn(String),
    /// The content process broke the protocol, or died.
    Protocol(String),
}

impl SandboxError {
    /// What to do next.
    pub fn guidance(&self) -> String {
        match self {
            Self::Cage(_) => {
                "this host cannot sandbox the page; run without it with --no-sandbox (pages then run with your rights)"
                    .into()
            }
            Self::Spawn(_) => "check that the pageon binary can be executed".into(),
            Self::Protocol(_) => "open the page again (a new content process starts)".into(),
        }
    }
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cage(m) => write!(f, "cannot sandbox the content process: {m}"),
            Self::Spawn(m) => write!(f, "cannot start the content process: {m}"),
            Self::Protocol(m) => write!(f, "the content process failed: {m}"),
        }
    }
}

impl std::error::Error for SandboxError {}

/// A caged session: pages run in a content process with no file, network or program access;
/// everything they need from outside comes from this broker's `Env`.
///
/// ```no_run
/// use pageon::{Live, LoadOptions, Sandbox, SandboxOptions};
///
/// let mut sandbox = Sandbox::new(Live::default(), SandboxOptions::default())?;
/// eprintln!("{}", sandbox.receipt());
/// let mut tab = sandbox.open("https://example.com", LoadOptions::default())?;
/// println!("{}", tab.observe()?);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Sandbox<E: Env> {
    env: E,
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
    receipt: String,
    broken: Option<String>,
    /// While a lookahead runs: what its branch did so far, and the request it was
    /// stopped at.
    branch: Option<(Vec<crate::Outcome>, Option<Pending>)>,
}

impl<E: Env> fmt::Debug for Sandbox<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sandbox")
            .field("receipt", &self.receipt)
            .finish_non_exhaustive()
    }
}

/// The cage of a content process.
fn policy(program: &AbsPath) -> Policy {
    let mut p = Policy::default();
    // macOS needs a base for `Exec::Never`: every program maps the system's code.
    p.base = Base::V1;
    p.net = Network::None;
    p.process.exec = Exec::Never;
    // A lookahead forks the content process; it still cannot run programs.
    p.process.fork = true;
    p.fs.push(FsRule::new(
        Access::Read,
        FsTarget::Literal(program.clone()),
    ));
    p
}

impl<E: Env> Sandbox<E> {
    /// Starts a caged content process serving pages through `env`.
    pub fn new(env: E, options: SandboxOptions) -> Result<Self, SandboxError> {
        let program = match options.program {
            Some(p) => p,
            None => std::env::current_exe().map_err(|e| SandboxError::Spawn(e.to_string()))?,
        };
        // The cage names the binary literally: no `..`, no symlinks (an installed symlink, or a
        // path like `bin/../pageon`).
        let program = std::fs::canonicalize(&program)
            .map_err(|e| SandboxError::Spawn(format!("{}: {e}", program.display())))?;
        let abs = AbsPath::new(program.as_os_str().as_encoded_bytes().to_vec())
            .map_err(|e| SandboxError::Spawn(e.to_string()))?;
        // Only relaxations that make the cage stricter are accepted; any is named in the receipt.
        let relax = procage::policy::lower::Relax::new().no_unix_sockets();
        let (cage, relaxed) = Cage::fit(&policy(&abs), &CageOptions::default(), &relax)
            .map_err(|e| SandboxError::Cage(e.to_string()))?;
        let mut cmd = Command::new(&program);
        cmd.arg("content")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cage
            .spawn(&mut cmd, &Grants::new())
            .map_err(|e| SandboxError::Cage(e.to_string()))?;
        let (Some(input), Some(output)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(SandboxError::Spawn("no standard input or output".into()));
        };
        let backend = if cfg!(target_os = "macos") {
            "Seatbelt"
        } else {
            "Landlock + seccomp"
        };
        let mut sb = Self {
            env,
            child,
            input,
            output: BufReader::new(output),
            next: 1,
            receipt: format!(
                "sandbox: {backend}, base v1; reads only {}; no network, no files, no programs{}",
                program.display(),
                relaxed
                    .iter()
                    .map(|r| format!("; {}", r.relaxation))
                    .collect::<String>()
            ),
            broken: None,
            branch: None,
        };
        match protocol::recv::<FromContent>(&mut sb.output) {
            Ok(FromContent::Hello { pageon }) if pageon == protocol::VERSION => Ok(sb),
            Ok(FromContent::Hello { pageon }) => Err(SandboxError::Protocol(format!(
                "content process speaks protocol {pageon}, expected {}",
                protocol::VERSION
            ))),
            Ok(_) => Err(SandboxError::Protocol("no greeting".into())),
            Err(e) => Err(SandboxError::Protocol(e.to_string())),
        }
    }

    /// How the content process is caged (one line).
    pub fn receipt(&self) -> &str {
        &self.receipt
    }

    /// The environment.
    pub fn env(&self) -> &E {
        &self.env
    }

    fn fail(&mut self, why: String) -> SandboxError {
        let _ = self.child.kill_all();
        self.broken = Some(why.clone());
        SandboxError::Protocol(why)
    }

    /// Runs one command, serving the content process's `Env` calls until it is done.
    fn call<T: DeserializeOwned>(&mut self, op: Op) -> Result<T, SandboxError> {
        if let Some(why) = &self.broken {
            return Err(SandboxError::Protocol(why.clone()));
        }
        let id = self.next;
        self.next += 1;
        if let Err(e) = protocol::send(&mut self.input, &ToContent::Cmd { id, op }) {
            return Err(self.fail(e.to_string()));
        }
        loop {
            let msg = match protocol::recv::<FromContent>(&mut self.output) {
                Ok(m) => m,
                Err(e) => return Err(self.fail(e.to_string())),
            };
            // A lookahead branch may read, never write: a request that may change something
            // is held (never sent, never answered) and the branch ends.
            if let (Some((_, pending)), FromContent::Env(EnvCall::Fetch(req))) =
                (self.branch.as_mut(), &msg)
                && !matches!(req.method.as_str(), "GET" | "HEAD" | "OPTIONS")
            {
                *pending = Some(Pending {
                    method: req.method.clone(),
                    url: req.url.clone(),
                });
                if let Err(e) = protocol::send(&mut self.input, &ToContent::Stop) {
                    return Err(self.fail(e.to_string()));
                }
                continue;
            }
            let reply = match msg {
                FromContent::Ran(o) => {
                    if let Some((outcomes, _)) = self.branch.as_mut() {
                        outcomes.push(*o);
                    }
                    continue;
                }
                FromContent::Env(call) => match call {
                    EnvCall::Fetch(req) => EnvReply::Fetch(self.env.fetch(&req)),
                    EnvCall::Now => EnvReply::Num(self.env.now_ms()),
                    EnvCall::Random => EnvReply::Num(self.env.random_u64()),
                    EnvCall::Secret { name, origin } => {
                        EnvReply::Secret(self.env.secret(&name, &origin))
                    }
                    EnvCall::Records => EnvReply::Flag(self.env.recorder().is_some()),
                    EnvCall::Profile => EnvReply::Profile(self.env.profile()),
                    EnvCall::Step(step) => {
                        if let Some(r) = self.env.recorder() {
                            r.step(*step);
                        }
                        EnvReply::Flag(true)
                    }
                },
                FromContent::Done { id: got, value } if got == id => {
                    return serde_json::from_value(value)
                        .map_err(|e| self.fail(format!("bad result: {e}")));
                }
                FromContent::Failed { id: got, message } if got == id => {
                    return Err(SandboxError::Protocol(message));
                }
                other => return Err(self.fail(format!("unexpected message {other:?}"))),
            };
            if let Err(e) = protocol::send(&mut self.input, &ToContent::Reply(reply)) {
                return Err(self.fail(e.to_string()));
            }
        }
    }

    /// Looks ahead on tab `id`: the content process forks, the copy runs `actions`
    /// and is observed, then ends. The tab is unchanged; nothing is recorded; a request that
    /// may change something is held (never sent) and the lookahead stops there, as `pending`.
    pub fn lookahead_tab(
        &mut self,
        id: u32,
        actions: Vec<Action>,
    ) -> Result<Lookahead, SandboxError> {
        if self.env.recorder().is_some_and(|r| r.replaying()) {
            return Err(SandboxError::Protocol(
                "a lookahead cannot run while the session is being re-run (a reset or replay)"
                    .into(),
            ));
        }
        #[allow(clippy::disallowed_methods)] // the lookahead's own duration, never page-visible
        let start = std::time::Instant::now();
        if let Some(r) = self.env.recorder() {
            r.pause(true);
        }
        self.branch = Some((Vec::new(), None));
        let r: Result<Option<Lookahead>, SandboxError> =
            self.call(Op::Lookahead { tab: id, actions });
        let (ran, pending) = self.branch.take().unwrap_or_default();
        if let Some(rec) = self.env.recorder() {
            rec.pause(false);
        }
        let mut la = r?.unwrap_or(Lookahead {
            outcomes: ran,
            ..Lookahead::default()
        });
        if pending.is_some() {
            la.observation = None;
        }
        la.pending = pending;
        la.ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        Ok(la)
    }

    /// Opens a page in a new tab in the content process.
    pub fn open(&mut self, url: &str, options: LoadOptions) -> Result<RemoteTab<'_, E>, LoadError> {
        let (id, opened) = self.open_tab(url, options)?;
        Ok(RemoteTab {
            sandbox: self,
            id,
            opened,
        })
    }

    /// Opens a page in a new tab; returns the tab's id and what opening did (the id-based API
    /// behind [`RemoteTab`], for servers that keep tabs by id).
    pub fn open_tab(
        &mut self,
        url: &str,
        options: LoadOptions,
    ) -> Result<(u32, Outcome), LoadError> {
        let r: Result<(u32, Outcome), LoadError> = self
            .call(Op::Open {
                url: url.to_string(),
                options,
            })
            .map_err(|e| LoadError::Sandbox(e.to_string()))?;
        r
    }

    /// Handles one CDP client message in the content process; returns the
    /// messages to send back. The CDP session starts with the first message, with `options`.
    pub fn cdp(
        &mut self,
        message: &str,
        options: LoadOptions,
    ) -> Result<Vec<String>, SandboxError> {
        self.call(Op::Cdp {
            message: message.to_string(),
            options,
        })
    }

    /// Tells the CDP session the client has been idle `elapsed_ms` (real time); returns the
    /// messages to send.
    pub fn cdp_idle(&mut self, elapsed_ms: u64) -> Result<Vec<String>, SandboxError> {
        self.call(Op::CdpIdle { elapsed_ms })
    }

    /// One WebDriver BiDi client message; returns the messages to send.
    pub fn bidi(
        &mut self,
        message: &str,
        options: LoadOptions,
    ) -> Result<Vec<String>, SandboxError> {
        self.call(Op::Bidi {
            message: message.to_string(),
            options,
        })
    }

    /// Tells the BiDi session the client has been idle `elapsed_ms` (real time).
    pub fn bidi_idle(&mut self, elapsed_ms: u64) -> Result<Vec<String>, SandboxError> {
        self.call(Op::BidiIdle { elapsed_ms })
    }

    /// The agent view of tab `id` (`full`: complete, refs kept).
    pub fn observe_tab(&mut self, id: u32, full: bool) -> Result<String, SandboxError> {
        self.call(Op::Observe { tab: id, full })
    }

    /// Performs an action in tab `id`.
    pub fn act_tab(&mut self, id: u32, action: Action) -> Result<Outcome, ActionError> {
        self.act_tab_as(id, action, false)
    }

    /// Performs a person's input in tab `id` (see [`crate::Tab::act_as_person`]).
    pub fn act_tab_as_person(&mut self, id: u32, action: Action) -> Result<Outcome, ActionError> {
        self.act_tab_as(id, action, true)
    }

    fn act_tab_as(
        &mut self,
        id: u32,
        action: Action,
        person: bool,
    ) -> Result<Outcome, ActionError> {
        let r: Result<Result<Outcome, ActionError>, SandboxError> = self.call(Op::Act {
            tab: id,
            action,
            person,
        });
        r.map_err(|e| ActionError::Engine(e.to_string()))?
    }

    /// A view of tab `id`.
    pub fn view_tab(
        &mut self,
        id: u32,
        kind: Dump,
        viewport: Viewport,
        budget: Budget,
    ) -> Result<String, SandboxError> {
        self.call(Op::View {
            tab: id,
            kind,
            width: viewport.width,
            height: viewport.height,
            budget,
        })
    }

    /// Paints tab `id` (see [`crate::Tab::screenshot`]).
    pub fn screenshot_tab(
        &mut self,
        id: u32,
        options: &crate::ScreenshotOptions,
    ) -> Result<crate::Image, crate::PaintError> {
        let r: Result<Result<crate::Image, crate::PaintError>, SandboxError> =
            self.call(Op::Screenshot {
                tab: id,
                options: *options,
            });
        r.map_err(|e| crate::PaintError::Engine(e.to_string()))?
    }

    /// Paints one element of tab `id` (see [`crate::Tab::crop`]).
    pub fn crop_tab(
        &mut self,
        id: u32,
        reference: &str,
        margin: f32,
        format: crate::Format,
        scale: f32,
    ) -> Result<crate::Image, crate::PaintError> {
        let r: Result<Result<crate::Image, crate::PaintError>, SandboxError> =
            self.call(Op::Crop {
                tab: id,
                reference: reference.to_string(),
                margin,
                format,
                scale,
            });
        r.map_err(|e| crate::PaintError::Engine(e.to_string()))?
    }

    /// Tab `id`'s status line and HTTP status.
    pub fn status_tab(
        &mut self,
        id: u32,
    ) -> Result<(String, u16, Option<crate::Refusal>), SandboxError> {
        self.call(Op::Status { tab: id })
    }

    /// Closes tab `id`.
    pub fn close_tab(&mut self, id: u32) {
        if self.broken.is_none() {
            let _: Result<serde_json::Value, _> = self.call(Op::Close { tab: id });
        }
    }

    /// The content process's cookies and `localStorage` (Playwright `storageState`).
    pub fn storage_state(&mut self) -> Result<StorageState, SandboxError> {
        self.call(Op::GetState)
    }

    /// Drops every tab and starts a fresh session in the content process.
    pub fn clear(&mut self) -> Result<(), SandboxError> {
        self.call(Op::Clear)
    }

    /// Adds cookies and `localStorage` to the content process's session.
    pub fn set_storage_state(&mut self, state: &StorageState) -> Result<(), SandboxError> {
        let _: serde_json::Value = self.call(Op::SetState {
            state: state.clone(),
        })?;
        Ok(())
    }

    /// The cage self-test, run inside the content process: `js=ok read-home=denied …`.
    pub fn probe(&mut self) -> Result<String, SandboxError> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        self.call(Op::Probe { home })
    }
}

/// A tab in a [`Sandbox`]: the same operations as [`crate::Tab`], across the cage.
pub struct RemoteTab<'s, E: Env> {
    sandbox: &'s mut Sandbox<E>,
    id: u32,
    opened: Outcome,
}

impl<E: Env> fmt::Debug for RemoteTab<'_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteTab")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<E: Env> RemoteTab<'_, E> {
    /// The tab's id in its sandbox.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// What opening the tab did.
    pub fn opened(&self) -> &Outcome {
        &self.opened
    }

    /// The agent view (see [`crate::Tab::observe`]).
    pub fn observe(&mut self) -> Result<String, SandboxError> {
        self.sandbox.observe_tab(self.id, false)
    }

    /// The complete agent view, refs kept (see [`crate::Tab::observe_full`]).
    pub fn observe_full(&mut self) -> Result<String, SandboxError> {
        self.sandbox.observe_tab(self.id, true)
    }

    /// Performs an action (see [`crate::Tab::act`]).
    pub fn act(&mut self, action: Action) -> Result<Outcome, ActionError> {
        self.sandbox.act_tab(self.id, action)
    }

    /// A person's input (see [`crate::Tab::act_as_person`]).
    pub fn act_as_person(&mut self, action: Action) -> Result<Outcome, ActionError> {
        self.sandbox.act_tab_as_person(self.id, action)
    }

    /// Paints the tab (see [`crate::Tab::screenshot`]).
    pub fn screenshot(
        &mut self,
        options: &crate::ScreenshotOptions,
    ) -> Result<crate::Image, crate::PaintError> {
        self.sandbox.screenshot_tab(self.id, options)
    }

    /// Paints one element (see [`crate::Tab::crop`]).
    pub fn crop(
        &mut self,
        reference: &str,
        margin: f32,
        format: crate::Format,
        scale: f32,
    ) -> Result<crate::Image, crate::PaintError> {
        self.sandbox
            .crop_tab(self.id, reference, margin, format, scale)
    }

    /// A view at a viewport and budget (see [`crate::Tab::view`]).
    pub fn view(
        &mut self,
        kind: Dump,
        viewport: Viewport,
        budget: Budget,
    ) -> Result<String, SandboxError> {
        self.sandbox.view_tab(self.id, kind, viewport, budget)
    }

    /// What `actions` would do here, without doing it: see
    /// [`Sandbox::lookahead_tab`].
    pub fn lookahead(&mut self, actions: Vec<Action>) -> Result<Lookahead, SandboxError> {
        self.sandbox.lookahead_tab(self.id, actions)
    }

    /// The page's status line, HTTP status and the site's refusal.
    pub fn status(&mut self) -> Result<(String, u16, Option<crate::Refusal>), SandboxError> {
        self.sandbox.status_tab(self.id)
    }
}

impl<E: Env> Drop for RemoteTab<'_, E> {
    fn drop(&mut self) {
        self.sandbox.close_tab(self.id);
    }
}

/// What a lookahead found: the actions ran on a fork of the content process, which
/// then ended; the tab itself is unchanged, nothing was recorded, and no write was sent (reads
/// were, and the summary counts them).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Lookahead {
    /// One per action that ran, in order.
    pub outcomes: Vec<crate::Outcome>,
    /// The first action that failed (the rest did not run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::ActionError>,
    /// The agent view after the actions: the diff against what the agent last saw. Absent when
    /// the branch was stopped at a held request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<String>,
    /// The request the branch was stopped at: never sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<Pending>,
    /// Wall time (ms).
    pub ms: u64,
}

/// A request a lookahead held back (`POST https://shop.test/orders`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pending {
    /// Method.
    pub method: String,
    /// URL.
    pub url: String,
}

impl fmt::Display for Lookahead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Reads go out as they would for real (a held write never does): say how many.
        let sent = self
            .outcomes
            .iter()
            .flat_map(|o| &o.requests)
            .filter(|r| !r.from_cache && !matches!(r.outcome, pageon_net::Outcome::Blocked(_)))
            .count();
        write!(
            f,
            "lookahead (the page is unchanged; writes were held, {sent} read(s) sent)"
        )?;
        // The observation starts with the actions' outcome lines; without one, list them.
        if self.observation.is_none() {
            for o in &self.outcomes {
                write!(f, "\n{o}")?;
            }
        }
        if let Some(e) = &self.error {
            write!(f, "\nerror: {e}\n  {}", e.guidance())?;
        }
        if let Some(p) = &self.pending {
            write!(
                f,
                "\npending: {} {} (held; do the action for real to send it)",
                p.method, p.url
            )?;
        }
        if let Some(o) = &self.observation {
            write!(f, "\n{o}")?;
        }
        Ok(())
    }
}
