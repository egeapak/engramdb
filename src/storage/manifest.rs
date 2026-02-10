//! Manifest file read/write operations

use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use super::error::Result;
use std::path::Path;
use std::fs;

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: String,
    pub project: String,
    pub created_at: DateTime<Utc>,
    pub description: String,
    pub stats: ManifestStats,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestStats {
    pub memory_count: usize,
    pub logical_scopes: Vec<String>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            schema_version: "0.1.0".to_string(),
            project: "engramdb-project".to_string(),
            created_at: Utc::now(),
            description: "Agent memory store. See config.toml for retrieval settings.".to_string(),
            stats: ManifestStats {
                memory_count: 0,
                logical_scopes: Vec::new(),
            },
        }
    }
}

/// Load manifest from manifest.toml
pub fn load_manifest(path: &Path) -> Result<Manifest> {
    let content = fs::read_to_string(path)?;
    let manifest: Manifest = toml::from_str(&content)?;
    Ok(manifest)
}

/// Save manifest to manifest.toml
pub fn save_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let content = toml::to_string_pretty(manifest)
        .map_err(|e| super::error::StorageError::Validation(e.to_string()))?;
    fs::write(path, content)?;
    Ok(())
}

/// Update manifest stats (memory count and logical scopes)
pub fn update_stats(manifest: &mut Manifest, memory_count: usize, logical_scopes: Vec<String>) {
    manifest.stats.memory_count = memory_count;
    manifest.stats.logical_scopes = logical_scopes;
}
