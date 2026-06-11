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
use super::lance_index::{
    IndexEntry, IndexFilterable, IndexForFiltering, IndexSummary, LanceIndex, VectorMatch,
};
use super::registry::RegistryBackend;
use super::{manifest, memory_file, paths, project_id, write_lock};
use crate::config::load_config;
use engram_types::{Memory, MemoryUpdate, Visibility};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;

/// Main storage interface for EngramDB operations.
///
/// Manages memory files, a unified LanceDB index, manifest, and coordinates
/// between shared (project-level) and personal (user-level) storage locations.
///
/// # Concurrency
/// Mutating operations (`create`, `update`, `delete`, `reindex`,
/// `upsert_chunks`, `delete_chunks`) acquire an advisory file lock
/// (`flock(2)`) per project, serializing concurrent writes across processes.
/// Read operations (`get`, `list_*`, `count`, `get_batch`, `batch_exists`)
/// are lock-free — LanceDB MVCC handles concurrent readers.
///
/// File writes use atomic temp-then-rename to prevent partial reads.
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
    ///
    /// Idempotent: re-running `init` on an already-initialized project is
    /// safe. Existing `manifest.toml` (created_at, stats, parent link,
    /// embedding fingerprint) and `config.toml` (user-customized provider /
    /// dimensions / NLI / rerank settings) are never overwritten; only
    /// missing pieces are created.
    pub async fn init(dir: &Path, registry: &dyn RegistryBackend) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        // Compute the project ID up front so concurrent inits serialize on
        // the per-project advisory write lock. The lock file lives under the
        // *global* data dir (`projects/<id>/write.lock`), and
        // `acquire_write_lock` creates that directory itself, so there is no
        // bootstrap-ordering problem with the project dirs created below.
        let project_id = project_id::compute_project_id(dir);
        let _lock = write_lock::acquire_write_lock(&project_id).await?;

        // Create directory structure (create_dir_all is idempotent)
        async_fs::create_dir_all(&engramdb_dir).await?;
        async_fs::create_dir_all(paths::memories_dir(dir)).await?;

        // Create manifest.toml only if missing. Re-running `init` must not
        // reset created_at / stats / parent_project_id, and critically must
        // not drop the embedding fingerprint (that would flip the store to
        // Untracked and defeat the model-swap corruption guard).
        let manifest_path = engramdb_dir.join("manifest.toml");
        if !manifest_path.exists() {
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
        }

        // Create the placeholder config.toml only if missing — never clobber
        // a user-customized config (a dimensions change alone would make the
        // next open build LanceIndex against an incompatible chunks table).
        let config_path = engramdb_dir.join("config.toml");
        if !config_path.exists() {
            async_fs::write(
                &config_path,
                "# EngramDB configuration\n# See documentation for available settings\n",
            )
            .await?;
        }

        // Create global LanceDB directory
        let lance_path = paths::lancedb_dir(&project_id)?;
        async_fs::create_dir_all(&lance_path).await?;

        // Load config to get embedding dimensions
        let config = match load_config(&config_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "Failed to load config from {}, using defaults: {}",
                    config_path.display(),
                    e
                );
                engram_types::EngramConfig::default()
            }
        };

        // Initialize LanceIndex with configured dimensions
        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB init failed: {}", e)))?;

        // Create personal directories
        async_fs::create_dir_all(paths::personal_memories_dir(&project_id)?).await?;

        // Update registry. `update` keeps the first registration: if another
        // still-existing checkout already owns this project ID (two clones of
        // the same remote), the entry continues to point at it.
        registry.update(dir, &project_id).await?;

        // Surface the shared-ID situation loudly at init time. Destructive
        // index operations are additionally guarded in `reindex`; `doctor`
        // reports it as a warning check.
        if let Ok(reg) = registry.load().await {
            if let Some(other) = super::registry::conflicting_checkout_path(&reg, &project_id, dir)
            {
                tracing::warn!(
                    "Project ID {} is already registered to another checkout at {} — \
                     both checkouts share one index; see `engramdb doctor`",
                    project_id,
                    other.display()
                );
            }
        }

        Ok(Self {
            project_dir: dir.to_path_buf(),
            project_id,
            lance_index,
        })
    }

    /// Initialize the global memory store.
    ///
    /// The global store lives under `<global_data_dir>/global/` and mirrors
    /// a normal project layout (`.engramdb/memories/`, `manifest.toml`, etc.)
    /// so all `MemoryStore` methods work unchanged.
    pub async fn init_global() -> Result<Self> {
        let global_dir = paths::global_store_dir()?;
        let engramdb_dir = paths::project_dir(&global_dir);

        async_fs::create_dir_all(&engramdb_dir).await?;
        async_fs::create_dir_all(paths::memories_dir(&global_dir)).await?;

        // Create manifest
        let manifest_path = engramdb_dir.join("manifest.toml");
        if !manifest_path.exists() {
            let manifest = manifest::Manifest {
                project: "global".to_string(),
                ..Default::default()
            };
            manifest::save_manifest(&manifest_path, &manifest).await?;
        }

        // Create empty config if missing
        let config_path = engramdb_dir.join("config.toml");
        if !config_path.exists() {
            async_fs::write(
                &config_path,
                "# EngramDB global configuration\n# See documentation for available settings\n",
            )
            .await?;
        }

        let project_id = paths::GLOBAL_PROJECT_ID.to_string();

        // Create global LanceDB directory
        let lance_path = paths::global_lancedb_dir()?;
        async_fs::create_dir_all(&lance_path).await?;

        let config: engram_types::EngramConfig =
            load_config(&config_path).await.unwrap_or_default();

        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("Global LanceDB init failed: {}", e)))?;

        Ok(Self {
            project_dir: global_dir,
            project_id,
            lance_index,
        })
    }

    /// Open the global memory store, creating it if necessary.
    pub async fn open_global() -> Result<Self> {
        let global_dir = paths::global_store_dir()?;
        let engramdb_dir = paths::project_dir(&global_dir);

        if !engramdb_dir.exists() {
            return Self::init_global().await;
        }

        let project_id = paths::GLOBAL_PROJECT_ID.to_string();
        let lance_path = paths::global_lancedb_dir()?;
        async_fs::create_dir_all(&lance_path).await?;

        let config_path = engramdb_dir.join("config.toml");
        let config: engram_types::EngramConfig =
            load_config(&config_path).await.unwrap_or_default();

        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("Global LanceDB open failed: {}", e)))?;

        Ok(Self {
            project_dir: global_dir,
            project_id,
            lance_index,
        })
    }

    /// Returns `true` if this store is the global memory store.
    pub fn is_global(&self) -> bool {
        self.project_id == paths::GLOBAL_PROJECT_ID
    }

    /// Open an existing EngramDB store.
    pub async fn open(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        if !engramdb_dir.exists() {
            return Err(StorageError::NotInitialized);
        }

        // Compute project ID
        let project_id = project_id::compute_project_id(dir);

        // Load config to get embedding dimensions
        let config_path = engramdb_dir.join("config.toml");
        let config = match load_config(&config_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "Failed to load config from {}, using defaults: {}",
                    config_path.display(),
                    e
                );
                engram_types::EngramConfig::default()
            }
        };

        // Open (or create) global LanceDB
        let lance_path = paths::lancedb_dir(&project_id)?;
        async_fs::create_dir_all(&lance_path).await?;

        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB open failed: {}", e)))?;

        Ok(Self {
            project_dir: dir.to_path_buf(),
            project_id,
            lance_index,
        })
    }

    /// Create a new memory.
    pub async fn create(&self, memory: &Memory) -> Result<String> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let memories_dir = self.get_memories_dir(&memory.visibility)?;
        async_fs::create_dir_all(&memories_dir).await?;

        // Write memory file atomically (filename includes title slug if present)
        let filename = memory_file::memory_filename(memory);
        let file_path = memories_dir.join(&filename);
        let content = memory_file::write_memory_file(memory)?;
        atomic_write(&file_path, &content).await?;

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

    /// Get multiple memories by their full IDs in a single batch.
    ///
    /// Performs one directory scan of shared and personal memory dirs,
    /// then reads only the requested files.  This is O(dir_size + N)
    /// instead of O(dir_size × N) for N individual [`get`] calls.
    ///
    /// Returns a Vec of `(id, Memory)` pairs.  IDs that cannot be loaded
    /// (missing file, parse error) are silently skipped.
    pub async fn get_batch<S: AsRef<str>>(&self, ids: &[S]) -> Result<Vec<(String, Memory)>> {
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;

        let shared_map = scan_dir_to_map(&shared_dir).await;
        let personal_map = scan_dir_to_map(&personal_dir).await;

        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            let id_str = id.as_ref();
            let path = shared_map.get(id_str).or_else(|| personal_map.get(id_str));
            if let Some(path) = path {
                if let Ok(content) = async_fs::read_to_string(path).await {
                    if let Ok(memory) = memory_file::parse_memory_file(&content) {
                        results.push((id_str.to_owned(), memory));
                    }
                }
            }
        }
        Ok(results)
    }

    /// Check which of the given IDs have `.md` files on disk.
    ///
    /// Scans shared and personal directories once each, returning only
    /// those IDs that have a corresponding file.  Much cheaper than
    /// [`get_batch`] because no files are read or parsed.
    pub async fn batch_exists<S: AsRef<str>>(&self, ids: &[S]) -> Result<HashSet<String>> {
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;

        let mut on_disk = HashSet::new();
        collect_stems(&shared_dir, &mut on_disk).await;
        collect_stems(&personal_dir, &mut on_disk).await;

        Ok(ids
            .iter()
            .filter(|id| on_disk.contains(id.as_ref()))
            .map(|id| id.as_ref().to_owned())
            .collect())
    }

    async fn get_from_dir(&self, id: &str, dir: &Path) -> Result<Memory> {
        let matches = find_memory_files(dir, id).await?;

        match matches.len() {
            0 => Err(StorageError::NotFound(id.to_string())),
            1 => {
                let content = async_fs::read_to_string(&matches[0]).await?;
                memory_file::parse_memory_file(&content)
            }
            _ => {
                // Multiple files for ONE full ID (`find_memory_files` already
                // rejected distinct IDs as ambiguous): a stale duplicate left
                // by an update that crashed between writing the renamed file
                // and removing the old one. The newest file is the current
                // state — the rename path always writes the new file last.
                let newest = newest_by_mtime(&matches).await;
                let content = async_fs::read_to_string(newest).await?;
                memory_file::parse_memory_file(&content)
            }
        }
    }

    /// Update a memory.
    ///
    /// Note: callers that first read the memory to compute the update perform
    /// an unlocked read-modify-write — a concurrent update can be lost. Use
    /// [`MemoryStore::update_with`] to make read-modify-write atomic.
    pub async fn update(&self, id: &str, updates: MemoryUpdate) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        // Get existing memory (inside the lock)
        let mut memory = self.get(id).await?;
        let old_visibility = memory.visibility;

        // Apply updates
        updates.apply_to(&mut memory);

        self.write_updated_locked(id, &memory, old_visibility).await
    }

    /// Atomically read-modify-write a memory.
    ///
    /// Acquires the per-project write lock, re-reads the memory inside the
    /// lock, applies `f` to it, then persists via the same write path as
    /// [`MemoryStore::update`]. This makes the whole read-merge-write sequence
    /// one critical section, so two concurrent `update_with` calls (or an
    /// `update_with` racing an `update`) cannot silently erase each other's
    /// changes.
    ///
    /// The memory's `updated_at` is bumped after `f` succeeds, matching the
    /// `MemoryUpdate::apply_to` behavior of `update`. If `f` returns an error,
    /// nothing is written and the error is surfaced as a validation error.
    ///
    /// Returns the memory exactly as persisted.
    pub async fn update_with<F>(&self, id: &str, f: F) -> Result<Memory>
    where
        F: FnOnce(&mut Memory) -> anyhow::Result<()>,
    {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        // Re-read inside the lock so `f` sees the latest persisted state.
        let mut memory = self.get(id).await?;
        let old_visibility = memory.visibility;

        f(&mut memory).map_err(|e| StorageError::Validation(format!("{:#}", e)))?;
        memory.mark_updated();

        self.write_updated_locked(id, &memory, old_visibility)
            .await?;

        Ok(memory)
    }

    /// Shared write path for `update` / `update_with`. Callers MUST hold the
    /// per-project write lock — this helper does not (re-)acquire it, because
    /// one acquisition must span the entire read-modify-write critical
    /// section.
    async fn write_updated_locked(
        &self,
        id: &str,
        memory: &Memory,
        old_visibility: Visibility,
    ) -> Result<()> {
        // Locate the existing file(s) for this memory before writing. `id`
        // may be a prefix; the filename may be about to change (title or
        // visibility update); and a stale duplicate from a previously
        // crashed update may also still be present.
        let old_dir = self.get_memories_dir(&old_visibility)?;
        let old_paths = find_memory_files(&old_dir, id).await?;

        let memories_dir = self.get_memories_dir(&memory.visibility)?;
        async_fs::create_dir_all(&memories_dir).await?;
        let filename = memory_file::memory_filename(memory);
        let file_path = memories_dir.join(&filename);
        let content = memory_file::write_memory_file(memory)?;

        // Durability ordering: write the NEW file first, THEN remove any old
        // file at a different path. In the common case (filename unchanged)
        // the atomic_write replaces the file in place and nothing is deleted,
        // so the memory is never absent from disk and lock-free readers can
        // never observe a spurious NotFound. When the filename changed
        // (title/visibility update), a crash between the write and the
        // removal leaves at worst TWO files for one ID — readers resolve
        // that to the newest mtime (see `get_from_dir`) and the next
        // update/delete/reindex cleans the stale one up. The old
        // delete-then-write order could lose the memory's file entirely.
        atomic_write(&file_path, &content).await?;
        for old in &old_paths {
            if *old != file_path {
                match async_fs::remove_file(old).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }

        // Upsert metadata to LanceDB (chunks are managed separately)
        let entry = IndexEntry::from(memory);
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
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

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

    /// List all memories with filterable/displayable columns (12 of 14).
    ///
    /// Omits `provenance_source` and `confidence` which no caller reads.
    pub async fn list_filterable(&self) -> Result<Vec<IndexFilterable>> {
        self.lance_index
            .list_filterable()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB list_filterable failed: {}", e)))
    }

    /// List minimal columns for index-level filtering (6 of 14).
    ///
    /// Returns only the fields needed by `apply_index_filters` plus `id`.
    /// Use this for the retrieval pipeline where full metadata is loaded
    /// later via `get()` for surviving entries only.
    pub async fn list_for_filtering(&self) -> Result<Vec<IndexForFiltering>> {
        self.lance_index.list_for_filtering().await.map_err(|e| {
            StorageError::Validation(format!("LanceDB list_for_filtering failed: {}", e))
        })
    }

    /// List lightweight metadata summaries for all memories (7 columns).
    pub async fn list_summary(&self) -> Result<Vec<IndexSummary>> {
        self.lance_index
            .list_summary()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB list_summary failed: {}", e)))
    }

    /// List all memory IDs.
    pub async fn list_ids(&self) -> Result<Vec<String>> {
        self.lance_index
            .list_ids()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB list_ids failed: {}", e)))
    }

    /// Return the count of memories without loading data.
    pub async fn count(&self) -> Result<usize> {
        self.lance_index
            .count()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB count failed: {}", e)))
    }

    /// Rebuild the LanceDB metadata index from memory files on disk.
    ///
    /// Clears and rebuilds only the memories (metadata) table. Existing
    /// embedding vectors in the chunks table are preserved — chunks are
    /// keyed by `memory_id`, so a metadata rebuild does not invalidate
    /// them. Chunk rows whose `memory_id` no longer corresponds to a file
    /// on disk are deleted, keeping the chunks table consistent. Callers
    /// that intend to re-embed everything (and only those) should follow
    /// up with [`Self::clear_chunks`].
    ///
    /// **Shared-ID guard:** when [`Self::checkout_conflict`] detects that a
    /// different, still-existing checkout owns this project ID (two clones
    /// of the same git remote share one index but have separate
    /// `.engramdb/memories/` trees), the rebuild degrades to a
    /// non-destructive, upsert-only pass: the table is not cleared and
    /// orphan chunks are not pruned, so the other checkout's rows and
    /// vectors survive. Index rows have no per-checkout ownership column,
    /// so a clear here would be unrecoverable data loss for the other clone.
    pub async fn reindex(&self) -> Result<usize> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let foreign_checkout = self.checkout_conflict().await;
        if let Some(other) = &foreign_checkout {
            tracing::warn!(
                "Project ID is shared with another checkout at {} — running a \
                 non-destructive (upsert-only) reindex; index rows and vectors \
                 belonging to the other checkout are preserved",
                other.display()
            );
        } else {
            // Clear only the metadata table — never the vectors. Dropping the
            // chunks table here would silently destroy all embeddings whenever
            // no provider is available to rebuild them afterwards.
            self.lance_index
                .clear_memories()
                .await
                .map_err(|e| StorageError::Validation(format!("LanceDB clear failed: {}", e)))?;
        }

        let mut indexed_ids = Vec::new();

        // Reindex shared memories
        let shared_dir = paths::memories_dir(&self.project_dir);
        if shared_dir.exists() {
            self.reindex_dir(&shared_dir, Visibility::Shared, &mut indexed_ids)
                .await?;
        }

        // Reindex personal memories
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        if personal_dir.exists() {
            self.reindex_dir(&personal_dir, Visibility::Personal, &mut indexed_ids)
                .await?;
        }

        // Prune chunks for memories that no longer exist on disk, so the
        // preserved chunks table stays consistent with the rebuilt index.
        // Skipped under a checkout conflict: the other clone's memory files
        // are not visible from here, so its chunk rows would all look like
        // orphans and be deleted.
        if foreign_checkout.is_none() {
            let indexed_set: std::collections::HashSet<&str> =
                indexed_ids.iter().map(|s| s.as_str()).collect();
            let chunk_ids = self
                .lance_index
                .list_chunk_memory_ids()
                .await
                .map_err(|e| {
                    StorageError::Validation(format!("LanceDB list_chunk_memory_ids failed: {}", e))
                })?;
            for chunk_id in chunk_ids {
                if !indexed_set.contains(chunk_id.as_str()) {
                    self.lance_index
                        .delete_chunks(&chunk_id)
                        .await
                        .map_err(|e| {
                            StorageError::Validation(format!("LanceDB delete_chunks failed: {}", e))
                        })?;
                }
            }
        }

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(indexed_ids.len())
    }

    /// Drop and recreate the chunks (vectors) table.
    ///
    /// Destroys all embedding vectors and recreates the table with the
    /// currently configured dimensions. Only call this when a full re-embed
    /// is about to follow (i.e. an embedding provider is confirmed
    /// available) — see [`Self::reindex`] for the vector-preserving path.
    pub async fn clear_chunks(&self) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .clear_chunks()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB clear_chunks failed: {}", e)))
    }

    /// Upsert embedding chunks for a memory.
    pub async fn upsert_chunks(&self, memory_id: &str, chunks: Vec<Vec<f32>>) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .upsert_chunks(memory_id, chunks)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert_chunks failed: {}", e)))
    }

    /// Delete all embedding chunks for a memory.
    pub async fn delete_chunks(&self, memory_id: &str) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .delete_chunks(memory_id)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB delete_chunks failed: {}", e)))
    }

    /// List all distinct memory_ids present in the chunks table.
    pub async fn list_chunk_memory_ids(&self) -> Result<Vec<String>> {
        self.lance_index.list_chunk_memory_ids().await.map_err(|e| {
            StorageError::Validation(format!("LanceDB list_chunk_memory_ids failed: {}", e))
        })
    }

    /// Read every embedding chunk for `memory_id`, ordered by chunk index.
    ///
    /// Empty when the memory was never embedded. Used to relocate vectors
    /// during worktree consolidation so migrated memories stay searchable.
    pub async fn export_chunks(&self, memory_id: &str) -> Result<Vec<Vec<f32>>> {
        self.lance_index
            .chunks_for_memory(memory_id)
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB chunks_for_memory failed: {}", e))
            })
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

    /// Delete just the .md file(s) from a directory (does not touch LanceDB).
    ///
    /// All files matching the ID are removed: `find_memory_files` guarantees
    /// they share one full ID, so any extra file is a stale duplicate left by
    /// a crashed rename-update — deleting the memory sweeps it up too.
    async fn delete_file_from_dir(&self, id: &str, dir: &Path) -> Result<()> {
        let matches = find_memory_files(dir, id).await?;

        if matches.is_empty() {
            return Err(StorageError::NotFound(id.to_string()));
        }
        for path in &matches {
            match async_fs::remove_file(path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Resolve a prefix ID to a full ID using a LanceDB WHERE filter.
    async fn resolve_full_id(&self, id: &str) -> Result<String> {
        let matches = self.lance_index.find_ids_by_prefix(id).await.map_err(|e| {
            StorageError::Validation(format!("LanceDB prefix search failed: {}", e))
        })?;

        match matches.len() {
            0 => Err(StorageError::NotFound(id.to_string())),
            1 => Ok(matches.into_iter().next().unwrap()),
            _ => Err(StorageError::Validation(format!(
                "Ambiguous ID prefix '{}': matches {} memories",
                id,
                matches.len()
            ))),
        }
    }

    /// Reindex all .md files in a directory with a given visibility,
    /// appending the IDs of successfully indexed memories to `indexed_ids`.
    ///
    /// Skips files that cannot be read or parsed, logging a warning for each,
    /// so that a single corrupted file does not abort the entire reindex.
    ///
    /// When two files share one memory ID (a stale duplicate left by an
    /// update that crashed between writing the renamed file and removing the
    /// old one), the newest file wins and the stale one is deleted — reindex
    /// runs under the project write lock and is the documented repair path.
    async fn reindex_dir(
        &self,
        dir: &Path,
        visibility: Visibility,
        indexed_ids: &mut Vec<String>,
    ) -> Result<()> {
        let mut by_id: HashMap<String, (PathBuf, std::time::SystemTime, Memory)> = HashMap::new();

        let mut entries = async_fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                let content = match async_fs::read_to_string(&path).await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("Skipping {}: failed to read: {}", path.display(), e);
                        continue;
                    }
                };
                let memory = match memory_file::parse_memory_file(&content) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!("Skipping {}: failed to parse: {}", path.display(), e);
                        continue;
                    }
                };
                let mtime = file_mtime(&path).await;
                let stale = match by_id.entry(memory.id.clone()) {
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert((path, mtime, memory));
                        None
                    }
                    std::collections::hash_map::Entry::Occupied(mut slot) => {
                        if mtime >= slot.get().1 {
                            let (old_path, _, _) = slot.insert((path, mtime, memory));
                            Some(old_path)
                        } else {
                            Some(path)
                        }
                    }
                };
                if let Some(stale_path) = stale {
                    tracing::warn!(
                        "Removing stale duplicate file {} (same memory ID, older mtime)",
                        stale_path.display()
                    );
                    if let Err(e) = async_fs::remove_file(&stale_path).await {
                        tracing::warn!(
                            "Failed to remove stale duplicate {}: {}",
                            stale_path.display(),
                            e
                        );
                    }
                }
            }
        }

        for (id, (_path, _mtime, memory)) in by_id {
            let mut index_entry = IndexEntry::from(&memory);
            index_entry.visibility = visibility;
            self.lance_index
                .upsert(&index_entry)
                .await
                .map_err(|e| StorageError::Validation(format!("LanceDB upsert failed: {}", e)))?;
            indexed_ids.push(id);
        }
        Ok(())
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

    /// Recompute and persist manifest stats (load-modify-save of
    /// `manifest.toml`).
    ///
    /// Callers MUST hold the per-project write lock — every call site
    /// (`create`, `write_updated_locked`, `delete`, `reindex`) already runs
    /// inside it. Calling this unlocked could race another manifest writer
    /// (e.g. [`Self::set_embedding_fingerprint`]) and clobber its fields.
    async fn update_manifest_stats(&self) -> Result<()> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let mut manifest = manifest::load_manifest(&manifest_path).await?;

        let summaries = self.list_summary().await?;
        let memory_count = summaries.len();
        let logical_scopes: Vec<String> = summaries
            .iter()
            .flat_map(|e| e.logical.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        manifest::update_stats(&mut manifest, memory_count, logical_scopes);
        manifest::save_manifest(&manifest_path, &manifest).await?;

        Ok(())
    }

    /// Detect whether a different, still-existing checkout owns this
    /// project ID.
    ///
    /// Two independent clones of the same git remote hash to the same
    /// project ID (deliberately — the ID is stable across moves/re-clones)
    /// and therefore share one LanceDB index, write lock, and personal
    /// memories dir, while each keeps its own `.engramdb/memories/` files.
    /// The global registry records which checkout registered the ID first;
    /// when that path is a different, still-existing directory, destructive
    /// index operations from this checkout would corrupt the other one's
    /// data (see [`Self::reindex`]).
    ///
    /// Returns the registered checkout's canonicalized path, or `None` when
    /// this checkout is the registered owner, the registered path no longer
    /// exists, this is the global store, or the registry is unavailable.
    /// Linked git worktrees of the registered checkout never count as a
    /// conflict.
    pub async fn checkout_conflict(&self) -> Option<PathBuf> {
        if self.is_global() {
            return None;
        }
        let registry = super::registry::FileRegistry::global().ok()?;
        let reg = registry.load().await.ok()?;
        super::registry::conflicting_checkout_path(&reg, &self.project_id, &self.project_dir)
    }

    /// Read the persisted embedding-model fingerprint, if any. `None` on
    /// legacy stores created before model tracking (treated as untracked).
    pub async fn embedding_fingerprint(&self) -> Result<Option<manifest::EmbeddingFingerprint>> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let manifest = manifest::load_manifest(&manifest_path).await?;
        Ok(manifest.embedding)
    }

    /// Stamp the store with the embedding-model fingerprint its vectors
    /// were produced with. Called after a successful full (re)embed.
    ///
    /// Acquires the per-project write lock: this is a load-modify-save of
    /// `manifest.toml`, racing `update_manifest_stats` (which every mutating
    /// op runs under the lock). Without the lock, a concurrent `create`
    /// could load the pre-stamp manifest and save over the fingerprint,
    /// silently flipping the store back to Untracked right after a
    /// successful reindex.
    ///
    /// Callers must NOT already hold the project write lock: `flock(2)` is
    /// per open-file-description, so re-acquiring on a second fd in the same
    /// process blocks forever. All call sites (e.g. `ops::reindex`) invoke
    /// this after `store.reindex()` has returned and released the lock.
    pub async fn set_embedding_fingerprint(
        &self,
        fingerprint: manifest::EmbeddingFingerprint,
    ) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let mut manifest = manifest::load_manifest(&manifest_path).await?;
        manifest.embedding = Some(fingerprint);
        manifest::save_manifest(&manifest_path, &manifest).await?;
        Ok(())
    }
}

/// Write content atomically and durably using write-to-temp-then-rename.
///
/// Creates a temp file in the same directory, writes and **fsyncs** it, then
/// persists (renames) to the target path.  `rename(2)` is atomic on
/// APFS/ext4, eliminating partial-read windows, and the fsync-before-rename
/// ensures the rename can never survive a power loss without the data (which
/// would leave a zero-length or partial file).  On Unix the parent directory
/// is fsynced afterwards so the rename itself is durable too.  The temp file
/// is auto-cleaned on error.
///
/// The blocking syscalls (write/fsync/rename) run on the blocking thread
/// pool so the async executor is never stalled.
pub(crate) async fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let path = path.to_path_buf();
    let content = content.to_owned();
    tokio::task::spawn_blocking(move || -> Result<()> {
        use std::io::Write;

        let parent = path.parent().ok_or_else(|| {
            StorageError::Validation("atomic_write target has no parent directory".into())
        })?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        tmp.as_file_mut().write_all(content.as_bytes())?;
        // fsync BEFORE rename: otherwise the rename can be persisted without
        // the file contents.
        tmp.as_file().sync_all()?;
        tmp.persist(&path)?;
        // fsync the directory so the rename (the file's existence under its
        // final name) is durable. Directories can't be opened for writing on
        // Windows, so this is Unix-only; the data itself is already synced.
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })
    .await
    .map_err(|e| StorageError::Validation(format!("atomic_write task failed: {}", e)))?
}

/// Locate every file in `dir` whose stem matches the given ID (or ID prefix).
///
/// Multiple files may legitimately share one full ID: a rename-update (title
/// or visibility change) writes the new file before removing the old one, so
/// a crash in between leaves a stale duplicate. All files for the SAME full
/// ID are returned (callers resolve by mtime or clean them all up); matches
/// spanning DISTINCT full IDs are a true ambiguous-prefix error.
///
/// Returns an empty Vec when the directory does not exist.
async fn find_memory_files(dir: &Path, id: &str) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut matches = Vec::new();
    let mut distinct_ids = HashSet::new();
    let mut entries = async_fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            if memory_file::stem_matches_id_prefix(stem, id) {
                distinct_ids.insert(memory_file::extract_id_from_stem(stem).to_string());
                matches.push(path);
            }
        }
    }

    if distinct_ids.len() > 1 {
        return Err(StorageError::Validation(format!(
            "Ambiguous ID prefix '{}': matches {} memories",
            id,
            distinct_ids.len()
        )));
    }
    Ok(matches)
}

/// Modification time of a file; `UNIX_EPOCH` when unavailable, so a file we
/// cannot stat loses any newest-wins comparison.
async fn file_mtime(path: &Path) -> std::time::SystemTime {
    match async_fs::metadata(path).await {
        Ok(meta) => meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
        Err(_) => std::time::SystemTime::UNIX_EPOCH,
    }
}

/// Pick the most recently modified path. `paths` must be non-empty.
async fn newest_by_mtime(paths: &[PathBuf]) -> &PathBuf {
    let mut best = &paths[0];
    let mut best_mtime = file_mtime(best).await;
    for path in &paths[1..] {
        let mtime = file_mtime(path).await;
        if mtime >= best_mtime {
            best = path;
            best_mtime = mtime;
        }
    }
    best
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

/// Scan a directory and build a `HashMap` mapping memory ID → path
/// for all `.md` files.  Handles both old (`<uuid>.md`) and new
/// (`<slug>_<uuid>.md`) filename formats.
/// Returns an empty map if the directory does not exist.
async fn scan_dir_to_map(dir: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    if !dir.exists() {
        return map;
    }
    let Ok(mut entries) = async_fs::read_dir(dir).await else {
        return map;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let id = memory_file::extract_id_from_stem(stem).to_string();
                match map.get(&id) {
                    // Stale duplicate from a crashed rename-update: keep the
                    // newest file (the rename path writes the new file last).
                    Some(existing) => {
                        if file_mtime(&path).await >= file_mtime(existing).await {
                            map.insert(id, path);
                        }
                    }
                    None => {
                        map.insert(id, path);
                    }
                }
            }
        }
    }
    map
}

/// Collect memory IDs from `.md` filenames in a directory into a `HashSet`.
/// Handles both old (`<uuid>.md`) and new (`<slug>_<uuid>.md`) formats.
async fn collect_stems(dir: &Path, stems: &mut HashSet<String>) {
    if !dir.exists() {
        return;
    }
    let Ok(mut entries) = async_fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let id = memory_file::extract_id_from_stem(stem);
                stems.insert(id.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::InMemoryRegistry;
    use engram_types::{Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    /// `atomic_write` must replace an existing file in place. On Unix this is
    /// `rename(2)`; on Windows `tempfile::persist` uses `MoveFileEx`-with-
    /// replace. Guard that overwriting works on every platform (and leaves no
    /// stray temp file behind).
    #[tokio::test]
    async fn atomic_write_overwrites_existing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("data.md");

        atomic_write(&path, "first").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "first");

        atomic_write(&path, "second").await.unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), "second");

        // Only the target file remains — the temp file was consumed by persist.
        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("data.md")]);
    }

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

    /// Re-running `init` on an already-initialized project must not clobber
    /// a user-customized config.toml and must not drop the embedding
    /// fingerprint from the manifest (data-loss regression: `init` used to
    /// unconditionally rewrite both files).
    #[tokio::test]
    async fn test_reinit_preserves_config_and_embedding_fingerprint() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let registry = InMemoryRegistry::new();

        let store = MemoryStore::init(project_dir, &registry).await.unwrap();

        // Customize the config (valid TOML so re-init's load_config parses it).
        let config_path = project_dir.join(".engramdb/config.toml");
        let custom_config = "# user-customized configuration\n[nli]\nenabled = false\n";
        async_fs::write(&config_path, custom_config).await.unwrap();

        // Stamp an embedding fingerprint, as a successful (re)embed would.
        let fingerprint = manifest::EmbeddingFingerprint {
            model: "onnx/all-MiniLM-L6-v2-q".to_string(),
            dimensions: 384,
        };
        store
            .set_embedding_fingerprint(fingerprint.clone())
            .await
            .unwrap();

        // Re-running init must preserve both files byte-for-byte semantics.
        MemoryStore::init(project_dir, &registry).await.unwrap();

        let config_after = async_fs::read_to_string(&config_path).await.unwrap();
        assert_eq!(
            config_after, custom_config,
            "re-init clobbered the user's config.toml"
        );

        let manifest_path = project_dir.join(".engramdb/manifest.toml");
        let manifest_after = manifest::load_manifest(&manifest_path).await.unwrap();
        assert_eq!(
            manifest_after.embedding,
            Some(fingerprint),
            "re-init dropped the embedding fingerprint (store flipped to Untracked)"
        );
    }

    /// Re-running `init` on a store with existing memories must keep the
    /// memories readable and preserve manifest stats / created_at.
    #[tokio::test]
    async fn test_reinit_preserves_memories_and_manifest_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();
        let registry = InMemoryRegistry::new();

        let store = MemoryStore::init(project_dir, &registry).await.unwrap();
        let memory = create_test_memory("reinit-keep-123", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let manifest_path = project_dir.join(".engramdb/manifest.toml");
        let before = manifest::load_manifest(&manifest_path).await.unwrap();
        assert_eq!(before.stats.memory_count, 1);

        let store2 = MemoryStore::init(project_dir, &registry).await.unwrap();

        // Memory is still readable through the re-initialized store.
        let retrieved = store2.get("reinit-keep-123").await.unwrap();
        assert_eq!(retrieved.summary, "Test summary");

        let after = manifest::load_manifest(&manifest_path).await.unwrap();
        assert_eq!(
            after.created_at, before.created_at,
            "re-init reset manifest created_at"
        );
        assert_eq!(
            after.stats.memory_count, before.stats.memory_count,
            "re-init reset manifest stats"
        );
        assert_eq!(after.project, before.project);
    }

    #[tokio::test]
    async fn test_open_uninitialized() {
        let temp_dir = TempDir::new().unwrap();
        let project_dir = temp_dir.path();

        let result = MemoryStore::open(project_dir).await;
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

    /// Set a file's mtime `secs` seconds into the past, so newest-wins
    /// duplicate resolution has an unambiguous ordering.
    fn age_file(path: &Path, secs: u64) {
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(secs))
            .unwrap();
    }

    /// All files in `dir` whose stem matches `id` (same matching rule the
    /// store uses).
    fn files_matching_id(dir: &Path, id: &str) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| {
                let path = e.unwrap().path();
                let stem = path.file_stem()?.to_str()?.to_owned();
                memory_file::stem_matches_id_prefix(&stem, id).then_some(path)
            })
            .collect()
    }

    /// Issue-1 contract (a): an update that does not change the filename must
    /// replace the file in place — the old path still exists afterwards and
    /// no second file appears (the old delete-then-write order made the file
    /// briefly absent, so racing lock-free readers saw spurious NotFound).
    #[tokio::test]
    async fn test_update_same_filename_replaces_in_place() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("inplace-update-123", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let dir = paths::memories_dir(&store.project_dir);
        let path = dir.join("inplace-update-123.md");
        assert!(path.exists(), "file must exist after create");

        let mut update = MemoryUpdate::new();
        update.content = Some("New content".to_string());
        store.update("inplace-update-123", update).await.unwrap();

        assert!(
            path.exists(),
            "same-filename update must keep the file at its original path"
        );
        assert_eq!(
            files_matching_id(&dir, "inplace-update-123").len(),
            1,
            "exactly one file for the id"
        );
        let retrieved = store.get("inplace-update-123").await.unwrap();
        assert_eq!(retrieved.content, "New content");
    }

    /// Issue-1 contract (b): a title-changing update renames the file —
    /// afterwards exactly one file exists for the id and `get` returns the
    /// new state.
    #[tokio::test]
    async fn test_update_title_change_leaves_single_file() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let id = "aaaa1111-2222-3333-4444-555566667777";
        let mut memory = create_test_memory(id, Visibility::Shared);
        memory.title = Some("First Title".to_string());
        store.create(&memory).await.unwrap();

        let dir = paths::memories_dir(&store.project_dir);
        let old_path = dir.join(format!("first-title_{}.md", id));
        assert!(old_path.exists(), "slugged filename expected after create");

        let mut update = MemoryUpdate::new();
        update.title = Some("Second Title".to_string());
        update.content = Some("Renamed content".to_string());
        store.update(id, update).await.unwrap();

        assert!(!old_path.exists(), "old slug file must be removed");
        let new_path = dir.join(format!("second-title_{}.md", id));
        assert!(new_path.exists(), "new slug file must exist");
        assert_eq!(
            files_matching_id(&dir, id).len(),
            1,
            "exactly one file for the id after a rename-update"
        );

        let retrieved = store.get(id).await.unwrap();
        assert_eq!(retrieved.title.as_deref(), Some("Second Title"));
        assert_eq!(retrieved.content, "Renamed content");
    }

    /// Issue-1 contract (c): simulate the crash window of a rename-update —
    /// BOTH the new and the stale old file exist for one id. Readers must
    /// resolve to the newest file (not error as "ambiguous"), and `delete`
    /// must sweep both files.
    #[tokio::test]
    async fn test_stale_duplicate_resolves_to_newest_and_delete_sweeps_both() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let id = "cccc1111-2222-3333-4444-555566667777";
        let mut memory = create_test_memory(id, Visibility::Shared);
        memory.title = Some("New Title".to_string());
        memory.content = "Current content".to_string();
        store.create(&memory).await.unwrap();

        // Manually create the stale pre-rename file (old title, old content)
        // with an unambiguously older mtime.
        let dir = paths::memories_dir(&store.project_dir);
        let mut stale = memory.clone();
        stale.title = Some("Old Title".to_string());
        stale.content = "Stale content".to_string();
        let stale_path = dir.join(memory_file::memory_filename(&stale));
        std::fs::write(&stale_path, memory_file::write_memory_file(&stale).unwrap()).unwrap();
        age_file(&stale_path, 3600);
        assert_eq!(files_matching_id(&dir, id).len(), 2);

        // get resolves to the newest file instead of failing as ambiguous.
        let got = store.get(id).await.unwrap();
        assert_eq!(got.content, "Current content");

        // get_batch resolves the duplicate the same way.
        let batch = store.get_batch(&[id]).await.unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].1.content, "Current content");

        // delete removes BOTH files — no orphan left behind.
        store.delete(id).await.unwrap();
        assert!(files_matching_id(&dir, id).is_empty());
        assert!(store.get(id).await.is_err());
    }

    /// A subsequent update also cleans up a stale duplicate left by a
    /// crashed rename-update.
    #[tokio::test]
    async fn test_update_cleans_up_stale_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let id = "dddd1111-2222-3333-4444-555566667777";
        let mut memory = create_test_memory(id, Visibility::Shared);
        memory.title = Some("New Title".to_string());
        store.create(&memory).await.unwrap();

        let dir = paths::memories_dir(&store.project_dir);
        let mut stale = memory.clone();
        stale.title = Some("Old Title".to_string());
        let stale_path = dir.join(memory_file::memory_filename(&stale));
        std::fs::write(&stale_path, memory_file::write_memory_file(&stale).unwrap()).unwrap();
        age_file(&stale_path, 3600);
        assert_eq!(files_matching_id(&dir, id).len(), 2);

        let mut update = MemoryUpdate::new();
        update.content = Some("Post-crash content".to_string());
        store.update(id, update).await.unwrap();

        assert_eq!(
            files_matching_id(&dir, id).len(),
            1,
            "update must sweep the stale duplicate"
        );
        assert!(!stale_path.exists());
        let got = store.get(id).await.unwrap();
        assert_eq!(got.content, "Post-crash content");
    }

    /// Reindex with a stale duplicate on disk: the newest file wins (its
    /// content is what gets indexed), the duplicate counts once, and the
    /// stale file is removed (reindex is the repair path).
    #[tokio::test]
    async fn test_reindex_dedupes_and_removes_stale_duplicate() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let id = "eeee1111-2222-3333-4444-555566667777";
        let mut memory = create_test_memory(id, Visibility::Shared);
        memory.title = Some("New Title".to_string());
        memory.summary = "Current summary".to_string();
        store.create(&memory).await.unwrap();

        let dir = paths::memories_dir(&store.project_dir);
        let mut stale = memory.clone();
        stale.title = Some("Old Title".to_string());
        stale.summary = "Stale summary".to_string();
        let stale_path = dir.join(memory_file::memory_filename(&stale));
        std::fs::write(&stale_path, memory_file::write_memory_file(&stale).unwrap()).unwrap();
        age_file(&stale_path, 3600);

        let count = store.reindex().await.unwrap();
        assert_eq!(count, 1, "duplicate files for one id must index once");
        assert!(
            !stale_path.exists(),
            "reindex must remove the stale duplicate"
        );
        assert_eq!(files_matching_id(&dir, id).len(), 1);

        let got = store.get(id).await.unwrap();
        assert_eq!(got.summary, "Current summary");
        assert_eq!(store.list_ids().await.unwrap(), vec![id.to_string()]);
    }

    /// Issue-3 regression: `set_embedding_fingerprint` racing concurrent
    /// `create`s must not lose the fingerprint. Both paths do a
    /// load-modify-save of manifest.toml; without the write lock a create
    /// could load the pre-stamp manifest and save over the fingerprint.
    #[tokio::test]
    async fn test_set_embedding_fingerprint_survives_concurrent_creates() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let fingerprint = manifest::EmbeddingFingerprint {
            model: "onnx/race-test-model".to_string(),
            dimensions: 384,
        };

        let mut tasks = Vec::new();
        for i in 0..8 {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                let mem = create_test_memory(&format!("fp-race-{}", i), Visibility::Shared);
                store.create(&mem).await.unwrap();
            }));
        }
        let stamp = {
            let store = store.clone();
            let fingerprint = fingerprint.clone();
            tokio::spawn(async move {
                store.set_embedding_fingerprint(fingerprint).await.unwrap();
            })
        };

        for task in tasks {
            task.await.unwrap();
        }
        stamp.await.unwrap();

        assert_eq!(
            store.embedding_fingerprint().await.unwrap(),
            Some(fingerprint),
            "a concurrent create clobbered the fingerprint (manifest RMW race)"
        );
        assert_eq!(store.count().await.unwrap(), 8, "all creates must land");
    }

    /// The core atomicity property of `update_with`: N concurrent
    /// read-modify-write updates must all survive. Each task appends one
    /// distinct tag inside the closure; because the memory is re-read under
    /// the per-project write lock, no task can snapshot stale state and
    /// overwrite another task's tag. (The old unlocked get-merge-update flow
    /// would lose tags here.)
    #[tokio::test]
    async fn test_update_with_serializes_concurrent_updates() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("test-update-with-race", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let tasks: Vec<_> = (0..8)
            .map(|i| {
                let store = store.clone();
                tokio::spawn(async move {
                    store
                        .update_with("test-update-with-race", move |m| {
                            m.tags.push(format!("tag-{}", i));
                            Ok(())
                        })
                        .await
                        .unwrap();
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }

        let final_memory = store.get("test-update-with-race").await.unwrap();
        for i in 0..8 {
            assert!(
                final_memory.tags.contains(&format!("tag-{}", i)),
                "tag-{} was lost; tags = {:?} — update_with did not serialize",
                i,
                final_memory.tags
            );
        }
        assert_eq!(final_memory.tags.len(), 8);
    }

    /// `update_with` returns the memory exactly as persisted (closure applied,
    /// `updated_at` bumped), and the same state is readable back from disk.
    #[tokio::test]
    async fn test_update_with_returns_persisted_memory() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("test-update-with-ret", Visibility::Shared);
        let created_at = memory.updated_at;
        store.create(&memory).await.unwrap();

        let returned = store
            .update_with("test-update-with-ret", |m| {
                m.summary = "Closure summary".to_string();
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(returned.summary, "Closure summary");
        assert!(returned.updated_at >= created_at);

        let reloaded = store.get("test-update-with-ret").await.unwrap();
        assert_eq!(reloaded.summary, "Closure summary");
        assert_eq!(reloaded.updated_at, returned.updated_at);
    }

    /// A failing closure must abort the update: nothing is written and the
    /// error surfaces to the caller.
    #[tokio::test]
    async fn test_update_with_closure_error_writes_nothing() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("test-update-with-err", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let result = store
            .update_with("test-update-with-err", |m| {
                m.summary = "Should not persist".to_string();
                anyhow::bail!("merge failed")
            })
            .await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("merge failed"));

        let reloaded = store.get("test-update-with-err").await.unwrap();
        assert_eq!(reloaded.summary, "Test summary");
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

        let ids = store.list_ids().await.unwrap();
        assert_eq!(ids.len(), 3);

        assert!(ids.contains(&"list-test-1".to_string()));
        assert!(ids.contains(&"list-test-2".to_string()));
        assert!(ids.contains(&"list-test-3".to_string()));
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

        assert_eq!(store.count().await.unwrap(), 2);

        // Clear LanceDB to simulate corruption
        store.lance_index.clear().await.unwrap();
        assert_eq!(store.count().await.unwrap(), 0);

        // Reindex
        let count = store.reindex().await.unwrap();
        assert_eq!(count, 2);

        let ids = store.list_ids().await.unwrap();
        assert_eq!(ids.len(), 2);

        assert!(ids.contains(&"reindex-test-1".to_string()));
        assert!(ids.contains(&"reindex-test-2".to_string()));
    }

    /// Regression test for the reindex data-loss bug: rebuilding metadata
    /// from the markdown files must NOT destroy existing embedding vectors.
    /// Chunks for memories that no longer exist on disk are pruned, keeping
    /// the chunks table consistent without losing live vectors.
    #[tokio::test]
    async fn test_reindex_preserves_chunks_and_prunes_orphans() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let memory = create_test_memory("reindex-chunks-live", Visibility::Shared);
        store.create(&memory).await.unwrap();
        store
            .upsert_chunks(
                "reindex-chunks-live",
                vec![vec![0.25f32; 384], vec![0.5f32; 384]],
            )
            .await
            .unwrap();

        // Chunks for a memory with no file on disk (deleted out-of-band).
        store
            .upsert_chunks("reindex-chunks-ghost", vec![vec![0.75f32; 384]])
            .await
            .unwrap();

        let count = store.reindex().await.unwrap();
        assert_eq!(count, 1);

        // The live memory's vectors must survive a metadata-only reindex.
        let chunks = store.export_chunks("reindex-chunks-live").await.unwrap();
        assert_eq!(chunks.len(), 2, "vectors must survive reindex");
        assert_eq!(chunks[0], vec![0.25f32; 384]);

        // Orphaned chunks must be pruned so the table stays consistent.
        assert_eq!(
            store.list_chunk_memory_ids().await.unwrap(),
            vec!["reindex-chunks-live".to_string()],
            "ghost chunks must be pruned, live chunks kept"
        );
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

        let entries = store.list_filterable().await.unwrap();
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

    // --- get_batch tests ---

    #[tokio::test]
    async fn test_get_batch_returns_all() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let mut ids = Vec::new();
        for i in 0..5 {
            let mem = Memory::new(
                MemoryType::Decision,
                format!("Summary {}", i),
                format!("Content {}", i),
                Provenance::human(),
            );
            ids.push(store.create(&mem).await.unwrap());
        }

        let results = store.get_batch(&ids).await.unwrap();
        assert_eq!(results.len(), 5);
        for (id, _mem) in &results {
            assert!(ids.contains(id));
        }
    }

    #[tokio::test]
    async fn test_get_batch_skips_missing() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let mut ids = Vec::new();
        for i in 0..3 {
            let mem = Memory::new(
                MemoryType::Decision,
                format!("Summary {}", i),
                format!("Content {}", i),
                Provenance::human(),
            );
            ids.push(store.create(&mem).await.unwrap());
        }
        ids.push("fake-id-1".to_string());
        ids.push("fake-id-2".to_string());

        let results = store.get_batch(&ids).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    // --- batch_exists tests ---

    #[tokio::test]
    async fn test_batch_exists_all_present() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let mut ids = Vec::new();
        for i in 0..5 {
            let mem = Memory::new(
                MemoryType::Decision,
                format!("Summary {}", i),
                format!("Content {}", i),
                Provenance::human(),
            );
            ids.push(store.create(&mem).await.unwrap());
        }

        let existing = store.batch_exists(&ids).await.unwrap();
        assert_eq!(existing.len(), 5);
        for id in &ids {
            assert!(existing.contains(id));
        }
    }

    #[tokio::test]
    async fn test_batch_exists_some_missing() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let mut ids = Vec::new();
        for i in 0..3 {
            let mem = Memory::new(
                MemoryType::Decision,
                format!("Summary {}", i),
                format!("Content {}", i),
                Provenance::human(),
            );
            ids.push(store.create(&mem).await.unwrap());
        }
        ids.push("fake-id-1".to_string());
        ids.push("fake-id-2".to_string());

        let existing = store.batch_exists(&ids).await.unwrap();
        assert_eq!(existing.len(), 3);
        assert!(!existing.contains("fake-id-1"));
        assert!(!existing.contains("fake-id-2"));
    }

    // --- Second-clone (shared project ID) guard tests ---

    /// Create a fake git clone with a fixed remote URL so two directories
    /// compute the same (remote-derived) project ID. Each test uses a
    /// distinct `remote` so per-test global state never collides.
    fn make_clone(root: &Path, name: &str, remote: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join(".git").join("config"),
            format!(
                "[remote \"origin\"]\n\turl = https://github.com/example/{}.git\n",
                remote
            ),
        )
        .unwrap();
        dir
    }

    #[tokio::test]
    async fn test_second_clone_detects_checkout_conflict() {
        let tmp = TempDir::new().unwrap();
        let a = make_clone(tmp.path(), "clone-a", "conflict-detect");
        let b = make_clone(tmp.path(), "clone-b", "conflict-detect");
        // `checkout_conflict` consults the global file registry (redirected to
        // a per-process temp dir by the test-isolation arm), so init through
        // it rather than an InMemoryRegistry.
        let registry = crate::registry::FileRegistry::global().unwrap();

        let store_a = MemoryStore::init(&a, &registry).await.unwrap();
        let store_b = MemoryStore::init(&b, &registry).await.unwrap();
        assert_eq!(
            store_a.project_id, store_b.project_id,
            "clones of one remote share a project ID by design"
        );

        assert_eq!(
            store_a.checkout_conflict().await,
            None,
            "the registered owner sees no conflict"
        );
        assert_eq!(
            store_b.checkout_conflict().await,
            Some(a.canonicalize().unwrap()),
            "the second clone must detect the registered checkout"
        );

        // The conflict is equally detectable from a plain `open`.
        let reopened_b = MemoryStore::open(&b).await.unwrap();
        assert_eq!(
            reopened_b.checkout_conflict().await,
            Some(a.canonicalize().unwrap())
        );
    }

    #[tokio::test]
    async fn test_second_clone_init_does_not_steal_registration_until_owner_gone() {
        let tmp = TempDir::new().unwrap();
        let a = make_clone(tmp.path(), "clone-a", "conflict-repoint");
        let b = make_clone(tmp.path(), "clone-b", "conflict-repoint");
        let registry = crate::registry::FileRegistry::global().unwrap();

        let store_a = MemoryStore::init(&a, &registry).await.unwrap();
        MemoryStore::init(&b, &registry).await.unwrap();

        let loaded = registry.load().await.unwrap();
        let entry = loaded
            .projects
            .iter()
            .find(|e| e.project_id == store_a.project_id)
            .expect("project registered");
        assert_eq!(
            PathBuf::from(&entry.project_path),
            a.canonicalize().unwrap(),
            "registry must keep the first checkout while it exists"
        );

        // The first checkout disappears (deleted / moved clone): the next
        // init from B legitimately takes over the registration.
        async_fs::remove_dir_all(&a).await.unwrap();
        let store_b = MemoryStore::init(&b, &registry).await.unwrap();

        let loaded = registry.load().await.unwrap();
        let entry = loaded
            .projects
            .iter()
            .find(|e| e.project_id == store_b.project_id)
            .expect("project registered");
        assert_eq!(
            PathBuf::from(&entry.project_path),
            b.canonicalize().unwrap(),
            "registry must self-heal once the old checkout is gone"
        );
        assert_eq!(store_b.checkout_conflict().await, None);
    }

    /// CRITICAL data-loss guard: a reindex run from the second clone shares
    /// the LanceDB index with the first clone but only sees its own memory
    /// files — it must NOT clear the other clone's index rows or prune its
    /// vectors as "orphans".
    #[tokio::test]
    async fn test_reindex_from_second_clone_preserves_other_checkouts_data() {
        let tmp = TempDir::new().unwrap();
        let a = make_clone(tmp.path(), "clone-a", "conflict-reindex");
        let b = make_clone(tmp.path(), "clone-b", "conflict-reindex");
        let registry = crate::registry::FileRegistry::global().unwrap();

        let store_a = MemoryStore::init(&a, &registry).await.unwrap();
        let store_b = MemoryStore::init(&b, &registry).await.unwrap();

        // Clone A's memory file lives only in A's tree; its index row and
        // vector live in the shared LanceDB table.
        let mem_a = create_test_memory("clone-a-mem", Visibility::Shared);
        store_a.create(&mem_a).await.unwrap();
        store_a
            .upsert_chunks("clone-a-mem", vec![vec![0.25f32; 384]])
            .await
            .unwrap();

        let mem_b = create_test_memory("clone-b-mem", Visibility::Shared);
        store_b.create(&mem_b).await.unwrap();

        let count = store_b.reindex().await.unwrap();
        assert_eq!(count, 1, "only this checkout's files are scanned");

        let ids = store_b.list_ids().await.unwrap();
        assert!(
            ids.contains(&"clone-a-mem".to_string()),
            "the other clone's index row must survive a guarded reindex"
        );
        assert!(ids.contains(&"clone-b-mem".to_string()));

        let chunks = store_b.export_chunks("clone-a-mem").await.unwrap();
        assert_eq!(
            chunks.len(),
            1,
            "the other clone's vectors must not be pruned as orphans"
        );
        assert_eq!(chunks[0], vec![0.25f32; 384]);
    }

    /// Control: once the conflicting checkout is gone (and the registry has
    /// self-healed), reindex is destructive again — stale rows and orphan
    /// chunks from the departed clone are cleaned up.
    #[tokio::test]
    async fn test_reindex_becomes_destructive_again_once_other_clone_gone() {
        let tmp = TempDir::new().unwrap();
        let a = make_clone(tmp.path(), "clone-a", "conflict-cleanup");
        let b = make_clone(tmp.path(), "clone-b", "conflict-cleanup");
        let registry = crate::registry::FileRegistry::global().unwrap();

        let store_a = MemoryStore::init(&a, &registry).await.unwrap();
        let store_b = MemoryStore::init(&b, &registry).await.unwrap();

        let mem_a = create_test_memory("clone-a-mem", Visibility::Shared);
        store_a.create(&mem_a).await.unwrap();
        store_a
            .upsert_chunks("clone-a-mem", vec![vec![0.25f32; 384]])
            .await
            .unwrap();
        let mem_b = create_test_memory("clone-b-mem", Visibility::Shared);
        store_b.create(&mem_b).await.unwrap();

        // Clone A disappears; re-init from B takes over the registration.
        async_fs::remove_dir_all(&a).await.unwrap();
        let store_b = MemoryStore::init(&b, &registry).await.unwrap();
        assert_eq!(store_b.checkout_conflict().await, None);

        let count = store_b.reindex().await.unwrap();
        assert_eq!(count, 1);

        let ids = store_b.list_ids().await.unwrap();
        assert_eq!(ids, vec!["clone-b-mem".to_string()]);
        assert_eq!(
            store_b.list_chunk_memory_ids().await.unwrap(),
            Vec::<String>::new(),
            "the departed clone's orphan chunks are pruned by a full reindex"
        );
    }

    // --- Global memory store tests ---

    /// Handle returned by [`setup_global_store`]. Holds the live
    /// `MemoryStore` alongside a process-wide lock guard that serializes
    /// global-store tests within a single `cargo test` process. Derefs to
    /// `MemoryStore` so existing call sites (`let store = ...;
    /// store.create(...)`) keep working without change.
    struct GlobalStoreHandle {
        store: MemoryStore,
        _lock: crate::test_support::GlobalTestLock,
    }

    impl std::ops::Deref for GlobalStoreHandle {
        type Target = MemoryStore;
        fn deref(&self) -> &MemoryStore {
            &self.store
        }
    }

    /// Initialize a fresh global store for testing.
    ///
    /// Acquires the shared global-test lock and wipes the on-disk global
    /// layout before each test, so concurrent `cargo test` runs don't race
    /// on LanceDB table creation or leak state across tests.
    async fn setup_global_store() -> GlobalStoreHandle {
        let lock = crate::test_support::acquire_global_test_lock().await;
        let store = MemoryStore::init_global().await.unwrap();
        GlobalStoreHandle { store, _lock: lock }
    }

    #[tokio::test]
    async fn test_global_init_creates_structure() {
        let store = setup_global_store().await;

        assert!(store.is_global());
        assert_eq!(store.project_id, paths::GLOBAL_PROJECT_ID);

        let global_dir = paths::global_store_dir().unwrap();
        assert!(global_dir.join(".engramdb").exists());
        assert!(global_dir.join(".engramdb/memories").exists());
        assert!(global_dir.join(".engramdb/manifest.toml").exists());
        assert!(global_dir.join(".engramdb/config.toml").exists());
        assert!(paths::global_lancedb_dir().unwrap().exists());
    }

    #[tokio::test]
    async fn test_global_open_auto_inits() {
        // open_global should auto-init if not present
        let store = setup_global_store().await;
        assert!(store.is_global());
        assert_eq!(store.project_id, paths::GLOBAL_PROJECT_ID);
    }

    #[tokio::test]
    async fn test_global_create_and_get() {
        let store = setup_global_store().await;
        let memory = create_test_memory("global-test-001", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let retrieved = store.get("global-test-001").await.unwrap();
        assert_eq!(retrieved.id, "global-test-001");
        assert_eq!(retrieved.summary, "Test summary");
    }

    #[tokio::test]
    async fn test_global_update() {
        let store = setup_global_store().await;
        let memory = create_test_memory("global-test-update", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let mut update = MemoryUpdate::new();
        update.summary = Some("Updated global summary".to_string());
        store.update("global-test-update", update).await.unwrap();

        let retrieved = store.get("global-test-update").await.unwrap();
        assert_eq!(retrieved.summary, "Updated global summary");
    }

    #[tokio::test]
    async fn test_global_delete() {
        let store = setup_global_store().await;
        let memory = create_test_memory("global-test-delete", Visibility::Shared);
        store.create(&memory).await.unwrap();
        assert!(store.get("global-test-delete").await.is_ok());

        store.delete("global-test-delete").await.unwrap();

        let result = store.get("global-test-delete").await;
        assert!(result.is_err());
        match result {
            Err(StorageError::NotFound(_)) => {}
            _ => panic!("Expected NotFound error after delete"),
        }
    }

    #[tokio::test]
    async fn test_global_reindex() {
        let store = setup_global_store().await;
        let memory = create_test_memory("global-test-reindex", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let count = store.reindex().await.unwrap();
        assert!(count >= 1);

        // Memory should still be accessible after reindex
        let retrieved = store.get("global-test-reindex").await.unwrap();
        assert_eq!(retrieved.id, "global-test-reindex");
    }

    #[tokio::test]
    async fn test_global_isolation_from_project() {
        // Global store should NOT contain project memories and vice versa.
        // Hold the global-test lock for the whole test; otherwise a parallel
        // global test can mutate `global_store`'s on-disk state mid-assertion.
        let _lock = crate::test_support::acquire_global_test_lock().await;
        let temp_dir = TempDir::new().unwrap();
        let project_store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let global_store = MemoryStore::init_global().await.unwrap();

        let project_mem = create_test_memory("project-only-mem", Visibility::Shared);
        project_store.create(&project_mem).await.unwrap();

        let global_mem = create_test_memory("global-only-mem", Visibility::Shared);
        global_store.create(&global_mem).await.unwrap();

        // Global store should NOT have the project memory
        assert!(global_store.get("project-only-mem").await.is_err());

        // Project store should NOT have the global memory
        assert!(project_store.get("global-only-mem").await.is_err());
    }

    #[tokio::test]
    async fn test_global_is_not_global_for_regular_store() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        assert!(!store.is_global());
    }
}
