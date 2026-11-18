//! Storage layer for EngramDB.
//!
//! This crate handles all file-system + LanceDB operations for memories
//! (frontmatter markdown parsing/writing, index/manifest management, config
//! loading, project identity, path resolution) plus the runtime `telemetry`
//! stack persisted alongside the index.
//!
//! It is re-exported by the top-level `engramdb` crate under its historical
//! `storage` / `telemetry` module paths.

pub mod bash_retry;
pub mod config;
pub mod conversation_index;
pub mod digest;
pub mod edge_index;
pub mod error;
pub mod harvest_state;
pub mod hook_seen;
pub mod lance_index;
pub mod manifest;
pub mod memory_file;
pub mod paths;
pub mod project_id;
pub mod registry;
pub mod sql;
pub mod staleness;
mod state_file;
pub mod store;
pub mod store_target;
pub mod task_state;
pub mod telemetry;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod transcript_archive;
pub mod transcripts;
pub mod volume;
pub mod worktree;
pub mod write_lock;

pub use conversation_index::{ConversationHit, ConversationIndex, ConversationRow, MatchedOn};
pub use digest::FileDigest;
pub use edge_index::{EdgeField, EdgeRow};
pub use error::{Result, StorageError};
pub use lance_index::{
    FilterProjection, Fragmentation, IndexFilterable, IndexForFiltering, IndexOptimizeStats,
    IndexSummary, VectorMatch, WatchedEntry,
};
pub use manifest::{embedding_status, EmbeddingFingerprint, EmbeddingModelStatus, Manifest};
pub use project_id::detect_worktree_main;
pub use registry::{
    collect_descendants, conflicting_checkout_path, list_children, protected_project_ids,
    resolve_root_project_id, stale_registrations_for, subscriptions_of, FileRegistry, GroupEntry,
    InMemoryRegistry, Registry, RegistryBackend, RegistryEntry,
};
pub use store::{
    CrossDirDuplicate, CrossDirWinner, MemoryStore, ReindexCounts, SchemaState,
    UnparseableMemoryFile,
};
pub use store_target::{OpenMode, StoreSpec, StoreTarget};
pub use worktree::{consolidate_worktree_into_main, resolve_project_root};

// Test isolation: link `engram-test-support` so its `#[ctor]` redirects
// `ENGRAMDB_DATA_DIR` / `ENGRAMDB_CONFIG_DIR` to per-process temp dirs before
// any test runs. The explicit `arm()` reference keeps the linker from
// dead-stripping the constructor out of this crate's test binary.
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
        &crate::paths::global_data_dir().unwrap(),
        &crate::paths::global_config_dir().unwrap(),
    );
}

/// `serde_json` must be built **without** its `preserve_order` feature, so a
/// `serde_json::Map` (every `json!{}` object) sorts its keys. CLI and MCP JSON
/// output, the insta snapshots and the dashboard's pinned layout digests all
/// depend on that order. Cargo unifies features across the build, so any
/// dependency that turns the feature on changes it for every crate:
/// lancedb 0.41 does (`serde_json = { features = ["preserve_order"] }`),
/// which is one reason the workspace stays on lancedb 0.37. This crate depends on
/// lancedb directly, so its test build sees the same unified features.
#[cfg(test)]
#[test]
fn serde_json_maps_sort_their_keys() {
    let map = serde_json::json!({ "b": 1, "a": 2 });
    assert_eq!(
        map.to_string(),
        r#"{"a":2,"b":1}"#,
        "serde_json's preserve_order feature is on: a dependency enabled it \
         (find it with `cargo tree -e features -i serde_json`)"
    );
}
