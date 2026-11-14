//! Golden tests over the capture corpus: every capture renders to its
//! reviewed snapshot, stays within budget, and meets its `.case.toml` expectations.

use std::path::{Path, PathBuf};

use pageon_agent::{Budget, Capture, ax};

#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Case {
    budget: Option<usize>,
    must_contain: Vec<String>,
    must_not_contain: Vec<String>,
    forbidden_line_prefixes: Vec<String>,
    flagged: Vec<String>,
    first_line_prefix: Option<String>,
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn captures() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(corpus())
        .unwrap_or_else(|e| {
            panic!("corpus missing: {e}; run `uv run pageon-bench corpus` in bench/")
        })
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".capture.json.gz"))
        .collect();
    v.sort();
    v
}

fn stem(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .trim_end_matches(".capture.json.gz")
        .to_string()
}

#[test]
fn golden_snapshots_and_expectations() {
    let all = captures();
    assert!(
        all.len() >= 10,
        "golden corpus needs ≥ 10 page captures, found {}",
        all.len()
    );
    for path in all {
        let name = stem(&path);
        let case: Case = std::fs::read_to_string(corpus().join(format!("{name}.case.toml")))
            .map(|t| toml::from_str(&t).unwrap_or_else(|e| panic!("{name}.case.toml: {e}")))
            .unwrap_or_default();
        let budget = Budget::o200k(case.budget.unwrap_or(4000));
        let capture = Capture::load(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let text = ax(&capture, budget);
        assert!(
            budget.count(&text) <= budget.max_tokens,
            "{name}: over budget"
        );
        for s in &case.must_contain {
            assert!(text.contains(s.as_str()), "{name}: missing {s:?}\n{text}");
        }
        for s in &case.must_not_contain {
            assert!(
                !text.contains(s.as_str()),
                "{name}: must not contain {s:?}\n{text}"
            );
        }
        for p in &case.forbidden_line_prefixes {
            assert!(
                !text.lines().any(|l| l.trim_start().starts_with(p.as_str())),
                "{name}: a line starts with {p:?}"
            );
        }
        for f in &case.flagged {
            assert!(
                text.lines()
                    .any(|l| l.starts_with("flagged hidden-field") && l.contains(f.as_str())),
                "{name}: {f} not flagged\n{text}"
            );
        }
        if let Some(prefix) = &case.first_line_prefix {
            let first = text.lines().nth(1).unwrap_or_default();
            assert!(
                first.starts_with(prefix.as_str()),
                "{name}: first line {first:?}"
            );
        }
        insta::with_settings!({ snapshot_suffix => name.clone(), prepend_module_to_snapshot => false }, {
            insta::assert_snapshot!("ax", text);
        });
    }
}
