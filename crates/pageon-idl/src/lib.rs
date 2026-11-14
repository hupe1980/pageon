//! pageon's WebIDL binding generator: pinned IDL plus an allowlist in, `generated.rs` out.
//!
//! Used by `cargo xtask idl` (writes the file) and by pageon-js's freshness test (compares).

pub mod emit;
pub mod model;

use std::path::Path;

/// Generates `generated.rs` from the IDL directory (with its `allowlist.toml`).
pub fn generate(idl_dir: &Path) -> Result<String, Vec<String>> {
    let model = model::load(idl_dir).map_err(|e| vec![e])?;
    let text = std::fs::read_to_string(idl_dir.join("allowlist.toml"))
        .map_err(|e| vec![format!("allowlist.toml: {e}")])?;
    let table: toml::Table =
        toml::from_str(&text).map_err(|e| vec![format!("allowlist.toml: {e}")])?;
    let mut allow = emit::Allow::new();
    for (owner, v) in table {
        let members = v
            .get("members")
            .and_then(|m| m.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        allow.insert(owner, members);
    }
    emit::generate(&model, &allow)
}
