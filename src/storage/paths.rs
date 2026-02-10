//! Path resolution utilities for EngramDB storage locations.
//!
//! This module provides functions to resolve all EngramDB storage paths:
//! - Project-local paths (.engramdb/, .engramdb/memories/)
//! - Global paths (~/.config/engramdb/)
//! - Personal project paths (~/.config/engramdb/projects/{id}/personal/)
//! - LanceDB vector storage paths
//! - Registry path
//!
//! All functions return PathBuf and handle both shared (project-level) and
//! personal (user-level) storage locations.

use std::path::{Path, PathBuf};

/// Returns the project-specific EngramDB directory (.engramdb/)
pub fn project_dir(dir: &Path) -> PathBuf {
    dir.join(".engramdb")
}

/// Returns the shared memories directory in the project
pub fn memories_dir(dir: &Path) -> PathBuf {
    project_dir(dir).join("memories")
}

/// Returns the global configuration directory (~/.config/engramdb/)
pub fn global_config_dir() -> PathBuf {
    dirs::config_dir()
        .expect("Could not determine config directory")
        .join("engramdb")
}

/// Returns the personal project directory for a given project ID
pub fn personal_dir(project_id: &str) -> PathBuf {
    global_config_dir()
        .join("projects")
        .join(project_id)
        .join("personal")
}

/// Returns the personal memories directory for a given project ID
pub fn personal_memories_dir(project_id: &str) -> PathBuf {
    personal_dir(project_id).join("memories")
}

/// Returns the LanceDB directory for a given project ID
pub fn lancedb_dir(project_id: &str) -> PathBuf {
    global_config_dir()
        .join("projects")
        .join(project_id)
        .join("lancedb")
}

/// Returns the global registry path (~/.config/engramdb/registry.json)
pub fn registry_path() -> PathBuf {
    global_config_dir().join("registry.json")
}

/// Returns the memory file path for a given memory ID
/// Note: This function tries to find the memory in both shared and personal directories
/// by checking for files that start with the given ID prefix.
pub fn memory_path(dir: &Path, id: &str) -> Option<PathBuf> {
    // Try shared memories first
    let shared_dir = memories_dir(dir);
    if let Some(path) = find_memory_in_dir(&shared_dir, id) {
        return Some(path);
    }

    // Try personal memories
    // We need to compute project_id to find personal dir
    // For simplicity, we'll use the compute_project_id from project_id module
    use crate::storage::project_id::compute_project_id;
    let project_id = compute_project_id(dir);
    let personal_dir = personal_memories_dir(&project_id);
    find_memory_in_dir(&personal_dir, id)
}

/// Helper function to find a memory file by ID prefix in a directory
fn find_memory_in_dir(dir: &Path, id: &str) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(filename) = path.file_stem().and_then(|s| s.to_str()) {
            if filename.starts_with(id) {
                return Some(path);
            }
        }
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
        let result = personal_dir("abc123");
        assert!(result
            .to_string_lossy()
            .ends_with("projects/abc123/personal"));
    }

    #[test]
    fn test_personal_memories_dir() {
        let result = personal_memories_dir("abc123");
        assert!(result.to_string_lossy().ends_with("personal/memories"));
    }

    #[test]
    fn test_lancedb_dir() {
        let result = lancedb_dir("abc123");
        assert!(result
            .to_string_lossy()
            .ends_with("projects/abc123/lancedb"));
    }

    #[test]
    fn test_find_memory_in_dir_with_exact_match() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let test_id = "abc123";
        let file_path = temp_dir.path().join(format!("{}.md", test_id));
        std::fs::write(&file_path, "test content").unwrap();

        let result = find_memory_in_dir(temp_dir.path(), test_id);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), file_path);
    }

    #[test]
    fn test_find_memory_in_dir_with_prefix_match() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let full_id = "abc123-456-789";
        let file_path = temp_dir.path().join(format!("{}.md", full_id));
        std::fs::write(&file_path, "test content").unwrap();

        let result = find_memory_in_dir(temp_dir.path(), "abc");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), file_path);
    }

    #[test]
    fn test_find_memory_in_dir_not_found() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let result = find_memory_in_dir(temp_dir.path(), "nonexistent");
        assert!(result.is_none());
    }

    #[test]
    fn test_find_memory_in_dir_nonexistent_dir() {
        use std::path::Path;

        let nonexistent_path = Path::new("/nonexistent/directory");
        let result = find_memory_in_dir(nonexistent_path, "test");
        assert!(result.is_none());
    }
}
