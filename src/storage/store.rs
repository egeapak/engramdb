//! Main storage orchestrator - MemoryStore.
//!
//! This module provides the [`MemoryStore`] struct, which orchestrates all
//! file system operations for memories:
//! - Initialize new EngramDB stores with `init()`
//! - Open existing stores with `open()`
//! - Create, read, update, delete memories (CRUD operations)
//! - List all memories via index
//! - Rebuild index from files with `reindex()`
//!
//! MemoryStore handles both shared (project-level) and personal (user-level)
//! memories, maintaining separate indexes for each. It also manages the global
//! registry of projects and updates manifest statistics automatically.
//!
//! ID matching supports prefix matching for convenience (e.g., "abcd" matches
//! "abcd1234-5678-..."), with ambiguity detection.

use super::error::{Result, StorageError};
use super::{index, manifest, memory_file, paths, project_id};
use crate::types::{Memory, MemoryUpdate, Visibility};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Entry in the global registry tracking a single project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Unique project identifier (hash of git remote or path)
    pub project_id: String,
    /// Absolute path to the project directory
    pub project_path: String,
    /// Last time this project was opened
    pub last_opened: chrono::DateTime<chrono::Utc>,
}

/// Global registry of all EngramDB projects on this machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    /// List of all registered projects
    pub projects: Vec<RegistryEntry>,
}

/// Main storage interface for EngramDB operations.
///
/// Manages memory files, indexes, manifest, and coordinates between
/// shared (project-level) and personal (user-level) storage locations.
pub struct MemoryStore {
    /// Root directory of the project (contains .engramdb/)
    pub project_dir: PathBuf,
    /// Unique identifier for this project
    pub project_id: String,
}

impl MemoryStore {
    /// Initialize a new EngramDB store in the given directory
    pub fn init(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        // Create directory structure
        fs::create_dir_all(&engramdb_dir)?;
        fs::create_dir_all(paths::memories_dir(dir))?;

        // Create manifest.toml with project name derived from directory
        let manifest_path = engramdb_dir.join("manifest.toml");
        let project_name = dir
            .canonicalize()
            .unwrap_or_else(|_| dir.to_path_buf())
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed-project".to_string());
        let manifest = manifest::Manifest {
            project: project_name,
            ..Default::default()
        };
        manifest::save_manifest(&manifest_path, &manifest)?;

        // Create empty config.toml
        let config_path = engramdb_dir.join("config.toml");
        fs::write(
            config_path,
            "# EngramDB configuration\n# See documentation for available settings\n",
        )?;

        // Create empty index.json
        let index_path = engramdb_dir.join("index.json");
        let empty_index = index::Index::default();
        index::save_index(&index_path, &empty_index)?;

        // Compute project ID
        let project_id = project_id::compute_project_id(dir);

        // Create personal directories
        fs::create_dir_all(paths::personal_memories_dir(&project_id)?)?;
        fs::create_dir_all(paths::lancedb_dir(&project_id)?)?;

        // Update registry
        Self::update_registry(dir, &project_id)?;

        Ok(Self {
            project_dir: dir.to_path_buf(),
            project_id,
        })
    }

    /// Open an existing EngramDB store
    pub fn open(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        if !engramdb_dir.exists() {
            return Err(StorageError::NotInitialized);
        }

        // Compute project ID
        let project_id = project_id::compute_project_id(dir);

        // Update registry
        Self::update_registry(dir, &project_id)?;

        Ok(Self {
            project_dir: dir.to_path_buf(),
            project_id,
        })
    }

    /// Create a new memory
    pub fn create(&self, memory: &Memory) -> Result<String> {
        let memories_dir = self.get_memories_dir(&memory.visibility)?;
        fs::create_dir_all(&memories_dir)?;

        // Write memory file
        let file_path = memories_dir.join(format!("{}.md", memory.id));
        let content = memory_file::write_memory_file(memory)?;
        fs::write(&file_path, content)?;

        // Update index
        let index_path = self.get_index_path(&memory.visibility)?;
        let mut idx = index::load_index(&index_path)?;
        let entry = index::IndexEntry::from(memory);
        index::add_entry(&mut idx, entry);
        index::save_index(&index_path, &idx)?;

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(memory.id.clone())
    }

    /// Get a memory by ID (supports prefix matching)
    pub fn get(&self, id: &str) -> Result<Memory> {
        // Try shared memories first
        match self.get_from_dir(id, &paths::memories_dir(&self.project_dir)) {
            Ok(memory) => return Ok(memory),
            Err(StorageError::Validation(msg)) => return Err(StorageError::Validation(msg)),
            Err(StorageError::NotFound(_)) => {
                // Fall through to personal
            }
            Err(e) => return Err(e),
        }

        // Try personal memories
        self.get_from_dir(id, &paths::personal_memories_dir(&self.project_id)?)
    }

    fn get_from_dir(&self, id: &str, dir: &Path) -> Result<Memory> {
        if !dir.exists() {
            return Err(StorageError::NotFound(id.to_string()));
        }

        let mut matches = Vec::new();

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(filename) = path.file_stem().and_then(|s| s.to_str()) {
                if filename.starts_with(id) {
                    matches.push(path);
                }
            }
        }

        match matches.len() {
            0 => Err(StorageError::NotFound(id.to_string())),
            1 => {
                let content = fs::read_to_string(&matches[0])?;
                memory_file::parse_memory_file(&content)
            }
            _ => Err(StorageError::Validation(format!(
                "Ambiguous ID prefix '{}': matches {} memories",
                id,
                matches.len()
            ))),
        }
    }

    /// Update a memory
    pub fn update(&self, id: &str, updates: MemoryUpdate) -> Result<()> {
        // Get existing memory
        let mut memory = self.get(id)?;
        let old_visibility = memory.visibility;

        // Apply updates
        updates.apply_to(&mut memory);

        // Check if visibility changed
        if memory.visibility != old_visibility {
            // Delete from old location
            self.delete_from_dir(id, &self.get_memories_dir(&old_visibility)?)?;

            // Write to new location
            self.create(&memory)?;
        } else {
            // Write updated memory
            let memories_dir = self.get_memories_dir(&memory.visibility)?;
            let file_path = memories_dir.join(format!("{}.md", memory.id));
            let content = memory_file::write_memory_file(&memory)?;
            fs::write(&file_path, content)?;

            // Update index
            let index_path = self.get_index_path(&memory.visibility)?;
            let mut idx = index::load_index(&index_path)?;
            let entry = index::IndexEntry::from(&memory);
            index::update_entry(&mut idx, entry);
            index::save_index(&index_path, &idx)?;
        }

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(())
    }

    /// Delete a memory
    pub fn delete(&self, id: &str) -> Result<()> {
        // Try to delete from shared
        if self
            .delete_from_dir(id, &paths::memories_dir(&self.project_dir))
            .is_ok()
        {
            self.update_manifest_stats()?;
            return Ok(());
        }

        // Try to delete from personal
        self.delete_from_dir(id, &paths::personal_memories_dir(&self.project_id)?)?;
        self.update_manifest_stats()?;
        Ok(())
    }

    fn delete_from_dir(&self, id: &str, dir: &Path) -> Result<()> {
        if !dir.exists() {
            return Err(StorageError::NotFound(id.to_string()));
        }

        let mut matches = Vec::new();

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(filename) = path.file_stem().and_then(|s| s.to_str()) {
                if filename.starts_with(id) {
                    let fname = filename.to_string();
                    matches.push((path, fname));
                }
            }
        }

        match matches.len() {
            0 => Err(StorageError::NotFound(id.to_string())),
            1 => {
                let (path, memory_id) = &matches[0];
                fs::remove_file(path)?;

                // Update index based on directory
                let visibility = if dir == paths::memories_dir(&self.project_dir) {
                    Visibility::Shared
                } else {
                    Visibility::Personal
                };

                let index_path = self.get_index_path(&visibility)?;
                let mut idx = index::load_index(&index_path)?;
                index::remove_entry(&mut idx, memory_id);
                index::save_index(&index_path, &idx)?;

                Ok(())
            }
            _ => Err(StorageError::Validation(format!(
                "Ambiguous ID prefix '{}': matches {} memories",
                id,
                matches.len()
            ))),
        }
    }

    /// List all memories (returns index entries)
    pub fn list(&self) -> Result<Vec<index::IndexEntry>> {
        let mut all_entries = Vec::new();

        // Load shared index
        let shared_index_path = paths::project_dir(&self.project_dir).join("index.json");
        if let Ok(idx) = index::load_index(&shared_index_path) {
            all_entries.extend(idx.memories);
        }

        // Load personal index
        let personal_index_path = paths::personal_dir(&self.project_id)?.join("index.json");
        if let Ok(idx) = index::load_index(&personal_index_path) {
            all_entries.extend(idx.memories);
        }

        Ok(all_entries)
    }

    /// Rebuild index from memory files
    pub fn reindex(&self) -> Result<usize> {
        let mut count = 0;

        // Reindex shared memories
        let shared_dir = paths::memories_dir(&self.project_dir);
        if shared_dir.exists() {
            let idx = index::rebuild_index_from_files(&shared_dir)?;
            count += idx.memories.len();
            let index_path = paths::project_dir(&self.project_dir).join("index.json");
            index::save_index(&index_path, &idx)?;
        }

        // Reindex personal memories
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        if personal_dir.exists() {
            let idx = index::rebuild_index_from_files(&personal_dir)?;
            count += idx.memories.len();
            let index_path = paths::personal_dir(&self.project_id)?.join("index.json");
            index::save_index(&index_path, &idx)?;
        }

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(count)
    }

    // Helper methods

    fn get_memories_dir(&self, visibility: &Visibility) -> Result<PathBuf> {
        match visibility {
            Visibility::Shared => Ok(paths::memories_dir(&self.project_dir)),
            Visibility::Personal => paths::personal_memories_dir(&self.project_id),
        }
    }

    fn get_index_path(&self, visibility: &Visibility) -> Result<PathBuf> {
        match visibility {
            Visibility::Shared => Ok(paths::project_dir(&self.project_dir).join("index.json")),
            Visibility::Personal => Ok(paths::personal_dir(&self.project_id)?.join("index.json")),
        }
    }

    fn update_manifest_stats(&self) -> Result<()> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let mut manifest = manifest::load_manifest(&manifest_path)?;

        // Count memories and collect logical scopes
        let entries = self.list()?;
        let memory_count = entries.len();
        let logical_scopes: Vec<String> = entries
            .iter()
            .flat_map(|e| e.logical.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        manifest::update_stats(&mut manifest, memory_count, logical_scopes);
        manifest::save_manifest(&manifest_path, &manifest)?;

        Ok(())
    }

    fn update_registry(dir: &Path, project_id: &str) -> Result<()> {
        let registry_path = paths::registry_path()?;

        // Create registry directory if needed
        if let Some(parent) = registry_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Load or create registry
        let mut registry = if registry_path.exists() {
            let content = fs::read_to_string(&registry_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Registry::default()
        };

        // Update or add entry
        let abs_path = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let path_str = abs_path.to_string_lossy().to_string();

        if let Some(entry) = registry
            .projects
            .iter_mut()
            .find(|e| e.project_id == project_id)
        {
            entry.last_opened = chrono::Utc::now();
            entry.project_path = path_str;
        } else {
            registry.projects.push(RegistryEntry {
                project_id: project_id.to_string(),
                project_path: path_str,
                last_opened: chrono::Utc::now(),
            });
        }

        // Save registry
        let content = serde_json::to_string_pretty(&registry)?;
        fs::write(&registry_path, content)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    fn create_test_memory(id: &str, visibility: Visibility) -> Memory {
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Test summary",
            "Test content",
            Provenance::human(),
        );
        memory.id = id.to_string();
        memory.visibility = visibility;
        memory
    }

    #[test]
    fn test_init_creates_structure() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();

        // Check main directories
        assert!(project_dir.join(".engramdb").exists());
        assert!(project_dir.join(".engramdb/memories").exists());

        // Check files
        assert!(project_dir.join(".engramdb/manifest.toml").exists());
        assert!(project_dir.join(".engramdb/config.toml").exists());
        assert!(project_dir.join(".engramdb/index.json").exists());

        // Check personal directories
        assert!(paths::personal_memories_dir(&store.project_id)
            .unwrap()
            .exists());
        assert!(paths::lancedb_dir(&store.project_id).unwrap().exists());
    }

    #[test]
    fn test_open_uninitialized() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let result = MemoryStore::open(project_dir);
        assert!(result.is_err());
        match result {
            Err(StorageError::NotInitialized) => {}
            _ => panic!("Expected NotInitialized error"),
        }
    }

    #[test]
    fn test_create_and_get() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();
        let memory = create_test_memory("test-create-123", Visibility::Shared);

        let created_id = store.create(&memory).unwrap();
        assert_eq!(created_id, "test-create-123");

        let retrieved = store.get("test-create-123").unwrap();
        assert_eq!(retrieved.id, "test-create-123");
        assert_eq!(retrieved.summary, "Test summary");
        assert_eq!(retrieved.content, "Test content");
    }

    #[test]
    fn test_get_prefix_matching() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();
        let memory = create_test_memory("abcd1234-5678-90ab-cdef-1234567890ab", Visibility::Shared);

        store.create(&memory).unwrap();

        // Should match with prefix
        let retrieved = store.get("abcd1234").unwrap();
        assert_eq!(retrieved.id, "abcd1234-5678-90ab-cdef-1234567890ab");

        // Even shorter prefix
        let retrieved = store.get("abcd").unwrap();
        assert_eq!(retrieved.id, "abcd1234-5678-90ab-cdef-1234567890ab");
    }

    #[test]
    fn test_get_ambiguous_prefix() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();

        // Create two memories with same prefix
        // We'll manually set IDs to ensure they start the same
        let memory1 =
            create_test_memory("aaaa1111-0000-0000-0000-000000000001", Visibility::Shared);
        let memory2 =
            create_test_memory("aaaa2222-0000-0000-0000-000000000002", Visibility::Shared);

        store.create(&memory1).unwrap();
        store.create(&memory2).unwrap();

        // Verify both exist with full IDs
        assert!(store.get("aaaa1111-0000-0000-0000-000000000001").is_ok());
        assert!(store.get("aaaa2222-0000-0000-0000-000000000002").is_ok());

        // Verify both can be found with unique prefixes
        assert!(store.get("aaaa1111").is_ok());
        assert!(store.get("aaaa2222").is_ok());

        // Should fail with ambiguous prefix "aaaa"
        let result = store.get("aaaa");
        assert!(result.is_err());
        match result {
            Err(StorageError::Validation(msg)) => {
                assert!(msg.contains("Ambiguous"));
                assert!(msg.contains("2 memories"));
            }
            other => panic!(
                "Expected Validation error for ambiguous prefix, got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_get_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();

        let result = store.get("nonexistent-id");
        assert!(result.is_err());
        match result {
            Err(StorageError::NotFound(id)) => {
                assert_eq!(id, "nonexistent-id");
            }
            _ => panic!("Expected NotFound error"),
        }
    }

    #[test]
    fn test_update_modifies_memory() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();
        let memory = create_test_memory("test-update-123", Visibility::Shared);

        store.create(&memory).unwrap();

        // Update summary
        let mut update = MemoryUpdate::new();
        update.summary = Some("Updated summary".to_string());

        store.update("test-update-123", update).unwrap();

        let retrieved = store.get("test-update-123").unwrap();
        assert_eq!(retrieved.summary, "Updated summary");
        assert_eq!(retrieved.content, "Test content"); // Content unchanged
    }

    #[test]
    fn test_delete_removes_memory() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();
        let memory = create_test_memory("test-delete-123", Visibility::Shared);

        store.create(&memory).unwrap();

        // Verify it exists
        assert!(store.get("test-delete-123").is_ok());

        // Delete it
        store.delete("test-delete-123").unwrap();

        // Verify it's gone
        let result = store.get("test-delete-123");
        assert!(result.is_err());
        match result {
            Err(StorageError::NotFound(_)) => {}
            _ => panic!("Expected NotFound error after delete"),
        }
    }

    #[test]
    fn test_list_returns_all() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();

        // Create 3 memories
        let memory1 = create_test_memory("list-test-1", Visibility::Shared);
        let memory2 = create_test_memory("list-test-2", Visibility::Shared);
        let memory3 = create_test_memory("list-test-3", Visibility::Personal);

        store.create(&memory1).unwrap();
        store.create(&memory2).unwrap();
        store.create(&memory3).unwrap();

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 3);

        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"list-test-1"));
        assert!(ids.contains(&"list-test-2"));
        assert!(ids.contains(&"list-test-3"));
    }

    #[test]
    fn test_reindex_rebuilds_from_files() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();

        // Create some memories
        let memory1 = create_test_memory("reindex-test-1", Visibility::Shared);
        let memory2 = create_test_memory("reindex-test-2", Visibility::Shared);

        store.create(&memory1).unwrap();
        store.create(&memory2).unwrap();

        // Verify they're in the index
        assert_eq!(store.list().unwrap().len(), 2);

        // Overwrite index with empty
        let empty_index = index::Index::default();
        let index_path = project_dir.join(".engramdb/index.json");
        index::save_index(&index_path, &empty_index).unwrap();

        // Verify index is now empty
        let loaded = index::load_index(&index_path).unwrap();
        assert_eq!(loaded.memories.len(), 0);

        // Reindex
        let count = store.reindex().unwrap();
        assert_eq!(count, 2);

        // Verify entries are restored
        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 2);

        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"reindex-test-1"));
        assert!(ids.contains(&"reindex-test-2"));
    }
}
