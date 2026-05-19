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
/// re-embedding. Returns the number of memories migrated.
///
/// Files that can't be read or parsed are skipped (a single corrupt file
/// must not abort consolidation). `create` is keyed by memory id, so a
/// re-run after a partial migration simply overwrites — idempotent.
async fn migrate_dir(
    src_dir: &Path,
    wt_store: &MemoryStore,
    main_store: &MemoryStore,
) -> Result<usize> {
    if !src_dir.exists() {
        return Ok(0);
    }

    let mut migrated = 0;
    let mut entries = async_fs::read_dir(src_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = async_fs::read_to_string(&path).await else {
            continue;
        };
        let Ok(memory) = memory_file::parse_memory_file(&content) else {
            continue;
        };

        // Write the memory (file + metadata) into the main project.
        main_store.create(&memory).await?;

        // Relocate the existing embedding vectors so the migrated memory
        // stays searchable immediately — no model load, no re-embedding.
        let chunks = wt_store.export_chunks(&memory.id).await.unwrap_or_default();
        if !chunks.is_empty() {
            main_store.upsert_chunks(&memory.id, chunks).await?;
        }

        migrated += 1;
    }
    Ok(migrated)
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

    // Only migrate when the worktree actually has a stray store AND the main
    // store exists (the caller guarantees the latter before routing to it;
    // refusing to delete the stray store otherwise avoids data loss).
    if wt_engramdb.exists() && paths::project_dir(main_dir).exists() {
        let wt_store = MemoryStore::open(worktree_dir).await?;
        let main_store = MemoryStore::open(main_dir).await?;

        // Shared memories live in the worktree's own .engramdb/; personal
        // ones are keyed by the worktree's project ID in the global data dir.
        moved += migrate_dir(&paths::memories_dir(worktree_dir), &wt_store, &main_store).await?;
        if let Ok(wt_personal) = paths::personal_memories_dir(&wt_id) {
            moved += migrate_dir(&wt_personal, &wt_store, &main_store).await?;
        }

        // Remove the stray worktree store so future ops route to main.
        async_fs::remove_dir_all(&wt_engramdb).await?;
    }

    // Drop the worktree's stale global data dir (its now-migrated personal
    // memories and obsolete LanceDB index). Best-effort: data already moved.
    if let Ok(global_data) = paths::global_data_dir() {
        let wt_global = global_data.join("projects").join(&wt_id);
        if wt_global.exists() {
            let _ = async_fs::remove_dir_all(&wt_global).await;
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
    use crate::storage::{InMemoryRegistry, RegistryBackend};
    use crate::types::{Memory, MemoryType, Provenance};
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
            .vector_search(vec![0.25f32; 384], 5)
            .await
            .unwrap();
        assert!(
            hits.iter().any(|m| m.id == mem_id),
            "migrated memory must be vector-searchable in main"
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
