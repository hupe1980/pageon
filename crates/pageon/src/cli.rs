//! Named sessions from the shell: `pageon open` starts a background pageon serving MCP on a
//! private Unix socket; every other command sends one tool call and prints the reply.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use base64::Engine as _;
use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::{SessionArgs, prepare, save_state, terminal_safe};

/// The agent skill `pageon skill` prints.
const SKILL: &str = include_str!("skill.md");

/// Options of the session itself (given to `open`, used when it starts).
#[derive(Args)]
pub struct OpenArgs {
    /// Close the session after this long without a command: minutes, or with a unit
    /// (`90s`, `2h`); 0 keeps it until `pageon close`.
    #[arg(long, value_name = "DURATION", default_value = "30", value_parser = parse_idle)]
    idle: Duration,
    /// How much views show at the start: full, or compact (no text runs).
    #[arg(long, value_name = "LEVEL", default_value = "full", value_parser = ["full", "compact"])]
    detail: String,
    #[command(flatten)]
    session: SessionArgs,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Direction {
    Down,
    Up,
    Left,
    Right,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum ViewKind {
    /// The agent view (elements with refs).
    Ax,
    /// The main content as Markdown, links with refs.
    Markdown,
    /// Readable text.
    Text,
    /// Every request, blocked ones with the reason.
    Requests,
    /// Script errors.
    Errors,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum TabAction {
    List,
    New,
    Select,
    Close,
}

/// The session commands (flattened into `pageon`'s own).
#[derive(Subcommand)]
pub enum SessionCommand {
    /// Open a page in a named session (-s), starting the session on first use.
    ///
    /// Examples:
    ///   pageon open https://example.com
    ///   pageon open https://shop.example --origin https://shop.example --secret shop@https://shop.example
    ///   pageon open http://localhost:3000 --private-network allow --origin http://localhost:3000
    ///   pageon -s b open https://other.example      # a second, separate session
    ///
    /// Session options apply when the session starts; `pageon close` ends it (writing
    /// --save-state and --record). Then: click, type, press, view, find, screenshot, …
    #[command(verbatim_doc_comment)]
    Open {
        /// An http(s) URL, a local file (with --allow-files), or download:N.
        url: String,
        #[command(flatten)]
        open: OpenArgs,
    },
    /// The session process (started by `pageon open`).
    #[command(hide = true)]
    Session {
        /// Ignored (the URL `open` was given).
        url: Option<String>,
        #[command(flatten)]
        open: OpenArgs,
    },
    /// Click an element (a ref from the view, e.g. e5).
    Click {
        /// A ref from the view, e.g. e5.
        #[arg(value_name = "REF")]
        r#ref: String,
    },
    /// Replace an element's text (`secret://NAME` types a session secret).
    Type {
        /// A ref from the view, e.g. e5.
        #[arg(value_name = "REF")]
        r#ref: String,
        /// The text, or secret://NAME.
        text: String,
        /// Press Enter afterwards.
        #[arg(long)]
        submit: bool,
    },
    /// Press a key on the focused element: Enter, Tab, Escape, ArrowDown, or one character.
    Press {
        /// Enter, Tab, Escape, Backspace, ArrowDown, or one character.
        key: String,
    },
    /// Select options of a <select> by value or label (or that text in another element).
    Select {
        /// A ref from the view, e.g. e5.
        #[arg(value_name = "REF")]
        r#ref: String,
        /// Option values or labels.
        #[arg(required = true)]
        values: Vec<String>,
    },
    /// Paste text into an element, or the focused one.
    Paste {
        /// The text to paste.
        text: String,
        /// The element (default: the focused one).
        #[arg(long, value_name = "REF")]
        r#ref: Option<String>,
    },
    /// Move the pointer onto an element.
    Hover {
        /// A ref from the view, e.g. e5.
        #[arg(value_name = "REF")]
        r#ref: String,
    },
    /// Drag an element onto another.
    Drag {
        /// A ref from the view, e.g. e5.
        #[arg(value_name = "REF")]
        r#ref: String,
        /// The ref to drop onto.
        #[arg(value_name = "TO")]
        to: String,
    },
    /// Scroll the page (or --ref's container) by a page or --pixels; with only --ref, bring
    /// it into view.
    Scroll {
        #[arg(value_enum)]
        direction: Option<Direction>,
        /// How far (default: a page).
        #[arg(long)]
        pixels: Option<f64>,
        /// The container to scroll, or the element to bring into view.
        #[arg(long, value_name = "REF")]
        r#ref: Option<String>,
    },
    /// Let time pass on the page (milliseconds).
    Wait {
        /// Milliseconds (at most 60000).
        ms: u64,
    },
    /// Go back in the tab's history.
    Back,
    /// Go forward in the tab's history.
    Forward,
    /// Put staged files (`--file NAME@ORIGIN=PATH` at open) into a file input.
    Upload {
        /// A ref from the view, e.g. e5.
        #[arg(value_name = "REF")]
        r#ref: String,
        /// Staged file names.
        #[arg(required = true, value_name = "NAME")]
        files: Vec<String>,
    },
    /// What changed since the last view (--full: the whole view).
    Observe {
        #[arg(long)]
        full: bool,
        /// How much views show from now on.
        #[arg(long, value_name = "LEVEL", value_parser = ["full", "compact"])]
        detail: Option<String>,
    },
    /// The whole page: the agent view (default), markdown, text, requests or errors.
    View {
        #[arg(value_enum, default_value = "ax")]
        kind: ViewKind,
    },
    /// Find elements by words of their role, name, value or nearby text, in the whole page.
    Find {
        /// Words to look for, e.g. delete ada.
        #[arg(required = true)]
        query: Vec<String>,
        /// At most this many (default 20).
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Write a screenshot to FILE (.png or .jpg); notes (legend, provenance) on stdout.
    Screenshot {
        /// The image file (.png, .jpg).
        file: PathBuf,
        /// Label every visible element with its ref.
        #[arg(long)]
        marks: bool,
        /// Paint over third-party frames and text a person cannot read.
        #[arg(long)]
        mask: bool,
        /// The whole page instead of the viewport.
        #[arg(long)]
        full_page: bool,
        /// Only this element.
        #[arg(long, value_name = "REF")]
        r#ref: Option<String>,
    },
    /// Tabs: list (default), new URL, select tN, close [tN].
    Tabs {
        #[arg(value_enum, default_value = "list")]
        action: TabAction,
        /// The URL (new) or the tab (select, close).
        target: Option<String>,
    },
    /// Rebuild the page exactly as at an earlier step (default: undo the last), sending
    /// nothing again.
    Reset {
        /// The step (each reply ends with `step N`); 0 is the open.
        step: Option<u64>,
    },
    /// Try actions on a fork and see the result; the page is unchanged, writes are held.
    Lookahead {
        /// Actions as text: "click e5", "type e3 ada", "press Enter".
        #[arg(required = true)]
        actions: Vec<String>,
    },
    /// Call a tool the page declares (WebMCP), with a JSON object of arguments.
    Call {
        /// The tool's name, as listed in the view.
        name: String,
        /// Its arguments, e.g. '{"query": "shoes"}'.
        #[arg(value_name = "JSON")]
        arguments: Option<String>,
        /// The tool's origin, when two frames declare the same name.
        #[arg(long)]
        origin: Option<String>,
    },
    /// Ask a person to take over (a CAPTCHA, a second factor): prints a link, waits for Done.
    Takeover {
        /// Why a person is needed, shown to them.
        reason: String,
        /// Seconds to wait (at most 3600).
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
    },
    /// End the session (or --all), writing --save-state and --record.
    Close {
        #[arg(long)]
        all: bool,
    },
    /// List the running sessions and their tabs.
    Sessions,
    /// Print the agent skill (SKILL.md), or install it with --install [DIR].
    ///
    /// Examples:
    ///   pageon skill --install                 # .claude/skills/pageon/SKILL.md
    ///   pageon skill --install ~/.codex/skills
    #[command(verbatim_doc_comment)]
    Skill {
        /// Write DIR/pageon/SKILL.md [default DIR: .claude/skills].
        #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = ".claude/skills")]
        install: Option<PathBuf>,
    },
}

fn parse_idle(s: &str) -> Result<Duration, String> {
    let (n, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => (s, "m"),
    };
    let n: u64 = n
        .parse()
        .map_err(|_| format!("{s:?}: expected minutes, or 90s, 30m, 2h"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return Err(format!("{s:?}: expected minutes, or 90s, 30m, 2h")),
    };
    Ok(Duration::from_secs(secs))
}

/// Runs a session command for session `name`.
pub fn run(name: &str, command: SessionCommand) -> ExitCode {
    if let Err(e) = check_name(name) {
        eprintln!("error: {e}");
        return ExitCode::from(2);
    }
    match command {
        SessionCommand::Skill { install } => return skill(install),
        SessionCommand::Sessions => return sessions(),
        SessionCommand::Close { all } => return close(name, all),
        SessionCommand::Session { open, .. } => return session(name, &open),
        _ => {}
    }
    let dir = match dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let (tool, args, image) = match tool_call(command) {
        Ok(Some(call)) => call,
        Ok(None) => return ExitCode::SUCCESS,
        Err(code) => return code,
    };
    let sock = dir.join(format!("{name}.sock"));
    // Unix socket paths are short (104 bytes on macOS, 108 on Linux).
    if sock.as_os_str().len() > 103 {
        eprintln!(
            "error: {} is too long for a socket ({} bytes, at most 103)\n  set PAGEON_SESSIONS to a shorter private directory",
            sock.display(),
            sock.as_os_str().len()
        );
        return ExitCode::from(2);
    }
    if tool == "browser_open"
        && let Err(code) = ensure(name, &dir, &sock)
    {
        return code;
    }
    let stream = match connect(&sock) {
        Some(s) => s,
        None => {
            let s = if name == "default" {
                String::new()
            } else {
                format!("-s {name} ")
            };
            eprintln!("error: no session {name:?}\n  start one with: pageon {s}open URL");
            return ExitCode::from(2);
        }
    };
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
    "name": tool, "arguments": args,
    "_meta": {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "progressToken": 1,
    }}});
    match exchange(stream, &msg) {
        Some(reply) => print_reply(&reply, image.as_deref()),
        None => {
            eprintln!(
                "error: session {name:?} ended without answering\n  see {}",
                dir.join(format!("{name}.log")).display()
            );
            ExitCode::from(3)
        }
    }
}

/// The tool and arguments a command calls (`None`: nothing to call), and for screenshots the
/// file the image goes to.
#[allow(clippy::type_complexity)]
fn tool_call(
    c: SessionCommand,
) -> Result<Option<(&'static str, Value, Option<PathBuf>)>, ExitCode> {
    let mut image = None;
    let (tool, args) = match c {
        SessionCommand::Open { url, .. } => ("browser_open", json!({"url": url})),
        SessionCommand::Click { r#ref } => ("browser_click", json!({"ref": r#ref})),
        SessionCommand::Type {
            r#ref,
            text,
            submit,
        } => (
            "browser_type",
            json!({"ref": r#ref, "text": text, "submit": submit}),
        ),
        SessionCommand::Press { key } => ("browser_press", json!({"key": key})),
        SessionCommand::Select { r#ref, values } => {
            ("browser_select", json!({"ref": r#ref, "values": values}))
        }
        SessionCommand::Paste { text, r#ref } => {
            let mut a = json!({"text": text});
            if let Some(r) = r#ref {
                a["ref"] = json!(r);
            }
            ("browser_paste", a)
        }
        SessionCommand::Hover { r#ref } => ("browser_hover", json!({"ref": r#ref})),
        SessionCommand::Drag { r#ref, to } => ("browser_drag", json!({"ref": r#ref, "to": to})),
        SessionCommand::Scroll {
            direction,
            pixels,
            r#ref,
        } => {
            let mut a = json!({});
            if let Some(d) = direction {
                a["direction"] = json!(match d {
                    Direction::Down => "down",
                    Direction::Up => "up",
                    Direction::Left => "left",
                    Direction::Right => "right",
                });
            }
            if let Some(p) = pixels {
                a["pixels"] = json!(p);
            }
            if let Some(r) = r#ref {
                a["ref"] = json!(r);
            }
            ("browser_scroll", a)
        }
        SessionCommand::Wait { ms } => ("browser_wait", json!({"ms": ms})),
        SessionCommand::Back => ("browser_back", json!({})),
        SessionCommand::Forward => ("browser_forward", json!({})),
        SessionCommand::Upload { r#ref, files } => {
            ("browser_upload", json!({"ref": r#ref, "files": files}))
        }
        SessionCommand::Observe { full, detail } => {
            let mut a = json!({"full": full});
            if let Some(d) = detail {
                a["detail"] = json!(d);
            }
            ("browser_observe", a)
        }
        SessionCommand::View { kind } => match kind {
            ViewKind::Ax => ("browser_observe", json!({"full": true})),
            ViewKind::Markdown => ("browser_view", json!({"kind": "markdown"})),
            ViewKind::Text => ("browser_view", json!({"kind": "text"})),
            ViewKind::Requests => ("browser_view", json!({"kind": "requests"})),
            ViewKind::Errors => ("browser_view", json!({"kind": "errors"})),
        },
        SessionCommand::Find { query, limit } => {
            let mut a = json!({"query": query.join(" ")});
            if let Some(l) = limit {
                a["limit"] = json!(l);
            }
            ("browser_find", a)
        }
        SessionCommand::Screenshot {
            file,
            marks,
            mask,
            full_page,
            r#ref,
        } => {
            let ext = file
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase);
            let format = match ext.as_deref() {
                Some("png") => "png",
                Some("jpg" | "jpeg") => "jpeg",
                _ => {
                    eprintln!("error: {}: name a .png or .jpg file", file.display());
                    return Err(ExitCode::from(2));
                }
            };
            let mut a =
                json!({"marks": marks, "mask": mask, "full_page": full_page, "format": format});
            if let Some(r) = r#ref {
                a["ref"] = json!(r);
            }
            image = Some(file);
            ("browser_screenshot", a)
        }
        SessionCommand::Tabs { action, target } => {
            let a = match (action, target) {
                (TabAction::List, _) => json!({"action": "list"}),
                (TabAction::New, Some(url)) => json!({"action": "new", "url": url}),
                (TabAction::New, None) => {
                    eprintln!("error: tabs new needs a URL\n  pageon tabs new https://example.com");
                    return Err(ExitCode::from(2));
                }
                (TabAction::Select, Some(t)) => json!({"action": "select", "tab": t}),
                (TabAction::Select, None) => {
                    eprintln!(
                        "error: tabs select needs a tab\n  pageon tabs select t2 (see pageon tabs)"
                    );
                    return Err(ExitCode::from(2));
                }
                (TabAction::Close, t) => {
                    let mut a = json!({"action": "close"});
                    if let Some(t) = t {
                        a["tab"] = json!(t);
                    }
                    a
                }
            };
            ("browser_tabs", a)
        }
        SessionCommand::Reset { step } => {
            let mut a = json!({});
            if let Some(s) = step {
                a["step"] = json!(s);
            }
            ("browser_reset", a)
        }
        SessionCommand::Lookahead { actions } => ("browser_lookahead", json!({"actions": actions})),
        SessionCommand::Call {
            name,
            arguments,
            origin,
        } => {
            let mut a = json!({"name": name});
            if let Some(text) = arguments {
                match serde_json::from_str::<Value>(&text) {
                    Ok(v @ Value::Object(_)) => a["arguments"] = v,
                    _ => {
                        eprintln!(
                            "error: the arguments are not a JSON object\n  pageon call {name} '{{\"query\": \"shoes\"}}'"
                        );
                        return Err(ExitCode::from(2));
                    }
                }
            }
            if let Some(o) = origin {
                a["origin"] = json!(o);
            }
            ("browser_call_tool", a)
        }
        SessionCommand::Takeover { reason, timeout } => {
            let mut a = json!({"reason": reason});
            if let Some(t) = timeout {
                a["timeout_s"] = json!(t);
            }
            ("browser_takeover", a)
        }
        SessionCommand::Session { .. }
        | SessionCommand::Close { .. }
        | SessionCommand::Sessions
        | SessionCommand::Skill { .. } => return Ok(None),
    };
    Ok(Some((tool, args, image)))
}

/// Prints a reply: text to stdout (or stderr for errors), an image to its file.
fn print_reply(reply: &Value, image: Option<&Path>) -> ExitCode {
    if let Some(e) = reply.get("error") {
        eprintln!(
            "error: {}",
            e["message"].as_str().unwrap_or("invalid request")
        );
        return ExitCode::from(2);
    }
    let result = &reply["result"];
    let failed = result["isError"].as_bool().unwrap_or(false);
    let mut text = String::new();
    for c in result["content"].as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("text") => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(c["text"].as_str().unwrap_or_default());
            }
            Some("image") => {
                let Some(path) = image else { continue };
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(c["data"].as_str().unwrap_or_default())
                    .unwrap_or_default();
                if let Err(e) = std::fs::write(path, bytes) {
                    eprintln!("error: {}: {e}", path.display());
                    return ExitCode::from(3);
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("wrote {}", path.display()));
            }
            _ => {}
        }
    }
    if failed {
        // The CLI's own shape: the error, then what to do, indented.
        let e = &result["structuredContent"]["error"];
        if let (Some(m), Some(g)) = (e["message"].as_str(), e["guidance"].as_str()) {
            eprintln!("error: {}\n  {}", terminal_safe(m), terminal_safe(g));
        } else {
            eprintln!("{}", terminal_safe(&text).trim_end());
        }
        return ExitCode::from(3);
    }
    let text = terminal_safe(&text);
    let text = text.trim_end();
    if !text.is_empty() {
        println!("{text}");
    }
    ExitCode::SUCCESS
}

/// Sends one request line and returns the reply, printing progress on stderr.
fn exchange(mut stream: UnixStream, msg: &Value) -> Option<Value> {
    let mut line = serde_json::to_vec(msg).ok()?;
    line.push(b'\n');
    stream.write_all(&line).ok()?;
    stream.shutdown(std::net::Shutdown::Write).ok()?;
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return None };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("method").is_some() {
            if let Some(m) = v.pointer("/params/message").and_then(Value::as_str) {
                eprintln!("{}", terminal_safe(m));
            }
            continue;
        }
        return Some(v);
    }
    None
}

fn connect(sock: &Path) -> Option<UnixStream> {
    UnixStream::connect(sock).ok()
}

/// Session names: letters, digits, `_` and `-`, 1 to 32 of them (never a path).
fn check_name(name: &str) -> Result<(), String> {
    if (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Ok(())
    } else {
        Err(format!(
            "{name:?} is not a session name\n  use 1 to 32 letters, digits, _ or -, e.g. -s shop"
        ))
    }
}

/// The sessions directory, created 0700; refused when another user owns it or can open it.
fn dir() -> Result<PathBuf, String> {
    let dir = match std::env::var_os("PAGEON_SESSIONS") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(d) if !d.is_empty() => PathBuf::from(d).join("pageon"),
            _ => {
                let home = std::env::var_os("HOME")
                    .ok_or("HOME is not set\n  set PAGEON_SESSIONS to a private directory")?;
                PathBuf::from(home).join(".pageon").join("sessions")
            }
        },
    };
    if !dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!(
            "{} is not a directory\n  set PAGEON_SESSIONS to a private directory",
            dir.display()
        ));
    }
    if meta.uid() != uid() || meta.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is open to other users (mode {:o}, owner {})\n  chmod 700 it, or set PAGEON_SESSIONS to a directory only you can open",
            dir.display(),
            meta.mode() & 0o777,
            meta.uid()
        ));
    }
    Ok(dir)
}

#[allow(unsafe_code)]
fn uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Starts the session when none answers; warns when it runs with other options.
fn ensure(name: &str, dir: &Path, sock: &Path) -> Result<(), ExitCode> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let (at, url) = open_position(&argv).ok_or(ExitCode::from(2))?;
    let options: Vec<String> = argv[at + 1..]
        .iter()
        .filter(|a| **a != url)
        .cloned()
        .collect();
    let args_file = dir.join(format!("{name}.args"));
    if connect(sock).is_some() {
        let before: Vec<String> = std::fs::read_to_string(&args_file)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if !options.is_empty() && options != before {
            eprintln!(
                "warning: session {name:?} is running with its own options; these are ignored\n  pageon{} close, then open again to change them",
                if name == "default" {
                    String::new()
                } else {
                    format!(" -s {name}")
                }
            );
        }
        return Ok(());
    }
    // A socket nobody answers: the session died.
    let _ = std::fs::remove_file(sock);
    let log = dir.join(format!("{name}.log"));
    let started = (|| -> std::io::Result<std::process::Child> {
        write_private(
            &args_file,
            serde_json::to_string(&options)
                .unwrap_or_default()
                .as_bytes(),
        )?;
        let err = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&log)?;
        let mut cmd = std::process::Command::new(std::env::current_exe()?);
        let mut session_argv = argv.clone();
        session_argv[at] = "session".into();
        cmd.args(&session_argv)
            .env("PAGEON_SESSION", name)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(err)
            .process_group(0);
        cmd.spawn()
    })();
    let mut child = match started {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: cannot start session {name:?}: {e}");
            return Err(ExitCode::from(3));
        }
    };
    for _ in 0..750 {
        if connect(sock).is_some() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let tail: Vec<&str> = text.lines().rev().take(8).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            eprintln!(
                "{}\nerror: session {name:?} did not start ({status})",
                terminal_safe(&tail.join("\n"))
            );
            let code = status.code().unwrap_or(3);
            return Err(ExitCode::from(u8::try_from(code).unwrap_or(3)));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!(
        "error: session {name:?} did not answer within 15 s\n  see {}",
        log.display()
    );
    Err(ExitCode::from(3))
}

/// The position of the `open` subcommand in `argv` (without the program) and its URL.
fn open_position(argv: &[String]) -> Option<(usize, String)> {
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == "-s" || a == "--session" {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        if a != "open" {
            return None;
        }
        // The URL is the first positional after `open` (no value of an option).
        let url = argv[i + 1..]
            .iter()
            .scan(false, |value_next, t| {
                let positional = !*value_next && !t.starts_with('-');
                *value_next =
                    t == "-s" || (t.starts_with("--") && !t.contains('=') && takes_value(t));
                Some((positional, t))
            })
            .find(|(p, _)| *p)
            .map(|(_, t)| t.clone())?;
        return Some((i, url));
    }
    None
}

/// Whether a long option of `open` takes a value (the flags that do not are listed).
fn takes_value(flag: &str) -> bool {
    !matches!(
        flag,
        "--no-js" | "--no-sandbox" | "--allow-files" | "--help"
    )
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    f.write_all(bytes)
}

/// The session process: MCP on the socket until closed or idle, then state and recording.
fn session(name: &str, open: &OpenArgs) -> ExitCode {
    let dir = match dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let s = &open.session;
    let mut prepared = match prepare(s) {
        Ok(p) => p,
        Err(code) => return code,
    };
    prepared.options.allow_files = s.allow_files;
    let options = pageon::mcp::McpOptions {
        sandbox: !s.no_sandbox,
        load: prepared.options,
        budget: pageon::Budget::o200k(s.budget),
        state: prepared.state.clone(),
        socket: Some(pageon::mcp::SocketOptions {
            path: dir.join(format!("{name}.sock")),
            idle: (!open.idle.is_zero()).then_some(open.idle),
        }),
        detail: pageon_agent::Detail::parse(&open.detail).unwrap_or_default(),
        ..pageon::mcp::McpOptions::default()
    };
    eprintln!("session {name:?} started (pid {})", std::process::id());
    let r = pageon::mcp::serve_with(&prepared.config, options, std::io::empty(), std::io::sink());
    let code = match r {
        Ok(state) => match save_state(s.save_state.as_ref(), &state) {
            Ok(()) => {
                eprintln!("session {name:?} closed");
                ExitCode::SUCCESS
            }
            Err(code) => code,
        },
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(3)
        }
    };
    // Last: `close` waits for this file to go, so state and recording are on disk by then.
    let _ = std::fs::remove_file(dir.join(format!("{name}.args")));
    code
}

/// Closes one session (or all), waiting until each has written its state and recording.
fn close(name: &str, all: bool) -> ExitCode {
    let dir = match dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let names = if all {
        running(&dir)
    } else {
        vec![name.to_string()]
    };
    let mut missing = false;
    for n in &names {
        let sock = dir.join(format!("{n}.sock"));
        let Some(stream) = connect(&sock) else {
            if !all {
                eprintln!("error: no session {n:?}");
                missing = true;
            }
            let _ = std::fs::remove_file(&sock);
            let _ = std::fs::remove_file(dir.join(format!("{n}.args")));
            continue;
        };
        let _ = exchange(
            stream,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "pageon/close"}),
        );
        // The session removes its options file once state and recording are written.
        let args = dir.join(format!("{n}.args"));
        for _ in 0..1500 {
            if !sock.exists() && !args.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        println!("closed {n}");
    }
    if missing {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

/// Names of the sessions with a socket in `dir`.
fn running(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".sock")
                .map(str::to_string)
        })
        .filter(|n| check_name(n).is_ok())
        .collect();
    names.sort();
    names
}

/// Lists running sessions with their tabs; removes the sockets of dead ones.
fn sessions() -> ExitCode {
    let dir = match dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let mut any = false;
    for n in running(&dir) {
        let sock = dir.join(format!("{n}.sock"));
        let Some(stream) = connect(&sock) else {
            let _ = std::fs::remove_file(&sock);
            continue;
        };
        any = true;
        let reply = exchange(
            stream,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "pageon/status"}),
        );
        let tabs = reply
            .as_ref()
            .and_then(|r| r.pointer("/result/content/0/text"))
            .and_then(Value::as_str)
            .unwrap_or("(busy)");
        println!("{n}");
        for line in tabs.lines() {
            println!("  {}", terminal_safe(line));
        }
    }
    if !any {
        println!("no sessions; start one with: pageon open URL");
    }
    ExitCode::SUCCESS
}

fn skill(install: Option<PathBuf>) -> ExitCode {
    let Some(dir) = install else {
        print!("{SKILL}");
        return ExitCode::SUCCESS;
    };
    let dir = dir.join("pageon");
    let path = dir.join("SKILL.md");
    match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, SKILL)) {
        Ok(()) => {
            println!("wrote {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {}: {e}", path.display());
            ExitCode::from(3)
        }
    }
}
