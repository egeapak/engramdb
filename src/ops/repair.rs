//! Repair a project whose ID drifted out from under its registry entry.
//!
//! `project_id::compute_project_id` prefers the git remote and falls back to
//! the absolute path, so adding a remote after `engramdb init` — the ordinary
//! order when you create a repo locally and push it later — permanently
//! re-keys a project. The registry keeps the old ID; every live operation uses
//! the new one. The symptoms are all silent:
//!
//! - memories vanish from queries (the live ID's index is empty, though the
//!   `.md` files are untouched in the project tree),
//! - group subscriptions detach — they are looked up as
//!   `subscriptions_of(reg, compute_project_id(dir))`, which no longer matches
//!   the entry recording them,
//! - personal memories, which live *only* under the old ID's data directory,
//!   become invisible.
//!
//! Re-registering is not the repair: `RegistryBackend::update` pushes a *new*
//! entry with empty `subscriptions` and no `parent_project_id`, leaving two
//! rows for one path. [`repair_project_id`] migrates the entry instead.

use crate::storage::{
    memory_file, paths, project_id, stale_registrations_for, RegistryBackend, RegistryEntry,
};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use tokio::fs as async_fs;

/// What a repair did (or, from [`plan_repair`], would do).
#[derive(Debug, Clone)]
pub struct RepairReport {
    /// The project directory that was re-keyed.
    pub path: PathBuf,
    /// The ID the registry recorded.
    pub old_id: String,
    /// The ID the directory hashes to today.
    pub new_id: String,
    /// Personal memory files moved from the old data dir to the live one.
    pub personal_migrated: usize,
    /// Personal files left behind because the live dir already held a newer
    /// copy of the same memory.
    pub personal_superseded: usize,
    /// A duplicate registry row for this path was dropped (the state produced
    /// by running `engramdb init` on an already-drifted project).
    pub removed_duplicate_entry: bool,
    /// Sub-projects (worktrees) whose `parent_project_id` was re-pointed at
    /// the live ID.
    pub reparented_children: Vec<String>,
    /// Whether the old data directory was removed once everything authoritative
    /// had been moved out of it.
    pub old_data_dir_removed: bool,
}

/// Inspect `dir` and describe the repair it needs, without changing anything.
///
/// `None` means the registration is consistent — the caller should report
/// "nothing to repair" rather than treating it as an error.
pub async fn plan_repair(
    registry: &dyn RegistryBackend,
    dir: &Path,
) -> Result<Option<RepairReport>> {
    let new_id = project_id::compute_project_id(dir);
    let reg = registry.load().await?;
    let stale = stale_registrations_for(&reg, dir, &new_id);
    let Some(old_entry) = stale.first() else {
        return Ok(None);
    };
    let old_id = old_entry.project_id.clone();

    let (migrate, supersede) = survey_personal(&old_id, &new_id).await;
    let reparented = children_of(&reg, &old_id);
    let removed_duplicate_entry = reg.projects.iter().any(|e| e.project_id == new_id);

    Ok(Some(RepairReport {
        path: dir.to_path_buf(),
        old_id,
        new_id,
        personal_migrated: migrate,
        personal_superseded: supersede,
        removed_duplicate_entry,
        reparented_children: reparented,
        old_data_dir_removed: false,
    }))
}

/// Re-key `dir`'s registration to the ID it hashes to today, carrying its
/// machine-local data across.
///
/// Returns `None` when there is nothing to repair, so running it twice is
/// harmless. Reindexing is deliberately left to the caller: `ops` must not
/// depend on the engine or daemon layers.
pub async fn repair_project_id(
    registry: &dyn RegistryBackend,
    dir: &Path,
) -> Result<Option<RepairReport>> {
    let Some(mut report) = plan_repair(registry, dir).await? else {
        return Ok(None);
    };

    // A second live checkout of the same old remote still resolves to `old_id`
    // and shares that data directory. Moving its personal memories out, or
    // deleting the directory, would destroy data this project does not own.
    //
    // `conflicting_checkout_path` is the wrong tool here: it looks up the
    // *first* entry for the ID and returns `None` when that entry is us, which
    // is exactly the shape this case has (two rows for one ID, ours among
    // them). The question here is "does any OTHER live path hold this ID".
    let reg = registry.load().await?;
    if let Some(other) = other_checkout_sharing_id(&reg, &report.old_id, dir) {
        bail!(
            "project ID {} is also registered to the checkout at {}, which shares this data \
             directory — refusing to migrate it. Remove or re-register that checkout first.",
            report.old_id,
            other.display()
        );
    }
    drop(reg);

    // Locks are per-ID and this touches two. `flock` here is an unbounded
    // block with no deadlock detection, so acquire in a deterministic order:
    // two processes repairing in opposite directions would otherwise deadlock.
    // Both guards are dropped before any reindex — `MemoryStore::reindex`
    // re-acquires, and a second acquire in one process blocks forever.
    let (first, second) = if report.old_id <= report.new_id {
        (&report.old_id, &report.new_id)
    } else {
        (&report.new_id, &report.old_id)
    };
    let _lock_a = crate::storage::write_lock::acquire_write_lock(first).await?;
    let _lock_b = crate::storage::write_lock::acquire_write_lock(second).await?;

    // Personal memories first: they are the only per-ID data with no other
    // copy. Everything else under the old ID is derived (the metadata index)
    // or rebuildable (vectors, via the caller's reindex).
    let (migrated, superseded) = migrate_personal(&report.old_id, &report.new_id).await?;
    report.personal_migrated = migrated;
    report.personal_superseded = superseded;

    // One critical section for the whole registry rewrite, calling only
    // load/save while held (the built-in mutators re-acquire this lock).
    {
        let _reg_lock = registry.lock_exclusive().await?;
        let mut reg = registry.load().await?;

        let live_exists = reg.projects.iter().any(|e| e.project_id == report.new_id);
        if live_exists {
            // The two-entry state: fold the stale row's membership into the
            // live one rather than dropping it, then remove the stale row.
            let stale_entry = reg
                .projects
                .iter()
                .find(|e| e.project_id == report.old_id)
                .cloned();
            if let Some(stale) = stale_entry {
                if let Some(live) = reg
                    .projects
                    .iter_mut()
                    .find(|e| e.project_id == report.new_id)
                {
                    for group in stale.subscriptions {
                        if !live.subscriptions.contains(&group) {
                            live.subscriptions.push(group);
                        }
                    }
                    if live.parent_project_id.is_none() {
                        live.parent_project_id = stale.parent_project_id;
                    }
                }
            }
            reg.projects.retain(|e| e.project_id != report.old_id);
            report.removed_duplicate_entry = true;
        } else if let Some(entry) = reg
            .projects
            .iter_mut()
            .find(|e| e.project_id == report.old_id)
        {
            // Mutating in place is what preserves `subscriptions` and
            // `parent_project_id`; pushing a fresh entry would silently drop
            // both, and losing subscriptions does not error — queries just
            // stop fanning in group memories.
            entry.project_id = report.new_id.clone();
        }

        // Children still pointing at the old ID would become dangling, and
        // prune's hierarchy pass silently clears a dangling parent — turning
        // every linked worktree into its own independent project. Re-point
        // them inside the same critical section.
        report.reparented_children.clear();
        for entry in reg.projects.iter_mut() {
            if entry.parent_project_id.as_deref() == Some(report.old_id.as_str()) {
                entry.parent_project_id = Some(report.new_id.clone());
                report.reparented_children.push(entry.project_id.clone());
            }
        }

        registry.save(&reg).await?;
    }

    // Only now is the old directory expendable: its authoritative content has
    // been moved and no registry entry names it. Left in place on failure —
    // prune will collect it later, and leaving a stale directory is always
    // preferable to deleting one whose contents did not make it across.
    report.old_data_dir_removed = remove_old_data_dir(&report.old_id).await;

    Ok(Some(report))
}

/// A still-existing checkout other than `dir` registered under `project_id`.
///
/// Such a checkout shares `projects/<project_id>/` — its LanceDB index, write
/// lock, and personal memories — so that directory is not ours to migrate or
/// delete.
fn other_checkout_sharing_id(
    reg: &crate::storage::Registry,
    project_id: &str,
    dir: &Path,
) -> Option<PathBuf> {
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    reg.projects
        .iter()
        .filter(|e| e.project_id == project_id)
        .map(|e| {
            let p = PathBuf::from(&e.project_path);
            p.canonicalize().unwrap_or(p)
        })
        .find(|p| p != &canon && p.exists())
}

/// Registry IDs whose `parent_project_id` is `parent`.
fn children_of(reg: &crate::storage::Registry, parent: &str) -> Vec<String> {
    reg.projects
        .iter()
        .filter(|e| e.parent_project_id.as_deref() == Some(parent))
        .map(|e| e.project_id.clone())
        .collect()
}

/// Count what [`migrate_personal`] would move, without moving it.
async fn survey_personal(old_id: &str, new_id: &str) -> (usize, usize) {
    let (Ok(from), Ok(to)) = (
        paths::personal_memories_dir(old_id),
        paths::personal_memories_dir(new_id),
    ) else {
        return (0, 0);
    };
    let mut migrate = 0;
    let mut supersede = 0;
    for (_, path) in memory_files(&from).await {
        match should_migrate(&path, &to).await {
            Some(true) => migrate += 1,
            Some(false) => supersede += 1,
            None => {}
        }
    }
    (migrate, supersede)
}

/// Move personal memory files from the old ID's data dir into the live one.
///
/// Mirrors `worktree::consolidate_worktree_into_main`'s discipline — that
/// function is the existing precedent for merging one memories directory into
/// another, but it cannot be reused here: it derives both IDs from two
/// *directories* and returns early when they match, and its per-file worker is
/// private. Same rules, though: an unparseable file is skipped rather than
/// aborting the migration, a same-ID collision is resolved newest-wins, and
/// each source file is deleted only after its copy lands, so an interrupted
/// run resumes instead of restarting.
async fn migrate_personal(old_id: &str, new_id: &str) -> Result<(usize, usize)> {
    let from = paths::personal_memories_dir(old_id)?;
    let to = paths::personal_memories_dir(new_id)?;
    if !from.exists() {
        return Ok((0, 0));
    }
    async_fs::create_dir_all(&to).await?;

    let mut migrated = 0;
    let mut superseded = 0;
    for (file_name, path) in memory_files(&from).await {
        match should_migrate(&path, &to).await {
            Some(true) => {
                let dest = to.join(&file_name);
                async_fs::copy(&path, &dest).await?;
                async_fs::remove_file(&path).await?;
                migrated += 1;
            }
            Some(false) => {
                // The live dir already holds a newer copy of this memory.
                async_fs::remove_file(&path).await?;
                superseded += 1;
            }
            None => {}
        }
    }
    Ok((migrated, superseded))
}

/// `(file_name, path)` for every `.md` file directly under `dir`.
async fn memory_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(mut entries) = async_fs::read_dir(dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        out.push((entry.file_name().to_string_lossy().to_string(), path));
    }
    out
}

/// `Some(true)` migrate, `Some(false)` drop as superseded, `None` skip.
///
/// Collisions are keyed on the memory ID rather than the filename: `create`
/// derives the filename from the title slug, so the same memory can be stored
/// under different names on either side.
async fn should_migrate(source: &Path, target_dir: &Path) -> Option<bool> {
    let content = async_fs::read_to_string(source).await.ok()?;
    // An unparseable file is skipped, not migrated and not deleted: a single
    // corrupt file must not abort the repair or destroy its own contents.
    let memory = memory_file::parse_memory_file(&content).ok()?;

    for (name, path) in memory_files(target_dir).await {
        let stem = Path::new(&name).file_stem()?.to_string_lossy().to_string();
        if memory_file::extract_id_from_stem(&stem) != memory.id {
            continue;
        }
        let Ok(existing_content) = async_fs::read_to_string(&path).await else {
            return Some(true);
        };
        let Ok(existing) = memory_file::parse_memory_file(&existing_content) else {
            return Some(true);
        };
        // Strictly newer wins; equal timestamps re-migrate, matching the
        // worktree precedent (a crash between writing a file and relocating
        // its vectors leaves a copy that still needs carrying).
        return Some(existing.updated_at <= memory.updated_at);
    }
    Some(true)
}

/// Remove `projects/<old_id>/`, reporting whether it went.
async fn remove_old_data_dir(old_id: &str) -> bool {
    let Ok(root) = paths::global_data_dir() else {
        return false;
    };
    let dir = root.join("projects").join(old_id);
    if !dir.exists() {
        return false;
    }
    async_fs::remove_dir_all(&dir).await.is_ok()
}

/// Registry entries recorded against `dir` under a non-live ID, for callers
/// that want the raw rows (the CLI prints them in its blast radius).
pub fn stale_entries<'a>(
    reg: &'a crate::storage::Registry,
    dir: &Path,
    live_id: &str,
) -> Vec<&'a RegistryEntry> {
    stale_registrations_for(reg, dir, live_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore, Registry};
    use crate::types::{Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    /// Init a project, then re-key its registry entry to a stale ID — exactly
    /// what adding a git remote after `init` produces.
    async fn drifted(registry: &InMemoryRegistry, dir: &Path, stale_id: &str) -> String {
        let store = MemoryStore::init(dir, registry).await.unwrap();
        let live_id = store.project_id.clone();
        let mut reg = registry.load().await.unwrap();
        let entry = reg
            .projects
            .iter_mut()
            .find(|e| e.project_id == live_id)
            .unwrap();
        entry.project_id = stale_id.to_string();
        registry.save(&reg).await.unwrap();
        live_id
    }

    async fn write_personal(project_id: &str, summary: &str) -> (String, PathBuf) {
        let dir = paths::personal_memories_dir(project_id).unwrap();
        async_fs::create_dir_all(&dir).await.unwrap();
        let mut memory = Memory::new(
            MemoryType::Decision,
            summary,
            "content",
            Provenance::human(),
        );
        memory.visibility = Visibility::Personal;
        let path = dir.join(memory_file::memory_filename(&memory));
        async_fs::write(&path, memory_file::write_memory_file(&memory).unwrap())
            .await
            .unwrap();
        (memory.id.clone(), path)
    }

    #[tokio::test]
    async fn no_drift_is_a_no_op() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(tmp.path(), &registry).await.unwrap();

        assert!(repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn rekeys_the_entry_preserving_subscriptions_and_parent() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let live_id = drifted(&registry, tmp.path(), "stale00000000000").await;

        // Membership and a parent link recorded against the stale entry.
        registry
            .subscribe("stale00000000000", "__g_shared")
            .await
            .unwrap();
        registry
            .set_parent("stale00000000000", Some("parent0000000000"))
            .await
            .unwrap();

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .expect("drift must be detected");
        assert_eq!(report.old_id, "stale00000000000");
        assert_eq!(report.new_id, live_id);

        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1, "no duplicate row left behind");
        let entry = &reg.projects[0];
        assert_eq!(entry.project_id, live_id);
        assert_eq!(
            entry.subscriptions,
            vec!["__g_shared"],
            "losing subscriptions does not error — queries just stop fanning in"
        );
        assert_eq!(entry.parent_project_id.as_deref(), Some("parent0000000000"));
    }

    #[tokio::test]
    async fn collapses_the_duplicate_entry_left_by_re_running_init() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let live_id = drifted(&registry, tmp.path(), "stale00000000000").await;
        registry
            .subscribe("stale00000000000", "__g_from_stale")
            .await
            .unwrap();
        // Following doctor's old advice: `init` pushes a SECOND row for the
        // same path, with empty subscriptions.
        registry.update(tmp.path(), &live_id).await.unwrap();
        assert_eq!(registry.load().await.unwrap().projects.len(), 2);

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert!(report.removed_duplicate_entry);

        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert_eq!(reg.projects[0].project_id, live_id);
        assert_eq!(
            reg.projects[0].subscriptions,
            vec!["__g_from_stale"],
            "the stale row's membership must be folded in, not dropped"
        );
    }

    #[tokio::test]
    async fn reparents_children_that_pointed_at_the_old_id() {
        let tmp = TempDir::new().unwrap();
        let child_tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let live_id = drifted(&registry, tmp.path(), "stale00000000000").await;
        registry
            .update_with_parent(
                child_tmp.path(),
                "child00000000000",
                Some("stale00000000000"),
            )
            .await
            .unwrap();

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.reparented_children, vec!["child00000000000"]);

        let reg = registry.load().await.unwrap();
        let child = reg
            .projects
            .iter()
            .find(|e| e.project_id == "child00000000000")
            .unwrap();
        assert_eq!(
            child.parent_project_id.as_deref(),
            Some(live_id.as_str()),
            "a child left dangling would be silently unlinked by prune"
        );
    }

    #[tokio::test]
    async fn migrates_personal_memories_to_the_live_data_dir() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let live_id = drifted(&registry, tmp.path(), "stale00000000000").await;
        let (id, old_path) = write_personal("stale00000000000", "Only copy").await;

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.personal_migrated, 1);

        assert!(!old_path.exists(), "source removed after a successful move");
        let live_dir = paths::personal_memories_dir(&live_id).unwrap();
        let moved = super::memory_files(&live_dir).await;
        assert_eq!(moved.len(), 1);
        let content = async_fs::read_to_string(&moved[0].1).await.unwrap();
        assert!(content.contains(&id));
    }

    #[tokio::test]
    async fn a_newer_copy_in_the_live_dir_supersedes_the_old_one() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let live_id = drifted(&registry, tmp.path(), "stale00000000000").await;

        // Same memory ID on both sides, the live one newer.
        let dir_old = paths::personal_memories_dir("stale00000000000").unwrap();
        let dir_new = paths::personal_memories_dir(&live_id).unwrap();
        async_fs::create_dir_all(&dir_old).await.unwrap();
        async_fs::create_dir_all(&dir_new).await.unwrap();
        let mut old = Memory::new(MemoryType::Decision, "Old", "old", Provenance::human());
        old.visibility = Visibility::Personal;
        let mut new = old.clone();
        new.summary = "New".to_string();
        new.updated_at = old.updated_at + chrono::Duration::seconds(60);
        async_fs::write(
            dir_old.join(memory_file::memory_filename(&old)),
            memory_file::write_memory_file(&old).unwrap(),
        )
        .await
        .unwrap();
        async_fs::write(
            dir_new.join(memory_file::memory_filename(&new)),
            memory_file::write_memory_file(&new).unwrap(),
        )
        .await
        .unwrap();

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.personal_migrated, 0);
        assert_eq!(report.personal_superseded, 1);

        let surviving = super::memory_files(&dir_new).await;
        assert_eq!(surviving.len(), 1);
        let content = async_fs::read_to_string(&surviving[0].1).await.unwrap();
        assert!(content.contains("New"), "the newer copy must win");
    }

    #[tokio::test]
    async fn refuses_when_another_checkout_shares_the_old_data_dir() {
        let tmp = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        drifted(&registry, tmp.path(), "stale00000000000").await;
        // A second, still-existing checkout registered under the SAME old ID:
        // the data dir is shared, so migrating it would destroy its data.
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(RegistryEntry {
            project_id: "stale00000000000".to_string(),
            project_path: other.path().to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        let err = repair_project_id(&registry, tmp.path())
            .await
            .expect_err("must refuse to migrate a shared data directory");
        assert!(format!("{err}").contains("shares this data"));
    }

    #[tokio::test]
    async fn is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        drifted(&registry, tmp.path(), "stale00000000000").await;

        assert!(repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .is_some());
        assert!(
            repair_project_id(&registry, tmp.path())
                .await
                .unwrap()
                .is_none(),
            "a second run must be a no-op, not a second migration"
        );
    }

    #[tokio::test]
    async fn plan_repair_describes_without_mutating() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        drifted(&registry, tmp.path(), "stale00000000000").await;
        write_personal("stale00000000000", "Pending").await;

        let plan = plan_repair(&registry, tmp.path()).await.unwrap().unwrap();
        assert_eq!(plan.old_id, "stale00000000000");
        assert_eq!(plan.personal_migrated, 1);
        assert!(!plan.old_data_dir_removed);

        // Untouched.
        let reg: Registry = registry.load().await.unwrap();
        assert_eq!(reg.projects[0].project_id, "stale00000000000");
    }
}
