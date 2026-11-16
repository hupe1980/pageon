//! Platform APIs pages use, as libraries use them: Web Animations and the task scheduler on the
//! virtual clock, permissions and notifications (never granted), user activation, beacons
//! through the page's network policy, and values that agree caged, in-process and in workers
//! (UTC, cores, screen, the PDF viewer lists).

mod common;

use common::*;
use pageon::{LoadOptions, Replay, Sandbox, SandboxOptions, Session};

const CHECKS: &str = r#"<!doctype html><title>checks</title><div id=box>box</div><button id=b>Go</button><pre id=out>running</pre><script>
const results = {};
const ok = (name, cond, detail) => { results[name] = cond ? true : String(detail); };
const done = () => { out.textContent = JSON.stringify(results); };
(async () => {
  // A dialog that leaves after its exit animation (the pattern UI libraries use).
  const a = box.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 300, fill: 'forwards' });
  let finishEvent = false;
  a.onfinish = (e) => { finishEvent = e.currentTime === 300 && e.type === 'finish'; };
  ok('animation running', a.playState === 'running' && document.getAnimations().length === 1, a.playState);
  const t0 = performance.now();
  await a.finished;
  ok('animation finished on time', Math.round(performance.now() - t0) === 300, performance.now() - t0);
  ok('animation state', a.playState === 'finished' && a.currentTime === 300, a.playState + ' ' + a.currentTime);
  a.commitStyles();
  ok('commitStyles', box.style.opacity === '0', box.style.opacity);
  ok('fill forwards stays relevant', box.getAnimations().length === 1, box.getAnimations().length);
  await new Promise((r) => setTimeout(r, 0));
  ok('finish event', finishEvent, finishEvent);
  const c = box.animate({ transform: ['none', 'translateX(10px)'] }, 1000);
  const cancelled = c.finished.then(() => 'resolved', (e) => e.name);
  c.cancel();
  ok('cancel rejects', (await cancelled) === 'AbortError', await cancelled);
  const inf = box.animate([{}, {}], { duration: 100, iterations: Infinity });
  let threw = '';
  try { inf.finish(); } catch (e) { threw = e.name; }
  ok('infinite finish throws', threw === 'InvalidStateError', threw);
  inf.cancel();
  ok('timeline', typeof document.timeline.currentTime === 'number', document.timeline.currentTime);
  ok('timing', box.animate([], { duration: 50, delay: 10 }).effect.getComputedTiming().endTime === 60, 'endTime');

  // The scheduler: priorities, continuations, delays, aborts and priority changes.
  const order = [];
  const tasks = [
    scheduler.postTask(() => order.push('background'), { priority: 'background' }),
    scheduler.postTask(() => order.push('visible')),
    scheduler.postTask(() => order.push('blocking'), { priority: 'user-blocking' }),
  ];
  await Promise.all(tasks);
  ok('priority order', order.join() === 'blocking,visible,background', order.join());
  const seq = [];
  scheduler.postTask(() => seq.push('task'));
  await scheduler.yield();
  seq.push('continuation');
  await new Promise((r) => setTimeout(r, 0));
  ok('yield continues first', seq.join() === 'continuation,task', seq.join());
  const t1 = performance.now();
  await scheduler.postTask(() => {}, { delay: 250 });
  ok('delay on the virtual clock', Math.round(performance.now() - t1) === 250, performance.now() - t1);
  const tc = new TaskController({ priority: 'background' });
  let changed = '';
  tc.signal.onprioritychange = (e) => { changed = e.previousPriority + '>' + tc.signal.priority; };
  tc.setPriority('user-blocking');
  ok('priority change', changed === 'background>user-blocking' && tc.signal instanceof AbortSignal, changed);
  const aborted = scheduler.postTask(() => 'ran', { signal: tc.signal });
  tc.abort();
  ok('abort rejects', (await aborted.catch((e) => e.name)) === 'AbortError', 'not aborted');

  // Permissions and notifications: asked, never granted, both APIs agree.
  const q = async (name) => (await navigator.permissions.query({ name })).state;
  ok('permissions before', (await q('notifications')) === 'prompt' && Notification.permission === 'default' && (await q('clipboard-write')) === 'granted' && (await q('camera')) === 'prompt', await q('notifications'));
  const bad = await navigator.permissions.query({ name: 'teleport' }).then(() => 'ok', (e) => e.name);
  ok('unknown permission', bad === 'TypeError', bad);
  const answer = await Notification.requestPermission();
  ok('notifications denied', answer === 'denied' && Notification.permission === 'denied' && (await q('notifications')) === 'denied', answer);

  // Screen, hardware and the PDF viewer, as the standard describes them.
  ok('screen', screenX === 0 && screenTop === 0 && screen.orientation.type === 'landscape-primary' && screen.orientation.angle === 0, screen.orientation.type);
  const lock = await screen.orientation.lock('portrait').then(() => 'ok', (e) => e.name);
  ok('orientation lock', lock === 'NotSupportedError', lock);
  ok('pdf viewer', navigator.pdfViewerEnabled === true && navigator.plugins.length === 5 && navigator.plugins[0].name === 'PDF Viewer'
    && navigator.plugins.namedItem('Chrome PDF Viewer').filename === 'internal-pdf-viewer' && navigator.mimeTypes[0].type === 'application/pdf'
    && navigator.mimeTypes[1].enabledPlugin.name === 'PDF Viewer' && [...navigator.plugins].length === 5 && navigator.javaEnabled() === false,
    navigator.plugins.length);
  ok('utc', Intl.DateTimeFormat().resolvedOptions().timeZone === 'UTC' && new Date(0).getHours() === 0, Intl.DateTimeFormat().resolvedOptions().timeZone);

  // Workers see the same values.
  const w = new Worker(URL.createObjectURL(new Blob([
    "postMessage([navigator.hardwareConcurrency, Intl.DateTimeFormat().resolvedOptions().timeZone, typeof scheduler.postTask, navigator.userAgent])"
  ], { type: 'text/javascript' })));
  const [cores, tz, sched, ua] = await new Promise((r) => { w.onmessage = (e) => r(e.data); });
  ok('worker agrees', cores === navigator.hardwareConcurrency && cores === 8 && tz === Intl.DateTimeFormat().resolvedOptions().timeZone && sched === 'function' && ua === navigator.userAgent, [cores, tz, sched].join());

  // User activation: none until a person (or the agent) acts.
  ok('no activation yet', navigator.userActivation.hasBeenActive === false && navigator.userActivation.isActive === false, navigator.userActivation.hasBeenActive);
  b.onclick = () => {
    results['activation on click'] = navigator.userActivation.isActive && navigator.userActivation.hasBeenActive ? true : 'not active';
    // Beacons: a POST through the page's own network; oversized and non-http ones refused.
    results['beacon sent'] = navigator.sendBeacon('/collect', 'event=click') === true ? true : 'false';
    results['beacon too big'] = navigator.sendBeacon('/collect', 'x'.repeat(70000)) === false ? true : 'sent';
    try { navigator.sendBeacon('ftp://x.test/'); results['beacon scheme'] = 'no error'; } catch (e) { results['beacon scheme'] = e.name === 'TypeError' ? true : e.name; }
    results['beacon to a private address'] = navigator.sendBeacon('http://127.0.0.1:9/x', 'secret') === true ? true : 'false';
    done();
  };
  done();
})().catch((e) => { results.crash = String(e && e.stack || e); done(); });
</script>"#;

fn site_log() -> pageon_env::Log {
    site(vec![
        html("https://c.test/", CHECKS),
        page("https://c.test/collect", 204, &[], ""),
    ])
}

fn results(text: &str) -> serde_json::Map<String, serde_json::Value> {
    let start = text
        .find('{')
        .unwrap_or_else(|| panic!("no results: {text}"));
    let end = text.rfind('}').unwrap_or(text.len() - 1);
    serde_json::from_str::<serde_json::Value>(&text[start..=end])
        .unwrap_or_else(|e| panic!("{e}: {text}"))
        .as_object()
        .cloned()
        .unwrap_or_default()
}

fn failed(r: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    r.iter()
        .filter(|(_, v)| **v != serde_json::Value::Bool(true))
        .map(|(k, v)| format!("{k}: {v}"))
        .collect()
}

fn out_of(view: &str) -> String {
    view.lines()
        .find(|l| l.contains("text \"{"))
        .map(|l| {
            let q = l.split_once("text ").map_or("", |x| x.1);
            serde_json::from_str::<String>(q).unwrap_or_default()
        })
        .unwrap_or_else(|| panic!("no output line: {view}"))
}

const EXPECTED: usize = 29;

#[test]
fn the_platform_checks_pass_in_process() {
    let env = Capture::new(site_log());
    let s = Session::new(&env);
    let mut tab = s
        .open("https://c.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    // A library embedding keeps its process's zone (the pageon binary and the content process
    // set UTC; see `the_binary_shows_pages_utc`).
    let only_zone = |r: &serde_json::Map<String, serde_json::Value>| {
        failed(r)
            .into_iter()
            .filter(|f| !f.starts_with("utc:"))
            .collect::<Vec<_>>()
    };
    let r = results(&out_of(&tab.observe()));
    assert!(only_zone(&r).is_empty(), "{:#?}", only_zone(&r));
    let b = find(&tab.observe_full(), "button", "Go");
    let o = tab
        .act(pageon::Action::Click(b))
        .unwrap_or_else(|e| panic!("{e}"));
    let r = results(&out_of(&tab.observe_full()));
    assert!(only_zone(&r).is_empty(), "{:#?}", only_zone(&r));
    assert_eq!(r.len(), EXPECTED, "{r:#?}");
    // The beacon is a request like any other: sent, and logged; the private one is refused.
    let beacon = env.headers("POST", "https://c.test/collect");
    assert!(
        beacon
            .iter()
            .any(|(k, v)| k == "content-type" && v.starts_with("text/plain")),
        "{beacon:?}"
    );
    assert!(
        !env.urls().iter().any(|u| u.contains("127.0.0.1")),
        "{:?}",
        env.urls()
    );
    let refused = o
        .requests
        .iter()
        .find(|q| q.url.contains("127.0.0.1"))
        .unwrap_or_else(|| panic!("{o:#?}"));
    assert!(
        !matches!(refused.outcome, pageon::RequestOutcome::Ok),
        "{refused:?}"
    );
}

#[test]
fn the_platform_checks_pass_caged() {
    let mut sb = Sandbox::new(
        Replay::new(site_log()),
        SandboxOptions {
            program: Some(env!("CARGO_BIN_EXE_pageon").into()),
            ..SandboxOptions::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let (t, _) = sb
        .open_tab("https://c.test/", LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    let view = sb.observe_tab(t, true).unwrap_or_else(|e| panic!("{e}"));
    let b = find(&view, "button", "Go");
    sb.act_tab(t, pageon::Action::Click(b))
        .unwrap_or_else(|e| panic!("{e}"));
    let r = results(&out_of(
        &sb.observe_tab(t, true).unwrap_or_else(|e| panic!("{e}")),
    ));
    assert!(failed(&r).is_empty(), "{:#?}", failed(&r));
    assert_eq!(r.len(), EXPECTED, "{r:#?}");
}

#[test]
fn the_binary_shows_pages_utc() {
    let dir = std::env::temp_dir().join(format!("pageon-utc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let page = dir.join("tz.html");
    std::fs::write(
        &page,
        "<p id=o></p><script>o.textContent = Intl.DateTimeFormat().resolvedOptions().timeZone + ' ' + new Date(0).getHours()</script>",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    for extra in [&[][..], &["--no-sandbox"]] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_pageon"))
            .arg("fetch")
            .arg(format!("file://{}", page.display()))
            .args(extra)
            .env("TZ", "Asia/Tokyo")
            .output()
            .unwrap_or_else(|e| panic!("{e}"));
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.trim_end().ends_with("UTC 0"), "{extra:?}: {text}");
    }
}

#[test]
fn floods_stay_within_the_engine_s_limits() {
    let body = "<p id=out>start</p><script>\
        (function again() { scheduler.postTask(again); scheduler.yield(); })();\
        document.body.animate([{}, {}], { duration: 16, iterations: Infinity });\
        setTimeout(() => out.textContent = 'still alive', 50);</script>";
    let env = Replay::new(site(vec![html("https://f.test/", body)]));
    #[allow(clippy::disallowed_methods)]
    let t = std::time::Instant::now();
    let doc = pageon::load("https://f.test/", &env, LoadOptions::default())
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    assert!(
        doc.summary().pending_timers > 0 || doc.summary().errors > 0,
        "{:?}",
        doc.summary()
    );
}

#[test]
#[ignore = "timing: run in release (cargo test --release -- --ignored)"]
fn animations_cost_no_wall_time_and_unused_apis_nothing() {
    let time = |body: &str| {
        (0..10)
            .map(|_| {
                let env = Replay::new(site(vec![html("https://t.test/", body)]));
                #[allow(clippy::disallowed_methods)]
                let t = std::time::Instant::now();
                let doc = pageon::load("https://t.test/", &env, LoadOptions::default())
                    .unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(text_of(&doc, "out"), "done");
                t.elapsed()
            })
            .min()
            .unwrap_or_default()
    };
    let plain = time("<p id=out></p><script>out.textContent = 'done'</script>");
    let animated = time(
        "<p id=out></p><script>out.animate([{opacity: 1}, {opacity: 0}], 300).finished.then(() => out.textContent = 'done')</script>",
    );
    eprintln!("a page {plain:?}; a 300 ms animation {animated:?}");
    assert!(
        animated.saturating_sub(plain).as_millis() < 5,
        "{animated:?} vs {plain:?}"
    );
}
