//! Manifest file read/write operations.
//!
//! This module manages the manifest.toml file, which stores project metadata
//! and statistics. The manifest includes:
//! - Schema version (for future format changes)
//! - Project name and description
//! - Creation timestamp
//! - Statistics (memory count, logical scopes)
//!
//! The manifest is automatically updated when memories are created, updated,
//! or deleted, ensuring statistics remain accurate.

use super::error::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Project manifest stored in manifest.toml.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// Schema version for future compatibility
    pub schema_version: String,
    /// Project name
    pub project: String,
    /// When this manifest was created
    pub created_at: DateTime<Utc>,
    /// Human-readable project description
    pub description: String,
    /// Project statistics (updated automatically)
    pub stats: ManifestStats,
}

/// Statistics tracked in the manifest.
#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestStats {
    /// Total number of memories (shared + personal)
    pub memory_count: usize,
    /// All unique logical scopes used in memories
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
pub async fn load_manifest(path: &Path) -> Result<Manifest> {
    let content = tokio::fs::read_to_string(path).await?;
    let manifest: Manifest = toml::from_str(&content)?;
    Ok(manifest)
}

/// Save manifest to manifest.toml atomically via write-to-temp-then-rename.
pub async fn save_manifest(path: &Path, manifest: &Manifest) -> Result<()> {
    let content = toml::to_string_pretty(manifest)
        .map_err(|e| super::error::StorageError::Validation(e.to_string()))?;
    let tmp_path = path.with_extension(format!(
        "{}.{}.toml.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&tmp_path, &content).await?;
    tokio::fs::rename(&tmp_path, path).await?;
    Ok(())
}

/// Update manifest stats (memory count and logical scopes)
pub fn update_stats(manifest: &mut Manifest, memory_count: usize, logical_scopes: Vec<String>) {
    manifest.stats.memory_count = memory_count;
    manifest.stats.logical_scopes = logical_scopes;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_manifest_default() {
        let manifest = Manifest::default();
        assert_eq!(manifest.schema_version, "0.1.0");
        assert_eq!(manifest.stats.memory_count, 0);
        assert!(manifest.stats.logical_scopes.is_empty());
    }

    #[tokio::test]
    async fn test_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.toml");

        let mut original = Manifest {
            project: "test_project".to_string(),
            description: "Test description".to_string(),
            ..Default::default()
        };
        original.stats.memory_count = 42;
        original.stats.logical_scopes = vec!["scope1".to_string(), "scope2".to_string()];

        save_manifest(&manifest_path, &original).await.unwrap();
        let loaded = load_manifest(&manifest_path).await.unwrap();

        assert_eq!(loaded.schema_version, original.schema_version);
        assert_eq!(loaded.project, original.project);
        assert_eq!(loaded.description, original.description);
        assert_eq!(loaded.stats.memory_count, original.stats.memory_count);
        assert_eq!(loaded.stats.logical_scopes, original.stats.logical_scopes);
    }

    #[tokio::test]
    async fn test_load_manifest_file_not_found() {
        let result = load_manifest(Path::new("/nonexistent/path/manifest.toml")).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            super::super::error::StorageError::Io(_)
        ));
    }

    #[tokio::test]
    async fn test_load_manifest_invalid_toml() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("manifest.toml");

        tokio::fs::write(&manifest_path, "invalid { toml content")
            .await
            .unwrap();

        let result = load_manifest(&manifest_path).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            super::super::error::StorageError::Toml(_)
        ));
    }

    #[test]
    fn test_update_stats() {
        let mut manifest = Manifest::default();
        let scopes = vec!["scope_a".to_string(), "scope_b".to_string()];

        update_stats(&mut manifest, 100, scopes.clone());

        assert_eq!(manifest.stats.memory_count, 100);
        assert_eq!(manifest.stats.logical_scopes, scopes);
    }
}
