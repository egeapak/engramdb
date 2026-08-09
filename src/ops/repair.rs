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
    /// Personal memory files copied from the old data dir to the live one.
    pub personal_migrated: usize,
    /// Personal files not copied because the live dir already held a newer
    /// copy of the same memory.
    pub personal_superseded: usize,
    /// Files under the old personal dir that could not be read or parsed and
    /// were therefore left untouched. Non-zero means the old directory still
    /// holds data nothing else has.
    pub personal_skipped: usize,
    /// A duplicate registry row for this path was dropped (the state produced
    /// by running `engramdb init` on an already-drifted project).
    pub removed_duplicate_entry: bool,
    /// Sub-projects (worktrees) whose `parent_project_id` was re-pointed at
    /// the live ID.
    pub reparented_children: Vec<String>,
    /// The old data directory, always left in place. Repair never deletes it:
    /// an unregistered sibling clone of the same remote shares that directory
    /// and is structurally invisible to the registry (`update_inner_impl`
    /// keeps one row per ID and declines to add a second), so no check here
    /// can prove the directory is ours alone. Reclaiming it is `prune`'s job,
    /// which has the `protected_project_ids` guard this does not.
    pub old_data_dir: PathBuf,
}

/// Inspect `dir` and describe the repair it needs, without changing anything.
///
/// `None` means the registration is consistent — the caller should report
/// "nothing to repair" rather than treating it as an error. Any condition that
/// makes the repair unsafe is raised here too, so the caller never shows a
/// blast radius for work the next step would refuse.
pub async fn plan_repair(
    registry: &dyn RegistryBackend,
    dir: &Path,
) -> Result<Option<RepairReport>> {
    let reg = registry.load().await?;
    plan_from(&reg, dir).await
}

/// [`plan_repair`] against an already-loaded snapshot, so the execute path can
/// re-derive the plan inside its critical section from the same rules.
async fn plan_from(reg: &crate::storage::Registry, dir: &Path) -> Result<Option<RepairReport>> {
    // A linked worktree is not an independent project: its operations route to
    // the main checkout and it is registered as a sub-project. Re-keying one
    // would point its row at the worktree's own path hash and detach it.
    if let Some(main) = project_id::detect_worktree_main(dir) {
        bail!(
            "{} is a linked git worktree of {} — worktrees route to the main checkout and are \
             not re-keyed. Run this in the main checkout instead.",
            dir.display(),
            main.display()
        );
    }

    let new_id = project_id::compute_project_id(dir);
    let stale = stale_registrations_for(reg, dir, &new_id);
    if stale.is_empty() {
        return Ok(None);
    }
    let old_id = stale[0].project_id.clone();

    // Another registered checkout answering to either ID means the data dirs
    // are shared and this repair would move or graft data it does not own.
    // Checked against BOTH ids: the new-ID case is the one where a sibling's
    // row would absorb our subscriptions and our own row would be dropped.
    for id in [&old_id, &new_id] {
        if let Some(other) = other_checkout_sharing_id(reg, id, dir) {
            bail!(
                "project ID {} is also registered to the checkout at {}, which shares that data \
                 directory — refusing to re-key. Remove or re-register that checkout first.",
                id,
                other.display()
            );
        }
    }

    let (migrate, supersede, skipped) = survey_personal(&old_id, &new_id).await;
    Ok(Some(RepairReport {
        path: dir.to_path_buf(),
        personal_migrated: migrate,
        personal_superseded: supersede,
        personal_skipped: skipped,
        removed_duplicate_entry: reg
            .projects
            .iter()
            .any(|e| e.project_id == new_id && same_path(e, dir)),
        reparented_children: children_of(reg, &old_id),
        old_data_dir: data_dir_for(&old_id),
        old_id,
        new_id,
    }))
}

/// Re-key `dir`'s registration to the ID it hashes to today, carrying its
/// machine-local data across.
///
/// Returns `None` when there is nothing to repair, so running it twice is
/// harmless. Reindexing is deliberately left to the caller: `ops` must not
/// depend on the engine or daemon layers.
///
/// **Never deletes anything.** Personal memories are *copied* to the live data
/// directory and the old one is left in place — see [`RepairReport::old_data_dir`].
pub async fn repair_project_id(
    registry: &dyn RegistryBackend,
    dir: &Path,
) -> Result<Option<RepairReport>> {
    let Some(plan) = plan_repair(registry, dir).await? else {
        return Ok(None);
    };

    // Locks are per-ID and this touches two. `flock` here is an unbounded
    // block with no deadlock detection, so acquire in a deterministic order:
    // two processes repairing in opposite directions would otherwise deadlock.
    // Both guards are dropped before the caller reindexes —
    // `MemoryStore::reindex` re-acquires, and a second acquire in one process
    // blocks forever.
    let (first, second) = if plan.old_id <= plan.new_id {
        (&plan.old_id, &plan.new_id)
    } else {
        (&plan.new_id, &plan.old_id)
    };
    let _lock_a = crate::storage::write_lock::acquire_write_lock(first).await?;
    let _lock_b = crate::storage::write_lock::acquire_write_lock(second).await?;

    // Copy before the registry rewrite: if this fails, the registry is
    // untouched and the whole operation is a no-op the user can retry.
    let (migrated, superseded, skipped) = copy_personal(&plan.old_id, &plan.new_id).await?;

    // ONE critical section for read, decide and write. Splitting the decision
    // across separate `load()`s let a concurrent prune or repair land in
    // between, producing a report that described work nobody did.
    let _reg_lock = registry.lock_exclusive().await?;
    let mut reg = registry.load().await?;

    // Re-derive under the lock rather than trusting the pre-lock plan.
    let Some(mut report) = plan_from(&reg, dir).await? else {
        // Someone else repaired it while we waited for the lock.
        return Ok(None);
    };
    report.personal_migrated = migrated;
    report.personal_superseded = superseded;
    report.personal_skipped = skipped;

    // Every lookup is scoped to THIS path. Matching by ID alone let a
    // different checkout's row absorb our subscriptions while our own row was
    // deleted — two clones of one remote is enough to trigger it.
    let live_row_here = reg
        .projects
        .iter()
        .any(|e| e.project_id == report.new_id && same_path(e, dir));

    // Every stale row for this path, not just the first: drifting twice leaves
    // two, and repairing one would keep the other flagged forever.
    let stale_here: Vec<RegistryEntry> = reg
        .projects
        .iter()
        .filter(|e| e.project_id != report.new_id && same_path(e, dir))
        .cloned()
        .collect();

    if live_row_here {
        // Fold every stale row's membership into the live one, then drop them.
        let (subs, parent) = merge_membership(&stale_here);
        if let Some(live) = reg
            .projects
            .iter_mut()
            .find(|e| e.project_id == report.new_id && same_path(e, dir))
        {
            for group in subs {
                if !live.subscriptions.contains(&group) {
                    live.subscriptions.push(group);
                }
            }
            if live.parent_project_id.is_none() {
                live.parent_project_id = parent;
            }
        }
        let stale_ids: Vec<&str> = stale_here.iter().map(|e| e.project_id.as_str()).collect();
        reg.projects
            .retain(|e| !(stale_ids.contains(&e.project_id.as_str()) && same_path(e, dir)));
        report.removed_duplicate_entry = true;
    } else {
        // Re-key the first stale row in place — this is what preserves
        // `subscriptions` and `parent_project_id`; pushing a fresh entry would
        // silently drop both, and losing subscriptions does not error, queries
        // just stop fanning in group memories. Any further stale rows for this
        // path fold into it and are dropped.
        let (subs, parent) = merge_membership(&stale_here);
        let keep_id = stale_here[0].project_id.clone();
        if let Some(entry) = reg
            .projects
            .iter_mut()
            .find(|e| e.project_id == keep_id && same_path(e, dir))
        {
            entry.project_id = report.new_id.clone();
            for group in subs {
                if !entry.subscriptions.contains(&group) {
                    entry.subscriptions.push(group);
                }
            }
            if entry.parent_project_id.is_none() {
                entry.parent_project_id = parent;
            }
        }
        let extra: Vec<&str> = stale_here
            .iter()
            .skip(1)
            .map(|e| e.project_id.as_str())
            .collect();
        if !extra.is_empty() {
            reg.projects
                .retain(|e| !(extra.contains(&e.project_id.as_str()) && same_path(e, dir)));
            report.removed_duplicate_entry = true;
        }
    }

    // Children still naming a stale ID would become dangling, and prune's
    // hierarchy pass silently clears a dangling parent — turning every linked
    // worktree into its own independent project.
    report.reparented_children.clear();
    let stale_ids: Vec<String> = stale_here.iter().map(|e| e.project_id.clone()).collect();
    for entry in reg.projects.iter_mut() {
        if entry
            .parent_project_id
            .as_deref()
            .is_some_and(|p| stale_ids.iter().any(|s| s == p))
        {
            entry.parent_project_id = Some(report.new_id.clone());
            report.reparented_children.push(entry.project_id.clone());
        }
    }

    registry.save(&reg).await?;
    Ok(Some(report))
}

/// Whether a registry row points at `dir` (canonicalized both sides).
fn same_path(entry: &RegistryEntry, dir: &Path) -> bool {
    let canon = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let p = PathBuf::from(&entry.project_path);
    p.canonicalize().unwrap_or(p) == canon
}

/// Union the subscriptions of `rows` and take the first parent link found.
fn merge_membership(rows: &[RegistryEntry]) -> (Vec<String>, Option<String>) {
    let mut subs: Vec<String> = Vec::new();
    let mut parent = None;
    for row in rows {
        for group in &row.subscriptions {
            if !subs.contains(group) {
                subs.push(group.clone());
            }
        }
        if parent.is_none() {
            parent.clone_from(&row.parent_project_id);
        }
    }
    (subs, parent)
}

/// `<data>/projects/<id>`, or a bare relative path if the data dir can't be
/// resolved (only used for reporting).
fn data_dir_for(id: &str) -> PathBuf {
    paths::global_data_dir()
        .map(|d| d.join("projects").join(id))
        .unwrap_or_else(|_| PathBuf::from("projects").join(id))
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

/// Count what [`copy_personal`] would do, without doing it.
async fn survey_personal(old_id: &str, new_id: &str) -> (usize, usize, usize) {
    let (Ok(from), Ok(to)) = (
        paths::personal_memories_dir(old_id),
        paths::personal_memories_dir(new_id),
    ) else {
        return (0, 0, 0);
    };
    let (mut copy, mut supersede, mut skip) = (0, 0, 0);
    for (_, path) in memory_files(&from).await {
        match plan_file(&path, &to).await {
            Some(FileAction::Copy { .. }) => copy += 1,
            Some(FileAction::Superseded) => supersede += 1,
            None => skip += 1,
        }
    }
    (copy, supersede, skip)
}

/// What to do with one source file.
enum FileAction {
    /// Copy it in, first removing `replaces` (an older copy of the same memory
    /// stored under a different title slug).
    Copy { replaces: Option<PathBuf> },
    /// The live dir already holds a strictly newer copy.
    Superseded,
}

/// Copy personal memories from the old ID's data dir into the live one.
///
/// **Copies; never deletes.** The old directory is shared with any
/// unregistered sibling clone of the same remote, and those are structurally
/// invisible to the registry, so moving files out could take data belonging to
/// a checkout this repair has no claim over. Leaving the originals also means
/// an unreadable or unparseable file is simply not copied rather than silently
/// stranded and then destroyed.
///
/// Same collision rule as `worktree::migrate_dir`, the existing precedent for
/// merging one memories dir into another: keyed on the memory ID (filenames
/// derive from the title slug, so one memory can be stored under two names),
/// strictly-newer target wins, ties re-copy.
async fn copy_personal(old_id: &str, new_id: &str) -> Result<(usize, usize, usize)> {
    let from = paths::personal_memories_dir(old_id)?;
    let to = paths::personal_memories_dir(new_id)?;
    if !from.exists() {
        return Ok((0, 0, 0));
    }
    async_fs::create_dir_all(&to).await?;

    let (mut copied, mut superseded, mut skipped) = (0, 0, 0);
    for (file_name, path) in memory_files(&from).await {
        match plan_file(&path, &to).await {
            Some(FileAction::Copy { replaces }) => {
                // Remove the older copy first, or the same memory ends up in
                // two files and which one wins is `read_dir` order.
                if let Some(old) = replaces {
                    async_fs::remove_file(&old).await?;
                }
                async_fs::copy(&path, to.join(&file_name)).await?;
                copied += 1;
            }
            Some(FileAction::Superseded) => superseded += 1,
            None => skipped += 1,
        }
    }
    Ok((copied, superseded, skipped))
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

/// Decide what to do with `source`, or `None` to leave it alone.
///
/// `None` covers unreadable and unparseable files. Because nothing is deleted,
/// "leave it alone" is genuinely safe here — it is reported via
/// `personal_skipped` so the user knows the old directory still holds data.
async fn plan_file(source: &Path, target_dir: &Path) -> Option<FileAction> {
    let content = async_fs::read_to_string(source).await.ok()?;
    let memory = memory_file::parse_memory_file(&content).ok()?;

    for (name, path) in memory_files(target_dir).await {
        let stem = Path::new(&name).file_stem()?.to_string_lossy().to_string();
        if memory_file::extract_id_from_stem(&stem) != memory.id {
            continue;
        }
        let existing = async_fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|c| memory_file::parse_memory_file(&c).ok());
        return match existing {
            // Strictly newer target wins; ties re-copy, matching the worktree
            // precedent (a crash between writing a file and relocating its
            // vectors leaves a copy that still needs carrying).
            Some(e) if e.updated_at > memory.updated_at => Some(FileAction::Superseded),
            _ => Some(FileAction::Copy {
                replaces: Some(path),
            }),
        };
    }
    Some(FileAction::Copy { replaces: None })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore, Registry};
    use crate::types::{Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    /// Init at `dir`, then re-key its row to a stale ID — what adding a git
    /// remote after `init` produces. Returns `(live_id, stale_id)`.
    ///
    /// The stale ID is derived from the live one rather than hard-coded: these
    /// tests write real files under `personal_memories_dir(stale_id)`, and a
    /// shared constant would make them race under `cargo test --lib`, which
    /// (unlike nextest) gives every test in the process one data dir.
    async fn drifted(registry: &InMemoryRegistry, dir: &Path) -> (String, String) {
        let store = MemoryStore::init(dir, registry).await.unwrap();
        let live = store.project_id.clone();
        let stale = format!("stale{}", &live[..11]);
        let mut reg = registry.load().await.unwrap();
        reg.projects
            .iter_mut()
            .find(|e| e.project_id == live)
            .unwrap()
            .project_id = stale.clone();
        registry.save(&reg).await.unwrap();
        (live, stale)
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

    fn row(reg: &Registry, id: &str) -> Option<RegistryEntry> {
        reg.projects.iter().find(|e| e.project_id == id).cloned()
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
    async fn rekeys_in_place_preserving_subscriptions_and_parent() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, stale) = drifted(&registry, tmp.path()).await;
        registry.subscribe(&stale, "__g_shared").await.unwrap();
        registry
            .set_parent(&stale, Some("parent0000000000"))
            .await
            .unwrap();

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .expect("drift must be detected");
        assert_eq!(report.old_id, stale);
        assert_eq!(report.new_id, live);

        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1, "no duplicate row left behind");
        let entry = &reg.projects[0];
        assert_eq!(entry.project_id, live);
        assert_eq!(
            entry.subscriptions,
            vec!["__g_shared"],
            "losing subscriptions does not error — queries just stop fanning in"
        );
        assert_eq!(entry.parent_project_id.as_deref(), Some("parent0000000000"));
        assert_eq!(
            PathBuf::from(&entry.project_path),
            tmp.path().canonicalize().unwrap(),
            "the row must still point at this project"
        );
    }

    #[tokio::test]
    async fn collapses_the_duplicate_entry_left_by_re_running_init() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, stale) = drifted(&registry, tmp.path()).await;
        registry.subscribe(&stale, "__g_from_stale").await.unwrap();
        registry
            .set_parent(&stale, Some("parent0000000000"))
            .await
            .unwrap();
        // Following doctor's old advice: `init` pushes a SECOND row for the
        // same path, with empty subscriptions and no parent.
        registry.update(tmp.path(), &live).await.unwrap();
        assert_eq!(registry.load().await.unwrap().projects.len(), 2);

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert!(report.removed_duplicate_entry);

        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1);
        let entry = row(&reg, &live).unwrap();
        assert_eq!(
            entry.subscriptions,
            vec!["__g_from_stale"],
            "the stale row's membership must be folded in, not dropped"
        );
        assert_eq!(
            entry.parent_project_id.as_deref(),
            Some("parent0000000000"),
            "the parent fold is what keeps a worktree routed after a re-key"
        );
    }

    /// Drift twice (init → add remote → change the remote) leaves two stale
    /// rows. Repairing only the first would keep the project flagged forever
    /// and break the documented idempotence.
    #[tokio::test]
    async fn collapses_every_stale_row_for_the_path() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, stale) = drifted(&registry, tmp.path()).await;
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(RegistryEntry {
            project_id: format!("older{}", &live[..11]),
            project_path: tmp.path().to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec!["__g_older".to_string()],
        });
        registry.save(&reg).await.unwrap();

        repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();

        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1, "both stale rows must be collapsed");
        let entry = row(&reg, &live).unwrap();
        assert!(entry.subscriptions.contains(&"__g_older".to_string()));
        assert!(
            repair_project_id(&registry, tmp.path())
                .await
                .unwrap()
                .is_none(),
            "and the project must now be clean"
        );
        let _ = stale;
    }

    #[tokio::test]
    async fn reparents_children_that_pointed_at_the_old_id() {
        let tmp = TempDir::new().unwrap();
        let child_tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, stale) = drifted(&registry, tmp.path()).await;
        registry
            .update_with_parent(child_tmp.path(), "child00000000000", Some(&stale))
            .await
            .unwrap();

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.reparented_children, vec!["child00000000000"]);

        let reg = registry.load().await.unwrap();
        let child = row(&reg, "child00000000000").unwrap();
        assert_eq!(
            child.parent_project_id.as_deref(),
            Some(live.as_str()),
            "a child left dangling would be silently unlinked by prune"
        );
    }

    /// Repair must never delete: the old data dir is shared with any
    /// unregistered sibling clone of the same remote, which the registry
    /// cannot see. Copying leaves every party's data intact.
    #[tokio::test]
    async fn copies_personal_memories_and_leaves_the_old_dir_intact() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, stale) = drifted(&registry, tmp.path()).await;
        let (id, source) = write_personal(&stale, "Only copy").await;

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.personal_migrated, 1);

        let live_dir = paths::personal_memories_dir(&live).unwrap();
        let copied = super::memory_files(&live_dir).await;
        assert_eq!(copied.len(), 1);
        assert!(async_fs::read_to_string(&copied[0].1)
            .await
            .unwrap()
            .contains(&id));

        assert!(source.exists(), "the source must be copied, not moved");
        assert!(
            report.old_data_dir.exists(),
            "the old data dir must survive — an unregistered sibling clone may share it"
        );
    }

    /// An unreadable or unparseable file must be left alone AND counted, so
    /// the user knows the old directory still holds data nothing else has.
    #[tokio::test]
    async fn an_unparseable_personal_file_is_skipped_and_reported() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (_, stale) = drifted(&registry, tmp.path()).await;
        let dir = paths::personal_memories_dir(&stale).unwrap();
        async_fs::create_dir_all(&dir).await.unwrap();
        let corrupt = dir.join("corrupt.md");
        async_fs::write(&corrupt, "not frontmatter at all")
            .await
            .unwrap();

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.personal_skipped, 1);
        assert_eq!(report.personal_migrated, 0);
        assert!(corrupt.exists(), "a corrupt file must never be destroyed");
    }

    #[tokio::test]
    async fn a_newer_copy_in_the_live_dir_supersedes_the_old_one() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, stale) = drifted(&registry, tmp.path()).await;

        let dir_old = paths::personal_memories_dir(&stale).unwrap();
        let dir_new = paths::personal_memories_dir(&live).unwrap();
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
        assert!(async_fs::read_to_string(&surviving[0].1)
            .await
            .unwrap()
            .contains("New"));
    }

    /// A copy stored under a different title slug must REPLACE the older file,
    /// not sit beside it — two files with one memory ID make the winner
    /// depend on `read_dir` order.
    #[tokio::test]
    async fn an_older_copy_under_a_different_filename_is_replaced() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, stale) = drifted(&registry, tmp.path()).await;

        let dir_old = paths::personal_memories_dir(&stale).unwrap();
        let dir_new = paths::personal_memories_dir(&live).unwrap();
        async_fs::create_dir_all(&dir_old).await.unwrap();
        async_fs::create_dir_all(&dir_new).await.unwrap();
        let mut newer = Memory::new(MemoryType::Decision, "Newer", "new", Provenance::human());
        newer.visibility = Visibility::Personal;
        newer.title = Some("Newer Title".to_string());
        let mut older = newer.clone();
        older.title = Some("Older Title".to_string());
        older.updated_at = newer.updated_at - chrono::Duration::seconds(60);

        async_fs::write(
            dir_old.join(memory_file::memory_filename(&newer)),
            memory_file::write_memory_file(&newer).unwrap(),
        )
        .await
        .unwrap();
        async_fs::write(
            dir_new.join(memory_file::memory_filename(&older)),
            memory_file::write_memory_file(&older).unwrap(),
        )
        .await
        .unwrap();

        repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();

        let surviving = super::memory_files(&dir_new).await;
        assert_eq!(
            surviving.len(),
            1,
            "one memory ID must leave exactly one file: {surviving:?}"
        );
    }

    #[tokio::test]
    async fn refuses_when_another_registered_checkout_holds_the_old_id() {
        let tmp = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (_, stale) = drifted(&registry, tmp.path()).await;
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(RegistryEntry {
            project_id: stale.clone(),
            project_path: other.path().to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        let err = repair_project_id(&registry, tmp.path())
            .await
            .expect_err("must refuse to touch a shared data directory");
        assert!(format!("{err}").contains("shares that data"));
    }

    /// The destructive half: a sibling row holding the LIVE id would otherwise
    /// absorb our subscriptions while our own row was deleted.
    #[tokio::test]
    async fn refuses_when_another_registered_checkout_holds_the_new_id() {
        let tmp = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, _) = drifted(&registry, tmp.path()).await;
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(RegistryEntry {
            project_id: live.clone(),
            project_path: other.path().to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        let err = repair_project_id(&registry, tmp.path())
            .await
            .expect_err("must refuse when a sibling already holds the live ID");
        assert!(format!("{err}").contains("shares that data"));

        // And nothing was touched.
        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 2);
        assert!(row(&reg, &live).is_some());
    }

    /// Worktrees route to the main checkout; re-keying one would point its row
    /// at the worktree's own path hash and detach it from main.
    #[tokio::test]
    async fn refuses_inside_a_linked_worktree() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        let wt = tmp.path().join("feature");
        let wt_gitdir = main.join(".git").join("worktrees").join("feature");
        std::fs::create_dir_all(main.join(".git")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&wt_gitdir).unwrap();
        std::fs::write(wt_gitdir.join("commondir"), "../..").unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();

        let registry = InMemoryRegistry::new();
        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "stalewt00000000".to_string(),
            project_path: wt.to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        let err = repair_project_id(&registry, &wt)
            .await
            .expect_err("a worktree must not be re-keyed");
        assert!(format!("{err}").contains("worktree"));
    }

    #[tokio::test]
    async fn is_idempotent_and_actually_repaired_the_first_time() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, _) = drifted(&registry, tmp.path()).await;

        assert!(repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .is_some());
        // `None` on a second run is also what a repair that deleted our row
        // would produce, so assert the row is actually there and correct.
        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert_eq!(reg.projects[0].project_id, live);

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
        let (_, stale) = drifted(&registry, tmp.path()).await;
        let (_, source) = write_personal(&stale, "Pending").await;

        let plan = plan_repair(&registry, tmp.path()).await.unwrap().unwrap();
        assert_eq!(plan.old_id, stale);
        assert_eq!(plan.personal_migrated, 1);

        // Untouched — registry AND files. Asserting only the registry let a
        // `survey_personal` that actually copied the files pass.
        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects[0].project_id, stale);
        assert!(source.exists());
        let live_dir = paths::personal_memories_dir(&plan.new_id).unwrap();
        assert!(super::memory_files(&live_dir).await.is_empty());
    }
}
