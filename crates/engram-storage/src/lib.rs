//! Storage layer for EngramDB.
//!
//! This crate handles all file-system + LanceDB operations for memories
//! (frontmatter markdown parsing/writing, index/manifest management, config
//! loading, project identity, path resolution) plus the runtime `telemetry`
//! stack persisted alongside the index.
//!
//! It is re-exported by the top-level `engramdb` crate under its historical
//! `storage` / `telemetry` module paths.

pub mod config;
pub mod error;
pub mod lance_index;
pub mod manifest;
pub mod memory_file;
pub mod paths;
pub mod project_id;
pub mod registry;
pub mod store;
pub mod telemetry;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod worktree;
pub mod write_lock;

pub use error::{Result, StorageError};
pub use lance_index::{
    IndexFilterable, IndexForFiltering, IndexOptimizeStats, IndexSummary, VectorMatch,
};
pub use manifest::{embedding_status, EmbeddingFingerprint, EmbeddingModelStatus, Manifest};
pub use project_id::detect_worktree_main;
pub use registry::{
    collect_descendants, conflicting_checkout_path, list_children, resolve_root_project_id,
    FileRegistry, InMemoryRegistry, Registry, RegistryBackend, RegistryEntry,
};
pub use store::MemoryStore;
pub use worktree::{consolidate_worktree_into_main, resolve_project_root};

// Test isolation: link `engram-test-support` so its `#[ctor]` redirects
// `ENGRAMDB_DATA_DIR` / `ENGRAMDB_CONFIG_DIR` to per-process temp dirs before
// any test runs. The explicit `arm()` reference keeps the linker from
// dead-stripping the constructor out of this crate's test binary.
#[cfg(test)]
#[ctor::ctor]
fn arm_test_isolation() {
    engram_test_support::arm();
}
