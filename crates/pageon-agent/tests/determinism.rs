//! Byte-identical output from stored captures; `captured_at` never matters.

use std::path::Path;

use pageon_agent::{Budget, Capture, Episode, Mode, ax};

#[test]
fn rendering_is_deterministic_and_ignores_capture_time() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        if !p.to_string_lossy().ends_with(".capture.json.gz") {
            continue;
        }
        let a = Capture::load(&p).unwrap_or_else(|e| panic!("{e}"));
        let mut b = a.clone();
        b.meta.captured_at = "1999-01-01T00:00:00Z".into();
        let budget = Budget::o200k(2000);
        assert_eq!(ax(&a, budget), ax(&b, budget), "{}", p.display());
        let mut e1 = Episode::new(budget, Mode::Diff);
        let mut e2 = Episode::new(budget, Mode::Diff);
        assert_eq!(e1.observe(&a), e2.observe(&b));
    }
}
