//! The content process: page code runs here, in a procage cage, and reaches the outside world
//! (network, clock, randomness, secrets) only through the broker on the other end of its
//! standard input and output (see [`crate::Sandbox`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::rc::Rc;

use pageon_env::{Env, NetError, Request, Response};

use crate::protocol::{self, EnvCall, EnvReply, FromContent, Op, ProtocolError, ToContent};
use crate::{Session, Tab, Viewport};

/// The channel to the broker.
struct Channel {
    input: RefCell<Box<dyn BufRead>>,
    output: RefCell<Box<dyn Write>>,
}

/// `Env` served by the broker.
struct BrokerEnv {
    channel: Rc<Channel>,
    /// Whether the broker keeps steps (asked once).
    records: std::cell::Cell<Option<bool>>,
    /// The broker's identity profile (asked once).
    profile: std::cell::Cell<Option<pageon_env::Profile>>,
}

impl BrokerEnv {
    fn ask(&self, call: EnvCall) -> Result<EnvReply, NetError> {
        let broken = |e: ProtocolError| NetError::Network(format!("broker: {e}"));
        protocol::send(
            &mut *self.channel.output.borrow_mut(),
            &FromContent::Env(call),
        )
        .map_err(broken)?;
        match protocol::recv::<ToContent>(&mut *self.channel.input.borrow_mut()).map_err(broken)? {
            ToContent::Reply(r) => Ok(r),
            // A lookahead branch reached a held request: it ends here, without unwinding.
            ToContent::Stop => end_branch(BRANCH_STOPPED),
            ToContent::Cmd { .. } => Err(NetError::Network(
                "broker: a command arrived while an answer was expected".into(),
            )),
        }
    }
}

/// `MADV_DOFORK` over `[start, end)`. Linux applies advice to each mapping of a range and skips
/// unmapped gaps (answering ENOMEM), but stops at a mapping that refuses it (EINVAL: vvar and
/// other special mappings): such ranges are split until only the refusing mapping is left out.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn inherit_all(start: usize, end: usize) {
    // SAFETY: MADV_DOFORK only changes which mappings a fork inherits; no memory changes.
    let r = unsafe { libc::madvise(start as *mut libc::c_void, end - start, libc::MADV_DOFORK) };
    if r == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL) {
        return;
    }
    if end - start <= 4096 {
        return;
    }
    let mid = (start + (end - start) / 2) & !4095;
    inherit_all(start, mid);
    inherit_all(mid, end);
}

/// The exit status of a lookahead branch stopped at a held request.
const BRANCH_STOPPED: i32 = 3;

/// Ends a lookahead branch: no destructors, no V8 teardown, nothing more on the channel.
#[allow(unsafe_code)]
fn end_branch(status: i32) -> ! {
    // SAFETY: `_exit` only ends the process; it is async-signal-safe and runs no handlers.
    unsafe { libc::_exit(status) }
}

/// Runs a lookahead: forks this single-threaded process at the command loop. The
/// copy runs the actions, observes, answers on the inherited channel and ends; this process
/// waits meanwhile (the protocol is synchronous, so one process speaks at a time) and is
/// unchanged. `Ok(None)`: the copy answered; `Ok(Some(Null))`: it was stopped at a held
/// request; `Err`: it failed.
#[allow(unsafe_code)]
fn lookahead(
    tab: &mut Tab<'_, BrokerEnv>,
    actions: Vec<crate::Action>,
    id: u64,
    say: &dyn Fn(&FromContent) -> Result<(), String>,
) -> Result<Option<serde_json::Value>, String> {
    // The pinned rusty_v8 is built with `v8_enable_private_mapping_fork_optimization`: on Linux
    // V8 marks its mappings `MADV_DONTFORK`, and a child would have no heap. Mark every mapping
    // inheritable again (this process never spawns programs, so nothing is lost).
    #[cfg(target_os = "linux")]
    inherit_all(4096, 1 << 48);
    // SAFETY: the content process has one thread here (single-threaded V8 platform, the
    // watchdog only lives while a script runs), so the child is a complete copy.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(format!(
            "could not fork the content process: {}",
            std::io::Error::last_os_error()
        ));
    }
    if pid == 0 {
        let mut result = crate::Lookahead::default();
        for a in actions {
            match tab.act(a) {
                Ok(o) => {
                    let _ = say(&FromContent::Ran(Box::new(o.clone())));
                    result.outcomes.push(o);
                }
                Err(e) => {
                    result.error = Some(e);
                    break;
                }
            }
        }
        result.observation = Some(tab.observe());
        let sent = say(&FromContent::Done {
            id,
            value: json(&result),
        });
        end_branch(i32::from(sent.is_err()));
    }
    let mut status = 0;
    loop {
        // SAFETY: waits for the child this function forked.
        let r = unsafe { libc::waitpid(pid, &mut status, 0) };
        if r == pid {
            break;
        }
        if r < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(format!(
                "lost the lookahead branch: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    if libc::WIFEXITED(status) {
        match libc::WEXITSTATUS(status) {
            0 => return Ok(None),
            BRANCH_STOPPED => return Ok(Some(serde_json::Value::Null)),
            _ => {}
        }
    }
    Err(if libc::WIFSIGNALED(status) {
        format!(
            "the lookahead branch crashed (signal {}); this page is unchanged",
            libc::WTERMSIG(status)
        )
    } else {
        format!(
            "the lookahead branch failed (exit {}); this page is unchanged",
            libc::WEXITSTATUS(status)
        )
    })
}

fn unexpected<T>() -> Result<T, NetError> {
    Err(NetError::Network("broker: unexpected answer".into()))
}

impl Env for BrokerEnv {
    fn fetch(&self, req: &Request) -> Result<Response, NetError> {
        match self.ask(EnvCall::Fetch(req.clone()))? {
            EnvReply::Fetch(r) => r,
            _ => unexpected(),
        }
    }

    fn now_ms(&self) -> Result<u64, NetError> {
        match self.ask(EnvCall::Now)? {
            EnvReply::Num(r) => r,
            _ => unexpected(),
        }
    }

    fn random_u64(&self) -> Result<u64, NetError> {
        match self.ask(EnvCall::Random)? {
            EnvReply::Num(r) => r,
            _ => unexpected(),
        }
    }

    fn secret(&self, name: &str, origin: &str) -> Result<String, NetError> {
        match self.ask(EnvCall::Secret {
            name: name.into(),
            origin: origin.into(),
        })? {
            EnvReply::Secret(r) => r,
            _ => unexpected(),
        }
    }

    fn file(&self, name: &str, origin: &str) -> Result<pageon_env::StagedFile, NetError> {
        match self.ask(EnvCall::File {
            name: name.into(),
            origin: origin.into(),
        })? {
            EnvReply::File(r) => r,
            _ => unexpected(),
        }
    }

    fn recorder(&self) -> Option<&dyn pageon_env::Recorder> {
        let records = match self.records.get() {
            Some(r) => r,
            None => {
                let r = matches!(self.ask(EnvCall::Records), Ok(EnvReply::Flag(true)));
                self.records.set(Some(r));
                r
            }
        };
        records.then_some(self as &dyn pageon_env::Recorder)
    }

    fn profile(&self) -> pageon_env::Profile {
        if let Some(p) = self.profile.get() {
            return p;
        }
        let p = match self.ask(EnvCall::Profile) {
            Ok(EnvReply::Profile(p)) => p,
            _ => pageon_env::Profile::Agent,
        };
        self.profile.set(Some(p));
        p
    }
}

/// The broker keeps the recording; the content process only reports steps to it.
impl pageon_env::Recorder for BrokerEnv {
    fn step(&self, step: pageon_env::Step) {
        let _ = self.ask(EnvCall::Step(Box::new(step)));
    }

    fn steps(&self) -> Vec<pageon_env::Step> {
        Vec::new()
    }

    fn len(&self) -> usize {
        0
    }

    fn rewind(&self, _k: usize) -> Result<(), String> {
        Err("resets are driven by the broker".into())
    }

    fn cancel(&self) {}

    fn divergence(&self) -> Option<pageon_env::Divergence> {
        None
    }

    fn replaying(&self) -> bool {
        false
    }
}

fn json<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

/// Serves the broker on standard input and output until it closes the channel.
pub fn serve() -> Result<(), String> {
    serve_on(
        Box::new(BufReader::new(std::io::stdin())),
        Box::new(std::io::stdout()),
    )
}

fn serve_on(input: Box<dyn BufRead>, output: Box<dyn Write>) -> Result<(), String> {
    let channel = Rc::new(Channel {
        input: RefCell::new(input),
        output: RefCell::new(output),
    });
    let say = |msg: &FromContent| {
        protocol::send(&mut *channel.output.borrow_mut(), msg).map_err(|e| e.to_string())
    };
    say(&FromContent::Hello {
        pageon: protocol::VERSION,
    })?;
    let session = Session::new(BrokerEnv {
        channel: channel.clone(),
        records: std::cell::Cell::new(None),
        profile: std::cell::Cell::new(None),
    });
    let mut tabs: HashMap<u32, Tab<'_, BrokerEnv>> = HashMap::new();
    let mut cdp: Option<crate::cdp::Session<'_, BrokerEnv>> = None;
    let mut bidi: Option<crate::bidi::Session<'_, BrokerEnv>> = None;
    let mut next = 1u32;
    loop {
        let msg = match protocol::recv::<ToContent>(&mut *channel.input.borrow_mut()) {
            Ok(m) => m,
            Err(ProtocolError::Closed) => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let ToContent::Cmd { id, op } = msg else {
            return Err("an answer arrived while no question was asked".into());
        };
        let no_tab = |t: u32| format!("no tab {t}");
        let value: Result<serde_json::Value, String> = match op {
            Op::Open { url, options } => Ok(match session.open(&url, options) {
                Ok(tab) => {
                    let t = next;
                    next += 1;
                    let opened = tab.opened().clone();
                    tabs.insert(t, tab);
                    json(&Ok::<_, crate::LoadError>((t, opened)))
                }
                Err(e) => json(&Err::<(u32, crate::Outcome), _>(e)),
            }),
            Op::Clear => {
                tabs.clear();
                cdp = None;
                bidi = None;
                session.clear();
                Ok(serde_json::Value::Null)
            }
            Op::Act {
                tab,
                action,
                person,
            } => tabs
                .get_mut(&tab)
                .map(|t| {
                    json(&if person {
                        t.act_as_person(action)
                    } else {
                        t.act(action)
                    })
                })
                .ok_or_else(|| no_tab(tab)),
            Op::Observe { tab, full } => tabs
                .get_mut(&tab)
                .map(|t| json(&if full { t.observe_full() } else { t.observe() }))
                .ok_or_else(|| no_tab(tab)),
            Op::View {
                tab,
                kind,
                width,
                height,
                budget,
            } => tabs
                .get(&tab)
                .map(|t| json(&t.view(kind, Viewport { width, height }, budget)))
                .ok_or_else(|| no_tab(tab)),
            Op::Screenshot { tab, options } => tabs
                .get_mut(&tab)
                .map(|t| json(&t.screenshot(&options)))
                .ok_or_else(|| no_tab(tab)),
            Op::Crop {
                tab,
                reference,
                margin,
                format,
                scale,
            } => tabs
                .get_mut(&tab)
                .map(|t| json(&t.crop(&reference, margin, format, scale)))
                .ok_or_else(|| no_tab(tab)),
            Op::Status { tab } => tabs
                .get(&tab)
                .map(|t| {
                    json(&(
                        t.status_line(),
                        t.document().status(),
                        t.document().refusal(),
                    ))
                })
                .ok_or_else(|| no_tab(tab)),
            Op::Url { tab } => tabs
                .get(&tab)
                .map(|t| json(&t.url()))
                .ok_or_else(|| no_tab(tab)),
            Op::Close { tab } => {
                tabs.remove(&tab);
                Ok(serde_json::Value::Null)
            }
            Op::GetState => Ok(json(&session.storage_state())),
            Op::SetState { state } => {
                session.set_storage_state(&state);
                Ok(serde_json::Value::Null)
            }
            Op::Probe { home } => Ok(json(&probe(&home))),
            Op::CdpIdle { elapsed_ms } => Ok(json(
                &cdp.as_mut().map(|c| c.idle(elapsed_ms)).unwrap_or_default(),
            )),
            Op::Lookahead { tab, actions } => match tabs.get_mut(&tab) {
                None => Err(no_tab(tab)),
                Some(t) => match lookahead(t, actions, id, &say) {
                    // The branch answered the broker itself.
                    Ok(None) => continue,
                    Ok(Some(v)) => Ok(v),
                    Err(e) => Err(e),
                },
            },
            Op::Cdp { message, options } => Ok(json(
                &cdp.get_or_insert_with(|| crate::cdp::Session::new(&session, options))
                    .handle(&message),
            )),
            Op::Bidi { message, options } => {
                let b = match bidi.as_mut() {
                    Some(b) => b,
                    None => bidi.insert(crate::bidi::Session::over(
                        cdp.take()
                            .unwrap_or_else(|| crate::cdp::Session::new(&session, options)),
                    )),
                };
                Ok(json(&b.handle(&message)))
            }
            Op::BidiIdle { elapsed_ms } => Ok(json(
                &bidi
                    .as_mut()
                    .map(|b| b.idle(elapsed_ms))
                    .unwrap_or_default(),
            )),
        };
        match value {
            Ok(value) => say(&FromContent::Done { id, value })?,
            Err(message) => say(&FromContent::Failed { id, message })?,
        }
    }
}

/// Tries what a caged content process must not be able to do, and reports each result as
/// `name=ok|denied` (the cage self-test). `home` is the user's home directory (the broker's;
/// the cage does not pass `HOME` on).
pub fn probe(home: &str) -> String {
    let mut out = Vec::new();
    let mut engine = pageon_js::Engine::new(0, 1, pageon_js::Limits::default());
    let dom = pageon_dom::parse_document("<p>x</p>", pageon_dom::ParseOptions::default());
    let realm = engine.add_realm(
        Rc::new(RefCell::new(dom)),
        pageon_js::RealmConfig {
            url: "https://probe.test/".into(),
            origin: "https://probe.test".into(),
            navigator: pageon_env::Profile::Agent.navigator(),
            viewport: Viewport::default(),
            stylesheets: Vec::new(),
            client: pageon_net::Client::top(
                &url::Url::parse("https://probe.test/").unwrap_or_else(|e| panic!("{e}")),
            ),
            net: Rc::default(),
            accept_dialogs: false,
            crypto_key: [7; 32],
        },
    );
    engine.run_script(realm, "document.title = String(6 * 7)", "probe", None);
    let ran = engine.errors().is_empty();
    out.push(format!("js={}", if ran { "ok" } else { "failed" }));
    let verdict = |ok: bool| if ok { "ok" } else { "denied" };
    out.push(format!(
        "read-home={}",
        verdict(std::fs::read_dir(home).is_ok())
    ));
    out.push(format!(
        "write-tmp={}",
        verdict(std::fs::write(std::env::temp_dir().join("pageon-probe"), b"x").is_ok())
    ));
    let addr: std::net::SocketAddr = ([1, 1, 1, 1], 80).into();
    out.push(format!(
        "connect={}",
        verdict(
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500))
                .is_ok()
        )
    ));
    out.push(format!(
        "exec={}",
        verdict(std::process::Command::new("/bin/echo").output().is_ok())
    ));
    out.join(" ")
}
