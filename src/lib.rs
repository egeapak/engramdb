pub mod daemon;
pub mod ops;
pub mod prompt_intent;
pub mod retrieval;
pub mod scope;
pub mod scoring;
pub mod search;

// Extracted workspace crates, re-exported under their historical module paths so
// every `crate::<module>::…` / `engramdb::<module>::…` reference keeps resolving
// unchanged. The `cli` and `mcp` front-ends are their own crates
// (`engram-cli`, `engram-mcp`) that depend on this core, so they are not
// re-exported here (that would invert the dependency).
pub use engram_models::{embeddings, nli, title};
#[cfg(feature = "onnxruntime")]
pub use engram_onnx as onnx_ep;
pub use engram_storage as storage;
pub use engram_storage::telemetry;
pub use engram_types as types;

// Test isolation: link `engram-test-support` so its `#[ctor]` redirects
// `ENGRAMDB_DATA_DIR` / `ENGRAMDB_CONFIG_DIR` to per-process temp dirs before
// any test runs (nextest's process-per-test makes this load-bearing). The
// `arm()` reference keeps the linker from dead-stripping the constructor.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn arm_test_isolation() {
    engram_test_support::arm();
}

/// The `#[ctor]` above really ran in this test binary: the product resolves
/// its data and config dirs under the temp dir, never the user's.
#[cfg(test)]
#[test]
fn the_data_and_config_dirs_are_isolated() {
    engram_test_support::assert_isolated(
        &crate::storage::paths::global_data_dir().unwrap(),
        &crate::storage::paths::global_config_dir().unwrap(),
    );
}

/// No test moves the isolation variables. The `#[ctor]` arm sets them once per
/// process; a test that re-points `ENGRAMDB_DATA_DIR` (and does not restore
/// it, or restores it while another test thread reads it) changes every other
/// test of a `cargo test` process, which is what made two `--lib` tests flaky.
/// Tests that need data apart from others use a fresh project id instead.
#[cfg(test)]
#[test]
fn no_test_mutates_the_global_dirs() {
    use std::path::{Path, PathBuf};
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(&root.join("src"), &mut files);
    for krate in std::fs::read_dir(root.join("crates")).unwrap().flatten() {
        if krate.file_name() == "engram-test-support" {
            continue;
        }
        walk(&krate.path().join("src"), &mut files);
    }
    assert!(
        files.len() > 50,
        "the scan found too few files: {}",
        files.len()
    );
    // Built from pieces so this file does not match itself.
    let vars =
        ["DATA_DIR", "CONFIG_DIR", "DASHBOARD_CACHE_DIR"].map(|v| format!("\"ENGRAMDB_{v}\""));
    let mut found = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap();
        for call in ["set_var(", "remove_var("] {
            for (at, _) in text.match_indices(call) {
                let arg = text[at + call.len()..].trim_start();
                if vars.iter().any(|v| arg.starts_with(v.as_str())) {
                    let line = text[..at].lines().count();
                    found.push(format!("{}:{line}", f.display()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "tests must not move the isolation variables (use a fresh project id): {found:#?}"
    );
}
