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
use super::registry::RegistryBackend;
use super::{manifest, memory_file, paths, project_id};
use crate::storage::config::load_config;
use crate::types::{Memory, MemoryUpdate, Visibility};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;

/// Main storage interface for EngramDB operations.
///
/// Manages memory files, a unified LanceDB index, manifest, and coordinates
/// between shared (project-level) and personal (user-level) storage locations.
///
/// # Cloning
/// Cloning is cheap (Arc'd LanceDB connection, paths). However, `update()`
/// does a non-atomic read-modify-write on the filesystem, so concurrent
/// updates to the **same** memory ID from different clones can race.
/// Callers must ensure cloned handles don't write to overlapping IDs.
#[derive(Clone)]
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
    pub async fn init(dir: &Path, registry: &dyn RegistryBackend) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        // Create directory structure
        async_fs::create_dir_all(&engramdb_dir).await?;
        async_fs::create_dir_all(paths::memories_dir(dir)).await?;

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
        manifest::save_manifest(&manifest_path, &manifest).await?;

        // Create empty config.toml
        let config_path = engramdb_dir.join("config.toml");
        async_fs::write(
            config_path,
            "# EngramDB configuration\n# See documentation for available settings\n",
        )
        .await?;

        // Compute project ID before creating global directories
        let project_id = project_id::compute_project_id(dir);

        // Create global LanceDB directory
        let lance_path = paths::lancedb_dir(&project_id)?;
        async_fs::create_dir_all(&lance_path).await?;

        // Load config to get embedding dimensions
        let config_path = engramdb_dir.join("config.toml");
        let config = load_config(&config_path).await.unwrap_or_default();

        // Initialize LanceIndex with configured dimensions
        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB init failed: {}", e)))?;

        // Create personal directories
        async_fs::create_dir_all(paths::personal_memories_dir(&project_id)?).await?;

        // Update registry
        registry.update(dir, &project_id).await?;

        Ok(Self {
            project_dir: dir.to_path_buf(),
            project_id,
            lance_index,
        })
    }

    /// Open an existing EngramDB store.
    pub async fn open(dir: &Path, registry: &dyn RegistryBackend) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        if !engramdb_dir.exists() {
            return Err(StorageError::NotInitialized);
        }

        // Compute project ID
        let project_id = project_id::compute_project_id(dir);

        // Load config to get embedding dimensions
        let config_path = engramdb_dir.join("config.toml");
        let config = load_config(&config_path).await.unwrap_or_default();

        // Open (or create) global LanceDB
        let lance_path = paths::lancedb_dir(&project_id)?;
        async_fs::create_dir_all(&lance_path).await?;

        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB open failed: {}", e)))?;

        let store = Self {
            project_dir: dir.to_path_buf(),
            project_id,
            lance_index,
        };

        // Update registry
        registry.update(dir, &store.project_id).await?;

        Ok(store)
    }

    /// Create a new memory.
    pub async fn create(&self, memory: &Memory) -> Result<String> {
        let memories_dir = self.get_memories_dir(&memory.visibility)?;
        async_fs::create_dir_all(&memories_dir).await?;

        // Write memory file
        let file_path = memories_dir.join(format!("{}.md", memory.id));
        let content = memory_file::write_memory_file(memory)?;
        async_fs::write(&file_path, content).await?;

        // Upsert metadata to LanceDB (vectors stored separately in chunks table)
        let entry = IndexEntry::from(memory);
        self.lance_index
            .upsert(&entry)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert failed: {}", e)))?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(memory.id.clone())
    }

    /// Get a memory by ID (supports prefix matching).
    pub async fn get(&self, id: &str) -> Result<Memory> {
        // Try shared memories first
        match self
            .get_from_dir(id, &paths::memories_dir(&self.project_dir))
            .await
        {
            Ok(memory) => return Ok(memory),
            Err(StorageError::Validation(msg)) => return Err(StorageError::Validation(msg)),
            Err(StorageError::NotFound(_)) => {
                // Fall through to personal
            }
            Err(e) => return Err(e),
        }

        // Try personal memories
        self.get_from_dir(id, &paths::personal_memories_dir(&self.project_id)?)
            .await
    }

    async fn get_from_dir(&self, id: &str, dir: &Path) -> Result<Memory> {
        if !dir.exists() {
            return Err(StorageError::NotFound(id.to_string()));
        }

        let mut matches = Vec::new();

        let mut entries = async_fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
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
                let content = async_fs::read_to_string(&matches[0]).await?;
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
    pub async fn update(&self, id: &str, updates: MemoryUpdate) -> Result<()> {
        // Get existing memory
        let mut memory = self.get(id).await?;
        let old_visibility = memory.visibility;

        // Apply updates
        updates.apply_to(&mut memory);

        // Check if visibility changed
        if memory.visibility != old_visibility {
            // Delete from old location (file only)
            self.delete_file_from_dir(id, &self.get_memories_dir(&old_visibility)?)
                .await?;

            // Write to new location
            let memories_dir = self.get_memories_dir(&memory.visibility)?;
            async_fs::create_dir_all(&memories_dir).await?;
            let file_path = memories_dir.join(format!("{}.md", memory.id));
            let content = memory_file::write_memory_file(&memory)?;
            async_fs::write(&file_path, content).await?;
        } else {
            // Write updated memory
            let memories_dir = self.get_memories_dir(&memory.visibility)?;
            let file_path = memories_dir.join(format!("{}.md", memory.id));
            let content = memory_file::write_memory_file(&memory)?;
            async_fs::write(&file_path, content).await?;
        }

        // Upsert metadata to LanceDB (chunks are managed separately)
        let entry = IndexEntry::from(&memory);
        self.lance_index
            .upsert(&entry)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert failed: {}", e)))?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(())
    }

    /// Delete a memory.
    pub async fn delete(&self, id: &str) -> Result<()> {
        // Resolve full ID first via the index
        let full_id = self.resolve_full_id(id).await?;

        // Try to delete file from shared
        let shared_deleted = self
            .delete_file_from_dir(&full_id, &paths::memories_dir(&self.project_dir))
            .await;

        // If not in shared, try personal
        if shared_deleted.is_err() {
            self.delete_file_from_dir(&full_id, &paths::personal_memories_dir(&self.project_id)?)
                .await?;
        }

        // Delete from LanceDB (metadata + chunks)
        self.lance_index
            .delete(&full_id)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB delete failed: {}", e)))?;
        self.lance_index
            .delete_chunks(&full_id)
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB delete_chunks failed: {}", e))
            })?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(())
    }

    /// List all memories (returns index entries from LanceDB).
    pub async fn list(&self) -> Result<Vec<IndexEntry>> {
        self.lance_index
            .list()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB list failed: {}", e)))
    }

    /// Rebuild LanceDB index from memory files on disk.
    pub async fn reindex(&self) -> Result<usize> {
        // Clear LanceDB
        self.lance_index
            .clear()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB clear failed: {}", e)))?;

        let mut count = 0;

        // Reindex shared memories
        let shared_dir = paths::memories_dir(&self.project_dir);
        if shared_dir.exists() {
            count += self.reindex_dir(&shared_dir, Visibility::Shared).await?;
        }

        // Reindex personal memories
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        if personal_dir.exists() {
            count += self
                .reindex_dir(&personal_dir, Visibility::Personal)
                .await?;
        }

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(count)
    }

    /// Upsert embedding chunks for a memory.
    pub async fn upsert_chunks(&self, memory_id: &str, chunks: Vec<Vec<f32>>) -> Result<()> {
        self.lance_index
            .upsert_chunks(memory_id, chunks)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert_chunks failed: {}", e)))
    }

    /// Delete all embedding chunks for a memory.
    pub async fn delete_chunks(&self, memory_id: &str) -> Result<()> {
        self.lance_index
            .delete_chunks(memory_id)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB delete_chunks failed: {}", e)))
    }

    /// Perform vector similarity search.
    pub async fn vector_search(&self, query: Vec<f32>, limit: usize) -> Result<Vec<VectorMatch>> {
        self.lance_index
            .vector_search(query, limit)
            .await
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
    async fn delete_file_from_dir(&self, id: &str, dir: &Path) -> Result<()> {
        if !dir.exists() {
            return Err(StorageError::NotFound(id.to_string()));
        }

        let mut matches = Vec::new();
        let mut entries = async_fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
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
                async_fs::remove_file(&matches[0]).await?;
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
    async fn resolve_full_id(&self, id: &str) -> Result<String> {
        let entries = self.list().await?;
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
    async fn reindex_dir(&self, dir: &Path, visibility: Visibility) -> Result<usize> {
        let mut count = 0;
        let mut entries = async_fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                let content = async_fs::read_to_string(&path).await?;
                let memory = memory_file::parse_memory_file(&content)?;
                let mut index_entry = IndexEntry::from(&memory);
                index_entry.visibility = visibility;
                self.lance_index.upsert(&index_entry).await.map_err(|e| {
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
    pub async fn check_staleness(&self) -> Result<Option<String>> {
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        let md_count = count_md_files(&shared_dir).await + count_md_files(&personal_dir).await;
        let lance_count = self
            .lance_index
            .count()
            .await
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

    async fn update_manifest_stats(&self) -> Result<()> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let mut manifest = manifest::load_manifest(&manifest_path).await?;

        let entries = self.list().await?;
        let memory_count = entries.len();
        let logical_scopes: Vec<String> = entries
            .iter()
            .flat_map(|e| e.logical.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        manifest::update_stats(&mut manifest, memory_count, logical_scopes);
        manifest::save_manifest(&manifest_path, &manifest).await?;

        Ok(())
    }
}

/// Count `.md` files in a directory. Returns 0 if the directory doesn't exist.
async fn count_md_files(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    let Ok(mut entries) = async_fs::read_dir(dir).await else {
        return 0;
    };
    let mut count = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().extension().and_then(|s| s.to_str()) == Some("md") {
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::registry::InMemoryRegistry;
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

    #[tokio::test]
    async fn test_init_creates_structure() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();

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

    #[tokio::test]
    async fn test_open_uninitialized() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let result = MemoryStore::open(project_dir, &InMemoryRegistry::new()).await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NotInitialized) => {}
            _ => panic!("Expected NotInitialized error"),
        }
    }

    #[tokio::test]
    async fn test_create_and_get() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("test-create-123", Visibility::Shared);

        let created_id = store.create(&memory).await.unwrap();
        assert_eq!(created_id, "test-create-123");

        let retrieved = store.get("test-create-123").await.unwrap();
        assert_eq!(retrieved.id, "test-create-123");
        assert_eq!(retrieved.summary, "Test summary");
        assert_eq!(retrieved.content, "Test content");
    }

    #[tokio::test]
    async fn test_get_prefix_matching() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("abcd1234-5678-90ab-cdef-1234567890ab", Visibility::Shared);

        store.create(&memory).await.unwrap();

        let retrieved = store.get("abcd1234").await.unwrap();
        assert_eq!(retrieved.id, "abcd1234-5678-90ab-cdef-1234567890ab");

        let retrieved = store.get("abcd").await.unwrap();
        assert_eq!(retrieved.id, "abcd1234-5678-90ab-cdef-1234567890ab");
    }

    #[tokio::test]
    async fn test_get_ambiguous_prefix() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();

        let memory1 =
            create_test_memory("aaaa1111-0000-0000-0000-000000000001", Visibility::Shared);
        let memory2 =
            create_test_memory("aaaa2222-0000-0000-0000-000000000002", Visibility::Shared);

        store.create(&memory1).await.unwrap();
        store.create(&memory2).await.unwrap();

        assert!(store
            .get("aaaa1111-0000-0000-0000-000000000001")
            .await
            .is_ok());
        assert!(store
            .get("aaaa2222-0000-0000-0000-000000000002")
            .await
            .is_ok());
        assert!(store.get("aaaa1111").await.is_ok());
        assert!(store.get("aaaa2222").await.is_ok());

        let result = store.get("aaaa").await;
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

    #[tokio::test]
    async fn test_get_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();

        let result = store.get("nonexistent-id").await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NotFound(id)) => {
                assert_eq!(id, "nonexistent-id");
            }
            _ => panic!("Expected NotFound error"),
        }
    }

    #[tokio::test]
    async fn test_update_modifies_memory() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("test-update-123", Visibility::Shared);

        store.create(&memory).await.unwrap();

        let mut update = MemoryUpdate::new();
        update.summary = Some("Updated summary".to_string());

        store.update("test-update-123", update).await.unwrap();

        let retrieved = store.get("test-update-123").await.unwrap();
        assert_eq!(retrieved.summary, "Updated summary");
        assert_eq!(retrieved.content, "Test content");
    }

    #[tokio::test]
    async fn test_delete_removes_memory() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("test-delete-123", Visibility::Shared);

        store.create(&memory).await.unwrap();
        assert!(store.get("test-delete-123").await.is_ok());

        store.delete("test-delete-123").await.unwrap();

        let result = store.get("test-delete-123").await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NotFound(_)) => {}
            _ => panic!("Expected NotFound error after delete"),
        }
    }

    #[tokio::test]
    async fn test_list_returns_all() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();

        let memory1 = create_test_memory("list-test-1", Visibility::Shared);
        let memory2 = create_test_memory("list-test-2", Visibility::Shared);
        let memory3 = create_test_memory("list-test-3", Visibility::Personal);

        store.create(&memory1).await.unwrap();
        store.create(&memory2).await.unwrap();
        store.create(&memory3).await.unwrap();

        let entries = store.list().await.unwrap();
        assert_eq!(entries.len(), 3);

        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"list-test-1"));
        assert!(ids.contains(&"list-test-2"));
        assert!(ids.contains(&"list-test-3"));
    }

    #[tokio::test]
    async fn test_reindex_rebuilds_from_files() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();

        let memory1 = create_test_memory("reindex-test-1", Visibility::Shared);
        let memory2 = create_test_memory("reindex-test-2", Visibility::Shared);

        store.create(&memory1).await.unwrap();
        store.create(&memory2).await.unwrap();

        assert_eq!(store.list().await.unwrap().len(), 2);

        // Clear LanceDB to simulate corruption
        store.lance_index.clear().await.unwrap();
        assert_eq!(store.list().await.unwrap().len(), 0);

        // Reindex
        let count = store.reindex().await.unwrap();
        assert_eq!(count, 2);

        let entries = store.list().await.unwrap();
        assert_eq!(entries.len(), 2);

        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"reindex-test-1"));
        assert!(ids.contains(&"reindex-test-2"));
    }

    #[tokio::test]
    async fn test_list_includes_visibility() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();

        let shared = create_test_memory("vis-shared", Visibility::Shared);
        let personal = create_test_memory("vis-personal", Visibility::Personal);

        store.create(&shared).await.unwrap();
        store.create(&personal).await.unwrap();

        let entries = store.list().await.unwrap();
        let shared_entry = entries.iter().find(|e| e.id == "vis-shared").unwrap();
        let personal_entry = entries.iter().find(|e| e.id == "vis-personal").unwrap();

        assert_eq!(shared_entry.visibility, Visibility::Shared);
        assert_eq!(personal_entry.visibility, Visibility::Personal);
    }

    #[tokio::test]
    async fn test_check_staleness_in_sync() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("staleness-sync-1", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let result = store.check_staleness().await.unwrap();
        assert!(
            result.is_none(),
            "Expected no staleness warning when in sync"
        );
    }

    #[tokio::test]
    async fn test_check_staleness_detects_mismatch() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let store = MemoryStore::init(project_dir, &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("staleness-mismatch-1", Visibility::Shared);
        store.create(&memory).await.unwrap();

        // Delete LanceDB entry but leave the .md file
        store.lance_index.clear().await.unwrap();

        let result = store.check_staleness().await.unwrap();
        assert!(
            result.is_some(),
            "Expected staleness warning after clearing index"
        );
        let warning = result.unwrap();
        assert!(warning.contains("1 memories on disk"));
        assert!(warning.contains("0 indexed"));
    }
}
