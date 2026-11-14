//! Repository tasks.
//!
//! - `cargo xtask idl`: regenerates `crates/pageon-js/src/generated.rs` from the pinned WebIDL
//!   and `idl/allowlist.toml`.
//! - `cargo xtask idl --check`: fails if the committed file is stale.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/pageon-js");
    match args.first().map(String::as_str) {
        Some("idl") => {
            let out = match pageon_idl::generate(&root.join("idl")) {
                Ok(o) => o,
                Err(errors) => {
                    for e in errors {
                        eprintln!("error: {e}");
                    }
                    return ExitCode::FAILURE;
                }
            };
            let path = root.join("src/generated.rs");
            if args.iter().any(|a| a == "--check") {
                if std::fs::read_to_string(&path).unwrap_or_default() == out {
                    println!("generated.rs is up to date");
                    ExitCode::SUCCESS
                } else {
                    eprintln!("generated.rs is stale: run `cargo xtask idl`");
                    ExitCode::FAILURE
                }
            } else {
                match std::fs::write(&path, &out) {
                    Ok(()) => {
                        println!("wrote {} ({} lines)", path.display(), out.lines().count());
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        ExitCode::FAILURE
                    }
                }
            }
        }
        _ => {
            eprintln!("usage: cargo xtask idl [--check]");
            ExitCode::FAILURE
        }
    }
}
