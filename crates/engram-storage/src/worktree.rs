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
/// embedding vectors over from `wt_store` (when there is one) so search keeps
/// working without re-embedding. Returns `(migrated, left_behind)`.
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
    wt_store: Option<&MemoryStore>,
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
        let Ok(memory) = memory_file::parse_memory_file_from(&content, &path) else {
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
    //
    // Without a readable worktree index (`wt_store` is `None`) the memories
    // move without vectors and re-embed on the next pass.
    let mut chunks_by_id = match wt_store {
        Some(store) => store
            .export_chunks_batch(&migrate_ids)
            .await
            .unwrap_or_default(),
        None => Default::default(),
    };
    // Carry each memory's embed digest across with its vectors. The digest is
    // salted with the embedding model, dimensions, composition and chunk size,
    // so copying it verbatim is self-validating and needs no comparison of the
    // two stores' configs: the worktree store loads its OWN `config.toml`, and
    // if any of those differ, the copied digest is simply not what the main
    // store's engine would compute, so the memory re-embeds on the next pass.
    // When they match — the overwhelmingly common case, since a worktree is a
    // checkout of the same project — the digest matches too and the relocated
    // vectors are correctly recognised as current.
    let mut digests_by_id = match wt_store {
        Some(store) => store.embed_digests().await.unwrap_or_default(),
        None => Default::default(),
    };

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
    let mut relocate: Vec<crate::lance_index::GuardedChunkWrite> = Vec::new();
    for (path, memory) in &to_migrate {
        if let Some(chunks) = chunks_by_id.remove(&memory.id) {
            if !chunks.is_empty() {
                let digest = digests_by_id.remove(&memory.id).flatten();
                relocate.push((memory.id.clone(), memory.updated_at, chunks, digest));
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

/// What the `.engramdb/` directory inside a linked worktree is.
///
/// A repository that commits its memories (`init` gitignores only `state/`)
/// has `.engramdb/` in **every** worktree as an ordinary checkout. That is
/// not a stray store: it belongs to git and to the branch checked out there.
/// Only a directory git does not track can be one EngramDB created itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeStore {
    /// No `.engramdb/` in the worktree.
    Absent,
    /// git tracks at least one file under it: a checkout. EngramDB writes
    /// nothing under it, deletes nothing under it and copies nothing out of it.
    Tracked,
    /// git tracks nothing under it: a stray store, safe to consolidate.
    Untracked,
    /// git could not be asked (not on `PATH`, timed out, refused the
    /// repository). Treated exactly like [`WorktreeStore::Tracked`]: when in
    /// doubt the directory stays.
    Unknown,
}

/// How long the `git ls-files` child may run before the answer is `Unknown`.
const GIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Ask git whether the worktree's `.engramdb/` holds any tracked file.
///
/// Runs `git -C <worktree> ls-files --cached -z -- .engramdb` with the
/// repository-selecting variables removed from the child's environment (a
/// hook started from inside `git commit` inherits `GIT_DIR` and
/// `GIT_INDEX_FILE`, which would answer for another index), no optional
/// locks, no stdin and a 2 s timeout. Every failure is `Unknown`.
pub async fn classify_worktree_store(worktree_dir: &Path) -> WorktreeStore {
    if std::fs::symlink_metadata(paths::project_dir(worktree_dir)).is_err() {
        return WorktreeStore::Absent;
    }
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C")
        .arg(worktree_dir)
        .args(["ls-files", "--cached", "-z", "--", ".engramdb"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_NAMESPACE",
        "GIT_PREFIX",
    ] {
        cmd.env_remove(var);
    }
    match tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => {
            if out.stdout.is_empty() {
                WorktreeStore::Untracked
            } else {
                WorktreeStore::Tracked
            }
        }
        _ => WorktreeStore::Unknown,
    }
}

/// `(len, mtime)` of a worktree's git index: the only file whose change can
/// alter what `git ls-files --cached` answers.
type IndexSig = (u64, u64, u32);

fn index_signature(worktree_dir: &Path) -> Option<IndexSig> {
    let link = project_id::detect_worktree(worktree_dir)?;
    let meta = std::fs::metadata(link.gitdir.join("index")).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some((meta.len(), mtime.as_secs(), mtime.subsec_nanos()))
}

fn tracked_memo() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, IndexSig>> {
    static MEMO: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, IndexSig>>,
    > = std::sync::OnceLock::new();
    MEMO.get_or_init(Default::default)
}

/// The machine-local memo file for one worktree, under the **main**
/// project's gitignored `state/` (never under the worktree's checkout).
fn tracked_memo_path(main_dir: &Path, wt_id: &str) -> PathBuf {
    paths::project_dir(main_dir)
        .join("state")
        .join("worktree_store")
        .join(wt_id)
}

fn format_sig(sig: IndexSig) -> String {
    format!("{} {} {}", sig.0, sig.1, sig.2)
}

/// Whether a `Tracked` verdict is already recorded for this index state.
///
/// Only `Tracked` is ever memoized, so a stale memo can only say "leave the
/// directory alone". The process-wide map serves the daemon (where hooks
/// run); the file lets one-shot hook processes skip the git spawn too.
fn tracked_memo_hit(worktree_dir: &Path, main_dir: &Path, wt_id: &str, sig: IndexSig) -> bool {
    if let Ok(memo) = tracked_memo().lock() {
        if memo.get(worktree_dir) == Some(&sig) {
            return true;
        }
    }
    let on_disk = crate::state_file::read_state_file(&tracked_memo_path(main_dir, wt_id))
        .ok()
        .flatten();
    if on_disk.as_deref().map(str::trim) == Some(format_sig(sig).as_str()) {
        if let Ok(mut memo) = tracked_memo().lock() {
            memo.insert(worktree_dir.to_path_buf(), sig);
        }
        return true;
    }
    false
}

fn remember_tracked(worktree_dir: &Path, main_dir: &Path, wt_id: &str, sig: IndexSig) {
    if let Ok(mut memo) = tracked_memo().lock() {
        memo.insert(worktree_dir.to_path_buf(), sig);
    }
    // Best-effort: without the file every one-shot process asks git again.
    let _ =
        crate::state_file::write_state_file(&tracked_memo_path(main_dir, wt_id), &format_sig(sig));
}

/// Warn once per process and worktree that git could not be asked.
fn warn_unknown_once(worktree_dir: &Path) {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    let first = WARNED
        .get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(worktree_dir.to_path_buf()))
        .unwrap_or(false);
    if first {
        tracing::warn!(
            "git could not be asked whether {} is tracked; the worktree-local \
             .engramdb/ is left alone (operations still go to the main worktree)",
            paths::project_dir(worktree_dir).display()
        );
    }
}

/// Consolidate a linked worktree's stray memory store into the main project.
///
/// **A worktree `.engramdb/` that git tracks is a checkout, not a store**:
/// nothing is written under it, deleted under it or copied out of it. Only a
/// directory git reports as untracked ([`WorktreeStore::Untracked`]) is
/// consolidated; when git cannot be asked the directory stays
/// ([`WorktreeStore::Unknown`]), so without git on `PATH` a stray store is
/// simply never consolidated. Routing to the main worktree never depended on
/// this function.
///
/// For an untracked stray store this moves both shared
/// (`<worktree>/.engramdb/memories/`) and personal (keyed by the worktree's
/// project ID under the global data dir) memories into the main project —
/// **carrying their embedding vectors along** so the migrated memories remain
/// searchable without re-embedding — then removes the stray store.
///
/// Personal memories keyed by the worktree's id are not in the repository, so
/// they move in every case. For a tracked (or unknown) directory the
/// worktree's index is opened read-only for the vectors; if that open fails
/// the memories move without vectors and re-embed (warned).
///
/// Returns the number of memories migrated. Idempotent: a no-op when the
/// worktree has no stray store (the common case once linked).
pub async fn consolidate_worktree_into_main(worktree_dir: &Path, main_dir: &Path) -> Result<usize> {
    consolidate_with(worktree_dir, main_dir, |dir| async move {
        classify_worktree_store(&dir).await
    })
    .await
}

/// [`consolidate_worktree_into_main`] with the git question injectable.
pub(crate) async fn consolidate_with<F, Fut>(
    worktree_dir: &Path,
    main_dir: &Path,
    classify: F,
) -> Result<usize>
where
    F: Fn(PathBuf) -> Fut,
    Fut: std::future::Future<Output = WorktreeStore>,
{
    let wt_id = project_id::compute_project_id(worktree_dir);
    let main_id = project_id::compute_project_id(main_dir);

    // Identical IDs would mean removing the stray store nukes the real one.
    if wt_id == main_id {
        return Ok(0);
    }

    let wt_engramdb = paths::project_dir(worktree_dir);
    let main_exists = paths::project_dir(main_dir).exists();
    let mut moved = 0;

    let verdict = if std::fs::symlink_metadata(&wt_engramdb).is_err() {
        WorktreeStore::Absent
    } else {
        // The signature is read BEFORE git is asked: an index that changes in
        // between leaves a memo for a state that is already gone, which the
        // next call does not match.
        let sig = index_signature(worktree_dir);
        if sig.is_some_and(|sig| tracked_memo_hit(worktree_dir, main_dir, &wt_id, sig)) {
            WorktreeStore::Tracked
        } else {
            let verdict = classify(worktree_dir.to_path_buf()).await;
            if let (WorktreeStore::Tracked, Some(sig)) = (verdict, sig) {
                if main_exists {
                    remember_tracked(worktree_dir, main_dir, &wt_id, sig);
                }
            }
            verdict
        }
    };

    match verdict {
        WorktreeStore::Absent => {}
        WorktreeStore::Untracked => {
            // Only migrate when the main store exists (the caller guarantees
            // it before routing to it; refusing to delete the stray store
            // otherwise avoids data loss).
            if main_exists {
                moved += consolidate_stray_store(worktree_dir, main_dir, &wt_id, &classify).await?;
            }
        }
        WorktreeStore::Tracked | WorktreeStore::Unknown => {
            if verdict == WorktreeStore::Unknown {
                warn_unknown_once(worktree_dir);
            }
            // The checkout is left alone. Personal memories keyed by the
            // worktree's id live in the global data dir, not in the
            // repository, so they still move. The check below is a single
            // `read_dir` of a directory that normally does not exist: no
            // store is opened on the steady-state path.
            if main_exists {
                if let Ok(wt_personal) = paths::personal_memories_dir(&wt_id) {
                    if paths::holds_files_with_extension(&wt_personal, "md").await {
                        let wt_store = match MemoryStore::open_read_only(worktree_dir).await {
                            Ok(store) => Some(store),
                            Err(e) => {
                                tracing::warn!(
                                    "could not open the index of {} read-only ({e}); its \
                                     personal memories move without vectors and re-embed",
                                    worktree_dir.display()
                                );
                                None
                            }
                        };
                        let main_store = MemoryStore::open(main_dir).await?;
                        let (n, _) =
                            migrate_dir(&wt_personal, wt_store.as_ref(), &main_store).await?;
                        moved += n;
                    }
                }
            }
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

/// Move an **untracked** stray worktree store into main and remove it.
async fn consolidate_stray_store<F, Fut>(
    worktree_dir: &Path,
    main_dir: &Path,
    wt_id: &str,
    classify: &F,
) -> Result<usize>
where
    F: Fn(PathBuf) -> Fut,
    Fut: std::future::Future<Output = WorktreeStore>,
{
    let wt_engramdb = paths::project_dir(worktree_dir);
    let mut moved = 0;
    // Files `migrate_dir` could not read or parse. They are still data, and
    // they are the ONLY copy — so the directory holding them is not ours to
    // remove, however tidy that would be.
    let mut stranded_shared = 0;

    let wt_store = MemoryStore::open(worktree_dir).await?;
    let main_store = MemoryStore::open(main_dir).await?;

    // Shared memories live in the worktree's own .engramdb/; personal
    // ones are keyed by the worktree's project ID in the global data dir.
    let (n, skipped) = migrate_dir(
        &paths::memories_dir(worktree_dir),
        Some(&wt_store),
        &main_store,
    )
    .await?;
    moved += n;
    stranded_shared += skipped;
    if let Ok(wt_personal) = paths::personal_memories_dir(wt_id) {
        let (n, _) = migrate_dir(&wt_personal, Some(&wt_store), &main_store).await?;
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
    //
    // git is asked again immediately before the delete: a `git add` since
    // the first answer makes the directory a checkout, and only a directory
    // that is still untracked may go.
    if stranded_shared == 0
        && ledger_adopted
        && classify(worktree_dir.to_path_buf()).await == WorktreeStore::Untracked
    {
        async_fs::remove_dir_all(&wt_engramdb).await?;
    }
    Ok(moved)
}

/// Resolve `dir` to the project root that should own its memory operations.
///
/// When `dir` is a linked git worktree this ensures the main project's store
/// exists, consolidates any stray (git-untracked) worktree memories into it,
/// registers the worktree as a sub-project, and returns the main worktree's
/// path.
///
/// For a main worktree, a plain non-git directory, or a malformed worktree
/// pointer, returns `dir` unchanged. Idempotent and cheap on the common path
/// (a single `.git` stat), so it is safe to call on every invocation.
pub async fn resolve_project_root(dir: &Path, registry: &dyn RegistryBackend) -> Result<PathBuf> {
    resolve_with(dir, registry, |dir| async move {
        classify_worktree_store(&dir).await
    })
    .await
}

/// [`resolve_project_root`] with the git question injectable.
pub(crate) async fn resolve_with<F, Fut>(
    dir: &Path,
    registry: &dyn RegistryBackend,
    classify: F,
) -> Result<PathBuf>
where
    F: Fn(PathBuf) -> Fut,
    Fut: std::future::Future<Output = WorktreeStore>,
{
    let Some(main) = project_id::detect_worktree_main(dir) else {
        return Ok(dir.to_path_buf());
    };

    // The main project's store must exist before operations route to it.
    if !paths::project_dir(&main).exists() {
        MemoryStore::init(&main, registry).await?;
    }

    // Pull any memories written under the worktree's stray store into main.
    consolidate_with(dir, &main, classify).await?;

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
        // git's back-link from the per-worktree gitdir to the worktree's `.git`.
        fs::write(
            wt_gitdir.join("gitdir"),
            format!("{}\n", wt.join(".git").display()),
        )
        .unwrap();
        (main, wt)
    }

    /// The fake layouts below are not git repositories, so git cannot
    /// classify them; these run the consolidation as git would answer for a
    /// store it does not track.
    async fn consolidate_untracked(wt: &Path, main: &Path) -> Result<usize> {
        consolidate_with(wt, main, |_| async { WorktreeStore::Untracked }).await
    }

    async fn resolve_untracked(wt: &Path, registry: &InMemoryRegistry) -> Result<PathBuf> {
        resolve_with(wt, registry, |_| async { WorktreeStore::Untracked }).await
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

        let resolved = resolve_untracked(&wt, &registry).await.unwrap();
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

        let moved = consolidate_untracked(&wt, &main).await.unwrap();
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

        let moved = consolidate_untracked(&wt, &main).await.unwrap();
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
        assert_eq!(consolidate_untracked(&wt, &main).await.unwrap(), 1);

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
        assert_eq!(consolidate_untracked(&wt, &main).await.unwrap(), 0);
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

        let moved = consolidate_untracked(&wt, &main).await.unwrap();
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

        let migrated = consolidate_untracked(&wt, &main).await.unwrap();
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

        consolidate_untracked(&wt, &main).await.unwrap();

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

        let result = consolidate_untracked(&wt, &main).await;
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

        consolidate_untracked(&wt, &main).await.unwrap();
        assert!(
            !wt.join(".engramdb").exists(),
            "run 1 clears the stray store"
        );
        assert!(stranded.exists(), "run 1 must keep the only copy");

        consolidate_untracked(&wt, &main).await.unwrap();
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

        let moved = consolidate_untracked(&wt, &main).await.unwrap();
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

        let resolved = resolve_untracked(&wt, &registry).await.unwrap();
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

    // ---- real git ---------------------------------------------------------

    /// Run git in `dir`; panics when git is missing or the command fails
    /// (these tests must fail, not skip, without git).
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("git must be on PATH for the worktree tests");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Every file under `dir` (relative path → bytes).
    fn snapshot_dir(dir: &Path) -> std::collections::BTreeMap<PathBuf, String> {
        fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<PathBuf, String>) {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else {
                    out.insert(
                        path.strip_prefix(base).unwrap().to_path_buf(),
                        String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned(),
                    );
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(dir, dir, &mut out);
        out
    }

    /// A real repository whose main checkout commits `.engramdb/` (one
    /// memory), plus a linked worktree on a new branch `feature`.
    /// Returns `(main, worktree, memory)`.
    async fn real_repo_with_tracked_store(
        root: &Path,
        registry: &InMemoryRegistry,
    ) -> (PathBuf, PathBuf, Memory) {
        let main = root.join("main");
        let wt = root.join("wt");
        fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        let store = MemoryStore::init(&main, registry).await.unwrap();
        let mem = Memory::new(
            MemoryType::Decision,
            "Committed memory",
            "tracked by git",
            Provenance::human(),
        );
        store.create(&mem).await.unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-q", "-m", "store"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                wt.to_str().unwrap(),
            ],
        );
        assert!(wt.join(".engramdb/manifest.toml").exists());
        (main, wt, mem)
    }

    /// The P0: a worktree of a repository that commits `.engramdb/` has that
    /// directory as an ordinary checkout. It used to be consolidated and
    /// deleted on every command, leaving every tracked file "deleted" in
    /// `git status`.
    #[tokio::test]
    async fn a_tracked_worktree_store_is_never_touched() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (main, wt, _) = real_repo_with_tracked_store(tmp.path(), &registry).await;
        let main_before = snapshot_dir(&paths::memories_dir(&main));
        let wt_before = snapshot_dir(&wt.join(".engramdb"));

        for _ in 0..2 {
            let resolved = resolve_project_root(&wt, &registry).await.unwrap();
            assert_eq!(
                resolved.canonicalize().unwrap(),
                main.canonicalize().unwrap()
            );
        }

        assert_eq!(
            git(&wt, &["status", "--porcelain"]),
            "",
            "the worktree checkout must stay clean"
        );
        assert_eq!(snapshot_dir(&wt.join(".engramdb")), wt_before);
        assert_eq!(snapshot_dir(&paths::memories_dir(&main)), main_before);
        assert_eq!(git(&main, &["status", "--porcelain"]), "");
    }

    /// A memory committed only on the worktree's branch is not copied into
    /// the main worktree's working tree, and a newer edit there does not
    /// overwrite main's copy.
    #[tokio::test]
    async fn a_branch_only_memory_stays_on_its_branch() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (main, wt, mem) = real_repo_with_tracked_store(tmp.path(), &registry).await;

        // On the branch: one extra memory, and a newer edit of the shared one.
        let extra = Memory::new(
            MemoryType::Hazard,
            "Branch only",
            "exists on feature only",
            Provenance::human(),
        );
        let wt_memories = paths::memories_dir(&wt);
        fs::write(
            wt_memories.join(memory_file::memory_filename(&extra)),
            memory_file::write_memory_file(&extra).unwrap(),
        )
        .unwrap();
        let mut edited = mem.clone();
        edited.summary = "Edited on the branch".to_string();
        edited.updated_at = mem.updated_at + chrono::Duration::hours(1);
        fs::write(
            wt_memories.join(memory_file::memory_filename(&edited)),
            memory_file::write_memory_file(&edited).unwrap(),
        )
        .unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-q", "-m", "branch work"]);
        let main_before = snapshot_dir(&paths::memories_dir(&main));

        resolve_project_root(&wt, &registry).await.unwrap();

        assert_eq!(snapshot_dir(&paths::memories_dir(&main)), main_before);
        let main_store = MemoryStore::open(&main).await.unwrap();
        assert!(main_store.get(&extra.id).await.is_err());
        assert_eq!(
            main_store.get(&mem.id).await.unwrap().summary,
            "Committed memory"
        );
        assert_eq!(git(&wt, &["status", "--porcelain"]), "");
    }

    /// A store `init` created inside a real worktree whose branch does not
    /// track `.engramdb/` is still a stray store: existing behavior.
    #[tokio::test]
    async fn an_untracked_stray_store_is_still_consolidated() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let main = tmp.path().join("main");
        let wt = tmp.path().join("wt");
        fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-q"]);
        fs::write(main.join("README"), "x").unwrap();
        git(&main, &["add", "-A"]);
        git(&main, &["commit", "-q", "-m", "init"]);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feature",
                wt.to_str().unwrap(),
            ],
        );

        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "Stray", "c", Provenance::human());
        let id = wt_store.create(&mem).await.unwrap();
        assert_eq!(classify_worktree_store(&wt).await, WorktreeStore::Untracked);

        let resolved = resolve_project_root(&wt, &registry).await.unwrap();

        assert!(!wt.join(".engramdb").exists(), "the stray store is removed");
        let main_store = MemoryStore::open(&resolved).await.unwrap();
        assert_eq!(main_store.get(&id).await.unwrap().summary, "Stray");
    }

    #[tokio::test]
    async fn git_reports_a_committed_store_as_tracked() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (_main, wt, _) = real_repo_with_tracked_store(tmp.path(), &registry).await;
        assert_eq!(classify_worktree_store(&wt).await, WorktreeStore::Tracked);
        // Not a repository at all: git cannot answer.
        let plain = tmp.path().join("plain");
        fs::create_dir_all(plain.join(".engramdb")).unwrap();
        assert_eq!(
            classify_worktree_store(&plain).await,
            WorktreeStore::Unknown
        );
        assert_eq!(
            classify_worktree_store(&tmp.path().join("missing")).await,
            WorktreeStore::Absent
        );
    }

    /// When git cannot be asked the directory stays: nothing is migrated and
    /// nothing is deleted.
    #[tokio::test]
    async fn an_unknown_classification_keeps_the_store() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();
        let main_store = MemoryStore::init(&main, &registry).await.unwrap();
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "Stays", "c", Provenance::human());
        wt_store.create(&mem).await.unwrap();
        drop(wt_store);
        let before = snapshot_dir(&wt.join(".engramdb"));

        let moved = consolidate_with(&wt, &main, |_| async { WorktreeStore::Unknown })
            .await
            .unwrap();

        assert_eq!(moved, 0);
        assert_eq!(snapshot_dir(&wt.join(".engramdb")), before);
        assert!(main_store.list_summary().await.unwrap().is_empty());
    }

    /// A `git add` between the first answer and the delete makes the
    /// directory a checkout; the second answer decides the delete.
    #[tokio::test]
    async fn a_store_that_becomes_tracked_before_the_delete_is_kept() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let registry = InMemoryRegistry::new();
        MemoryStore::init(&main, &registry).await.unwrap();
        let wt_store = MemoryStore::init(&wt, &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "Racy", "c", Provenance::human());
        wt_store.create(&mem).await.unwrap();

        let asked = std::sync::atomic::AtomicUsize::new(0);
        consolidate_with(&wt, &main, |_| {
            let n = asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async move {
                if n == 0 {
                    WorktreeStore::Untracked
                } else {
                    WorktreeStore::Tracked
                }
            }
        })
        .await
        .unwrap();

        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(
            wt.join(".engramdb/manifest.toml").exists(),
            "a directory that became tracked must not be deleted"
        );
    }

    /// The git spawn runs on every hook, so a `Tracked` answer is remembered
    /// for the index state it was given for — in the process and on disk.
    #[tokio::test]
    async fn a_tracked_verdict_is_asked_once_per_index_state() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (main, wt, _) = real_repo_with_tracked_store(tmp.path(), &registry).await;
        let asked = AtomicUsize::new(0);
        let counting = |dir: PathBuf| {
            asked.fetch_add(1, Ordering::SeqCst);
            async move { classify_worktree_store(&dir).await }
        };

        resolve_with(&wt, &registry, counting).await.unwrap();
        resolve_with(&wt, &registry, counting).await.unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 1);

        // A new process has an empty map; the file answers.
        tracked_memo().lock().unwrap().remove(&wt);
        resolve_with(&wt, &registry, counting).await.unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // The memo lives in main's gitignored state dir, never in the checkout.
        assert_eq!(git(&main, &["status", "--porcelain"]), "");
        assert_eq!(git(&wt, &["status", "--porcelain"]), "");

        // Any index change invalidates it.
        fs::write(wt.join("new-file"), "x").unwrap();
        git(&wt, &["add", "new-file"]);
        resolve_with(&wt, &registry, counting).await.unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 2);
    }

    /// Personal memories keyed by the worktree's id are in the global data
    /// dir, not in the repository, so they move even when the checkout is
    /// left alone.
    #[tokio::test]
    async fn personal_memories_of_a_tracked_worktree_still_move() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (main, wt, _) = real_repo_with_tracked_store(tmp.path(), &registry).await;

        let mut personal = Memory::new(
            MemoryType::Preference,
            "Mine alone",
            "c",
            Provenance::human(),
        );
        personal.visibility = engram_types::Visibility::Personal;
        let wt_id = project_id::compute_project_id(&wt);
        let dir = paths::personal_memories_dir(&wt_id).unwrap();
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join(memory_file::memory_filename(&personal));
        fs::write(&src, memory_file::write_memory_file(&personal).unwrap()).unwrap();

        resolve_project_root(&wt, &registry).await.unwrap();

        assert!(!src.exists(), "the personal file moved");
        let main_store = MemoryStore::open(&main).await.unwrap();
        let moved = main_store.get(&personal.id).await.unwrap();
        assert_eq!(moved.visibility, engram_types::Visibility::Personal);
        assert_eq!(git(&wt, &["status", "--porcelain"]), "");
    }

    /// Resolution runs on every command and hook in a worktree; once the
    /// worktree is linked it must not rewrite the global registry again.
    #[cfg(unix)]
    #[tokio::test]
    async fn resolving_a_linked_worktree_twice_saves_the_registry_once() {
        use std::os::unix::fs::MetadataExt;
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree(tmp.path());
        let path = tmp.path().join("registry.json");
        let registry = crate::FileRegistry::new(path.clone());
        MemoryStore::init(&main, &registry).await.unwrap();

        resolve_project_root(&wt, &registry).await.unwrap();
        let first = fs::metadata(&path).unwrap().ino();
        resolve_project_root(&wt, &registry).await.unwrap();
        resolve_project_root(&wt, &registry).await.unwrap();

        assert_eq!(fs::metadata(&path).unwrap().ino(), first);
    }
}
