//! `pageon bench`: how fast pageon opens pages, measured offline. A URL is recorded once into a
//! cache; every run replays it, so runs are reproducible and the network is not measured.
//! Cold runs are separate `pageon fetch` processes (caged, as users run them); warm runs open
//! the page again in one in-process session.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use serde::{Deserialize, Serialize};

/// Times in milliseconds over the runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Times {
    pub p50: f64,
    pub p95: f64,
}

impl Times {
    fn of(mut ms: Vec<f64>) -> Self {
        if ms.is_empty() {
            return Self::default();
        }
        ms.sort_by(f64::total_cmp);
        let at = |q: f64| ms[((ms.len() - 1) as f64 * q).round() as usize];
        Self {
            p50: at(0.5),
            p95: at(0.95),
        }
    }
}

/// One page's results.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Row {
    /// What was asked for: a URL or a recording.
    pub page: String,
    pub url: String,
    pub recording: String,
    /// A `pageon fetch` process, start to the agent view printed.
    pub first_view_ms: Times,
    /// Opening the page again in a session.
    pub warm_ms: Times,
    /// CPU (user and system) of a cold run, its cage included.
    pub cpu_ms: Times,
    /// The largest resident size of a cold run's processes.
    pub memory_mb: f64,
    /// Full layouts while loading (scripts reading sizes force them).
    pub layouts: u32,
    /// The agent view's size (o200k).
    pub tokens: usize,
    /// Every warm run gave the same layouts and tokens.
    pub steady: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A whole run.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Report {
    pub pageon: String,
    pub os: String,
    pub arch: String,
    pub runs: u32,
    pub pages: Vec<Row>,
}

/// What `pageon bench` was asked.
pub(crate) struct Ask<'a> {
    pub pages: &'a [String],
    pub runs: u32,
    pub refresh: bool,
    pub json: bool,
    pub against: Option<&'a Path>,
    /// Session options for every run (`--private-network allow`, …).
    pub options: &'a [String],
}

pub(crate) fn run(ask: &Ask<'_>) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: cannot find the pageon executable: {e}");
            return ExitCode::from(3);
        }
    };
    let before = match ask.against.map(read_report).transpose() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let runs = ask.runs.max(1);
    // Progress on a terminal only, on one line cleared at the end.
    let progress = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let mut rows = Vec::new();
    for (i, page) in ask.pages.iter().enumerate() {
        if progress {
            eprint!("\r\x1b[2K{}/{} {page}", i + 1, ask.pages.len());
        }
        rows.push(measure(&exe, page, runs, ask.refresh, ask.options));
    }
    if progress {
        eprint!("\r\x1b[2K");
    }
    let report = Report {
        pageon: env!("CARGO_PKG_VERSION").into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        runs,
        pages: rows,
    };
    if ask.json {
        match serde_json::to_string_pretty(&report) {
            Ok(t) => println!("{t}"),
            Err(e) => {
                eprintln!("error: cannot write the report: {e}");
                return ExitCode::from(3);
            }
        }
    } else {
        print!("{}", render(&report, before.as_ref()));
    }
    if report.pages.iter().any(|r| r.error.is_some()) {
        ExitCode::from(3)
    } else {
        ExitCode::SUCCESS
    }
}

fn read_report(path: &Path) -> Result<Report, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| {
        format!(
            "{} is not a `pageon bench --json` report: {e}",
            path.display()
        )
    })
}

fn measure(exe: &Path, page: &str, runs: u32, refresh: bool, options: &[String]) -> Row {
    let mut row = Row {
        page: page.into(),
        ..Row::default()
    };
    let fail = |mut row: Row, e: String| {
        row.error = Some(e);
        row
    };
    let (url, recording) = match recording(exe, page, refresh, options) {
        Ok(r) => r,
        Err(e) => return fail(row, e),
    };
    row.url.clone_from(&url);
    row.recording = recording.display().to_string();
    let (mut wall, mut cpu, mut peak) = (Vec::new(), Vec::new(), 0u64);
    for _ in 0..runs {
        match cold(exe, &url, &recording, options) {
            Ok(r) => {
                wall.push(r.wall_ms);
                cpu.push(r.cpu_ms);
                peak = peak.max(r.peak_bytes);
            }
            Err(e) => return fail(row, e),
        }
    }
    row.first_view_ms = Times::of(wall);
    row.cpu_ms = Times::of(cpu);
    row.memory_mb = peak as f64 / (1024.0 * 1024.0);
    match warm(&url, &recording, runs, options) {
        Ok((ms, layouts, tokens, steady)) => {
            row.warm_ms = Times::of(ms);
            row.layouts = layouts;
            row.tokens = tokens;
            row.steady = steady;
        }
        Err(e) => return fail(row, e),
    }
    row
}

/// The page's URL and recording: the recording given, or the URL's in the cache (recorded now
/// when missing or with `--refresh`).
fn recording(
    exe: &Path,
    page: &str,
    refresh: bool,
    options: &[String],
) -> Result<(String, PathBuf), String> {
    let path = Path::new(page);
    if !page.contains("://") && path.is_file() {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let log: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("{} is not a recording: {e}", path.display()))?;
        let url = log["steps"][0]["input"]["open"]["url"]
            .as_str()
            .ok_or_else(|| format!("{} does not start by opening a page", path.display()))?;
        return Ok((url.into(), path.to_path_buf()));
    }
    let dir = cache_dir().join("bench");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let name = format!("{}.rec.json", pageon_env::fnv64(page));
    let file = dir.join(name);
    if refresh || !file.is_file() {
        // Recorded to a temporary file first: a failed recording leaves no cache entry, and
        // two benches never see each other's half-written file.
        let tmp = file.with_extension(format!("{}.tmp", std::process::id()));
        // The page goes after `--`: it is never read as an option.
        let out = Command::new(exe)
            .args(["fetch", "--dump", "ax", "--record"])
            .arg(&tmp)
            .args(options)
            .args(["--", page])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("cannot run pageon: {e}"))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&tmp);
            let why = String::from_utf8_lossy(&out.stderr);
            let why = why.lines().rfind(|l| !l.trim().is_empty()).unwrap_or("");
            return Err(format!("recording failed: {why}"));
        }
        std::fs::rename(&tmp, &file)
            .map_err(|e| format!("cannot keep the recording {}: {e}", file.display()))?;
    }
    Ok((page.into(), file))
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("pageon")
}

struct Cold {
    wall_ms: f64,
    cpu_ms: f64,
    peak_bytes: u64,
}

/// One cold run: a `pageon fetch` process replaying the page (caged, as users run it).
// A benchmark measures wall time on purpose.
#[allow(clippy::disallowed_methods)]
fn cold(exe: &Path, url: &str, recording: &Path, options: &[String]) -> Result<Cold, String> {
    let start = std::time::Instant::now();
    let child = Command::new(exe)
        .args(["fetch", "--dump", "ax", "--replay"])
        .arg(recording)
        .args(options)
        .args(["--", url])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run pageon: {e}"))?;
    let (status, usage) = wait(child.id())?;
    let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
    if status != 0 {
        return Err(format!(
            "the replay failed (exit {status}); run `pageon replay {}` to see why",
            recording.display()
        ));
    }
    Ok(Cold {
        wall_ms,
        cpu_ms: usage.cpu_ms,
        peak_bytes: usage.peak_bytes,
    })
}

struct Usage {
    cpu_ms: f64,
    peak_bytes: u64,
}

/// Waits for a child; its exit status and what it used (with the processes it waited for: its
/// cage). Peak memory is the largest of those processes' resident sizes.
#[allow(unsafe_code)]
fn wait(pid: u32) -> Result<(i32, Usage), String> {
    let pid = libc::pid_t::try_from(pid).map_err(|_| "bad process id".to_string())?;
    let mut status: libc::c_int = 0;
    // SAFETY: an all-zero `rusage` is a valid value of the plain C struct.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `pid` is our own child, not waited for elsewhere; `status` and `ru` are valid
        // writable locations for the call's duration.
        let r = unsafe { libc::wait4(pid, &raw mut status, 0, &raw mut ru) };
        if r == pid {
            break;
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("cannot wait for pageon: {e}"));
        }
    }
    let ms = |t: libc::timeval| t.tv_sec as f64 * 1000.0 + t.tv_usec as f64 / 1000.0;
    // Linux reports the peak in KiB, macOS in bytes.
    let unit: u64 = if cfg!(target_os = "macos") { 1 } else { 1024 };
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -1
    };
    Ok((
        code,
        Usage {
            cpu_ms: ms(ru.ru_utime) + ms(ru.ru_stime),
            peak_bytes: u64::try_from(ru.ru_maxrss).unwrap_or(0) * unit,
        },
    ))
}

/// Warm runs: the page opened again in one in-process session, after a first open.
// A benchmark measures wall time on purpose.
#[allow(clippy::disallowed_methods)]
fn warm(
    url: &str,
    recording: &Path,
    runs: u32,
    options: &[String],
) -> Result<(Vec<f64>, u32, usize, bool), String> {
    let args = ["pageon", "--no-sandbox", "--replay"];
    let parsed = <super::Flags as clap::Parser>::try_parse_from(
        args.into_iter()
            .map(std::ffi::OsString::from)
            .chain([recording.into()])
            .chain(options.iter().map(std::ffi::OsString::from)),
    )
    .map_err(|e| e.to_string())?;
    let prepared = super::prepare(&parsed.session).map_err(|_| "bad options".to_string())?;
    let mut times = Vec::new();
    let mut seen: Option<(u32, usize)> = None;
    let mut steady = true;
    // A replay hands out its recorded clock and responses once: each open gets its own, in
    // this process (warm: the engine started, caches filled by the first open).
    for i in 0..=runs {
        let (ms, now) = super::with_env(&prepared.config, false, false, |env| {
            let s = pageon::Session::new(env);
            let start = std::time::Instant::now();
            let mut tab = s
                .open(url, prepared.options.clone())
                .map_err(|e| super::fail(&e))?;
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let facts = tab.audit();
            Ok((ms, (facts.layouts, facts.view_tokens)))
        })
        .map_err(|_| format!("the page did not open from {}", recording.display()))?;
        steady &= seen.is_none_or(|s| s == now);
        seen = Some(now);
        if i > 0 {
            times.push(ms);
        }
    }
    let (layouts, tokens) = seen.unwrap_or_default();
    Ok((times, layouts, tokens, steady))
}

fn render(report: &Report, before: Option<&Report>) -> String {
    use std::fmt::Write as _;
    let mut out = format!(
        "pageon {} · {} {} · {} runs · offline replays (p50)\n\n",
        report.pageon, report.os, report.arch, report.runs
    );
    let width = report
        .pages
        .iter()
        .map(|r| short(&r.page).chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let _ = writeln!(
        out,
        "{:width$}  {:>10}  {:>8}  {:>8}  {:>8}  {:>7}  {:>7}",
        "page", "first view", "warm", "CPU", "memory", "layouts", "tokens"
    );
    for r in &report.pages {
        let page = short(&r.page);
        if let Some(e) = &r.error {
            let _ = writeln!(out, "{page:width$}  error: {e}");
            continue;
        }
        let _ = writeln!(
            out,
            "{page:width$}  {:>10}  {:>8}  {:>8}  {:>8}  {:>7}  {:>7}{}",
            time(r.first_view_ms.p50),
            time(r.warm_ms.p50),
            time(r.cpu_ms.p50),
            format!("{:.0} MB", r.memory_mb),
            r.layouts,
            r.tokens,
            if r.steady { "" } else { "  (varies)" },
        );
        if let Some(b) = before.and_then(|b| b.pages.iter().find(|p| p.page == r.page)) {
            let _ = writeln!(
                out,
                "{:width$}  {:>10}  {:>8}  {:>8}  {:>8}  {:>7}  {:>7}",
                "",
                change(b.first_view_ms.p50, r.first_view_ms.p50),
                change(b.warm_ms.p50, r.warm_ms.p50),
                change(b.cpu_ms.p50, r.cpu_ms.p50),
                change(b.memory_mb, r.memory_mb),
                change(f64::from(b.layouts), f64::from(r.layouts)),
                change(b.tokens as f64, r.tokens as f64),
            );
        }
    }
    out
}

/// A page as shown: the URL without its scheme.
fn short(page: &str) -> &str {
    page.split_once("://").map_or(page, |(_, rest)| rest)
}

fn time(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2} s", ms / 1000.0)
    } else {
        format!("{ms:.0} ms")
    }
}

fn change(before: f64, now: f64) -> String {
    if before <= 0.0 {
        return String::new();
    }
    let pct = (now - before) / before * 100.0;
    if pct.abs() < 0.5 {
        "±0 %".into()
    } else {
        format!("{pct:+.0} %")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let t = Times::of((1..=100).map(f64::from).collect());
        assert_eq!((t.p50, t.p95), (51.0, 95.0));
        assert_eq!(Times::of(vec![]).p50, 0.0);
    }

    #[test]
    fn changes_read_as_percent() {
        assert_eq!(change(200.0, 100.0), "-50 %");
        assert_eq!(change(100.0, 100.2), "±0 %");
        assert_eq!(change(0.0, 5.0), "");
    }
}
