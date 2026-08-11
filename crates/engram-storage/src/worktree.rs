//! Git worktree consolidation and project-root resolution.
//!
//! A linked git worktree (`<repo>/.git` is a *file* pointing into the main
//! repo's `.git/worktrees/<name>/`) shares code with the main checkout but
//! lives at a different path. Without special handling each worktree would
//! hash to its own project ID and accumulate an independent, invisible memory
//! store.
//!
//! This module centralizes the fix so every entry point — CLI commands and
//! the MCP server, "with or without mcp" — behaves identically:
//!
//! 1. detect that `dir` is a linked worktree and find the main worktree root,
//! 2. ensure the main project's store exists,
//! 3. consolidate any memories that were already written under the worktree's
//!    own stray store into the main project's store,
//! 4. register the worktree as a sub-project of the main project,
//!
//! then route the operation at the main worktree's path.

use super::error::Result;
use super::registry::RegistryBackend;
use super::{memory_file, paths, project_id, MemoryStore};
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;

/// Migrate every memory file in `src_dir` into `main_store`, carrying its
/// embedding vectors over from `wt_store` so search keeps working without
/// re-embedding. Returns `(migrated, left_behind)`.
///
/// Files that can't be read or parsed are skipped (a single corrupt file must
/// not abort consolidation) and counted in `left_behind`. The caller must not
/// delete `src_dir` while that count is non-zero: skipping a file only protects
/// it if something else does not then remove the directory it sits in.
///
/// Re-runs are made safe two ways:
/// - **newest wins**: when main already holds the same ID with an
///   `updated_at` at least as new, the stray copy is dropped instead of
///   migrated — `create` is a full overwrite, so migrating unconditionally
///   would resurrect a stale snapshot (file, index row, AND vectors) over
///   changes made in the main store after a partial run (a crash or a failed
///   `remove_dir_all` leaves the stray store behind while main keeps moving);
/// - **delete-after-durable**: source files are removed only once the batched
///   `create` has committed, so a crash before that point leaves every source
///   file in place for the next run instead of losing the ones already handled.
async fn migrate_dir(
    src_dir: &Path,
    wt_store: &MemoryStore,
    main_store: &MemoryStore,
) -> Result<(usize, usize)> {
    if !src_dir.exists() {
        return Ok((0, 0));
    }

    // Phase 1 — read and parse every stray file up front.
    //
    // The per-file work below needs two lookups against *other* stores
    // (main's current copy, and the worktree's vectors), and both have batch
    // forms. Collecting the parse results first is what makes those batchable
    // — the previous shape interleaved them one memory at a time, so the
    // `get` against main was a full directory scan per stray file, i.e.
    // O(stray * main_size). Unreadable or unparseable files are skipped here
    // exactly as before: a single corrupt file must not abort consolidation —
    // and counted, so the caller knows not to remove the directory holding
    // them.
    let mut pending: Vec<(PathBuf, engram_types::Memory)> = Vec::new();
    let mut left_behind = 0;
    let mut entries = async_fs::read_dir(src_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = async_fs::read_to_string(&path).await else {
            left_behind += 1;
            continue;
        };
        let Ok(memory) = memory_file::parse_memory_file(&content) else {
            left_behind += 1;
            continue;
        };
        pending.push((path, memory));
    }
    if pending.is_empty() {
        return Ok((0, left_behind));
    }

    // Phase 2 — one batched read of main's existing copies, replacing the
    // per-file `get`.
    //
    // A missing row here means main has no copy, which is the common case and
    // the same thing the old `get` reported as `Err(NotFound)`. Unlike the
    // `compress` verification loops, this call site does not need to tell a
    // missing memory from an unreadable one: the old code treated *every*
    // `get` error identically (fall through and migrate), because migrating
    // over an unreadable main-side copy is the correct repair either way.
    let ids: Vec<&str> = pending.iter().map(|(_, m)| m.id.as_str()).collect();
    let existing: std::collections::HashMap<String, engram_types::Memory> = main_store
        .get_batch(&ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();

    // Newest wins (see doc comment): a STRICTLY newer copy in main means this
    // stray file is a leftover from an earlier (partial) consolidation whose
    // memory has since been edited — drop it, don't resurrect it. Equal
    // timestamps re-migrate: a crash between main's `create` and the chunk
    // relocation below leaves an equal-updated_at copy in main with NO
    // vectors, and dropping the stray file then would lose them; re-migrating
    // the identical snapshot is idempotent (create is a full overwrite, chunk
    // upsert replaces in place).
    let mut to_migrate = Vec::with_capacity(pending.len());
    for (path, memory) in pending {
        match existing.get(&memory.id) {
            Some(e) if e.updated_at > memory.updated_at => {
                let _ = async_fs::remove_file(&path).await;
            }
            _ => to_migrate.push((path, memory)),
        }
    }
    if to_migrate.is_empty() {
        return Ok((0, left_behind));
    }

    // Phase 3 — pull all the vectors out of the worktree store in one scan
    // rather than one `export_chunks` query per memory.
    let migrate_ids: Vec<&str> = to_migrate.iter().map(|(_, m)| m.id.as_str()).collect();
    let mut chunks_by_id = wt_store
        .export_chunks_batch(&migrate_ids)
        .await
        .unwrap_or_default();

    // Phase 4 — write into main with a single batched create.
    //
    // This is the term that actually dominates consolidation. A per-memory
    // `create` pays four separate O(main store size) operations (ID probe,
    // chunk-presence probe, index commit, manifest stats scan), so migrating
    // W memories into a store of M was quadratic — measured at 24 ms per
    // create into an empty store rising to 582 ms into a 1000-memory one.
    // Batching the *lookups* around it, as an earlier pass did, changed
    // nothing measurable precisely because this was the floor.
    let batch: Vec<engram_types::Memory> = to_migrate.iter().map(|(_, m)| m.clone()).collect();
    main_store.create_batch(&batch).await?;
    let migrated = batch.len();

    // Source files are consumed only once the whole batch is durable. The
    // per-file version deleted each source immediately after its own create,
    // so a crash mid-loop left the remainder for the next run; with one
    // batched commit there is no mid-loop to crash in — either every memory
    // is in main or none is, and in the latter case every source file is
    // still present for a re-run.
    let mut relocate: Vec<(String, chrono::DateTime<chrono::Utc>, Vec<Vec<f32>>)> = Vec::new();
    for (path, memory) in &to_migrate {
        if let Some(chunks) = chunks_by_id.remove(&memory.id) {
            if !chunks.is_empty() {
                relocate.push((memory.id.clone(), memory.updated_at, chunks));
            }
        }
        let _ = async_fs::remove_file(path).await;
    }

    // Phase 5 — one batched vector upsert. Ordering is unchanged from the
    // per-memory version: every file and index row is durable in main before
    // any vector is attached, so a crash here leaves memories that are
    // present but not yet semantically searchable — recoverable by a
    // `reindex --embeddings-only` — never vectors pointing at absent
    // memories.
    //
    // This is the *guarded* batch (the batched `upsert_chunks_if_current`),
    // where the per-memory original used the unguarded `upsert_chunks`. The
    // snapshot is the `updated_at` we just wrote via `create`, so the guard
    // is a no-op unless another writer changed the memory in between — and in
    // that case dropping the vectors is the correct outcome, not a
    // regression: attaching the worktree's vectors to content that has since
    // been edited would leave the memory silently mis-embedded, where
    // skipping leaves it merely un-embedded and a `reindex
    // --embeddings-only` away from correct.
    if !relocate.is_empty() {
        main_store.upsert_chunks_batch(relocate).await?;
    }

    Ok((migrated, left_behind))
}

/// Consolidate a linked worktree's stray memory store into the main project.
///
/// Moves both shared (`<worktree>/.engramdb/memories/`) and personal (keyed by
/// the worktree's project ID under the global data dir) memories into the main
/// project — **carrying their embedding vectors along** so the migrated
/// memories remain searchable without re-embedding — then removes the stray
/// store so all future operations route to the main project.
///
/// Returns the number of memories migrated. Idempotent: a no-op when the
/// worktree has no stray store (the common case once linked).
pub async fn consolidate_worktree_into_main(worktree_dir: &Path, main_dir: &Path) -> Result<usize> {
    let wt_id = project_id::compute_project_id(worktree_dir);
    let main_id = project_id::compute_project_id(main_dir);

    // Identical IDs would mean removing the stray store nukes the real one.
    if wt_id == main_id {
        return Ok(0);
    }

    let wt_engramdb = paths::project_dir(worktree_dir);
    let mut moved = 0;
    // Files `migrate_dir` could not read or parse. They are still data, and
    // they are the ONLY copy — so the directory holding them is not ours to
    // remove, however tidy that would be.
    let mut stranded_shared = 0;

    // Only migrate when the worktree actually has a stray store AND the main
    // store exists (the caller guarantees the latter before routing to it;
    // refusing to delete the stray store otherwise avoids data loss).
    if wt_engramdb.exists() && paths::project_dir(main_dir).exists() {
        let wt_store = MemoryStore::open(worktree_dir).await?;
        let main_store = MemoryStore::open(main_dir).await?;

        // Shared memories live in the worktree's own .engramdb/; personal
        // ones are keyed by the worktree's project ID in the global data dir.
        let (n, skipped) =
            migrate_dir(&paths::memories_dir(worktree_dir), &wt_store, &main_store).await?;
        moved += n;
        stranded_shared += skipped;
        if let Ok(wt_personal) = paths::personal_memories_dir(&wt_id) {
            let (n, _) = migrate_dir(&wt_personal, &wt_store, &main_store).await?;
            moved += n;
        }

        // Fold the worktree's harvest ledger into the main project's before
        // the directory goes.
        //
        // `.engramdb/state/harvest_ledger.jsonl` is the append-only review
        // record — decision, note and memory ids per session. It is
        // gitignored and machine-local, so unlike the memory files it has no
        // copy anywhere, and `migrate_dir` does not look at it: it only walks
        // `memories/`. A directory used standalone before it became a linked
        // worktree therefore carried a full review history straight into the
        // `remove_dir_all` below.
        //
        // `adopt_ledger` is the same operation the harvest flow already runs
        // for linked sub-projects, and concatenation IS the merge here: the
        // ledger is a patch log folded in timestamp order, so appending one
        // log to another needs no precedence rule. It also migrates a legacy
        // `harvested_sessions.json` and refuses a planted symlink on the way.
        let ledger_adopted = match crate::harvest_state::adopt_ledger(worktree_dir, main_dir) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    "could not fold the harvest ledger at {} into {} ({e}); \
                         keeping the worktree store so the review record is not lost",
                    worktree_dir.display(),
                    main_dir.display()
                );
                false
            }
        };

        // Remove the stray worktree store so future ops route to main — but
        // only once nothing is left in it that exists nowhere else. Routing
        // does not depend on this (`detect_worktree_main` decides that), so
        // keeping the directory costs a re-scan per invocation and nothing
        // else — which is the whole reason a failed adoption can block the
        // delete rather than being swallowed. The next invocation retries.
        //
        // `state/session_tasks.json` does go with the directory. That one is
        // deliberately not preserved: it maps a live session to its task and
        // the SessionEnd hook clears it anyway, so it is per-session scratch
        // rather than a record of anything.
        if stranded_shared == 0 && ledger_adopted {
            async_fs::remove_dir_all(&wt_engramdb).await?;
        }
    }

    // Drop the worktree's stale global data dir (its now-migrated personal
    // memories and obsolete LanceDB index) — unless personal memories are still
    // in it, which means `migrate_dir` could not read them and they exist
    // nowhere else.
    //
    // The decision reads the directory itself rather than a count from the
    // block above, and that is load-bearing: this runs on EVERY command in a
    // linked worktree, and the block above is skipped once the stray
    // `.engramdb` is gone. A counter would be zero on the second invocation for
    // want of having looked, and would delete exactly what the first
    // invocation preserved.
    if let Ok(global_data) = paths::global_data_dir() {
        let wt_global = global_data.join("projects").join(&wt_id);
        if wt_global.exists() {
            paths::reclaim_project_data_dir(&wt_global).await;
        }
    }

    Ok(moved)
}

/// Resolve `dir` to the project root that should own its memory operations.
///
/// When `dir` is a linked git worktree this ensures the main project's store
/// exists, consolidates any stray worktree memories into it, registers the
/// worktree as a sub-project, and returns the main worktree's path.
///
/// For a main worktree, a plain non-git directory, or a malformed worktree
/// pointer, returns `dir` unchanged. Idempotent and cheap on the common path
/// (a single `.git` stat), so it is safe to call on every invocation.
pub async fn resolve_project_root(dir: &Path, registry: &dyn RegistryBackend) -> Result<PathBuf> {
    let Some(main) = project_id::detect_worktree_main(dir) else {
        return Ok(dir.to_path_buf());
    };

    // The main project's store must exist before operations route to it.
    if !paths::project_dir(&main).exists() {
        MemoryStore::init(&main, registry).await?;
    }

    // Pull any memories written under the worktree's stray store into main.
    consolidate_worktree_into_main(dir, &main).await?;

    // Register the worktree as a sub-project so its ID/path resolves to main.
    let child_id = project_id::compute_project_id(dir);
    let parent_id = project_id::compute_project_id(&main);
    if child_id != parent_id {
        registry
            .update_with_parent(dir, &child_id, Some(&parent_id))
            .await?;
    }

    Ok(main)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryRegistry, RegistryBackend};
    use engram_types::{Memory, MemoryType, Provenance};
    use std::fs;
    use tempfile::TempDir;

    /// Build a fake main + linked-worktree layout mirroring git's structure.
    /// Returns `(main_path, worktree_path)`.
    fn make_fake_worktree(root: &Path) -> (PathBuf, PathBuf) {
        let main = root.join("main");
        let wt = root.join("wt");
        let wt_gitdir = main.join(".git").join("worktrees").join("wt");
        fs::create_dir_all(main.join(".git")).unwrap();
        fs::create_dir_all(&wt).unwrap();
        fs::create_dir_all(&wt_gitdir).unwrap();
        fs::write(wt_gitdir.join("commondir"), "../..").unwrap();
        fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();
        (main, wt)
    }

    #[tokio::test]
    async fn resolve_project_root_returns_dir_for_non_worktree() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let resolved = resolve_project_root(tmp.path(), &registry).await.unwrap();
        assert_eq!(resolved, tmp.path());
        // Nothing registered for a plain directory.
        assert!(registry.load().await.unwrap().projects.is_empty());
    }

    #[tokio::test]
    async fn resolve_project_root_in_worktree_inits_main_and_links() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();

        let resolved = resolve_project_root(&wt, &registry).await.unwrap();
        // `resolve_project_root` canonicalizes its result (on macOS
        // `$TMPDIR` is a `/var` -> `/private/var` symlink), so compare the
        // fully-resolved form on both sides — symlink-agnostic and a no-op
        // on platforms without the indirection.
        assert_eq!(
            resolved.canonicalize().unwrap(),
            main.canonicalize().unwrap()
        );

        // Main got initialized; the worktree never gets its own store.
        assert!(main.join(".engramdb").exists());
        assert!(!wt.join(".engramdb").exists());

        let reg = registry.load().await.unwrap();
        let main_id = project_id::compute_project_id(&main);
        let wt_id = project_id::compute_project_id(&wt);
        let wt_entry = reg
            .projects
            .iter()
            .find(|e| e.project_id == wt_id)
            .expect("worktree registered");
        assert_eq!(
            wt_entry.parent_project_id.as_deref(),
            Some(main_id.as_str())
        );
    }

    #[tokio::test]
    async fn consolidate_moves_stray_worktree_memories_into_main() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();

        // Simulate the broken state: a memory written into the worktree's own
        // stray store before linking existed.
        let main_store = MemoryStore::init(&main, &registry).await.unwrap();
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        let mem = Memory::new(
            MemoryType::Decision,
            "Strand in worktree",
            "This was created before the worktree was linked",
            Provenance::human(),
        );
        let mem_id = wt_store.create(&mem).await.unwrap();
        assert!(wt.join(".engramdb").exists());

        let moved = consolidate_worktree_into_main(&wt, &main).await.unwrap();
        assert_eq!(moved, 1, "the stray memory should be migrated");

        // Stray store removed; memory now lives in (and is indexed by) main.
        assert!(!wt.join(".engramdb").exists());
        let summaries = main_store.list_summary().await.unwrap();
        assert_eq!(summaries.len(), 1);
        let migrated = main_store.get(&mem_id).await.unwrap();
        assert_eq!(migrated.summary, "Strand in worktree");
    }

    #[tokio::test]
    async fn consolidate_carries_embeddings_over_to_main() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();

        MemoryStore::init(&main, &registry).await.unwrap();
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        let mem = Memory::new(
            MemoryType::Decision,
            "Embedded in worktree",
            "Has a vector that must survive consolidation",
            Provenance::human(),
        );
        let mem_id = wt_store.create(&mem).await.unwrap();
        // Embedding produced earlier (e.g. by the MCP background embedder).
        wt_store
            .upsert_chunks(&mem_id, vec![vec![0.25f32; 384]])
            .await
            .unwrap();

        let moved = consolidate_worktree_into_main(&wt, &main).await.unwrap();
        assert_eq!(moved, 1);

        // The vector moved with the memory: it is queryable in main and not
        // silently dropped (which would require a costly re-embed).
        let main_store = MemoryStore::open(&main).await.unwrap();
        let chunks = main_store.export_chunks(&mem_id).await.unwrap();
        assert_eq!(chunks.len(), 1, "embedding must be carried into main");
        assert_eq!(chunks[0].len(), 384);
        let hits = main_store
            .vector_search(vec![0.25f32; 384], 5, None)
            .await
            .unwrap();
        assert!(
            hits.iter().any(|m| m.id == mem_id),
            "migrated memory must be vector-searchable in main"
        );
    }

    /// Regression: a consolidation re-run (crash / failed `remove_dir_all`
    /// left the stray store behind) must NOT resurrect the stale worktree
    /// snapshot over a copy that was updated in the main store since.
    #[tokio::test]
    async fn consolidate_rerun_does_not_resurrect_stale_content() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();

        let main_store = MemoryStore::init(&main, &registry).await.unwrap();
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        let mem = Memory::new(
            MemoryType::Decision,
            "Original summary",
            "Original content",
            Provenance::human(),
        );
        let mem_id = wt_store.create(&mem).await.unwrap();

        // First consolidation migrates the memory into main.
        assert_eq!(consolidate_worktree_into_main(&wt, &main).await.unwrap(), 1);

        // Simulate the partial-run leftover: the stray store reappears with
        // the ORIGINAL (now stale) snapshot still in it.
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        wt_store.create(&mem).await.unwrap();
        wt_store
            .upsert_chunks(&mem_id, vec![vec![0.1f32; 384]])
            .await
            .unwrap();

        // The main copy moves on (newer updated_at).
        main_store
            .update_with(&mem_id, |m| {
                m.summary = "Updated in main".to_string();
                Ok(())
            })
            .await
            .unwrap();
        main_store
            .upsert_chunks(&mem_id, vec![vec![0.9f32; 384]])
            .await
            .unwrap();

        // Re-run: the stale stray copy must be dropped, not migrated.
        assert_eq!(consolidate_worktree_into_main(&wt, &main).await.unwrap(), 0);
        assert!(!wt.join(".engramdb").exists());

        let after = main_store.get(&mem_id).await.unwrap();
        assert_eq!(
            after.summary, "Updated in main",
            "newer main copy must survive a consolidation re-run"
        );
        let chunks = main_store.export_chunks(&mem_id).await.unwrap();
        assert_eq!(chunks.len(), 1);
        assert!(
            (chunks[0][0] - 0.9).abs() < f32::EPSILON,
            "main's newer vectors must not be overwritten by the stale copy"
        );
    }

    /// A file `migrate_dir` cannot parse is skipped so it is not destroyed —
    /// but the stray store was then removed wholesale, which destroyed it
    /// anyway. Skipping only protects a file if the directory survives too.
    #[tokio::test]
    async fn a_corrupt_stray_file_is_not_deleted_with_the_store() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();
        MemoryStore::init(&main, &registry).await.unwrap();

        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        let good = Memory::new(
            MemoryType::Decision,
            "Migrates fine",
            "content",
            Provenance::human(),
        );
        let good_id = wt_store.create(&good).await.unwrap();
        let corrupt = paths::memories_dir(&wt).join("corrupt.md");
        async_fs::write(&corrupt, "not frontmatter at all")
            .await
            .unwrap();

        let moved = consolidate_worktree_into_main(&wt, &main).await.unwrap();
        assert_eq!(moved, 1, "the readable memory still migrates");
        let main_store = MemoryStore::open(&main).await.unwrap();
        assert!(main_store.get(&good_id).await.is_ok());

        assert!(
            corrupt.exists(),
            "an unparseable file is the only copy of its data — it must survive"
        );
        assert_eq!(
            async_fs::read_to_string(&corrupt).await.unwrap(),
            "not frontmatter at all"
        );
    }

    /// A mixed batch: some stray files are stale (main is newer, drop them),
    /// some are fresh (migrate them), in one consolidation.
    ///
    /// The single-memory tests above cannot catch a partitioning bug in the
    /// batched migrate path — dropping the wrong file, migrating a stale
    /// snapshot, or losing the count — because with one memory every
    /// partition is trivially correct.
    #[tokio::test]
    async fn consolidate_mixed_batch_drops_only_the_stale_entries() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();

        let main_store = MemoryStore::init(&main, &registry).await.unwrap();
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();

        // Two memories that already exist in main and will be advanced there,
        // so their stray copies are stale by the time we consolidate.
        let mut stale_ids = Vec::new();
        for i in 0..2 {
            let mem = Memory::new(
                MemoryType::Decision,
                format!("Stale {}", i),
                "Original content",
                Provenance::human(),
            );
            let id = wt_store.create(&mem).await.unwrap();
            main_store.create(&mem).await.unwrap();
            main_store
                .update_with(&id, |m| {
                    m.summary = format!("Updated in main {}", i);
                    Ok(())
                })
                .await
                .unwrap();
            stale_ids.push(id);
        }

        // Three memories that exist only in the worktree — these must migrate,
        // vectors and all.
        let mut fresh_ids = Vec::new();
        for i in 0..3 {
            let mem = Memory::new(
                MemoryType::Convention,
                format!("Fresh {}", i),
                "Worktree-only content",
                Provenance::human(),
            );
            let id = wt_store.create(&mem).await.unwrap();
            wt_store
                .upsert_chunks(&id, vec![vec![0.25f32; 384]])
                .await
                .unwrap();
            fresh_ids.push(id);
        }

        let migrated = consolidate_worktree_into_main(&wt, &main).await.unwrap();
        assert_eq!(migrated, 3, "only the three worktree-only memories migrate");

        for (i, id) in stale_ids.iter().enumerate() {
            let m = main_store.get(id).await.unwrap();
            assert_eq!(
                m.summary,
                format!("Updated in main {}", i),
                "a stale stray copy must not overwrite the newer main copy"
            );
        }

        for (i, id) in fresh_ids.iter().enumerate() {
            let m = main_store.get(id).await.unwrap();
            assert_eq!(m.summary, format!("Fresh {}", i));
            let chunks = main_store.export_chunks(id).await.unwrap();
            assert_eq!(
                chunks.len(),
                1,
                "vectors must relocate for every migrated memory"
            );
            assert!((chunks[0][0] - 0.25).abs() < f32::EPSILON);
        }

        assert!(!wt.join(".engramdb").exists(), "stray store is removed");
    }

    /// The first run strands an unparseable personal file and keeps its
    /// directory. The SECOND run must keep it too — `consolidate` runs on every
    /// command in a worktree, and by then the stray `.engramdb` is gone, so a
    /// count carried from the migration block is zero for want of having
    /// looked and deletes exactly what the first run preserved.
    /// The worktree's harvest ledger is folded into the main project's before
    /// the stray store is deleted.
    ///
    /// `.engramdb/state/harvest_ledger.jsonl` is gitignored and machine-local,
    /// so it has no copy in the project tree the way memory files do, and
    /// `migrate_dir` never sees it — it walks `memories/` only. A directory
    /// used standalone before it became a linked worktree carried a full
    /// review history (decisions, notes, memory ids) straight into the
    /// `remove_dir_all`.
    #[tokio::test]
    async fn the_worktrees_harvest_ledger_is_merged_into_main_before_deletion() {
        use crate::harvest_state::{self, HarvestDecision};

        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();
        MemoryStore::init(&main, &registry).await.unwrap();
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();

        // A shared memory that migrates cleanly, so the stray store is
        // eligible for removal — the exact case that used to lose the ledger.
        let good = Memory::new(MemoryType::Decision, "Clean", "c", Provenance::human());
        wt_store.create(&good).await.unwrap();

        // Review records on BOTH sides: the fold has to be a merge, not a
        // move that clobbers what main already knew.
        harvest_state::mark_harvested(
            &main,
            "session-main",
            &["mem-main".to_string()],
            HarvestDecision::Harvested,
            None,
        )
        .unwrap();
        harvest_state::mark_harvested(
            &wt,
            "session-worktree",
            &["mem-wt".to_string()],
            HarvestDecision::Skipped,
            Some("not worth keeping".to_string()),
        )
        .unwrap();

        consolidate_worktree_into_main(&wt, &main).await.unwrap();

        assert!(
            !wt.join(".engramdb").exists(),
            "the stray store should be gone once everything in it was carried across"
        );
        let folded = harvest_state::read_harvested(&main);
        assert_eq!(
            folded.len(),
            2,
            "both sides' review records must survive the fold: {folded:?}"
        );
        let adopted = folded
            .get("session-worktree")
            .expect("the worktree's review record was lost with its directory");
        assert_eq!(adopted.decision, Some(HarvestDecision::Skipped));
        assert_eq!(adopted.note.as_deref(), Some("not worth keeping"));
        assert!(folded.contains_key("session-main"));
    }

    /// A ledger that cannot be folded blocks the delete rather than being
    /// destroyed by it.
    ///
    /// Keeping the directory costs a re-scan per invocation and nothing else —
    /// which is precisely why a failed adoption is allowed to stop the
    /// removal instead of being swallowed. The next invocation retries.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_ledger_that_cannot_be_folded_keeps_the_worktree_store() {
        use crate::harvest_state::{self, HarvestDecision};
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();
        MemoryStore::init(&main, &registry).await.unwrap();
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        let good = Memory::new(MemoryType::Decision, "Clean", "c", Provenance::human());
        wt_store.create(&good).await.unwrap();

        harvest_state::mark_harvested(
            &wt,
            "session-worktree",
            &["mem-wt".to_string()],
            HarvestDecision::Harvested,
            None,
        )
        .unwrap();

        // Make the worktree's own state dir unwritable so `adopt_ledger`
        // cannot rename the log aside — the step that commits the adoption.
        let wt_state = wt.join(".engramdb").join("state");
        let set_mode = |mode: u32| {
            let mut perms = fs::metadata(&wt_state).unwrap().permissions();
            perms.set_mode(mode);
            fs::set_permissions(&wt_state, perms).unwrap();
        };
        set_mode(0o500);
        // Probe without touching the ledger: renaming it aside to test the
        // rename would be exactly the destructive step under test.
        let probe = wt_state.join(".write-probe");
        let blocked = fs::write(&probe, b"x").is_err();
        let _ = fs::remove_file(&probe);

        let result = consolidate_worktree_into_main(&wt, &main).await;
        // Restore only if the directory is still there — under root the
        // adoption succeeds and the whole store is removed, which is the
        // outcome the skip below accounts for.
        if wt_state.exists() {
            set_mode(0o755);
        }
        result.unwrap();

        // Root ignores the mode bits, so on a root runner there is nothing to
        // block and the assertion is skipped. CI runs as a normal user.
        if blocked {
            assert!(
                wt.join(".engramdb").exists(),
                "an unfoldable ledger must keep its directory, not be deleted with it"
            );
            assert!(
                wt_state.join("harvest_ledger.jsonl").exists(),
                "the review record must still be on disk for the next attempt"
            );
        }
    }

    #[tokio::test]
    async fn a_stranded_personal_file_survives_the_next_consolidation() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();
        MemoryStore::init(&main, &registry).await.unwrap();
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();

        // A shared memory that migrates cleanly, so the stray `.engramdb` is
        // removed on run 1 and the block is skipped on run 2.
        let good = Memory::new(MemoryType::Decision, "Clean", "c", Provenance::human());
        wt_store.create(&good).await.unwrap();

        let wt_id = project_id::compute_project_id(&wt);
        let personal = paths::personal_memories_dir(&wt_id).unwrap();
        async_fs::create_dir_all(&personal).await.unwrap();
        let stranded = personal.join("unparseable.md");
        async_fs::write(&stranded, "written by a newer schema")
            .await
            .unwrap();

        consolidate_worktree_into_main(&wt, &main).await.unwrap();
        assert!(
            !wt.join(".engramdb").exists(),
            "run 1 clears the stray store"
        );
        assert!(stranded.exists(), "run 1 must keep the only copy");

        consolidate_worktree_into_main(&wt, &main).await.unwrap();
        assert!(
            stranded.exists(),
            "run 2 deleted what run 1 preserved — the decision must read the \
             directory, not a count from a block that no longer runs"
        );
    }

    #[tokio::test]
    async fn consolidate_is_noop_without_stray_store() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();
        MemoryStore::init(&main, &registry).await.unwrap();

        let moved = consolidate_worktree_into_main(&wt, &main).await.unwrap();
        assert_eq!(moved, 0);
        assert!(!wt.join(".engramdb").exists());
    }

    #[tokio::test]
    async fn resolve_project_root_consolidates_then_links_end_to_end() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();

        // Memory exists only in the worktree's stray store.
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        let mem = Memory::new(
            MemoryType::Hazard,
            "Pre-link hazard",
            "Stored before resolution ran",
            Provenance::human(),
        );
        wt_store.create(&mem).await.unwrap();

        let resolved = resolve_project_root(&wt, &registry).await.unwrap();
        // `resolve_project_root` canonicalizes its result (on macOS
        // `$TMPDIR` is a `/var` -> `/private/var` symlink), so compare the
        // fully-resolved form on both sides — symlink-agnostic and a no-op
        // on platforms without the indirection.
        assert_eq!(
            resolved.canonicalize().unwrap(),
            main.canonicalize().unwrap()
        );

        // The memory is now owned by the main project and the link exists.
        let main_store = MemoryStore::open(&main).await.unwrap();
        assert_eq!(main_store.list_summary().await.unwrap().len(), 1);

        let reg = registry.load().await.unwrap();
        let main_id = project_id::compute_project_id(&main);
        let wt_id = project_id::compute_project_id(&wt);
        let wt_entry = reg.projects.iter().find(|e| e.project_id == wt_id).unwrap();
        assert_eq!(
            wt_entry.parent_project_id.as_deref(),
            Some(main_id.as_str())
        );
    }
}
