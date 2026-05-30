pub mod daemon;
pub mod ops;
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
pub use engram_onnx as onnx_ep;
pub use engram_storage as storage;
pub use engram_storage::telemetry;
pub use engram_types as types;

// Test isolation: link `engram-test-support` so its `#[ctor]` redirects
// `ENGRAMDB_DATA_DIR` / `ENGRAMDB_CONFIG_DIR` to per-process temp dirs before
// any test runs (nextest's process-per-test makes this load-bearing). The
// `arm()` reference keeps the linker from dead-stripping the constructor.
#[cfg(test)]
#[ctor::ctor]
fn arm_test_isolation() {
    engram_test_support::arm();
}
