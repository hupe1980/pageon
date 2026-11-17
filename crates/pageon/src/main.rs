//! The `pageon` command line.

#[cfg(unix)]
mod bench;
mod cli;

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use pageon::{
    Action, Budget, Dump, Effect, Env, Identity, LoadError, LoadOptions, Mode, Recording, Replay,
    Sandbox, SandboxOptions, Session, StorageState,
};

/// A browser engine for autonomous agents.
#[derive(Parser)]
#[command(name = "pageon", version, about, propagate_version = true)]
struct Cli {
    /// The named session of open, click, view and the other session commands.
    #[arg(
        short,
        long = "session",
        global = true,
        value_name = "NAME",
        env = "PAGEON_SESSION",
        default_value = "default"
    )]
    session_name: String,
    #[command(subcommand)]
    command: Command,
}

// Parsed once per run: the size difference between the subcommands does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    #[cfg(unix)]
    #[command(flatten)]
    Session(cli::SessionCommand),
    /// Load a page and print it.
    ///
    /// Examples:
    ///   pageon fetch https://example.com
    ///   pageon fetch https://example.com --dump tree
    ///   pageon fetch https://example.com --dump ax --budget 2000
    ///   pageon fetch https://example.com --dump markdown
    ///   pageon fetch https://example.com --dump errors
    ///   pageon fetch https://example.com --no-js
    ///   pageon fetch https://example.com --dump requests
    ///   pageon fetch http://localhost:3000 --private-network allow
    ///   pageon fetch https://app.example --state state.json --save-state state.json
    ///   pageon fetch https://shop.example --origin https://shop.example --approve
    ///   pageon fetch https://example.com --act "click e3" --act "type e5 hello" --dump ax
    ///   PAGEON_SECRET_SHOP=… pageon fetch https://shop.example/login --secret shop@https://shop.example --act 'type e2 secret://shop'
    ///   pageon fetch https://example.com --record example.rec.json
    ///   pageon fetch https://example.com --replay example.rec.json   (offline)
    ///   pageon fetch https://example.com --screenshot page.png
    ///   pageon fetch https://example.com --screenshot marks.png --marks   (legend on stdout)
    ///   pageon fetch https://example.com --screenshot pay.png --crop e9   (provenance on stdout)
    #[command(verbatim_doc_comment)]
    Fetch {
        /// URL (http, https, file) or path to a local file.
        target: String,
        /// What to print [default: text, or nothing with --screenshot].
        #[arg(long, value_enum)]
        dump: Option<DumpArg>,
        /// Viewport for `--dump boxes`, `ax` and `markdown`, e.g. `1280x720`.
        #[arg(long, value_name = "WxH", default_value = "1280x720", value_parser = parse_viewport)]
        viewport: pageon::Viewport,
        /// Act on the page after loading, in order (repeatable): `click e5`, `type e3 hello`,
        /// `press Enter`, `select e7 Germany`, `hover e2`, `wait 500`, `goto URL`, `back`.
        /// Refs are those of `--dump ax`.
        #[arg(long = "act", value_name = "ACTION")]
        acts: Vec<String>,
        /// Ask on the terminal before every write (POST, PUT, …) to an --origin.
        #[arg(long)]
        approve: bool,
        /// After the last --act: an expectation to check (repeatable; exit 8 when one
        /// fails): `url /done`, `see "Thanks"`, `see button "Pay"`, `no "Error"`,
        /// `request POST /orders 2xx`, `download *.pdf`.
        #[arg(long = "expect", value_name = "CHECK")]
        expect: Vec<String>,
        /// Ask for the site's Markdown rendition first (`Accept: text/markdown`), for crawls;
        /// a Markdown answer is labelled `source:server-markdown`.
        #[arg(long)]
        prefer_markdown: bool,
        #[command(flatten)]
        paint: PaintArgs,
        #[command(flatten)]
        session: SessionArgs,
    },
    /// How ready a page is for agents: names, actions, page tools, what the agent's view costs,
    /// access and health, scored with what to fix first.
    ///
    /// Examples:
    ///   pageon audit https://example.com
    ///   pageon audit https://example.com --json > audit.json
    ///   pageon audit http://localhost:3000 --private-network allow --fail-under 80   (CI)
    ///
    /// Also reads the site's /robots.txt and /llms.txt and asks for the page as Markdown,
    /// under the same options.
    #[command(verbatim_doc_comment)]
    Audit {
        /// URL (http, https, file) or path to a local file.
        target: String,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
        /// Exit 9 when the score is under this (0 to 100).
        #[arg(long, value_name = "SCORE", value_parser = clap::value_parser!(u8).range(0..=100))]
        fail_under: Option<u8>,
        #[command(flatten)]
        session: SessionArgs,
    },
    /// Measure how fast pages open: first view, warm opens, CPU, memory, layouts, tokens.
    ///
    /// Examples:
    ///   pageon bench https://news.ycombinator.com https://github.com
    ///   pageon bench ep.json --runs 10            (a recording made with --record)
    ///   pageon bench https://example.com --json > before.json
    ///   pageon bench https://example.com --against before.json
    ///   pageon bench http://localhost:3000 -- --private-network allow
    ///
    /// A URL is recorded once (cached under ~/.cache/pageon/bench) and every run replays it
    /// offline: the network is not measured and runs are reproducible. First view and CPU are
    /// separate `pageon fetch` processes, caged; warm opens reuse one in-process session.
    #[command(verbatim_doc_comment)]
    Bench {
        /// Pages: URLs, or recordings made with --record.
        #[arg(required = true)]
        pages: Vec<String>,
        /// Runs per page.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=1000))]
        runs: u32,
        /// Record the URLs again instead of using the cached recordings.
        #[arg(long)]
        refresh: bool,
        /// Print the results as JSON.
        #[arg(long)]
        json: bool,
        /// An earlier `--json` result: show the change per column.
        #[arg(long, value_name = "FILE")]
        against: Option<PathBuf>,
        /// Session options for every run, after `--` (as for `pageon fetch`).
        #[arg(last = true, value_name = "OPTIONS")]
        options: Vec<String>,
    },
    /// Re-run a recorded session and check that every step reaches the same page.
    ///
    /// Examples:
    ///   pageon fetch https://shop.example/login --act "type e3 ada" --act "click e5" --record ep.json
    ///   pageon replay ep.json              # ✓ per step; exit 0 identical, 5 diverged
    ///   pageon replay ep.json --to 1       # the agent view at step 1
    ///   pageon replay ep.json --live       # against the live site: has it changed since?
    ///   pageon replay ep.json --trace trace.zip && npx playwright show-trace trace.zip
    ///
    /// Offline replays serve every request from the recording; nothing leaves the machine.
    #[command(verbatim_doc_comment)]
    Replay {
        /// A recording made with --record (by `pageon fetch --act` or `pageon mcp`).
        file: PathBuf,
        /// Stop after this step and print the page as it was then (see --dump).
        #[arg(long, value_name = "N")]
        to: Option<usize>,
        /// Re-run against the live network instead of the recording, under this command's
        /// options (not the recording's). Recorded writes are held: refused, or asked with
        /// --approve.
        #[arg(long)]
        live: bool,
        /// With --live: ask on the terminal before each recorded write instead of refusing it.
        #[arg(long, requires = "live")]
        approve: bool,
        /// With --to: what to print [default: ax].
        #[arg(long, value_enum)]
        dump: Option<DumpArg>,
        /// Also write a Playwright trace of the replay (actions, pages, network, console).
        #[arg(long, value_name = "FILE")]
        trace: Option<PathBuf>,
        /// Print what each step cost (tokens, wall and page time) with totals and
        /// p50/p95/p99, from the recording, without re-running it.
        #[arg(long, conflicts_with_all = ["to", "live", "trace"])]
        report: bool,
        #[command(flatten)]
        session: SessionArgs,
    },
    /// Serve a sandboxed browser to an agent over MCP (stdio, or Streamable HTTP with --http).
    ///
    /// Add it to an agent host:
    ///   Claude Code:     claude mcp add pageon -- pageon mcp
    ///   Claude Desktop, Cursor (mcpServers in their JSON config):
    ///     "pageon": { "command": "pageon", "args": ["mcp"] }
    ///   Over HTTP:       pageon mcp --http 8931, then add the printed URL
    ///
    /// Protocol: MCP 2026-07-28 (no handshake) and 2024-11-05 … 2025-11-25 (initialize).
    ///
    /// Tools:
    ///   read     browser_open, browser_observe, browser_find, browser_view, browser_screenshot
    ///   tabs     browser_tabs (new, select, close, list; replies name the tab)
    ///   act      browser_click, browser_type, browser_upload, browser_press, browser_select,
    ///            browser_paste, browser_hover, browser_drag, browser_scroll, browser_wait,
    ///            browser_back, browser_forward, browser_call_tool
    ///   explore  browser_lookahead (try actions on a fork; the page is unchanged, writes are held),
    ///            browser_reset (back to any step, exactly, sending nothing again)
    ///   reuse    browser_workflow_save, browser_workflow_run
    ///   people   browser_takeover (a live view for a person: CAPTCHAs, second factors)
    ///
    /// Every reply ends with its step number. Logs go to stderr; over stdio, stdout carries the
    /// protocol.
    #[command(verbatim_doc_comment)]
    Mcp {
        /// Serve MCP 2026-07-28 over Streamable HTTP on 127.0.0.1 instead of stdio, at the
        /// printed URL (it holds a per-run token); no value or 0 picks a free port.
        #[arg(long, value_name = "PORT", num_args = 0..=1, default_missing_value = "0")]
        http: Option<u16>,
        /// With --http: allow calls from a web origin (repeatable; `*` for any). Browsers send
        /// an Origin, other clients do not.
        #[arg(long, value_name = "ORIGIN", requires = "http")]
        allow_origin: Vec<String>,
        /// Let the vendor toolsets run `javascript_exec`: model-written expressions, in an
        /// isolated world over the page (its DOM, cookies and same-origin requests).
        #[arg(long)]
        allow_javascript_exec: bool,
        /// Send one OpenTelemetry span per tool call to this OTLP/HTTP collector
        /// (http://localhost:4318); a client's `_meta.traceparent` becomes the parent.
        #[arg(long, value_name = "URL")]
        otlp: Option<String>,
        /// How much views show at the start: full, or compact (no text runs; agents read
        /// text with browser_view markdown). browser_observe changes it.
        #[arg(long, value_name = "LEVEL", default_value = "full", value_parser = ["full", "compact"])]
        detail: String,
        #[command(flatten)]
        watch: Watch,
        #[command(flatten)]
        session: SessionArgs,
    },
    /// Serve a sandboxed browser over the Chrome DevTools Protocol and WebDriver BiDi, for
    /// Puppeteer, Playwright and BiDi clients.
    ///
    /// Connect with:
    ///   puppeteer.connect({ browserURL: 'http://127.0.0.1:9222' })
    ///   chromium.connectOverCDP('http://127.0.0.1:9222')
    ///   puppeteer.connect({ browserWSEndpoint: 'ws://127.0.0.1:9222/session',
    ///                       protocol: 'webDriverBiDi' })
    ///
    /// pageon implements the CDP subset Puppeteer and Playwright use: pages, frames and
    /// navigation, evaluate and handles, selectors, locators and waiting, clicks and typing,
    /// cookies, events and screenshots. Methods it lacks fail at once with what to use
    /// instead. The agent view is one call away:
    /// `(await page.createCDPSession()).send('Pageon.observe')`, and set-of-marks another:
    /// `send('Pageon.screenshot', {marks: true})`. Over BiDi: `pageon:observe` and `pageon:act`.
    ///
    /// Listens on 127.0.0.1 only; serves one client and exits when it disconnects.
    #[command(verbatim_doc_comment, visible_alias = "bidi")]
    Cdp {
        /// Port on 127.0.0.1 (0: pick a free one).
        #[arg(long, default_value_t = 9222)]
        port: u16,
        /// Allow WebSocket connections from a web origin (repeatable; `*` for any). Browsers
        /// send an Origin, Node clients do not.
        #[arg(long, value_name = "ORIGIN")]
        allow_origin: Vec<String>,
        /// Serve `/json/version` and `/json/list` for clients that discover the WebSocket URL
        /// over HTTP (Playwright's `connectOverCDP('http://…')`). Any local process can then
        /// read the URL; prefer connecting to the printed `ws://` URL.
        #[arg(long)]
        discovery: bool,
        #[command(flatten)]
        session: SessionArgs,
    },
    /// Create and publish a signed agent identity (Web Bot Auth).
    ///
    /// Examples:
    ///   pageon identity new --agent https://bot.example
    ///   pageon identity directory > http-message-signatures-directory
    ///   pageon fetch https://example.com --identity pageon-identity.json
    ///
    /// With --identity (or PAGEON_IDENTITY), every request pageon sends carries a Web Bot Auth
    /// signature (RFC 9421, Ed25519) that sites and CDNs can verify against your directory.
    #[command(verbatim_doc_comment)]
    Identity {
        #[command(subcommand)]
        command: IdentityCommand,
    },
    /// Compile a recorded session into a workflow, and run workflows without a model.
    ///
    /// Examples:
    ///   pageon workflow compile ep.json -o report.flow.json
    ///   pageon workflow run report.flow.json --param user=bob --secret shop@https://shop.example
    ///
    /// A workflow names elements as the agent saw them (role and name), with parameters for
    /// typed values. A run stops with `drifted` (exit 7) when the site has moved on; repair by
    /// acting by hand and compiling again.
    #[command(verbatim_doc_comment)]
    Workflow {
        #[command(subcommand)]
        command: WorkflowCommand,
    },
    /// The caged content process (started by pageon itself).
    #[command(hide = true)]
    Content {
        /// Run the cage self-test.
        #[arg(long)]
        probe: bool,
    },
}

#[derive(Subcommand)]
// Parsed once per run: the size difference between the subcommands does not matter.
#[allow(clippy::large_enum_variant)]
enum WorkflowCommand {
    /// Compile the steps of a recording (`--record`) into a workflow.
    Compile {
        /// The recording.
        recording: PathBuf,
        /// Where to write the workflow [default: stdout].
        #[arg(short, long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Run a workflow live; exit 0 done, 7 drifted.
    Run {
        /// The workflow.
        file: PathBuf,
        /// A parameter value (repeatable): NAME=VALUE.
        #[arg(long = "param", value_name = "NAME=VALUE")]
        params: Vec<String>,
        #[command(flatten)]
        session: SessionArgs,
    },
}

#[derive(Subcommand)]
enum IdentityCommand {
    /// Create a new Ed25519 identity.
    New {
        /// Your Signature-Agent URL (https): where verifiers find your key directory.
        #[arg(long, value_name = "URL")]
        agent: String,
        /// The key file to write (never overwritten; readable by you only).
        #[arg(long, value_name = "FILE", default_value = "pageon-identity.json")]
        out: PathBuf,
    },
    /// Print the key directory to host (body on stdout, response headers on stderr).
    Directory {
        /// The key file.
        #[arg(
            long,
            value_name = "FILE",
            env = "PAGEON_IDENTITY",
            default_value = "pageon-identity.json"
        )]
        identity: PathBuf,
        /// Days the directory's signature stays valid.
        #[arg(long, value_name = "DAYS", default_value_t = 90)]
        days: u64,
    },
}

/// What an agent's session adds to the page options: supervision and sign-ins.
#[derive(Args)]
struct Watch {
    /// A sign-in workflow (`pageon workflow compile` of a recorded login) whose start page is
    /// the site's sign-in page: when an action finds the session signed out of that site,
    /// pageon runs it and opens the page again (repeatable, one per site).
    #[arg(long = "sign-in", value_name = "FILE")]
    sign_in: Vec<PathBuf>,
    /// Serve a page that shows each step and pauses the session (its URL on stderr).
    #[arg(long)]
    ui: bool,
    /// Ask in that page before each write the policy holds (writes to an --origin, after
    /// reading another site with --cross-site-writes refuse, a guard's ask). Implies --ui.
    #[arg(long)]
    approve: bool,
}

impl Watch {
    /// The sign-ins, loaded and checked.
    fn sign_ins(&self) -> Result<Vec<pageon::sign_in::SignIn>, ExitCode> {
        let all = self
            .sign_in
            .iter()
            .map(|p| pageon::sign_in::SignIn::load(p))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|all| pageon::sign_in::check(&all).map(|()| all));
        all.map_err(|e| {
            eprintln!("error: {e}");
            ExitCode::from(2)
        })
    }

    /// Starts the viewer when asked and makes it the approver with --approve.
    fn start(
        &self,
        config: &mut pageon::Config,
    ) -> Result<Option<pageon::viewer::Viewer>, ExitCode> {
        if !self.ui && !self.approve {
            return Ok(None);
        }
        let viewer = pageon::viewer::Viewer::start().map_err(|e| {
            eprintln!("error: cannot serve the viewer: {e}");
            ExitCode::from(2)
        })?;
        eprintln!("viewer: {}", viewer.url());
        if self.approve {
            *config = config.clone().approver(viewer.approver());
        }
        Ok(Some(viewer))
    }
}

/// Pixels, after loading and any --act.
#[derive(Args)]
struct PaintArgs {
    /// Write a screenshot of the viewport to FILE (.png, or .jpg/.jpeg).
    #[arg(long, value_name = "FILE")]
    screenshot: Option<PathBuf>,
    /// The whole page instead of the viewport (capped at 16384 px).
    #[arg(long, requires = "screenshot")]
    full_page: bool,
    /// Set-of-marks: outline and label every visible element with its ref; the legend goes
    /// to stdout.
    #[arg(long, requires = "screenshot")]
    marks: bool,
    /// Only this element (a ref of `--dump ax`), with where its pixels come from on stdout.
    #[arg(long, value_name = "REF", requires = "screenshot", conflicts_with_all = ["full_page", "marks"])]
    crop: Option<String>,
    /// Device pixels per CSS pixel, 0.25 to 2.
    #[arg(long, default_value_t = 1.0, requires = "screenshot")]
    scale: f32,
    /// Paint over third-party frames and text a person cannot read (for vision models reading
    /// untrusted pages); the notes say what was masked.
    #[arg(long, requires = "screenshot")]
    mask: bool,
}

/// What every page session needs: network, sandbox, policy, secrets, state, recordings.
#[derive(Args)]
struct SessionArgs {
    /// Record everything the session used to this file.
    #[arg(long, value_name = "FILE", conflicts_with = "replay")]
    record: Option<PathBuf>,
    /// Serve from a recording, offline.
    #[arg(long, value_name = "FILE")]
    replay: Option<PathBuf>,
    /// Seconds before a request times out.
    #[arg(long, value_name = "SECS", default_value_t = 30)]
    timeout: u64,
    /// Maximum response size in bytes.
    #[arg(long, value_name = "BYTES", default_value_t = 50 * 1024 * 1024)]
    max_bytes: u64,
    /// Do not run the page's scripts.
    #[arg(long)]
    no_js: bool,
    /// Token budget (o200k) of the agent views (`ax`, `markdown`).
    #[arg(long, value_name = "TOKENS", default_value_t = 4000)]
    budget: usize,
    /// Whether the page may reach the private network (localhost, 10.0.0.0/8, 169.254.0.0/16,
    /// …). Denied by default.
    #[arg(long, value_enum, value_name = "POLICY", default_value_t = PrivateNetwork::Deny)]
    private_network: PrivateNetwork,
    /// Start with this state: a Playwright storageState file (cookies and localStorage) or a
    /// cookie array.
    #[arg(long, value_name = "FILE")]
    state: Option<PathBuf>,
    /// Same as `--state`.
    #[arg(long, value_name = "FILE", hide = true, conflicts_with = "state")]
    cookies: Option<PathBuf>,
    /// Write the session's cookies and localStorage (Playwright storageState) here at the end.
    #[arg(long, value_name = "FILE")]
    save_state: Option<PathBuf>,
    /// Run the page in-process instead of the sandbox (pages then run with your rights).
    #[arg(long)]
    no_sandbox: bool,
    /// Let the agent open local files (file: URLs and paths) as pages over MCP, CDP and BiDi.
    /// Off by default there; web pages never load local files either way.
    #[arg(long)]
    allow_files: bool,
    /// An origin the task may read and write (repeatable; `https://*.example.com` for
    /// subdomains). With any --origin or --read-origin, requests elsewhere are refused.
    #[arg(long, value_name = "ORIGIN")]
    origin: Vec<String>,
    /// An origin the task may only read (repeatable).
    #[arg(long, value_name = "ORIGIN")]
    read_origin: Vec<String>,
    /// A secret for `type eN secret://NAME`, bound to ORIGIN: NAME@ORIGIN, value from
    /// PAGEON_SECRET_<NAME>; NAME:totp@ORIGIN for a TOTP seed (repeatable).
    #[arg(long, value_name = "NAME@ORIGIN")]
    secret: Vec<String>,
    /// A file the agent may upload as NAME (`upload eN NAME`), to pages of ORIGIN (several
    /// comma-separated): NAME@ORIGIN=PATH (repeatable). Read when uploaded; never a path the
    /// agent chooses.
    #[arg(long = "file", id = "staged_file", value_name = "NAME@ORIGIN=PATH")]
    staged_file: Vec<String>,
    /// Send every request through this proxy (HTTP_PROXY / HTTPS_PROXY / NO_PROXY are used
    /// without it).
    #[arg(long, value_name = "URL", conflicts_with = "replay")]
    proxy: Option<String>,
    /// Ask this program about every request but reads of known origins: one JSON
    /// effect per line on its stdin, one JSON verdict (`{"verdict": "allow"|"deny"|"ask"}`)
    /// per line on its stdout; it fails closed.
    #[arg(long, value_name = "COMMAND", env = "PAGEON_GUARD")]
    guard: Option<String>,
    /// Sign every request with this identity (Web Bot Auth; `pageon identity new`).
    #[arg(long, value_name = "FILE", env = "PAGEON_IDENTITY")]
    identity: Option<PathBuf>,
    /// Sign in with the passkeys in this store (created 0600 when missing; passkeys a site
    /// creates are added). Keys never reach the page process.
    #[arg(long, value_name = "FILE", env = "PAGEON_PASSKEYS")]
    passkeys: Option<PathBuf>,
    /// Who pageon says it is: an automated agent, or a pinned, coherent Chrome.
    #[arg(long, value_enum, value_name = "PROFILE", default_value_t = ProfileArg::Agent, env = "PAGEON_PROFILE")]
    profile: ProfileArg,
    /// The Chrome profile's platform: the machine's own (`host`), or another.
    #[arg(long, value_enum, value_name = "PLATFORM", env = "PAGEON_PLATFORM")]
    platform: Option<PlatformArg>,
    /// Writes to a site after the agent read another site's content: `allow` (labelled for
    /// the guard and the approver) or `refuse` (unless the approver approves).
    #[arg(long, value_enum, value_name = "POLICY", default_value_t = CrossSiteArg::Allow, env = "PAGEON_CROSS_SITE_WRITES")]
    cross_site_writes: CrossSiteArg,
    /// The tag request signatures carry (`web-bot-auth`; a payment network's agent program may
    /// name another, e.g. `agent-browser-auth`).
    #[arg(long, value_name = "TAG", env = "PAGEON_SIGNATURE_TAG")]
    signature_tag: Option<String>,
    /// Whether to honour robots.txt for the pages the agent opens. `ignore` is for sessions a
    /// person directs.
    #[arg(long, value_enum, value_name = "POLICY", default_value_t = RobotsArg::Respect)]
    robots: RobotsArg,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProfileArg {
    /// pageon, an automated agent (honest user agent, navigator.webdriver true).
    Agent,
    /// Chrome 149, coherent across TLS, HTTP/2, headers, client hints and scripts.
    #[value(alias = "browser-compatible")]
    Chrome,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum CrossSiteArg {
    /// Allow them; the guard and the approver see which sites were read.
    Allow,
    /// Refuse them unless the approver approves.
    Refuse,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PlatformArg {
    /// This machine's operating system.
    Host,
    /// Windows 11.
    Windows,
    /// macOS.
    Macos,
    /// Linux.
    Linux,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum RobotsArg {
    /// Honour robots.txt (RFC 9309, product token `pageon`).
    Respect,
    /// Do not check it (a session a person directs).
    Ignore,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum PrivateNetwork {
    /// Refuse loopback, private and link-local destinations.
    Deny,
    /// Allow them (local development).
    Allow,
}

#[derive(Clone, Copy, ValueEnum)]
enum DumpArg {
    /// Readable text.
    Text,
    /// HTML serialization.
    Html,
    /// Element outline.
    Tree,
    /// Element outline with boxes and flags (hidden, offscreen, covered-by).
    Boxes,
    /// The agent view: landmarks, headings, quoted text, elements with refs.
    Ax,
    /// The main content as Markdown (visible text only; links carry refs).
    Markdown,
    /// Script errors of the page and its frames, one per line.
    Errors,
    /// Every request of the load (frames, scripts, fetch, XHR), with blocked ones and why.
    Requests,
}

fn parse_viewport(s: &str) -> Result<pageon::Viewport, String> {
    let (w, h) = s
        .split_once(['x', '×'])
        .ok_or_else(|| format!("{s:?}: expected WxH, e.g. 1280x720"))?;
    let num = |v: &str| {
        v.trim()
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=16384).contains(n))
    };
    match (num(w), num(h)) {
        (Some(width), Some(height)) => Ok(pageon::Viewport { width, height }),
        _ => Err(format!(
            "{s:?}: width and height must be 1–16384, e.g. 1280x720"
        )),
    }
}

fn fail(e: &LoadError) -> ExitCode {
    eprintln!("error: {e}\n  {}", e.guidance());
    ExitCode::from(e.exit_code())
}

fn dump_kind(d: DumpArg) -> Dump {
    match d {
        DumpArg::Text => Dump::Text,
        DumpArg::Html => Dump::Html,
        DumpArg::Tree => Dump::Tree,
        DumpArg::Boxes => Dump::Boxes,
        DumpArg::Ax => Dump::Ax,
        DumpArg::Markdown => Dump::Markdown,
        DumpArg::Errors => Dump::Errors,
        DumpArg::Requests => Dump::Requests,
    }
}

/// Asks on the terminal whether a write may leave (deny without a terminal).
fn ask(effect: &Effect) -> bool {
    use std::io::BufRead as _;
    let Ok(tty) = std::fs::File::open("/dev/tty") else {
        eprintln!("policy: {effect} refused (no terminal to ask on)");
        return false;
    };
    eprint!("allow {effect}? [y/N] ");
    let mut line = String::new();
    let _ = std::io::BufReader::new(tty).read_line(&mut line);
    matches!(line.trim(), "y" | "Y" | "yes")
}

/// The session flags alone, parsed from arguments (for `pageon bench`'s warm runs).
#[derive(Parser)]
struct Flags {
    #[command(flatten)]
    session: SessionArgs,
}

/// The session flags, checked: load options, the environment and the starting state.
struct Prepared {
    options: LoadOptions,
    config: pageon::Config,
    state: Option<StorageState>,
}

fn prepare(s: &SessionArgs) -> Result<Prepared, ExitCode> {
    let platform = match s.platform {
        None | Some(PlatformArg::Host) => pageon::Platform::host(),
        Some(PlatformArg::Windows) => pageon::Platform::Windows,
        Some(PlatformArg::Macos) => pageon::Platform::Macos,
        Some(PlatformArg::Linux) => pageon::Platform::Linux,
    };
    if s.platform.is_some() && s.profile == ProfileArg::Agent {
        eprintln!(
            "error: --platform applies to --profile chrome\n  add --profile chrome, or drop --platform"
        );
        return Err(ExitCode::from(2));
    }
    let mut config = pageon::Config::new()
        .timeout(Duration::from_secs(s.timeout))
        .max_bytes(s.max_bytes)
        .profile(match s.profile {
            ProfileArg::Agent => pageon::Profile::Agent,
            ProfileArg::Chrome => pageon::Profile::BrowserCompatible(platform),
        });
    if let Some(t) = &s.signature_tag {
        config = config.signature_tag(t);
    }
    config = config.cross_site_writes(s.cross_site_writes == CrossSiteArg::Refuse);
    for o in &s.origin {
        config = config.origin(o);
    }
    for o in &s.read_origin {
        config = config.read_origin(o);
    }
    for spec in &s.secret {
        config = config.secret_spec(spec);
    }
    for spec in &s.staged_file {
        config = config.file_spec(spec);
    }
    if let Some(g) = &s.guard {
        config = config.guard(g);
    }
    if let Some(p) = &s.proxy {
        config = config.proxy(p);
    }
    if let Some(p) = &s.identity {
        config = config.identity(p);
    }
    if let Some(p) = &s.passkeys {
        config = config.passkeys(p);
    }
    if let Some(p) = &s.record {
        config = config.record(p);
    }
    if let Some(p) = &s.replay {
        config = config.replay(p);
    }
    match config.check() {
        Ok(warnings) => {
            for w in warnings {
                eprintln!("warning: {w}");
            }
        }
        Err(e) => return Err(config_failed(&e)),
    }
    let state = match s.state.as_ref().or(s.cookies.as_ref()) {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| fail(&LoadError::Io(format!("{}: {e}", path.display()))))?;
            Some(StorageState::from_json(&text).map_err(|e| {
                fail(&LoadError::Io(format!(
                    "{}: not a storageState file or cookie array: {e}",
                    path.display()
                )))
            })?)
        }
        None => None,
    };
    Ok(Prepared {
        options: LoadOptions {
            scripts: !s.no_js,
            allow_private_network: s.private_network == PrivateNetwork::Allow,
            robots: s.robots == RobotsArg::Respect,
            ..LoadOptions::default()
        },
        config,
        state,
    })
}

/// Prints a configuration error with its fix; the exit code (3 for files, else 2).
fn config_failed(e: &pageon::ConfigError) -> ExitCode {
    eprintln!("error: {e}\n  {}", e.guidance());
    ExitCode::from(if e.is_io() { 3 } else { 2 })
}

/// Runs `job` over the session's environment (with the terminal approver when asked; `keep`
/// keeps the steps in memory for MCP's resets).
fn with_env<T>(
    config: &pageon::Config,
    approve: bool,
    keep: bool,
    job: impl FnOnce(&dyn Env) -> Result<T, ExitCode>,
) -> Result<T, ExitCode> {
    let config = if approve {
        config.clone().approver(ask)
    } else {
        config.clone()
    };
    let r = if keep {
        config.run_keeping(job)
    } else {
        config.run(job)
    };
    r.map_err(|e| config_failed(&e))?
}

fn save_state(path: Option<&PathBuf>, s: &StorageState) -> Result<(), ExitCode> {
    if let Some(path) = path {
        let text = serde_json::to_string_pretty(s).unwrap_or_default() + "\n";
        pageon_env::write_private(path, text.as_bytes())
            .map_err(|e| fail(&LoadError::Io(format!("{}: {e}", path.display()))))?;
    }
    Ok(())
}

fn identity(command: IdentityCommand) -> ExitCode {
    match command {
        IdentityCommand::New { agent, out } => {
            let id = match Identity::generate(&agent).and_then(|id| id.save(&out).map(|()| id)) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(2);
                }
            };
            let base = id.agent().trim_end_matches('/');
            println!("wrote {} (keyid {})", out.display(), id.keyid());
            println!("next:");
            println!(
                "  1. host the directory: pageon identity directory --identity {} \\",
                out.display()
            );
            println!("       > http-message-signatures-directory");
            println!("     at {base}/.well-known/http-message-signatures-directory");
            println!("     (serve it with the headers printed on stderr)");
            println!(
                "  2. sign requests: pageon fetch <url> --identity {}   (or PAGEON_IDENTITY)",
                out.display()
            );
            println!(
                "  3. register {base} with the verifiers you need (Cloudflare: Bot Submission Form)"
            );
            ExitCode::SUCCESS
        }
        IdentityCommand::Directory { identity, days } => {
            let id = match Identity::load(&identity) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(2);
                }
            };
            let authority = url::Url::parse(id.agent())
                .ok()
                .and_then(|u| {
                    u.host_str().map(|h| {
                        u.port()
                            .map_or_else(|| h.to_string(), |p| format!("{h}:{p}"))
                    })
                })
                .unwrap_or_default();
            #[allow(clippy::disallowed_methods)] // the directory's validity is real time
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let (body, headers) = id.directory(&authority, now, now + days * 86_400);
            println!("{body}");
            for (k, v) in headers {
                eprintln!("{k}: {v}");
            }
            ExitCode::SUCCESS
        }
    }
}

/// Page text with control characters other than newline and tab escaped (`\u001b`), so a page
/// cannot drive the terminal it is printed to (titles, cursor moves, clearing, hyperlinks).
fn terminal_safe(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return s.into();
    }
    s.chars()
        .map(|c| match c {
            '\n' | '\t' => c.to_string(),
            c if c.is_control() => format!("\\u{:04x}", c as u32),
            c => c.to_string(),
        })
        .collect::<String>()
        .into()
}

fn acted(r: Result<pageon::Outcome, pageon::ActionError>) -> Result<pageon::Outcome, ExitCode> {
    match r {
        Ok(o) => {
            eprintln!("{o}");
            Ok(o)
        }
        Err(e) => {
            eprintln!("error: {e}\n  {}", e.guidance());
            Err(ExitCode::from(3))
        }
    }
}

/// Checks `fetch`'s expectations, one line each on stderr; exit 8 when one fails.
fn expected(
    written: &[String],
    outcome: Option<&pageon::Outcome>,
    facts: &mut dyn pageon::verify::Facts,
) -> Result<(), ExitCode> {
    let expects = pageon::verify::parse_all(written).map_err(|e| {
        eprintln!("error: {e}");
        ExitCode::from(2)
    })?;
    let mut failed = false;
    for (e, w) in expects.iter().zip(written) {
        let c = pageon::verify::check(e, w, outcome, facts);
        failed |= !c.ok;
        eprintln!("{}", terminal_safe(&c.to_string()));
    }
    if failed {
        Err(ExitCode::from(8))
    } else {
        Ok(())
    }
}

/// Page facts of a caged tab.
struct RemoteFacts<'a, 's, E: Env>(&'a mut pageon::RemoteTab<'s, E>);

impl<E: Env> pageon::verify::Facts for RemoteFacts<'_, '_, E> {
    fn url(&mut self) -> Option<String> {
        self.0
            .observe_full()
            .ok()
            .and_then(|v| pageon::workflow::view_url(&v))
    }

    fn text(&mut self) -> Result<String, String> {
        self.0
            .read(pageon::Read::Page {
                filter: pageon_agent::Filter::All,
                depth: u32::MAX,
                root: None,
                budget: Budget::o200k(200_000),
            })
            .map_err(|e| e.to_string())
    }

    fn elements(&mut self, name: &str) -> Result<Vec<String>, String> {
        self.0
            .read(pageon::Read::Find {
                query: name.to_string(),
                limit: 50,
            })
            .map(|t| t.lines().map(str::to_string).collect())
            .map_err(|e| e.to_string())
    }
}

/// Page facts of an in-process tab.
struct LocalFacts<'a, 'b, E: Env>(&'a mut pageon::Tab<'b, E>);

impl<E: Env> pageon::verify::Facts for LocalFacts<'_, '_, E> {
    fn url(&mut self) -> Option<String> {
        Some(self.0.url().to_string())
    }

    fn text(&mut self) -> Result<String, String> {
        self.0
            .read(&pageon::Read::Page {
                filter: pageon_agent::Filter::All,
                depth: u32::MAX,
                root: None,
                budget: Budget::o200k(200_000),
            })
            .map_err(|e| e.to_string())
    }

    fn elements(&mut self, name: &str) -> Result<Vec<String>, String> {
        self.0
            .read(&pageon::Read::Find {
                query: name.to_string(),
                limit: 50,
            })
            .map(|t| t.lines().map(str::to_string).collect())
            .map_err(|e| e.to_string())
    }
}

fn broken(e: pageon::SandboxError) -> ExitCode {
    eprintln!("error: {e}\n  {}", e.guidance());
    ExitCode::from(3)
}

/// What a fetch prints: the dump, the page's HTTP status and the site's refusal.
type Printed = (String, u16, Option<pageon::Refusal>);

struct Fetch<'a> {
    target: &'a str,
    dump: Option<DumpArg>,
    paint: &'a PaintArgs,
    viewport: pageon::Viewport,
    actions: Vec<Action>,
    expect: Vec<String>,
    budget: Budget,
    session: &'a SessionArgs,
    prepared: &'a Prepared,
}

/// Loads the page (caged by default), runs the actions (outcome lines to stderr), and
/// returns the dump.
fn fetch(env: &dyn Env, f: Fetch<'_>) -> Result<Printed, ExitCode> {
    let format = match &f.paint.screenshot {
        Some(p) => Some(
            pageon::Format::from_path(&p.to_string_lossy()).ok_or_else(|| {
                eprintln!(
                    "error: cannot tell the image format of {}\n  name the file .png, .jpg or .jpeg",
                    p.display()
                );
                ExitCode::from(2)
            })?,
        ),
        None => None,
    };
    let dump = match (f.dump, format) {
        (Some(d), _) => Some(d),
        (None, None) => Some(DumpArg::Text),
        (None, Some(_)) => None,
    };
    let kind = dump_kind(dump.unwrap_or(DumpArg::Text));
    let acted_ax = matches!(kind, Dump::Ax) && !f.actions.is_empty();
    let options = f.prepared.options.clone();
    if !f.session.no_sandbox {
        let mut sb = Sandbox::new(env, SandboxOptions::default()).map_err(broken)?;
        if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            eprintln!("{}", sb.receipt());
        }
        if let Some(s) = &f.prepared.state {
            sb.set_storage_state(s).map_err(broken)?;
        }
        let (text, status, refusal) = {
            let mut tab = sb.open(f.target, options).map_err(|e| fail(&e))?;
            if !f.actions.is_empty() {
                tab.observe().map_err(broken)?;
            }
            let mut last = None;
            for a in f.actions {
                last = Some(acted(tab.act(a))?);
            }
            expected(&f.expect, last.as_ref(), &mut RemoteFacts(&mut tab))?;
            let (line, code, refusal) = tab.status().map_err(broken)?;
            eprintln!("{}", terminal_safe(&line));
            let mut text = match dump {
                None => String::new(),
                Some(_) if acted_ax => tab.observe().map_err(broken)?,
                Some(_) => tab.view(kind, f.viewport, f.budget).map_err(broken)?,
            };
            if let (Some(path), Some(format)) = (&f.paint.screenshot, format) {
                let img = match &f.paint.crop {
                    Some(r) => tab.crop(r, 8.0, format, f.paint.scale),
                    None => tab.screenshot(&screenshot_options(f.paint, format)),
                };
                text.push_str(&painted(img, path)?);
            }
            (text, code, refusal)
        };
        if f.session.save_state.is_some() {
            save_state(
                f.session.save_state.as_ref(),
                &sb.storage_state().map_err(broken)?,
            )?;
        }
        return Ok((text, status, refusal));
    }
    let session = Session::new(env);
    if let Some(s) = &f.prepared.state {
        session.set_storage_state(s);
    }
    let mut tab = session.open(f.target, options).map_err(|e| fail(&e))?;
    tab.set_view(f.budget, Mode::Full);
    if !f.actions.is_empty() {
        tab.observe();
    }
    let mut last = None;
    for a in f.actions {
        last = Some(acted(tab.act(a))?);
    }
    expected(&f.expect, last.as_ref(), &mut LocalFacts(&mut tab))?;
    eprintln!("{}", terminal_safe(&tab.status_line()));
    let mut text = match dump {
        None => String::new(),
        Some(_) if acted_ax => tab.observe(),
        Some(_) => tab.view(kind, f.viewport, f.budget),
    };
    if let (Some(path), Some(format)) = (&f.paint.screenshot, format) {
        let img = match &f.paint.crop {
            Some(r) => tab.crop(r, 8.0, format, f.paint.scale),
            None => tab.screenshot(&screenshot_options(f.paint, format)),
        };
        text.push_str(&painted(img, path)?);
    }
    let status = tab.status();
    let refusal = tab.document().refusal().cloned();
    drop(tab);
    save_state(f.session.save_state.as_ref(), &session.storage_state())?;
    Ok((text, status, refusal))
}

/// `pageon audit`: loads the page and the site's files, prints the report, exits 9 under
/// `--fail-under`.
fn audit(target: &str, json: bool, fail_under: Option<u8>, session: &SessionArgs) -> ExitCode {
    let prepared = match prepare(session) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let facts = with_env(&prepared.config, false, false, |env| {
        audit_facts(env, target, session, &prepared)
    });
    let (page, site) = match facts {
        Ok(f) => f,
        Err(code) => return code,
    };
    let report = pageon::audit::Report::new(&page, &site);
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(e) => {
                eprintln!("error: cannot write the report: {e}");
                return ExitCode::from(3);
            }
        }
    } else {
        let color = std::io::IsTerminal::is_terminal(&std::io::stdout())
            && std::env::var_os("NO_COLOR").is_none();
        print!("{}", report.render(pageon::audit::Style { color }));
    }
    match fail_under {
        Some(min) if report.score < min => {
            eprintln!("audit: {} is under --fail-under {min}", report.score);
            ExitCode::from(9)
        }
        _ => ExitCode::SUCCESS,
    }
}

/// The audit's page facts (caged by default) and site facts: robots.txt, llms.txt and a
/// Markdown request, loaded without scripts under the same options.
fn audit_facts(
    env: &dyn Env,
    target: &str,
    session: &SessionArgs,
    prepared: &Prepared,
) -> Result<(pageon::audit::PageFacts, pageon::audit::SiteFacts), ExitCode> {
    let options = prepared.options.clone();
    let plain = LoadOptions {
        scripts: false,
        ..options.clone()
    };
    let markdown = LoadOptions {
        prefer_markdown: true,
        ..plain.clone()
    };
    if !session.no_sandbox {
        let mut sb = Sandbox::new(env, SandboxOptions::default()).map_err(broken)?;
        if let Some(s) = &prepared.state {
            sb.set_storage_state(s).map_err(broken)?;
        }
        let page = sb
            .open(target, options)
            .map_err(|e| fail(&e))?
            .audit()
            .map_err(broken)?;
        let site = site_facts(&page.url, &plain, &markdown, &mut |url, o| {
            sb.open(url, o.clone()).ok()?.audit().ok()
        });
        return Ok((page, site));
    }
    let s = Session::new(env);
    if let Some(state) = &prepared.state {
        s.set_storage_state(state);
    }
    let page = s.open(target, options).map_err(|e| fail(&e))?.audit();
    let site = site_facts(&page.url, &plain, &markdown, &mut |url, o| {
        Some(s.open(url, o.clone()).ok()?.audit())
    });
    Ok((page, site))
}

fn site_facts(
    url: &str,
    plain: &LoadOptions,
    markdown: &LoadOptions,
    open: &mut dyn FnMut(&str, &LoadOptions) -> Option<pageon::audit::PageFacts>,
) -> pageon::audit::SiteFacts {
    let Ok(u) = url::Url::parse(url) else {
        return pageon::audit::SiteFacts::default();
    };
    if !matches!(u.scheme(), "http" | "https") {
        return pageon::audit::SiteFacts::default();
    }
    let origin = u.origin().ascii_serialization();
    let path = match u.query() {
        Some(q) => format!("{}?{q}", u.path()),
        None => u.path().to_string(),
    };
    let robots = open(&format!("{origin}/robots.txt"), plain);
    let llms = open(&format!("{origin}/llms.txt"), plain);
    let md = open(url, markdown);
    pageon::audit::SiteFacts::from_loads(&path, robots.as_ref(), llms.as_ref(), md.as_ref())
}

fn screenshot_options(p: &PaintArgs, format: pageon::Format) -> pageon::ScreenshotOptions {
    pageon::ScreenshotOptions {
        area: if p.full_page {
            pageon::Area::FullPage
        } else {
            pageon::Area::Viewport
        },
        scale: p.scale,
        format,
        marks: p.marks,
        budget: None,
        mask: p.mask,
    }
}

/// Writes a painted image; its summary and notes go to stderr, its legend or provenance is
/// returned for stdout.
fn painted(
    img: Result<pageon::Image, pageon::PaintError>,
    path: &std::path::Path,
) -> Result<String, ExitCode> {
    let img = img.map_err(|e| {
        eprintln!("error: {e}\n  {}", e.guidance());
        ExitCode::from(3)
    })?;
    std::fs::write(path, &img.bytes).map_err(|e| {
        eprintln!(
            "error: cannot write {}: {e}\n  check the directory exists and is writable",
            path.display()
        );
        ExitCode::from(2)
    })?;
    eprintln!("wrote {} ({})", path.display(), img.summary());
    for n in &img.notes {
        eprintln!("  {n}");
    }
    Ok(match (img.legend, img.provenance) {
        (Some(l), _) => l,
        (None, Some(p)) => p.to_string(),
        (None, None) => String::new(),
    })
}

fn content(probe: bool) -> ExitCode {
    if probe {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        println!("{}", pageon::content::probe(&home));
        return ExitCode::SUCCESS;
    }
    match pageon::content::serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("content process: {e}");
            ExitCode::from(70)
        }
    }
}

/// The step costs of a recording, a total and p50/p95/p99 per step.
fn replay_report(file: &std::path::Path) -> ExitCode {
    let log: pageon_env::Log = match std::fs::read_to_string(file)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot read the recording {}: {e}", file.display());
            return ExitCode::from(2);
        }
    };
    let mut out = String::from("step  tokens  wall ms  page ms  action\n");
    let (mut tokens, mut wall, mut page) = (Vec::new(), Vec::new(), Vec::new());
    for (i, st) in log.steps.iter().enumerate() {
        let action = pageon_agent::quote::clip(&st.action, 70);
        match st.cost {
            Some(c) => {
                out.push_str(&format!(
                    "{i:>4}  {:>6}  {:>7}  {:>7}  {action}\n",
                    c.tokens, c.wall_ms, c.virtual_ms
                ));
                tokens.push(u64::from(c.tokens));
                wall.push(c.wall_ms);
                page.push(c.virtual_ms);
            }
            None => out.push_str(&format!(
                "{i:>4}  {:>6}  {:>7}  {:>7}  {action}\n",
                "-", "-", "-"
            )),
        }
    }
    let pct = |v: &mut Vec<u64>, p: f64| -> u64 {
        v.sort_unstable();
        // Nearest rank.
        let rank = ((p * v.len() as f64).ceil() as usize).clamp(1, v.len());
        v[rank - 1]
    };
    if tokens.is_empty() {
        out.push_str("\nno costs in this recording (recorded before pageon kept them)\n");
    } else {
        let n = tokens.len();
        out.push_str(&format!(
            "\n{} steps: {} tokens, {} ms wall, {} ms on the page's clock\n",
            n,
            tokens.iter().sum::<u64>(),
            wall.iter().sum::<u64>(),
            page.iter().sum::<u64>()
        ));
        for (name, v) in [
            ("tokens", &mut tokens),
            ("wall ms", &mut wall),
            ("page ms", &mut page),
        ] {
            out.push_str(&format!(
                "{name:<8} p50 {}  p95 {}  p99 {}  max {}\n",
                pct(v, 0.5),
                pct(v, 0.95),
                pct(v, 0.99),
                v.last().copied().unwrap_or(0)
            ));
        }
    }
    print!("{}", terminal_safe(&out));
    ExitCode::SUCCESS
}

fn replay(
    file: &std::path::Path,
    to: Option<usize>,
    live: bool,
    approve: bool,
    dump: Option<DumpArg>,
    trace: Option<&std::path::Path>,
    s: &SessionArgs,
) -> ExitCode {
    let log: pageon_env::Log = match std::fs::read_to_string(file)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "error: cannot read the recording {}: {e}\n  record one with `pageon fetch URL --act … --record FILE`",
                file.display()
            );
            return ExitCode::from(2);
        }
    };
    if log.steps.is_empty() {
        eprintln!(
            "error: {} has no steps (it was recorded without actions, or before pageon 0.2)\n  record one with `pageon fetch URL --act … --record FILE` or `pageon mcp --record FILE`",
            file.display()
        );
        return ExitCode::from(2);
    }
    let last = log.steps.len() - 1;
    let upto = to.unwrap_or(last);
    if upto > last {
        eprintln!("error: step {upto} does not exist\n  the recording has steps 0 to {last}");
        return ExitCode::from(2);
    }
    if log.user_agent != log.profile.user_agent() {
        eprintln!(
            "warning: recorded by {:?}; this is {:?} (a different version may replay differently)",
            log.user_agent,
            log.profile.user_agent()
        );
    }
    let prepared = match prepare(s) {
        Ok(p) => p,
        Err(code) => return code,
    };
    let base: Box<dyn Env> = if live {
        // The live web, as this command's session would reach it: its profile, identity and
        // proxy, not the recording's.
        match prepared.config.network() {
            Ok(l) => Box::new(l),
            Err(e) => return config_failed(&e),
        }
    } else {
        Box::new(Replay::new(log.clone()))
    };
    let steps = log.steps.clone();
    let traced = trace.map(|_| log.clone());
    let rec = match Recording::resume(log, upto, base, !live) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let kind = dump.map(dump_kind).unwrap_or(Dump::Ax);
    let quiet = to.is_some();
    let say = move |line: String| {
        if quiet {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    let job = |env: &dyn Env| -> Result<u8, ExitCode> {
        let mut progress = |i: usize, st: &pageon_env::Step| say(format!("✓ {i} {}", st.action));
        let mut run = |d: &mut dyn pageon::replay::Driver| -> Result<u8, ExitCode> {
            let ran = match (trace, &traced) {
                (Some(path), Some(log)) => {
                    match pageon::trace::write(d, log, upto, path, &mut progress) {
                        Ok(None) => Ok(()),
                        Ok(Some(div)) => Err(div),
                        Err(e) => {
                            eprintln!("error: cannot write the trace {}: {e}", path.display());
                            return Err(ExitCode::from(2));
                        }
                    }
                }
                _ => pageon::replay::rerun(d, &steps, upto, &mut progress),
            };
            if let Some(path) = trace {
                eprintln!(
                    "trace: {} (open it with `npx playwright show-trace {}`)",
                    path.display(),
                    path.display()
                );
            }
            match ran {
                Err(div) => {
                    say(pageon::replay::report(&div));
                    say(format!("diverged at step {} of {}", div.step, steps.len()));
                    Ok(5)
                }
                Ok(()) => {
                    if to.is_some() {
                        let v = d.view(kind).map_err(|e| {
                            eprintln!("error: {e}");
                            ExitCode::from(3)
                        })?;
                        println!("{v}");
                    } else {
                        say(format!("identical: {} steps", upto + 1));
                    }
                    Ok(0)
                }
            }
        };
        // Live, the operator's safety options replace the recorded ones.
        let operator = prepared.options;
        let run_as = |d: &mut dyn pageon::replay::Driver| {
            if live {
                run(&mut pageon::replay::Operator::new(d, operator))
            } else {
                run(d)
            }
        };
        if s.no_sandbox {
            let session = Session::new(env);
            if let Some(st) = &prepared.state {
                session.set_storage_state(st);
            }
            run_as(&mut pageon::replay::InProcess::new(&session))
        } else {
            let mut sb = Sandbox::new(env, SandboxOptions::default()).map_err(broken)?;
            if let Some(st) = &prepared.state {
                sb.set_storage_state(st).map_err(broken)?;
            }
            run_as(&mut pageon::replay::Caged::new(&mut sb))
        }
    };
    let mut config = prepared.config.clone().hold_writes(live);
    if approve {
        config = config.approver(ask);
    }
    match config.run_on(&rec, job) {
        Ok(Ok(code)) => ExitCode::from(code),
        Ok(Err(code)) => code,
        Err(e) => config_failed(&e),
    }
}

fn workflow(command: WorkflowCommand) -> ExitCode {
    match command {
        WorkflowCommand::Compile { recording, out } => {
            let log: pageon_env::Log = match std::fs::read_to_string(&recording)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            {
                Ok(l) => l,
                Err(e) => {
                    eprintln!(
                        "error: cannot read the recording {}: {e}\n  record one with `pageon fetch URL --act … --record FILE`",
                        recording.display()
                    );
                    return ExitCode::from(2);
                }
            };
            let wf = match pageon::workflow::compile(&log.steps) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(2);
                }
            };
            let refs = wf
                .steps
                .iter()
                .filter(|s| s.to_string().contains("(recorded ref)"))
                .count();
            if refs > 0 {
                eprintln!(
                    "warning: {refs} step(s) name a recorded ref (an older recording): they may drift"
                );
            }
            let json = serde_json::to_string_pretty(&wf).unwrap_or_default() + "\n";
            match &out {
                Some(path) => {
                    if let Err(e) = pageon_env::write_private(path, json.as_bytes()) {
                        eprintln!("error: cannot write {}: {e}", path.display());
                        return ExitCode::from(3);
                    }
                }
                None => print!("{json}"),
            }
            let params: Vec<&str> = wf.params.iter().map(|p| p.name.as_str()).collect();
            eprintln!(
                "compiled {} steps from {}{}",
                wf.steps.len(),
                wf.start,
                if params.is_empty() {
                    String::new()
                } else {
                    format!("; parameters: {}", params.join(", "))
                }
            );
            ExitCode::SUCCESS
        }
        WorkflowCommand::Run {
            file,
            params,
            session: s,
        } => {
            let wf: pageon::workflow::Workflow = match std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            {
                Ok(w) => w,
                Err(e) => {
                    eprintln!(
                        "error: cannot read the workflow {}: {e}\n  compile one with `pageon workflow compile RECORDING`",
                        file.display()
                    );
                    return ExitCode::from(2);
                }
            };
            let mut values = std::collections::HashMap::new();
            for p in &params {
                let Some((k, v)) = p.split_once('=') else {
                    eprintln!("error: {p:?}: expected NAME=VALUE");
                    return ExitCode::from(2);
                };
                values.insert(k.to_string(), v.to_string());
            }
            let prepared = match prepare(&s) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let options = prepared.options;
            let r = with_env(&prepared.config, false, false, |env| {
                let go = |d: &mut dyn pageon::replay::Driver| -> Result<u8, ExitCode> {
                    let report = pageon::workflow::run(d, &wf, &values, options, &mut |l| {
                        eprintln!("{l}");
                    })
                    .map_err(|e| {
                        eprintln!("error: {e}");
                        ExitCode::from(2)
                    })?;
                    eprintln!("{report}");
                    match &report.stop {
                        Some(stop) => {
                            println!("{}", stop.view);
                            Ok(7)
                        }
                        None => Ok(0),
                    }
                };
                if s.no_sandbox {
                    let session = Session::new(env);
                    if let Some(st) = &prepared.state {
                        session.set_storage_state(st);
                    }
                    go(&mut pageon::replay::InProcess::new(&session))
                } else {
                    let mut sb = Sandbox::new(env, SandboxOptions::default()).map_err(broken)?;
                    if let Some(st) = &prepared.state {
                        sb.set_storage_state(st).map_err(broken)?;
                    }
                    go(&mut pageon::replay::Caged::new(&mut sb))
                }
            });
            match r {
                Ok(code) => ExitCode::from(code),
                Err(code) => code,
            }
        }
    }
}

fn mcp(
    http: Option<u16>,
    allow_origins: Vec<String>,
    allow_javascript_exec: bool,
    otlp: Option<String>,
    detail: pageon_agent::Detail,
    watch: &Watch,
    s: &SessionArgs,
) -> ExitCode {
    if http.is_some() && s.save_state.is_some() {
        eprintln!(
            "error: --save-state needs pageon mcp over stdio (the HTTP server runs until it is stopped)\n  drop --http, or drop --save-state"
        );
        return ExitCode::from(2);
    }
    if allow_origins.iter().any(|o| o == "*") {
        eprintln!(
            "warning: --allow-origin '*' lets any web page you open in a browser drive pageon"
        );
    }
    let mut prepared = match prepare(s) {
        Ok(p) => p,
        Err(code) => return code,
    };
    // An agent drives this session: local files only when the operator says so.
    prepared.options.allow_files = s.allow_files;
    let sign_ins = match watch.sign_ins() {
        Ok(s) => s,
        Err(code) => return code,
    };
    let viewer = match watch.start(&mut prepared.config) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let options = pageon::mcp::McpOptions {
        sandbox: !s.no_sandbox,
        load: prepared.options,
        budget: Budget::o200k(s.budget),
        state: prepared.state.clone(),
        http: http.map(|port| pageon::mcp::HttpOptions {
            port,
            allow_origins: allow_origins.clone(),
        }),
        allow_javascript_exec,
        otlp,
        detail,
        viewer,
        sign_ins,
        ..pageon::mcp::McpOptions::default()
    };
    // The same assembly an embedder gets.
    let stdin = std::io::stdin();
    match pageon::mcp::serve_with(&prepared.config, options, stdin.lock(), std::io::stdout()) {
        Ok(state) => match save_state(s.save_state.as_ref(), &state) {
            Ok(()) => ExitCode::SUCCESS,
            Err(code) => code,
        },
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(3)
        }
    }
}

fn cdp(port: u16, allow_origins: Vec<String>, discovery: bool, s: &SessionArgs) -> ExitCode {
    if allow_origins.iter().any(|o| o == "*") {
        eprintln!(
            "warning: --allow-origin '*' lets any web page you open in a browser drive pageon"
        );
    }
    let mut prepared = match prepare(s) {
        Ok(p) => p,
        Err(code) => return code,
    };
    // Clients (and agents behind them) drive this session: local files only when allowed.
    prepared.options.allow_files = s.allow_files;
    let r = with_env(&prepared.config, false, false, |env| {
        let server = pageon::cdp::server::Server::bind(pageon::cdp::server::ServerOptions {
            port,
            allow_origins,
            discovery,
        })
        .map_err(|e| {
            eprintln!(
                "error: cannot listen on 127.0.0.1:{port}: {e}\n  pick another --port, or --port 0 for a free one"
            );
            ExitCode::from(2)
        })?;
        let options = prepared.options;
        let failed = |e: String| {
            eprintln!("error: {e}");
            ExitCode::from(3)
        };
        let state = if s.no_sandbox {
            let session = Session::new(env);
            if let Some(st) = &prepared.state {
                session.set_storage_state(st);
            }
            use pageon::cdp::server::{Input, Proto};
            let mut cdp = Some(pageon::cdp::Session::new(&session, options.clone()));
            let mut bidi: Option<pageon::bidi::Session<'_, _>> = None;
            eprintln!("DevTools listening on {}", server.ws_url());
            eprintln!("WebDriver BiDi listening on {}", server.bidi_url());
            server
                .serve(&mut |proto, input| {
                    if proto == Proto::Bidi && bidi.is_none() {
                        let c = cdp.take().unwrap_or_else(|| {
                            pageon::cdp::Session::new(&session, options.clone())
                        });
                        bidi = Some(pageon::bidi::Session::over(c));
                    }
                    Ok(match (bidi.as_mut(), cdp.as_mut(), input) {
                        (Some(b), _, Input::Message(m)) => b.handle(m),
                        (Some(b), _, Input::Idle(ms)) => b.idle(ms),
                        (None, Some(c), Input::Message(m)) => c.handle(m),
                        (None, Some(c), Input::Idle(ms)) => c.idle(ms),
                        (None, None, _) => Vec::new(),
                    })
                })
                .map_err(failed)?;
            drop(bidi);
            drop(cdp);
            session.storage_state()
        } else {
            let mut sb = Sandbox::new(env, SandboxOptions::default()).map_err(broken)?;
            if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
                eprintln!("{}", sb.receipt());
            }
            if let Some(st) = &prepared.state {
                sb.set_storage_state(st).map_err(broken)?;
            }
            eprintln!("DevTools listening on {}", server.ws_url());
            eprintln!("WebDriver BiDi listening on {}", server.bidi_url());
            server
                .serve(&mut |proto, input| {
                    use pageon::cdp::server::{Input, Proto};
                    match (proto, input) {
                        (Proto::Cdp, Input::Message(m)) => sb.cdp(m, options.clone()),
                        (Proto::Cdp, Input::Idle(ms)) => sb.cdp_idle(ms),
                        (Proto::Bidi, Input::Message(m)) => sb.bidi(m, options.clone()),
                        (Proto::Bidi, Input::Idle(ms)) => sb.bidi_idle(ms),
                    }
                    .map_err(|e| e.to_string())
                })
                .map_err(failed)?;
            sb.storage_state().map_err(broken)?
        };
        save_state(s.save_state.as_ref(), &state)
    });
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// Pages see UTC, in-process as in the content process: one zone on every machine, so replays
/// agree, and no hint of where the operator is.
#[allow(unsafe_code)]
fn pages_in_utc() {
    // SAFETY: called first in `main`, before any thread exists; nothing reads the environment
    // concurrently.
    unsafe { std::env::set_var("TZ", "UTC") };
}

fn main() -> ExitCode {
    pages_in_utc();
    let cli = Cli::parse();
    let (target, dump, viewport, acts, approve, expect, prefer_markdown, paint, session) =
        match cli.command {
            #[cfg(unix)]
            Command::Session(c) => return cli::run(&cli.session_name, c),
            Command::Content { probe } => return content(probe),
            Command::Identity { command } => return identity(command),
            Command::Workflow { command } => return workflow(command),
            Command::Mcp {
                http,
                allow_origin,
                allow_javascript_exec,
                otlp,
                detail,
                watch,
                session,
            } => {
                let detail = pageon_agent::Detail::parse(&detail).unwrap_or_default();
                return mcp(
                    http,
                    allow_origin,
                    allow_javascript_exec,
                    otlp,
                    detail,
                    &watch,
                    &session,
                );
            }
            Command::Replay {
                file,
                to,
                live,
                approve,
                dump,
                trace,
                report,
                session,
            } => {
                if report {
                    return replay_report(&file);
                }
                return replay(&file, to, live, approve, dump, trace.as_deref(), &session);
            }
            Command::Cdp {
                port,
                allow_origin,
                discovery,
                session,
            } => return cdp(port, allow_origin, discovery, &session),
            Command::Audit {
                target,
                json,
                fail_under,
                session,
            } => return audit(&target, json, fail_under, &session),
            Command::Bench {
                pages,
                runs,
                refresh,
                json,
                against,
                options,
            } => {
                return bench::run(&bench::Ask {
                    pages: &pages,
                    runs,
                    refresh,
                    json,
                    against: against.as_deref(),
                    options: &options,
                });
            }
            Command::Fetch {
                target,
                dump,
                viewport,
                acts,
                approve,
                expect,
                prefer_markdown,
                paint,
                session,
            } => (
                target,
                dump,
                viewport,
                acts,
                approve,
                expect,
                prefer_markdown,
                paint,
                session,
            ),
        };
    let mut actions = Vec::new();
    for a in &acts {
        match a.parse::<Action>() {
            Ok(x) => actions.push(x),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        }
    }
    let mut prepared = match prepare(&session) {
        Ok(p) => p,
        Err(code) => return code,
    };
    prepared.options.prefer_markdown = prefer_markdown;
    let result = with_env(&prepared.config, approve, false, |env| {
        fetch(
            env,
            Fetch {
                target: &target,
                dump,
                paint: &paint,
                viewport,
                actions,
                expect,
                budget: Budget::o200k(session.budget),
                session: &session,
                prepared: &prepared,
            },
        )
    });
    let (text, status, refusal) = match result {
        Ok(d) => d,
        Err(code) => return code,
    };
    let text = terminal_safe(&text);
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    if !text.is_empty() && !text.ends_with('\n') {
        let _ = out.write_all(b"\n");
    }
    if let Some(r) = refusal {
        eprintln!("refused: {r}\n  {}", r.guidance());
        return ExitCode::from(6);
    }
    if status >= 400 {
        eprintln!(
            "error: the server answered {status}\n  the page above is the server's error page"
        );
        return ExitCode::from(4);
    }
    ExitCode::SUCCESS
}
