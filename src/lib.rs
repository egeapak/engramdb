pub mod cli;
pub mod daemon;
pub mod embeddings;
pub mod mcp;
pub mod nli;
pub mod ops;
pub mod retrieval;
pub mod scope;
pub mod scoring;
pub mod search;
pub mod storage;
pub mod telemetry;
pub mod title;

// Extracted workspace crates, re-exported under their historical module paths so
// every `crate::<module>::…` / `engramdb::<module>::…` reference keeps resolving
// unchanged after the extraction.
pub use engram_onnx as onnx_ep;
pub use engram_types as types;

/// Test isolation: redirect global data and config dirs to per-process temp directories.
///
/// Since nextest runs each test in its own process, this ensures no test
/// pollutes the real `~/Library/Application Support/engramdb/` directory
/// or reads the user's real config/registry.
///
/// The `TempDir` handles are held in statics so they persist (and are cleaned
/// up) when the process exits.
#[cfg(test)]
mod test_isolation {
    use std::sync::LazyLock;

    static TEST_DATA_DIR: LazyLock<tempfile::TempDir> =
        LazyLock::new(|| tempfile::TempDir::new().expect("failed to create test data dir"));
    static TEST_CONFIG_DIR: LazyLock<tempfile::TempDir> =
        LazyLock::new(|| tempfile::TempDir::new().expect("failed to create test config dir"));

    #[ctor::ctor]
    fn init() {
        std::env::set_var("ENGRAMDB_DATA_DIR", TEST_DATA_DIR.path());
        std::env::set_var("ENGRAMDB_CONFIG_DIR", TEST_CONFIG_DIR.path());
    }
}
