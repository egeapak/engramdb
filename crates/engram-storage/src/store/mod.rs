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
use super::{lance_index, manifest, memory_file, paths, project_id, registry, write_lock};
use crate::config::{load_config, load_config_or_default};
use crate::digest::FileDigest;
use crate::staleness::StalenessFindings;
use chrono::{DateTime, Utc};
use engram_types::config::{IndexConfig, StalenessCheck};
use engram_types::{Memory, MemoryType, MemoryUpdate, Visibility};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;

mod chunks;
mod edges;
mod fingerprint;
mod list;
mod open;
mod reindex;
mod staleness;
mod write;

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
