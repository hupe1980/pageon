//! Serialization round trips: fixture pages in one step, adversarial html5lib inputs from the
//! second round on.

use pageon_dom::{ParseOptions, parse_document};

fn inputs() -> Vec<String> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/html5lib");
    let mut v = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if e.path().extension().is_some_and(|x| x == "dat") {
            let t = std::fs::read_to_string(e.path()).unwrap_or_default();
            let mut take = false;
            let mut cur = String::new();
            for line in t.split('\n') {
                if line == "#data" {
                    take = true;
                    cur.clear();
                } else if line.starts_with('#') && take {
                    take = false;
                    v.push(cur.clone());
                } else if take {
                    cur.push_str(line);
                    cur.push('\n');
                }
            }
        }
    }
    v
}

fn ser(x: &str) -> String {
    let d = parse_document(x, ParseOptions::default());
    d.to_html(d.root())
}

/// Real pages (here: the constructed fixture pages) round-trip in one step.
#[test]
fn fixture_pages_round_trip() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/fixtures");
    let mut n = 0;
    let mut stack = vec![fixtures];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "html") {
                let x = std::fs::read_to_string(&p).unwrap_or_default();
                let s1 = ser(&x);
                assert_eq!(ser(&s1), s1, "{}", p.display());
                n += 1;
            }
        }
    }
    assert!(n >= 20, "fixture pages missing: {n}");
}

/// Adversarial html5lib inputs: serialization is stable from the second round on. One round
/// may change mis-nested markup (foster parenting, adoption agency), doctypes (the standard
/// serializes only the name, which can change the quirks mode) and leading newlines in
/// `<pre>`/`<textarea>` — known limits of HTML serialization. `<plaintext>` never round-trips:
/// it consumes the rest of the input, so its serialized end tag cannot be honoured.
#[test]
fn serialization_is_stable() {
    let mut all = inputs();
    assert!(all.len() > 1500);
    all.retain(|x| !x.to_ascii_lowercase().contains("<plaintext"));
    let bad: Vec<String> = all
        .iter()
        .filter_map(|x| {
            let s2 = ser(&ser(x));
            let s3 = ser(&s2);
            (s2 != s3).then(|| format!("{x:?}\n  s2: {s2:?}\n  s3: {s3:?}"))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "{} inputs not stable:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn well_formed_documents_round_trip_in_one_step() {
    for x in [
        "<!DOCTYPE html><html><head><title>t</title></head><body><p class=\"a\">Hi &amp; bye</p></body></html>",
        "<!DOCTYPE html><html><head></head><body><template><b>in template</b></template><svg viewBox=\"0 0 1 1\"><path d=\"M0\"></path></svg></body></html>",
    ] {
        let a = parse_document(x, ParseOptions::default());
        assert_eq!(a.to_html(a.root()), x);
    }
}
