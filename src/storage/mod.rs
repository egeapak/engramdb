//! Storage layer for EngramDB
//!
//! This module handles all file system operations for memories:
//! - Frontmatter markdown parsing/writing
//! - Index management
//! - Manifest updates
//! - Configuration loading
//! - Project identity computation
//! - Path resolution

pub mod config;
pub mod error;
pub mod lance_index;
pub mod manifest;
pub mod memory_file;
pub mod paths;
pub mod project_id;
pub mod registry;
pub mod store;
#[cfg(test)]
pub mod test_support;
pub mod worktree;
pub mod write_lock;

pub use error::{Result, StorageError};
pub use lance_index::{IndexFilterable, IndexForFiltering, IndexSummary, VectorMatch};
pub use manifest::{embedding_status, EmbeddingFingerprint, EmbeddingModelStatus, Manifest};
pub use project_id::detect_worktree_main;
pub use registry::{
    collect_descendants, list_children, resolve_root_project_id, FileRegistry, InMemoryRegistry,
    Registry, RegistryBackend, RegistryEntry,
};
pub use store::MemoryStore;
pub use worktree::{consolidate_worktree_into_main, resolve_project_root};
