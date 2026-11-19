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
    IndexDigest, IndexEntry, IndexFilterable, IndexForFiltering, IndexSummary, LanceIndex,
    VectorMatch,
};
use super::registry::RegistryBackend;
use super::{manifest, memory_file, paths, project_id, write_lock};
use crate::config::{load_config, load_config_or_default};
use crate::digest::FileDigest;
use crate::staleness::StalenessFindings;
use chrono::{DateTime, Utc};
use engram_types::config::{IndexConfig, StalenessCheck};
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
        match memory_file::parse_memory_file_from(content, path) {
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

/// Where a store's on-disk schema stands against this binary's
/// ([`MemoryStore::schema_state`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SchemaState {
    /// The `schema_version` recorded in `manifest.toml`; `None` when the
    /// manifest is missing or unreadable.
    pub stored: Option<String>,
    /// This binary's [`manifest::CURRENT_SCHEMA_VERSION`].
    pub current: &'static str,
    /// The next ordinary open would migrate (schema behind, or stems written
    /// by another normalizer).
    pub needs_migration: bool,
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

/// Failpoint: make the chunk-presence probe of a batched update fail.
#[cfg(test)]
static FAIL_CHUNK_PROBE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How many schema migrations actually rebuilt the index in this process
/// (the re-check under the lock passed), for `concurrent_opens_migrate_once`.
#[cfg(test)]
static MIGRATION_REBUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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
        paths::ensure_private_parent(&lance_path)?;
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
            .map_err(StorageError::index("init"))?;

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

        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        Self::open_writable_at(
            global_dir,
            paths::GLOBAL_PROJECT_ID.to_string(),
            &paths::global_lancedb_dir()?,
            &config,
            "global init",
            Some(_lock),
        )
        .await
    }

    /// Open the global memory store, creating it if necessary.
    pub async fn open_global() -> Result<Self> {
        let global_dir = paths::global_store_dir()?;
        let engramdb_dir = paths::project_dir(&global_dir);

        if !engramdb_dir.exists() {
            return Self::init_global().await;
        }

        let config_path = engramdb_dir.join("config.toml");
        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        Self::open_writable_at(
            global_dir,
            paths::GLOBAL_PROJECT_ID.to_string(),
            &paths::global_lancedb_dir()?,
            &config,
            "global open",
            None,
        )
        .await
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

        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        Self::open_writable_at(
            group_dir,
            group_id.to_string(),
            &paths::group_lancedb_dir(group_id)?,
            &config,
            "group init",
            Some(_lock),
        )
        .await
    }

    /// Open a named group memory store, creating it if necessary.
    pub async fn open_group(group_id: &str) -> Result<Self> {
        let group_dir = paths::group_store_dir(group_id)?;
        let engramdb_dir = paths::project_dir(&group_dir);

        if !engramdb_dir.exists() {
            return Self::init_group(group_id).await;
        }

        let config_path = engramdb_dir.join("config.toml");
        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        Self::open_writable_at(
            group_dir,
            group_id.to_string(),
            &paths::group_lancedb_dir(group_id)?,
            &config,
            "group open",
            None,
        )
        .await
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

        // Open (or create) global LanceDB. Every open (the hot path for all
        // CLI commands and the MCP server) must migrate a store that predates
        // the current schema, exactly like the init/global paths — otherwise
        // reads that project the new columns fail and writes hit a schema
        // mismatch on an un-upgraded table. `open_writable_at` does both.
        let lance_path = paths::lancedb_dir(&project_id)?;
        Self::open_writable_at(
            dir.to_path_buf(),
            project_id,
            &lance_path,
            &config,
            "open",
            None,
        )
        .await
    }

    /// The tail every writable open but [`Self::init`] shares: create the
    /// LanceDB directory (owner-only parent), open the index at the
    /// configured width, then migrate the schema if it is behind.
    ///
    /// `lock` is the store's write lock when the caller holds it (the lazy
    /// creation paths). It is released after the index is open and before
    /// the migration, because the migration's reindex re-acquires it and a
    /// second acquire in this process would deadlock.
    ///
    /// The config is the caller's: [`Self::open`] and the shared-store opens
    /// load it with different fallbacks and warnings. `init` is not routed
    /// here because its registry write sits between the index open and the
    /// migration, and moving the index open after it would change what a
    /// failed init leaves behind.
    async fn open_writable_at(
        project_dir: PathBuf,
        project_id: String,
        lance_path: &Path,
        config: &engram_types::EngramConfig,
        what: &'static str,
        lock: Option<write_lock::WriteLockGuard>,
    ) -> Result<Self> {
        paths::ensure_private_parent(lance_path)?;
        async_fs::create_dir_all(lance_path).await?;
        let lance_index = LanceIndex::new(lance_path, config.embeddings.dimensions)
            .await
            .map_err(StorageError::index(what))?;
        let store = Self {
            project_dir,
            project_id,
            lance_index,
        };
        drop(lock);
        store.migrate_schema_if_needed().await?;
        Ok(store)
    }

    /// Open an existing project store for reading only, with no side effects.
    ///
    /// [`Self::open`] is not side-effect free, and must not be for its
    /// callers: it backfills `.engramdb/.gitignore`, creates the LanceDB
    /// directory and tables when missing, and runs the schema migration (a
    /// full reindex under the write lock). A long-running viewer (the
    /// dashboard) opening every store on the machine must do none of that —
    /// looking at a store is not a reason to rewrite it, and a migration run
    /// by a viewer would race the CLI the user is actually typing into. So
    /// this path writes nothing, creates no directory, table or lock, and a
    /// store behind the current schema is *reported* through
    /// [`Self::schema_state`], never fixed: reads that project a column the
    /// old table lacks fail on their own and say so.
    ///
    /// `NotInitialized` when `dir` has no `.engramdb/`; `Validation` naming
    /// the remedy when the store has never been indexed on this machine (a
    /// fresh clone with committed memories and no `lancedb/` yet).
    ///
    /// The returned value is still a full `MemoryStore`, so the type does not
    /// stop a caller from writing through it: a read-only consumer should
    /// wrap it in a type that exposes reads only (the dashboard does).
    pub async fn open_read_only(dir: &Path) -> Result<Self> {
        let engramdb_dir = paths::project_dir(dir);
        if !engramdb_dir.is_dir() {
            return Err(StorageError::NotInitialized);
        }
        let project_id = project_id::compute_project_id(dir);
        let lance_path = paths::lancedb_dir(&project_id)?;
        Self::open_read_only_at(dir.to_path_buf(), project_id, &lance_path).await
    }

    /// [`Self::open_read_only`] for the global store. `NotInitialized` when
    /// it was never created (unlike [`Self::open_global`], which creates it).
    pub async fn open_global_read_only() -> Result<Self> {
        let global_dir = paths::global_store_dir()?;
        if !paths::project_dir(&global_dir).is_dir() {
            return Err(StorageError::NotInitialized);
        }
        let lance_path = paths::global_lancedb_dir()?;
        Self::open_read_only_at(
            global_dir,
            paths::GLOBAL_PROJECT_ID.to_string(),
            &lance_path,
        )
        .await
    }

    /// [`Self::open_read_only`] for a group store. `NotInitialized` when it
    /// was never created (unlike [`Self::open_group`], which creates it).
    pub async fn open_group_read_only(group_id: &str) -> Result<Self> {
        let group_dir = paths::group_store_dir(group_id)?;
        if !paths::project_dir(&group_dir).is_dir() {
            return Err(StorageError::NotInitialized);
        }
        let lance_path = paths::group_lancedb_dir(group_id)?;
        Self::open_read_only_at(group_dir, group_id.to_string(), &lance_path).await
    }

    async fn open_read_only_at(
        project_dir: PathBuf,
        project_id: String,
        lance_path: &Path,
    ) -> Result<Self> {
        let config_path = paths::project_dir(&project_dir).join("config.toml");
        let config: engram_types::EngramConfig = load_config_or_default(&config_path).await;
        let lance_index = LanceIndex::open_existing(lance_path, config.embeddings.dimensions)
            .await
            .map_err(StorageError::index("open"))?
            .ok_or_else(|| {
                StorageError::Validation(format!(
                    "{} has not been indexed on this machine yet: run any engramdb \
                     command there (or `engramdb reindex`) to build its index",
                    project_dir.display()
                ))
            })?;
        Ok(Self {
            project_dir,
            project_id,
            lance_index,
        })
    }

    /// Where this store's on-disk schema stands against this binary's, read
    /// from `manifest.toml` without changing anything — what
    /// [`Self::migrate_schema_if_needed`] would act on, for a caller that must
    /// only report it ([`Self::open_read_only`]).
    pub async fn schema_state(&self) -> SchemaState {
        Self::schema_state_at(&self.project_dir).await
    }

    /// [`Self::schema_state`] for the store rooted at `project_dir`, without
    /// opening it: a listing of every store on the machine reads this per row
    /// and must not connect to each LanceDB to do so.
    pub async fn schema_state_at(project_dir: &Path) -> SchemaState {
        let manifest_path = paths::project_dir(project_dir).join("manifest.toml");
        let loaded = manifest::load_manifest(&manifest_path).await.ok();
        let stored = loaded.as_ref().map(|m| m.schema_version.clone());
        let schema_stale = stored
            .as_deref()
            .is_none_or(|v| !manifest::schema_version_is_current(v));
        let normalizer_stale = loaded.as_ref().and_then(|m| m.normalizer.as_deref())
            != Some(engram_types::NORMALIZER_STAMP);
        SchemaState {
            stored,
            current: manifest::CURRENT_SCHEMA_VERSION,
            needs_migration: schema_stale || normalizer_stale,
        }
    }

    /// Create a new memory.
    pub async fn create(&self, memory: &Memory) -> Result<String> {
        self.create_inner(memory, None).await
    }

    /// [`Self::create`] for a memory whose vectors are already computed: the
    /// file, then the chunks, then the index row with `has_embedding` already
    /// true — one memories-table commit instead of the two that `create`
    /// followed by a chunk upsert costs (the row, then its flag).
    ///
    /// `embed_sha256` describes the text `chunks` were embedded from, as for
    /// [`Self::upsert_chunks_if`]. Empty `chunks` is plain `create`.
    ///
    /// A crash between the chunks and the row leaves a file with vectors and
    /// no row: `doctor` reports the missing row and `reindex` rebuilds it,
    /// deriving `has_embedding` from chunk presence, so the vectors are kept.
    pub async fn create_with_chunks(
        &self,
        memory: &Memory,
        chunks: Vec<Vec<f32>>,
        embed_sha256: Option<String>,
    ) -> Result<String> {
        if chunks.is_empty() {
            return self.create_inner(memory, None).await;
        }
        self.create_inner(memory, Some((chunks, embed_sha256)))
            .await
    }

    async fn create_inner(
        &self,
        memory: &Memory,
        chunks: Option<(Vec<Vec<f32>>, Option<String>)>,
    ) -> Result<String> {
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
        //
        // An equality probe, not the prefix `LIKE`: the question is about
        // this one whole id.
        let id_preexisting = self
            .lance_index
            .id_exists(&memory.id)
            .await
            .map_err(StorageError::index("id probe"))?;
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
        // ranking (R3). A brand-new id has no row and therefore no chunks
        // (chunks are only ever written for an existing memory), so the
        // chunk probe is skipped for it: one index read per fresh create.
        let mut entry = IndexEntry::for_file(memory, &digest);
        entry.has_embedding = match chunks {
            Some((chunks, embed_sha256)) => {
                // Chunks before the row: the row is then born with the flag
                // set, and a crash in between is the recoverable direction
                // (see `create_with_chunks`).
                self.lance_index
                    .write_chunks(&memory.id, chunks, embed_sha256.as_deref())
                    .await
                    .map_err(StorageError::index("upsert_chunks"))?;
                true
            }
            None if id_preexisting => self
                .lance_index
                .has_chunks(&memory.id)
                .await
                .map_err(StorageError::index("has_chunks"))?,
            None => false,
        };
        self.lance_index
            .upsert(&entry)
            .await
            .map_err(StorageError::index("upsert"))?;
        self.sync_edges_locked(std::slice::from_ref(memory)).await?;

        // Update manifest stats. A fresh id adds exactly one row, which the
        // cheap path can verify; a re-create may have changed scopes on an
        // existing row, which only the scan can see.
        if id_preexisting {
            self.update_manifest_stats().await?;
        } else {
            self.update_manifest_stats_after(1, &memory.logical).await?;
        }

        Ok(memory.id.clone())
    }

    /// [`Self::create`] for many memories at once.
    ///
    /// Takes the per-project write lock **once**, writes every file, then
    /// performs a single ID probe, a single chunk-presence probe, a single
    /// index upsert and a single manifest refresh for the whole set.
    ///
    /// The per-memory version cost *four* separate O(store size) operations
    /// each — a prefix `LIKE` scan, the `has_chunks` query, the
    /// `merge_insert` commit, and the full-column scan behind
    /// `update_manifest_stats` — so creating W memories into a store of M was
    /// quadratic. Measured then: one `create` cost 24 ms into an empty store
    /// and 582 ms into a 1000-memory one. A single `create` of a fresh id is
    /// now one equality probe, one commit and a metadata row count, but it is
    /// still one commit (and one lock) per memory, which this avoids.
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
            .map_err(StorageError::index("memory_ids_with_chunks"))?;

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
            .map_err(StorageError::index("upsert_batch"))?;
        self.sync_edges_locked(memories).await?;

        self.update_manifest_stats().await?;

        Ok(memories.iter().map(|m| m.id.clone()).collect())
    }

    /// Get a memory by ID (supports prefix matching).
    ///
    /// When the same id has a file in both the shared and the personal
    /// directory, [`cross_dir_winner`] decides which one is the memory — the
    /// same rule every other reader and the index use.
    pub async fn get(&self, id: &str) -> Result<Memory> {
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        let shared = locate_memory_file(&shared_dir, id).await?;
        let personal = match locate_memory_file(&personal_dir, id).await {
            Ok(found) => found,
            // A shared hit used to answer without looking at the personal
            // directory at all; a problem there must not fail it now.
            Err(_) if shared.is_some() => None,
            Err(e) => return Err(e),
        };

        let candidates: Vec<&PathBuf> = match (&shared, &personal) {
            (Some(s), Some(p)) if stem_id(s) == stem_id(p) => match cross_dir_winner(s, p).await {
                CrossDirWinner::Shared => vec![s, p],
                CrossDirWinner::Personal => vec![p, s],
            },
            // A prefix naming different ids in the two directories: shared
            // first, as before.
            (Some(s), Some(p)) => vec![s, p],
            (Some(s), None) => vec![s],
            (None, Some(p)) => vec![p],
            (None, None) => Vec::new(),
        };
        // A file removed between the scan and the read (a concurrent
        // rename-update or delete) falls through to the next candidate.
        for path in candidates {
            match Self::read_found_file(path, id).await {
                Err(StorageError::NotFound(_)) => continue,
                other => return other,
            }
        }
        Err(StorageError::NotFound(id.to_string()))
    }

    /// Every id that has a file in both the shared and the personal
    /// directory, with the copy that wins. Read-only; for `doctor`.
    pub async fn cross_dir_duplicates(&self) -> Result<Vec<CrossDirDuplicate>> {
        let shared = scan_dir_to_stats(&paths::memories_dir(&self.project_dir)).await;
        let personal = scan_dir_to_stats(&paths::personal_memories_dir(&self.project_id)?).await;
        let mut duplicates = merge_cross_dir(shared, personal).await.duplicates;
        duplicates.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(duplicates)
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
        if ids.is_empty() {
            // No directory scans for nothing to read.
            return Ok(Vec::new());
        }
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
        let mut to_read: Vec<(String, PathBuf)> = Vec::with_capacity(ids.len());
        for id in ids {
            let id_str = id.as_ref();
            let path = match (shared_map.get(id_str), personal_map.get(id_str)) {
                // In both directories: the same winner `get` and the index pick.
                (Some(s), Some(p)) => match cross_dir_winner(s, p).await {
                    CrossDirWinner::Shared => s,
                    CrossDirWinner::Personal => p,
                },
                (Some(path), None) | (None, Some(path)) => path,
                (None, None) => continue,
            };
            to_read.push((id_str.to_owned(), path.clone()));
        }
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

    /// Read + parse a memory file found by a directory scan.
    ///
    /// Reads are lock-free, so a concurrent rename-update (title/visibility
    /// change writes the new file then unlinks the old) or delete can remove
    /// the scanned path before we read it. That is the documented `NotFound`
    /// condition — `get` relies on it to fall through to the personal dir —
    /// not an opaque I/O failure.
    async fn read_found_file(path: &Path, id: &str) -> Result<Memory> {
        match async_fs::read_to_string(path).await {
            Ok(content) => memory_file::parse_memory_file_from(&content, path),
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
        let mut written: Vec<Memory> = Vec::new();
        let mut probe_error: Option<StorageError> = None;

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
            // The file is already written. Returning on a failed probe left
            // this file — and every file written earlier in the batch —
            // without an index row. Record the row as un-embedded (a reindex
            // derives the flag from chunk presence again), finish the batch's
            // index writes, and report the error afterwards.
            entry.has_embedding = match self.probe_has_chunks(&memory.id).await {
                Ok(has) => has,
                Err(e) => {
                    probe_error.get_or_insert(e);
                    false
                }
            };
            entries.push(entry);
            written.push(memory);
            updated.push(canonical);
        }

        if !entries.is_empty() {
            self.lance_index
                .upsert_batch(&entries)
                .await
                .map_err(StorageError::index("upsert_batch"))?;
            self.sync_edges_locked(&written).await?;
            // One refresh for the whole batch instead of one per memory.
            self.update_manifest_stats().await?;
        }
        if let Some(e) = probe_error {
            return Err(e);
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
        // that to the newest mtime (see `locate_memory_file`) and the next
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
        // An explicit write to this id also heals a cross-directory
        // duplicate: a copy of the same id in the OTHER visibility directory
        // (a `git checkout` restoring a tracked shared file after the memory
        // went personal, a crashed visibility flip) would otherwise keep
        // competing with the file just written. `create` sweeps both
        // directories the same way.
        for dir in [
            self.get_memories_dir(&Visibility::Shared)?,
            self.get_memories_dir(&Visibility::Personal)?,
        ] {
            if dir == old_dir {
                continue;
            }
            for other in find_memory_files(&dir, &memory.id).await? {
                if other == file_path {
                    continue;
                }
                match async_fs::remove_file(&other).await {
                    Ok(()) => tracing::warn!(
                        "removed {}: a second copy of memory {} in the other visibility \
                         directory, replaced by this update",
                        other.display(),
                        memory.id
                    ),
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
        entry.has_embedding = self
            .lance_index
            .has_chunks(&memory.id)
            .await
            .map_err(StorageError::index("has_chunks"))?;
        self.lance_index
            .upsert(&entry)
            .await
            .map_err(StorageError::index("upsert"))?;
        self.sync_edges_locked(std::slice::from_ref(memory)).await?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(())
    }

    /// Replace the `edges` rows owned by `memories` with what their files now
    /// declare (`edge_index` module docs). Callers MUST hold the write lock.
    ///
    /// Runs on every write rather than only on writes that touched an edge
    /// field: `src_invalidated` follows `invalidated_at`, and an ordinary
    /// update can change that too. A memory that had no edges and still has
    /// none costs one read and no commit (`replace_owned`).
    ///
    /// **Never fails the write.** The table is derived and the `.md` file is
    /// already on disk when this runs: an error here used to fail a create
    /// whose file existed, and the retry then created the memory twice. A
    /// failure is logged; the next `reindex` rebuilds the table, and `doctor`'s
    /// "Memory edges" check reports a table that disagrees with the files.
    async fn sync_edges_locked(&self, memories: &[Memory]) -> Result<()> {
        let owners: Vec<String> = memories.iter().map(|m| m.id.clone()).collect();
        let rows: Vec<crate::edge_index::EdgeRow> = memories
            .iter()
            .flat_map(crate::edge_index::edges_of)
            .collect();
        if let Err(e) =
            crate::edge_index::replace_owned(self.lance_index.connection(), &owners, &rows).await
        {
            tracing::warn!(
                "edges table not updated for {} memor{} (the files are written; run \
                 `engramdb reindex` to rebuild it): {e:#}",
                owners.len(),
                if owners.len() == 1 { "y" } else { "ies" }
            );
        }
        Ok(())
    }

    /// Every edge in this store, deduplicated (`edge_index::dedupe`). Empty
    /// when the table does not exist yet. Read-only: creates nothing, so it
    /// is safe on a store from [`Self::open_read_only`].
    pub async fn list_edges(&self) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_all(self.lance_index.connection())
            .await
            .map(crate::edge_index::dedupe)
            .map_err(StorageError::index("edges read"))
    }

    /// Every row of the edges table, **not** deduplicated: one per declaring
    /// file. What the personal-id leak check needs — dedupe folds a
    /// `superseded_by` mirror into its `supersedes` row, and the mirror is
    /// the one written into the *other* memory's file.
    pub async fn list_edge_rows(&self) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_all(self.lance_index.connection())
            .await
            .map_err(StorageError::index("edges read"))
    }

    /// The rows with `id` at either end, not deduplicated (see
    /// [`Self::list_edge_rows`]).
    pub async fn list_edge_rows_touching(
        &self,
        id: &str,
    ) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_touching(self.lance_index.connection(), id)
            .await
            .map_err(StorageError::index("edges read"))
    }

    /// The edges touching any of `ids` (full ids), deduplicated, in one
    /// query (`edge_index::load_touching_any`). Read-only.
    pub async fn edges_touching_any(
        &self,
        ids: &[String],
    ) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_touching_any(self.lance_index.connection(), ids)
            .await
            .map(crate::edge_index::dedupe)
            .map_err(StorageError::index("edges read"))
    }

    /// Index rows for exactly `ids` (full ids; unknown ids are simply
    /// absent), in `IN` lists of at most 500. Read-only. The rows carry no
    /// `keyword_stems` (`FilterProjection::NoStems`).
    pub async fn list_for_filtering_by_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<IndexForFiltering>> {
        use lancedb::expr::{col, is_in, lit};
        let mut out = Vec::new();
        for chunk in ids.chunks(500) {
            let expr = is_in(col("id"), chunk.iter().map(lit).collect());
            // No caller of this reads the stems.
            out.extend(
                self.lance_index
                    .list_for_filtering_where_expr_projected(
                        expr,
                        crate::lance_index::FilterProjection::NoStems,
                    )
                    .await
                    .map_err(StorageError::index("list_for_filtering_by_ids"))?,
            );
        }
        Ok(out)
    }

    /// The edges with `id` (a full id) at either end, deduplicated.
    pub async fn edges_touching(&self, id: &str) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_touching(self.lance_index.connection(), id)
            .await
            .map(crate::edge_index::dedupe)
            .map_err(StorageError::index("edges read"))
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
            self.delete_files_in_both_dirs(id).await?;
        }

        self.lance_index
            .delete_batch(&to_delete)
            .await
            .map_err(StorageError::index("delete_batch"))?;
        self.lance_index
            .delete_chunks_batch(&to_delete)
            .await
            .map_err(StorageError::index("delete_chunks_batch"))?;
        self.delete_edges_locked(&to_delete).await?;

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
        self.delete_files_in_both_dirs(full_id).await?;

        // Delete from LanceDB (metadata + chunks)
        self.lance_index
            .delete(full_id)
            .await
            .map_err(StorageError::index("delete"))?;
        self.lance_index
            .delete_chunks(full_id)
            .await
            .map_err(StorageError::index("delete_chunks"))?;
        self.delete_edges_locked(&[full_id.to_string()]).await?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(())
    }

    /// Drop the edge rows the deleted memories' files declared. Edges *into*
    /// them, declared by other files, stay: a dangling edge is legal, and the
    /// readers report it rather than this path silently rewriting other
    /// memories' files.
    ///
    /// Like [`Self::sync_edges_locked`], a failure is logged, never returned:
    /// the files are already gone.
    async fn delete_edges_locked(&self, ids: &[String]) -> Result<()> {
        if let Err(e) = crate::edge_index::delete_owned(self.lance_index.connection(), ids).await {
            tracing::warn!(
                "edges of {} deleted memor{} left in the edges table (run `engramdb reindex`): \
                 {e:#}",
                ids.len(),
                if ids.len() == 1 { "y" } else { "ies" }
            );
        }
        Ok(())
    }

    /// List all memories with filterable/displayable columns (12 of 14).
    ///
    /// Omits `provenance_source` and `confidence` which no caller reads.
    pub async fn list_filterable(&self) -> Result<Vec<IndexFilterable>> {
        self.lance_index
            .list_filterable()
            .await
            .map_err(StorageError::index("list_filterable"))
    }

    /// List minimal columns for index-level filtering (6 of 14).
    ///
    /// Returns only the fields needed by `apply_index_filters` plus `id`.
    /// Use this for the retrieval pipeline where full metadata is loaded
    /// later via `get()` for surviving entries only.
    pub async fn list_for_filtering(&self) -> Result<Vec<IndexForFiltering>> {
        self.lance_index
            .list_for_filtering()
            .await
            .map_err(StorageError::index("list_for_filtering"))
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
            .map_err(StorageError::index("list_for_filtering_where"))
    }

    /// [`Self::list_for_filtering_where`] with a chosen projection; a caller
    /// that never reads `keyword_stems` passes
    /// [`crate::lance_index::FilterProjection::NoStems`].
    pub async fn list_for_filtering_where_projected(
        &self,
        predicate: Option<String>,
        proj: crate::lance_index::FilterProjection,
    ) -> Result<Vec<IndexForFiltering>> {
        self.lance_index
            .list_for_filtering_where_projected(predicate, proj)
            .await
            .map_err(StorageError::index("list_for_filtering_where"))
    }

    /// The rows that watch at least one path (the PostToolUse hook's read).
    /// See [`LanceIndex::list_watched`].
    pub async fn list_watched(&self) -> Result<Vec<crate::lance_index::WatchedEntry>> {
        self.lance_index
            .list_watched()
            .await
            .map_err(StorageError::index("list_watched"))
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
            .map_err(StorageError::index("list_for_filtering_where_expr"))
    }

    /// Build the memories-table scalar indexes. See
    /// [`LanceIndex::create_scalar_indexes`]. Best-effort: a scalar index only
    /// changes predicate speed, never which rows match.
    pub async fn create_scalar_indexes(&self) -> Result<Vec<String>> {
        self.lance_index
            .create_scalar_indexes()
            .await
            .map_err(StorageError::index("create_scalar_indexes"))
    }

    /// Build the FM substring index on `tags`. See
    /// [`LanceIndex::create_tag_search_index`].
    pub async fn create_tag_search_index(&self) -> Result<bool> {
        self.lance_index
            .create_tag_search_index()
            .await
            .map_err(StorageError::index("create_tag_search_index"))
    }

    /// List lightweight metadata summaries for all memories (7 columns).
    pub async fn list_summary(&self) -> Result<Vec<IndexSummary>> {
        self.lance_index
            .list_summary()
            .await
            .map_err(StorageError::index("list_summary"))
    }

    /// Every memory id that has a **file on disk**, plus the bytes-on-disk
    /// length of each.
    ///
    /// Deliberately distinct from [`Self::list_ids`], which reads the *index*.
    /// Anything comparing the two sides — "which files are not indexed?",
    /// "how many memories are actually on disk?" — must read this side from the
    /// filesystem, or it is comparing the index against itself and can only
    /// ever report agreement.
    ///
    /// Duplicate ids resolve newest-wins, matching what a reader would get.
    pub async fn list_file_ids(&self) -> Result<Vec<String>> {
        let shared = scan_dir_to_stats(&paths::memories_dir(&self.project_dir)).await;
        let personal = scan_dir_to_stats(&paths::personal_memories_dir(&self.project_id)?).await;
        let mut ids: Vec<String> = shared
            .by_id
            .into_keys()
            .chain(personal.by_id.into_keys())
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// List all memory IDs **from the index**.
    ///
    /// Not the files — see [`Self::list_file_ids`] when the question is what
    /// exists on disk.
    pub async fn list_ids(&self) -> Result<Vec<String>> {
        self.lance_index
            .list_ids()
            .await
            .map_err(StorageError::index("list_ids"))
    }

    /// List every memory in this store that cites a source session
    /// (schema v0.7.0). See [`LanceIndex::list_source_session_links`].
    pub async fn list_source_session_links(
        &self,
    ) -> Result<Vec<super::lance_index::SourceSessionLink>> {
        self.lance_index
            .list_source_session_links()
            .await
            .map_err(StorageError::index("source-session scan"))
    }

    /// How many uncompacted fragments this store's tables carry.
    ///
    /// Cheap enough for a command hot path (fragment metadata only). Used to
    /// decide whether compaction is due *before* the time-based maintenance
    /// window expires — a write-heavy session degrades scans several-fold
    /// within a single window, see `docs/contributors/query-latency-profile.md`.
    pub async fn fragmentation(&self) -> Result<super::lance_index::Fragmentation> {
        self.lance_index
            .fragmentation()
            .await
            .map_err(StorageError::index("stats"))
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
            .map_err(StorageError::index("optimize"))
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
            .map_err(StorageError::index("count_by_type"))
    }

    pub async fn count(&self) -> Result<usize> {
        self.lance_index
            .count()
            .await
            .map_err(StorageError::index("count"))
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
    /// migration does nothing. Callers MUST NOT hold the write lock: a stale
    /// store takes it once, re-checks the manifest under it (so concurrent
    /// opens migrate once), and holds it across the column add, the rebuild
    /// and the stamp. Migration failures propagate so a broken store is loud
    /// rather than silently half-migrated.
    ///
    /// **Cancellation.** A migration (or any reindex) whose task is aborted
    /// midway is restart-safe: the files are never modified, the table is
    /// emptied by one overwrite commit and refilled by upserts, and the
    /// manifest is stamped last. An aborted run leaves a table with some rows
    /// missing and the old stamp, so the next open migrates again; an aborted
    /// plain `reindex` leaves rows missing until the next `reindex` (the
    /// staleness check reports the count mismatch meanwhile).
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

        // Stale by the unlocked read. Take the write lock ONCE for the whole
        // migration and look again: several processes opening a stale store
        // at the same moment (every session's hooks after an upgrade) all
        // pass the check above, and without the re-check each rebuilt the
        // table in turn — while the lock was released between the column
        // add, the rebuild and the stamp, so another writer could land
        // between any two of them.
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        let loaded = manifest::load_manifest(&manifest_path).await.ok();
        let stored = loaded.as_ref().map(|m| m.schema_version.clone());
        let stored_normalizer = loaded.as_ref().and_then(|m| m.normalizer.clone());
        let schema_stale = stored
            .as_deref()
            .is_none_or(|v| !manifest::schema_version_is_current(v));
        let normalizer_stale = stored_normalizer.as_deref() != Some(engram_types::NORMALIZER_STAMP);
        if !schema_stale && !normalizer_stale {
            return Ok(());
        }
        #[cfg(test)]
        MIGRATION_REBUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
        // ordinary read command.
        self.lance_index
            .add_missing_chunk_columns()
            .await
            .map_err(StorageError::index("chunks-table column add"))?;

        // `force_schema_reset`: a schema migration must recreate the memories
        // table with the current columns. The default (upsert-only) reindex path
        // taken under a foreign checkout can't add columns and would fail on a
        // schema mismatch, so migration forces the table rebuild even then.
        // `reindex_locked`, not `reindex_with`: the lock is already held and
        // is not reentrant.
        self.reindex_locked(true, false).await?;
        // reindex's `update_manifest_stats` rewrote the manifest but preserved
        // the old `schema_version`; stamp the new one now that the rebuild
        // succeeded (mirrors how the embedding fingerprint is stamped only on
        // success). Propagate a load failure rather than clobbering the store's
        // identity with a default manifest. Still under the same lock: this is
        // a load-modify-save of `manifest.toml` racing every mutating op's
        // `update_manifest_stats`.
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
        Ok(self.reindex_with(false, false).await?.indexed)
    }

    /// [`Self::reindex`] with the full counts — including the files that did
    /// not parse, which a surface must report.
    pub async fn reindex_counted(&self) -> Result<ReindexCounts> {
        self.reindex_with(false, false).await
    }

    /// Rebuild only the rows whose file has changed since it was indexed.
    ///
    /// Every file is still enumerated, read, hashed and deduplicated — the skip
    /// removes the *parse*, the stem derivation and the row write, not the I/O,
    /// because deciding whether a file changed requires reading it. That bounds
    /// the possible win; see `docs/contributors/architecture.md`.
    ///
    /// Falls back to a full rebuild, by design, when the manifest says this
    /// store's rows were not derived by this binary — a schema version or
    /// normalizer stamp behind the current one means the *derivation* changed
    /// even where the bytes did not, and an unchanged file is then exactly the
    /// case that needs rebuilding. The returned `skipped` is zero on that path.
    pub async fn reindex_incremental(&self) -> Result<ReindexCounts> {
        let safe = self.incremental_is_safe().await;
        self.reindex_with(false, safe).await
    }

    /// Whether row-level skipping may be trusted for this store.
    ///
    /// Both stamps must match this binary. `migrate_schema_if_needed` normally
    /// guarantees that on open, but the check is repeated here rather than
    /// assumed: it is what makes "the bytes are unchanged" imply "the row is
    /// correct", and that implication is the whole basis of the skip.
    async fn incremental_is_safe(&self) -> bool {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let Ok(manifest) = manifest::load_manifest(&manifest_path).await else {
            // No readable manifest means no derivation stamp to trust.
            return false;
        };
        let schema_current = manifest.schema_version == manifest::CURRENT_SCHEMA_VERSION;
        let normalizer_current =
            manifest.normalizer.as_deref() == Some(engram_types::NORMALIZER_STAMP);
        if !schema_current || !normalizer_current {
            tracing::info!(
                "incremental reindex not applicable (schema {}, normalizer {:?}); \
                 rebuilding every row",
                manifest.schema_version,
                manifest.normalizer
            );
        }
        schema_current && normalizer_current
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
    ///
    /// `incremental` skips rewriting a row whose file still hashes to the
    /// digest that row records. It implies upsert-only mode — clearing the
    /// table would destroy precisely the rows it is about to decide to keep —
    /// so deletions are reconciled explicitly instead, by removing rows for ids
    /// no longer present on disk.
    ///
    /// **The lock only excludes other EngramDB processes.** A `git checkout` or
    /// a hand edit landing between the hash in phase 3 and the upsert in phase 5
    /// stamps a digest for bytes that are no longer on disk — which is exactly
    /// the class of writer this feature exists to notice. It is self-correcting
    /// (the next `doctor` or staleness check reports the drift) but it is not
    /// excluded, and cannot be with an advisory lock.
    async fn reindex_with(
        &self,
        force_schema_reset: bool,
        incremental: bool,
    ) -> Result<ReindexCounts> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.reindex_locked(force_schema_reset, incremental).await
    }

    /// [`Self::reindex_with`] for a caller that already holds the project
    /// write lock (the schema migration). Nothing in here may acquire it:
    /// `flock` on a fresh descriptor blocks in the same process too.
    async fn reindex_locked(
        &self,
        force_schema_reset: bool,
        incremental: bool,
    ) -> Result<ReindexCounts> {
        // What each row was built from, read before any file is touched. Empty
        // in full-rebuild mode, which makes every file a miss and every row a
        // rewrite — the same work the unconditional path always did.
        // Two views of the same read, and they must stay separate.
        //
        // `recorded` drives the skip and holds only rows that HAVE a digest —
        // a row without one records nothing about its file and can never
        // match. `existing_ids` drives the deletion reconciliation and holds
        // EVERY row: deriving deletions from `recorded` instead would leave a
        // digest-less row whose file is gone in the index permanently, since
        // it is absent from the map that decides what to remove.
        let (recorded, existing_ids): (HashMap<String, RowIdentity>, Vec<String>) = if incremental {
            let rows = self
                .lance_index
                .list_digests()
                .await
                .map_err(StorageError::index("list_digests"))?;
            let ids = rows.iter().map(|r| r.memory_id.clone()).collect();
            let map = rows
                .into_iter()
                .filter_map(|row| {
                    let sha = row.content_sha256?;
                    let len = row.content_len?;
                    Some((
                        row.memory_id,
                        RowIdentity {
                            sha256: sha,
                            len,
                            has_embedding: row.has_embedding,
                        },
                    ))
                })
                .collect();
            (map, ids)
        } else {
            (HashMap::new(), Vec::new())
        };

        // Parse everything BEFORE touching the table: the files that do not
        // parse decide which existing rows must survive the rebuild.
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        let mut shared = if shared_dir.exists() {
            self.scan_dir(&shared_dir).await?
        } else {
            ScannedDir {
                owned: Vec::new(),
                paths: HashMap::new(),
                unparseable: Vec::new(),
                renamed: Vec::new(),
            }
        };
        let mut personal = if personal_dir.exists() {
            self.scan_dir(&personal_dir).await?
        } else {
            ScannedDir {
                owned: Vec::new(),
                paths: HashMap::new(),
                unparseable: Vec::new(),
                renamed: Vec::new(),
            }
        };
        // An id with a file in BOTH directories is one memory: index the
        // winner, keep and report the loser (see `ReindexCounts::shadowed`).
        // Writing both rows let whichever directory was written last win the
        // row, whatever `get` said, and counted the memory twice.
        let mut renamed: Vec<(String, PathBuf)> = shared
            .renamed
            .drain(..)
            .chain(personal.renamed.drain(..))
            .collect();
        renamed.sort();
        let mut shadowed: Vec<(String, PathBuf)> = Vec::new();
        {
            let personal_updated: HashMap<&str, chrono::DateTime<chrono::Utc>> = personal
                .owned
                .iter()
                .map(|(id, m, _)| (id.as_str(), m.updated_at))
                .collect();
            let mut drop_shared: HashSet<String> = HashSet::new();
            let mut drop_personal: HashSet<String> = HashSet::new();
            for (id, memory, _) in &shared.owned {
                let Some(personal_at) = personal_updated.get(id.as_str()) else {
                    continue;
                };
                match cross_dir_rule(Some(memory.updated_at), Some(*personal_at)) {
                    CrossDirWinner::Shared => drop_personal.insert(id.clone()),
                    CrossDirWinner::Personal => drop_shared.insert(id.clone()),
                };
            }
            drop(personal_updated);
            for (side, dropped) in [(&mut shared, drop_shared), (&mut personal, drop_personal)] {
                if dropped.is_empty() {
                    continue;
                }
                side.owned.retain(|(id, _, _)| !dropped.contains(id));
                for id in dropped {
                    let path = side.paths.remove(&id).unwrap_or_default();
                    tracing::warn!(
                        "memory {id} has a file in both the shared and the personal directory; \
                         {} is the older copy and is not indexed (it is left on disk — update \
                         or delete the memory to remove it)",
                        path.display()
                    );
                    shadowed.push((id, path));
                }
            }
            shadowed.sort();
        }
        let parsed_ids: std::collections::HashSet<&str> = shared
            .owned
            .iter()
            .chain(personal.owned.iter())
            .map(|(id, _, _)| id.as_str())
            .collect();
        let mut unparseable: Vec<UnparseableMemoryFile> = shared
            .unparseable
            .drain(..)
            .chain(personal.unparseable.drain(..))
            .collect();
        // An id a parseable file also claims is that file's (it wins, as a
        // duplicate would); only the rest are kept.
        let mut kept_ids: Vec<String> = unparseable
            .iter()
            .filter_map(|u| u.id.clone())
            .filter(|id| !parsed_ids.contains(id.as_str()))
            .collect();
        kept_ids.sort();
        kept_ids.dedup();
        drop(parsed_ids);

        let foreign_checkout = self.checkout_conflict().await;
        let will_clear = !incremental && (foreign_checkout.is_none() || force_schema_reset);
        let existing_kept: Vec<String> = if kept_ids.is_empty() {
            Vec::new()
        } else {
            let all: std::collections::HashSet<String> = self
                .lance_index
                .list_ids()
                .await
                .map_err(StorageError::index("list_ids"))?
                .into_iter()
                .collect();
            kept_ids
                .iter()
                .filter(|id| all.contains(*id))
                .cloned()
                .collect()
        };
        for u in &mut unparseable {
            u.preserved = u.id.as_ref().is_some_and(|id| existing_kept.contains(id));
        }
        let kept_rows = if will_clear {
            self.lance_index
                .snapshot_rows(&existing_kept)
                .await
                .map_err(StorageError::index("snapshot"))?
        } else {
            Vec::new()
        };
        // Their edges cannot be re-derived either; keep the rows they own.
        // A failed read is fatal, before anything is cleared: the rebuild
        // below overwrites the whole table, so carrying on would delete the
        // rows these files own until each is fixed and reindexed again.
        let kept_edges: Vec<crate::edge_index::EdgeRow> = if existing_kept.is_empty() {
            Vec::new()
        } else {
            let owners: std::collections::HashSet<&str> =
                existing_kept.iter().map(String::as_str).collect();
            crate::edge_index::load_all(self.lance_index.connection())
                .await
                .map_err(StorageError::index(
                    "edges read for the unparseable memories (their rows would be lost by \
                     the rebuild; reindex refused)",
                ))?
                .into_iter()
                .filter(|r| owners.contains(r.owner_id.as_str()))
                .collect()
        };
        let kept_ids = existing_kept;
        let mut cleared = false;
        if incremental {
            // No clear: the surviving rows ARE the result. Deletions are
            // reconciled after the dir passes below.
        } else if let Some(other) = &foreign_checkout {
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
                self.lance_index
                    .clear_memories()
                    .await
                    .map_err(StorageError::index("clear"))?;
                cleared = true;
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
                .map_err(StorageError::index("clear"))?;
            cleared = true;
        }

        debug_assert_eq!(
            cleared, will_clear,
            "snapshot taken iff the table is cleared"
        );
        let mut indexed_ids = Vec::new();

        // The set of memories that currently have embedding chunks. Gathered
        // once, up front (the chunks table is untouched by the metadata
        // rebuild), and used both to stamp each rebuilt row's `has_embedding`
        // flag (R3) and to prune orphan chunks below.
        let chunk_ids: std::collections::HashSet<String> = self
            .lance_index
            .list_chunk_memory_ids()
            .await
            .map_err(StorageError::index("list_chunk_memory_ids"))?
            .into_iter()
            .collect();

        let mut skipped = 0;
        let mut edges: Vec<crate::edge_index::EdgeRow> = Vec::new();

        skipped += self
            .write_dir(
                &shared.owned,
                Visibility::Shared,
                &chunk_ids,
                &mut indexed_ids,
                &recorded,
                &mut edges,
            )
            .await?;
        skipped += self
            .write_dir(
                &personal.owned,
                Visibility::Personal,
                &chunk_ids,
                &mut indexed_ids,
                &recorded,
                &mut edges,
            )
            .await?;

        // Carry the unparseable files' rows across the clear, and count them
        // as indexed so neither the deletion reconciliation nor the orphan
        // prune below takes their row or their vectors.
        if !kept_rows.is_empty() {
            let (_, lost) = self
                .lance_index
                .restore_rows(kept_rows)
                .await
                .map_err(StorageError::index("restore of unparseable rows"))?;
            if lost > 0 {
                tracing::warn!(
                    "{lost} unparseable memory row(s) could not be carried across this schema \
                     change; they reappear once their files are fixed and reindexed"
                );
            }
        }
        indexed_ids.extend(kept_ids.iter().cloned());
        edges.extend(kept_edges);

        // Reconcile deletions. The unconditional path gets this free by
        // clearing the table first; the incremental path never clears, so a row
        // whose file is gone would survive forever and keep answering queries
        // with a memory that no longer exists.
        //
        // Skipped under a checkout conflict for the same reason the orphan
        // prune is: the other clone's files are not visible here, so all of its
        // rows would look deleted.
        if incremental && foreign_checkout.is_none() {
            let on_disk: std::collections::HashSet<&str> =
                indexed_ids.iter().map(|s| s.as_str()).collect();
            let removed: Vec<String> = existing_ids
                .iter()
                .filter(|id| !on_disk.contains(id.as_str()))
                .cloned()
                .collect();
            if !removed.is_empty() {
                self.lance_index
                    .delete_batch(&removed)
                    .await
                    .map_err(StorageError::index("delete_batch"))?;
            }
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
                .map_err(StorageError::index("delete_chunks"))?;
        }

        // The derived edges table (schema 0.9.0). Rebuilt whole in one commit
        // unless another checkout shares this index: its files are invisible
        // here, so its memories' rows would all look deleted — replace only
        // the rows this checkout's memories own, like the upsert-only rows
        // above. That same per-owner path is also correct for an incremental
        // pass, whose removed ids were reconciled above; their rows go too.
        let conn = self.lance_index.connection();
        let edge_result = if foreign_checkout.is_none() {
            crate::edge_index::rebuild(conn, &edges).await
        } else {
            crate::edge_index::replace_owned(conn, &indexed_ids, &edges).await
        };
        edge_result.map_err(StorageError::index("edges rebuild"))?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(ReindexCounts {
            indexed: indexed_ids.len(),
            skipped,
            unparseable,
            shadowed,
            renamed,
        })
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
            .map_err(StorageError::index("clear_chunks"))
    }

    /// [`Self::clear_chunks`], keeping the vectors (and `has_embedding`) of
    /// the memories in `keep` — the unparseable files of a forced re-embed
    /// under an unchanged model. See `LanceIndex::clear_chunks_keeping`.
    pub async fn clear_chunks_keeping(&self, keep: &[String]) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .clear_chunks_keeping(keep)
            .await
            .map_err(StorageError::index("clear_chunks_keeping"))
    }

    /// The vector width of the on-disk chunks table, or `None` when the
    /// table doesn't exist yet. See `LanceIndex::chunks_table_dimensions`.
    pub async fn chunks_table_dimensions(&self) -> Result<Option<usize>> {
        self.lance_index
            .chunks_table_dimensions()
            .await
            .map_err(StorageError::index("chunks schema read"))
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
    /// [`Self::upsert_chunks_if`] and [`Self::upsert_chunks_batch_if`],
    /// which the retrieval engine drives.
    pub async fn upsert_chunks(&self, memory_id: &str, chunks: Vec<Vec<f32>>) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .upsert_chunks(memory_id, chunks, None)
            .await
            .map_err(StorageError::index("upsert_chunks"))
    }

    /// Upsert embedding chunks only if the memory, re-read under the write
    /// lock, still passes `still_current`. Returns `true` when the chunks were
    /// written.
    ///
    /// Detached ingest tasks embed a `Memory` snapshot with no ordering
    /// guarantee: two rapid updates spawn two tasks whose `upsert_chunks`
    /// commits the flock serializes but does not order, so the task carrying
    /// the OLDER content can commit last (vectors then describe v1 while the
    /// file holds v2 until the next update/reindex); and a create-then-delete
    /// leaves a late embed re-inserting chunks for a deleted memory (orphan
    /// rows occupying top-k slots until doctor/reindex prunes them). This
    /// re-reads the memory UNDER the write lock and skips the write when the
    /// memory is gone or `still_current` refuses it.
    ///
    /// The predicate belongs to the caller because what the vectors depend on
    /// (the embedding composition) lives above this crate. The engine passes
    /// "the memory still embeds to the same texts", so a metadata-only write
    /// (a link, a challenge, `harvest mark --memory`) racing the embed no
    /// longer drops its vectors — comparing `updated_at` did.
    ///
    /// `embed_sha256` describes the text `chunks` were computed from and is
    /// written in the same `RecordBatch` as the vectors, so it can never end up
    /// describing a different snapshot than the vectors beside it.
    pub async fn upsert_chunks_if<F>(
        &self,
        memory_id: &str,
        chunks: Vec<Vec<f32>>,
        embed_sha256: Option<String>,
        still_current: F,
    ) -> Result<bool>
    where
        F: FnOnce(&Memory) -> bool + Send,
    {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        match self.get(memory_id).await {
            Ok(current) if still_current(&current) => {}
            Ok(_) => {
                tracing::info!(
                    "skipping stale chunk upsert for {memory_id}: its text changed since the snapshot"
                );
                return Ok(false);
            }
            Err(StorageError::NotFound(_)) => {
                tracing::info!("skipping chunk upsert for deleted memory {memory_id}");
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
        self.lance_index
            .upsert_chunks(memory_id, chunks, embed_sha256.as_deref())
            .await
            .map_err(StorageError::index("upsert_chunks"))?;
        Ok(true)
    }

    /// [`Self::upsert_chunks_if`] for many memories at once.
    ///
    /// Takes the per-project write lock **once**, re-reads every memory in a
    /// single batched directory scan, drops the entries `still_current`
    /// refuses (called with the re-read memory and the entry's digest) or that
    /// vanished, and writes what is left in one merge_insert. Returns the ids
    /// actually written.
    pub async fn upsert_chunks_batch_if<F>(
        &self,
        entries: Vec<crate::lance_index::ChunkWrite>,
        still_current: F,
    ) -> Result<Vec<String>>
    where
        F: Fn(&Memory, Option<&str>) -> bool + Send,
    {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let ids: Vec<&str> = entries.iter().map(|(id, _, _)| id.as_str()).collect();
        let current: HashMap<String, Memory> = self.get_batch(&ids).await?.into_iter().collect();

        let mut fresh: Vec<crate::lance_index::ChunkWrite> = Vec::with_capacity(entries.len());
        for (id, chunks, digest) in entries {
            match current.get(&id) {
                Some(mem) if still_current(mem, digest.as_deref()) => {
                    fresh.push((id, chunks, digest))
                }
                Some(_) => tracing::info!(
                    "skipping stale chunk upsert for {id}: its text changed since the snapshot"
                ),
                None => tracing::info!("skipping chunk upsert for deleted memory {id}"),
            }
        }
        if fresh.is_empty() {
            return Ok(Vec::new());
        }

        self.lance_index
            .upsert_chunks_batch(&fresh)
            .await
            .map_err(StorageError::index("upsert_chunks_batch"))?;
        Ok(fresh.into_iter().map(|(id, _, _)| id).collect())
    }

    /// The `updated_at`-guarded batch write: like
    /// [`Self::upsert_chunks_batch_if`], but an entry is written only while the
    /// memory's `updated_at` still equals the entry's snapshot.
    ///
    /// Kept for worktree consolidation, which **relocates** vectors it did not
    /// compute and so has no digest or composition to compare; any metadata
    /// write in between makes it skip, which there only costs a re-embed. The
    /// engine's own writes use [`Self::upsert_chunks_batch_if`].
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
            .map_err(StorageError::index("upsert_chunks_batch"))?;
        Ok(fresh.into_iter().map(|(id, _, _)| id).collect())
    }

    /// Whether the chunks table holds any row for `id`.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn has_chunks_for(&self, id: &str) -> Result<bool> {
        self.lance_index
            .has_chunks(id)
            .await
            .map_err(StorageError::index("has_chunks"))
    }

    /// Null the `keyword_stems` column of these rows (a pre-0.5.0 row shape),
    /// so a fixture can reach the query path's file fallback.
    /// The memories table's version (one per commit), for tests that count
    /// a write path's commits from outside this crate.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn memories_version_for_test(&self) -> Result<u64> {
        self.lance_index
            .memories_version()
            .await
            .map_err(StorageError::index("memories_version"))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn null_keyword_stems_for_test(&self, ids: &[String]) -> Result<()> {
        self.lance_index
            .null_keyword_stems_for_test(ids)
            .await
            .map_err(StorageError::index("null_keyword_stems"))
    }

    /// Write an index row directly, bypassing the file write.
    ///
    /// Test-only: production always derives the row from the file it just
    /// wrote, which is what keeps the content digest honest. Exposed for
    /// fixtures that need to reproduce a row shape no current write path
    /// produces (e.g. a pre-0.8.0 row with no digest).
    #[cfg(any(test, feature = "test-support"))]
    pub async fn upsert_index_entry_for_test(
        &self,
        entry: &crate::lance_index::IndexEntry,
    ) -> Result<()> {
        self.lance_index
            .upsert(entry)
            .await
            .map_err(StorageError::index("upsert"))
    }

    /// Every indexed row's content digest (schema v0.8.0).
    ///
    /// See [`crate::lance_index::LanceIndex::list_digests`].
    pub async fn index_digests(&self) -> Result<Vec<crate::lance_index::IndexDigest>> {
        self.lance_index
            .list_digests()
            .await
            .map_err(StorageError::index("list_digests"))
    }

    /// Every memory's on-disk digest, computed in **two directory scans total**
    /// rather than two per memory.
    ///
    /// This is the batched form of [`Self::read_memory_bytes`] and exists
    /// because the unbatched one is a trap at scale: it resolves each id with
    /// `find_memory_files`, which is a full `read_dir` of both the shared and
    /// personal directories. Calling it in a loop over every row — which is
    /// exactly what a whole-store drift check does — makes the check quadratic
    /// in dirent work. `stats` runs one on every invocation, so the cost is
    /// paid by a command that used to do no file I/O at all.
    ///
    /// Returns `(digests, unreadable)`. A file that exists but cannot be read
    /// lands in `unreadable`, never silently in the clean set: the whole point
    /// of the distinction in [`Self::read_memory_bytes`].
    pub async fn file_digests(&self) -> Result<(HashMap<String, FileDigest>, Vec<String>)> {
        let shared = scan_dir_to_stats(&paths::memories_dir(&self.project_dir)).await;
        let personal = scan_dir_to_stats(&paths::personal_memories_dir(&self.project_id)?).await;

        let mut digests = HashMap::new();
        let mut unreadable = Vec::new();
        for (id, stat) in merge_cross_dir(shared, personal).await.by_id {
            match async_fs::read(&stat.path).await {
                Ok(bytes) => {
                    digests.insert(id, FileDigest::of(&bytes));
                }
                Err(e) => {
                    tracing::warn!("cannot read memory file {}: {e}", stat.path.display());
                    unreadable.push(id);
                }
            }
        }
        unreadable.sort();
        Ok((digests, unreadable))
    }

    /// The raw bytes of a memory's `.md` file: `Ok(None)` when no file exists,
    /// `Err` when one exists but cannot be read.
    ///
    /// The distinction is the point. A currency check that folded an unreadable
    /// file into "absent" would report it as clean — an assertion of health it
    /// cannot support — while a missing file is a different finding that
    /// `doctor` already reports as a stale index entry.
    ///
    /// **Do not call this in a loop over the store.** Each call is a full
    /// `read_dir` of two directories; use [`Self::file_digests`] for a
    /// whole-store pass.
    pub async fn read_memory_bytes(&self, id: &str) -> Result<Option<Vec<u8>>> {
        for visibility in [Visibility::Shared, Visibility::Personal] {
            let dir = self.get_memories_dir(&visibility)?;
            let mut paths = find_memory_files(&dir, id).await?;
            // Newest wins, matching `locate_memory_file`'s duplicate resolution: the
            // row describes whichever file a reader would resolve to.
            if paths.len() > 1 {
                let mut with_mtime: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
                for p in paths {
                    with_mtime.push((file_mtime(&p).await, p));
                }
                with_mtime.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
                paths = with_mtime.into_iter().map(|(_, p)| p).collect();
            }
            if let Some(path) = paths.first() {
                return async_fs::read(path).await.map(Some).map_err(Into::into);
            }
        }
        Ok(None)
    }

    /// Every embedded memory's `embed_sha256` (schema v0.8.0), keyed by memory
    /// id. See [`crate::lance_index::LanceIndex::list_embed_digests`].
    pub async fn embed_digests(&self) -> Result<HashMap<String, Option<String>>> {
        self.lance_index
            .list_embed_digests()
            .await
            .map_err(StorageError::index("list_embed_digests"))
    }

    /// Delete all embedding chunks for a memory.
    pub async fn delete_chunks(&self, memory_id: &str) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .delete_chunks(memory_id)
            .await
            .map_err(StorageError::index("delete_chunks"))
    }

    /// List all distinct memory_ids present in the chunks table.
    pub async fn list_chunk_memory_ids(&self) -> Result<Vec<String>> {
        self.lance_index
            .list_chunk_memory_ids()
            .await
            .map_err(StorageError::index("list_chunk_memory_ids"))
    }

    /// Read every embedding chunk for `memory_id`, ordered by chunk index.
    ///
    /// Empty when the memory was never embedded. Used to relocate vectors
    /// during worktree consolidation so migrated memories stay searchable.
    pub async fn export_chunks(&self, memory_id: &str) -> Result<Vec<Vec<f32>>> {
        self.lance_index
            .chunks_for_memory(memory_id)
            .await
            .map_err(StorageError::index("chunks_for_memory"))
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
            .map_err(StorageError::index("chunks_for_memories"))
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
            .map_err(StorageError::index("vector_search"))
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
    /// Whether `id` has embedding chunks, for a row about to be written.
    async fn probe_has_chunks(&self, id: &str) -> Result<bool> {
        #[cfg(test)]
        if FAIL_CHUNK_PROBE.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(StorageError::index("has_chunks")(anyhow::anyhow!(
                "injected chunk-probe failure"
            )));
        }
        self.lance_index
            .has_chunks(id)
            .await
            .map_err(StorageError::index("has_chunks"))
    }

    /// Remove every file of `id` from the shared AND the personal directory.
    ///
    /// Both, always: stopping at the first directory that had a copy left a
    /// cross-directory duplicate's other copy on disk, and the next reindex
    /// brought the deleted memory back. `NotFound` only when neither
    /// directory had one; a real I/O error propagates as-is.
    async fn delete_files_in_both_dirs(&self, id: &str) -> Result<()> {
        let shared = self
            .delete_file_from_dir(id, &paths::memories_dir(&self.project_dir))
            .await;
        let personal = self
            .delete_file_from_dir(id, &paths::personal_memories_dir(&self.project_id)?)
            .await;
        match (shared, personal) {
            (Err(StorageError::NotFound(_)), Err(e @ StorageError::NotFound(_))) => Err(e),
            (Err(StorageError::NotFound(_)), other) | (other, Err(StorageError::NotFound(_))) => {
                other
            }
            (Err(e), _) | (_, Err(e)) => Err(e),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

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
        self.resolve_id(id).await
    }

    /// The full id a unique prefix (or a full id) names, from the index.
    /// `NotFound` when nothing matches, `AmbiguousId` when the prefix is
    /// ambiguous.
    pub async fn resolve_id(&self, id: &str) -> Result<String> {
        let matches = self
            .lance_index
            .find_ids_by_prefix(id)
            .await
            .map_err(StorageError::index("prefix search"))?;

        match matches.len() {
            0 => Err(StorageError::NotFound(id.to_string())),
            1 => Ok(matches.into_iter().next().unwrap()),
            // Exact full-ID match beats prefix ambiguity (legacy non-UUID
            // ids can be prefixes of each other) — mirrors
            // `find_memory_files` and `paths::find_memory_in_dir`.
            _ if matches.iter().any(|m| m == id) => Ok(id.to_string()),
            _ => Err(StorageError::AmbiguousId {
                prefix: id.to_string(),
                matches: matches.len(),
            }),
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
    ///
    /// `recorded` maps memory id → the content digest its current row was built
    /// from. Non-empty only in incremental mode; a file whose hash matches its
    /// entry has its row left untouched. Returns how many rows were skipped.
    async fn scan_dir(&self, dir: &Path) -> Result<ScannedDir> {
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
        // A file that does not parse is NOT dropped silently: its id is
        // recovered (frontmatter line, else filename stem) so the caller can
        // keep its existing row and vectors, and it is reported. Dropping it
        // made a merge conflict in a hidden block delete the memory from the
        // index — and its vectors with it — on the next reindex.
        let results: Vec<std::result::Result<_, UnparseableMemoryFile>> = read
            .into_par_iter()
            .map(|(path, mtime, content)| {
                match memory_file::parse_memory_file_at(&content, mtime.into()) {
                    Ok(memory) => {
                        let digest = FileDigest::of(content.as_bytes());
                        Ok((path, mtime, memory, digest))
                    }
                    Err(e) => Err(UnparseableMemoryFile {
                        id: memory_file::recover_id(&content, &path),
                        path,
                        error: e.to_string(),
                        preserved: false,
                    }),
                }
            })
            .collect();
        let mut parsed: Vec<(PathBuf, std::time::SystemTime, Memory, FileDigest)> = Vec::new();
        let mut unparseable = Vec::new();
        for r in results {
            match r {
                Ok(p) => parsed.push(p),
                Err(u) => {
                    tracing::warn!(
                        "{} does not parse ({}); its index row and vectors are kept as they \
                         were — fix the file (an unresolved merge conflict?) and reindex",
                        u.path.display(),
                        u.error
                    );
                    unparseable.push(u);
                }
            }
        }

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

        // Heal a file whose name hides its id. Every lookup by id goes
        // through the filename (`extract_id_from_stem`), so a file whose stem
        // does not yield the id inside it is indexed and then unreachable:
        // `get`, `update` and `delete` all answer NotFound. That is what a
        // titled non-UUID id was renamed to before `memory_filename` stopped
        // adding a slug to one, and what a hand-named file is. Renamed here,
        // under the write lock; the bytes (and so the digest) do not change.
        let mut renamed: Vec<(String, PathBuf)> = Vec::new();
        for (id, (path, _mtime, memory, _digest)) in by_id.iter_mut() {
            if stem_id(path) == Some(id.as_str()) {
                continue;
            }
            let target = dir.join(memory_file::memory_filename(memory));
            if target == *path {
                // An id no filename can carry (see `memory_filename`).
                continue;
            }
            if target.exists() {
                tracing::warn!(
                    "{} holds memory {id} but its name does not carry that id, and {} \
                     already exists; leaving it (the memory is not reachable by id)",
                    path.display(),
                    target.display()
                );
                continue;
            }
            match async_fs::rename(&*path, &target).await {
                Ok(()) => {
                    tracing::warn!(
                        "renamed {} to {}: the old name did not carry the id of the memory \
                         in it, so the memory could not be found by id",
                        path.display(),
                        target.display()
                    );
                    *path = target.clone();
                    renamed.push((id.clone(), target));
                }
                Err(e) => tracing::warn!(
                    "could not rename {} to {} ({e}); memory {id} stays unreachable by id",
                    path.display(),
                    target.display()
                ),
            }
        }

        let mut paths = HashMap::with_capacity(by_id.len());
        let owned: Vec<(String, Memory, FileDigest)> = by_id
            .into_iter()
            .map(|(id, (path, _mtime, memory, digest))| {
                paths.insert(id.clone(), path);
                (id, memory, digest)
            })
            .collect();
        Ok(ScannedDir {
            owned,
            paths,
            unparseable,
            renamed,
        })
    }

    /// Phase 5 of a reindex: write the rows [`Self::scan_dir`] derived.
    async fn write_dir(
        &self,
        owned: &[(String, Memory, crate::digest::FileDigest)],
        visibility: Visibility,
        chunk_ids: &std::collections::HashSet<String>,
        indexed_ids: &mut Vec<String>,
        recorded: &HashMap<String, RowIdentity>,
        edges: &mut Vec<crate::edge_index::EdgeRow>,
    ) -> Result<usize> {
        use rayon::prelude::*;

        // Phase 5 — build the index rows, in parallel. `IndexEntry::for_file`
        // derives the memory's keyword stems (tokenize → stoplist → Snowball
        // stem → dedup for three fields), the second-largest CPU cost here at
        // ~16ms per 1,000 memories serially, ~5.7ms across four cores
        // (`benches/parallel_simd.rs`, `stems/*`).
        //
        // One batched merge_insert instead of one commit (and one LanceDB
        // dataset version) per memory — reindexing a 1,000-memory store used
        // to perform 1,000 sequential commits that optimize() then compacted.

        // Incremental mode drops the rows whose file still hashes to what the
        // row was built from. This happens AFTER phase 4, never before: phase 4
        // deletes the loser of a duplicate-id pair, so skipping enumeration or
        // deduplication would leave stale files on disk forever. What is skipped
        // is only the derivation and the write.
        //
        // `indexed_ids` gets every id either way, skipped ones included. It
        // drives the orphan-chunk prune, so omitting a skipped memory would make
        // its vectors look orphaned and delete them — turning a no-op reindex
        // into silent vector loss.
        // A row is skippable only when EVERY column a rebuild would write is
        // already what the rebuild would write. Comparing the hash alone was
        // not enough, because two of those columns are not determined by it:
        //
        // - `content_len` is the RAW on-disk length while `content_sha256` is
        //   computed over CRLF-normalized bytes, deliberately (see
        //   `digest.rs`). A `core.autocrlf` checkout therefore leaves a
        //   matching hash and a stale length — and the default `size`
        //   staleness tier compares exactly that length, so the store warned on
        //   every read and the incremental reindex the warning names could
        //   never clear it.
        // - `has_embedding` mirrors the chunks table, not the file. Vectors
        //   added or dropped since the last rebuild leave an unchanged file and
        //   a stale flag, and the retrieval scorer reads that flag to decide
        //   whether a memory has semantic evidence at all.
        //
        // `visibility` needs no check: it is serialized into the file's hidden
        // metadata block, so changing it rewrites the bytes and moves the hash.
        let mut skipped = 0;
        let to_write: Vec<&(String, Memory, crate::digest::FileDigest)> = owned
            .iter()
            .filter(|(id, _, digest)| {
                let current = recorded.get(id).is_some_and(|row| {
                    row.sha256 == digest.sha256
                        && row.len == digest.len
                        && row.has_embedding == chunk_ids.contains(id.as_str())
                });
                if current {
                    skipped += 1;
                }
                !current
            })
            .collect();

        let entries_batch: Vec<IndexEntry> = to_write
            .par_iter()
            .map(|(id, memory, digest)| {
                let mut index_entry = IndexEntry::for_file(memory, digest);
                index_entry.visibility = visibility;
                // R3: stamp the embedding flag from the chunk-table snapshot so a
                // reindex (including the on-open schema migration) leaves
                // `has_embedding` authoritative.
                index_entry.has_embedding = chunk_ids.contains(id.as_str());
                index_entry
            })
            .collect();
        indexed_ids.extend(owned.iter().map(|(id, _, _)| id.clone()));
        // Every memory's edges, skipped rows included: the edges table is
        // rebuilt whole (or per owner) from this, and deriving it is a walk
        // over already-parsed memories — no I/O, so the incremental skip has
        // nothing to save here.
        edges.extend(
            owned
                .iter()
                .flat_map(|(_, memory, _)| crate::edge_index::edges_of(memory)),
        );
        if !entries_batch.is_empty() {
            self.lance_index
                .upsert_batch(&entries_batch)
                .await
                .map_err(StorageError::index("upsert"))?;
        }
        Ok(skipped)
    }

    /// Check whether the LanceDB index has fallen behind the memory files on
    /// disk, at the tier `cfg` asks for.
    ///
    /// Returns `Some(warning_message)` if drift was confirmed, `None` if the
    /// store looks current. See [`crate::staleness`] for what each tier
    /// compares, why the default is `size` rather than exact hashing, and why a
    /// finding must be confirmed by a second pass.
    ///
    /// **A `None` here is not proof of currency** — only the `content` tier is
    /// exact, and even it is budget-bounded. `doctor` and `reindex --dry-run`
    /// are the unbudgeted authority; this runs on every read and is a
    /// best-effort early warning.
    ///
    /// Never returns `Err` for a *tier* failure. The three callers all treat an
    /// error as "no warning", so a tier that cannot complete degrades to the
    /// strongest tier that can rather than silently reporting a clean store.
    /// An error here means the index itself could not be counted.
    pub async fn check_staleness(&self, cfg: &IndexConfig) -> Result<Option<String>> {
        // A shared project id means two checkouts legitimately hold different
        // bytes for the same ids, and the index holds the union. Every tier
        // would fire, permanently and unrepairably: reindex degrades to a
        // non-destructive upsert under conflict by design, so the warning would
        // never clear.
        if self.checkout_conflict().await.is_some() {
            return Ok(None);
        }

        let first = self.staleness_pass(cfg).await?;
        if first.is_clean() {
            return Ok(None);
        }
        // Something was found — which on a lock-free read path is as likely to
        // be a write committing across the pass as it is real drift. Pay for a
        // second pass only here, and report only what both agree on.
        let second = self.staleness_pass(cfg).await?;
        Ok(first.confirmed_by(&second).message(cfg.staleness_check))
    }

    /// One tiered comparison of the files against the index.
    ///
    /// Ordering is load-bearing: the file enumeration happens **first** and the
    /// index projection second. `create` writes the file before the row, so
    /// reading the index first would make every concurrent create look like a
    /// missing row; this ordering makes it look like an extra file instead,
    /// which the second pass then clears. Neither ordering avoids the race —
    /// that is what [`StalenessFindings::confirmed_by`] is for — but this one
    /// keeps the transient state consistent between the two passes.
    async fn staleness_pass(&self, cfg: &IndexConfig) -> Result<StalenessFindings> {
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;

        // Tier A always runs: it is the cheapest signal and the only one that
        // needs no per-file `statx`.
        if cfg.staleness_check == StalenessCheck::Counts {
            let md_count = count_distinct_md_files(&shared_dir, &personal_dir).await;
            let lance_count = self.lance_count().await?;
            return Ok(StalenessFindings {
                md_count,
                lance_count,
                ..Default::default()
            });
        }

        let shared = scan_dir_to_stats(&shared_dir).await;
        let personal = scan_dir_to_stats(&personal_dir).await;
        // A file count, not a distinct-id count, WITHIN a directory: a stale
        // duplicate left by a crashed rename is a real inconsistency, and
        // tier A has always reported it. An id present in BOTH directories is
        // counted once and compared through its winning copy: reindex keeps
        // (and reports) such a pair rather than deleting a file, so counting
        // both made this warning permanent.
        let merged = merge_cross_dir(shared, personal).await;
        let md_count = merged.file_count;
        let by_id = merged.by_id;

        let rows = self.index_digests().await?;
        let lance_count = rows.len();

        let mut findings = StalenessFindings {
            md_count,
            lance_count,
            ..Default::default()
        };

        // Tier B — id sets. Catches a delete paired with a create, which leaves
        // the counts equal.
        let mut indexed_ids = std::collections::HashSet::with_capacity(rows.len());
        // Tier C candidates: rows whose length still matches, which only the
        // content tier can rule on.
        let mut len_matched = Vec::new();
        for row in &rows {
            indexed_ids.insert(row.memory_id.as_str());
            let Some(stat) = by_id.get(&row.memory_id) else {
                findings.missing_from_disk.insert(row.memory_id.clone());
                continue;
            };
            match row.content_len {
                // Tier C — length. A row that predates schema 0.8.0 records no
                // length; it is *unknown*, not matching, and must not be
                // reported as changed. Backfilling it is reindex's job.
                None => {}
                Some(len) if len != stat.len => {
                    findings.changed.insert(row.memory_id.clone());
                }
                Some(_) => len_matched.push((row, stat)),
            }
        }
        for id in by_id.keys() {
            if !indexed_ids.contains(id.as_str()) {
                findings.missing_from_index.insert(id.clone());
            }
        }

        if cfg.staleness_check == StalenessCheck::Content {
            self.hash_len_matched(&len_matched, cfg.staleness_max_bytes, &mut findings)
                .await;
        }
        Ok(findings)
    }

    /// Tier D — hash the files whose length still matches, up to `max_bytes`.
    ///
    /// Sorted by id so the budget cuts at a deterministic place: an arbitrary
    /// `read_dir` order would make a budget-limited check report a different
    /// subset on every run, which reads as flapping drift.
    ///
    /// Stops at the budget and records that it did, rather than downgrading to
    /// tier C silently — a partial exact check must never be reported as a
    /// clean exact check.
    async fn hash_len_matched(
        &self,
        candidates: &[(&IndexDigest, &FileStat)],
        max_bytes: u64,
        findings: &mut StalenessFindings,
    ) {
        let mut ordered: Vec<_> = candidates.iter().collect();
        ordered.sort_by(|a, b| a.0.memory_id.cmp(&b.0.memory_id));

        let mut spent: u64 = 0;
        for (row, stat) in ordered {
            let Some(recorded) = row.content_sha256.as_deref() else {
                continue;
            };
            if spent.saturating_add(stat.len) > max_bytes {
                findings.budget_exhausted = true;
                break;
            }
            spent = spent.saturating_add(stat.len);
            match async_fs::read(&stat.path).await {
                Ok(bytes) => {
                    if crate::digest::FileDigest::of(&bytes).sha256 != recorded {
                        findings.changed.insert(row.memory_id.clone());
                    }
                }
                // Unreadable is *undetermined*, not drifted. `doctor` reports
                // it as its own category; on the read path, warning "changed"
                // for a permissions problem would send the user to reindex,
                // which cannot fix it either.
                Err(e) => {
                    tracing::debug!(
                        "staleness: cannot hash memory {}: {e}",
                        row.memory_id.as_str()
                    );
                }
            }
        }
    }

    /// Row count, as an index error rather than a tier failure.
    async fn lance_count(&self) -> Result<usize> {
        self.lance_index
            .count()
            .await
            .map_err(StorageError::index("count"))
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
        let (manifest_path, manifest) = self.load_manifest_for_stats().await;
        self.rescan_manifest_stats(&manifest_path, manifest).await
    }

    /// [`Self::update_manifest_stats`] for a write that added `added` rows
    /// carrying `new_scopes`, without the full `logical` column scan when the
    /// manifest can be shown to have been current.
    ///
    /// The check is `count_rows()` (table metadata, no scan) against
    /// `manifest.memory_count + added`: when they agree the manifest was in
    /// step before this write, so the new count and the union of the scopes
    /// are exactly what the scan would produce. Any disagreement — the
    /// manifest is a committed file, and a branch switch or a merge can change
    /// it under the index — falls back to the scan, so a drifted manifest
    /// still self-heals on the next write. (A manifest whose count happens to
    /// match while its scope list is stale is corrected by the next update,
    /// delete or reindex, all of which always scan.)
    async fn update_manifest_stats_after(&self, added: usize, new_scopes: &[String]) -> Result<()> {
        let (manifest_path, mut manifest) = self.load_manifest_for_stats().await;
        let rows = self
            .lance_index
            .count()
            .await
            .map_err(StorageError::index("count"))?;
        if rows != manifest.stats.memory_count + added {
            return self.rescan_manifest_stats(&manifest_path, manifest).await;
        }
        let mut logical_scopes = manifest.stats.logical_scopes.clone();
        logical_scopes.extend(new_scopes.iter().cloned());
        logical_scopes.sort();
        logical_scopes.dedup();
        manifest::update_stats(&mut manifest, rows, logical_scopes);
        manifest::save_manifest(&manifest_path, &manifest).await?;
        Ok(())
    }

    /// The manifest to refresh, or a rebuilt default one.
    async fn load_manifest_for_stats(&self) -> (PathBuf, manifest::Manifest) {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        // Stats refresh runs AFTER the mutation (file + index row) is durable,
        // so a missing/corrupt manifest must not fail the operation — the
        // caller would report failure for a create/update/delete that in fact
        // succeeded, and nothing would ever self-heal the manifest. Rebuild a
        // default one instead (the project id is known) and let the stats
        // write below recreate the file.
        let manifest = match manifest::load_manifest(&manifest_path).await {
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
        (manifest_path, manifest)
    }

    /// Recompute the stats from the index (one `logical` column scan) and
    /// write the manifest when they changed.
    async fn rescan_manifest_stats(
        &self,
        manifest_path: &Path,
        mut manifest: manifest::Manifest,
    ) -> Result<()> {
        let (memory_count, scope_set) = self
            .lance_index
            .count_and_logical_scopes()
            .await
            .map_err(StorageError::index("stats scan"))?;
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
        manifest::save_manifest(manifest_path, &manifest).await?;

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
            .map_err(StorageError::index("chunk count"))
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

    /// Stamp `fingerprint` only if the store has none yet; returns whether it
    /// wrote. The unset check and the write happen under one write lock.
    ///
    /// This is the first-embed stamp's writer (`embed_memory_with`), and the
    /// check has to be repeated here rather than trusted from the caller:
    /// the caller decides "the store is unstamped" *before* its embed and its
    /// chunk upsert, then stamps after them, and anything may stamp the store
    /// in between — a `reindex --embeddings-only` for a newly configured
    /// model, or a concurrent first embed. Acting on the stale answer wrote
    /// the ingest's model over that newer stamp, so the manifest no longer
    /// described the vectors. Lock rules as [`Self::set_embedding_fingerprint`].
    pub async fn stamp_embedding_fingerprint_if_unset(
        &self,
        fingerprint: manifest::EmbeddingFingerprint,
    ) -> Result<bool> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let mut manifest = manifest::load_manifest(&manifest_path).await?;
        if manifest.embedding.is_some() {
            return Ok(false);
        }
        manifest.embedding = Some(fingerprint);
        manifest::save_manifest(&manifest_path, &manifest).await?;
        Ok(true)
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
            StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "atomic_write target has no parent directory",
            ))
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
    .map_err(|e| {
        StorageError::Io(std::io::Error::other(format!(
            "atomic_write task failed: {e}"
        )))
    })?
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
        // Memory files only. `<id>.bak` has the stem `<id>` and
        // `<id>.md.orig` a stem that starts with it; without this filter an
        // editor's backup was read as the memory, and removed by the next
        // update or delete.
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
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
        return Err(StorageError::AmbiguousId {
            prefix: id.to_string(),
            matches: distinct_ids.len(),
        });
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

/// Which copy of an id present in both visibility directories is the memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CrossDirWinner {
    Shared,
    Personal,
}

/// An id with a file in both the shared and the personal directory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CrossDirDuplicate {
    pub id: String,
    pub shared: PathBuf,
    pub personal: PathBuf,
    pub winner: CrossDirWinner,
}

/// The one rule for an id in both directories, from each copy's parsed
/// `updated_at` (`None` = the copy does not parse):
///
/// - the newest `updated_at` wins;
/// - on a tie the **personal** copy wins — the private choice is the safe
///   one: a `git checkout` that restores a tracked shared file must not make
///   a memory public again;
/// - a copy that does not parse loses to one that does.
fn cross_dir_rule(
    shared: Option<chrono::DateTime<chrono::Utc>>,
    personal: Option<chrono::DateTime<chrono::Utc>>,
) -> CrossDirWinner {
    match (shared, personal) {
        (Some(s), Some(p)) if s > p => CrossDirWinner::Shared,
        (Some(_), None) => CrossDirWinner::Shared,
        _ => CrossDirWinner::Personal,
    }
}

/// [`cross_dir_rule`] for two files. `get`, `get_batch`, the staleness check,
/// `file_digests` and `reindex` all go through the rule, so they agree.
async fn cross_dir_winner(shared: &Path, personal: &Path) -> CrossDirWinner {
    async fn updated_at(path: &Path) -> Option<chrono::DateTime<chrono::Utc>> {
        let content = async_fs::read_to_string(path).await.ok()?;
        Some(
            memory_file::parse_memory_file_from(&content, path)
                .ok()?
                .updated_at,
        )
    }
    cross_dir_rule(updated_at(shared).await, updated_at(personal).await)
}

/// The id a memory file's name carries.
fn stem_id(path: &Path) -> Option<&str> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(memory_file::extract_id_from_stem)
}

/// The current file for `id` (or a unique prefix) in one directory: the
/// newest when a crashed rename left several for the same id.
async fn locate_memory_file(dir: &Path, id: &str) -> Result<Option<PathBuf>> {
    let mut matches = find_memory_files(dir, id).await?;
    Ok(match matches.len() {
        0 => None,
        1 => matches.pop(),
        // Multiple files for ONE full ID (`find_memory_files` already
        // rejected distinct IDs as ambiguous): a stale duplicate left by an
        // update that crashed between writing the renamed file and removing
        // the old one. The newest file is the current state — the rename
        // path always writes the new file last.
        _ => Some(newest_by_mtime(&matches).await.clone()),
    })
}

/// The two directories' files as one id → file map.
struct MergedDirs {
    by_id: HashMap<String, FileStat>,
    /// Every `.md` file, an id present in both directories counted once.
    file_count: usize,
    duplicates: Vec<CrossDirDuplicate>,
}

/// Merge the shared and the personal scan, resolving an id present in both
/// with [`cross_dir_winner`] (two file reads per such id, none otherwise).
async fn merge_cross_dir(shared: DirStats, personal: DirStats) -> MergedDirs {
    let mut file_count = shared.file_count + personal.file_count;
    let mut by_id = shared.by_id;
    let mut duplicates = Vec::new();
    for (id, personal_stat) in personal.by_id {
        let Some(shared_stat) = by_id.get(&id) else {
            by_id.insert(id, personal_stat);
            continue;
        };
        let winner = cross_dir_winner(&shared_stat.path, &personal_stat.path).await;
        duplicates.push(CrossDirDuplicate {
            id: id.clone(),
            shared: shared_stat.path.clone(),
            personal: personal_stat.path.clone(),
            winner,
        });
        file_count = file_count.saturating_sub(1);
        if winner == CrossDirWinner::Personal {
            by_id.insert(id, personal_stat);
        }
    }
    MergedDirs {
        by_id,
        file_count,
        duplicates,
    }
}

/// `.md` files in both directories, an id present in both counted once (the
/// `counts` tier: names only, no `statx`, no read).
async fn count_distinct_md_files(shared_dir: &Path, personal_dir: &Path) -> usize {
    async fn ids(dir: &Path) -> (usize, HashSet<String>) {
        let mut ids = HashSet::new();
        let mut count = 0;
        let Ok(mut entries) = async_fs::read_dir(dir).await else {
            return (0, ids);
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("md") {
                count += 1;
                if let Some(id) = stem_id(&path) {
                    ids.insert(id.to_string());
                }
            }
        }
        (count, ids)
    }
    let (shared_count, shared_ids) = ids(shared_dir).await;
    let (personal_count, personal_ids) = ids(personal_dir).await;
    let in_both = shared_ids.intersection(&personal_ids).count();
    (shared_count + personal_count).saturating_sub(in_both)
}

/// Count `.md` files in a directory. Returns 0 if the directory doesn't exist.
#[cfg(test)]
async fn count_md_files(dir: &Path) -> usize {
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

/// One `.md` file's identity on disk, as the staleness check needs it.
pub(crate) struct FileStat {
    pub path: PathBuf,
    pub len: u64,
}

/// A directory's `.md` files, deduplicated by memory id.
pub(crate) struct DirStats {
    /// Winner per id, matching [`scan_dir_to_map`]'s newest-wins rule.
    pub by_id: HashMap<String, FileStat>,
    /// Every `.md` file, including duplicates that lost the id race — this is
    /// what the count tier has always compared against the row count.
    pub file_count: usize,
}

/// The columns an incremental rebuild must compare before it may skip a row.
///
/// Everything here is something a full rebuild would (re)write and that a
/// matching content hash does **not** imply — see the skip predicate in
/// `reindex_dir` for why each one is load-bearing.
struct RowIdentity {
    sha256: String,
    len: u64,
    has_embedding: bool,
}

/// One directory's parse result ([`MemoryStore::scan_dir`]).
struct ScannedDir {
    owned: Vec<(String, Memory, crate::digest::FileDigest)>,
    /// The file each owned id was read from.
    paths: HashMap<String, PathBuf>,
    unparseable: Vec<UnparseableMemoryFile>,
    /// Files renamed because their name hid their id: `(id, new path)`.
    renamed: Vec<(String, PathBuf)>,
}

/// A memory file `reindex` could not parse.
///
/// Its row and vectors are kept as they were (`preserved`) rather than
/// dropped: the file still exists, it is just unreadable right now — most
/// often an unresolved merge conflict — and dropping the row would delete the
/// memory from every query until someone noticed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnparseableMemoryFile {
    pub path: PathBuf,
    /// Recovered from the frontmatter or the filename; `None` when neither
    /// yields an id (nothing can be kept then).
    pub id: Option<String>,
    pub error: String,
    /// Whether an existing row was carried across the rebuild.
    pub preserved: bool,
}

/// What a metadata rebuild did.
///
/// `indexed` counts every memory now represented in the index — skipped rows
/// included, since they are still correctly indexed. `skipped` is how many of
/// those needed no rewrite, and is always 0 for a full rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReindexCounts {
    pub indexed: usize,
    pub skipped: usize,
    /// Files that did not parse. Their rows, vectors and edges were kept
    /// (see [`UnparseableMemoryFile`]); every surface must show these.
    pub unparseable: Vec<UnparseableMemoryFile>,
    /// Ids with a file in both the shared and the personal directory: the
    /// id and the path of the copy that LOST ([`cross_dir_rule`]). The
    /// winner is indexed; the loser is left on disk — it may be a tracked
    /// file git would restore anyway, or a private copy with edits, and an
    /// unattended reindex (it also runs as the forced migration) must not
    /// decide that. An `update` or `delete` of the id removes it.
    pub shadowed: Vec<(String, PathBuf)>,
    /// Files renamed because their name did not carry the id of the memory
    /// inside (so it could not be found by id): `(id, new path)`.
    pub renamed: Vec<(String, PathBuf)>,
}

/// Scan a directory for `.md` files, recording each one's path and length.
///
/// One `statx` per file (`DirEntry::metadata`, which on Linux is served from
/// the `getdents` batch where possible) and no file reads — this is the whole
/// cost of the `size` tier.
///
/// Duplicate ids resolve newest-wins, exactly as [`scan_dir_to_map`] does, so
/// the file the staleness check compares is the same one the read path would
/// serve. A file whose metadata cannot be read is skipped rather than counted
/// with a length of zero, which would report as drift.
async fn scan_dir_to_stats(dir: &Path) -> DirStats {
    let mut by_id: HashMap<String, FileStat> = HashMap::new();
    let mut file_count = 0;
    if !dir.exists() {
        return DirStats { by_id, file_count };
    }
    let Ok(mut entries) = async_fs::read_dir(dir).await else {
        return DirStats { by_id, file_count };
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        file_count += 1;
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        let id = memory_file::extract_id_from_stem(stem).to_string();
        let stat = FileStat {
            path,
            len: meta.len(),
        };
        match by_id.get(&id) {
            Some(existing) => {
                // `prefers_newer`, like every other duplicate resolution: a
                // bare `>=` let equal mtimes resolve by directory order.
                if prefers_newer(
                    (file_mtime(&stat.path).await, &stat.path),
                    (file_mtime(&existing.path).await, &existing.path),
                ) {
                    by_id.insert(id, stat);
                }
            }
            None => {
                by_id.insert(id, stat);
            }
        }
    }
    DirStats { by_id, file_count }
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
                        if prefers_newer(
                            (file_mtime(&path).await, &path),
                            (file_mtime(existing).await, existing),
                        ) {
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
mod tests;
