//! Main storage orchestrator - MemoryStore.
//!
//! This module provides the [`MemoryStore`] struct, which orchestrates all
//! file system operations for memories:
//! - Initialize new EngramDB stores with `init()`
//! - Open existing stores with `open()`
//! - Create, read, update, delete memories (CRUD operations)
//! - List all memories via unified LanceDB index
//! - Rebuild index from files with `reindex()`
//! - Vector search and embedding storage via LanceDB
//!
//! MemoryStore handles both shared (project-level) and personal (user-level)
//! memories in a single LanceDB table with a `visibility` column. It also
//! manages the global registry of projects and updates manifest statistics
//! automatically.
//!
//! ID matching supports prefix matching for convenience (e.g., "abcd" matches
//! "abcd1234-5678-..."), with ambiguity detection.

use super::error::{Result, StorageError};
use super::lance_index::{IndexEntry, LanceIndex, VectorMatch};
use super::{manifest, memory_file, paths, project_id};
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
/// Manages memory files, a unified LanceDB index, manifest, and coordinates
/// between shared (project-level) and personal (user-level) storage locations.
pub struct MemoryStore {
    /// Root directory of the project (contains .engramdb/)
    pub project_dir: PathBuf,
    /// Unique identifier for this project
    pub project_id: String,
    /// Unified LanceDB index (metadata + optional vectors)
    lance_index: LanceIndex,
}

impl MemoryStore {
    /// Initialize a new EngramDB store in the given directory.
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

        // Compute project ID before creating global directories
        let project_id = project_id::compute_project_id(dir);

        // Create global LanceDB directory
        let lance_path = paths::lancedb_dir(&project_id)?;
        fs::create_dir_all(&lance_path)?;

        // Initialize LanceIndex
        let lance_index = LanceIndex::new(&lance_path)
            .map_err(|e| StorageError::Validation(format!("LanceDB init failed: {}", e)))?;

        // Create personal directories
        fs::create_dir_all(paths::personal_memories_dir(&project_id)?)?;

        // Update registry
        Self::update_registry(dir, &project_id)?;

        Ok(Self {
            project_dir: dir.to_path_buf(),
            project_id,
            lance_index,
        })
    }

    /// Open an existing EngramDB store.
    pub fn open(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        if !engramdb_dir.exists() {
            return Err(StorageError::NotInitialized);
        }

        // Compute project ID
        let project_id = project_id::compute_project_id(dir);

        // Open (or create) global LanceDB
        let lance_path = paths::lancedb_dir(&project_id)?;
        fs::create_dir_all(&lance_path)?;

        let lance_index = LanceIndex::new(&lance_path)
            .map_err(|e| StorageError::Validation(format!("LanceDB open failed: {}", e)))?;

        let store = Self {
            project_dir: dir.to_path_buf(),
            project_id,
            lance_index,
        };

        // Update registry
        Self::update_registry(dir, &store.project_id)?;

        Ok(store)
    }

    /// Create a new memory.
    pub fn create(&self, memory: &Memory) -> Result<String> {
        let memories_dir = self.get_memories_dir(&memory.visibility)?;
        fs::create_dir_all(&memories_dir)?;

        // Write memory file
        let file_path = memories_dir.join(format!("{}.md", memory.id));
        let content = memory_file::write_memory_file(memory)?;
        fs::write(&file_path, content)?;

        // Upsert to LanceDB (vector=None for now)
        let entry = IndexEntry::from(memory);
        self.lance_index
            .upsert(&entry, None)
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert failed: {}", e)))?;

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(memory.id.clone())
    }

    /// Get a memory by ID (supports prefix matching).
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

    /// Update a memory.
    pub fn update(&self, id: &str, updates: MemoryUpdate) -> Result<()> {
        // Get existing memory
        let mut memory = self.get(id)?;
        let old_visibility = memory.visibility;

        // Apply updates
        updates.apply_to(&mut memory);

        // Check if visibility changed
        if memory.visibility != old_visibility {
            // Delete from old location (file only)
            self.delete_file_from_dir(id, &self.get_memories_dir(&old_visibility)?)?;

            // Write to new location
            let memories_dir = self.get_memories_dir(&memory.visibility)?;
            fs::create_dir_all(&memories_dir)?;
            let file_path = memories_dir.join(format!("{}.md", memory.id));
            let content = memory_file::write_memory_file(&memory)?;
            fs::write(&file_path, content)?;
        } else {
            // Write updated memory
            let memories_dir = self.get_memories_dir(&memory.visibility)?;
            let file_path = memories_dir.join(format!("{}.md", memory.id));
            let content = memory_file::write_memory_file(&memory)?;
            fs::write(&file_path, content)?;
        }

        // Upsert to LanceDB (preserving existing vector if present)
        let entry = IndexEntry::from(&memory);
        self.lance_index
            .upsert(&entry, None)
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert failed: {}", e)))?;

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(())
    }

    /// Delete a memory.
    pub fn delete(&self, id: &str) -> Result<()> {
        // Resolve full ID first via the index
        let full_id = self.resolve_full_id(id)?;

        // Try to delete file from shared
        let shared_deleted =
            self.delete_file_from_dir(&full_id, &paths::memories_dir(&self.project_dir));

        // If not in shared, try personal
        if shared_deleted.is_err() {
            self.delete_file_from_dir(&full_id, &paths::personal_memories_dir(&self.project_id)?)?;
        }

        // Delete from LanceDB
        self.lance_index
            .delete(&full_id)
            .map_err(|e| StorageError::Validation(format!("LanceDB delete failed: {}", e)))?;

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(())
    }

    /// List all memories (returns index entries from LanceDB).
    pub fn list(&self) -> Result<Vec<IndexEntry>> {
        self.lance_index
            .list()
            .map_err(|e| StorageError::Validation(format!("LanceDB list failed: {}", e)))
    }

    /// Rebuild LanceDB index from memory files on disk.
    pub fn reindex(&self) -> Result<usize> {
        // Clear LanceDB
        self.lance_index
            .clear()
            .map_err(|e| StorageError::Validation(format!("LanceDB clear failed: {}", e)))?;

        let mut count = 0;

        // Reindex shared memories
        let shared_dir = paths::memories_dir(&self.project_dir);
        if shared_dir.exists() {
            count += self.reindex_dir(&shared_dir, Visibility::Shared)?;
        }

        // Reindex personal memories
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        if personal_dir.exists() {
            count += self.reindex_dir(&personal_dir, Visibility::Personal)?;
        }

        // Update manifest stats
        self.update_manifest_stats()?;

        Ok(count)
    }

    /// Update the vector embedding for a memory.
    pub fn update_vector(&self, id: &str, vector: Vec<f32>) -> Result<()> {
        self.lance_index
            .update_vector(id, vector)
            .map_err(|e| StorageError::Validation(format!("LanceDB update_vector failed: {}", e)))
    }

    /// Perform vector similarity search.
    pub fn vector_search(&self, query: Vec<f32>, limit: usize) -> Result<Vec<VectorMatch>> {
        self.lance_index
            .vector_search(query, limit)
            .map_err(|e| StorageError::Validation(format!("LanceDB vector_search failed: {}", e)))
    }

    // ---- Helper methods ----

    fn get_memories_dir(&self, visibility: &Visibility) -> Result<PathBuf> {
        match visibility {
            Visibility::Shared => Ok(paths::memories_dir(&self.project_dir)),
            Visibility::Personal => paths::personal_memories_dir(&self.project_id),
        }
    }

    /// Delete just the .md file from a directory (does not touch LanceDB).
    fn delete_file_from_dir(&self, id: &str, dir: &Path) -> Result<()> {
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
                fs::remove_file(&matches[0])?;
                Ok(())
            }
            _ => Err(StorageError::Validation(format!(
                "Ambiguous ID prefix '{}': matches {} memories",
                id,
                matches.len()
            ))),
        }
    }

    /// Resolve a prefix ID to a full ID using the LanceDB index.
    fn resolve_full_id(&self, id: &str) -> Result<String> {
        let entries = self.list()?;
        let matches: Vec<&IndexEntry> = entries.iter().filter(|e| e.id.starts_with(id)).collect();

        match matches.len() {
            0 => Err(StorageError::NotFound(id.to_string())),
            1 => Ok(matches[0].id.clone()),
            _ => Err(StorageError::Validation(format!(
                "Ambiguous ID prefix '{}': matches {} memories",
                id,
                matches.len()
            ))),
        }
    }

    /// Reindex all .md files in a directory with a given visibility.
    fn reindex_dir(&self, dir: &Path, visibility: Visibility) -> Result<usize> {
        let mut count = 0;
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                let content = fs::read_to_string(&path)?;
                let memory = memory_file::parse_memory_file(&content)?;
                let mut index_entry = IndexEntry::from(&memory);
                index_entry.visibility = visibility;
                self.lance_index.upsert(&index_entry, None).map_err(|e| {
                    StorageError::Validation(format!("LanceDB upsert failed: {}", e))
                })?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Check whether the LanceDB index is stale compared to memory files on disk.
    ///
    /// Returns `Some(warning_message)` if the counts differ, `None` if in sync.
    pub fn check_staleness(&self) -> Result<Option<String>> {
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        let md_count = count_md_files(&shared_dir) + count_md_files(&personal_dir);
        let lance_count = self
            .lance_index
            .count()
            .map_err(|e| StorageError::Validation(format!("LanceDB count failed: {}", e)))?;
        if md_count != lance_count {
            Ok(Some(format!(
                "Index may be stale ({} memories on disk, {} indexed). Run 'engramdb reindex' to rebuild.",
                md_count, lance_count
            )))
        } else {
            Ok(None)
        }
    }

    fn update_manifest_stats(&self) -> Result<()> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let mut manifest = manifest::load_manifest(&manifest_path)?;

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

        if let Some(parent) = registry_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut registry = if registry_path.exists() {
            let content = fs::read_to_string(&registry_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            Registry::default()
        };

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

        let content = serde_json::to_string_pretty(&registry)?;
        fs::write(&registry_path, content)?;

        Ok(())
    }
}

/// Count `.md` files in a directory. Returns 0 if the directory doesn't exist.
fn count_md_files(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("md"))
                .count()
        })
        .unwrap_or(0)
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

        // Check project-local directories
        assert!(project_dir.join(".engramdb").exists());
        assert!(project_dir.join(".engramdb/memories").exists());

        // LanceDB should NOT be in the project directory
        assert!(!project_dir.join(".engramdb/lancedb").exists());

        // LanceDB should be in the global data directory
        assert!(paths::lancedb_dir(&store.project_id).unwrap().exists());

        // Check files
        assert!(project_dir.join(".engramdb/manifest.toml").exists());
        assert!(project_dir.join(".engramdb/config.toml").exists());

        // No index.json should be created
        assert!(!project_dir.join(".engramdb/index.json").exists());

        // Check personal directories
        assert!(paths::personal_memories_dir(&store.project_id)
            .unwrap()
            .exists());
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

        let retrieved = store.get("abcd1234").unwrap();
        assert_eq!(retrieved.id, "abcd1234-5678-90ab-cdef-1234567890ab");

        let retrieved = store.get("abcd").unwrap();
        assert_eq!(retrieved.id, "abcd1234-5678-90ab-cdef-1234567890ab");
    }

    #[test]
    fn test_get_ambiguous_prefix() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();

        let memory1 =
            create_test_memory("aaaa1111-0000-0000-0000-000000000001", Visibility::Shared);
        let memory2 =
            create_test_memory("aaaa2222-0000-0000-0000-000000000002", Visibility::Shared);

        store.create(&memory1).unwrap();
        store.create(&memory2).unwrap();

        assert!(store.get("aaaa1111-0000-0000-0000-000000000001").is_ok());
        assert!(store.get("aaaa2222-0000-0000-0000-000000000002").is_ok());
        assert!(store.get("aaaa1111").is_ok());
        assert!(store.get("aaaa2222").is_ok());

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

        let mut update = MemoryUpdate::new();
        update.summary = Some("Updated summary".to_string());

        store.update("test-update-123", update).unwrap();

        let retrieved = store.get("test-update-123").unwrap();
        assert_eq!(retrieved.summary, "Updated summary");
        assert_eq!(retrieved.content, "Test content");
    }

    #[test]
    fn test_delete_removes_memory() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();
        let memory = create_test_memory("test-delete-123", Visibility::Shared);

        store.create(&memory).unwrap();
        assert!(store.get("test-delete-123").is_ok());

        store.delete("test-delete-123").unwrap();

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

        let memory1 = create_test_memory("reindex-test-1", Visibility::Shared);
        let memory2 = create_test_memory("reindex-test-2", Visibility::Shared);

        store.create(&memory1).unwrap();
        store.create(&memory2).unwrap();

        assert_eq!(store.list().unwrap().len(), 2);

        // Clear LanceDB to simulate corruption
        store.lance_index.clear().unwrap();
        assert_eq!(store.list().unwrap().len(), 0);

        // Reindex
        let count = store.reindex().unwrap();
        assert_eq!(count, 2);

        let entries = store.list().unwrap();
        assert_eq!(entries.len(), 2);

        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"reindex-test-1"));
        assert!(ids.contains(&"reindex-test-2"));
    }

    #[test]
    fn test_list_includes_visibility() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();

        let shared = create_test_memory("vis-shared", Visibility::Shared);
        let personal = create_test_memory("vis-personal", Visibility::Personal);

        store.create(&shared).unwrap();
        store.create(&personal).unwrap();

        let entries = store.list().unwrap();
        let shared_entry = entries.iter().find(|e| e.id == "vis-shared").unwrap();
        let personal_entry = entries.iter().find(|e| e.id == "vis-personal").unwrap();

        assert_eq!(shared_entry.visibility, Visibility::Shared);
        assert_eq!(personal_entry.visibility, Visibility::Personal);
    }

    #[test]
    fn test_check_staleness_in_sync() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();
        let memory = create_test_memory("staleness-sync-1", Visibility::Shared);
        store.create(&memory).unwrap();

        let result = store.check_staleness().unwrap();
        assert!(
            result.is_none(),
            "Expected no staleness warning when in sync"
        );
    }

    #[test]
    fn test_check_staleness_detects_mismatch() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir).unwrap();
        let memory = create_test_memory("staleness-mismatch-1", Visibility::Shared);
        store.create(&memory).unwrap();

        // Delete LanceDB entry but leave the .md file
        store.lance_index.clear().unwrap();

        let result = store.check_staleness().unwrap();
        assert!(
            result.is_some(),
            "Expected staleness warning after clearing index"
        );
        let warning = result.unwrap();
        assert!(warning.contains("1 memories on disk"));
        assert!(warning.contains("0 indexed"));
    }
}
