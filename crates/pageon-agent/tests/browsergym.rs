//! BrowserGym observation objects: bids are the episode's refs, in the AX tree and
//! the DOM snapshot alike; frames merge into one tree; extra properties cover every bid.

use std::collections::BTreeSet;
use std::path::Path;

use pageon_agent::{Budget, Capture, Episode, Mode, browsergym};
use serde_json::Value;

fn corpus() -> Vec<(String, Capture)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.to_string_lossy().ends_with(".capture.json.gz") {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            out.push((name, Capture::load(&p).unwrap_or_else(|e| panic!("{e}"))));
        }
    }
    assert!(out.len() >= 8);
    out
}

/// Every `browsergym_id` reachable from the root, depth first (as `flatten_axtree_to_str`).
fn reachable_bids(ax: &Value) -> BTreeSet<String> {
    let nodes = ax["nodes"].as_array().cloned().unwrap_or_default();
    let by_id: std::collections::HashMap<String, &Value> = nodes
        .iter()
        .map(|n| (n["nodeId"].as_str().unwrap_or_default().to_string(), n))
        .collect();
    let mut out = BTreeSet::new();
    let mut stack = vec![nodes.first().cloned().unwrap_or(Value::Null)];
    let mut seen = std::collections::HashSet::new();
    while let Some(n) = stack.pop() {
        if !seen.insert(n["nodeId"].as_str().unwrap_or_default().to_string()) {
            continue;
        }
        if let Some(b) = n["browsergym_id"].as_str() {
            out.insert(b.to_string());
        }
        for c in n["childIds"].as_array().into_iter().flatten() {
            if let Some(child) = c.as_str().and_then(|c| by_id.get(c)) {
                stack.push((*child).clone());
            }
        }
    }
    out
}

fn dom_bids(dom: &Value) -> BTreeSet<String> {
    let strings: Vec<&str> = dom["strings"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| s.as_str().unwrap_or_default())
        .collect();
    let mut out = BTreeSet::new();
    for d in dom["documents"].as_array().into_iter().flatten() {
        for attrs in d["nodes"]["attributes"].as_array().into_iter().flatten() {
            let a: Vec<i64> = attrs
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_i64)
                .collect();
            for p in a.chunks(2) {
                if p.len() == 2 && strings[p[0] as usize] == "bid" {
                    out.insert(strings[p[1] as usize].to_string());
                }
            }
        }
    }
    out
}

#[test]
fn bids_are_the_episodes_refs_everywhere() {
    for (name, capture) in corpus() {
        let mut episode = Episode::new(Budget::o200k(100_000), Mode::Diff);
        let first = episode.observe(&capture);
        let obs = browsergym(&mut episode, &capture);
        let refs: BTreeSet<String> = episode
            .elements(&capture)
            .into_iter()
            .map(|e| e.reference)
            .collect();
        assert!(!refs.is_empty(), "{name}");
        let ax = reachable_bids(&obs["axtree_object"]);
        let dom = dom_bids(&obs["dom_object"]);
        let extra: BTreeSet<String> = obs["extra_element_properties"]
            .as_object()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        assert_eq!(dom, refs, "{name}: DOM bids");
        assert_eq!(extra, refs, "{name}: extra properties");
        assert!(
            ax.is_subset(&refs) && ax.len() * 10 >= refs.len() * 9,
            "{name}: AX bids {} of {}",
            ax.len(),
            refs.len()
        );
        // Every ref the agent saw is a bid, and the episode's next diff is unaffected.
        for r in &refs {
            assert!(first.contains(r.as_str()), "{name}: {r}");
        }
        let full = episode.full_view(&capture);
        assert_eq!(full, first, "{name}: the full view is the agent's view");
        let next = episode.observe(&capture);
        assert!(
            !next.contains("@page"),
            "{name}: unchanged page gives an empty diff, got {next}"
        );
    }
}

#[test]
fn frames_merge_and_properties_make_sense() {
    let (_, capture) = corpus()
        .into_iter()
        .find(|(n, _)| n.starts_with("nested-frames"))
        .unwrap_or_else(|| panic!("nested-frames"));
    let mut episode = Episode::new(Budget::o200k(100_000), Mode::Full);
    let obs = browsergym(&mut episode, &capture);
    let nodes = obs["axtree_object"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        nodes
            .iter()
            .any(|n| n["nodeId"].as_str().is_some_and(|i| i.starts_with("f1:")))
    );
    // Child frame elements are reachable from the root.
    let framed: BTreeSet<String> = nodes
        .iter()
        .filter(|n| n["nodeId"].as_str().is_some_and(|i| i.starts_with('f')))
        .filter_map(|n| n["browsergym_id"].as_str().map(str::to_string))
        .collect();
    assert!(!framed.is_empty());
    assert!(framed.is_subset(&reachable_bids(&obs["axtree_object"])));
    for (bid, p) in obs["extra_element_properties"]
        .as_object()
        .into_iter()
        .flatten()
    {
        let v = p["visibility"].as_f64().unwrap_or(-1.0);
        assert!((0.0..=1.0).contains(&v), "{bid}: {p}");
        assert_eq!(p["set_of_marks"].as_bool(), Some(v > 0.5), "{bid}");
    }
}
