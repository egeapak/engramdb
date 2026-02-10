//! Storage layer for EngramDB
//!
//! This module handles all file system operations for memories:
//! - Frontmatter markdown parsing/writing
//! - Index management
//! - Manifest updates
//! - Configuration loading
//! - Project identity computation
//! - Path resolution

pub mod error;
pub mod memory_file;
pub mod index;
pub mod manifest;
pub mod config;
pub mod project_id;
pub mod paths;
pub mod store;

pub use error::{Result, StorageError};
pub use store::MemoryStore;
pub use index::{Index, IndexEntry};
pub use manifest::Manifest;
