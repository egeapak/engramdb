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
}
