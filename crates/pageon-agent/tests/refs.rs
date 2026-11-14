//! Refs across scripted step sequences.

use std::collections::BTreeMap;
use std::path::Path;

use pageon_agent::{Budget, Capture, Episode, Mode, RefError};

fn steps(name: &str) -> Vec<Capture> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/corpus/sequences")
        .join(name);
    let mut out = Vec::new();
    for k in 0.. {
        let p = dir.join(format!("step-{k}.capture.json.gz"));
        if !p.exists() {
            break;
        }
        out.push(Capture::load(&p).unwrap_or_else(|e| panic!("{name} step {k}: {e}")));
    }
    assert!(
        !out.is_empty(),
        "sequence {name} missing; run `uv run pageon-bench corpus` in bench/"
    );
    out
}

/// `"name" → ref` for element lines of a full observation.
fn refs_by_name(obs: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in obs.lines() {
        let mut parts = line.trim_start().splitn(3, ' ');
        let (_role, r, rest) = (
            parts.next(),
            parts.next().unwrap_or_default(),
            parts.next().unwrap_or_default(),
        );
        if r.starts_with('e') && r[1..].chars().all(|c| c.is_ascii_digit()) && rest.starts_with('"')
        {
            let name = rest.split('"').nth(1).unwrap_or_default().to_string();
            m.insert(name, r.to_string());
        }
    }
    m
}

fn episode() -> Episode {
    Episode::new(Budget::o200k(16000), Mode::Full)
}

#[test]
fn stable_refs_survive_updates() {
    let caps = steps("stable");
    let mut ep = episode();
    let first = refs_by_name(&ep.observe(&caps[0]));
    let v0 = ep.version();
    for c in &caps[1..] {
        assert_eq!(refs_by_name(&ep.observe(c)), first);
    }
    assert!(ep.version() > v0, "text change must bump the version");
}

#[test]
fn relocation_rate_and_confidence() {
    let mut total = 0;
    let mut correct = 0;
    for (name, conf) in [("rerender-same", 1.0), ("rerender-moved", 0.4)] {
        let caps = steps(name);
        let mut ep = episode();
        let before = refs_by_name(&ep.observe(&caps[0]));
        let after = refs_by_name(&ep.observe(&caps[1]));
        for (label, r) in &before {
            if !label.starts_with("Item") && !label.starts_with("Action") {
                continue;
            }
            total += 1;
            let t = ep
                .resolve(r, &caps[1])
                .unwrap_or_else(|e| panic!("{name} {label}: {e}"));
            let rel = t
                .relocated
                .unwrap_or_else(|| panic!("{name} {label}: not reported as relocated"));
            assert!(
                (rel - conf).abs() < 1e-9,
                "{name} {label}: confidence {rel}"
            );
            if after.get(label) == Some(r) {
                correct += 1;
            } else {
                assert!(
                    rel < 0.5,
                    "{name} {label}: wrong relocation reported with confidence {rel}"
                );
            }
        }
    }
    assert!(total >= 40, "needs ≥ 40 replaced elements, have {total}");
    assert!(
        correct * 100 >= total * 95,
        "relocated correctly: {correct}/{total}"
    );
}

#[test]
fn removed_is_stale_with_step() {
    let caps = steps("removed");
    let mut ep = episode();
    let r = refs_by_name(&ep.observe(&caps[0]))["Delete message"].clone();
    ep.observe(&caps[1]);
    assert_eq!(
        ep.resolve(&r, &caps[1]),
        Err(RefError::Stale { since_step: 1 })
    );
}

#[test]
fn ambiguous_is_refused() {
    let caps = steps("ambiguous");
    let mut ep = episode();
    let r = refs_by_name(&ep.observe(&caps[0]))["Remove"].clone();
    ep.observe(&caps[1]);
    match ep.resolve(&r, &caps[1]) {
        Err(RefError::Ambiguous { candidates }) => assert_eq!(candidates.len(), 2),
        other => panic!("expected ambiguous, got {other:?}"),
    }
}

#[test]
fn inserted_elements_get_fresh_refs() {
    let caps = steps("inserted");
    let mut ep = episode();
    let before = refs_by_name(&ep.observe(&caps[0]));
    let obs = ep.observe(&caps[1]);
    let after = refs_by_name(&obs);
    let max_before = before
        .values()
        .filter_map(|r| r[1..].parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    let fresh = &after["Try again"];
    assert!(fresh[1..].parse::<u32>().unwrap_or(0) > max_before);
    assert!(
        obs.contains("alert \"Card declined. Try another card.\""),
        "{obs}"
    );
}

#[test]
fn navigation_resets_refs() {
    let caps = steps("navigate");
    let mut ep = episode();
    let r = refs_by_name(&ep.observe(&caps[0]))["Stay"].clone();
    ep.note_action("click", "e1", None);
    let obs = ep.observe(&caps[1]);
    assert!(
        obs.lines()
            .next()
            .unwrap_or_default()
            .contains("refs-reset"),
        "{obs}"
    );
    assert_eq!(ep.resolve(&r, &caps[1]), Err(RefError::Reset));
    let new_ref = &refs_by_name(&obs)["Stay"];
    assert_ne!(new_ref, &r, "refs are never recycled");
}

#[test]
fn unknown_and_malformed_refs() {
    let caps = steps("stable");
    let mut ep = episode();
    ep.observe(&caps[0]);
    assert_eq!(ep.resolve("e999", &caps[0]), Err(RefError::Unknown));
    assert_eq!(ep.resolve("click", &caps[0]), Err(RefError::Unknown));
}
