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
    /// If this project is a sub-project (e.g. a git worktree), the project ID
    /// of its parent.  Memory operations on a sub-project are routed to the
    /// root of the hierarchy; the child has no local storage of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_project_id: Option<String>,
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

    /// Load, upsert a project entry (without parent), and save.
    ///
    /// This preserves any existing `parent_project_id` on the entry — use
    /// [`update_with_parent`](Self::update_with_parent) if you need to
    /// set/clear the parent explicitly.
    async fn update(&self, dir: &Path, project_id: &str) -> Result<()> {
        self.update_inner(dir, project_id, None, false).await
    }

    /// Load, upsert a project entry with a parent link, and save.
    ///
    /// Passing `parent_project_id = None` explicitly *clears* any existing
    /// parent on the entry (promoting the project to a root).
    async fn update_with_parent(
        &self,
        dir: &Path,
        project_id: &str,
        parent_project_id: Option<&str>,
    ) -> Result<()> {
        self.update_inner(dir, project_id, parent_project_id, true)
            .await
    }

    /// Internal upsert used by both `update` and `update_with_parent`.
    /// When `overwrite_parent` is false, the existing parent on the entry
    /// (if any) is preserved.
    #[doc(hidden)]
    async fn update_inner(
        &self,
        dir: &Path,
        project_id: &str,
        parent_project_id: Option<&str>,
        overwrite_parent: bool,
    ) -> Result<()> {
        let mut registry = self.load().await?;

        let abs_path = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let path_str = abs_path.to_string_lossy().to_string();

        if let Some(entry) = registry
            .projects
            .iter_mut()
            .find(|e| e.project_id == project_id)
        {
            entry.project_path = path_str;
            if overwrite_parent {
                entry.parent_project_id = parent_project_id.map(|s| s.to_string());
            }
        } else {
            registry.projects.push(RegistryEntry {
                project_id: project_id.to_string(),
                project_path: path_str,
                parent_project_id: parent_project_id.map(|s| s.to_string()),
            });
        }

        self.save(&registry).await
    }

    /// Set (or clear) the `parent_project_id` of an already-registered project.
    ///
    /// Unlike [`update_with_parent`](Self::update_with_parent), this does not
    /// touch the project path and returns an error if `project_id` is not in
    /// the registry. Pass `parent_project_id = None` to promote the project
    /// back to a root.
    async fn set_parent(&self, project_id: &str, parent_project_id: Option<&str>) -> Result<()> {
        let mut registry = self.load().await?;
        let entry = registry
            .projects
            .iter_mut()
            .find(|e| e.project_id == project_id)
            .ok_or_else(|| {
                super::error::StorageError::Validation(format!(
                    "Project '{}' not found in registry",
                    project_id
                ))
            })?;
        entry.parent_project_id = parent_project_id.map(|s| s.to_string());
        self.save(&registry).await
    }
}

/// Collect all direct children of `project_id`.
pub fn list_children<'a>(registry: &'a Registry, project_id: &str) -> Vec<&'a RegistryEntry> {
    registry
        .projects
        .iter()
        .filter(|e| e.parent_project_id.as_deref() == Some(project_id))
        .collect()
}

/// Breadth-first walk of all descendants of `project_id`, returning project
/// ids. Cycle-safe (visited-set bounded by registry size). Does not include
/// `project_id` itself.
pub fn collect_descendants(registry: &Registry, project_id: &str) -> Vec<String> {
    use std::collections::{HashSet, VecDeque};
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    // Seed with the starting node so a cycle back to it doesn't cause it to
    // be reported as its own descendant.
    seen.insert(project_id.to_string());
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(project_id.to_string());

    while let Some(current) = queue.pop_front() {
        for child in list_children(registry, &current) {
            if seen.insert(child.project_id.clone()) {
                out.push(child.project_id.clone());
                queue.push_back(child.project_id.clone());
            }
        }
    }
    out
}

/// Walk `parent_project_id` links in the registry to find the root project.
///
/// Returns `project_id` unchanged if it has no parent or is not found in the
/// registry.  Detects and breaks cycles by bounding the chain length at the
/// number of projects in the registry.
pub fn resolve_root_project_id(registry: &Registry, project_id: &str) -> String {
    let mut current = project_id.to_string();
    // Bound the loop by registry size to guard against cycles.
    for _ in 0..=registry.projects.len() {
        match registry.projects.iter().find(|e| e.project_id == current) {
            Some(entry) => match &entry.parent_project_id {
                Some(parent) if parent != &current => current = parent.clone(),
                _ => return current,
            },
            None => return current,
        }
    }
    current
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
            Ok(serde_json::from_str(&content)?)
        } else {
            Ok(Registry::default())
        }
    }

    async fn save(&self, registry: &Registry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            async_fs::create_dir_all(parent).await?;
        }
        let content = serde_json::to_string_pretty(registry)?;
        super::store::atomic_write(&self.path, &content).await?;
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
            parent_project_id: None,
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
            parent_project_id: None,
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
            parent_project_id: None,
        });
        data.projects.push(RegistryEntry {
            project_id: "pre-2".to_string(),
            project_path: "/tmp/pre2".to_string(),
            parent_project_id: None,
        });

        let registry = InMemoryRegistry::with(data);
        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 2);
        assert_eq!(loaded.projects[0].project_id, "pre-1");
        assert_eq!(loaded.projects[1].project_id, "pre-2");
    }

    #[tokio::test]
    async fn test_update_with_parent_sets_parent_id() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry
            .update_with_parent(temp_dir.path(), "child-id", Some("parent-id"))
            .await
            .unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects.len(), 1);
        let entry = &loaded.projects[0];
        assert_eq!(entry.project_id, "child-id");
        assert_eq!(entry.parent_project_id.as_deref(), Some("parent-id"));
    }

    #[tokio::test]
    async fn test_update_preserves_existing_parent() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        // Create a child entry with a parent.
        registry
            .update_with_parent(temp_dir.path(), "child-id", Some("parent-id"))
            .await
            .unwrap();

        // Calling plain `update` must not wipe the parent.
        registry.update(temp_dir.path(), "child-id").await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(
            loaded.projects[0].parent_project_id.as_deref(),
            Some("parent-id")
        );
    }

    #[tokio::test]
    async fn test_update_with_parent_none_clears_parent() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        registry
            .update_with_parent(temp_dir.path(), "child-id", Some("parent-id"))
            .await
            .unwrap();
        registry
            .update_with_parent(temp_dir.path(), "child-id", None)
            .await
            .unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects[0].parent_project_id, None);
    }

    #[test]
    fn test_resolve_root_project_id_no_parent() {
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "solo".into(),
            project_path: "/tmp/solo".into(),
            parent_project_id: None,
        });
        assert_eq!(resolve_root_project_id(&reg, "solo"), "solo");
    }

    #[test]
    fn test_resolve_root_project_id_single_level() {
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "root".into(),
            project_path: "/tmp/root".into(),
            parent_project_id: None,
        });
        reg.projects.push(RegistryEntry {
            project_id: "child".into(),
            project_path: "/tmp/child".into(),
            parent_project_id: Some("root".into()),
        });
        assert_eq!(resolve_root_project_id(&reg, "child"), "root");
    }

    #[test]
    fn test_resolve_root_project_id_follows_chain() {
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "a".into(),
            project_path: "/a".into(),
            parent_project_id: None,
        });
        reg.projects.push(RegistryEntry {
            project_id: "b".into(),
            project_path: "/b".into(),
            parent_project_id: Some("a".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "c".into(),
            project_path: "/c".into(),
            parent_project_id: Some("b".into()),
        });
        assert_eq!(resolve_root_project_id(&reg, "c"), "a");
    }

    #[test]
    fn test_resolve_root_project_id_breaks_cycles() {
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "a".into(),
            project_path: "/a".into(),
            parent_project_id: Some("b".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "b".into(),
            project_path: "/b".into(),
            parent_project_id: Some("a".into()),
        });
        // Must terminate even with a cycle; return value can be either node.
        let root = resolve_root_project_id(&reg, "a");
        assert!(root == "a" || root == "b");
    }

    #[test]
    fn test_resolve_root_project_id_unknown_returns_input() {
        let reg = Registry::default();
        assert_eq!(resolve_root_project_id(&reg, "unknown"), "unknown");
    }

    #[test]
    fn test_registry_entry_missing_parent_field_deserializes_as_none() {
        // Ensure backward-compat with older registry.json files that don't
        // include parent_project_id.
        let json = r#"{
            "projects": [
                {"project_id": "x", "project_path": "/x"}
            ]
        }"#;
        let reg: Registry = serde_json::from_str(json).unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert_eq!(reg.projects[0].parent_project_id, None);
    }

    #[tokio::test]
    async fn test_file_registry_load_corrupted_json_returns_error() {
        let temp_dir = TempDir::new().unwrap();
        let registry_path = temp_dir.path().join("registry.json");

        // Write corrupted JSON
        async_fs::write(&registry_path, "{ not valid json !!!")
            .await
            .unwrap();

        let file_registry = FileRegistry::new(registry_path);
        let result = file_registry.load().await;
        assert!(
            result.is_err(),
            "Corrupted registry JSON should return an error, not silently discard data"
        );
    }

    // ---- set_parent ----

    #[tokio::test]
    async fn test_set_parent_on_existing_entry() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        registry.update(temp_dir.path(), "child").await.unwrap();

        registry.set_parent("child", Some("parent")).await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(
            loaded.projects[0].parent_project_id.as_deref(),
            Some("parent")
        );
    }

    #[tokio::test]
    async fn test_set_parent_clears_with_none() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        registry
            .update_with_parent(temp_dir.path(), "child", Some("parent"))
            .await
            .unwrap();

        registry.set_parent("child", None).await.unwrap();

        let loaded = registry.load().await.unwrap();
        assert_eq!(loaded.projects[0].parent_project_id, None);
    }

    #[tokio::test]
    async fn test_set_parent_errors_when_child_missing() {
        let registry = InMemoryRegistry::new();
        let err = registry
            .set_parent("nonexistent", Some("x"))
            .await
            .expect_err("set_parent on missing child should error");
        assert!(format!("{err}").contains("nonexistent"));
    }

    // ---- list_children / collect_descendants ----

    fn tree_registry() -> Registry {
        // root
        // ├── a
        // │   └── a1
        // └── b
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "root".into(),
            project_path: "/root".into(),
            parent_project_id: None,
        });
        reg.projects.push(RegistryEntry {
            project_id: "a".into(),
            project_path: "/a".into(),
            parent_project_id: Some("root".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "a1".into(),
            project_path: "/a1".into(),
            parent_project_id: Some("a".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "b".into(),
            project_path: "/b".into(),
            parent_project_id: Some("root".into()),
        });
        reg
    }

    #[test]
    fn test_list_children_direct_only() {
        let reg = tree_registry();
        let ids: Vec<_> = list_children(&reg, "root")
            .iter()
            .map(|e| e.project_id.clone())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn test_list_children_returns_empty_for_leaf() {
        let reg = tree_registry();
        assert!(list_children(&reg, "b").is_empty());
    }

    #[test]
    fn test_collect_descendants_walks_whole_subtree() {
        let reg = tree_registry();
        let mut desc = collect_descendants(&reg, "root");
        desc.sort();
        assert_eq!(desc, vec!["a", "a1", "b"]);
    }

    #[test]
    fn test_collect_descendants_returns_empty_for_leaf() {
        let reg = tree_registry();
        assert!(collect_descendants(&reg, "a1").is_empty());
    }

    #[test]
    fn test_collect_descendants_cycle_safe() {
        // a → b → a cycle. collect_descendants("a") must terminate.
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "a".into(),
            project_path: "/a".into(),
            parent_project_id: Some("b".into()),
        });
        reg.projects.push(RegistryEntry {
            project_id: "b".into(),
            project_path: "/b".into(),
            parent_project_id: Some("a".into()),
        });
        let mut desc = collect_descendants(&reg, "a");
        desc.sort();
        // Descendants of `a` includes `b` (whose parent is `a`). `a` itself
        // is not reported. The walk must not loop forever.
        assert_eq!(desc, vec!["b"]);
    }
}
