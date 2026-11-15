//! The broker: pages run in a caged content process. The broker keeps the network, the
//! clock, randomness, secrets and policy (its `Env`), and serves them to the content process
//! over the content process's standard input and output.

use std::fmt;
use std::io::BufReader;
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};

use pageon_env::Env;
use procage::native::{Cage, CageOptions, Child, Grants};
use procage::policy::{AbsPath, Access, Base, Exec, FsRule, FsTarget, Network, Policy};
use serde::de::DeserializeOwned;

use crate::protocol::{self, EnvCall, EnvReply, FromContent, Op, ToContent};
use crate::{
    Action, ActionError, Budget, Dump, LoadError, LoadOptions, Outcome, StorageState, Viewport,
};

/// Where the content process comes from.
#[derive(Debug, Clone)]
pub struct SandboxOptions {
    /// The program to start as `<program> content`; default: this binary. Binaries that are
    /// not `pageon` call [`crate::content::serve`] when started with the argument `content`,
    /// or point here at an installed `pageon`.
    pub program: Option<PathBuf>,
    /// How long the content process may stay silent during a command (default 60 s); past
    /// it, the content process is stopped and the sandbox reports it broken.
    pub command_timeout: std::time::Duration,
}

impl Default for SandboxOptions {
    fn default() -> Self {
        Self {
            program: None,
            command_timeout: std::time::Duration::from_secs(60),
        }
    }
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
    /// The content process's messages, read by a thread so a silent process can time out.
    output: std::sync::mpsc::Receiver<Result<FromContent, String>>,
    timeout: std::time::Duration,
    next: u64,
    receipt: String,
    broken: Option<String>,
    /// While a lookahead runs: what its branch did so far, and the request it was
    /// stopped at.
    branch: Option<Branch>,
    /// Each open tab's private-network setting (from the `open` the broker sent).
    tabs: std::collections::HashMap<u32, bool>,
    /// What the command being served allows.
    serving: Serving,
    /// The `HttpOnly` cookies.
    jar: BrokerJar,
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
        // Landlock binds the binary's read rule to its inode at spawn: a rename-replace of the
        // file loses the rule (stricter, never broader). Accepted by name; exact on macOS.
        let lower = procage::policy::lower::LowerOptions::default()
            .allow(procage::policy::lower::Construct::FsLiteral);
        let cage_options = CageOptions::default().with_lower(lower);
        let (cage, relaxed) = Cage::fit(&policy(&abs), &cage_options, &relax)
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
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            loop {
                let msg = protocol::recv::<FromContent>(&mut output).map_err(|e| e.to_string());
                let end = msg.is_err();
                if tx.send(msg).is_err() || end {
                    break;
                }
            }
        });
        let mut sb = Self {
            env,
            child,
            input,
            output: rx,
            timeout: options.command_timeout,
            next: 1,
            receipt: format!(
                "sandbox: {backend}, base v1; reads only {}{}; no network, no files, no programs{}",
                program.display(),
                if cfg!(target_os = "linux") {
                    " (bound to its inode at spawn)"
                } else {
                    ""
                },
                relaxed
                    .iter()
                    .map(|r| format!("; {}", r.relaxation))
                    .collect::<String>()
            ),
            broken: None,
            branch: None,
            tabs: std::collections::HashMap::new(),
            serving: Serving::default(),
            jar: BrokerJar::default(),
        };
        match sb.receive() {
            Ok(FromContent::Hello { pageon }) if pageon == protocol::VERSION => Ok(sb),
            Ok(FromContent::Hello { pageon }) => Err(SandboxError::Protocol(format!(
                "content process speaks protocol {pageon}, expected {}",
                protocol::VERSION
            ))),
            Ok(_) => Err(SandboxError::Protocol("no greeting".into())),
            Err(e) => Err(SandboxError::Protocol(e.to_string())),
        }
    }

    /// Changes how long the content process may stay silent during a command.
    pub fn set_command_timeout(&mut self, timeout: std::time::Duration) {
        self.timeout = timeout;
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

    /// The content process's next message, within the command timeout.
    fn receive(&mut self) -> Result<FromContent, String> {
        match self.output.recv_timeout(self.timeout) {
            Ok(m) => m,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "the content process did not answer within {} s and was stopped; open a new session",
                self.timeout.as_secs_f64()
            )),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("the content process exited".into())
            }
        }
    }

    /// Answers one `Env` call of the content process (see [`answer`]).
    fn answer(&mut self, call: EnvCall) -> EnvReply {
        let branch = self.branch.is_some();
        answer(&self.env, &mut self.serving, &mut self.jar, branch, call)
    }

    /// Runs one command, serving the content process's `Env` calls until it is done.
    fn call<T: DeserializeOwned>(&mut self, op: Op) -> Result<T, SandboxError> {
        if let Some(why) = &self.broken {
            return Err(SandboxError::Protocol(why.clone()));
        }
        let id = self.next;
        self.next += 1;
        self.serving = Serving::of(&op, &self.tabs);
        if let Err(e) = protocol::send(&mut self.input, &ToContent::Cmd { id, op }) {
            return Err(self.fail(e.to_string()));
        }
        loop {
            let msg = match self.receive() {
                Ok(m) => m,
                Err(e) => return Err(self.fail(e)),
            };
            // A lookahead branch may read, never write: a request that may change something
            // is held (never sent, never answered) and the branch ends. Past its deadline, the
            // branch is stopped at its next request the same way.
            if let (Some(b), FromContent::Env(call)) = (self.branch.as_mut(), &msg) {
                #[allow(clippy::disallowed_methods)] // the branch deadline is wall time
                let late = std::time::Instant::now() >= b.until;
                let write = match call {
                    EnvCall::Fetch(req) => {
                        !matches!(req.method.as_str(), "GET" | "HEAD" | "OPTIONS")
                    }
                    // A sent frame is a write.
                    EnvCall::Stream(c) => matches!(**c, pageon_env::StreamCall::Send { .. }),
                    _ => false,
                };
                if late || write {
                    let held = match call {
                        EnvCall::Fetch(req) => Some(req),
                        EnvCall::Stream(c) => match &**c {
                            pageon_env::StreamCall::Send { req, .. } => Some(req),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let (Some(req), false) = (held, late) {
                        b.pending = Some(Pending {
                            method: req.method.clone(),
                            url: req.url.clone(),
                        });
                    }
                    b.timed_out |= late;
                    if let Err(e) = protocol::send(&mut self.input, &ToContent::Stop) {
                        return Err(self.fail(e.to_string()));
                    }
                    continue;
                }
            }
            let reply = match msg {
                FromContent::Ran(o) => {
                    if let Some(b) = self.branch.as_mut() {
                        b.outcomes.push(*o);
                    }
                    continue;
                }
                FromContent::Env(call) => self.answer(call),
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
        self.branch = Some(Branch {
            outcomes: Vec::new(),
            pending: None,
            until: start + BRANCH_DEADLINE,
            timed_out: false,
        });
        let r: Result<Option<Lookahead>, SandboxError> =
            self.call(Op::Lookahead { tab: id, actions });
        let (ran, pending, timed_out) = self
            .branch
            .take()
            .map(|b| (b.outcomes, b.pending, b.timed_out))
            .unwrap_or_default();
        if let Some(rec) = self.env.recorder() {
            rec.pause(false);
        }
        let mut la = r?.unwrap_or(Lookahead {
            outcomes: ran,
            ..Lookahead::default()
        });
        if pending.is_some() || timed_out {
            la.observation = None;
        }
        la.pending = pending;
        la.timed_out = timed_out;
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
        let allow = options.allow_private_network;
        let r: Result<(u32, Outcome), LoadError> = self
            .call(Op::Open {
                url: url.to_string(),
                options,
            })
            .map_err(|e| LoadError::Sandbox(e.to_string()))?;
        if let Ok((id, _)) = &r {
            self.tabs.insert(*id, allow);
        }
        r
    }

    /// Handles one CDP client message in the content process; returns the
    /// messages to send back. The CDP session starts with the first message, with `options`.
    pub fn cdp(
        &mut self,
        message: &str,
        options: LoadOptions,
    ) -> Result<Vec<String>, SandboxError> {
        let parsed: serde_json::Value = serde_json::from_str(message).unwrap_or_default();
        let method = parsed["method"].as_str().unwrap_or_default().to_string();
        let mut message = message.to_string();
        // HttpOnly cookies are the broker's: writes are kept here, reads merged (P-04).
        match method.as_str() {
            "Network.setCookie" if parsed["params"]["httpOnly"] == true => {
                if let Ok(c) =
                    crate::cdp::page::cdp_to_cookie(&parsed["params"], None, self.jar.now)
                {
                    self.jar.jar.add(&[c]);
                }
                let mut reply =
                    serde_json::json!({"id": parsed["id"], "result": {"success": true}});
                if let Some(sid) = parsed.get("sessionId") {
                    reply["sessionId"] = sid.clone();
                }
                return Ok(vec![reply.to_string()]);
            }
            "Network.setCookies" | "Storage.setCookies" => {
                let mut p = parsed.clone();
                if let Some(list) = p["params"]["cookies"].as_array_mut() {
                    let now = self.jar.now;
                    let (ours, theirs): (Vec<_>, Vec<_>) =
                        list.drain(..).partition(|c| c["httpOnly"] == true);
                    let kept: Vec<_> = ours
                        .iter()
                        .filter_map(|c| crate::cdp::page::cdp_to_cookie(c, None, now).ok())
                        .collect();
                    self.jar.jar.add(&kept);
                    *list = theirs;
                }
                message = p.to_string();
            }
            "Network.deleteCookies" => {
                let name = parsed["params"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let domain = parsed["params"]["domain"]
                    .as_str()
                    .map(|d| d.trim_start_matches('.').to_ascii_lowercase())
                    .or_else(|| {
                        parsed["params"]["url"]
                            .as_str()
                            .and_then(|u| url::Url::parse(u).ok())
                            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
                    });
                self.jar.jar.retain(|c| {
                    !(c.name == name
                        && domain
                            .as_deref()
                            .is_none_or(|d| pageon_net::cookie::domain_match(d, &c.domain)))
                });
            }
            "Network.clearBrowserCookies" | "Storage.clearCookies" => self.jar.jar.clear(),
            _ => {}
        }
        let mut out: Vec<String> = self.call(Op::Cdp { message, options })?;
        if matches!(
            method.as_str(),
            "Network.getCookies" | "Network.getAllCookies" | "Storage.getCookies"
        ) {
            let urls: Option<Vec<url::Url>> = parsed["params"]["urls"].as_array().map(|a| {
                a.iter()
                    .filter_map(|u| u.as_str().and_then(|u| url::Url::parse(u).ok()))
                    .collect()
            });
            let ours: Vec<serde_json::Value> = self
                .jar
                .jar
                .all(self.jar.now)
                .iter()
                .filter(|c| {
                    urls.as_ref()
                        .is_none_or(|us| us.iter().any(|u| crate::cdp::page::applies(c, u)))
                })
                .map(crate::cdp::page::cookie_to_cdp)
                .collect();
            merge_reply(&mut out, &parsed["id"], &["result", "cookies"], ours);
        }
        Ok(out)
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
        let parsed: serde_json::Value = serde_json::from_str(message).unwrap_or_default();
        let method = parsed["method"].as_str().unwrap_or_default().to_string();
        let filter = parsed["params"]["filter"].clone();
        // The broker's HttpOnly cookies, as BiDi cookies matching the request's filter.
        let ours = |jar: &BrokerJar| -> Vec<(pageon_net::cookie::Cookie, serde_json::Value)> {
            jar.jar
                .all(jar.now)
                .into_iter()
                .map(|c| {
                    let b = crate::bidi::context::cdp_to_bidi(&crate::cdp::page::cookie_to_cdp(&c));
                    (c, b)
                })
                .filter(|(_, b)| {
                    filter.is_null() || crate::bidi::context::cookie_matches(b, &filter)
                })
                .collect()
        };
        if method == "storage.deleteCookies" {
            let gone: Vec<pageon_net::cookie::Cookie> =
                ours(&self.jar).into_iter().map(|(c, _)| c).collect();
            self.jar.jar.retain(|c| !gone.contains(c));
        }
        let mut out: Vec<String> = self.call(Op::Bidi {
            message: message.to_string(),
            options,
        })?;
        if method == "storage.getCookies" {
            let list = ours(&self.jar).into_iter().map(|(_, b)| b).collect();
            merge_reply(&mut out, &parsed["id"], &["result", "cookies"], list);
        }
        Ok(out)
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

    /// A read of tab `id` with the agent's refs (see [`crate::Tab::read`]).
    pub fn read_tab(&mut self, id: u32, read: crate::Read) -> Result<String, ActionError> {
        let r: Result<Result<String, ActionError>, SandboxError> =
            self.call(Op::Read { tab: id, read });
        r.map_err(|e| ActionError::Engine(e.to_string()))?
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

    /// Tab `id`'s URL and title (secrets redacted).
    pub fn page_tab(&mut self, id: u32) -> Result<(String, String), SandboxError> {
        self.call(Op::Url { tab: id })
    }

    /// Closes tab `id`.
    pub fn close_tab(&mut self, id: u32) {
        self.tabs.remove(&id);
        if self.broken.is_none() {
            let _: Result<serde_json::Value, _> = self.call(Op::Close { tab: id });
        }
    }

    /// The content process's cookies and `localStorage` (Playwright `storageState`).
    pub fn storage_state(&mut self) -> Result<StorageState, SandboxError> {
        let mut state: StorageState = self.call(Op::GetState)?;
        // The HttpOnly cookies are the broker's (the content process never had them).
        state.cookies.retain(|c| !c.http_only);
        state.cookies.extend(self.jar.jar.all(self.jar.now));
        Ok(state)
    }

    /// Drops every tab and starts a fresh session in the content process.
    pub fn clear(&mut self) -> Result<(), SandboxError> {
        for id in std::mem::take(&mut self.jar.streams) {
            let _ = self.env.stream(&pageon_env::StreamCall::Close {
                id,
                code: 1001,
                reason: String::new(),
            });
        }
        self.jar.jar.clear();
        self.tabs.clear();
        self.call(Op::Clear)
    }

    /// Adds cookies and `localStorage` to the content process's session.
    pub fn set_storage_state(&mut self, state: &StorageState) -> Result<(), SandboxError> {
        // HttpOnly cookies stay in the broker; the rest goes to the content process.
        let (ours, theirs): (Vec<_>, Vec<_>) =
            state.cookies.iter().cloned().partition(|c| c.http_only);
        self.jar.jar.add(&ours);
        let mut state = state.clone();
        state.cookies = theirs;
        let _: serde_json::Value = self.call(Op::SetState { state })?;
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
    /// The branch ran past its deadline (10 s) and was stopped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
    /// Wall time (ms).
    pub ms: u64,
}

/// What the broker allows while it serves one command: decided from the command it sent,
/// never from what the content process says (P-04).
#[derive(Debug, Default)]
struct Serving {
    /// Requests may reach the private network.
    allow_private: bool,
    /// Secret handles the command types, each released once.
    secrets: Vec<String>,
    /// Staged files the command uploads, each released once.
    files: Vec<String>,
    /// The step this command may record (action text, input), once.
    step: Option<(String, serde_json::Value)>,
    /// The command is the agent's input: passkeys may be used (never on a load, a person's
    /// input or a lookahead).
    passkeys: bool,
}

/// The staged files an upload action names (download handles are the tab's own).
fn uploaded(action: &Action) -> Vec<String> {
    match action {
        Action::Upload(_, names) => names
            .iter()
            .filter(|n| !n.starts_with("download:"))
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

/// The `secret://` handle a type action types, if any.
fn typed_secret(action: &Action) -> Option<String> {
    match action {
        Action::Type(_, text) | Action::TypeFocused(text) => {
            text.strip_prefix("secret://").map(str::to_string)
        }
        _ => None,
    }
}

impl Serving {
    /// What `op` allows, given each open tab's private-network setting.
    fn of(op: &Op, tabs: &std::collections::HashMap<u32, bool>) -> Self {
        let tab = |t: &u32| tabs.get(t).copied().unwrap_or(false);
        match op {
            Op::Open { url, options } => Self {
                allow_private: options.allow_private_network,
                step: Some((
                    format!("open {url}"),
                    serde_json::json!({"open": {"url": url, "options": options}}),
                )),
                ..Self::default()
            },
            Op::Act {
                tab: t,
                action,
                person,
            } => Self {
                allow_private: tab(t),
                secrets: typed_secret(action).into_iter().collect(),
                files: uploaded(action),
                passkeys: !person,
                step: (!person).then(|| {
                    (
                        action.to_string(),
                        serde_json::to_value(action).unwrap_or_default(),
                    )
                }),
            },
            Op::Lookahead { tab: t, actions } => Self {
                allow_private: tab(t),
                secrets: actions.iter().filter_map(typed_secret).collect(),
                files: actions.iter().flat_map(uploaded).collect(),
                step: None,
                passkeys: false,
            },
            Op::Cdp { message, options } | Op::Bidi { message, options } => Self {
                allow_private: options.allow_private_network,
                passkeys: true,
                // `DOM.setFileInputFiles` / `input.setFiles`: staged paths or names.
                files: serde_json::from_str::<serde_json::Value>(message)
                    .ok()
                    .filter(|m| {
                        m["method"] == "DOM.setFileInputFiles" || m["method"] == "input.setFiles"
                    })
                    .and_then(|m| m["params"]["files"].as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|f| f.as_str().map(str::to_string))
                    .collect(),
                // pageon's own action method (`Pageon.act`) may type a handle.
                secrets: serde_json::from_str::<serde_json::Value>(message)
                    .ok()
                    .filter(|m| m["method"] == "Pageon.act")
                    .and_then(|m| m["params"]["action"].as_str().map(str::to_string))
                    .and_then(|a| a.parse::<Action>().ok())
                    .and_then(|a| typed_secret(&a))
                    .into_iter()
                    .collect(),
                step: None,
            },
            Op::Observe { tab: t, .. }
            | Op::View { tab: t, .. }
            | Op::Read { tab: t, .. }
            | Op::Status { tab: t }
            | Op::Screenshot { tab: t, .. }
            | Op::Crop { tab: t, .. }
            | Op::Url { tab: t }
            | Op::Close { tab: t } => Self {
                allow_private: tab(t),
                ..Self::default()
            },
            _ => Self::default(),
        }
    }
}

/// Answers one `Env` call of the content process, deciding from the command being served
/// what the content process may have (P-04): never its word for the private network,
/// secrets, steps or `secret:` labels.
fn answer<E: Env>(
    env: &E,
    serving: &mut Serving,
    jar: &mut BrokerJar,
    branch: bool,
    call: EnvCall,
) -> EnvReply {
    match call {
        EnvCall::Fetch(mut req) => {
            req.allow_private = serving.allow_private;
            req.labels.retain(|l| !l.starts_with("secret:"));
            // A branch's reads do not make origins known: the real first request to an
            // origin keeps its `first-contact` label (and the guard's look).
            if branch {
                req.labels.push("lookahead".into());
            }
            let url = url::Url::parse(&req.url).ok();
            let scope = req.cookie_scope.clone();
            if let (Some(u), Some(sc)) = (&url, &scope) {
                jar.attach(&mut req, u, sc);
            }
            let mut r = env.fetch(&req);
            if let (Ok(resp), Some(u)) = (&mut r, &url) {
                jar.take(resp, u, scope.as_ref());
            }
            EnvReply::Fetch(r)
        }
        EnvCall::Now => {
            let now = env.now_ms();
            if let Ok(t) = &now {
                jar.now = *t;
            }
            EnvReply::Num(now)
        }
        EnvCall::Random => EnvReply::Num(env.random_u64()),
        EnvCall::Secret { name, origin } => match serving.secrets.iter().position(|n| *n == name) {
            Some(i) => {
                serving.secrets.remove(i);
                EnvReply::Secret(env.secret(&name, &origin))
            }
            None => EnvReply::Secret(Err(pageon_env::NetError::Blocked(format!(
                "secret://{name} was not asked for by the agent (only a type action may use it)"
            )))),
        },
        EnvCall::File { name, origin } => match serving.files.iter().position(|n| *n == name) {
            Some(i) => {
                serving.files.remove(i);
                EnvReply::File(env.file(&name, &origin))
            }
            None => EnvReply::File(Err(pageon_env::NetError::Blocked(format!(
                "file \"{name}\" was not asked for by the agent (only an upload may use it)"
            )))),
        },
        EnvCall::Passkey(call) => EnvReply::Passkey(match &*call {
            pageon_env::PasskeyCall::Available => env.passkey(&call),
            _ if serving.passkeys => env.passkey(&call),
            _ => Ok(pageon_env::PasskeyReply::Refused {
                name: "NotAllowedError".into(),
                message: "passkeys are used only during the agent's actions".into(),
            }),
        }),
        EnvCall::Stream(call) => EnvReply::Stream(stream(env, serving, jar, branch, *call)),
        EnvCall::Records => EnvReply::Flag(env.recorder().is_some()),
        EnvCall::Profile => EnvReply::Profile(env.profile()),
        EnvCall::Step(mut step) => {
            // One step per agent command, described by what the broker sent; the content
            // process supplies only what it observed.
            if let Some((action, input)) = serving.step.take()
                && let Some(r) = env.recorder()
            {
                step.action = action;
                step.input = input;
                r.step(*step);
            }
            EnvReply::Flag(true)
        }
    }
}

/// A realtime connection call of the content process. A lookahead branch opens nothing,
/// receives nothing and closes nothing of the real page (its sends never get here: they are
/// held); otherwise as for a fetch: the broker's `allow_private`, its `HttpOnly` cookies on the
/// handshake, no labels from the content process.
fn stream<E: Env>(
    env: &E,
    serving: &Serving,
    jar: &mut BrokerJar,
    branch: bool,
    call: pageon_env::StreamCall,
) -> Result<pageon_env::StreamReply, pageon_env::NetError> {
    use pageon_env::{StreamCall, StreamReply};
    match call {
        StreamCall::Open { mut req, kind } => {
            if branch {
                return Err(pageon_env::NetError::Blocked(
                    "a lookahead does not open realtime connections".into(),
                ));
            }
            req.allow_private = serving.allow_private;
            req.labels.retain(|l| !l.starts_with("secret:"));
            // Cookies go along as for the http(s) URL of the handshake.
            let http = req
                .url
                .replacen("wss://", "https://", 1)
                .replacen("ws://", "http://", 1);
            if let (Ok(u), Some(sc)) = (url::Url::parse(&http), req.cookie_scope.clone()) {
                jar.attach(&mut req, &u, &sc);
            }
            let r = env.stream(&StreamCall::Open { req, kind });
            if let Ok(StreamReply::Opened { id, .. }) = &r {
                jar.streams.push(*id);
            }
            r
        }
        StreamCall::Send { id, mut req, text } => {
            req.allow_private = serving.allow_private;
            req.labels.retain(|l| !l.starts_with("secret:"));
            env.stream(&StreamCall::Send { id, req, text })
        }
        StreamCall::Close { .. } if branch => Ok(StreamReply::Done),
        StreamCall::Poll { .. } if branch => Ok(StreamReply::Events(Vec::new())),
        StreamCall::Close { id, code, reason } => {
            jar.streams.retain(|s| *s != id);
            env.stream(&StreamCall::Close { id, code, reason })
        }
        StreamCall::Poll { now, wait_ms } => env.stream(&StreamCall::Poll { now, wait_ms }),
    }
}

/// The `HttpOnly` cookies, kept by the broker only: the content process never sees them
/// (P-04). Its clock is the last time the broker gave the content process (deterministic
/// in replay).
#[derive(Debug, Default)]
pub(crate) struct BrokerJar {
    jar: pageon_net::cookie::Jar,
    now: u64,
    /// The realtime connections the broker holds for the session (closed on a clear).
    streams: Vec<u64>,
}

/// Whether a `Set-Cookie` line has the `HttpOnly` attribute.
fn http_only(line: &str) -> bool {
    line.split(';')
        .skip(1)
        .any(|a| a.trim().eq_ignore_ascii_case("httponly"))
}

impl BrokerJar {
    fn cx<'a>(
        url: &'a url::Url,
        scope: &'a pageon_env::CookieScope,
    ) -> pageon_net::cookie::Context<'a> {
        pageon_net::cookie::Context {
            url,
            top_site: &scope.top_site,
            same_site: scope.same_site,
            top_level_safe_nav: scope.top_level_safe_nav,
            script: false,
        }
    }

    /// Adds the broker's cookies for `url` to a request that carries cookies.
    fn attach(
        &self,
        req: &mut pageon_env::Request,
        url: &url::Url,
        scope: &pageon_env::CookieScope,
    ) {
        let ours = self.jar.header(&Self::cx(url, scope), self.now);
        if ours.is_empty() {
            return;
        }
        match req
            .headers
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        {
            Some((_, v)) if !v.is_empty() => {
                v.push_str("; ");
                v.push_str(&ours);
            }
            Some((_, v)) => *v = ours,
            None => req.headers.push(("cookie".into(), ours)),
        }
    }

    /// Takes the `HttpOnly` cookies out of a response (stored when the request carried
    /// cookies); a plain cookie of a name the broker holds replaces the broker's copy.
    fn take(
        &mut self,
        resp: &mut pageon_env::Response,
        url: &url::Url,
        scope: Option<&pageon_env::CookieScope>,
    ) {
        let now = self.now;
        resp.headers.retain(|(k, v)| {
            if !k.eq_ignore_ascii_case("set-cookie") {
                return true;
            }
            if http_only(v) {
                if let Some(sc) = scope {
                    let _ = self.jar.set(v, &Self::cx(url, sc), now);
                }
                return false;
            }
            let name = v
                .split(['=', ';'])
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
            self.jar.retain(|c| {
                !(c.name == name && pageon_net::cookie::domain_match(&host, &c.domain))
            });
            true
        });
    }
}

/// Appends `items` to the array at `path` of the reply with id `id` among `out`.
fn merge_reply(
    out: &mut [String],
    id: &serde_json::Value,
    path: &[&str],
    items: Vec<serde_json::Value>,
) {
    if items.is_empty() {
        return;
    }
    for m in out.iter_mut() {
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(m) else {
            continue;
        };
        if v["id"] != *id {
            continue;
        }
        let mut at = &mut v;
        for k in path {
            at = &mut at[*k];
        }
        if let Some(a) = at.as_array_mut() {
            a.extend(items);
            *m = v.to_string();
        }
        return;
    }
}

/// A lookahead branch in progress.
struct Branch {
    outcomes: Vec<crate::Outcome>,
    pending: Option<Pending>,
    /// When it must end (wall time): past it, the branch is stopped at its next request.
    until: std::time::Instant,
    timed_out: bool,
}

/// How long a lookahead branch may run (wall time).
const BRANCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

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
        if self.timed_out {
            f.write_str("\ntimed out: the lookahead ran past 10 s and was stopped")?;
        }
        if let Some(o) = &self.observation {
            write!(f, "\n{o}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// An environment that keeps every request it is asked for.
    #[derive(Default)]
    struct Seen(RefCell<Vec<pageon_env::Request>>);

    impl Env for Seen {
        fn fetch(
            &self,
            req: &pageon_env::Request,
        ) -> Result<pageon_env::Response, pageon_env::NetError> {
            self.0.borrow_mut().push(req.clone());
            Err(pageon_env::NetError::ReplayMiss(String::new()))
        }
        fn now_ms(&self) -> Result<u64, pageon_env::NetError> {
            Ok(0)
        }
        fn random_u64(&self) -> Result<u64, pageon_env::NetError> {
            Ok(0)
        }
    }

    fn act(tab: u32, a: &str, person: bool) -> Op {
        Op::Act {
            tab,
            action: a.parse().unwrap_or_else(|e| panic!("{e}")),
            person,
        }
    }

    fn private_req() -> EnvCall {
        let mut r = pageon_env::Request::get("http://127.0.0.1:9/admin");
        r.allow_private = true;
        r.labels = vec!["secret:pw".into(), "tool:x".into()];
        EnvCall::Fetch(r)
    }

    /// P-04: the private network is the broker's decision, per tab.
    #[test]
    fn allow_private_comes_from_the_served_command() {
        let env = Seen::default();
        let mut tabs = std::collections::HashMap::new();
        tabs.insert(1, false);
        tabs.insert(2, true);
        for (op, want) in [
            (act(1, "click e1", false), false),
            (act(2, "click e1", false), true),
            (
                Op::Observe {
                    tab: 9,
                    full: false,
                },
                false,
            ),
            (Op::Clear, false),
            (
                Op::Open {
                    url: "http://127.0.0.1/".into(),
                    options: LoadOptions {
                        allow_private_network: true,
                        ..LoadOptions::default()
                    },
                },
                true,
            ),
        ] {
            let mut serving = Serving::of(&op, &tabs);
            let _ = answer(
                &env,
                &mut serving,
                &mut BrokerJar::default(),
                false,
                private_req(),
            );
            let seen = env
                .0
                .borrow_mut()
                .pop()
                .unwrap_or_else(|| panic!("no request"));
            assert_eq!(seen.allow_private, want, "{op:?}");
            assert_eq!(
                seen.labels,
                vec!["tool:x".to_string()],
                "secret labels dropped"
            );
        }
    }

    /// P-04: a secret only for the handle a served type action types, once.
    #[test]
    fn secrets_only_for_the_typed_handle_once() {
        let env = crate::Secrets::new(Seen::default())
            .with(
                "pw",
                crate::Secret::value("hunter2").origins(["https://a.test"]),
            )
            .with(
                "other",
                crate::Secret::value("x").origins(["https://a.test"]),
            );
        let tabs = std::collections::HashMap::new();
        let ask = |name: &str| EnvCall::Secret {
            name: name.into(),
            origin: "https://a.test".into(),
        };
        let got = |r: EnvReply| match r {
            EnvReply::Secret(r) => r.map_err(|e| e.to_string()),
            other => panic!("{other:?}"),
        };
        for op in [
            Op::Observe {
                tab: 1,
                full: false,
            },
            act(1, "click e1", false),
            act(1, "type e3 \"ada\"", false),
        ] {
            let mut serving = Serving::of(&op, &tabs);
            let e = got(answer(
                &env,
                &mut serving,
                &mut BrokerJar::default(),
                false,
                ask("pw"),
            ))
            .unwrap_err();
            assert!(e.contains("not asked for by the agent"), "{op:?}: {e}");
        }
        let mut serving = Serving::of(&act(1, "type e3 secret://pw", false), &tabs);
        assert!(
            got(answer(
                &env,
                &mut serving,
                &mut BrokerJar::default(),
                false,
                ask("other")
            ))
            .is_err(),
            "another name"
        );
        assert_eq!(
            got(answer(
                &env,
                &mut serving,
                &mut BrokerJar::default(),
                false,
                ask("pw")
            ))
            .as_deref(),
            Ok("hunter2")
        );
        assert!(
            got(answer(
                &env,
                &mut serving,
                &mut BrokerJar::default(),
                false,
                ask("pw")
            ))
            .is_err(),
            "once per action"
        );
        let cdp = Op::Cdp {
            message: r#"{"id":1,"method":"Pageon.act","params":{"action":"type e3 secret://pw"}}"#
                .into(),
            options: LoadOptions::default(),
        };
        let mut serving = Serving::of(&cdp, &tabs);
        assert!(
            got(answer(
                &env,
                &mut serving,
                &mut BrokerJar::default(),
                false,
                ask("pw")
            ))
            .is_ok()
        );
    }

    /// P-04: one step per recordable command, described by the broker.
    #[test]
    fn steps_come_from_the_served_command() {
        let env = pageon_env::Recording::new(Seen::default());
        let tabs = std::collections::HashMap::new();
        let forged = || {
            EnvCall::Step(Box::new(pageon_env::Step {
                action: "click e666 (forged)".into(),
                hash: "0123456789abcdef".into(),
                ..pageon_env::Step::default()
            }))
        };
        for op in [
            Op::Observe {
                tab: 1,
                full: false,
            },
            act(1, "press Enter", true),
        ] {
            let mut serving = Serving::of(&op, &tabs);
            let _ = answer(
                &env,
                &mut serving,
                &mut BrokerJar::default(),
                false,
                forged(),
            );
        }
        let mut serving = Serving::of(&act(1, "click e5", false), &tabs);
        let _ = answer(
            &env,
            &mut serving,
            &mut BrokerJar::default(),
            false,
            forged(),
        );
        let _ = answer(
            &env,
            &mut serving,
            &mut BrokerJar::default(),
            false,
            forged(),
        );
        let steps = env.log().steps;
        assert_eq!(steps.len(), 1, "{steps:#?}");
        assert_eq!(steps[0].action, "click e5");
        assert_eq!(steps[0].input, serde_json::json!({"Click": "e5"}));
        assert_eq!(
            steps[0].hash, "0123456789abcdef",
            "what it observed is kept"
        );
    }

    /// An environment answering every request with cookies, keeping the requests.
    #[derive(Default)]
    struct Sets(RefCell<Vec<pageon_env::Request>>);

    impl Env for Sets {
        fn fetch(
            &self,
            req: &pageon_env::Request,
        ) -> Result<pageon_env::Response, pageon_env::NetError> {
            self.0.borrow_mut().push(req.clone());
            Ok(pageon_env::Response {
                status: 200,
                headers: vec![
                    (
                        "set-cookie".into(),
                        "sid=s3cr3t; Path=/; HttpOnly; Secure".into(),
                    ),
                    ("set-cookie".into(), "theme=dark; Path=/".into()),
                ],
                body: Vec::new(),
            })
        }
        fn now_ms(&self) -> Result<u64, pageon_env::NetError> {
            Ok(1_000)
        }
        fn random_u64(&self) -> Result<u64, pageon_env::NetError> {
            Ok(0)
        }
    }

    /// P-04: an HttpOnly cookie never reaches the content process, and goes along
    /// with later requests that carry cookies.
    #[test]
    fn http_only_cookies_stay_in_the_broker() {
        let env = Sets::default();
        let mut jar = BrokerJar::default();
        let mut serving = Serving::default();
        let scope = pageon_env::CookieScope {
            top_site: "https://a.test".into(),
            same_site: true,
            top_level_safe_nav: true,
        };
        let req = |cookie: Option<&str>, scope: Option<pageon_env::CookieScope>| {
            let mut r = pageon_env::Request::get("https://a.test/account");
            if let Some(c) = cookie {
                r.headers.push(("cookie".into(), c.into()));
            }
            r.cookie_scope = scope;
            EnvCall::Fetch(r)
        };
        let _ = answer(&env, &mut serving, &mut jar, false, EnvCall::Now);
        let reply = answer(
            &env,
            &mut serving,
            &mut jar,
            false,
            req(None, Some(scope.clone())),
        );
        let EnvReply::Fetch(Ok(resp)) = reply else {
            panic!("{reply:?}")
        };
        let text = format!("{:?}", resp.headers);
        assert!(
            !text.contains("s3cr3t") && text.contains("theme=dark"),
            "{text}"
        );
        // The next request with cookies carries the broker's sid after the page's own.
        let _ = answer(
            &env,
            &mut serving,
            &mut jar,
            false,
            req(Some("theme=dark"), Some(scope)),
        );
        let sent = env
            .0
            .borrow()
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("no request"));
        let cookie = sent
            .headers
            .iter()
            .find(|(k, _)| k == "cookie")
            .map(|(_, v)| v.clone());
        assert_eq!(cookie.as_deref(), Some("theme=dark; sid=s3cr3t"));
        // A request without cookies (credentials omitted) gets none.
        let _ = answer(&env, &mut serving, &mut jar, false, req(None, None));
        let sent = env
            .0
            .borrow()
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("no request"));
        assert!(
            !sent.headers.iter().any(|(k, _)| k == "cookie"),
            "{:?}",
            sent.headers
        );
    }

    /// A staged file only for the upload action naming it, once.
    #[test]
    fn files_only_for_the_upload_once() {
        let path = std::env::temp_dir().join(format!("pageon-sb-file-{}.txt", std::process::id()));
        std::fs::write(&path, b"cv").unwrap_or_else(|e| panic!("{e}"));
        let env = crate::Files::new(Seen::default()).with("cv", &path, ["https://a.test"]);
        let tabs = std::collections::HashMap::new();
        let ask = || EnvCall::File {
            name: "cv".into(),
            origin: "https://a.test".into(),
        };
        let ok = |r: EnvReply| matches!(r, EnvReply::File(Ok(_)));
        let mut jar = BrokerJar::default();
        for op in [act(1, "click e1", false), act(1, "type e3 cv", false)] {
            let mut serving = Serving::of(&op, &tabs);
            assert!(
                !ok(answer(&env, &mut serving, &mut jar, false, ask())),
                "{op:?}"
            );
        }
        let mut serving = Serving::of(&act(1, "upload e7 cv", false), &tabs);
        assert!(ok(answer(&env, &mut serving, &mut jar, false, ask())));
        assert!(
            !ok(answer(&env, &mut serving, &mut jar, false, ask())),
            "once per action"
        );
        let cdp = Op::Cdp {
            message: format!(
                r#"{{"id":1,"method":"DOM.setFileInputFiles","params":{{"files":[{:?}],"nodeId":3}}}}"#,
                path.to_string_lossy()
            ),
            options: LoadOptions::default(),
        };
        let mut serving = Serving::of(&cdp, &tabs);
        let by_path = EnvCall::File {
            name: path.to_string_lossy().into_owned(),
            origin: "https://a.test".into(),
        };
        assert!(
            ok(answer(&env, &mut serving, &mut jar, false, by_path)),
            "a staged path"
        );
        let _ = std::fs::remove_file(&path);
    }
}
