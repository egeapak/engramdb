//! Registry backend trait and implementations.
//!
//! The registry tracks all EngramDB projects on this machine.  Production code
//! uses [`FileRegistry`] (reads/writes JSON to disk) while tests use
//! [`InMemoryRegistry`] (zero filesystem access, full isolation).

use super::error::Result;
use super::paths;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs as async_fs;
use tokio::sync::Mutex;

/// Entry in the global registry tracking a single project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Unique project identifier (hash of git remote or path)
    pub project_id: String,
    /// Absolute path to the project directory
    pub project_path: String,
}

/// Global registry of all EngramDB projects on this machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    /// List of all registered projects
    pub projects: Vec<RegistryEntry>,
}

/// Trait for registry persistence backends.
#[async_trait]
pub trait RegistryBackend: Send + Sync {
    /// Load the full registry.
    async fn load(&self) -> Result<Registry>;

    /// Save the full registry.
    async fn save(&self, registry: &Registry) -> Result<()>;

    /// Load, upsert a project entry, and save.
    async fn update(&self, dir: &Path, project_id: &str) -> Result<()> {
        let mut registry = self.load().await?;

        let abs_path = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let path_str = abs_path.to_string_lossy().to_string();

        if let Some(entry) = registry
            .projects
            .iter_mut()
            .find(|e| e.project_id == project_id)
        {
            entry.project_path = path_str;
        } else {
            registry.projects.push(RegistryEntry {
                project_id: project_id.to_string(),
                project_path: path_str,
            });
        }

        self.save(&registry).await
    }
}

// ---------------------------------------------------------------------------
// FileRegistry — reads/writes JSON to a file on disk
// ---------------------------------------------------------------------------

/// File-backed registry that persists to a JSON file.
pub struct FileRegistry {
    path: PathBuf,
}

impl FileRegistry {
    /// Create a `FileRegistry` pointing at the platform-default global path.
    pub fn global() -> Result<Self> {
        Ok(Self {
            path: paths::registry_path()?,
        })
    }

    /// Create a `FileRegistry` at an arbitrary path (useful for testing).
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait]
impl RegistryBackend for FileRegistry {
    async fn load(&self) -> Result<Registry> {
        if self.path.exists() {
            let content = async_fs::read_to_string(&self.path).await?;
            Ok(serde_json::from_str(&content).unwrap_or_default())
        } else {
            Ok(Registry::default())
        }
    }

    async fn save(&self, registry: &Registry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            async_fs::create_dir_all(parent).await?;
        }
        let content = serde_json::to_string_pretty(registry)?;
        let tmp = tempfile::NamedTempFile::new_in(self.path.parent().unwrap_or(&self.path))?;
        async_fs::write(tmp.path(), &content).await?;
        tmp.persist(&self.path)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// InMemoryRegistry — zero filesystem, fully isolated
// ---------------------------------------------------------------------------

/// In-memory registry for tests.  Zero filesystem access, fully isolated.
pub struct InMemoryRegistry {
    data: Arc<Mutex<Registry>>,
}

impl InMemoryRegistry {
    /// Create an empty in-memory registry.
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(Registry::default())),
        }
    }

    /// Create an in-memory registry pre-populated with the given data.
    pub fn with(registry: Registry) -> Self {
        Self {
            data: Arc::new(Mutex::new(registry)),
        }
    }
}

impl Default for InMemoryRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RegistryBackend for InMemoryRegistry {
    async fn load(&self) -> Result<Registry> {
        Ok(self.data.lock().await.clone())
    }

    async fn save(&self, registry: &Registry) -> Result<()> {
        *self.data.lock().await = registry.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_file_registry_load_empty() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");
        let file_registry = FileRegistry::new(registry_path);

        let registry = file_registry.load().await.unwrap();
        assert!(registry.projects.is_empty());
    }

    #[tokio::test]
    async fn test_file_registry_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");
        let file_registry = FileRegistry::new(registry_path);

        let mut registry = Registry::default();
        registry.projects.push(RegistryEntry {
            project_id: "test-id".to_string(),
            project_path: "/tmp/test".to_string(),
        });

        file_registry.save(&registry).await.unwrap();

        let loaded = file_registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_id, "test-id");
    }

    #[tokio::test]
    async fn test_file_registry_update_creates_entry() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");
        let file_registry = FileRegistry::new(registry_path);

        file_registry
            .update(temp_dir.path(), "proj-1")
            .await
            .unwrap();

        let loaded = file_registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_id, "proj-1");
    }

    #[tokio::test]
    async fn test_file_registry_update_deduplicates() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");
        let file_registry = FileRegistry::new(registry_path);

        file_registry
            .update(temp_dir.path(), "proj-1")
            .await
            .unwrap();
        file_registry
            .update(temp_dir.path(), "proj-1")
            .await
            .unwrap();

        let loaded = file_registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
    }

    #[tokio::test]
    async fn test_in_memory_registry_load_empty() {
        let registry = InMemoryRegistry::new();
        let loaded = registry.load().await.unwrap();
        assert!(loaded.projects.is_empty());
    }

    #[tokio::test]
    async fn test_in_memory_registry_save_and_load() {
        let registry = InMemoryRegistry::new();

        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "mem-id".to_string(),
            project_path: "/tmp/mem".to_string(),
        });

        registry.save(&data).await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_id, "mem-id");
    }

    #[tokio::test]
    async fn test_in_memory_registry_update_creates_entry() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry.update(temp_dir.path(), "proj-2").await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        assert_eq!(loaded.projects[0].project_id, "proj-2");
    }

    #[tokio::test]
    async fn test_in_memory_registry_update_deduplicates() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry.update(temp_dir.path(), "proj-2").await.unwrap();
        registry.update(temp_dir.path(), "proj-2").await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
    }

    #[tokio::test]
    async fn test_in_memory_with_preloaded() {
        let mut data = Registry::default();
        data.projects.push(RegistryEntry {
            project_id: "pre-1".to_string(),
            project_path: "/tmp/pre".to_string(),
        });
        data.projects.push(RegistryEntry {
            project_id: "pre-2".to_string(),
            project_path: "/tmp/pre2".to_string(),
        });

        let registry = InMemoryRegistry::with(data);
        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 2);
        assert_eq!(loaded.projects[0].project_id, "pre-1");
        assert_eq!(loaded.projects[1].project_id, "pre-2");
    }
}
