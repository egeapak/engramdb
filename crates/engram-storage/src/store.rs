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
use crate::config::{load_config, load_config_or_default};
use chrono::{DateTime, Utc};
use engram_types::{Memory, MemoryType, MemoryUpdate, Visibility};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;

/// How many memory-file reads may be in flight at once.
///
/// Read latency is what serializing these costs, so overlapping them is worth
/// it; the bound is what keeps a 5,000-memory reindex from opening 5,000 file
/// descriptors at once.
const PARALLEL_READ_WIDTH: usize = 16;

/// Batch size at or above which [`parse_batch`] goes through rayon.
///
/// Parsing is ~20µs per memory, and entering the rayon pool costs a few µs of
/// its own, so the crossover is low — but the retrieval path calls `get_batch`
/// with a handful of survivor IDs on every query, and paying pool latency
/// there to save nothing would be a regression on the most common shape. 32 is
/// comfortably past the crossover in both directions (`parse/*` in
/// `benches/parallel_simd.rs` still shows ~2.6x at 100).
const PARALLEL_PARSE_MIN: usize = 32;

/// Parse a batch of already-read memory files, in parallel above
/// [`PARALLEL_PARSE_MIN`].
///
/// Frontmatter deserialization plus the V2 section split is pure CPU and
/// perfectly per-file — the shape rayon is for. Measured at ~20ms per 1,000
/// memories serially and ~6ms across four cores (`benches/parallel_simd.rs`,
/// `parse/*`).
///
/// Unparseable files are dropped with a warning rather than failing the batch,
/// matching what every caller did inline before.
///
/// Order follows the input, so a caller that ordered `ids` still gets its
/// order back — rayon's `collect` into a `Vec` is order-preserving.
fn parse_batch(read: Vec<(String, PathBuf, String)>) -> Vec<(String, Memory)> {
    use rayon::prelude::*;

    fn parse_one(id: String, path: &Path, content: &str) -> Option<(String, Memory)> {
        match memory_file::parse_memory_file(content) {
            Ok(memory) => Some((id, memory)),
            Err(e) => {
                tracing::warn!(
                    "skipping {} ({}): failed to parse: {}",
                    id,
                    path.display(),
                    e
                );
                None
            }
        }
    }

    if read.len() < PARALLEL_PARSE_MIN {
        return read
            .into_iter()
            .filter_map(|(id, path, content)| parse_one(id, &path, &content))
            .collect();
    }
    read.into_par_iter()
        .filter_map(|(id, path, content)| parse_one(id, &path, &content))
        .collect()
}

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

/// Keep `.engramdb/state/` out of version control.
///
/// `.engramdb/memories/` is meant to be committed — shared memories travel
/// with a `git clone`. `.engramdb/state/` is not: it holds per-machine session
/// bookkeeping (session→task mappings, the harvest ledger) whose contents are
/// session ids, task names, and local decisions. Committing those leaks local
/// working context into a shared repository and produces pointless diffs on
/// every checkout.
///
/// Written on `init` and best-effort: an existing file is never overwritten
/// (a user may have customized it), and a failure must not fail `init`.
async fn write_state_gitignore(engramdb_dir: &Path) {
    let path = engramdb_dir.join(".gitignore");
    if path.exists() {
        return;
    }
    let body = "# Per-machine session bookkeeping — not shared state.\n\
                # `memories/` is deliberately NOT ignored: it is meant to be committed.\n\
                state/\n";
    if let Err(e) = async_fs::write(&path, body).await {
        tracing::debug!("could not write .engramdb/.gitignore (non-fatal): {e}");
    }
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

        write_state_gitignore(&engramdb_dir).await;

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

        let store = Self {
            project_dir: dir.to_path_buf(),
            project_id,
            lance_index,
        };
        // Release the init lock before migrating: the schema migration runs a
        // reindex, which re-acquires the per-project write lock (a second
        // acquire in this process would deadlock).
        drop(_lock);
        store.migrate_schema_if_needed().await?;
        Ok(store)
    }

    /// Initialize the global memory store.
    ///
    /// The global store lives under `<global_data_dir>/global/` and mirrors
    /// a normal project layout (`.engramdb/memories/`, `manifest.toml`, etc.)
    /// so all `MemoryStore` methods work unchanged.
    pub async fn init_global() -> Result<Self> {
        let global_dir = paths::global_store_dir()?;
        let engramdb_dir = paths::project_dir(&global_dir);

        // Serialize lazy creation on the advisory write lock (as `init`/
        // `init_group` do): the everyone/global store is also created lazily on
        // first open, so concurrent first-writers must not race on table
        // creation.
        let _lock = write_lock::acquire_write_lock(paths::GLOBAL_PROJECT_ID).await?;

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

        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;

        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("Global LanceDB init failed: {}", e)))?;

        let store = Self {
            project_dir: global_dir,
            project_id,
            lance_index,
        };
        // Release before migrating (a reindex re-acquires this lock), mirroring
        // `init`.
        drop(_lock);
        store.migrate_schema_if_needed().await?;
        Ok(store)
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
        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;

        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("Global LanceDB open failed: {}", e)))?;

        let store = Self {
            project_dir: global_dir,
            project_id,
            lance_index,
        };
        store.migrate_schema_if_needed().await?;
        Ok(store)
    }

    /// Returns `true` if this store is the global memory store.
    pub fn is_global(&self) -> bool {
        self.project_id == paths::GLOBAL_PROJECT_ID
    }

    /// Initialize a named group memory store.
    ///
    /// A *group store* is the generalization of the global store: an ordinary
    /// machine-local store shared by a set of subscribed projects (see the
    /// multi-project-memories design). It lives under
    /// `<global_data_dir>/groups/<group_id>/` and mirrors a normal project
    /// layout so all `MemoryStore` methods work unchanged — this is a
    /// near-verbatim copy of [`init_global`](Self::init_global) rooted at the
    /// group paths, with `project_id = group_id` and a `group:<id>` manifest
    /// name so a group store is self-describing on disk.
    pub async fn init_group(group_id: &str) -> Result<Self> {
        let group_dir = paths::group_store_dir(group_id)?;
        let engramdb_dir = paths::project_dir(&group_dir);

        // Serialize lazy creation on the per-store advisory write lock, exactly
        // as `init` does. Group stores are created lazily on first open (from a
        // query fan-in or a `--group` write), so two concurrent multi-repo
        // sessions can race here on LanceDB table creation without it — the
        // loser would get a hard "table create failed". `acquire_write_lock`
        // creates its own lock dir, so there is no bootstrap-ordering problem.
        let _lock = write_lock::acquire_write_lock(group_id).await?;

        async_fs::create_dir_all(&engramdb_dir).await?;
        async_fs::create_dir_all(paths::memories_dir(&group_dir)).await?;

        // Create manifest
        let manifest_path = engramdb_dir.join("manifest.toml");
        if !manifest_path.exists() {
            let manifest = manifest::Manifest {
                project: format!("group:{group_id}"),
                ..Default::default()
            };
            manifest::save_manifest(&manifest_path, &manifest).await?;
        }

        // Create empty config if missing
        let config_path = engramdb_dir.join("config.toml");
        if !config_path.exists() {
            async_fs::write(
                &config_path,
                "# EngramDB group configuration\n# See documentation for available settings\n",
            )
            .await?;
        }

        let project_id = group_id.to_string();

        // Create group LanceDB directory
        let lance_path = paths::group_lancedb_dir(group_id)?;
        async_fs::create_dir_all(&lance_path).await?;

        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;

        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("Group LanceDB init failed: {}", e)))?;

        let store = Self {
            project_dir: group_dir,
            project_id,
            lance_index,
        };
        // Release before migrating (a reindex re-acquires this lock), mirroring
        // `init`.
        drop(_lock);
        store.migrate_schema_if_needed().await?;
        Ok(store)
    }

    /// Open a named group memory store, creating it if necessary.
    pub async fn open_group(group_id: &str) -> Result<Self> {
        let group_dir = paths::group_store_dir(group_id)?;
        let engramdb_dir = paths::project_dir(&group_dir);

        if !engramdb_dir.exists() {
            return Self::init_group(group_id).await;
        }

        let project_id = group_id.to_string();
        let lance_path = paths::group_lancedb_dir(group_id)?;
        async_fs::create_dir_all(&lance_path).await?;

        let config_path = engramdb_dir.join("config.toml");
        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;

        let lance_index = LanceIndex::new(&lance_path, config.embeddings.dimensions)
            .await
            .map_err(|e| StorageError::Validation(format!("Group LanceDB open failed: {}", e)))?;

        let store = Self {
            project_dir: group_dir,
            project_id,
            lance_index,
        };
        store.migrate_schema_if_needed().await?;
        Ok(store)
    }

    /// Returns `true` if this store is a named group memory store.
    pub fn is_group(&self) -> bool {
        paths::is_group_id(&self.project_id)
    }

    /// Open an existing EngramDB store.
    pub async fn open(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);

        if !engramdb_dir.exists() {
            return Err(StorageError::NotInitialized);
        }

        // Backfill on open, not just on init: every store created before the
        // `.gitignore` existed would otherwise keep `state/` tracked forever,
        // and nobody re-runs `init` on an existing project. `state/` now holds
        // the harvest ledger — session ids and free-text review notes — so
        // leaving it committable is exactly the leak the file exists to stop.
        // Cheap and idempotent: it returns immediately when the file is there.
        write_state_gitignore(&engramdb_dir).await;

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

        let store = Self {
            project_dir: dir.to_path_buf(),
            project_id,
            lance_index,
        };
        // Every open (the hot path for all CLI commands and the MCP server) must
        // migrate a store that predates the current schema, exactly like the
        // init/global paths — otherwise reads that project the new columns fail
        // and writes hit a schema mismatch on an un-upgraded table.
        store.migrate_schema_if_needed().await?;
        Ok(store)
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
        // Stamp the identity of exactly the bytes we are about to write — not a
        // re-serialization, and no re-read. `atomic_write` writes `content`
        // verbatim, so this IS the on-disk identity.
        let digest = crate::digest::FileDigest::of(content.as_bytes());
        atomic_write(&file_path, &content).await?;

        // Sweep up any pre-existing file(s) for this exact ID — in BOTH
        // visibility directories — that are not the file we just wrote. Without
        // this, `create` for an ID that already exists under a *different*
        // visibility (worktree consolidation re-runs, or a personal/shared
        // re-create) would orphan the old file on disk while the index row
        // simply flips, diverging disk from the index (finding #1). New-file-
        // first ordering (the `atomic_write` above) means a reader never sees a
        // spurious NotFound. NotFound on removal is benign (already gone).
        //
        // Gate the sweep on a cheap index probe: every store-managed file has
        // an index row (create/update upsert one synchronously), so a fresh
        // ID — the overwhelmingly common case, since IDs are new UUIDs —
        // skips both full directory scans. This is what kept bulk creates
        // (worktree consolidation, imports) from being O(n²) in dirents. A
        // crash-orphaned file with no index row is doctor's territory either
        // way — the sweep never saw files the index didn't know about
        // re-created under colliding fresh UUIDs in practice.
        let id_preexisting = !self
            .lance_index
            .find_ids_by_prefix(&memory.id)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB id probe failed: {}", e)))?
            .is_empty();
        if id_preexisting {
            for dir in [
                self.get_memories_dir(&Visibility::Shared)?,
                self.get_memories_dir(&Visibility::Personal)?,
            ] {
                for old in find_memory_files(&dir, &memory.id).await? {
                    if old != file_path {
                        match async_fs::remove_file(&old).await {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
            }
        }

        // Upsert metadata to LanceDB (vectors stored separately in chunks table).
        // A re-create of an existing memory (worktree consolidation, or a
        // personal/shared visibility flip — see the sweep above) must not reset
        // `has_embedding`: the chunks table is untouched here, so carry the
        // current chunk-presence state forward. A fresh `IndexEntry`'s default
        // `false` would otherwise drop an already-embedded memory from semantic
        // ranking (R3). For a brand-new memory this is `false` as expected.
        let mut entry = IndexEntry::for_file(memory, &digest);
        entry.has_embedding =
            self.lance_index.has_chunks(&memory.id).await.map_err(|e| {
                StorageError::Validation(format!("LanceDB has_chunks failed: {}", e))
            })?;
        self.lance_index
            .upsert(&entry)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert failed: {}", e)))?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(memory.id.clone())
    }

    /// [`Self::create`] for many memories at once.
    ///
    /// Takes the per-project write lock **once**, writes every file, then
    /// performs a single ID probe, a single chunk-presence probe, a single
    /// index upsert and a single manifest refresh for the whole set.
    ///
    /// The per-memory version costs *four* separate O(store size) operations
    /// each — the `find_ids_by_prefix` LIKE scan, the `has_chunks` query, the
    /// `merge_insert` commit, and the full-column scan behind
    /// `update_manifest_stats` — so creating W memories into a store of M is
    /// quadratic. Measured: one `create` costs 24 ms into an empty store and
    /// 582 ms into a 1000-memory one.
    ///
    /// Semantics are identical to calling [`Self::create`] in a loop, with two
    /// deliberate differences that both follow from batching:
    ///
    /// - The manifest is refreshed once at the end rather than after each
    ///   memory. Intermediate states were never observable anyway: the whole
    ///   loop ran under one lock in the caller's eyes, and the refresh is a
    ///   derived cache that `update_manifest_stats` recomputes from scratch.
    /// - The ID pre-existence probe reads the *filesystem* (one scan of both
    ///   visibility dirs) rather than the index. `create` gates its sweep on
    ///   an index probe reasoning that every store-managed file has an index
    ///   row; asking the filesystem answers the same question more directly,
    ///   and is what the sweep actually acts on. It is taken **before** any
    ///   file is written, so a memory in this batch cannot see its own new
    ///   file and mistake it for a pre-existing one.
    ///
    /// Ordering within the batch is preserved in the returned ids. Duplicate
    /// IDs within one call are not deduplicated — the later write wins, the
    /// same as consecutive `create` calls.
    pub async fn create_batch(&self, memories: &[Memory]) -> Result<Vec<String>> {
        if memories.is_empty() {
            return Ok(Vec::new());
        }

        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        // Probe BEFORE writing anything: after the writes every ID in the
        // batch would trivially "exist".
        let ids: Vec<&str> = memories.iter().map(|m| m.id.as_str()).collect();
        let preexisting = self.batch_exists(&ids).await?;

        // The index rows are built in a SECOND pass below (after one batched
        // chunk-presence probe), by which point each `content` string is long
        // out of scope — so the digests have to be carried forward rather than
        // recomputed. Recomputing would mean re-reading every file we just
        // wrote.
        let mut digests: HashMap<String, crate::digest::FileDigest> =
            HashMap::with_capacity(memories.len());

        for memory in memories {
            let memories_dir = self.get_memories_dir(&memory.visibility)?;
            async_fs::create_dir_all(&memories_dir).await?;

            let filename = memory_file::memory_filename(memory);
            let file_path = memories_dir.join(&filename);
            let content = memory_file::write_memory_file(memory)?;
            digests.insert(
                memory.id.clone(),
                crate::digest::FileDigest::of(content.as_bytes()),
            );
            atomic_write(&file_path, &content).await?;

            // Same cross-visibility sweep as `create`, on the same condition:
            // an ID that already had a file may have it under the *other*
            // visibility, or under a different title slug. New-file-first
            // ordering (the `atomic_write` above) means a concurrent reader
            // never sees a spurious NotFound. See `create` for the full
            // rationale; skipping this for fresh IDs is what keeps bulk
            // creates off the O(n^2) dirent path.
            if preexisting.contains(&memory.id) {
                for dir in [
                    self.get_memories_dir(&Visibility::Shared)?,
                    self.get_memories_dir(&Visibility::Personal)?,
                ] {
                    for old in find_memory_files(&dir, &memory.id).await? {
                        if old != file_path {
                            match async_fs::remove_file(&old).await {
                                Ok(()) => {}
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                Err(e) => return Err(e.into()),
                            }
                        }
                    }
                }
            }
        }

        // One chunk-presence probe for the whole batch. As in `create`, a
        // re-create must not reset `has_embedding`: the chunks table is
        // untouched here, so carry the current presence forward or an
        // already-embedded memory silently drops out of semantic ranking
        // (R3). Fresh IDs are absent from this set, giving `false`.
        let with_chunks = self
            .lance_index
            .memory_ids_with_chunks(&ids)
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB memory_ids_with_chunks failed: {}", e))
            })?;

        let entries: Vec<IndexEntry> = memories
            .iter()
            .map(|m| {
                // Every id was inserted in the write loop above, so the lookup
                // cannot miss; `without_digest` is an unreachable fallback that
                // degrades to "unknown" rather than panicking on a future
                // refactor that breaks the pairing.
                let mut entry = match digests.get(&m.id) {
                    Some(digest) => IndexEntry::for_file(m, digest),
                    None => IndexEntry::without_digest(m),
                };
                entry.has_embedding = with_chunks.contains(&m.id);
                entry
            })
            .collect();

        self.lance_index
            .upsert_batch(&entries)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert_batch failed: {}", e)))?;

        self.update_manifest_stats().await?;

        Ok(memories.iter().map(|m| m.id.clone()).collect())
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

        // Overlap the per-file reads (bounded) instead of awaiting each in
        // sequence — this backs the retrieval hot path, where serializing
        // dozens of read latencies adds up. `buffered` (not
        // `buffer_unordered`) preserves the caller's id order. The (id, path)
        // pairs are materialized first — an owned Vec, not a lazy iterator —
        // which also sidesteps the higher-ranked lifetime bound the stream
        // adapters would otherwise demand of the map-lookup closure.
        use futures_util::{stream, StreamExt};
        let to_read: Vec<(String, PathBuf)> = ids
            .iter()
            .filter_map(|id| {
                let id_str = id.as_ref();
                let path = shared_map
                    .get(id_str)
                    .or_else(|| personal_map.get(id_str))?;
                Some((id_str.to_owned(), path.clone()))
            })
            .collect();
        let read: Vec<(String, PathBuf, String)> = stream::iter(to_read)
            .map(|(id_str, path)| async move {
                // An indexed memory whose file is unreadable/unparseable is a
                // data-integrity problem. Drop it (as before, so one bad file
                // doesn't fail a whole batch) but `warn!` rather than swallow it
                // silently, matching `reindex_dir`'s handling (finding #15).
                match async_fs::read_to_string(&path).await {
                    Ok(content) => Some((id_str, path, content)),
                    Err(e) => {
                        tracing::warn!(
                            "get_batch: skipping {} ({}): failed to read: {}",
                            id_str,
                            path.display(),
                            e
                        );
                        None
                    }
                }
            })
            .buffered(PARALLEL_READ_WIDTH)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect();

        Ok(parse_batch(read))
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
            1 => Self::read_found_file(&matches[0], id).await,
            _ => {
                // Multiple files for ONE full ID (`find_memory_files` already
                // rejected distinct IDs as ambiguous): a stale duplicate left
                // by an update that crashed between writing the renamed file
                // and removing the old one. The newest file is the current
                // state — the rename path always writes the new file last.
                let newest = newest_by_mtime(&matches).await;
                Self::read_found_file(newest, id).await
            }
        }
    }

    /// Read + parse a memory file found by a directory scan.
    ///
    /// Reads are lock-free, so a concurrent rename-update (title/visibility
    /// change writes the new file then unlinks the old) or delete can remove
    /// the scanned path before we read it. That is the documented `NotFound`
    /// condition — `get` relies on it to fall through to the personal dir —
    /// not an opaque I/O failure.
    async fn read_found_file(path: &Path, id: &str) -> Result<Memory> {
        match async_fs::read_to_string(path).await {
            Ok(content) => memory_file::parse_memory_file(&content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::NotFound(id.to_string()))
            }
            Err(e) => Err(e.into()),
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

    /// [`Self::update_with`] for many memories under **one** lock, one batched
    /// read, one index upsert and one manifest-stats refresh.
    ///
    /// The per-memory version costs a lock, a directory scan, a `has_chunks`
    /// scan, an index commit and a full manifest-stats scan *each* — measured
    /// at 28 ms/memory at n=16 rising to 60 ms/memory at n=128, i.e.
    /// quadratic. Callers that mutate a set of memories (task completion,
    /// closing superseded validity windows) should use this.
    ///
    /// Semantics match `update_with` per entry: each memory is re-read inside
    /// the lock so `f` sees the latest persisted state, `updated_at` is bumped,
    /// and a failing closure fails only its own memory. Ids that do not resolve
    /// are reported rather than aborting the batch.
    ///
    /// **Prefix ids resolve, as they do for `update_with`.** [`Self::get_batch`]
    /// keys off the exact id, so anything shorter than a full id missed the map
    /// and would have been reported as "memory not found" — a silent divergence
    /// from the per-memory version this is documented to match. Resolution is a
    /// second pass over only the ids the batched read did not answer, so a
    /// caller passing full ids (all of them today) pays nothing for it.
    ///
    /// Returns `(updated_ids, per_id_errors)`, where the updated ids are the
    /// **canonical** ones — `update_with` returns the persisted `Memory`, whose
    /// id is canonical, so a caller that reports what it changed must not be
    /// handed back the prefix it happened to pass in.
    pub async fn update_batch_with<F>(
        &self,
        ids: &[String],
        mut f: F,
    ) -> Result<(Vec<String>, Vec<(String, String)>)>
    where
        F: FnMut(&mut Memory) -> anyhow::Result<()>,
    {
        if ids.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut current: HashMap<String, Memory> =
            self.get_batch(&id_refs).await?.into_iter().collect();

        // Second pass for the ids the exact-keyed batch read could not answer.
        // `get` is what `update_with` calls, so a prefix resolves by exactly
        // the same rules (including "an exact full-id match beats prefix
        // ambiguity") rather than by a second, subtly different implementation.
        // Empty in the common case — every caller today passes full ids — so
        // the directory scan it costs is paid only by a batch that would
        // otherwise have reported a resolvable id as missing.
        let unresolved: Vec<&String> = ids.iter().filter(|id| !current.contains_key(*id)).collect();
        let mut aliases: HashMap<String, String> = HashMap::new();
        for id in unresolved {
            let Ok(memory) = self.get(id).await else {
                continue;
            };
            let full = memory.id.clone();
            // Two spellings of one memory in the same batch: alias the prefix
            // onto the row already read rather than keeping both, so `f` still
            // sees each memory exactly once — running the caller's mutation
            // twice, the second time against a copy that predates the first
            // write, is a lost update.
            current.entry(full.clone()).or_insert(memory);
            aliases.insert(id.clone(), full);
        }

        let mut updated: Vec<String> = Vec::new();
        let mut errors: Vec<(String, String)> = Vec::new();
        let mut entries: Vec<IndexEntry> = Vec::new();

        for given in ids {
            let key = aliases.get(given).unwrap_or(given);
            let Some(mut memory) = current.remove(key) else {
                errors.push((given.clone(), "memory not found".to_string()));
                continue;
            };
            // Errors stay keyed by the id the caller passed — that is the
            // string it can act on — while `updated` reports the canonical one.
            let canonical = memory.id.clone();
            let old_visibility = memory.visibility;
            if let Err(e) = f(&mut memory) {
                errors.push((given.clone(), format!("{:#}", e)));
                continue;
            }
            memory.mark_updated();

            // File writes stay per-memory: each is an atomic rename to its own
            // path, and the rename-on-title-change cleanup is inherently
            // per-file. It is the LanceDB commits and the manifest scan that
            // were quadratic, not this.
            let digest = match self
                .write_updated_file_locked(&canonical, &memory, old_visibility)
                .await
            {
                Ok(digest) => digest,
                Err(e) => {
                    errors.push((given.clone(), format!("{:#}", e)));
                    continue;
                }
            };

            let mut entry = IndexEntry::for_file(&memory, &digest);
            entry.has_embedding = self.lance_index.has_chunks(&memory.id).await.map_err(|e| {
                StorageError::Validation(format!("LanceDB has_chunks failed: {}", e))
            })?;
            entries.push(entry);
            updated.push(canonical);
        }

        if !entries.is_empty() {
            self.lance_index.upsert_batch(&entries).await.map_err(|e| {
                StorageError::Validation(format!("LanceDB upsert_batch failed: {}", e))
            })?;
            // One refresh for the whole batch instead of one per memory.
            self.update_manifest_stats().await?;
        }

        Ok((updated, errors))
    }

    /// Close a memory's validity window (§2.4): set `invalidated_at = now`
    /// and, when the closure was caused by supersession, the ADR-style
    /// reverse link `superseded_by`. The memory is retained on disk and
    /// queryable via `include_invalidated`, but excluded from default
    /// retrieval. A no-op error-free path is deliberately NOT provided for
    /// already-invalidated memories — the caller decides whether to skip
    /// (ops-level supersession logs and skips; see `is_invalidated_at`).
    ///
    /// Built on [`MemoryStore::update_with`], so the read-modify-write is one
    /// critical section under the per-project write lock and `updated_at` is
    /// bumped. Reopening remains possible via a plain `update_with` clearing
    /// the two fields — invalidation is reversible, unlike deletion.
    pub async fn invalidate_with(
        &self,
        id: &str,
        superseded_by: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Memory> {
        self.update_with(id, |memory| {
            memory.invalidated_at = Some(now);
            memory.superseded_by = superseded_by;
            Ok(())
        })
        .await
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
        let digest = self
            .write_updated_file_locked(id, memory, old_visibility)
            .await?;
        self.sync_index_row_locked(memory, &digest).await
    }

    /// The file half of [`Self::write_updated_locked`]: locate the old
    /// file(s), atomically write the new one, remove any stale path.
    ///
    /// Split out so [`Self::update_batch_with`] reuses this exact code rather
    /// than reimplementing the durability ordering below — the ordering is
    /// subtle enough that a second copy would be a latent data-loss bug.
    ///
    /// Returns the identity of the bytes it wrote, so the index half stamps the
    /// row from the same string rather than re-reading (or, worse,
    /// re-serializing) the file.
    async fn write_updated_file_locked(
        &self,
        id: &str,
        memory: &Memory,
        old_visibility: Visibility,
    ) -> Result<crate::digest::FileDigest> {
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
        let digest = crate::digest::FileDigest::of(content.as_bytes());

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

        Ok(digest)
    }

    /// The index half of [`Self::write_updated_locked`].
    async fn sync_index_row_locked(
        &self,
        memory: &Memory,
        digest: &crate::digest::FileDigest,
    ) -> Result<()> {
        // Upsert metadata to LanceDB (chunks are managed separately). An update
        // must not reset `has_embedding`: the memory may already have chunks
        // that this update isn't touching. Carry the current chunk-presence
        // state forward (a content-changing update's re-embed will set it true
        // again via `upsert_chunks` regardless).
        let mut entry = IndexEntry::for_file(memory, digest);
        // Propagate a chunk-presence read error rather than defaulting to
        // `false`: silently clearing the flag would drop a still-embedded memory
        // from `has_embedding`-gated semantic ranking (R3) until the next reindex.
        entry.has_embedding =
            self.lance_index.has_chunks(&memory.id).await.map_err(|e| {
                StorageError::Validation(format!("LanceDB has_chunks failed: {}", e))
            })?;
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

        self.delete_locked(&full_id).await
    }

    /// Conditionally delete a memory under the per-project write lock.
    ///
    /// Re-reads the memory *inside* the lock and calls `predicate` on the
    /// fresh state, so callers that scored or inspected the memory earlier
    /// (lock-free) can re-validate that decision against the latest persisted
    /// state before destroying data — closing the check-then-delete TOCTOU
    /// window that a plain [`Self::delete`] after an unlocked read leaves
    /// open.
    ///
    /// Returns:
    /// - `Ok(true)` — predicate returned `true` and the memory was deleted.
    /// - `Ok(false)` — the memory was kept (predicate returned `false`), or
    ///   it no longer exists (concurrently deleted, or its data file is
    ///   missing). Missing memories are a skip, never an error.
    pub async fn delete_if<F>(&self, id: &str, predicate: F) -> Result<bool>
    where
        F: FnOnce(&Memory) -> bool,
    {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let full_id = match self.resolve_full_id(id).await {
            Ok(full_id) => full_id,
            Err(StorageError::NotFound(_)) => return Ok(false),
            Err(e) => return Err(e),
        };

        // Fresh read inside the lock — this is the state the predicate
        // judges. A missing data file (stale index entry) is also a skip.
        let memory = match self.get(&full_id).await {
            Ok(m) => m,
            Err(StorageError::NotFound(_)) => return Ok(false),
            Err(e) => return Err(e),
        };

        if !predicate(&memory) {
            return Ok(false);
        }

        self.delete_locked(&full_id).await?;
        Ok(true)
    }

    /// [`Self::delete_if`] for many ids under **one** lock, one index delete,
    /// one chunk delete and one manifest-stats refresh.
    ///
    /// The per-id version costs a lock, an id resolution scan, a directory
    /// scan, two LanceDB commits and a full manifest-stats scan *each* — the
    /// heaviest per-item primitive in the store, and GC calls it once per
    /// candidate. On a sweep that is quadratic.
    ///
    /// The predicate contract is unchanged: every memory is re-read inside
    /// the same lock that performs the deletion and judged individually, so a
    /// concurrently-modified memory is skipped exactly as before. Returns
    /// `(deleted_ids, skipped_ids)`, both in input order.
    pub async fn delete_batch_if<F>(
        &self,
        ids: &[String],
        mut predicate: F,
    ) -> Result<(Vec<String>, Vec<String>)>
    where
        F: FnMut(&Memory) -> bool,
    {
        if ids.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let current: HashMap<String, Memory> =
            self.get_batch(&id_refs).await?.into_iter().collect();

        let mut to_delete: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for id in ids {
            match current.get(id) {
                // A memory whose file vanished (stale index entry) is a skip,
                // matching `delete_if`'s NotFound handling.
                None => skipped.push(id.clone()),
                Some(memory) if predicate(memory) => to_delete.push(id.clone()),
                Some(_) => skipped.push(id.clone()),
            }
        }
        if to_delete.is_empty() {
            return Ok((Vec::new(), skipped));
        }

        // Files first: the index row is the recovery hint if we crash midway
        // (a reindex rebuilds the row from the file, so an orphaned row is
        // repairable while an orphaned file would resurrect on reindex).
        for id in &to_delete {
            match self
                .delete_file_from_dir(id, &paths::memories_dir(&self.project_dir))
                .await
            {
                Ok(()) => {}
                Err(StorageError::NotFound(_)) => {
                    self.delete_file_from_dir(id, &paths::personal_memories_dir(&self.project_id)?)
                        .await?;
                }
                Err(e) => return Err(e),
            }
        }

        self.lance_index
            .delete_batch(&to_delete)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB delete_batch failed: {}", e)))?;
        self.lance_index
            .delete_chunks_batch(&to_delete)
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB delete_chunks_batch failed: {}", e))
            })?;

        // One refresh for the whole sweep instead of one per deletion.
        self.update_manifest_stats().await?;

        Ok((to_delete, skipped))
    }

    /// Shared deletion path for `delete` / `delete_if`. Callers MUST hold the
    /// per-project write lock and pass a fully resolved ID — this helper does
    /// not (re-)acquire the lock (`flock(2)` on a second fd in the same
    /// process would deadlock).
    async fn delete_locked(&self, full_id: &str) -> Result<()> {
        // Try to delete file from shared. Only a NotFound falls through to
        // the personal dir — a real I/O error must propagate as-is, not be
        // masked by the personal lookup's NotFound.
        match self
            .delete_file_from_dir(full_id, &paths::memories_dir(&self.project_dir))
            .await
        {
            Ok(()) => {}
            Err(StorageError::NotFound(_)) => {
                self.delete_file_from_dir(
                    full_id,
                    &paths::personal_memories_dir(&self.project_id)?,
                )
                .await?;
            }
            Err(e) => return Err(e),
        }

        // Delete from LanceDB (metadata + chunks)
        self.lance_index
            .delete(full_id)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB delete failed: {}", e)))?;
        self.lance_index.delete_chunks(full_id).await.map_err(|e| {
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

    /// Like [`Self::list_for_filtering`], but pushes an optional LanceDB
    /// `WHERE`-clause predicate into the index scan so selective scalar
    /// filters (type, criticality, expiry) narrow the row set before any
    /// disk I/O. See [`LanceIndex::list_for_filtering_where`] for the
    /// predicate-safety contract (trusted, escaped inputs only).
    pub async fn list_for_filtering_where(
        &self,
        predicate: Option<String>,
    ) -> Result<Vec<IndexForFiltering>> {
        self.lance_index
            .list_for_filtering_where(predicate)
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB list_for_filtering_where failed: {}", e))
            })
    }

    /// [`Self::list_for_filtering_where`] driven by a type-safe expression.
    /// See [`LanceIndex::list_for_filtering_where_expr`].
    pub async fn list_for_filtering_where_expr(
        &self,
        predicate: lancedb::expr::DfExpr,
    ) -> Result<Vec<IndexForFiltering>> {
        self.lance_index
            .list_for_filtering_where_expr(predicate)
            .await
            .map_err(|e| {
                StorageError::Validation(format!(
                    "LanceDB list_for_filtering_where_expr failed: {}",
                    e
                ))
            })
    }

    /// Build the memories-table scalar indexes. See
    /// [`LanceIndex::create_scalar_indexes`]. Best-effort: a scalar index only
    /// changes predicate speed, never which rows match.
    pub async fn create_scalar_indexes(&self) -> Result<Vec<String>> {
        self.lance_index.create_scalar_indexes().await.map_err(|e| {
            StorageError::Validation(format!("LanceDB create_scalar_indexes failed: {}", e))
        })
    }

    /// Build the FM substring index on `tags`. See
    /// [`LanceIndex::create_tag_search_index`].
    pub async fn create_tag_search_index(&self) -> Result<bool> {
        self.lance_index
            .create_tag_search_index()
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB create_tag_search_index failed: {}", e))
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

    /// List every memory in this store that cites a source session
    /// (schema v0.7.0). See [`LanceIndex::list_source_session_links`].
    pub async fn list_source_session_links(
        &self,
    ) -> Result<Vec<super::lance_index::SourceSessionLink>> {
        self.lance_index
            .list_source_session_links()
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB source-session scan failed: {}", e))
            })
    }

    /// Compact fragments and prune old LanceDB dataset versions for the
    /// memories and chunks tables. See [`LanceIndex::optimize`].
    ///
    /// Every create/update/delete commits a new immutable Lance version, so
    /// this is the disk-space reclamation path. Safe to run concurrently
    /// with readers (version pruning keeps the lancedb-default 7 days of
    /// versions). Callers should treat failures as non-fatal.
    pub async fn optimize(&self) -> Result<super::lance_index::IndexOptimizeStats> {
        self.lance_index
            .optimize()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB optimize failed: {}", e)))
    }

    /// Return the count of memories without loading data.
    /// Count memories grouped by type in one single-column index scan.
    ///
    /// See [`LanceIndex::count_by_type`]. Cheaper than deriving the histogram
    /// from [`Self::list_summary`], which decodes six columns this does not
    /// need.
    pub async fn count_by_type(&self) -> Result<HashMap<MemoryType, usize>> {
        self.lance_index
            .count_by_type()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB count_by_type failed: {}", e)))
    }

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
    /// Migrate the on-disk index schema when the manifest records an older
    /// [`manifest::CURRENT_SCHEMA_VERSION`].
    ///
    /// The migration is a plain **reindex**: the memories table is rebuilt from
    /// the authoritative `.md` files (populating new columns like `decay` and
    /// `has_embedding`) while the chunks/vectors table is preserved untouched —
    /// so it takes seconds and never re-embeds. Idempotent: a store already at
    /// the current version is a no-op, and a second open after a successful
    /// migration does nothing. Callers MUST NOT hold the write lock (reindex
    /// acquires it). Migration failures propagate so a broken store is loud
    /// rather than silently half-migrated.
    async fn migrate_schema_if_needed(&self) -> Result<()> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let loaded = manifest::load_manifest(&manifest_path).await.ok();
        let stored = loaded.as_ref().map(|m| m.schema_version.clone());
        let stored_normalizer = loaded.as_ref().and_then(|m| m.normalizer.clone());
        // Migrate only when the store is genuinely behind: a version at or ahead
        // of current (a newer binary's store) is left untouched, and a
        // manifest-less store is treated as pre-migration.
        let schema_stale = stored
            .as_deref()
            .is_none_or(|v| !manifest::schema_version_is_current(v));
        // Second, independent axis: the `keyword_stems` column can be present
        // and well-formed while its contents were produced by a different
        // stemmer or stoplist. Those stems no longer match query terms, so the
        // keyword signal silently degrades — regenerate them. Unlike the schema
        // version this is an equality check, not an ordering one: any
        // difference, older or newer, means the stored stems were not produced
        // by this binary's normalizer.
        let normalizer_stale = stored_normalizer.as_deref() != Some(engram_types::NORMALIZER_STAMP);
        if !schema_stale && !normalizer_stale {
            return Ok(());
        }
        tracing::info!(
            "Migrating EngramDB store (schema {} -> {}, normalizer {} -> {}): rebuilding index \
             from memory files (vectors preserved, no re-embed)",
            stored.as_deref().unwrap_or("<none>"),
            manifest::CURRENT_SCHEMA_VERSION,
            stored_normalizer.as_deref().unwrap_or("<none>"),
            engram_types::NORMALIZER_STAMP
        );
        // The chunks table has no `.md` files to rebuild from — recreating it
        // would destroy the vectors the rebuild exists to preserve — so any new
        // column there is added in place, as all-nulls (= "provenance
        // unknown" = re-embed when asked). Done HERE rather than at store open
        // because it commits a Lance `Merge`, which conflicts with any
        // concurrent write: unlocked at open, a parallel MCP session writing
        // chunks would fail the loser's `MemoryStore::open` and break an
        // ordinary read command. `reindex_with` below takes the lock, so this
        // runs just before it, under the lock acquired for the stamp.
        {
            let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
            self.lance_index
                .add_missing_chunk_columns()
                .await
                .map_err(|e| {
                    StorageError::Validation(format!("chunks-table column add failed: {}", e))
                })?;
        }

        // `force_schema_reset`: a schema migration must recreate the memories
        // table with the current columns. The default (upsert-only) reindex path
        // taken under a foreign checkout can't add columns and would fail on a
        // schema mismatch, so migration forces the table rebuild even then.
        self.reindex_with(true).await?;
        // reindex's `update_manifest_stats` rewrote the manifest but preserved
        // the old `schema_version`; stamp the new one now that the rebuild
        // succeeded (mirrors how the embedding fingerprint is stamped only on
        // success). Propagate a load failure rather than clobbering the store's
        // identity with a default manifest.
        //
        // Stamp under the write lock: like `set_embedding_fingerprint`, this
        // is a load-modify-save of `manifest.toml` racing every mutating op's
        // `update_manifest_stats` — unlocked, the stamp could be lost to a
        // concurrent stats rewrite (forcing a redundant re-migration) or
        // itself clobber a fingerprint a parallel `reindex --embeddings-only`
        // just stamped. `reindex_with` released its lock above, so acquiring
        // here cannot self-deadlock.
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        let mut manifest = manifest::load_manifest(&manifest_path).await?;
        manifest.schema_version = manifest::CURRENT_SCHEMA_VERSION.to_string();
        // Stamped together and only on success: the rebuild that refreshed the
        // columns is the same one that regenerated the stems, so recording one
        // without the other would leave the store claiming a state it is not in.
        manifest.normalizer = Some(engram_types::NORMALIZER_STAMP.to_string());
        manifest::save_manifest(&manifest_path, &manifest).await?;
        Ok(())
    }

    pub async fn reindex(&self) -> Result<usize> {
        self.reindex_with(false).await
    }

    /// Rebuild the memories table from the on-disk `.md` files, preserving
    /// vectors.
    ///
    /// `force_schema_reset` controls the foreign-checkout case: normally
    /// (`false`) a store whose project ID is shared with another live checkout
    /// runs a non-destructive, upsert-only rebuild so the other checkout's rows
    /// survive. A schema migration passes `true`: the memories table must be
    /// recreated with the current column set (upsert-only can't add columns and
    /// would fail on a schema mismatch), so the table is rebuilt from this
    /// checkout's files even under a conflict. The other checkout's **vectors**
    /// are still preserved (the chunks table is never dropped and orphan-pruning
    /// stays skipped); its index rows are rebuilt when it runs its own migration.
    async fn reindex_with(&self, force_schema_reset: bool) -> Result<usize> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let foreign_checkout = self.checkout_conflict().await;
        if let Some(other) = &foreign_checkout {
            if force_schema_reset {
                // Order matters in the shared-table case: the OTHER checkout's
                // rows rebuild on its next open only if it hasn't migrated yet
                // (its manifest is per-checkout, so its own migration re-runs
                // this clear+rebuild). A checkout that already migrated won't
                // migrate again, so THIS clear leaves it unindexed until it
                // runs `engramdb reindex` — say so instead of promising a
                // self-heal that only holds for the first migrator.
                tracing::warn!(
                    "Project ID is shared with another checkout at {} — migrating the \
                     shared memories table to the current schema; that checkout's vectors \
                     are preserved, and its index rows rebuild on its next open (or via \
                     `engramdb reindex` there if it already migrated)",
                    other.display()
                );
                self.lance_index.clear_memories().await.map_err(|e| {
                    StorageError::Validation(format!("LanceDB clear failed: {}", e))
                })?;
            } else {
                tracing::warn!(
                    "Project ID is shared with another checkout at {} — running a \
                     non-destructive (upsert-only) reindex; index rows and vectors \
                     belonging to the other checkout are preserved",
                    other.display()
                );
            }
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

        // The set of memories that currently have embedding chunks. Gathered
        // once, up front (the chunks table is untouched by the metadata
        // rebuild), and used both to stamp each rebuilt row's `has_embedding`
        // flag (R3) and to prune orphan chunks below.
        let chunk_ids: std::collections::HashSet<String> = self
            .lance_index
            .list_chunk_memory_ids()
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB list_chunk_memory_ids failed: {}", e))
            })?
            .into_iter()
            .collect();

        // Reindex shared memories
        let shared_dir = paths::memories_dir(&self.project_dir);
        if shared_dir.exists() {
            self.reindex_dir(
                &shared_dir,
                Visibility::Shared,
                &chunk_ids,
                &mut indexed_ids,
            )
            .await?;
        }

        // Reindex personal memories
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        if personal_dir.exists() {
            self.reindex_dir(
                &personal_dir,
                Visibility::Personal,
                &chunk_ids,
                &mut indexed_ids,
            )
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
            let orphans: Vec<String> = chunk_ids
                .iter()
                .filter(|id| !indexed_set.contains(id.as_str()))
                .cloned()
                .collect();
            // One IN-list delete commit for all orphans, not one per ID.
            self.lance_index
                .delete_chunks_batch(&orphans)
                .await
                .map_err(|e| {
                    StorageError::Validation(format!("LanceDB delete_chunks failed: {}", e))
                })?;
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

    /// The vector width of the on-disk chunks table, or `None` when the
    /// table doesn't exist yet. See `LanceIndex::chunks_table_dimensions`.
    pub async fn chunks_table_dimensions(&self) -> Result<Option<usize>> {
        self.lance_index
            .chunks_table_dimensions()
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB chunks schema read failed: {}", e))
            })
    }

    /// Upsert embedding chunks for a memory, recording no provenance for them.
    ///
    /// The un-digested entry point: the vectors are stored with a null
    /// `embed_sha256`, which every currency check reads as *unknown* and
    /// therefore "re-embed when asked". That is the correct answer for its two
    /// real callers — worktree consolidation, which **relocates** vectors it did
    /// not compute (and cannot compute a digest for: the source store has its
    /// own config, so its model, dimensions, composition and chunk size may all
    /// differ), and tests. The digest-carrying paths are
    /// [`Self::upsert_chunks_if_current`] and [`Self::upsert_chunks_batch`],
    /// which the retrieval engine drives.
    pub async fn upsert_chunks(&self, memory_id: &str, chunks: Vec<Vec<f32>>) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .upsert_chunks(memory_id, chunks, None)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert_chunks failed: {}", e)))
    }

    /// Upsert embedding chunks only if the memory still matches the snapshot
    /// they were computed from. Returns `true` when the chunks were written.
    ///
    /// Detached ingest tasks embed a `Memory` snapshot with no ordering
    /// guarantee: two rapid updates spawn two tasks whose `upsert_chunks`
    /// commits the flock serializes but does not order, so the task carrying
    /// the OLDER content can commit last (vectors then describe v1 while the
    /// file holds v2 until the next update/reindex); and a create-then-delete
    /// leaves a late embed re-inserting chunks for a deleted memory (orphan
    /// rows occupying top-k slots until doctor/reindex prunes them). This
    /// variant re-reads the memory UNDER the write lock and skips the write
    /// when the memory is gone or its `updated_at` no longer equals
    /// `snapshot_updated_at`.
    ///
    /// `embed_sha256` describes the text `chunks` were computed from and is
    /// written in the same `RecordBatch` as the vectors, so it can never end up
    /// describing a different snapshot than the vectors beside it.
    pub async fn upsert_chunks_if_current(
        &self,
        memory_id: &str,
        chunks: Vec<Vec<f32>>,
        snapshot_updated_at: chrono::DateTime<chrono::Utc>,
        embed_sha256: Option<String>,
    ) -> Result<bool> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        match self.get(memory_id).await {
            Ok(current) if current.updated_at == snapshot_updated_at => {}
            Ok(_) => {
                tracing::debug!(
                    "skipping stale chunk upsert for {memory_id}: memory changed since snapshot"
                );
                return Ok(false);
            }
            Err(StorageError::NotFound(_)) => {
                tracing::debug!("skipping chunk upsert for deleted memory {memory_id}");
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
        self.lance_index
            .upsert_chunks(memory_id, chunks, embed_sha256.as_deref())
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB upsert_chunks failed: {}", e))
            })?;
        Ok(true)
    }

    /// [`Self::upsert_chunks_if_current`] for many memories at once.
    ///
    /// Takes the per-project write lock **once**, re-reads every memory in a
    /// single batched directory scan, drops the entries whose `updated_at` no
    /// longer matches the snapshot (or that vanished), and writes what is left
    /// in one merge_insert.
    ///
    /// The per-memory version costs a lock, a full directory scan and two
    /// LanceDB commits *each*, so re-embedding a whole store through it is
    /// quadratic. The freshness guarantee is unchanged: every surviving entry
    /// was verified inside the same lock that performs the write.
    ///
    /// Returns the ids actually written.
    pub async fn upsert_chunks_batch(
        &self,
        entries: Vec<crate::lance_index::GuardedChunkWrite>,
    ) -> Result<Vec<String>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let ids: Vec<&str> = entries.iter().map(|(id, _, _, _)| id.as_str()).collect();
        let current: HashMap<String, Memory> = self.get_batch(&ids).await?.into_iter().collect();

        let mut fresh: Vec<crate::lance_index::ChunkWrite> = Vec::with_capacity(entries.len());
        for (id, snapshot_updated_at, chunks, digest) in entries {
            match current.get(&id) {
                Some(mem) if mem.updated_at == snapshot_updated_at => {
                    fresh.push((id, chunks, digest))
                }
                Some(_) => tracing::debug!(
                    "skipping stale chunk upsert for {id}: memory changed since snapshot"
                ),
                None => tracing::debug!("skipping chunk upsert for deleted memory {id}"),
            }
        }
        if fresh.is_empty() {
            return Ok(Vec::new());
        }

        self.lance_index
            .upsert_chunks_batch(&fresh)
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB upsert_chunks_batch failed: {}", e))
            })?;
        Ok(fresh.into_iter().map(|(id, _, _)| id).collect())
    }

    /// Every embedded memory's `embed_sha256` (schema v0.8.0), keyed by memory
    /// id. See [`crate::lance_index::LanceIndex::list_embed_digests`].
    pub async fn embed_digests(&self) -> Result<HashMap<String, Option<String>>> {
        self.lance_index.list_embed_digests().await.map_err(|e| {
            StorageError::Validation(format!("LanceDB list_embed_digests failed: {}", e))
        })
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

    /// [`Self::export_chunks`] for many memories in one scan, keyed by id.
    ///
    /// Mirrors [`Self::get_batch`], and for the same reason: the per-item form
    /// re-opens the dataset and re-plans the query on every call, so a loop
    /// over it is O(n²). Ids with no stored vectors are absent from the map.
    pub async fn export_chunks_batch<S: AsRef<str>>(
        &self,
        ids: &[S],
    ) -> Result<HashMap<String, Vec<Vec<f32>>>> {
        let refs: Vec<&str> = ids.iter().map(AsRef::as_ref).collect();
        self.lance_index
            .chunks_for_memories(&refs)
            .await
            .map_err(|e| {
                StorageError::Validation(format!("LanceDB chunks_for_memories failed: {}", e))
            })
    }

    /// Perform vector similarity search.
    ///
    /// `restrict_to` optionally pushes a candidate `memory_id` set down into
    /// the LanceDB predicate so the top-k window isn't saturated by memories
    /// the caller has already filtered out. `Some(&[])` returns no matches;
    /// `None` searches the whole store.
    pub async fn vector_search(
        &self,
        query: Vec<f32>,
        limit: usize,
        restrict_to: Option<&[String]>,
    ) -> Result<Vec<VectorMatch>> {
        self.lance_index
            .vector_search(query, limit, restrict_to)
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
            // Exact full-ID match beats prefix ambiguity (legacy non-UUID
            // ids can be prefixes of each other) — mirrors
            // `find_memory_files` and `paths::find_memory_in_dir`.
            _ if matches.iter().any(|m| m == id) => Ok(id.to_string()),
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
        chunk_ids: &std::collections::HashSet<String>,
        indexed_ids: &mut Vec<String>,
    ) -> Result<()> {
        use rayon::prelude::*;

        use crate::digest::FileDigest;

        let mut by_id: HashMap<String, (PathBuf, std::time::SystemTime, Memory, FileDigest)> =
            HashMap::new();

        // Phase 1 — enumerate. Cheap, and it has to be sequential anyway.
        let mut md_paths: Vec<PathBuf> = Vec::new();
        let mut entries = async_fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                md_paths.push(path);
            }
        }

        // Phase 2 — read + stat, overlapped. Bounded at the same width as
        // `get_batch` so a large store cannot exhaust the fd table.
        use futures_util::{stream, StreamExt};
        let read: Vec<(PathBuf, std::time::SystemTime, String)> = stream::iter(md_paths)
            .map(|path| async move {
                match async_fs::read_to_string(&path).await {
                    Ok(content) => {
                        let mtime = file_mtime(&path).await;
                        Some((path, mtime, content))
                    }
                    Err(e) => {
                        tracing::warn!("Skipping {}: failed to read: {}", path.display(), e);
                        None
                    }
                }
            })
            .buffered(PARALLEL_READ_WIDTH)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect();

        // Phase 3 — parse, in parallel. Frontmatter deserialization plus the
        // V2 section split is pure CPU and pure per-file: measured at ~20ms
        // per 1,000 memories serially, ~6ms across four cores
        // (`benches/parallel_simd.rs`, `parse/*`). This is the single largest
        // component of `reindex`.
        //
        // `reindex` already runs under the per-project write lock and is not
        // latency-critical, so occupying the rayon pool here is free; the
        // calling tokio worker is blocked for a *shorter* time than before,
        // since the work used to run inline on it.
        // The content digest is computed HERE, not in phase 2 and not in phase
        // 5. Phase 2's `content` strings are consumed by this very iterator, so
        // by phase 5 they are gone — carrying them that far would hold every
        // memory file's full text in memory across three phases, and re-reading
        // the file afterwards would both cost a second I/O pass and race the
        // writer we are not excluding (a foreign editor, not another EngramDB
        // process). Hashing inside this closure rides the rayon pool that is
        // already parallelising the parse, over bytes that are already hot.
        let parsed: Vec<(PathBuf, std::time::SystemTime, Memory, FileDigest)> = read
            .into_par_iter()
            .filter_map(
                |(path, mtime, content)| match memory_file::parse_memory_file(&content) {
                    Ok(memory) => {
                        let digest = FileDigest::of(content.as_bytes());
                        Some((path, mtime, memory, digest))
                    }
                    Err(e) => {
                        tracing::warn!("Skipping {}: failed to parse: {}", path.display(), e);
                        None
                    }
                },
            )
            .collect();

        // Phase 4 — resolve duplicate IDs. Sequential by nature (it is a
        // fold over a shared map) and cheap: no I/O, no parsing.
        // The digest travels with its own file through this fold: when two
        // files claim one id, the row must record the identity of the file that
        // WON (the one left on disk), not the one about to be deleted.
        for (path, mtime, memory, digest) in parsed {
            let stale = match by_id.entry(memory.id.clone()) {
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert((path, mtime, memory, digest));
                    None
                }
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    let (ref slot_path, slot_mtime, _, _) = *slot.get();
                    if prefers_newer((mtime, &path), (slot_mtime, slot_path)) {
                        let (old_path, _, _, _) = slot.insert((path, mtime, memory, digest));
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

        // Phase 5 — build the index rows, in parallel. `IndexEntry::for_file`
        // derives the memory's keyword stems (tokenize → stoplist → Snowball
        // stem → dedup for three fields), the second-largest CPU cost here at
        // ~16ms per 1,000 memories serially, ~5.7ms across four cores
        // (`benches/parallel_simd.rs`, `stems/*`).
        //
        // One batched merge_insert instead of one commit (and one LanceDB
        // dataset version) per memory — reindexing a 1,000-memory store used
        // to perform 1,000 sequential commits that optimize() then compacted.
        let owned: Vec<(String, Memory, FileDigest)> = by_id
            .into_iter()
            .map(|(id, (_path, _mtime, memory, digest))| (id, memory, digest))
            .collect();
        let entries_batch: Vec<IndexEntry> = owned
            .par_iter()
            .map(|(id, memory, digest)| {
                let mut index_entry = IndexEntry::for_file(memory, digest);
                index_entry.visibility = visibility;
                // R3: stamp the embedding flag from the chunk-table snapshot so a
                // reindex (including the on-open schema migration) leaves
                // `has_embedding` authoritative.
                index_entry.has_embedding = chunk_ids.contains(id);
                index_entry
            })
            .collect();
        indexed_ids.extend(owned.into_iter().map(|(id, _, _)| id));
        self.lance_index
            .upsert_batch(&entries_batch)
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB upsert failed: {}", e)))?;
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
        let has_conflict = self.checkout_conflict().await.is_some();
        Ok(staleness_message(md_count, lance_count, has_conflict))
    }

    /// Recompute and persist manifest stats (load-modify-save of
    /// `manifest.toml`).
    ///
    /// Callers MUST hold the per-project write lock — every call site
    /// (`create`, `write_updated_locked`, `delete`, `reindex`) already runs
    /// inside it. Calling this unlocked could race another manifest writer
    /// (e.g. [`Self::set_embedding_fingerprint`]) and clobber its fields.
    ///
    /// This runs on every mutation while the lock is held, so it is kept
    /// deliberately cheap: one single-column index scan (no 7-column
    /// deserialize, no per-row summary allocation), and the `manifest.toml`
    /// rewrite is skipped entirely when the stats did not change (the common
    /// case for updates).
    async fn update_manifest_stats(&self) -> Result<()> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        // Stats refresh runs AFTER the mutation (file + index row) is durable,
        // so a missing/corrupt manifest must not fail the operation — the
        // caller would report failure for a create/update/delete that in fact
        // succeeded, and nothing would ever self-heal the manifest. Rebuild a
        // default one instead (the project id is known) and let the stats
        // write below recreate the file.
        let mut manifest = match manifest::load_manifest(&manifest_path).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    "manifest.toml missing or unreadable ({e}); recreating with fresh stats"
                );
                manifest::Manifest {
                    // Same derivation as `init` (the manifest records the
                    // human project NAME; the registry owns the id).
                    project: self
                        .project_dir
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unnamed-project".to_string()),
                    ..Default::default()
                }
            }
        };

        let (memory_count, scope_set) = self
            .lance_index
            .count_and_logical_scopes()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB stats scan failed: {}", e)))?;
        // Sorted for a deterministic manifest (HashSet order used to make
        // the scope list churn between otherwise-identical rewrites).
        let mut logical_scopes: Vec<String> = scope_set.into_iter().collect();
        logical_scopes.sort();

        let mut current = manifest.stats.logical_scopes.clone();
        current.sort();
        if manifest.stats.memory_count == memory_count && current == logical_scopes {
            return Ok(());
        }

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

    /// Whether the store holds ANY embedding vectors (cheap `count_rows`).
    /// See [`LanceIndex::has_any_chunks`].
    pub async fn has_any_chunks(&self) -> Result<bool> {
        self.lance_index
            .has_any_chunks()
            .await
            .map_err(|e| StorageError::Validation(format!("LanceDB chunk count failed: {}", e)))
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
///
/// Public because every writer of store-managed files — including the CLI's
/// `migrate`/`rollback` bulk rewrites — must honor the same atomicity
/// contract; a plain `std::fs::write` can leave a truncated memory file on
/// crash.
pub async fn atomic_write(path: &Path, content: &str) -> Result<()> {
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
        // An exact full-ID match beats prefix ambiguity: with legacy
        // non-UUID ids, "abc" and "abcd" can coexist, and `get("abc")` must
        // resolve to the exact memory rather than erroring (mirrors
        // `paths::find_memory_in_dir`).
        if distinct_ids.contains(id) {
            matches.retain(|path| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|stem| memory_file::extract_id_from_stem(stem) == id)
            });
            return Ok(matches);
        }
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

/// Whether candidate `a` should be preferred over the current best `b` when
/// resolving which of several duplicate files for one memory ID is "current".
///
/// Newer mtime wins. On a *tie* (identical mtimes — common on filesystems with
/// 1-second mtime granularity when an update and its stale predecessor are
/// written in the same second, or when files are copied), the lexicographically
/// greater path wins. The tiebreak makes duplicate resolution **deterministic**
/// rather than dependent on directory-iteration order, which would otherwise let
/// a crash-during-rename resurrect stale content nondeterministically (finding
/// #14).
fn prefers_newer(a: (std::time::SystemTime, &Path), b: (std::time::SystemTime, &Path)) -> bool {
    match a.0.cmp(&b.0) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => a.1 > b.1,
    }
}

/// Pick the most recently modified path. `paths` must be non-empty.
async fn newest_by_mtime(paths: &[PathBuf]) -> &PathBuf {
    let mut best = &paths[0];
    let mut best_mtime = file_mtime(best).await;
    for path in &paths[1..] {
        let mtime = file_mtime(path).await;
        if prefers_newer((mtime, path), (best_mtime, best)) {
            best = path;
            best_mtime = mtime;
        }
    }
    best
}

/// Decide the staleness warning, if any. Pure so the checkout-conflict
/// suppression rule is unit-testable without manipulating the global registry.
///
/// Under a shared-ID **checkout conflict** (two clones of the same git remote
/// share one LanceDB index but keep separate `.engramdb/memories/` trees), the
/// index legitimately holds the *other* checkout's rows, so `lance_count` is
/// permanently greater than this checkout's on-disk `md_count`. Emitting a
/// "run reindex" warning then is wrong: it never clears (reindex degrades to a
/// non-destructive upsert under conflict, by design) and only confuses the
/// user. So suppress the warning entirely when a conflict is present (finding
/// #5).
fn staleness_message(
    md_count: usize,
    lance_count: usize,
    checkout_conflict: bool,
) -> Option<String> {
    if checkout_conflict {
        return None;
    }
    if md_count != lance_count {
        Some(format!(
            "Index may be stale ({} memories on disk, {} indexed). Run 'engramdb reindex' to rebuild.",
            md_count, lance_count
        ))
    } else {
        None
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

    /// The batched chunk write must be observably identical to N single
    /// writes — same vectors readable back, same `has_embedding` flag.
    #[tokio::test]
    async fn upsert_chunks_batch_matches_per_memory_writes() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

        let mut mems = Vec::new();
        for i in 0..5 {
            let m = create_test_memory(&format!("batch-{i}"), Visibility::Shared);
            store.create(&m).await.unwrap();
            mems.push(m);
        }

        let entries: Vec<_> = mems
            .iter()
            .enumerate()
            .map(|(i, m)| {
                (
                    m.id.clone(),
                    m.updated_at,
                    vec![vec![i as f32; 384], vec![(i + 100) as f32; 384]],
                    None,
                )
            })
            .collect();
        let written = store.upsert_chunks_batch(entries).await.unwrap();
        assert_eq!(written.len(), 5);

        for (i, m) in mems.iter().enumerate() {
            let chunks = store.export_chunks(&m.id).await.unwrap();
            assert_eq!(chunks.len(), 2, "memory {i} should have both chunks");
            assert_eq!(chunks[0], vec![i as f32; 384], "chunk order must be stable");
            assert_eq!(chunks[1], vec![(i + 100) as f32; 384]);
            assert!(
                has_embedding_flag(&store, &m.id).await,
                "has_embedding must be set by the batch"
            );
        }
    }

    /// The freshness guard is per-entry: a memory modified since its snapshot
    /// is skipped while its neighbours in the same batch are still written.
    #[tokio::test]
    async fn upsert_chunks_batch_skips_only_the_stale_entry() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

        let fresh = create_test_memory("batch-fresh", Visibility::Shared);
        let stale = create_test_memory("batch-stale", Visibility::Shared);
        store.create(&fresh).await.unwrap();
        store.create(&stale).await.unwrap();

        let entries = vec![
            (
                fresh.id.clone(),
                fresh.updated_at,
                vec![vec![1.0f32; 384]],
                None,
            ),
            // A snapshot timestamp that no longer matches the stored memory.
            (
                stale.id.clone(),
                stale.updated_at - chrono::Duration::seconds(60),
                vec![vec![2.0f32; 384]],
                None,
            ),
        ];
        let written = store.upsert_chunks_batch(entries).await.unwrap();

        assert_eq!(written, vec![fresh.id.clone()]);
        assert_eq!(store.export_chunks(&fresh.id).await.unwrap().len(), 1);
        assert!(
            store.export_chunks(&stale.id).await.unwrap().is_empty(),
            "the stale entry must not have been written"
        );
    }

    /// A batch write must not touch chunks belonging to memories outside it —
    /// the `when_not_matched_by_source_delete_expr` has to be scoped.
    #[tokio::test]
    async fn upsert_chunks_batch_leaves_other_memories_alone() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

        let kept = create_test_memory("batch-kept", Visibility::Shared);
        let rewritten = create_test_memory("batch-rewritten", Visibility::Shared);
        store.create(&kept).await.unwrap();
        store.create(&rewritten).await.unwrap();
        store
            .upsert_chunks(&kept.id, vec![vec![9.0f32; 384]])
            .await
            .unwrap();

        store
            .upsert_chunks_batch(vec![(
                rewritten.id.clone(),
                rewritten.updated_at,
                vec![vec![3.0f32; 384]],
                None,
            )])
            .await
            .unwrap();

        assert_eq!(
            store.export_chunks(&kept.id).await.unwrap(),
            vec![vec![9.0f32; 384]],
            "a memory outside the batch must keep its chunks"
        );
    }

    /// Shrinking a memory's chunk count must delete the surplus rows, matching
    /// `upsert_chunks`'s stale-row cleanup.
    #[tokio::test]
    async fn upsert_chunks_batch_drops_surplus_chunks() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

        let m = create_test_memory("batch-shrink", Visibility::Shared);
        store.create(&m).await.unwrap();
        store
            .upsert_chunks_batch(vec![(
                m.id.clone(),
                m.updated_at,
                vec![vec![1.0f32; 384], vec![2.0f32; 384], vec![3.0f32; 384]],
                None,
            )])
            .await
            .unwrap();
        assert_eq!(store.export_chunks(&m.id).await.unwrap().len(), 3);

        store
            .upsert_chunks_batch(vec![(
                m.id.clone(),
                m.updated_at,
                vec![vec![4.0f32; 384]],
                None,
            )])
            .await
            .unwrap();
        assert_eq!(
            store.export_chunks(&m.id).await.unwrap(),
            vec![vec![4.0f32; 384]],
            "surplus chunks from the previous write must be gone"
        );
    }

    /// `export_chunks_batch` must agree with N `export_chunks` calls, including
    /// chunk order and absent ids.
    #[tokio::test]
    async fn export_chunks_batch_matches_per_memory_reads() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

        let mut ids = Vec::new();
        for i in 0..6 {
            let m = create_test_memory(&format!("read-{i}"), Visibility::Shared);
            store.create(&m).await.unwrap();
            // Leave memory 4 without vectors.
            if i != 4 {
                store
                    .upsert_chunks(&m.id, vec![vec![i as f32; 384], vec![(i + 50) as f32; 384]])
                    .await
                    .unwrap();
            }
            ids.push(m.id.clone());
        }

        let batched = store.export_chunks_batch(&ids).await.unwrap();
        for id in &ids {
            let single = store.export_chunks(id).await.unwrap();
            if single.is_empty() {
                assert!(
                    !batched.contains_key(id),
                    "a memory with no chunks must be absent from the map, not empty"
                );
            } else {
                assert_eq!(batched.get(id), Some(&single), "batched read must match");
            }
        }
        assert_eq!(batched.len(), 5);
    }

    async fn has_embedding_flag(store: &MemoryStore, id: &str) -> bool {
        store
            .lance_index
            .list_for_filtering()
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.id == id)
            .unwrap()
            .has_embedding
    }

    /// A non-finite `decay.floor` (hand-edited memory file; serde_json writes
    /// it as `null` without error) must not brick retrieval: the poisoned row
    /// degrades to undecayed with a warning instead of failing the whole
    /// `list_for_filtering` batch — the entry point of every query.
    #[tokio::test]
    async fn nonfinite_decay_floor_degrades_instead_of_failing_the_batch() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let mut poisoned = create_test_memory("decay-nan", Visibility::Shared);
        poisoned.decay = Some(engram_types::Decay {
            floor: f64::NAN,
            ..engram_types::Decay::linear(chrono::Duration::days(30))
        });
        store.create(&poisoned).await.unwrap();
        let mut healthy = create_test_memory("decay-ok", Visibility::Shared);
        healthy.decay = Some(engram_types::Decay::linear(chrono::Duration::days(30)));
        store.create(&healthy).await.unwrap();

        let entries = store.lance_index.list_for_filtering().await.unwrap();
        assert_eq!(entries.len(), 2, "both rows must survive");
        let get = |id: &str| entries.iter().find(|e| e.id == id).unwrap();
        assert!(
            get("decay-nan").decay.is_none(),
            "poisoned decay degrades to undecayed"
        );
        assert!(get("decay-ok").decay.is_some(), "healthy decay intact");
    }

    /// `clear_chunks` drops every vector, so it must also reset the
    /// memories-table `has_embedding` mirror: a memory whose re-embed then
    /// fails must score as "no evidence" (sem = None), not "checked, found
    /// nothing" (sem = 0.0).
    #[tokio::test]
    async fn clear_chunks_resets_has_embedding_flags() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let memory = create_test_memory("clear-flag", Visibility::Shared);
        store.create(&memory).await.unwrap();
        store
            .upsert_chunks("clear-flag", vec![vec![0.5f32; 384]])
            .await
            .unwrap();
        assert!(has_embedding_flag(&store, "clear-flag").await);

        store.clear_chunks().await.unwrap();
        assert!(
            !has_embedding_flag(&store, "clear-flag").await,
            "flag must not claim embeddings that were just dropped"
        );
    }

    /// The R2/R3 schema migration: an older-version store is transparently
    /// re-indexed on open, backfilling `decay` + `has_embedding` from the
    /// authoritative `.md` files and chunk table, and stamping the current
    /// version. A second open is a no-op.
    #[tokio::test]
    async fn schema_migration_on_open_backfills_decay_and_has_embedding() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

        // One decayed + embedded memory, one with decay explicitly disabled and
        // no chunks.
        let mut decayed = create_test_memory("mig-decayed", Visibility::Shared);
        decayed.decay = Some(engram_types::Decay::linear(chrono::Duration::days(30)));
        store.create(&decayed).await.unwrap();
        store
            .upsert_chunks("mig-decayed", vec![vec![0.1f32; 384]])
            .await
            .unwrap();
        let mut plain = create_test_memory("mig-plain", Visibility::Shared);
        plain.decay = None;
        store.create(&plain).await.unwrap();

        // Simulate a pre-migration store by downgrading the recorded version.
        let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
        let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
        m.schema_version = "0.1.0".to_string();
        manifest::save_manifest(&manifest_path, &m).await.unwrap();

        // Re-open → migration runs (reindex from files, vectors preserved).
        let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        assert_eq!(
            manifest::load_manifest(&manifest_path)
                .await
                .unwrap()
                .schema_version,
            manifest::CURRENT_SCHEMA_VERSION,
            "migration must stamp the current schema version"
        );

        let entries = store2.lance_index.list_for_filtering().await.unwrap();
        let decayed = entries.iter().find(|e| e.id == "mig-decayed").unwrap();
        assert!(
            matches!(
                decayed.decay.as_ref().map(|d| &d.strategy),
                Some(engram_types::DecayStrategy::Linear)
            ),
            "decay strategy backfilled from the .md file"
        );
        assert!(
            decayed.has_embedding,
            "embedded memory → has_embedding=true"
        );
        let plain = entries.iter().find(|e| e.id == "mig-plain").unwrap();
        assert!(plain.decay.is_none(), "decay=None round-trips as null");
        assert!(
            !plain.has_embedding,
            "unembedded memory → has_embedding=false"
        );

        // Idempotent: opening again does not re-migrate.
        let _store3 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        assert_eq!(
            manifest::load_manifest(&manifest_path)
                .await
                .unwrap()
                .schema_version,
            manifest::CURRENT_SCHEMA_VERSION
        );
    }

    /// The 0.3.0 schema migration (epistemic columns): a store stamped 0.2.0
    /// whose `.md` files predate the epistemic fields is transparently
    /// re-indexed on open. The seven new columns materialize the type-derived
    /// defaults (§2.5) — NOT the serde defaults — and vectors are preserved.
    #[tokio::test]
    async fn schema_migration_to_0_3_0_backfills_epistemic_columns() {
        use engram_types::{Epistemic, Generality};

        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

        // Diagonal memories write files byte-identical to the pre-epistemic
        // writer, so creating them and downgrading the stamp reproduces a
        // genuine pre-epistemic store. Debug's type default (Observation)
        // differs from the serde default (Fact), which is what makes the
        // materialization assertion meaningful.
        let mut debug_mem = create_test_memory("mig3-debug", Visibility::Shared);
        debug_mem.type_ = MemoryType::Debug;
        debug_mem.epistemic = MemoryType::Debug.default_epistemic();
        store.create(&debug_mem).await.unwrap();
        store
            .upsert_chunks("mig3-debug", vec![vec![0.1f32; 384]])
            .await
            .unwrap();
        let mut hazard_mem = create_test_memory("mig3-hazard", Visibility::Shared);
        hazard_mem.type_ = MemoryType::Hazard;
        hazard_mem.epistemic = MemoryType::Hazard.default_epistemic();
        store.create(&hazard_mem).await.unwrap();

        // Downgrade the recorded version to the pre-epistemic schema.
        let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
        let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
        m.schema_version = "0.2.0".to_string();
        manifest::save_manifest(&manifest_path, &m).await.unwrap();

        // Re-open → migration reindexes and stamps 0.3.0.
        let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        assert_eq!(
            manifest::load_manifest(&manifest_path)
                .await
                .unwrap()
                .schema_version,
            manifest::CURRENT_SCHEMA_VERSION,
        );

        let entries = store2.lance_index.list_for_filtering().await.unwrap();
        let debug_row = entries.iter().find(|e| e.id == "mig3-debug").unwrap();
        assert_eq!(
            debug_row.epistemic,
            Epistemic::Observation,
            "pre-epistemic Debug memory must materialize its TYPE default"
        );
        assert_eq!(debug_row.generality, Generality::Project);
        assert_eq!(debug_row.origin_task, None);
        assert_eq!(debug_row.invalidated_at, None);
        assert_eq!(debug_row.verified_at, None);
        assert!(debug_row.watch_paths.is_empty());
        assert!(
            debug_row.has_embedding,
            "vectors preserved across the 0.3.0 migration"
        );
        let hazard_row = entries.iter().find(|e| e.id == "mig3-hazard").unwrap();
        assert_eq!(hazard_row.epistemic, Epistemic::Fact);
    }

    /// The 0.3.0 columns carry real values through the normal write path:
    /// an off-diagonal memory with a full Validity lands in the filtering
    /// projection without any file loads.
    #[tokio::test]
    async fn epistemic_columns_roundtrip_through_index() {
        use engram_types::{Epistemic, Generality, Validity};

        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let mut memory = create_test_memory("epi-idx", Visibility::Shared);
        memory.type_ = MemoryType::Hazard;
        memory.epistemic = Epistemic::Observation; // off-diagonal
        memory.verified_at = Some("2026-03-01T00:00:00Z".parse().unwrap());
        memory.valid_from = Some("2026-01-10T00:00:00Z".parse().unwrap());
        memory.valid_while = Some(Validity {
            premise: Some("while ort is pinned".into()),
            invalidated_by: vec!["Cargo.lock".into(), "crates/engram-onnx/**".into()],
            origin_task: Some("epistemic-memory".into()),
            generality: Generality::Task,
            derived_from: vec![],
        });
        store.create(&memory).await.unwrap();

        let entries = store.lance_index.list_for_filtering().await.unwrap();
        let row = entries.iter().find(|e| e.id == "epi-idx").unwrap();
        assert_eq!(row.epistemic, Epistemic::Observation);
        assert_eq!(row.verified_at, memory.verified_at);
        assert_eq!(row.generality, Generality::Task);
        assert_eq!(row.origin_task.as_deref(), Some("epistemic-memory"));
        assert_eq!(row.invalidated_at, None);
        assert_eq!(
            row.watch_paths,
            vec![
                "Cargo.lock".to_string(),
                "crates/engram-onnx/**".to_string()
            ]
        );

        // The displayable projection carries valid_from.
        let filterable = store.lance_index.list_filterable().await.unwrap();
        let frow = filterable.iter().find(|e| e.id == "epi-idx").unwrap();
        assert_eq!(frow.valid_from, memory.valid_from);
    }

    /// `invalidate_with` closes the validity window atomically and reversibly.
    #[tokio::test]
    async fn invalidate_with_closes_and_reopens_window() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("inv-1", Visibility::Shared))
            .await
            .unwrap();

        let now: chrono::DateTime<chrono::Utc> = "2026-07-19T12:00:00Z".parse().unwrap();
        let closed = store
            .invalidate_with("inv-1", Some("succ-9".into()), now)
            .await
            .unwrap();
        assert_eq!(closed.invalidated_at, Some(now));
        assert_eq!(closed.superseded_by.as_deref(), Some("succ-9"));
        assert!(closed.is_invalidated_at(now));

        // Persisted (file + index), not just returned.
        let reread = store.get("inv-1").await.unwrap();
        assert_eq!(reread.invalidated_at, Some(now));
        let row = store
            .lance_index
            .list_for_filtering()
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.id == "inv-1")
            .unwrap();
        assert_eq!(row.invalidated_at, Some(now));

        // Reopening is a plain update clearing the two fields.
        store
            .update_with("inv-1", |m| {
                m.invalidated_at = None;
                m.superseded_by = None;
                Ok(())
            })
            .await
            .unwrap();
        let reopened = store.get("inv-1").await.unwrap();
        assert_eq!(reopened.invalidated_at, None);
        assert_eq!(reopened.superseded_by, None);
    }

    /// The `has_embedding` projection flag tracks the chunk lifecycle: false at
    /// create, true after chunks are written, preserved across a metadata-only
    /// update, false after chunks are deleted.
    #[tokio::test]
    async fn has_embedding_flag_tracks_chunk_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("he-1", Visibility::Shared))
            .await
            .unwrap();
        assert!(!has_embedding_flag(&store, "he-1").await, "no chunks yet");

        store
            .upsert_chunks("he-1", vec![vec![0.2f32; 384]])
            .await
            .unwrap();
        assert!(has_embedding_flag(&store, "he-1").await, "chunks written");

        // A metadata-only update must not reset the flag.
        store
            .update_with("he-1", |m| {
                m.criticality = 0.9;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            has_embedding_flag(&store, "he-1").await,
            "update must preserve has_embedding"
        );

        store.delete_chunks("he-1").await.unwrap();
        assert!(!has_embedding_flag(&store, "he-1").await, "chunks deleted");
    }

    /// REGRESSION: the BATCHED chunk delete must clear `has_embedding` too.
    ///
    /// `delete_chunks` cleared the flag but `delete_chunks_batch` did not,
    /// while `upsert_chunks_batch` routes every entry whose text embedded to
    /// nothing into the batched path. A memory edited down to empty content
    /// therefore ended with zero chunk rows and `has_embedding = true`, and
    /// stayed eligible for `has_embedding`-gated semantic ranking (R3) — with
    /// no vectors to rank on — until the next reindex rebuilt the flag.
    #[tokio::test]
    async fn delete_chunks_batch_clears_has_embedding_flag() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        for id in ["hb-1", "hb-2"] {
            store
                .create(&create_test_memory(id, Visibility::Shared))
                .await
                .unwrap();
            store
                .upsert_chunks(id, vec![vec![0.2f32; 384]])
                .await
                .unwrap();
            assert!(has_embedding_flag(&store, id).await);
        }

        store
            .lance_index
            .delete_chunks_batch(&["hb-1".to_string(), "hb-2".to_string()])
            .await
            .unwrap();

        for id in ["hb-1", "hb-2"] {
            assert!(
                !has_embedding_flag(&store, id).await,
                "{id}: batched chunk delete must clear has_embedding, \
                 exactly as the per-memory delete does"
            );
        }
    }

    /// The empty-text path through `upsert_chunks_batch` (which delegates to
    /// `delete_chunks_batch`) must leave the flag false — the end-to-end shape
    /// of the regression above, driven through the public API.
    #[tokio::test]
    async fn upsert_chunks_batch_with_no_chunks_clears_has_embedding() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mem = create_test_memory("hb-empty", Visibility::Shared);
        store.create(&mem).await.unwrap();
        store
            .upsert_chunks("hb-empty", vec![vec![0.2f32; 384]])
            .await
            .unwrap();
        assert!(has_embedding_flag(&store, "hb-empty").await);

        // Re-embedding a now-empty memory yields no chunks for it.
        let current = store.get("hb-empty").await.unwrap();
        store
            .upsert_chunks_batch(vec![(
                "hb-empty".to_string(),
                current.updated_at,
                Vec::new(),
                None,
            )])
            .await
            .unwrap();

        assert!(
            !has_embedding_flag(&store, "hb-empty").await,
            "a memory re-embedded to zero chunks must not stay has_embedding=true"
        );
    }

    /// A store predating `keyword_stems` must gain populated stems on open,
    /// and they must equal what computing from the `.md` file would produce.
    /// Equality is the whole point: the stored value is only useful because
    /// the query path can trust it matches what it would have derived.
    #[tokio::test]
    async fn schema_migration_to_0_5_0_backfills_keyword_stems() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();

        let mut mem = create_test_memory("mig5-stems", Visibility::Shared);
        mem.summary = "Compression merges near duplicate memories".to_string();
        mem.content = "Candidates surface by similarity; compression is deferred.".to_string();
        mem.tags = vec!["maintenance".to_string()];
        store.create(&mem).await.unwrap();

        // Downgrade both stamps to reproduce a genuine pre-0.5.0 store.
        let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
        let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
        m.schema_version = "0.4.0".to_string();
        m.normalizer = None;
        manifest::save_manifest(&manifest_path, &m).await.unwrap();

        let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        let after = manifest::load_manifest(&manifest_path).await.unwrap();
        assert_eq!(after.schema_version, manifest::CURRENT_SCHEMA_VERSION);
        assert_eq!(
            after.normalizer.as_deref(),
            Some(engram_types::NORMALIZER_STAMP),
            "the normalizer stamp is recorded alongside the schema version"
        );

        let entries = store2.lance_index.list_for_filtering().await.unwrap();
        let row = entries.iter().find(|e| e.id == "mig5-stems").unwrap();
        let stored = row
            .keyword_stems
            .as_ref()
            .expect("migration must populate keyword_stems");
        assert_eq!(
            *stored,
            engram_types::KeywordStems::compute(&mem.summary, &mem.tags, &mem.content),
            "stored stems must be identical to freshly computed ones"
        );
        // Sanity that stemming actually happened: "merges" -> "merg".
        assert!(
            stored.in_summary("merg"),
            "expected a stemmed summary term, got {:?}",
            stored.summary
        );
    }

    /// The second invalidation axis. A store already at the current schema but
    /// carrying stems from a different normalizer must still be rebuilt —
    /// the column is well-formed, its contents are simply not comparable.
    #[tokio::test]
    async fn stale_normalizer_stamp_triggers_reindex() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        store
            .create(&create_test_memory("stamp-mem", Visibility::Shared))
            .await
            .unwrap();

        // Schema stays current; only the normalizer stamp is foreign.
        let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
        let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
        assert_eq!(m.schema_version, manifest::CURRENT_SCHEMA_VERSION);
        m.normalizer = Some("some-other-stemmer-v9".to_string());
        manifest::save_manifest(&manifest_path, &m).await.unwrap();

        let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        assert_eq!(
            manifest::load_manifest(&manifest_path)
                .await
                .unwrap()
                .normalizer
                .as_deref(),
            Some(engram_types::NORMALIZER_STAMP),
            "a foreign normalizer stamp must be regenerated, not left in place"
        );
        let entries = store2.lance_index.list_for_filtering().await.unwrap();
        assert!(
            entries
                .iter()
                .find(|e| e.id == "stamp-mem")
                .unwrap()
                .keyword_stems
                .is_some(),
            "stems must be present after the normalizer-triggered rebuild"
        );
    }

    // ===================================================================
    // Schema 0.8.0: content digests (index currency).
    // ===================================================================

    /// The row's recorded digest for `id`, or `None` when unknown.
    async fn row_digest(store: &MemoryStore, id: &str) -> Option<crate::digest::FileDigest> {
        let digests = store.lance_index.list_digests().await.unwrap();
        let row = digests.into_iter().find(|d| d.memory_id == id)?;
        Some(crate::digest::FileDigest {
            sha256: row.content_sha256?,
            len: row.content_len?,
        })
    }

    /// The digest of whatever `id`'s file actually contains right now.
    async fn disk_digest(store: &MemoryStore, id: &str) -> crate::digest::FileDigest {
        let shared = paths::memories_dir(&store.project_dir);
        let personal = paths::personal_memories_dir(&store.project_id).unwrap();
        for dir in [shared, personal] {
            if let Some(path) = find_memory_files(&dir, id)
                .await
                .unwrap()
                .into_iter()
                .next()
            {
                let bytes = async_fs::read(&path).await.unwrap();
                return crate::digest::FileDigest::of(&bytes);
            }
        }
        panic!("no file on disk for {id}");
    }

    /// `create` must stamp the identity of the bytes it wrote — not of a
    /// re-serialization, and not nothing.
    #[tokio::test]
    async fn content_digest_stamped_on_create_matches_file_bytes() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("cd-1", Visibility::Shared))
            .await
            .unwrap();

        assert_eq!(
            row_digest(&store, "cd-1").await,
            Some(disk_digest(&store, "cd-1").await),
            "the row must record exactly what is on disk"
        );
    }

    /// Every write path stamps: single update, batch update, and batch create.
    #[tokio::test]
    async fn content_digest_stamped_on_every_write_path() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        // create_batch — the path where `content` is scoped to a different loop
        // than the index rows, so the digest has to be carried forward.
        store
            .create_batch(&[
                create_test_memory("cd-b1", Visibility::Shared),
                create_test_memory("cd-b2", Visibility::Shared),
            ])
            .await
            .unwrap();
        for id in ["cd-b1", "cd-b2"] {
            assert_eq!(
                row_digest(&store, id).await,
                Some(disk_digest(&store, id).await),
                "{id}: create_batch must stamp each row from its own file"
            );
        }

        // update_with — a content change must move the digest.
        let before = row_digest(&store, "cd-b1").await.unwrap();
        store
            .update_with("cd-b1", |m| {
                m.content = "materially different content".to_string();
                Ok(())
            })
            .await
            .unwrap();
        let after = row_digest(&store, "cd-b1").await.unwrap();
        assert_ne!(before.sha256, after.sha256, "update must restamp");
        assert_eq!(after, disk_digest(&store, "cd-b1").await);

        // update_batch_with — the separate batched path.
        store
            .update_batch_with(&["cd-b2".to_string()], |m| {
                m.content = "batched rewrite".to_string();
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            row_digest(&store, "cd-b2").await,
            Some(disk_digest(&store, "cd-b2").await),
            "update_batch_with must stamp too"
        );
    }

    /// A title change renames the file. The row must record the digest of the
    /// file that now exists, not of the one that was just deleted.
    #[tokio::test]
    async fn content_digest_survives_title_rename() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory(
                "0190cdcd-bbbb-7ccc-8ddd-000000000001",
                Visibility::Shared,
            ))
            .await
            .unwrap();

        store
            .update_with("0190cdcd-bbbb-7ccc-8ddd-000000000001", |m| {
                m.title = Some("a brand new title".to_string());
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(
            row_digest(&store, "0190cdcd-bbbb-7ccc-8ddd-000000000001").await,
            Some(disk_digest(&store, "0190cdcd-bbbb-7ccc-8ddd-000000000001").await),
            "the surviving file's digest must be the one recorded"
        );
    }

    /// `reindex` backfills digests from disk for rows that had none.
    #[tokio::test]
    async fn reindex_backfills_content_digest_from_disk() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("cd-rx", Visibility::Shared))
            .await
            .unwrap();

        // Simulate a row that predates the column by clearing it in place.
        let mut entry = IndexEntry::without_digest(&store.get("cd-rx").await.unwrap());
        entry.has_embedding = false;
        store.lance_index.upsert(&entry).await.unwrap();
        assert_eq!(row_digest(&store, "cd-rx").await, None, "cleared");

        store.reindex().await.unwrap();

        assert_eq!(
            row_digest(&store, "cd-rx").await,
            Some(disk_digest(&store, "cd-rx").await),
            "reindex must backfill the digest from the file it read"
        );
    }

    /// When two files claim one ID, `reindex_dir` deletes the loser. The row
    /// must carry the digest of the file left on disk — recording the deleted
    /// one would report permanent drift against a file that no longer exists.
    #[tokio::test]
    async fn content_digest_follows_winner_of_duplicate_id_resolution() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mut mem =
            create_test_memory("0190cdcd-bbbb-7ccc-8ddd-000000000002", Visibility::Shared);
        mem.title = Some("winner".to_string());
        store.create(&mem).await.unwrap();

        // A stale duplicate under a different slug, with an older mtime and
        // materially different content.
        let mut stale = mem.clone();
        stale.title = Some("loser".to_string());
        stale.content = "stale duplicate content".to_string();
        let shared = paths::memories_dir(&store.project_dir);
        let stale_path = shared.join(memory_file::memory_filename(&stale));
        std::fs::write(&stale_path, memory_file::write_memory_file(&stale).unwrap()).unwrap();
        age_file(&stale_path, 3600);

        store.reindex().await.unwrap();

        assert!(!stale_path.exists(), "the older duplicate is removed");
        assert_eq!(
            row_digest(&store, "0190cdcd-bbbb-7ccc-8ddd-000000000002").await,
            Some(disk_digest(&store, "0190cdcd-bbbb-7ccc-8ddd-000000000002").await),
            "the digest must be the surviving file's"
        );
    }

    /// The 0.8.0 migration against a store whose Arrow schema genuinely lacks
    /// the columns: they appear, are populated from the `.md` files, vectors
    /// survive, and the stamp advances.
    #[tokio::test]
    async fn schema_migration_to_0_8_0_upgrades_a_real_0_7_0_store() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("cd-mig", Visibility::Shared))
            .await
            .unwrap();
        store
            .upsert_chunks("cd-mig", vec![vec![0.4f32; 384]])
            .await
            .unwrap();
        drop(store);

        downgrade_to_0_7_0(tmp.path()).await;

        // Opening migrates.
        let store = MemoryStore::open(tmp.path()).await.unwrap();

        assert_eq!(
            row_digest(&store, "cd-mig").await,
            Some(disk_digest(&store, "cd-mig").await),
            "migration must backfill the digest from the file"
        );
        assert_eq!(
            store.export_chunks("cd-mig").await.unwrap(),
            vec![vec![0.4f32; 384]],
            "vectors must survive a metadata-only migration"
        );
        let m = manifest::load_manifest(&paths::project_dir(tmp.path()).join("manifest.toml"))
            .await
            .unwrap();
        assert_eq!(m.schema_version, manifest::CURRENT_SCHEMA_VERSION);
    }

    /// A file edited behind the store's back must be visible as drift: the
    /// recorded digest no longer matches the bytes on disk. This is the whole
    /// point of the column — counts and id sets are both unchanged here.
    #[tokio::test]
    async fn hand_edited_file_diverges_from_its_recorded_digest() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("cd-edit", Visibility::Shared))
            .await
            .unwrap();
        let recorded = row_digest(&store, "cd-edit").await.unwrap();

        // Edit the file directly, exactly as a human or `git checkout` would.
        let shared = paths::memories_dir(&store.project_dir);
        let path = find_memory_files(&shared, "cd-edit")
            .await
            .unwrap()
            .pop()
            .unwrap();
        let edited = std::fs::read_to_string(&path)
            .unwrap()
            .replace("Test content", "Edited behind the store's back");
        std::fs::write(&path, &edited).unwrap();

        assert_ne!(
            recorded,
            disk_digest(&store, "cd-edit").await,
            "an in-place edit must diverge from the recorded digest"
        );
        // ...and the checks that exist today cannot see it.
        assert_eq!(store.list_ids().await.unwrap().len(), 1);
        assert!(
            store.check_staleness().await.unwrap().is_none(),
            "the count-based check is blind to this — which is why the digest exists"
        );
    }

    /// The embed digest rides the same write as the vectors, and is readable
    /// back per memory.
    #[tokio::test]
    async fn embed_digest_is_written_and_read_back_with_its_vectors() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let m = create_test_memory("ed-1", Visibility::Shared);
        store.create(&m).await.unwrap();

        store
            .upsert_chunks_batch(vec![(
                "ed-1".to_string(),
                m.updated_at,
                vec![vec![0.1f32; 384], vec![0.2f32; 384]],
                Some("digest-abc".to_string()),
            )])
            .await
            .unwrap();

        let digests = store.embed_digests().await.unwrap();
        assert_eq!(
            digests.get("ed-1"),
            Some(&Some("digest-abc".to_string())),
            "the digest must be readable back for the memory it was written with"
        );
        // The vectors are unaffected by carrying the digest alongside them.
        assert_eq!(store.export_chunks("ed-1").await.unwrap().len(), 2);
    }

    /// The un-digested entry point records `None`, which reads as *unknown* —
    /// never as *current*. Worktree relocation and tests take this path.
    #[tokio::test]
    async fn upsert_chunks_without_digest_records_unknown() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("ed-2", Visibility::Shared))
            .await
            .unwrap();
        store
            .upsert_chunks("ed-2", vec![vec![0.3f32; 384]])
            .await
            .unwrap();

        assert_eq!(
            store.embed_digests().await.unwrap().get("ed-2"),
            Some(&None),
            "present but unknown — the memory has vectors, of unrecorded provenance"
        );
    }

    /// Deleting a memory's chunks takes its digest with them: absent from the
    /// map entirely, not left behind as a stale claim about vectors that are
    /// gone.
    #[tokio::test]
    async fn deleting_chunks_removes_the_digest_too() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let m = create_test_memory("ed-3", Visibility::Shared);
        store.create(&m).await.unwrap();
        store
            .upsert_chunks_batch(vec![(
                "ed-3".to_string(),
                m.updated_at,
                vec![vec![0.4f32; 384]],
                Some("digest-xyz".to_string()),
            )])
            .await
            .unwrap();
        assert!(store.embed_digests().await.unwrap().contains_key("ed-3"));

        store.delete_chunks("ed-3").await.unwrap();

        assert!(
            !store.embed_digests().await.unwrap().contains_key("ed-3"),
            "no vectors ⇒ no digest; the two are written and deleted together"
        );
    }

    /// A chunks table written before 0.8.0 has no `embed_sha256` column at all.
    /// The migration must add it in place — the vectors cannot be rebuilt from
    /// anything, so recreating the table would destroy exactly what the
    /// metadata-only migration exists to preserve.
    #[tokio::test]
    async fn schema_migration_adds_the_chunks_digest_column_in_place() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("ed-mig", Visibility::Shared))
            .await
            .unwrap();
        store
            .upsert_chunks("ed-mig", vec![vec![0.7f32; 384]])
            .await
            .unwrap();
        drop(store);

        drop_chunks_column(tmp.path(), "embed_sha256").await;
        downgrade_to_0_7_0(tmp.path()).await;

        let store = MemoryStore::open(tmp.path()).await.unwrap();

        // The column is back, the vectors survived, and their provenance reads
        // as unknown (so they re-embed when asked, rather than being trusted).
        assert_eq!(
            store.export_chunks("ed-mig").await.unwrap(),
            vec![vec![0.7f32; 384]],
            "vectors must survive the in-place column add"
        );
        assert_eq!(
            store.embed_digests().await.unwrap().get("ed-mig"),
            Some(&None)
        );

        // Idempotent: opening again must not fail or duplicate the column.
        drop(store);
        let store = MemoryStore::open(tmp.path()).await.unwrap();
        assert_eq!(store.export_chunks("ed-mig").await.unwrap().len(), 1);
    }

    /// Rewrite a store's `chunks` table without `col`, reproducing a table
    /// written before that column existed.
    async fn drop_chunks_column(dir: &Path, col: &str) {
        use arrow_array::RecordBatchIterator;
        use futures_util::stream::StreamExt;
        use lancedb::query::ExecutableQuery;
        use std::sync::Arc;

        let project_id = project_id::compute_project_id(dir);
        let lance_path = paths::lancedb_dir(&project_id).unwrap();
        let conn = lancedb::connect(lance_path.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let table = conn.open_table("chunks").execute().await.unwrap();

        let mut stream = table.query().execute().await.unwrap();
        let mut reduced = Vec::new();
        let mut reduced_schema = None;
        while let Some(batch) = stream.next().await {
            let batch = batch.unwrap();
            let schema = batch.schema();
            let keep: Vec<usize> = (0..schema.fields().len())
                .filter(|i| schema.field(*i).name() != col)
                .collect();
            assert_eq!(
                keep.len() + 1,
                schema.fields().len(),
                "fixture must actually remove {col}"
            );
            let fields: Vec<_> = keep.iter().map(|i| schema.field(*i).clone()).collect();
            let new_schema = Arc::new(arrow_schema::Schema::new(fields));
            let columns: Vec<_> = keep.iter().map(|i| batch.column(*i).clone()).collect();
            reduced.push(arrow_array::RecordBatch::try_new(new_schema.clone(), columns).unwrap());
            reduced_schema = Some(new_schema);
        }
        let schema = reduced_schema.expect("fixture needs at least one chunk");

        conn.drop_table("chunks", &[]).await.unwrap();
        let batches: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
            RecordBatchIterator::new(reduced.into_iter().map(Ok), schema),
        );
        conn.create_table("chunks", batches)
            .execute()
            .await
            .unwrap();
    }

    /// Rewrite a store's `memories` table without the `source_sessions`
    /// column, and stamp the manifest at 0.6.0.
    async fn downgrade_to_0_6_0(dir: &Path) {
        downgrade_dropping(dir, &["source_sessions"], "0.6.0").await
    }

    /// Rewrite a store's `memories` table without the 0.8.0 content-digest
    /// columns, and stamp the manifest at 0.7.0.
    async fn downgrade_to_0_7_0(dir: &Path) {
        downgrade_dropping(dir, &["content_sha256", "content_len"], "0.7.0").await
    }

    /// Rewrite a store's `memories` table with `drop_cols` removed from the
    /// Arrow schema, and stamp the manifest at `version`.
    ///
    /// The other migration tests here downgrade only the manifest stamp, which
    /// exercises the backfill but never the case that actually breaks: a table
    /// whose Arrow schema is genuinely missing the new column. This builds that
    /// table by reading the current one, dropping the fields and their arrays,
    /// and recreating the table from the reduced batches — so the fixture is the
    /// previous version's on-disk shape rather than a stand-in for it, and it
    /// stays honest if a later version adds another column.
    async fn downgrade_dropping(dir: &Path, drop_cols: &[&str], version: &str) {
        use arrow_array::RecordBatchIterator;
        use futures_util::stream::StreamExt;
        use lancedb::query::ExecutableQuery;
        use std::sync::Arc;

        let project_id = project_id::compute_project_id(dir);
        let lance_path = paths::lancedb_dir(&project_id).unwrap();
        let conn = lancedb::connect(lance_path.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let table = conn.open_table("memories").execute().await.unwrap();

        let mut stream = table.query().execute().await.unwrap();
        let mut reduced = Vec::new();
        let mut reduced_schema = None;
        while let Some(batch) = stream.next().await {
            let batch = batch.unwrap();
            let schema = batch.schema();
            let keep: Vec<usize> = (0..schema.fields().len())
                .filter(|i| !drop_cols.contains(&schema.field(*i).name().as_str()))
                .collect();
            assert_eq!(
                keep.len() + drop_cols.len(),
                schema.fields().len(),
                "fixture must actually remove every named column"
            );
            let fields: Vec<_> = keep.iter().map(|i| schema.field(*i).clone()).collect();
            let new_schema = Arc::new(arrow_schema::Schema::new(fields));
            let columns: Vec<_> = keep.iter().map(|i| batch.column(*i).clone()).collect();
            reduced.push(arrow_array::RecordBatch::try_new(new_schema.clone(), columns).unwrap());
            reduced_schema = Some(new_schema);
        }
        let schema = reduced_schema.expect("fixture needs at least one memory");

        conn.drop_table("memories", &[]).await.unwrap();
        let batches: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
            RecordBatchIterator::new(reduced.into_iter().map(Ok), schema),
        );
        conn.create_table("memories", batches)
            .execute()
            .await
            .unwrap();

        let manifest_path = paths::project_dir(dir).join("manifest.toml");
        let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
        m.schema_version = version.to_string();
        // Only the schema axis may trigger: leaving the normalizer stale would
        // migrate for the wrong reason and the test would pass with the version
        // gate broken.
        m.normalizer = Some(engram_types::NORMALIZER_STAMP.to_string());
        manifest::save_manifest(&manifest_path, &m).await.unwrap();
    }

    /// The 0.7.0 schema migration (harvest provenance), against a store whose
    /// table really was written by the previous version: the memories survive
    /// intact, the `source_sessions` column appears, vectors are preserved, and
    /// the stamp advances.
    #[tokio::test]
    async fn schema_migration_to_0_7_0_upgrades_a_real_0_6_0_store() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        {
            let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
            let mut shared = create_test_memory("mig7-shared", Visibility::Shared);
            shared.tags = vec!["alpha".into(), "beta".into()];
            shared.details = Some("details that must survive".into());
            store.create(&shared).await.unwrap();
            store
                .upsert_chunks("mig7-shared", vec![vec![0.2f32; 384]])
                .await
                .unwrap();
            store
                .create(&create_test_memory("mig7-personal", Visibility::Personal))
                .await
                .unwrap();
        }
        downgrade_to_0_6_0(tmp.path()).await;

        // Control: the fixture is genuinely old. A projection of the new column
        // against it fails, which is exactly what would happen to every pin
        // scan if the migration did not run.
        {
            let old = LanceIndex::new(
                &paths::lancedb_dir(&project_id::compute_project_id(tmp.path())).unwrap(),
                384,
            )
            .await
            .unwrap();
            assert!(
                old.list_source_session_links().await.is_err(),
                "fixture still has the 0.7.0 column, so the test proves nothing"
            );
        }

        // The hot path: a plain open must upgrade it.
        let store = MemoryStore::open(tmp.path()).await.unwrap();
        let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
        assert_eq!(
            manifest::load_manifest(&manifest_path)
                .await
                .unwrap()
                .schema_version,
            manifest::CURRENT_SCHEMA_VERSION,
            "migration must stamp the current schema version"
        );

        // Nothing lost: both memories, both visibilities, content and metadata.
        assert_eq!(store.count().await.unwrap(), 2);
        let shared = store.get("mig7-shared").await.unwrap();
        assert_eq!(shared.tags, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(shared.details.as_deref(), Some("details that must survive"));
        assert_eq!(shared.visibility, Visibility::Shared);
        assert!(
            shared.source_sessions.is_empty(),
            "a memory written before the column cites nothing — not an error"
        );
        let personal = store.get("mig7-personal").await.unwrap();
        assert_eq!(personal.visibility, Visibility::Personal);
        assert!(
            has_embedding_flag(&store, "mig7-shared").await,
            "vectors preserved across the 0.7.0 migration"
        );

        // The column is there and usable: the scan runs, and a link written
        // after the upgrade is visible to it.
        assert!(store.list_source_session_links().await.unwrap().is_empty());
        store
            .update_with("mig7-shared", |m| {
                m.link_source_session("sess-after-migration");
                Ok(())
            })
            .await
            .unwrap();
        let links = store.list_source_session_links().await.unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].memory_id, "mig7-shared");
        assert_eq!(links[0].sessions, vec!["sess-after-migration".to_string()]);

        // Idempotent: a second open does not re-migrate.
        let _store2 = MemoryStore::open(tmp.path()).await.unwrap();
        assert_eq!(
            manifest::load_manifest(&manifest_path)
                .await
                .unwrap()
                .schema_version,
            manifest::CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            store.list_source_session_links().await.unwrap().len(),
            1,
            "the link written after the migration must survive the next open"
        );
    }

    /// A store already current on both axes must NOT reindex — migration is
    /// meant to be a one-time cost, not a per-open one.
    #[tokio::test]
    async fn current_store_does_not_remigrate() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        {
            let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
            store
                .create(&create_test_memory("noop-mem", Visibility::Shared))
                .await
                .unwrap();
        }
        let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
        let before = manifest::load_manifest(&manifest_path).await.unwrap();
        assert_eq!(
            before.normalizer.as_deref(),
            Some(engram_types::NORMALIZER_STAMP),
            "a store created by this binary is born current"
        );

        let store2 = MemoryStore::init(tmp.path(), &reg).await.unwrap();
        let after = manifest::load_manifest(&manifest_path).await.unwrap();
        assert_eq!(after.schema_version, before.schema_version);
        assert_eq!(after.normalizer, before.normalizer);
        assert_eq!(store2.count().await.unwrap(), 1);
    }

    /// Migration must also run on the plain `open` path — the hot path for every
    /// CLI command and the MCP server — not just `init`/global. A pre-migration
    /// store opened via `open` is transparently re-indexed and stamped current.
    #[tokio::test]
    async fn schema_migration_runs_on_open_hot_path() {
        let tmp = TempDir::new().unwrap();
        let reg = InMemoryRegistry::new();
        // Create + embed a memory, then downgrade the recorded schema version.
        {
            let store = MemoryStore::init(tmp.path(), &reg).await.unwrap();
            store
                .create(&create_test_memory("open-mig", Visibility::Shared))
                .await
                .unwrap();
            store
                .upsert_chunks("open-mig", vec![vec![0.3f32; 384]])
                .await
                .unwrap();
        }
        let manifest_path = paths::project_dir(tmp.path()).join("manifest.toml");
        let mut m = manifest::load_manifest(&manifest_path).await.unwrap();
        m.schema_version = "0.1.0".to_string();
        manifest::save_manifest(&manifest_path, &m).await.unwrap();

        // Open via the hot path (NOT init) → migration must run.
        let store = MemoryStore::open(tmp.path()).await.unwrap();
        assert_eq!(
            manifest::load_manifest(&manifest_path)
                .await
                .unwrap()
                .schema_version,
            manifest::CURRENT_SCHEMA_VERSION,
            "open() must migrate a pre-migration store"
        );
        assert!(
            has_embedding_flag(&store, "open-mig").await,
            "has_embedding backfilled from chunks on the open() migration"
        );
    }

    /// Re-creating an existing memory (worktree consolidation / visibility flip)
    /// must not reset `has_embedding` to false — the chunks table is untouched,
    /// so a fresh `IndexEntry`'s default would otherwise drop the memory from
    /// semantic ranking (R3) until a reindex.
    #[tokio::test]
    async fn recreate_preserves_has_embedding() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mem = create_test_memory("recreate-1", Visibility::Shared);
        store.create(&mem).await.unwrap();
        store
            .upsert_chunks("recreate-1", vec![vec![0.4f32; 384]])
            .await
            .unwrap();
        assert!(has_embedding_flag(&store, "recreate-1").await, "embedded");

        // Re-create the same id without touching chunks (e.g. a re-run).
        store.create(&mem).await.unwrap();
        assert!(
            has_embedding_flag(&store, "recreate-1").await,
            "re-create must preserve has_embedding while chunks still exist"
        );
    }

    // ===================================================================
    // `create_batch` must be indistinguishable from `create` in a loop.
    // Each test below pins one guarantee the batched path could plausibly
    // drop, since it reimplements the probe/sweep/upsert sequence rather
    // than calling `create` internally.
    // ===================================================================

    #[tokio::test]
    async fn create_batch_writes_every_memory_and_indexes_them() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let batch: Vec<Memory> = (0..25)
            .map(|i| {
                create_test_memory(
                    &format!("0190aaaa-bbbb-7ccc-8ddd-{:012}", i),
                    Visibility::Shared,
                )
            })
            .collect();
        let ids = store.create_batch(&batch).await.unwrap();

        assert_eq!(ids.len(), 25, "one id returned per input, in order");
        assert_eq!(ids, batch.iter().map(|m| m.id.clone()).collect::<Vec<_>>());
        assert_eq!(store.count().await.unwrap(), 25, "every row indexed");
        for m in &batch {
            assert_eq!(store.get(&m.id).await.unwrap().id, m.id);
        }
    }

    #[tokio::test]
    async fn create_batch_empty_is_a_noop() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        assert!(store.create_batch(&[]).await.unwrap().is_empty());
        assert_eq!(store.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn create_batch_preserves_has_embedding_for_recreated_memories() {
        // The batched `has_embedding` carry-forward is a single probe over the
        // whole set, so a mistake here would silently drop already-embedded
        // memories out of semantic ranking (R3) — and only for re-creates,
        // which is exactly the worktree consolidation re-run case.
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let embedded =
            create_test_memory("0190aaaa-bbbb-7ccc-8ddd-00000000000a", Visibility::Shared);
        let bare = create_test_memory("0190aaaa-bbbb-7ccc-8ddd-00000000000b", Visibility::Shared);
        store.create(&embedded).await.unwrap();
        store
            .upsert_chunks(&embedded.id, vec![vec![0.4f32; 384]])
            .await
            .unwrap();
        assert!(has_embedding_flag(&store, &embedded.id).await);

        // Re-create BOTH in one batch, without touching chunks.
        store
            .create_batch(&[embedded.clone(), bare.clone()])
            .await
            .unwrap();

        assert!(
            has_embedding_flag(&store, &embedded.id).await,
            "re-create in a batch must preserve has_embedding while chunks exist"
        );
        assert!(
            !has_embedding_flag(&store, &bare.id).await,
            "a memory with no chunks must not be marked embedded"
        );
    }

    #[tokio::test]
    async fn create_batch_sweeps_old_visibility_file() {
        // Mirrors `create_same_id_different_visibility_does_not_orphan`. The
        // batched path takes its pre-existence probe once, before any write,
        // so this also pins that the probe is not accidentally reading the
        // batch's own freshly-written files.
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let id = "0190aaaa-bbbb-7ccc-8ddd-00000000000c";

        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();

        // Re-create the same id as Personal, in a batch alongside a fresh one.
        let fresh = create_test_memory("0190aaaa-bbbb-7ccc-8ddd-00000000000d", Visibility::Shared);
        store
            .create_batch(&[create_test_memory(id, Visibility::Personal), fresh.clone()])
            .await
            .unwrap();

        let shared = paths::memories_dir(&store.project_dir);
        let personal = paths::personal_memories_dir(&store.project_id).unwrap();

        assert_eq!(
            count_files(&shared).await,
            1,
            "only the fresh Shared memory remains; the old Shared file is swept"
        );
        assert_eq!(count_files(&personal).await, 1);
        assert_eq!(store.count().await.unwrap(), 2);
        assert_eq!(
            store.get(id).await.unwrap().visibility,
            Visibility::Personal
        );
    }

    #[tokio::test]
    async fn create_batch_refreshes_manifest_stats() {
        // The manifest refresh moved from once-per-memory to once-per-batch;
        // the end state must still be correct.
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let batch: Vec<Memory> = (0..7)
            .map(|i| {
                create_test_memory(
                    &format!("0190aaaa-bbbb-7ccc-8ddd-{:012}", 100 + i),
                    Visibility::Shared,
                )
            })
            .collect();
        store.create_batch(&batch).await.unwrap();

        let manifest_path = paths::project_dir(&store.project_dir).join("manifest.toml");
        let manifest = manifest::load_manifest(&manifest_path).await.unwrap();
        assert_eq!(
            manifest.stats.memory_count, 7,
            "manifest stats must reflect the whole batch"
        );
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
            composition: None,
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
            composition: None,
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

    /// `update_batch_with` is documented to match `update_with` per entry, and
    /// `update_with` resolves a prefix id. The batched read keys off the exact
    /// id, so a prefix used to fall straight through to "memory not found" —
    /// silently, and only for the batched callers.
    #[tokio::test]
    async fn update_batch_with_resolves_a_prefix_id() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("abcdef012345", Visibility::Shared))
            .await
            .unwrap();

        let (updated, errors) = store
            .update_batch_with(&["abcdef".to_string()], |m| {
                m.summary = "Reached by prefix".to_string();
                Ok(())
            })
            .await
            .unwrap();

        assert!(errors.is_empty(), "prefix must resolve, got {errors:?}");
        assert_eq!(
            updated,
            vec!["abcdef012345".to_string()],
            "the report names the canonical id, not the prefix the caller passed"
        );
        assert_eq!(
            store.get("abcdef012345").await.unwrap().summary,
            "Reached by prefix"
        );
    }

    /// Two spellings of one memory in one batch must not apply the closure
    /// twice — the closure is the caller's mutation, and running it on a
    /// stale copy of the same memory would let the second write erase the
    /// first.
    #[tokio::test]
    async fn update_batch_with_applies_a_duplicated_id_once() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("dupdupdup1234", Visibility::Shared))
            .await
            .unwrap();

        let mut calls = 0usize;
        let (updated, errors) = store
            .update_batch_with(&["dupdupdup1234".to_string(), "dupdup".to_string()], |m| {
                calls += 1;
                m.tags.push(format!("call-{calls}"));
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(calls, 1, "the closure must see each memory exactly once");
        assert_eq!(updated, vec!["dupdupdup1234".to_string()]);
        assert_eq!(
            errors.len(),
            1,
            "the second spelling has nothing left to do"
        );
        assert_eq!(
            store.get("dupdupdup1234").await.unwrap().tags,
            vec!["call-1"]
        );
    }

    /// An id that resolves and one that does not, in the same batch: the good
    /// one is applied and the bad one is reported under **the spelling the
    /// caller passed**, which is the only string it can act on.
    #[tokio::test]
    async fn update_batch_with_reports_an_unresolvable_id_by_the_caller_s_spelling() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&create_test_memory("realmemory0001", Visibility::Shared))
            .await
            .unwrap();

        let (updated, errors) = store
            .update_batch_with(&["realme".to_string(), "no-such-memory".to_string()], |m| {
                m.summary = "touched".to_string();
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(updated, vec!["realmemory0001".to_string()]);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "no-such-memory");
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
    async fn test_delete_if_predicate_true_deletes() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("delete-if-true-123", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let deleted = store
            .delete_if("delete-if-true-123", |m| m.summary == "Test summary")
            .await
            .unwrap();
        assert!(deleted, "predicate true must delete and report true");
        assert!(matches!(
            store.get("delete-if-true-123").await,
            Err(StorageError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn test_delete_if_predicate_false_keeps_memory() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("delete-if-false-123", Visibility::Shared);
        store.create(&memory).await.unwrap();

        let deleted = store
            .delete_if("delete-if-false-123", |_| false)
            .await
            .unwrap();
        assert!(!deleted, "predicate false must keep the memory");
        assert!(store.get("delete-if-false-123").await.is_ok());
    }

    /// The predicate sees the LATEST persisted state, not whatever the caller
    /// read before acquiring the lock — that re-read is the whole point.
    #[tokio::test]
    async fn test_delete_if_predicate_sees_fresh_state() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("delete-if-fresh-123", Visibility::Shared);
        store.create(&memory).await.unwrap();

        // Concurrent writer bumps criticality after the (hypothetical)
        // earlier unlocked read.
        let mut update = MemoryUpdate::new();
        update.criticality = Some(0.95);
        store.update("delete-if-fresh-123", update).await.unwrap();

        let deleted = store
            .delete_if("delete-if-fresh-123", |m| m.criticality < 0.9)
            .await
            .unwrap();
        assert!(!deleted, "predicate must judge the updated criticality");
        assert!(store.get("delete-if-fresh-123").await.is_ok());
    }

    #[tokio::test]
    async fn test_delete_if_missing_id_is_skip_not_error() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let deleted = store
            .delete_if("never-existed-123", |_| {
                panic!("predicate must not run for a missing memory")
            })
            .await
            .unwrap();
        assert!(!deleted, "missing id must be Ok(false), not an error");
    }

    /// Stale index entry (row in LanceDB, data file gone) is also a skip.
    #[tokio::test]
    async fn test_delete_if_stale_index_entry_is_skip() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let memory = create_test_memory("delete-if-stale-123", Visibility::Shared);
        store.create(&memory).await.unwrap();

        // Remove the .md file but leave the index row.
        let memories_dir = paths::memories_dir(temp_dir.path());
        for entry in std::fs::read_dir(&memories_dir).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }

        let deleted = store
            .delete_if("delete-if-stale-123", |_| true)
            .await
            .unwrap();
        assert!(!deleted, "stale index entry must be skipped, not an error");
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

    /// The freshness-guarded upsert used by detached ingest tasks: a stale
    /// snapshot (older updated_at) or a deleted memory must not write chunks;
    /// the current snapshot must.
    #[tokio::test]
    async fn test_upsert_chunks_if_current_guards_stale_and_deleted() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let memory = create_test_memory("guarded-chunks", Visibility::Shared);
        store.create(&memory).await.unwrap();

        // Current snapshot: written.
        let written = store
            .upsert_chunks_if_current(
                "guarded-chunks",
                vec![vec![0.1f32; 384]],
                memory.updated_at,
                None,
            )
            .await
            .unwrap();
        assert!(written);

        // The memory moves on (updated_at bumps)...
        let newer = store
            .update_with("guarded-chunks", |m| {
                m.summary = "v2".to_string();
                Ok(())
            })
            .await
            .unwrap();
        let written = store
            .upsert_chunks_if_current(
                "guarded-chunks",
                vec![vec![0.9f32; 384]],
                newer.updated_at,
                None,
            )
            .await
            .unwrap();
        assert!(written);

        // ...so the OLD snapshot's late-arriving vectors are refused.
        let written = store
            .upsert_chunks_if_current(
                "guarded-chunks",
                vec![vec![0.1f32; 384]],
                memory.updated_at,
                None,
            )
            .await
            .unwrap();
        assert!(!written, "stale snapshot must not overwrite newer vectors");
        let chunks = store.export_chunks("guarded-chunks").await.unwrap();
        assert!(
            (chunks[0][0] - 0.9).abs() < f32::EPSILON,
            "newer vectors survive"
        );

        // Deleted memory: a late embed must not re-insert orphan chunks.
        store.delete("guarded-chunks").await.unwrap();
        let written = store
            .upsert_chunks_if_current(
                "guarded-chunks",
                vec![vec![0.5f32; 384]],
                newer.updated_at,
                None,
            )
            .await
            .unwrap();
        assert!(!written, "deleted memory must not get orphan chunks");
        assert!(store
            .list_chunk_memory_ids()
            .await
            .unwrap()
            .iter()
            .all(|id| id != "guarded-chunks"));
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

    // ---- group stores (generalization of the global store) ----

    #[tokio::test]
    async fn test_group_init_open_roundtrip() {
        // Group stores live under `<ENGRAMDB_DATA_DIR>/groups/<id>/`, which the
        // ctor test-isolation arm redirects to a per-process temp dir — no
        // global-test lock needed (unlike the global store, whose fixed path is
        // shared across the process). A distinct group id per test name keeps
        // parallel group tests from colliding even within the process.
        let group_id = paths::compute_group_id("test-group-roundtrip");
        let store = MemoryStore::init_group(&group_id).await.unwrap();
        assert!(store.is_group());
        assert!(!store.is_global());
        assert_eq!(store.project_id, group_id);

        let group_dir = paths::group_store_dir(&group_id).unwrap();
        assert!(group_dir.join(".engramdb").exists());
        assert!(group_dir.join(".engramdb/memories").exists());
        assert!(group_dir.join(".engramdb/manifest.toml").exists());
        assert!(paths::group_lancedb_dir(&group_id).unwrap().exists());

        let memory = create_test_memory("group-mem-001", Visibility::Shared);
        store.create(&memory).await.unwrap();

        // Reopen via `open_group` (existing dir → no re-init) and confirm the
        // memory persisted and the store still reports as a group.
        let reopened = MemoryStore::open_group(&group_id).await.unwrap();
        assert!(reopened.is_group());
        let retrieved = reopened.get("group-mem-001").await.unwrap();
        assert_eq!(retrieved.id, "group-mem-001");
    }

    #[tokio::test]
    async fn test_group_is_not_group_for_regular_store() {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        assert!(!store.is_group());
    }

    // ===================================================================
    // Finding #1 (Critical): `create` must not orphan files across
    // visibility directories.
    //
    // Scenario: a memory with ID X is created as Shared. Later the *same* ID
    // is created again as Personal (e.g. worktree consolidation re-runs, or a
    // personal/shared re-create). The LanceDB row is keyed on ID so it simply
    // flips to Personal — but the old Shared `.md` file was never removed,
    // leaving TWO files for one ID across two directories and diverging disk
    // from the index.
    // ===================================================================

    async fn count_files(dir: &Path) -> usize {
        count_md_files(dir).await
    }

    #[tokio::test]
    async fn create_basic_roundtrips_without_orphans() {
        // POSITIVE: a plain create leaves exactly one file and one index row.
        let temp = TempDir::new().unwrap();
        let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let id = "0190aaaa-bbbb-7ccc-8ddd-000000000001";
        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();

        let shared = paths::memories_dir(&store.project_dir);
        let personal = paths::personal_memories_dir(&store.project_id).unwrap();
        assert_eq!(count_files(&shared).await, 1);
        assert_eq!(count_files(&personal).await, 0);
        assert_eq!(store.count().await.unwrap(), 1);
        assert_eq!(store.get(id).await.unwrap().visibility, Visibility::Shared);
    }

    #[tokio::test]
    async fn create_same_id_different_visibility_does_not_orphan() {
        // NEGATIVE (red before fix): re-creating an existing ID under a new
        // visibility must remove the old-visibility file, not orphan it.
        let temp = TempDir::new().unwrap();
        let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let id = "0190aaaa-bbbb-7ccc-8ddd-000000000002";

        store
            .create(&create_test_memory(id, Visibility::Shared))
            .await
            .unwrap();
        // Re-create the SAME id as Personal.
        store
            .create(&create_test_memory(id, Visibility::Personal))
            .await
            .unwrap();

        let shared = paths::memories_dir(&store.project_dir);
        let personal = paths::personal_memories_dir(&store.project_id).unwrap();

        // Exactly one file total, in the new (Personal) dir; no Shared orphan.
        assert_eq!(
            count_files(&shared).await,
            0,
            "old Shared file must be removed, not orphaned"
        );
        assert_eq!(count_files(&personal).await, 1);
        // Index agrees: one row, resolving to the new visibility.
        assert_eq!(store.count().await.unwrap(), 1);
        assert_eq!(
            store.get(id).await.unwrap().visibility,
            Visibility::Personal
        );
    }

    // ===================================================================
    // Finding #14 (Medium): tied-mtime duplicate resolution must be
    // deterministic (not directory-iteration-order dependent).
    // ===================================================================

    #[test]
    fn prefers_newer_breaks_mtime_ties_deterministically() {
        use std::time::{Duration, UNIX_EPOCH};
        let t = UNIX_EPOCH + Duration::from_secs(1000);
        let a = Path::new("aaa_id.md");
        let b = Path::new("bbb_id.md");

        // POSITIVE: strictly newer mtime always wins regardless of path.
        let newer = t + Duration::from_secs(1);
        assert!(prefers_newer((newer, a), (t, b)));
        assert!(!prefers_newer((t, b), (newer, a)));

        // NEGATIVE (red before fix): on an mtime TIE the result must be
        // order-independent. Lexicographically greater path ("bbb" > "aaa")
        // wins no matter which side it is on. Before the fix (`a.0 >= b.0`),
        // the first argument always won on ties → order-dependent.
        assert!(prefers_newer((t, b), (t, a)), "bbb should win the tie");
        assert!(
            !prefers_newer((t, a), (t, b)),
            "aaa must lose the tie regardless of argument order"
        );
    }

    // ===================================================================
    // Finding #5 (High): staleness warning must be suppressed under a
    // shared-ID checkout conflict (where lance_count > md_count is normal).
    // ===================================================================

    #[test]
    fn staleness_message_reports_real_drift_without_conflict() {
        // POSITIVE: genuine drift (no conflict) still warns.
        assert!(staleness_message(3, 5, false).is_some());
        // POSITIVE: in-sync counts never warn.
        assert!(staleness_message(5, 5, false).is_none());
    }

    #[test]
    fn staleness_message_suppressed_under_checkout_conflict() {
        // NEGATIVE (red before fix): under a conflict, a count mismatch is
        // expected (the index holds another checkout's rows) and must NOT warn.
        assert!(
            staleness_message(3, 5, true).is_none(),
            "must not warn about staleness during a checkout conflict"
        );
    }

    // ===================================================================
    // Finding #15 (Medium): get_batch tolerates a corrupt file (skips it,
    // returns the valid ones) rather than failing the whole batch.
    // ===================================================================

    #[tokio::test]
    async fn get_batch_skips_corrupt_file_returns_valid() {
        let temp = TempDir::new().unwrap();
        let store = MemoryStore::init(temp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let good = "0190aaaa-bbbb-7ccc-8ddd-000000000010";
        let bad = "0190aaaa-bbbb-7ccc-8ddd-000000000011";
        store
            .create(&create_test_memory(good, Visibility::Shared))
            .await
            .unwrap();
        store
            .create(&create_test_memory(bad, Visibility::Shared))
            .await
            .unwrap();

        // Corrupt the second file on disk (truncate to garbage that won't parse).
        let shared = paths::memories_dir(&store.project_dir);
        for path in find_memory_files(&shared, bad).await.unwrap() {
            async_fs::write(&path, "this is not a valid memory file")
                .await
                .unwrap();
        }

        let got = store.get_batch(&[good, bad]).await.unwrap();
        let ids: Vec<&str> = got.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec![good],
            "valid memory returned, corrupt one skipped"
        );
    }
}
