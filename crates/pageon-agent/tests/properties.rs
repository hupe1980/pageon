//! Properties of every view over every capture: within budget, no line
//! starts with a quote, every quoted run is a JSON string, element lines follow the grammar, and
//! malformed captures are errors, never panics.

use std::path::{Path, PathBuf};

use pageon_agent::{Budget, Capture, ax};

fn all_captures() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.to_string_lossy().ends_with(".capture.json.gz") {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus"),
        &mut v,
    );
    v.sort();
    v
}

/// Splits a line into quoted runs; returns false if quotes are unbalanced.
fn quoted_runs(line: &str) -> Option<Vec<String>> {
    let mut runs = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut escaped = false;
        let mut end = None;
        for (j, d) in chars.by_ref() {
            if escaped {
                escaped = false;
            } else if d == '\\' {
                escaped = true;
            } else if d == '"' {
                end = Some(j);
                break;
            }
        }
        let j = end?;
        runs.push(line[i..=j].to_string());
    }
    Some(runs)
}

const ROLES: &[&str] = &[
    "button",
    "link",
    "textbox",
    "searchbox",
    "checkbox",
    "radio",
    "switch",
    "slider",
    "spinbutton",
    "combobox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "tab",
    "treeitem",
    "disclosure",
    "color",
    "date",
    "clickable",
    "dialog",
    "alertdialog",
    "listbox",
    "menu",
    "menubar",
    "tablist",
    "radiogroup",
    "tree",
    "grid",
];

#[test]
fn every_view_meets_the_grammar() {
    let caps = all_captures();
    assert!(!caps.is_empty());
    for path in caps {
        let capture = Capture::load(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for max in [200, 1000, 4000, 16000] {
            let budget = Budget::o200k(max);
            let text = ax(&capture, budget);
            assert!(
                budget.count(&text) <= max,
                "{} @ {max}: over budget",
                path.display()
            );
            for line in text.lines() {
                let t = line.trim_start();
                assert!(!t.starts_with('"'), "line starts with a quote: {line}");
                let runs = quoted_runs(t).unwrap_or_else(|| panic!("unbalanced quotes: {line}"));
                for r in runs {
                    serde_json::from_str::<String>(&r)
                        .unwrap_or_else(|e| panic!("bad quoted run {r}: {e}"));
                }
                let first = t.split(' ').next().unwrap_or_default();
                if ROLES.contains(&first) {
                    let second = t.split(' ').nth(1).unwrap_or_default();
                    assert!(
                        second.starts_with('e') && second[1..].chars().all(|c| c.is_ascii_digit()),
                        "element line without ref: {line}"
                    );
                }
            }
        }
    }
}

#[test]
fn malformed_captures_are_errors_not_panics() {
    let good = all_captures()
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("empty corpus"));
    let text = {
        use std::io::Read;
        let mut s = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(good).unwrap_or_else(|e| panic!("{e}")))
            .read_to_string(&mut s)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    };
    let cases = [
        text[..text.len() / 2].to_string(),
        text.replacen("\"schema\":1", "\"schema\":2", 1),
        text.replacen("\"frames\":[", "\"frames\":7,\"x\":[", 1),
        "{}".to_string(),
        "[]".to_string(),
        "{\"schema\":1}".to_string(),
    ];
    for c in cases {
        assert!(Capture::from_json(&c).is_err());
    }
    // A structurally valid capture with dangling references renders without panicking.
    let mut v: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
    if let Some(nodes) = v
        .pointer_mut("/frames/0/ax_nodes")
        .and_then(|n| n.as_array_mut())
    {
        for n in nodes.iter_mut() {
            n["childIds"] = serde_json::json!(["missing-1", "missing-2"]);
        }
    }
    let c = Capture::from_json(&v.to_string()).unwrap_or_else(|e| panic!("{e}"));
    let _ = ax(&c, Budget::o200k(500));
}
