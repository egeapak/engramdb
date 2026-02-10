//! Path resolution utilities for EngramDB storage locations

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
