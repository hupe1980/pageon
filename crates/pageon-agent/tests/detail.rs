//! The detail levels on the capture corpus: tokens per page at each level, and how many of
//! each page's reviewed expectations (`.case.toml` `must_contain`) each level still meets.
//! Run with `--nocapture` for the table.

use std::path::{Path, PathBuf};

use pageon_agent::{Budget, Capture, Detail, Episode, Mode};

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Case {
    budget: Option<usize>,
    must_contain: Vec<String>,
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn view(capture: &Capture, budget: Budget, detail: Detail) -> String {
    let mut e = Episode::new(budget, Mode::Full);
    e.set_detail(detail);
    e.observe(capture)
}

#[test]
fn compact_views_are_smaller_and_keep_every_element() {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(corpus())
        .expect("corpus")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".capture.json.gz"))
        .collect();
    paths.sort();
    let unbounded = Budget::o200k(1_000_000);
    let mut ratios = Vec::new();
    let (mut met_full, mut met_compact, mut expected) = (0, 0, 0);
    println!("| Page | full | compact | ratio | expectations full | compact |");
    println!("| --- | --- | --- | --- | --- | --- |");
    for path in &paths {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .trim_end_matches(".capture.json.gz")
            .to_string();
        let capture = Capture::load(path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let full = view(&capture, unbounded, Detail::Full);
        let compact = view(&capture, unbounded, Detail::Compact);
        let (tf, tc) = (unbounded.count(&full), unbounded.count(&compact));
        assert!(
            compact
                .lines()
                .next()
                .unwrap_or("")
                .ends_with("detail:compact"),
            "{name}: the header says compact"
        );
        // Every element, heading, landmark, alert and frame line of the full view is kept.
        for l in full.lines().skip(1) {
            let t = l.trim_start();
            if t.starts_with("text ") || t.starts_with("hidden-text ") {
                continue;
            }
            assert!(compact.contains(l), "{name}: compact lost {l:?}");
        }
        let case: Case = std::fs::read_to_string(corpus().join(format!("{name}.case.toml")))
            .map(|t| toml::from_str(&t).unwrap_or_default())
            .unwrap_or_default();
        let budget = Budget::o200k(case.budget.unwrap_or(4000));
        let (bf, bc) = (
            view(&capture, budget, Detail::Full),
            view(&capture, budget, Detail::Compact),
        );
        let mf = case
            .must_contain
            .iter()
            .filter(|m| bf.contains(m.as_str()))
            .count();
        let mc = case
            .must_contain
            .iter()
            .filter(|m| bc.contains(m.as_str()))
            .count();
        expected += case.must_contain.len();
        met_full += mf;
        met_compact += mc;
        let n = case.must_contain.len();
        println!(
            "| {name} | {tf} | {tc} | {:.2} | {mf}/{n} | {mc}/{n} |",
            tc as f64 / tf.max(1) as f64
        );
        if name.starts_with("real-") {
            ratios.push(tc as f64 / tf.max(1) as f64);
        }
    }
    ratios.sort_by(f64::total_cmp);
    let median = ratios[ratios.len() / 2];
    println!(
        "real pages: median compact/full {median:.2}; expectations met: full {met_full}/{expected}, compact {met_compact}/{expected}"
    );
    assert!(ratios.len() >= 10, "the corpus has its real pages");
    assert!(
        median <= 0.75,
        "compact saves at least 25 % (median {median:.2})"
    );
}
