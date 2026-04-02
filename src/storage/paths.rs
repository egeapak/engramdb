//! Path resolution utilities for EngramDB storage locations.
//!
//! This module provides functions to resolve all EngramDB storage paths:
//! - Project-local paths (.engramdb/, .engramdb/memories/)
//! - Global config dir (platform-specific via `dirs::config_dir()`):
//!   - macOS: `~/Library/Application Support/engramdb/`
//!   - Linux: `$XDG_CONFIG_HOME/engramdb/` (default `~/.config/engramdb/`)
//! - Global data dir (platform-specific via `dirs::data_dir()`):
//!   - macOS: `~/Library/Application Support/engramdb/`
//!   - Linux: `$XDG_DATA_HOME/engramdb/` (default `~/.local/share/engramdb/`)
//! - Personal project paths (`<global_data_dir>/projects/{id}/personal/`)
//! - LanceDB vector storage paths (`<global_data_dir>/projects/{id}/lancedb/`)
//! - Registry path
//!
//! Functions that depend on platform directories return `Result<PathBuf>` so
//! callers can handle the (rare) case where the platform directory
//! cannot be determined.

use super::error::{Result, StorageError};
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;

/// Returns the project-specific EngramDB directory (.engramdb/)
pub fn project_dir(dir: &Path) -> PathBuf {
    dir.join(".engramdb")
}

/// Returns the shared memories directory in the project
pub fn memories_dir(dir: &Path) -> PathBuf {
    project_dir(dir).join("memories")
}

/// Returns the global configuration directory (platform-specific).
///
/// - macOS: `~/Library/Application Support/engramdb/`
/// - Linux: `$XDG_CONFIG_HOME/engramdb/` (default `~/.config/engramdb/`)
///
/// Used only for the global registry and future global settings.
/// Respects `ENGRAMDB_CONFIG_DIR` env var for testing isolation.
pub fn global_config_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("ENGRAMDB_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }
    dirs::config_dir()
        .ok_or_else(|| StorageError::Validation("Could not determine config directory".to_string()))
        .map(|p| p.join("engramdb"))
}

/// Returns the global data directory (platform-specific).
///
/// - macOS: `~/Library/Application Support/engramdb/`
/// - Linux: `$XDG_DATA_HOME/engramdb/` (default `~/.local/share/engramdb/`)
///
/// Used for per-project personal memories and LanceDB indices.
pub fn global_data_dir() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("ENGRAMDB_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    dirs::data_dir()
        .ok_or_else(|| StorageError::Validation("Could not determine data directory".to_string()))
        .map(|p| p.join("engramdb"))
}

/// Returns the model cache directory (platform-specific).
///
/// - macOS: `~/Library/Caches/engramdb/models/`
/// - Linux: `$XDG_CACHE_HOME/engramdb/models/` (default `~/.cache/engramdb/models/`)
///
/// Used for embedding models, reranker models, and NLI models.
pub fn model_cache_dir() -> Result<PathBuf> {
    dirs::cache_dir()
        .ok_or_else(|| StorageError::Validation("Could not determine cache directory".to_string()))
        .map(|p| p.join("engramdb").join("models"))
}

/// Returns the personal project directory for a given project ID
pub fn personal_dir(project_id: &str) -> Result<PathBuf> {
    Ok(global_data_dir()?
        .join("projects")
        .join(project_id)
        .join("personal"))
}

/// Returns the personal memories directory for a given project ID
pub fn personal_memories_dir(project_id: &str) -> Result<PathBuf> {
    Ok(personal_dir(project_id)?.join("memories"))
}

/// Returns the global LanceDB directory for a given project ID.
pub fn lancedb_dir(project_id: &str) -> Result<PathBuf> {
    Ok(global_data_dir()?
        .join("projects")
        .join(project_id)
        .join("lancedb"))
}

/// Well-known project ID for the global memory store.
///
/// This is 16 characters (matching the project ID format) but starts with
/// underscores so it can never collide with a real SHA-256-derived hex ID.
pub const GLOBAL_PROJECT_ID: &str = "__global_store__";

/// Returns the root directory for the global memory store.
///
/// Layout mirrors a normal project:
///   `<global_data_dir>/global/.engramdb/memories/`
///   `<global_data_dir>/global/.engramdb/manifest.toml`
///   `<global_data_dir>/global/.engramdb/config.toml`
pub fn global_store_dir() -> Result<PathBuf> {
    Ok(global_data_dir()?.join("global"))
}

/// Returns the LanceDB directory for the global memory store.
pub fn global_lancedb_dir() -> Result<PathBuf> {
    Ok(global_data_dir()?.join("global").join("lancedb"))
}

/// Returns the global registry path (`<global_config_dir>/registry.json`).
///
/// Respects `ENGRAMDB_REGISTRY_PATH` env var for testing isolation.
pub fn registry_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("ENGRAMDB_REGISTRY_PATH") {
        return Ok(PathBuf::from(path));
    }
    Ok(global_config_dir()?.join("registry.json"))
}

/// Returns the memory file path for a given memory ID
/// Note: This function tries to find the memory in both shared and personal directories
/// by checking for files that start with the given ID prefix.
pub async fn memory_path(dir: &Path, id: &str) -> Option<PathBuf> {
    // Try shared memories first
    let shared_dir = memories_dir(dir);
    if let Some(path) = find_memory_in_dir(&shared_dir, id).await {
        return Some(path);
    }

    // Try personal memories
    // We need to compute project_id to find personal dir
    // For simplicity, we'll use the compute_project_id from project_id module
    use crate::storage::project_id::compute_project_id;
    let project_id = compute_project_id(dir);
    if let Ok(personal_dir) = personal_memories_dir(&project_id) {
        return find_memory_in_dir(&personal_dir, id).await;
    }
    None
}

/// Helper function to find a memory file by ID prefix in a directory.
///
/// Handles both old (`<uuid>.md`) and new (`<slug>_<uuid>.md`) filename formats.
///
/// Matching strategy:
/// 1. Extract the UUID part from each file stem and check for exact or prefix match.
/// 2. If exactly one match is found, return it.
/// 3. If multiple matches are found, return `None` (ambiguous).
/// 4. If no matches are found, return `None`.
pub async fn find_memory_in_dir(dir: &Path, id: &str) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }

    let Ok(mut entries) = async_fs::read_dir(dir).await else {
        return None;
    };

    let mut prefix_matches: Vec<PathBuf> = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            if super::memory_file::stem_matches_id_prefix(stem, id) {
                let id_part = super::memory_file::extract_id_from_stem(stem);
                if id_part == id {
                    // Exact match — return immediately, no ambiguity.
                    return Some(path);
                }
                prefix_matches.push(path);
            }
        }
    }

    // Only return a prefix match if it's unambiguous (exactly one match).
    if prefix_matches.len() == 1 {
        return prefix_matches.into_iter().next();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_project_dir() {
        let path = Path::new("/tmp/my_project");
        let result = project_dir(path);
        assert_eq!(result, PathBuf::from("/tmp/my_project/.engramdb"));
    }

    #[test]
    fn test_memories_dir() {
        let path = Path::new("/tmp/my_project");
        let result = memories_dir(path);
        assert_eq!(result, PathBuf::from("/tmp/my_project/.engramdb/memories"));
    }

    #[test]
    fn test_personal_dir() {
        let result = personal_dir("abc123").unwrap();
        let path_str = result.to_string_lossy();
        assert!(path_str.ends_with("projects/abc123/personal"));
    }

    #[test]
    fn test_personal_memories_dir() {
        let result = personal_memories_dir("abc123").unwrap();
        assert!(result.to_string_lossy().ends_with("personal/memories"));
    }

    #[test]
    fn test_lancedb_dir() {
        let result = lancedb_dir("abc123").unwrap();
        let path_str = result.to_string_lossy();
        assert!(path_str.ends_with("projects/abc123/lancedb"));
    }

    #[test]
    fn test_global_data_dir() {
        let result = global_data_dir().unwrap();
        assert!(result.exists() || !result.to_string_lossy().is_empty());
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_with_exact_match() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let test_id = "abc123";
        let file_path = temp_dir.path().join(format!("{}.md", test_id));
        tokio::fs::write(&file_path, "test content").await.unwrap();

        let result = find_memory_in_dir(temp_dir.path(), test_id).await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), file_path);
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_with_prefix_match() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let full_id = "abc123-456-789";
        let file_path = temp_dir.path().join(format!("{}.md", full_id));
        tokio::fs::write(&file_path, "test content").await.unwrap();

        let result = find_memory_in_dir(temp_dir.path(), "abc").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap(), file_path);
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_not_found() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let result = find_memory_in_dir(temp_dir.path(), "nonexistent").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_nonexistent_dir() {
        use std::path::Path;

        let nonexistent_path = Path::new("/nonexistent/directory");
        let result = find_memory_in_dir(nonexistent_path, "test").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_ambiguous_prefix_returns_none() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        // Create two files that share the prefix "abc"
        let file1 = temp_dir.path().join("abc-111.md");
        let file2 = temp_dir.path().join("abc-222.md");
        tokio::fs::write(&file1, "content 1").await.unwrap();
        tokio::fs::write(&file2, "content 2").await.unwrap();

        // Searching for "abc" should return None because of ambiguity
        let result = find_memory_in_dir(temp_dir.path(), "abc").await;
        assert!(
            result.is_none(),
            "Ambiguous prefix should return None, got {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_find_memory_in_dir_exact_match_over_prefix() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        // Create files: "abc.md" (exact) and "abc-extra.md" (prefix)
        let exact_file = temp_dir.path().join("abc.md");
        let prefix_file = temp_dir.path().join("abc-extra.md");
        tokio::fs::write(&exact_file, "exact").await.unwrap();
        tokio::fs::write(&prefix_file, "prefix").await.unwrap();

        // Should return the exact match despite prefix matches existing
        let result = find_memory_in_dir(temp_dir.path(), "abc").await;
        assert_eq!(result, Some(exact_file));
    }
}
