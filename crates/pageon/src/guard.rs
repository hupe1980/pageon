//! A guard as a program: any command — a script, a local model, a remote critic —
//! decides about effects. pageon writes one JSON [`Effect`] per line to its stdin and reads one
//! JSON verdict per line from its stdout: `{"verdict": "allow"}`, `{"verdict": "deny", "reason":
//! "…"}` or `{"verdict": "ask"}`. It fails closed: a crash, malformed output or no answer within
//! the timeout denies, from then on.

use std::cell::RefCell;
use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use crate::policy::{Effect, Verdict};

/// How long a guard may take to answer.
pub const GUARD_TIMEOUT: Duration = Duration::from_secs(10);

enum State {
    Idle,
    Running {
        child: Child,
        stdin: ChildStdin,
        lines: Receiver<String>,
    },
    Failed(String),
}

/// A guard program, started on the first effect (`sh -c COMMAND`).
///
/// ```no_run
/// use pageon::{CommandGuard, Live, Policy};
///
/// let guard = CommandGuard::new("python3 critic.py");
/// let env = Policy::new(Live::default()).guard(move |e| guard.verdict(e));
/// ```
pub struct CommandGuard {
    command: String,
    timeout: Duration,
    state: RefCell<State>,
}

impl std::fmt::Debug for CommandGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandGuard")
            .field("command", &self.command)
            .finish_non_exhaustive()
    }
}

#[derive(serde::Deserialize)]
struct Answer {
    verdict: String,
    #[serde(default)]
    reason: Option<String>,
}

impl CommandGuard {
    /// A guard running `command` through the shell, answering within [`GUARD_TIMEOUT`].
    pub fn new(command: impl Into<String>) -> Self {
        Self::with_timeout(command, GUARD_TIMEOUT)
    }

    /// The same, with another timeout.
    pub fn with_timeout(command: impl Into<String>, timeout: Duration) -> Self {
        Self {
            command: command.into(),
            timeout,
            state: RefCell::new(State::Idle),
        }
    }

    fn start(&self) -> Result<State, String> {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&self.command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("could not start {:?}: {e}", self.command))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("no stdin or stdout".into());
        };
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(State::Running {
            child,
            stdin,
            lines,
        })
    }

    fn ask(&self, effect: &Effect) -> Result<Verdict, String> {
        let mut state = self.state.borrow_mut();
        if let State::Failed(why) = &*state {
            return Err(why.clone());
        }
        if matches!(*state, State::Idle) {
            *state = self.start()?;
        }
        let State::Running { stdin, lines, .. } = &mut *state else {
            return Err("the guard is not running".into());
        };
        let line = serde_json::to_string(effect).map_err(|e| e.to_string())?;
        writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("the guard stopped reading ({e})"))?;
        let answer = match lines.recv_timeout(self.timeout) {
            Ok(a) => a,
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!("no answer within {} s", self.timeout.as_secs()));
            }
            Err(RecvTimeoutError::Disconnected) => return Err("the guard exited".into()),
        };
        let a: Answer = serde_json::from_str(answer.trim())
            .map_err(|e| format!("not a verdict ({e}): {answer:?}"))?;
        match a.verdict.as_str() {
            "allow" => Ok(Verdict::Allow),
            "ask" => Ok(Verdict::Ask),
            "deny" => Ok(Verdict::Deny(
                a.reason.unwrap_or_else(|| "denied by the guard".into()),
            )),
            other => Err(format!("unknown verdict {other:?}")),
        }
    }

    /// The guard's verdict on `effect`; a failure denies, now and for every later effect.
    pub fn verdict(&self, effect: &Effect) -> Verdict {
        match self.ask(effect) {
            Ok(v) => v,
            Err(why) => {
                let old =
                    std::mem::replace(&mut *self.state.borrow_mut(), State::Failed(why.clone()));
                if let State::Running { mut child, .. } = old {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Verdict::Deny(format!("guard failed: {why}"))
            }
        }
    }
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        if let State::Running { mut child, .. } =
            std::mem::replace(&mut *self.state.borrow_mut(), State::Idle)
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
