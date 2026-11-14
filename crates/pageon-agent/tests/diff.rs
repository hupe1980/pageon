//! Diff exactness over scripted sequences.

use std::path::Path;

use pageon_agent::{Budget, Capture, Episode, Mode, diff};

fn sequences() -> Vec<(String, Vec<Capture>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/sequences");
    let mut names: Vec<String> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("{e}; run `uv run pageon-bench corpus` in bench/"))
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|n| {
            let mut caps = Vec::new();
            for k in 0.. {
                let p = root.join(&n).join(format!("step-{k}.capture.json.gz"));
                if !p.exists() {
                    break;
                }
                caps.push(Capture::load(&p).unwrap_or_else(|e| panic!("{e}")));
            }
            (n, caps)
        })
        .collect()
}

#[test]
fn applying_a_diff_reproduces_the_next_observation() {
    for budget in [300, 4000] {
        for (name, caps) in sequences() {
            let mut ep = Episode::new(Budget::o200k(budget), Mode::Full);
            ep.observe(&caps[0]);
            let mut prev = ep.last_lines().unwrap_or_default().to_vec();
            for c in &caps[1..] {
                ep.observe(c);
                let next = ep.last_lines().unwrap_or_default().to_vec();
                let ops = diff::diff(&prev, &next);
                assert_eq!(diff::apply(&prev, &ops), next, "{name} @ {budget}");
                prev = next;
            }
        }
    }
}

#[test]
fn diff_mode_sends_diffs_fallbacks_and_resets() {
    for (name, caps) in sequences() {
        let mut ep = Episode::new(Budget::o200k(4000), Mode::Diff);
        let first = ep.observe(&caps[0]);
        assert!(
            first.starts_with("@page"),
            "{name}: first observation is full"
        );
        for c in &caps[1..] {
            ep.note_action("click", "e1", None);
            let obs = ep.observe(c);
            let mut lines = obs.lines();
            let outcome = lines.next().unwrap_or_default();
            assert!(outcome.starts_with("ok click e1 v"), "{name}: {outcome}");
            let body = lines.next().unwrap_or_default();
            if name == "navigate" {
                assert!(
                    outcome.ends_with("refs-reset") && body.starts_with("@page"),
                    "{name}: {obs}"
                );
            } else {
                assert!(
                    body.starts_with("@diff") || body.contains("full:fallback"),
                    "{name}: {obs}"
                );
            }
        }
    }
}

#[test]
fn large_changes_fall_back_to_full() {
    let (_, caps) = sequences()
        .into_iter()
        .find(|(n, _)| n == "rerender-moved")
        .unwrap_or_else(|| panic!("missing"));
    let mut ep = Episode::new(Budget::o200k(4000), Mode::Diff);
    ep.observe(&caps[0]);
    let obs = ep.observe(&caps[1]);
    // Ten buttons move into a new landmark: either a compact diff or the marked full fallback,
    // never a diff larger than 40 % of the full observation.
    if !obs.contains("full:fallback") {
        let full = Episode::new(Budget::o200k(4000), Mode::Full).observe(&caps[1]);
        assert!(
            pageon_agent::count_tokens(&obs) as f64
                <= 0.4 * pageon_agent::count_tokens(&full) as f64 + 5.0
        );
    }
}
