//! The committed bindings are what the generator produces from the pinned IDL and the
//! allowlist (SC-007). Regenerate with `cargo xtask idl`.

#[test]
fn generated_bindings_are_fresh() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let want = pageon_idl::generate(&dir.join("idl"))
        .unwrap_or_else(|e| panic!("generator failed: {e:?}"));
    let have = std::fs::read_to_string(dir.join("src/generated.rs")).unwrap_or_default();
    assert!(
        have == want,
        "src/generated.rs is stale: run `cargo xtask idl`"
    );
}
