//! The session's environment as a library ([`Config`]).

use std::fmt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use pageon_env::{Env, Identity, Limits, Live, Profile, Recording, Replay};

use crate::{Effect, Files, Policy, Secret, Secrets};

/// A configuration the session cannot start with, and how to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// An origin without a scheme, or not an origin.
    Origin(String),
    /// A secret spec, or its value missing from the environment.
    Secret(String),
    /// A staged-file spec.
    File(String),
    /// A recording and a replay together, or a replay that cannot be read.
    Replay(String),
    /// An identity that cannot be loaded.
    Identity(String),
    /// A proxy URL.
    Proxy(String),
    /// A profile this build does not have.
    Profile(String),
    /// The recording could not be written.
    Recording(String),
}

impl ConfigError {
    /// What to do about it.
    pub fn guidance(&self) -> String {
        match self {
            Self::Origin(_) => "write origins with their scheme: https://shop.example".into(),
            Self::Secret(_) => {
                "write secrets as NAME@ORIGIN and set PAGEON_SECRET_<NAME> to the value".into()
            }
            Self::File(_) => {
                "write files as NAME@ORIGIN=PATH, e.g. cv@https://jobs.example=./cv.pdf".into()
            }
            Self::Replay(_) => "record one with `pageon fetch <url> --record FILE`".into(),
            Self::Identity(_) => "create one with `pageon identity new`".into(),
            Self::Proxy(_) => "pass a proxy URL such as http://127.0.0.1:3128".into(),
            Self::Profile(_) => "use the agent profile, or a build with the chrome feature".into(),
            Self::Recording(_) => "check the directory exists and is writable".into(),
        }
    }

    /// Whether the error is about a file (exit code 3 in the CLI) rather than the
    /// configuration (2).
    pub fn is_io(&self) -> bool {
        matches!(self, Self::Replay(_) | Self::Recording(_))
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Origin(m)
            | Self::Secret(m)
            | Self::File(m)
            | Self::Replay(m)
            | Self::Identity(m)
            | Self::Proxy(m)
            | Self::Profile(m)
            | Self::Recording(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ConfigError {}

/// An approver: asked about every write the policy holds; `true` lets it leave.
type Approver = Rc<dyn Fn(&Effect) -> bool>;

/// A session's environment, configured as the CLI's session flags configure it: what the
/// session may reach (origins), with what (secrets, staged files, an identity), who decides
/// (a guard, an approver), and where it is kept (a recording) or comes from (a replay).
///
/// The `pageon` binary assembles every command from a [`Config`]; a program that embeds pageon
/// gets exactly the same stack:
///
/// ```no_run
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let config = pageon::Config::new()
///     .origin("https://shop.example")
///     .secret_spec("shop@https://shop.example") // its value from PAGEON_SECRET_SHOP
///     .file_spec("cv@https://jobs.example=./cv.pdf")
///     .record("session.rec.json");
/// for warning in config.check()? {
///     eprintln!("warning: {warning}");
/// }
/// let stdin = std::io::stdin();
/// pageon::mcp::serve_with(&config, Default::default(), stdin.lock(), std::io::stdout())?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Config {
    read_write: Vec<String>,
    read_only: Vec<String>,
    secrets: Vec<(String, Secret)>,
    secret_specs: Vec<String>,
    files: Vec<(String, PathBuf, Vec<String>)>,
    file_specs: Vec<String>,
    guard: Option<String>,
    approver: Option<Approver>,
    hold_writes: bool,
    record: Option<PathBuf>,
    replay: Option<PathBuf>,
    identity: Option<PathBuf>,
    profile: Profile,
    proxy: Option<String>,
    timeout: Duration,
    max_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            read_write: Vec::new(),
            read_only: Vec::new(),
            secrets: Vec::new(),
            secret_specs: Vec::new(),
            files: Vec::new(),
            file_specs: Vec::new(),
            guard: None,
            approver: None,
            hold_writes: false,
            record: None,
            replay: None,
            identity: None,
            profile: Profile::Agent,
            proxy: None,
            timeout: Duration::from_secs(30),
            max_bytes: 50 * 1024 * 1024,
        }
    }
}

// Secrets show as their names and origins, never their values.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("read_write", &self.read_write)
            .field("read_only", &self.read_only)
            .field("secrets", &self.secrets)
            .field("secret_specs", &self.secret_specs)
            .field("files", &self.files)
            .field("file_specs", &self.file_specs)
            .field("guard", &self.guard)
            .field("approver", &self.approver.is_some())
            .field("hold_writes", &self.hold_writes)
            .field("record", &self.record)
            .field("replay", &self.replay)
            .field("identity", &self.identity)
            .field("profile", &self.profile)
            .field("proxy", &self.proxy)
            .field("timeout", &self.timeout)
            .field("max_bytes", &self.max_bytes)
            .finish()
    }
}

impl Config {
    /// The CLI's defaults: every origin, no secrets or files, the agent profile, no recording.
    pub fn new() -> Self {
        Self::default()
    }

    /// An origin the task may read and write (`https://*.example.com` for subdomains). With any
    /// origin set, requests elsewhere are refused.
    pub fn origin(mut self, origin: impl Into<String>) -> Self {
        self.read_write.push(origin.into());
        self
    }

    /// An origin the task may only read.
    pub fn read_origin(mut self, origin: impl Into<String>) -> Self {
        self.read_only.push(origin.into());
        self
    }

    /// A secret for `type eN secret://NAME`, with the origins it may be typed into.
    pub fn secret(mut self, name: impl Into<String>, secret: Secret) -> Self {
        self.secrets.push((name.into(), secret));
        self
    }

    /// A secret as the CLI writes it: `NAME@ORIGIN`, or `NAME:totp@ORIGIN` for a TOTP seed; the
    /// value is read from `PAGEON_SECRET_<NAME>` when checked.
    pub fn secret_spec(mut self, spec: impl Into<String>) -> Self {
        self.secret_specs.push(spec.into());
        self
    }

    /// A file the agent may upload as `name` (`upload eN NAME`), to pages of `origins`. Read
    /// when uploaded; never a path the agent chooses.
    pub fn file<S: Into<String>>(
        mut self,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
        origins: impl IntoIterator<Item = S>,
    ) -> Self {
        self.files.push((
            name.into(),
            path.into(),
            origins.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// A staged file as the CLI writes it: `NAME@ORIGIN[,ORIGIN…]=PATH`.
    pub fn file_spec(mut self, spec: impl Into<String>) -> Self {
        self.file_specs.push(spec.into());
        self
    }

    /// A program asked about every request but reads of known origins (see the guard
    /// protocol); it fails closed.
    pub fn guard(mut self, command: impl Into<String>) -> Self {
        self.guard = Some(command.into());
        self
    }

    /// Asks `approve` about every write the policy holds for a person (outside the origin sets
    /// or irreversible); `true` lets it leave.
    pub fn approver(mut self, approve: impl Fn(&Effect) -> bool + 'static) -> Self {
        self.approver = Some(Rc::new(approve));
        self
    }

    /// Holds every write and reports it as pending, never sending it (checking a site against
    /// a recording).
    pub fn hold_writes(mut self, hold: bool) -> Self {
        self.hold_writes = hold;
        self
    }

    /// Records everything the session used to this file (secrets redacted, owner-only).
    pub fn record(mut self, path: impl Into<PathBuf>) -> Self {
        self.record = Some(path.into());
        self
    }

    /// Serves the session from a recording, offline.
    pub fn replay(mut self, path: impl Into<PathBuf>) -> Self {
        self.replay = Some(path.into());
        self
    }

    /// Signs every request with this identity (Web Bot Auth; `pageon identity new`).
    pub fn identity(mut self, path: impl Into<PathBuf>) -> Self {
        self.identity = Some(path.into());
        self
    }

    /// Who the session says it is.
    pub fn profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        self
    }

    /// Sends every request through this proxy.
    pub fn proxy(mut self, url: impl Into<String>) -> Self {
        self.proxy = Some(url.into());
        self
    }

    /// How long a request may take (default 30 s).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The largest response in bytes (default 50 MiB).
    pub fn max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = max;
        self
    }

    /// Whether files are staged (the vendor toolsets then offer `file_upload`).
    pub fn has_files(&self) -> bool {
        !self.files.is_empty() || !self.file_specs.is_empty()
    }

    /// Checks the configuration (origins, secrets and their values, files, the identity); returns
    /// warnings to show (an identity readable by others). The network and a replay are checked
    /// when the session runs.
    pub fn check(&self) -> Result<Vec<String>, ConfigError> {
        for o in self.read_write.iter().chain(&self.read_only) {
            crate::check_origin(o).map_err(|e| ConfigError::Origin(e.to_string()))?;
        }
        self.all_secrets()?;
        self.all_files()?;
        if self.record.is_some() && self.replay.is_some() {
            return Err(ConfigError::Replay(
                "a session records or replays, not both".into(),
            ));
        }
        let mut warnings = Vec::new();
        if let Some(p) = &self.identity {
            Identity::load(p).map_err(|e| ConfigError::Identity(e.to_string()))?;
            if Identity::exposed(p) {
                warnings.push(format!(
                    "{} is readable by others; run `chmod 600 {}`",
                    p.display(),
                    p.display()
                ));
            }
        }
        Ok(warnings)
    }

    /// Runs `job` over the session's environment: the vault and staged files around the
    /// origin policy (with its guard and approver) around the network (live, recorded or
    /// replayed). A recording is saved afterwards, every secret value handed out redacted.
    pub fn run<T>(&self, job: impl FnOnce(&dyn Env) -> T) -> Result<T, ConfigError> {
        self.run_with(false, job)
    }

    /// [`run`](Self::run), keeping the session's steps in memory even without a recording
    /// (MCP's resets rebuild steps from them).
    pub fn run_keeping<T>(&self, job: impl FnOnce(&dyn Env) -> T) -> Result<T, ConfigError> {
        self.run_with(true, job)
    }

    /// Runs `job` with the vault, staged files and origin policy around an environment of the
    /// caller's (a resumed recording, an embedder's own network); nothing is saved.
    pub fn run_on<T>(
        &self,
        base: &dyn Env,
        job: impl FnOnce(&dyn Env) -> T,
    ) -> Result<T, ConfigError> {
        let secrets = self.all_secrets()?;
        let files = self.all_files()?;
        Ok(self.guarded(base, &secrets, &files, job).0)
    }

    /// The live network this configuration describes: its profile, proxy and identity.
    pub fn network(&self) -> Result<Live, ConfigError> {
        self.live()
    }

    fn run_with<T>(&self, keep: bool, job: impl FnOnce(&dyn Env) -> T) -> Result<T, ConfigError> {
        let secrets = self.all_secrets()?;
        let files = self.all_files()?;
        if let Some(path) = &self.replay {
            if self.record.is_some() {
                return Err(ConfigError::Replay(
                    "a session records or replays, not both".into(),
                ));
            }
            let env = Replay::load(path).map_err(|e| ConfigError::Replay(e.to_string()))?;
            if keep {
                let rec = Recording::new(env).with_body_limit(KEEP_LIMIT);
                return Ok(self.guarded(&rec, &secrets, &files, job).0);
            }
            return Ok(self.guarded(&env, &secrets, &files, job).0);
        }
        let live = self.live()?;
        let Some(path) = &self.record else {
            if keep {
                let rec = Recording::new(live).with_body_limit(KEEP_LIMIT);
                return Ok(self.guarded(&rec, &secrets, &files, job).0);
            }
            return Ok(self.guarded(&live, &secrets, &files, job).0);
        };
        let env = Recording::new(live);
        let (r, values) = self.guarded(&env, &secrets, &files, job);
        env.redact(values);
        env.save(path).map_err(|e| {
            ConfigError::Recording(format!(
                "could not write the recording {}: {e}",
                path.display()
            ))
        })?;
        Ok(r)
    }

    /// The vault and staged files around the policy around `base`; the result and every
    /// secret value handed out.
    fn guarded<T>(
        &self,
        base: &dyn Env,
        secrets: &[(String, Secret)],
        files: &[(String, PathBuf, Vec<String>)],
        job: impl FnOnce(&dyn Env) -> T,
    ) -> (T, Vec<String>) {
        let mut policy = Policy::new(base)
            .read_write(&self.read_write)
            .read_only(&self.read_only);
        if let Some(a) = &self.approver {
            let a = a.clone();
            policy = policy.approver(move |e| a(e));
        }
        if self.hold_writes {
            policy = policy.hold_writes();
        }
        if let Some(cmd) = &self.guard {
            let guard = crate::CommandGuard::new(cmd.clone());
            policy = policy.guard(move |e| guard.verdict(e));
        }
        let mut vault = Secrets::new(&policy);
        for (name, secret) in secrets {
            vault = vault.with(name, secret.clone());
        }
        let mut staged = Files::new(&vault);
        for (name, path, origins) in files {
            staged = staged.with(name, path.clone(), origins.clone());
        }
        let r = job(&staged);
        (r, vault.values())
    }

    /// The live network: its profile, proxy and identity.
    fn live(&self) -> Result<Live, ConfigError> {
        let limits = Limits {
            timeout: self.timeout,
            max_bytes: self.max_bytes,
        };
        let mut live = match self.profile {
            Profile::Agent => Live::new(limits),
            Profile::BrowserCompatible => {
                Live::browser_compatible(limits).map_err(|e| ConfigError::Profile(e.to_string()))?
            }
        };
        if let Some(p) = &self.proxy {
            live = live
                .with_proxy(p)
                .map_err(|e| ConfigError::Proxy(e.to_string()))?;
        }
        if let Some(path) = &self.identity {
            let id = Identity::load(path).map_err(|e| ConfigError::Identity(e.to_string()))?;
            live = live.with_identity(id);
        }
        Ok(live)
    }

    fn all_secrets(&self) -> Result<Vec<(String, Secret)>, ConfigError> {
        let mut out = self.secrets.clone();
        for spec in &self.secret_specs {
            out.push(parse_secret(spec).map_err(ConfigError::Secret)?);
        }
        Ok(out)
    }

    fn all_files(&self) -> Result<Vec<(String, PathBuf, Vec<String>)>, ConfigError> {
        let mut out = Vec::new();
        for (name, path, origins) in &self.files {
            check_file(name, path, origins).map_err(ConfigError::File)?;
            out.push((name.clone(), path.clone(), origins.clone()));
        }
        for spec in &self.file_specs {
            out.push(parse_file(spec).map_err(ConfigError::File)?);
        }
        Ok(out)
    }
}

/// Most response-body bytes an in-memory session recording keeps (MCP's resets); past that it
/// stops, and resets reach only the steps before.
const KEEP_LIMIT: usize = 64 << 20;

/// `NAME@ORIGIN` (or `NAME:totp@ORIGIN`), its value from `PAGEON_SECRET_<NAME>`.
fn parse_secret(spec: &str) -> Result<(String, Secret), String> {
    let (name, origin) = spec
        .split_once('@')
        .ok_or_else(|| format!("{spec:?}: expected NAME@ORIGIN, e.g. shop@https://shop.example"))?;
    let (name, totp) = match name.strip_suffix(":totp") {
        Some(n) => (n, true),
        None => (name, false),
    };
    crate::check_origin(origin).map_err(|e| format!("secret {name}: {e}"))?;
    let var = format!(
        "PAGEON_SECRET_{}",
        name.to_ascii_uppercase().replace(['-', '.'], "_")
    );
    let value = std::env::var(&var).map_err(|_| {
        format!("secret {name}: set {var} to its value (it never reaches the page's text)")
    })?;
    let secret = if totp {
        Secret::totp(&value).map_err(|e| format!("{var}: {e}"))?
    } else {
        Secret::value(value)
    };
    Ok((name.to_string(), secret.origins([origin.to_string()])))
}

/// `NAME@ORIGIN[,ORIGIN…]=PATH`.
fn parse_file(spec: &str) -> Result<(String, PathBuf, Vec<String>), String> {
    let help =
        || format!("{spec:?}: expected NAME@ORIGIN=PATH, e.g. cv@https://jobs.example=./cv.pdf");
    let (head, path) = spec.split_once('=').ok_or_else(help)?;
    let (name, origins) = head.split_once('@').ok_or_else(help)?;
    if path.is_empty() {
        return Err(help());
    }
    let origins: Vec<String> = origins.split(',').map(|o| o.trim().to_string()).collect();
    check_file(name, Path::new(path), &origins)?;
    Ok((name.to_string(), path.into(), origins))
}

fn check_file(name: &str, path: &Path, origins: &[String]) -> Result<(), String> {
    if name.is_empty() || name.starts_with("download:") || path.as_os_str().is_empty() {
        return Err(format!(
            "file {name:?}: a name (not download:N) and a path, e.g. cv@https://jobs.example=./cv.pdf"
        ));
    }
    for o in origins {
        crate::check_origin(o).map_err(|e| format!("--file {name}: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_and_errors_name_the_fix() {
        let e = Config::new().origin("shop.example").check().unwrap_err();
        assert!(matches!(e, ConfigError::Origin(_)), "{e:?}");
        let e = Config::new().file_spec("cv=./cv.pdf").check().unwrap_err();
        assert!(e.to_string().contains("NAME@ORIGIN=PATH"), "{e}");
        let e = Config::new()
            .secret_spec("pageon-test-unset@https://a.test")
            .check()
            .unwrap_err();
        assert!(
            e.to_string().contains("PAGEON_SECRET_PAGEON_TEST_UNSET"),
            "{e}"
        );
        let e = Config::new().record("a").replay("b").check().unwrap_err();
        assert!(e.is_io());
    }

    #[test]
    fn secrets_never_show_in_debug() {
        let c = Config::new().secret(
            "shop",
            Secret::value("hunter2-value").origins(["https://shop.test"]),
        );
        let d = format!("{c:?}");
        assert!(d.contains("shop") && !d.contains("hunter2"), "{d}");
    }
}
