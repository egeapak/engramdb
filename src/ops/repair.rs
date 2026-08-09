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
    /// The ID the registry recorded. When a project drifted more than once
    /// (`init` → add a remote → change the remote) there is more than one; this
    /// is the first, kept for display. [`RepairReport::old_ids`] is the full set
    /// and is what the migration actually walks.
    pub old_id: String,
    /// Every stale ID recorded for this path, in registry order. All of them
    /// are collapsed, and every one's personal memories are carried across —
    /// migrating only the first strands the directory the user was actually
    /// writing into before the last drift.
    pub old_ids: Vec<String>,
    /// The ID the directory hashes to today.
    pub new_id: String,
    /// Personal memory files copied from the old data dir to the live one.
    pub personal_migrated: usize,
    /// Personal files not copied because the live dir already held a newer
    /// copy of the same memory.
    pub personal_superseded: usize,
    /// Personal files left untouched because something on *either* side could
    /// not be read or parsed: an unreadable source, or a live file carrying the
    /// same memory ID that this binary cannot parse (replacing that one would
    /// destroy data of unknown vintage). Non-zero means an old directory still
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
    /// and prune only ever removes the *derived* half (`lancedb/`) — a
    /// directory holding personal memories is retained whole.
    pub old_data_dir: PathBuf,
    /// One entry per [`RepairReport::old_ids`] element, same order.
    pub old_data_dirs: Vec<PathBuf>,
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
    Ok(plan_from(&reg, dir).await?.map(|(report, _)| report))
}

/// [`plan_repair`] against an already-loaded snapshot, so the execute path can
/// re-derive the plan inside its critical section from the same rules.
///
/// Returns the stale rows alongside the report: the caller rewrites exactly
/// those, and re-deriving them from a second filter pass would leave the two
/// able to disagree (and an empty-vector index to panic on).
async fn plan_from(
    reg: &crate::storage::Registry,
    dir: &Path,
) -> Result<Option<(RepairReport, Vec<RegistryEntry>)>> {
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

    let canon = canon_of(dir);
    let new_id = project_id::compute_project_id(dir);
    let stale: Vec<RegistryEntry> = stale_registrations_for(reg, dir, &new_id)
        .into_iter()
        .cloned()
        .collect();
    if stale.is_empty() {
        return Ok(None);
    }
    let old_ids: Vec<String> = stale.iter().map(|e| e.project_id.clone()).collect();
    let old_id = old_ids[0].clone();

    // A row at ANOTHER path already holding the live ID is the one state that
    // makes the re-key destructive: the `live_row_here` branch would fold our
    // membership into *their* row, and the else branch would leave two rows
    // sharing one ID — `subscriptions_of` returns the first, so the drifted
    // project's groups still would not fan in and repair would report success.
    //
    // Deliberately NOT liveness-filtered. A dead sibling row that prune has not
    // collected yet produces the duplicate-ID registry just as surely as a live
    // one, and `exists()` also answers "gone" for an unreadable or unmounted
    // path. The old ID is *not* checked: two clones of one remote legitimately
    // share it, nothing here writes to or deletes the old directory, and
    // refusing there would permanently block a repair that needs nothing from
    // the sibling.
    if let Some(other) = other_checkout_with_id(reg, &new_id, &canon) {
        bail!(
            "the live project ID {} is already registered to the checkout at {} — refusing to \
             re-key, because two registry rows sharing one ID resolve to whichever comes first. \
             Re-register or `engramdb projects prune` that checkout first.",
            new_id,
            other.display()
        );
    }

    let (migrate, supersede, skipped) = survey_personal(&old_ids, &new_id).await;
    let report = RepairReport {
        path: dir.to_path_buf(),
        personal_migrated: migrate,
        personal_superseded: supersede,
        personal_skipped: skipped,
        removed_duplicate_entry: reg
            .projects
            .iter()
            .any(|e| e.project_id == new_id && same_path(e, &canon)),
        // Children of EVERY stale ID: the execute path re-points all of them,
        // and this is the one display whose stated purpose is the blast radius.
        reparented_children: children_of(reg, &old_ids),
        old_data_dir: data_dir_for(&old_id),
        old_data_dirs: old_ids.iter().map(|id| data_dir_for(id)).collect(),
        old_id,
        old_ids,
        new_id,
    };
    Ok(Some((report, stale)))
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
    let Some(pre) = plan_repair(registry, dir).await? else {
        return Ok(None);
    };

    // Locks are per-ID and this touches one per stale row plus the live one.
    // `flock` here is an unbounded block with no deadlock detection, so acquire
    // in a deterministic (sorted, deduped) order: two processes repairing in
    // opposite directions would otherwise deadlock. Every guard is dropped
    // before the caller reindexes — `MemoryStore::reindex` re-acquires, and a
    // second acquire in one process blocks forever.
    let mut lock_ids: Vec<String> = pre.old_ids.clone();
    lock_ids.push(pre.new_id.clone());
    lock_ids.sort();
    lock_ids.dedup();
    let mut _locks = Vec::with_capacity(lock_ids.len());
    for id in &lock_ids {
        _locks.push(crate::storage::write_lock::acquire_write_lock(id).await?);
    }

    // ONE critical section for read, decide, copy and write. Splitting the
    // decision across separate `load()`s let a concurrent prune or repair land
    // in between, producing a report that described work nobody did. The copy
    // is inside it too, so a plan that has since become `None` or unsafe cannot
    // leave files grafted into a directory the refusal exists to protect, nor
    // copied-but-never-indexed because the caller was told there was nothing
    // to do.
    let _reg_lock = registry.lock_exclusive().await?;
    let mut reg = registry.load().await?;

    // Re-derive under the lock rather than trusting the pre-lock plan.
    let Some((mut report, stale_here)) = plan_from(&reg, dir).await? else {
        // Someone else repaired it while we waited for the lock.
        return Ok(None);
    };

    // The per-ID locks above were taken against the pre-lock plan. If the
    // identity moved in that window (`git remote set-url`, a concurrent prune
    // dropping a stale row) they cover the wrong files, so stop rather than
    // migrate unlocked — a re-run picks up the new plan and is a no-op if
    // someone else got there first.
    if report.new_id != pre.new_id || report.old_ids != pre.old_ids {
        bail!(
            "{}'s registration changed while this repair was starting — nothing was modified. \
             Re-run `engramdb projects repair`.",
            dir.display()
        );
    }

    let (migrated, superseded, skipped) = copy_personal(&report.old_ids, &report.new_id).await?;
    report.personal_migrated = migrated;
    report.personal_superseded = superseded;
    report.personal_skipped = skipped;

    // Every lookup is scoped to THIS path. Matching by ID alone let a
    // different checkout's row absorb our subscriptions while our own row was
    // deleted — two clones of one remote is enough to trigger it.
    let canon = canon_of(dir);
    let live_row_here = reg
        .projects
        .iter()
        .any(|e| e.project_id == report.new_id && same_path(e, &canon));

    if live_row_here {
        // Fold every stale row's membership into the live one, then drop them.
        let (subs, parent) = merge_membership(&stale_here);
        if let Some(live) = reg
            .projects
            .iter_mut()
            .find(|e| e.project_id == report.new_id && same_path(e, &canon))
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
            .retain(|e| !(stale_ids.contains(&e.project_id.as_str()) && same_path(e, &canon)));
        report.removed_duplicate_entry = true;
    } else {
        // Re-key the first stale row in place — this is what preserves
        // `subscriptions` and `parent_project_id`; pushing a fresh entry would
        // silently drop both, and losing subscriptions does not error, queries
        // just stop fanning in group memories. Any further stale rows for this
        // path fold into it and are dropped.
        //
        // `stale_here` is non-empty by construction: `plan_from` returned
        // `Some`, and it hands back the very rows it planned against rather
        // than leaving the caller to re-filter (which could disagree, and did
        // index `[0]` of a vector a second `canonicalize` could empty).
        let (subs, parent) = merge_membership(&stale_here);
        let keep_id = stale_here[0].project_id.clone();
        if let Some(entry) = reg
            .projects
            .iter_mut()
            .find(|e| e.project_id == keep_id && same_path(e, &canon))
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
                .retain(|e| !(extra.contains(&e.project_id.as_str()) && same_path(e, &canon)));
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

/// `dir` canonicalized, falling back to the literal path.
///
/// Hoisted out of [`same_path`] deliberately: that predicate runs once per
/// registry row across five passes, and re-canonicalizing `dir` each time is
/// both a blocking syscall inside `async fn` (on a path that may be a dead
/// network mount) and a TOCTOU seam — the answer could change mid-rewrite.
fn canon_of(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// Whether a registry row points at `canon` (a path from [`canon_of`]).
fn same_path(entry: &RegistryEntry, canon: &Path) -> bool {
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

/// A checkout other than `canon` registered under `project_id`.
///
/// Liveness is deliberately not consulted — see the call site: the hazard this
/// guards is a duplicate ID in the registry, which a row whose directory is
/// gone (or merely unreadable) creates just as surely as a live one.
fn other_checkout_with_id(
    reg: &crate::storage::Registry,
    project_id: &str,
    canon: &Path,
) -> Option<PathBuf> {
    reg.projects
        .iter()
        .filter(|e| e.project_id == project_id)
        .map(|e| {
            let p = PathBuf::from(&e.project_path);
            p.canonicalize().unwrap_or(p)
        })
        .find(|p| p != canon)
}

/// Registry IDs whose `parent_project_id` is any of `parents`.
fn children_of(reg: &crate::storage::Registry, parents: &[String]) -> Vec<String> {
    reg.projects
        .iter()
        .filter(|e| {
            e.parent_project_id
                .as_deref()
                .is_some_and(|p| parents.iter().any(|s| s == p))
        })
        .map(|e| e.project_id.clone())
        .collect()
}

/// Count what [`copy_personal`] would do, without doing it.
///
/// Walks the same directories in the same order, but cannot model the target
/// dir filling up as earlier files land, so the counts are a preview rather
/// than a promise.
async fn survey_personal(old_ids: &[String], new_id: &str) -> (usize, usize, usize) {
    let Ok(to) = paths::personal_memories_dir(new_id) else {
        return (0, 0, 0);
    };
    let (mut copy, mut supersede, mut skip) = (0, 0, 0);
    for old_id in old_ids {
        let Ok(from) = paths::personal_memories_dir(old_id) else {
            continue;
        };
        for (_, path) in memory_files(&from).await {
            match plan_file(&path, &to).await {
                Some(FileAction::Copy { .. }) => copy += 1,
                Some(FileAction::Superseded) => supersede += 1,
                None => skip += 1,
            }
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
///
/// Every stale ID is walked, oldest registry row first, so a project that
/// drifted twice does not strand the directory it was actually writing into
/// before the last drift. Ordering is harmless: the collision rule is
/// timestamp-based, not arrival-based.
async fn copy_personal(old_ids: &[String], new_id: &str) -> Result<(usize, usize, usize)> {
    let to = paths::personal_memories_dir(new_id)?;
    let (mut copied, mut superseded, mut skipped) = (0, 0, 0);

    for old_id in old_ids {
        let from = paths::personal_memories_dir(old_id)?;
        if !from.exists() {
            continue;
        }
        async_fs::create_dir_all(&to).await?;
        for (file_name, path) in memory_files(&from).await {
            match plan_file(&path, &to).await {
                Some(FileAction::Copy { replaces }) => {
                    // Remove the older copy first, or the same memory ends up
                    // in two files and which one wins is `read_dir` order.
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
/// `None` covers an unreadable or unparseable file on *either* side. Because
/// nothing is deleted, "leave it alone" is genuinely safe here — it is reported
/// via `personal_skipped` so the user knows an old directory still holds data.
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
            Some(_) => Some(FileAction::Copy {
                replaces: Some(path),
            }),
            // The live file carries this memory ID but cannot be read or
            // parsed, so there is no timestamp to compare and replacing it
            // means unlinking data of unknown vintage. Protecting a corrupt
            // source while destroying a corrupt target would be exactly
            // backwards: skip, and report it.
            None => None,
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

    /// Two clones of one remote legitimately share the remote-derived ID. When
    /// only one of them changes its remote, the other is a healthy project that
    /// has nothing to do with this repair — refusing there would make the drift
    /// permanently unfixable without deregistering a working checkout. Nothing
    /// writes to or deletes the shared directory, so the repair is safe.
    #[tokio::test]
    async fn repairs_even_when_a_sibling_clone_still_answers_to_the_old_id() {
        let tmp = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, stale) = drifted(&registry, tmp.path()).await;
        let (_, sibling_file) = write_personal(&stale, "Sibling's own note").await;
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(RegistryEntry {
            project_id: stale.clone(),
            project_path: other.path().to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .expect("a sibling on the old ID must not block the repair");
        assert_eq!(report.new_id, live);

        let reg = registry.load().await.unwrap();
        assert_eq!(
            row(&reg, &stale).map(|e| e.project_path),
            Some(other.path().to_string_lossy().to_string()),
            "the sibling's registration must be left exactly as it was"
        );
        assert!(
            sibling_file.exists(),
            "and its data must be copied, never moved"
        );
    }

    /// The destructive half: a row at another path holding the LIVE id. In the
    /// `live_row_here` branch it would absorb our subscriptions while our own
    /// row was deleted; in the other, the re-key would leave two rows sharing
    /// one ID and `subscriptions_of` resolves the first — so repair would
    /// report success while the symptom persisted.
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
        assert!(format!("{err}").contains("already registered"));

        // And nothing was touched.
        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 2);
        assert!(row(&reg, &live).is_some());
    }

    /// The same refusal must fire for a row whose directory is *gone*. Filtering
    /// on liveness let a not-yet-pruned sibling through, and the re-key then
    /// produced two rows carrying one ID — the exact state repair exists to
    /// remove.
    #[tokio::test]
    async fn refuses_when_a_dead_row_elsewhere_already_holds_the_new_id() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, _) = drifted(&registry, tmp.path()).await;
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(RegistryEntry {
            project_id: live.clone(),
            project_path: tmp.path().join("removed-checkout").to_string_lossy().into(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        let err = repair_project_id(&registry, tmp.path())
            .await
            .expect_err("a dead duplicate row is still a duplicate ID");
        assert!(format!("{err}").contains("already registered"));
    }

    /// Drifting twice means the user was writing personal memories under the
    /// SECOND stale ID by the time the last drift happened. Migrating only
    /// `stale[0]` leaves those invisible, unmentioned, and un-migratable — the
    /// second run reports nothing to repair.
    #[tokio::test]
    async fn carries_personal_memories_from_every_stale_id() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, first_stale) = drifted(&registry, tmp.path()).await;
        let second_stale = format!("secnd{}", &live[..11]);
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(RegistryEntry {
            project_id: second_stale.clone(),
            project_path: tmp.path().to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        let (first_id, _) = write_personal(&first_stale, "From the first drift").await;
        let (second_id, _) = write_personal(&second_stale, "From the second drift").await;

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.personal_migrated, 2);
        assert_eq!(report.old_ids.len(), 2);

        let live_dir = paths::personal_memories_dir(&live).unwrap();
        let mut found = Vec::new();
        for (_, path) in super::memory_files(&live_dir).await {
            found.push(async_fs::read_to_string(&path).await.unwrap());
        }
        assert!(found.iter().any(|c| c.contains(&first_id)));
        assert!(
            found.iter().any(|c| c.contains(&second_id)),
            "the memory written under the second stale ID must come across too"
        );
    }

    /// The mirror of `an_unparseable_personal_file_is_skipped_and_reported`: a
    /// live file that cannot be parsed must not be unlinked and replaced by a
    /// stale copy of unknown vintage.
    #[tokio::test]
    async fn an_unparseable_file_in_the_live_dir_is_never_replaced() {
        let tmp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (live, stale) = drifted(&registry, tmp.path()).await;

        let dir_old = paths::personal_memories_dir(&stale).unwrap();
        let dir_new = paths::personal_memories_dir(&live).unwrap();
        async_fs::create_dir_all(&dir_old).await.unwrap();
        async_fs::create_dir_all(&dir_new).await.unwrap();

        let mut memory = Memory::new(MemoryType::Decision, "Stale", "old", Provenance::human());
        memory.visibility = Visibility::Personal;
        async_fs::write(
            dir_old.join(memory_file::memory_filename(&memory)),
            memory_file::write_memory_file(&memory).unwrap(),
        )
        .await
        .unwrap();

        // Same memory ID in the live dir (stored under a different title slug,
        // so the filenames differ), unparseable by this binary.
        let corrupt = dir_new.join(format!("live-copy_{}.md", memory.id));
        async_fs::write(&corrupt, "written by a newer schema")
            .await
            .unwrap();

        let report = repair_project_id(&registry, tmp.path())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(report.personal_migrated, 0);
        assert_eq!(report.personal_skipped, 1);
        assert_eq!(
            async_fs::read_to_string(&corrupt).await.unwrap(),
            "written by a newer schema",
            "an unparseable live file must survive untouched"
        );
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
