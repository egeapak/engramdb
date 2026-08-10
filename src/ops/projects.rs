//! Project management operations.
//!
//! Functions for inspecting, listing, deleting, linking, and aggregating
//! statistics across registered EngramDB projects.

use crate::storage::{
    collect_descendants, manifest, paths, resolve_root_project_id, MemoryStore, Registry,
    RegistryBackend, RegistryEntry,
};
use crate::types::MemoryType;
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tokio::fs as async_fs;

/// Information about a single project.
pub struct ProjectInfo {
    pub project_id: String,
    pub project_name: String,
    pub project_path: String,
    pub memory_count: usize,
    pub logical_scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// Parent project ID if this project is a sub-project (e.g. a worktree).
    pub parent_project_id: Option<String>,
}

/// Entry in the project list.
pub struct ProjectListEntry {
    pub project_id: String,
    pub project_path: String,
    pub exists: bool,
    /// Parent project ID if this project is a sub-project (e.g. a worktree).
    pub parent_project_id: Option<String>,
}

/// Result of deleting a project.
#[derive(Debug)]
pub struct DeleteResult {
    pub project_path: String,
    pub global_data_removed: bool,
    /// Project IDs of descendants that were also removed (cascade delete).
    /// Empty when cascade was not requested or the project had no descendants.
    pub cascaded_ids: Vec<String>,
    /// Data directories kept whole because they still hold data that exists
    /// nowhere else — personal memories or archived transcripts — and `purge`
    /// was not requested.
    pub retained_irreplaceable: Vec<String>,
}

/// Aggregate statistics across all projects.
pub struct AggregateStats {
    pub total_projects: usize,
    pub reachable_projects: usize,
    pub total_memories: usize,
    pub by_type: Vec<(MemoryType, usize)>,
}

/// Get info about the project in the given directory.
pub async fn get_project_info(dir: &Path) -> Result<ProjectInfo> {
    let store = MemoryStore::open(dir).await?;
    let manifest_path = paths::project_dir(dir).join("manifest.toml");
    let manifest = manifest::load_manifest(&manifest_path).await?;

    let summaries = store.list_summary().await?;
    let memory_count = summaries.len();

    let mut scope_set = std::collections::HashSet::new();
    for entry in &summaries {
        for scope in &entry.logical {
            scope_set.insert(scope.clone());
        }
    }
    let logical_scopes: Vec<String> = scope_set.into_iter().collect();

    let abs_path = dir
        .canonicalize()
        .unwrap_or_else(|_| dir.to_path_buf())
        .to_string_lossy()
        .to_string();

    Ok(ProjectInfo {
        project_id: store.project_id.clone(),
        project_name: manifest.project,
        project_path: abs_path,
        memory_count,
        logical_scopes,
        created_at: manifest.created_at,
        parent_project_id: manifest.parent_project_id,
    })
}

/// Whether a registry entry's project still exists on disk.
///
/// Sub-projects (worktrees) don't have their own `.engramdb/` — their storage
/// lives at the parent — so treat them as alive if the worktree directory
/// itself still exists. Root projects use the usual `.engramdb/` check. This is
/// the single source of truth shared by [`list_projects`] (for the `exists`
/// flag), [`prune_stale_projects`] (to decide what to remove), and `doctor`'s
/// reachable-projects count, so a linked worktree is never mistaken for a stale
/// entry — and so `doctor` can never report stale entries that `prune` then
/// declines to remove.
pub(crate) fn registry_entry_alive(e: &RegistryEntry) -> bool {
    // `try_exists`, with an error counting as ALIVE. `exists()` collapses
    // EACCES, ESTALE and a hung network mount into "gone", which would drop the
    // row and hand its data directory to the sweep while the checkout is merely
    // unreachable. This runs before `protected_project_ids`, so a fail-open
    // answer here makes that guard moot.
    let path = Path::new(&e.project_path);
    let target = if e.parent_project_id.is_some() {
        path.to_path_buf()
    } else {
        path.join(".engramdb")
    };
    target.try_exists().unwrap_or(true)
}

/// List all registered projects.
pub async fn list_projects(registry: &dyn RegistryBackend) -> Result<Vec<ProjectListEntry>> {
    let registry = registry.load().await?;

    let entries = registry
        .projects
        .into_iter()
        .map(|e| {
            let exists = registry_entry_alive(&e);
            ProjectListEntry {
                project_id: e.project_id,
                project_path: e.project_path,
                exists,
                parent_project_id: e.parent_project_id,
            }
        })
        .collect();

    Ok(entries)
}

/// Remove a project from the registry and delete its global data.
///
/// When `cascade` is true, also removes every descendant (direct or
/// transitive) of this project from the registry and deletes their global
/// data. This is the right choice when removing a parent whose children
/// (e.g. git worktrees) would otherwise be left dangling.
///
/// When `cascade` is false and the project has descendants, this function
/// returns an error rather than silently leaving orphaned children behind.
///
/// `purge` decides what happens to the data that lives *only* in the data
/// directory and has no copy in the project tree: personal memories, and the
/// transcript archives that outlive Claude Code's own pruning. With
/// `purge = false` (the default everywhere) the directory is kept whole
/// whenever it still holds either, index included, exactly as
/// `prune_stale_projects` does — because a
/// project ID derived from a git remote is shared by every
/// clone of that remote on the machine, and the registry keeps one row per ID,
/// so a sibling clone is structurally invisible here. Deleting project A's data
/// directory can therefore destroy project B's only copy, and no check inside
/// this function can rule that out. `purge = true` is the user saying they mean
/// it anyway.
pub async fn delete_project(
    registry: &dyn RegistryBackend,
    project_id: &str,
    cascade: bool,
    purge: bool,
) -> Result<DeleteResult> {
    // Registry removal is a manual load → mutate → save cycle, so it must
    // run under the backend's cross-process mutation lock — otherwise a
    // concurrent registration (locked `update_inner`) that lands between our
    // load and save is silently erased, and its data dir is then collected
    // as an orphan by the next prune. Held only across the registry rewrite;
    // dropped before the (slow) data-directory deletion below.
    let lock = registry.lock_exclusive().await?;
    let mut reg = registry.load().await?;

    let idx = reg.projects.iter().position(|e| e.project_id == project_id);

    let Some(idx) = idx else {
        bail!("Project '{}' not found in registry", project_id);
    };

    let descendants = collect_descendants(&reg, project_id);

    if !cascade && !descendants.is_empty() {
        bail!(
            "Project '{}' has {} descendant project(s). Re-run with `--cascade` to delete them too, or unlink them first.",
            project_id,
            descendants.len()
        );
    }

    let entry = reg.projects.remove(idx);
    // Remove descendants from registry as well.
    if cascade {
        reg.projects
            .retain(|e| !descendants.iter().any(|d| d == &e.project_id));
    }
    registry.save(&reg).await?;
    drop(lock);

    // Delete global data directory for this project.
    let projects_dir = paths::global_data_dir()?.join("projects");
    let global_project_dir = projects_dir.join(project_id);
    let mut retained_irreplaceable = Vec::new();
    let global_data_removed = if global_project_dir.exists() {
        if purge {
            async_fs::remove_dir_all(&global_project_dir).await?;
            true
        } else if reclaim_data_dir(&global_project_dir).await {
            true
        } else {
            retained_irreplaceable.push(project_id.to_string());
            false
        }
    } else {
        false
    };

    // Delete descendants' global data (only if we cascaded).
    if cascade {
        for desc_id in &descendants {
            let dir = projects_dir.join(desc_id);
            if dir.exists() {
                // Best-effort: don't abort the whole delete if one child's
                // data dir can't be removed.
                if purge {
                    let _ = async_fs::remove_dir_all(&dir).await;
                } else if !reclaim_data_dir(&dir).await {
                    retained_irreplaceable.push(desc_id.clone());
                }
            }
        }
    }

    Ok(DeleteResult {
        project_path: entry.project_path,
        global_data_removed,
        cascaded_ids: descendants,
        retained_irreplaceable,
    })
}

/// Link a child project to a parent, making the child a sub-project.
///
/// Rejects:
/// - linking to self
/// - linking where the parent is already a descendant of the child
///   (would form a cycle)
/// - linking when either project is not in the registry
pub async fn link_project(
    registry: &dyn RegistryBackend,
    child_id: &str,
    parent_id: &str,
) -> Result<()> {
    if child_id == parent_id {
        bail!("Cannot link a project to itself");
    }

    let reg = registry.load().await?;

    if !reg.projects.iter().any(|e| e.project_id == child_id) {
        bail!("Child project '{}' not found in registry", child_id);
    }
    if !reg.projects.iter().any(|e| e.project_id == parent_id) {
        bail!("Parent project '{}' not found in registry", parent_id);
    }

    // If the parent's root resolves to the child, adding this link would
    // create a cycle.
    let parent_root = resolve_root_project_id(&reg, parent_id);
    if parent_root == child_id {
        bail!(
            "Cannot link: '{}' is already an ancestor of '{}' (would create a cycle)",
            child_id,
            parent_id
        );
    }

    registry.set_parent(child_id, Some(parent_id)).await?;
    Ok(())
}

/// Remove the parent link on a project, promoting it back to a root project.
///
/// A project with no parent is a no-op.
pub async fn unlink_project(registry: &dyn RegistryBackend, child_id: &str) -> Result<()> {
    registry.set_parent(child_id, None).await?;
    Ok(())
}

/// Result of pruning stale projects.
#[derive(Debug)]
pub struct PruneResult {
    /// Number of stale registry entries removed.
    pub stale_removed: usize,
    /// Project IDs removed from registry.
    pub stale_ids: Vec<String>,
    /// Number of orphan data directories removed (on disk but not in registry).
    pub orphans_removed: usize,
    /// Orphan project IDs that were removed.
    pub orphan_ids: Vec<String>,
    /// Project IDs whose broken `parent_project_id` link was cleared
    /// (dangling, stale-parent, or cycle-participating sub-projects).
    pub hierarchy_cleared: Vec<String>,
    /// Data directories kept **whole** because they still hold data that
    /// exists nowhere else: personal memories, or archived transcripts whose
    /// originals Claude Code has already pruned. Deduplicated — a directory
    /// retained by the stale pass is seen again by the orphan sweep. Reported so the
    /// retained disk usage is visible rather than mysterious.
    pub retained_irreplaceable: Vec<String>,
}

/// Classification of a sub-project's parent chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentStatus {
    /// Parent chain resolves to a root that exists on disk.
    Ok,
    /// Parent ID (or an intermediate link) is not present in the registry.
    Dangling,
    /// Parent chain resolves to a root that has no `.engramdb/` directory.
    StaleParent,
    /// Parent chain loops back on itself.
    Cycle,
}

/// Hierarchy issues discovered in the registry.
#[derive(Debug, Default, Clone)]
pub struct HierarchyIssues {
    /// Sub-projects whose parent (or an intermediate ancestor) is missing.
    pub dangling: Vec<String>,
    /// Sub-projects whose root ancestor has no `.engramdb/` directory.
    pub stale_parent: Vec<String>,
    /// Sub-projects participating in a `parent_project_id` cycle.
    pub cycle_members: Vec<String>,
}

impl HierarchyIssues {
    /// Total number of affected sub-projects across all categories.
    pub fn total(&self) -> usize {
        self.dangling.len() + self.stale_parent.len() + self.cycle_members.len()
    }

    /// All affected project IDs, flattened across categories.
    fn into_all_ids(self) -> Vec<String> {
        let mut ids = self.dangling;
        ids.extend(self.stale_parent);
        ids.extend(self.cycle_members);
        ids
    }
}

/// Walk the parent chain of `child_id` and classify its outcome.
fn classify_parent_chain(registry: &Registry, child_id: &str) -> ParentStatus {
    let Some(child) = registry.projects.iter().find(|e| e.project_id == child_id) else {
        return ParentStatus::Ok;
    };
    let Some(mut current) = child.parent_project_id.as_deref() else {
        return ParentStatus::Ok;
    };

    let mut seen: HashSet<&str> = HashSet::new();
    seen.insert(child_id);

    loop {
        if !seen.insert(current) {
            return ParentStatus::Cycle;
        }
        let Some(entry) = registry.projects.iter().find(|e| e.project_id == current) else {
            return ParentStatus::Dangling;
        };
        match entry.parent_project_id.as_deref() {
            Some(next) => current = next,
            None => {
                return if Path::new(&entry.project_path).join(".engramdb").exists() {
                    ParentStatus::Ok
                } else {
                    ParentStatus::StaleParent
                };
            }
        }
    }
}

/// Scan the registry for broken `parent_project_id` links without modifying it.
pub fn scan_hierarchy_issues(registry: &Registry) -> HierarchyIssues {
    let mut out = HierarchyIssues::default();
    for entry in &registry.projects {
        if entry.parent_project_id.is_none() {
            continue;
        }
        match classify_parent_chain(registry, &entry.project_id) {
            ParentStatus::Ok => {}
            ParentStatus::Dangling => out.dangling.push(entry.project_id.clone()),
            ParentStatus::StaleParent => out.stale_parent.push(entry.project_id.clone()),
            ParentStatus::Cycle => out.cycle_members.push(entry.project_id.clone()),
        }
    }
    out
}

/// Scan-and-repair: clear `parent_project_id` on every sub-project with a
/// broken parent chain, promoting it back to a root.
///
/// Returns the issues that were repaired (empty when nothing was wrong).
pub async fn repair_hierarchy(registry: &dyn RegistryBackend) -> Result<HierarchyIssues> {
    // Scan and rewrite under the cross-process mutation lock so a concurrent
    // registration between our load and save isn't erased (see
    // `RegistryBackend::lock_exclusive`).
    let _lock = registry.lock_exclusive().await?;
    let mut reg = registry.load().await?;
    let issues = scan_hierarchy_issues(&reg);
    if issues.total() == 0 {
        return Ok(issues);
    }
    let ids: HashSet<String> = issues.clone().into_all_ids().into_iter().collect();
    for entry in reg.projects.iter_mut() {
        if ids.contains(&entry.project_id) {
            entry.parent_project_id = None;
        }
    }
    registry.save(&reg).await?;
    Ok(issues)
}

/// Count orphan data directories prune would actually reclaim.
///
/// "Not in the registry" is necessary but not sufficient: a directory still
/// holding personal memories is retained (see [`reclaim_data_dir`]), so
/// counting it here would make `doctor` warn "N orphan directories — run
/// `engramdb projects prune`" forever against a prune that removes nothing.
/// That is the same divergence `protected_project_ids` was introduced to end,
/// one rule further down.
pub async fn count_orphan_dirs(registry: &dyn RegistryBackend) -> Result<usize> {
    let reg = registry.load().await?;
    // Recorded IDs are not enough: a project re-keyed by a git remote added
    // after `init` still owns the data dir named by its *live* ID. See
    // `protected_project_ids`.
    let registered_ids = crate::storage::protected_project_ids(&reg);

    let projects_dir = paths::global_data_dir()?.join("projects");
    if !projects_dir.exists() {
        return Ok(0);
    }

    let mut count = 0;
    if let Ok(mut entries) = async_fs::read_dir(&projects_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if !entry.path().is_dir() {
                continue;
            }
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if !registered_ids.contains(&dir_name)
                && !crate::storage::paths::holds_irreplaceable_data(&entry.path()).await
            {
                count += 1;
            }
        }
    }

    Ok(count)
}

/// Reclaim a project data directory without ever destroying an only copy.
///
/// `<data>/projects/<id>/` holds derived data (`lancedb/`, `write.lock`) and
/// **authoritative** data (`personal/memories/*.md`, which exists nowhere
/// else). Deciding whether a directory is expendable from the registry alone
/// is not possible: two clones of one git remote share an ID, and
/// `RegistryBackend::update` keeps a single row per ID, so the *other* clone is
/// structurally invisible. Removing a checkout from disk therefore looked like
/// "this ID is dead" while a healthy sibling was still using it — reproduced,
/// and destroying the sibling's personal memories unattended via
/// `auto_maintain`.
///
/// So the rule is structural rather than provenance-based: keep the directory
/// whenever it still holds personal memories. A retained directory costs disk;
/// a deleted one costs the memories.
///
/// The predicate lives in `storage::paths` because four call sites must agree
/// on it — this one, the orphan sweep, `delete_project`, and worktree
/// consolidation — and a fifth (`count_orphan_dirs`) must agree on what is
/// *reclaimable* or `doctor` recommends a prune that then declines to act.
use crate::storage::paths::reclaim_project_data_dir as reclaim_data_dir;

/// Phase indicator for prune progress callbacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrunePhase {
    Stale,
    Orphan,
    Hierarchy,
}

/// Remove stale registry entries and orphan data directories.
///
/// Stale: in registry but project path no longer exists on disk.
/// Orphan: data directory exists under `projects/` but not in registry.
///
/// Neither ever removes personal memories — see [`reclaim_data_dir`]. Retained
/// directories come back in [`PruneResult::retained_irreplaceable`], and
/// [`count_orphan_dirs`] excludes them so `doctor` cannot recommend a prune
/// that then declines to act.
///
/// Deletion is sequential: it was `rayon`-parallel, but the retention check is
/// a directory read per candidate and interleaving those with `remove_dir_all`
/// across a rayon pool inside `async fn` bought less than it cost in blocking
/// the executor. `on_progress(phase)` still fires once per item removed, and
/// fires only for items actually reclaimed — size a progress bar from the
/// candidate count, not from `stale_removed`.
pub async fn prune_stale_projects(
    registry: &dyn RegistryBackend,
    on_progress: impl Fn(PrunePhase) + Send + Sync,
) -> Result<PruneResult> {
    // --- Stale registry entries ---
    //
    // The whole load → partition → save cycle runs under the backend's
    // cross-process mutation lock: without it, a project registered
    // concurrently (via the locked `update_inner`) between our load and save
    // is silently erased — and its data dir is then swept as an orphan on
    // the next prune pass. Directory deletion happens *after* the save,
    // outside the lock: once the entries are gone the dirs are plain
    // orphans, so a crash mid-delete just leaves work for the next pass.
    let lock = registry.lock_exclusive().await?;
    let mut reg = registry.load().await?;
    let (keep, stale): (Vec<_>, Vec<_>) = reg.projects.into_iter().partition(registry_entry_alive);
    let stale_removed = stale.len();
    reg.projects = keep;
    registry.save(&reg).await?;
    drop(lock);

    let projects_dir = paths::global_data_dir()?.join("projects");

    let stale_ids: Vec<String> = stale.iter().map(|e| e.project_id.clone()).collect();
    // A stale entry's data dir is only expendable if nothing that SURVIVED
    // still answers to that ID. Two clones of one git remote share an ID, so
    // deleting the dir of a removed checkout would take the surviving one's
    // index, vectors and personal memories with it — no drift required. The
    // set is computed from the post-removal registry for exactly that reason.
    let survivors = crate::storage::protected_project_ids(&reg);
    let mut retained_irreplaceable: Vec<String> = Vec::new();
    for entry in stale.iter().filter(|e| !survivors.contains(&e.project_id)) {
        let dir = projects_dir.join(&entry.project_id);
        if !dir.exists() {
            continue;
        }
        if !reclaim_data_dir(&dir).await {
            retained_irreplaceable.push(entry.project_id.clone());
        }
        on_progress(PrunePhase::Stale);
    }

    // --- Orphan data directories ---
    //
    // Re-load rather than reusing the pre-save snapshot: a project that
    // registered while the stale-dir deletion above ran would be missing
    // from the old snapshot and its fresh data dir would be swept as an
    // orphan. A fresh snapshot narrows that window to the sweep itself.
    // Same widened set as `count_orphan_dirs` — this is the destructive side,
    // so an ID missing here is a directory deleted outright.
    let registered_ids = crate::storage::protected_project_ids(&registry.load().await?);

    let mut orphan_paths = Vec::new();
    let mut orphan_ids = Vec::new();
    if projects_dir.exists() {
        if let Ok(mut entries) = async_fs::read_dir(&projects_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if !entry.path().is_dir() {
                    continue;
                }
                let dir_name = entry.file_name().to_string_lossy().to_string();
                if !registered_ids.contains(&dir_name) {
                    orphan_ids.push(dir_name);
                    orphan_paths.push(entry.path());
                }
            }
        }
    }

    let mut orphans_removed = 0;
    let mut kept_orphan_ids: Vec<String> = Vec::new();
    for (id, path) in orphan_ids.iter().zip(orphan_paths.iter()) {
        if reclaim_data_dir(path).await {
            orphans_removed += 1;
        } else {
            // Held back: still the only copy of its personal memories.
            retained_irreplaceable.push(id.clone());
            kept_orphan_ids.push(id.clone());
        }
        on_progress(PrunePhase::Orphan);
    }
    // A directory that was kept is not an orphan that was removed.
    orphan_ids.retain(|id| !kept_orphan_ids.contains(id));

    // A directory retained by the stale pass reaches the orphan sweep too —
    // its registry row was just removed, so it is no longer in
    // `registered_ids` — and is retained a second time. That is correct
    // behaviour for both passes and the wrong thing to report: the ID landed
    // in this list twice, so the CLI said "Kept 2 data director(ies): x, x"
    // and the JSON shipped the duplicate. Dedupe here rather than skipping
    // the second pass, which would need the sweep to know why a directory was
    // held back.
    let mut seen = HashSet::new();
    retained_irreplaceable.retain(|id| seen.insert(id.clone()));

    // --- Hierarchy repair ---
    //
    // Runs after stale removal so that any children orphaned by a stale
    // parent removal are caught in the same pass (they appear as "dangling"
    // after the parent is gone). Scan and rewrite share one critical
    // section: scanning one snapshot and mutating a re-loaded one would let
    // a registration between the two loads be erased by the save.
    let hierarchy_cleared = {
        let _lock = registry.lock_exclusive().await?;
        let mut reg = registry.load().await?;
        let issues = scan_hierarchy_issues(&reg);
        let hierarchy_cleared = issues.into_all_ids();
        if !hierarchy_cleared.is_empty() {
            let cleared_set: std::collections::HashSet<&str> =
                hierarchy_cleared.iter().map(|s| s.as_str()).collect();
            for entry in reg.projects.iter_mut() {
                if cleared_set.contains(entry.project_id.as_str()) {
                    entry.parent_project_id = None;
                }
            }
            registry.save(&reg).await?;
        }
        hierarchy_cleared
    };
    for _ in &hierarchy_cleared {
        on_progress(PrunePhase::Hierarchy);
    }

    Ok(PruneResult {
        stale_removed,
        stale_ids,
        orphans_removed,
        orphan_ids,
        hierarchy_cleared,
        retained_irreplaceable,
    })
}

/// Aggregate statistics across all registered projects.
pub async fn aggregate_stats(registry: &dyn RegistryBackend) -> Result<AggregateStats> {
    let reg = registry.load().await?;
    let total_projects = reg.projects.len();

    // Each project is an independent store open (config load, LanceDB
    // connect, possible schema migration) followed by one index scan, so the
    // walk is I/O-bound and the per-project cost dominates — it does not
    // shrink with a better query. Run a bounded number concurrently: on a
    // slow disk this is the difference between N serial round trips and
    // roughly N/CONCURRENCY. The cap keeps a machine with hundreds of
    // registered projects from opening hundreds of LanceDB connections at
    // once. Per-project write locks are independent, so concurrent opens of
    // *different* projects never contend.
    const CONCURRENCY: usize = 8;

    let per_project = futures_util::stream::iter(reg.projects.iter().map(|entry| async move {
        let dir = Path::new(&entry.project_path);
        if !dir.join(".engramdb").exists() {
            return None;
        }

        let store = MemoryStore::open(dir).await.ok()?;

        // Reachability is recorded even when the count below fails, matching
        // the sequential version: the store opened, so the project exists.
        // `count_by_type` reads one column instead of the seven
        // `list_summary` decodes — the six others (id, status, logical,
        // criticality, created_at, expires_at) cost a `serde_json` parse and
        // two RFC3339 parses per memory whose results were discarded here.
        Some(store.count_by_type().await.ok())
    }))
    .buffer_unordered(CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut reachable_projects = 0;
    let mut total_memories = 0;
    let mut type_counts: HashMap<MemoryType, usize> = HashMap::new();

    for counts in per_project.into_iter().flatten() {
        reachable_projects += 1;
        let Some(counts) = counts else { continue };
        for (type_, n) in counts {
            total_memories += n;
            *type_counts.entry(type_).or_insert(0) += n;
        }
    }

    let mut by_type: Vec<_> = type_counts.into_iter().collect();
    by_type.sort_by_key(|(t, _)| format!("{:?}", t));

    Ok(AggregateStats {
        total_projects,
        reachable_projects,
        total_memories,
        by_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, RegistryBackend};
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_get_project_info() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(dir, &registry).await.unwrap();

        let info = get_project_info(dir).await.unwrap();
        assert_eq!(info.project_id, store.project_id);
        assert_eq!(info.memory_count, 0);
        assert!(!info.project_name.is_empty());
        assert!(info.created_at <= Utc::now());
    }

    #[tokio::test]
    async fn test_get_project_info_with_memories() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(dir, &registry).await.unwrap();

        let mem1 = Memory::new(
            MemoryType::Decision,
            "First",
            "Content 1",
            Provenance::human(),
        );
        let mem2 = Memory::new(
            MemoryType::Context,
            "Second",
            "Content 2",
            Provenance::human(),
        );
        store.create(&mem1).await.unwrap();
        store.create(&mem2).await.unwrap();

        let info = get_project_info(dir).await.unwrap();
        assert_eq!(info.memory_count, 2);
    }

    #[tokio::test]
    async fn test_get_project_info_not_initialized() {
        let temp_dir = TempDir::new().unwrap();
        let result = get_project_info(temp_dir.path()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_projects_empty() {
        let registry = InMemoryRegistry::new();
        // Verify list_projects returns a Vec (may contain entries from other tests)
        let entries = list_projects(&registry).await.unwrap();
        // Just verify the function works and returns the right type
        let _ = entries.len();
    }

    #[tokio::test]
    async fn test_list_projects_with_entries() {
        let temp1 = TempDir::new().unwrap();
        let temp2 = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        let _store1 = MemoryStore::init(temp1.path(), &registry).await.unwrap();
        let _store2 = MemoryStore::init(temp2.path(), &registry).await.unwrap();

        // list_projects should succeed (registry is shared with parallel tests,
        // so we can't assert exact counts)
        let entries = list_projects(&registry).await.unwrap();
        // Verify each entry has the expected structure
        for entry in &entries {
            assert!(!entry.project_id.is_empty());
            assert!(!entry.project_path.is_empty());
        }
    }

    #[tokio::test]
    async fn test_list_projects_marks_missing() {
        // After init, delete the .engramdb dir to simulate a moved project.
        // list_projects should mark it as exists=false.
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let pid = store.project_id.clone();

        // Remove the .engramdb dir to simulate a missing project
        async_fs::remove_dir_all(temp_dir.path().join(".engramdb"))
            .await
            .unwrap();

        // Re-ensure registry entry exists
        registry.update(temp_dir.path(), &pid).await.unwrap();

        let entries = list_projects(&registry).await.unwrap();
        if let Some(entry) = entries.iter().find(|e| e.project_id == pid) {
            assert!(!entry.exists, "Entry should be marked as missing");
        }
        // If the entry isn't found (due to registry race), that's OK — just skip
    }

    #[tokio::test]
    async fn test_delete_project() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let pid = store.project_id.clone();
        let global_dir = paths::global_data_dir()
            .unwrap()
            .join("projects")
            .join(&pid);
        assert!(
            global_dir.exists(),
            "Global data dir should exist after init"
        );

        // Re-ensure our entry is in the registry right before deleting
        registry.update(temp_dir.path(), &pid).await.unwrap();

        let result = delete_project(&registry, &pid, false, true).await.unwrap();
        assert!(!result.project_path.is_empty());
        assert!(!global_dir.exists(), "Global data dir should be removed");
        assert!(result.cascaded_ids.is_empty());
    }

    #[tokio::test]
    async fn test_delete_project_not_found() {
        let registry = InMemoryRegistry::new();
        let result = delete_project(&registry, "nonexistent-id-12345", false, true).await;
        assert!(result.is_err());
    }

    /// `delete` removes a *registration*. The data directory behind it can be
    /// shared: a remote-derived ID is the same for every clone of that remote,
    /// and the registry keeps one row per ID, so a sibling clone is invisible
    /// here. Without `--purge`, personal memories — the only copy — survive.
    #[tokio::test]
    async fn delete_keeps_personal_memories_unless_purge_is_asked_for() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let pid = store.project_id.clone();

        let personal = paths::personal_memories_dir(&pid).unwrap();
        async_fs::create_dir_all(&personal).await.unwrap();
        let only_copy = personal.join("a-note_00000000-0000-0000-0000-000000000001.md");
        async_fs::write(&only_copy, "personal memory body")
            .await
            .unwrap();
        let lancedb = paths::global_data_dir()
            .unwrap()
            .join("projects")
            .join(&pid)
            .join("lancedb");
        assert!(lancedb.exists(), "init must have created an index");

        let result = delete_project(&registry, &pid, false, false).await.unwrap();
        assert!(registry.load().await.unwrap().projects.is_empty());
        assert!(
            only_copy.exists(),
            "a sibling clone's only copy must survive a registration delete"
        );
        // The directory is kept WHOLE, index included: it is retained because
        // an unregistered sibling may still be using it, and wiping that
        // sibling's index leaves a healthy project silently unsearchable.
        assert!(
            lancedb.exists(),
            "a retained directory must keep its index too"
        );
        assert!(!result.global_data_removed);
        assert_eq!(result.retained_irreplaceable, vec![pid.clone()]);

        // And `--purge` is the escape hatch that really removes it.
        registry.update(temp_dir.path(), &pid).await.unwrap();
        let result = delete_project(&registry, &pid, false, true).await.unwrap();
        assert!(result.global_data_removed);
        assert!(result.retained_irreplaceable.is_empty());
        assert!(!only_copy.exists());
    }

    #[tokio::test]
    async fn test_aggregate_stats_returns_valid_structure() {
        let registry = InMemoryRegistry::new();
        // Verify aggregate_stats returns a consistent structure
        let stats = aggregate_stats(&registry).await.unwrap();
        assert!(stats.reachable_projects <= stats.total_projects);
    }

    #[tokio::test]
    async fn test_aggregate_stats_counts_memories() {
        let registry = InMemoryRegistry::new();
        // Verify aggregate_stats succeeds and returns non-negative values
        let stats = aggregate_stats(&registry).await.unwrap();
        assert!(stats.reachable_projects <= stats.total_projects);
        // total_memories should be non-negative (always true for usize, but
        // this verifies the function ran to completion)
        let _ = stats.total_memories;
    }

    #[tokio::test]
    async fn test_aggregate_stats_unreachable_not_counted() {
        let registry = InMemoryRegistry::new();
        // aggregate_stats should never count unreachable projects in reachable count
        let stats = aggregate_stats(&registry).await.unwrap();
        assert!(stats.reachable_projects <= stats.total_projects);
    }

    #[tokio::test]
    async fn test_prune_stale_projects_removes_stale() {
        let registry = InMemoryRegistry::new();

        // Add a reachable project
        let temp_dir = TempDir::new().unwrap();
        let _store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        // Add a stale project (path doesn't exist)
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(crate::storage::registry::RegistryEntry {
            project_id: "stale-proj-001".to_string(),
            project_path: "/nonexistent/path/to/project".to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        assert_eq!(registry.load().await.unwrap().projects.len(), 2);

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();
        assert_eq!(result.stale_removed, 1);
        assert_eq!(result.stale_ids, vec!["stale-proj-001"]);

        let remaining = registry.load().await.unwrap();
        assert_eq!(remaining.projects.len(), 1);
        assert_ne!(remaining.projects[0].project_id, "stale-proj-001");
    }

    #[tokio::test]
    async fn test_prune_keeps_linked_worktree_subproject() {
        // A linked worktree is registered as a sub-project with no `.engramdb/`
        // of its own (its memories live at the parent). As long as the worktree
        // directory still exists, prune must NOT treat it as stale — otherwise
        // it would churn the very link that routes the worktree to main.
        let registry = InMemoryRegistry::new();

        let parent_dir = TempDir::new().unwrap();
        let parent = MemoryStore::init(parent_dir.path(), &registry)
            .await
            .unwrap();

        // Worktree dir exists but has no `.engramdb/` (consolidated into main).
        let worktree_dir = TempDir::new().unwrap();
        registry
            .update_with_parent(
                worktree_dir.path(),
                "wt-subproject-001",
                Some(&parent.project_id),
            )
            .await
            .unwrap();

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();
        assert!(
            !result.stale_ids.iter().any(|id| id == "wt-subproject-001"),
            "a live worktree sub-project must not be pruned as stale"
        );

        let reg = registry.load().await.unwrap();
        let wt = reg
            .projects
            .iter()
            .find(|e| e.project_id == "wt-subproject-001")
            .expect("worktree sub-project must remain registered");
        assert_eq!(
            wt.parent_project_id.as_deref(),
            Some(parent.project_id.as_str())
        );
    }

    /// A project re-keyed by a git remote added after `init` keeps operating
    /// under its NEW ID while the registry still records the OLD one. The new
    /// data dir is therefore absent from the recorded-ID set — and prune
    /// deletes orphans outright, taking the personal memories that exist
    /// nowhere else. `auto_maintain` runs this prune unattended, so nothing
    /// about the deletion is opt-in.
    #[tokio::test]
    async fn prune_keeps_the_live_data_dir_of_a_rekeyed_project() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(dir, &registry).await.unwrap();
        let live_id = store.project_id.clone();

        // Re-key the registry entry to a stale ID, exactly as adding a git
        // remote after `init` does (the path is unchanged, so the entry is not
        // "stale" by the liveness predicate — only its ID is wrong).
        let stale_id = "0000000000000000".to_string();
        let mut reg = registry.load().await.unwrap();
        reg.projects
            .iter_mut()
            .find(|e| e.project_id == live_id)
            .unwrap()
            .project_id = stale_id.clone();
        registry.save(&reg).await.unwrap();

        // A personal memory lives ONLY in the live data dir — there is no copy
        // in the project tree, so deleting that dir is unrecoverable.
        let personal = paths::personal_memories_dir(&live_id).unwrap();
        async_fs::create_dir_all(&personal).await.unwrap();
        let personal_file = personal.join("only-copy.md");
        async_fs::write(&personal_file, "---\n---\n").await.unwrap();

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();

        assert!(
            personal_file.exists(),
            "prune deleted the live data dir of a re-keyed project, destroying \
             the only copy of its personal memories"
        );
        assert!(
            !result.orphan_ids.contains(&live_id),
            "the live ID of a registered path must never be swept as an orphan"
        );
    }

    /// The global store owns `projects/__global_store__/` but is never a
    /// registry row, so the orphan sweep deleted it — and its personal
    /// memories, which exist nowhere else. One ordinary command was enough:
    /// `auto_maintain` runs this prune unattended.
    #[tokio::test]
    async fn prune_keeps_the_global_and_group_stores() {
        let registry = InMemoryRegistry::new();

        let global_personal = paths::personal_memories_dir(paths::GLOBAL_PROJECT_ID).unwrap();
        async_fs::create_dir_all(&global_personal).await.unwrap();
        let global_file = global_personal.join("global-only-copy.md");
        async_fs::write(&global_file, "---\n---\n").await.unwrap();

        let group_id = paths::compute_group_id("team");
        let group_personal = paths::personal_memories_dir(&group_id).unwrap();
        async_fs::create_dir_all(&group_personal).await.unwrap();
        let group_file = group_personal.join("group-only-copy.md");
        async_fs::write(&group_file, "---\n---\n").await.unwrap();

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();

        assert!(
            global_file.exists(),
            "prune deleted the global store's personal memories"
        );
        assert!(
            group_file.exists(),
            "prune deleted a group store's personal memories"
        );
        assert!(
            result.orphan_ids.is_empty(),
            "internal stores must not be reported as orphans: {:?}",
            result.orphan_ids
        );
    }

    /// Two clones of one git remote share a project ID, and the registry keeps
    /// exactly ONE row per ID (`update_inner_impl` declines to add a second),
    /// so the sibling is structurally invisible. Removing the registered
    /// checkout from disk therefore looks like "this ID is dead" while a
    /// healthy clone is still using it.
    ///
    /// Reproduced against the real binary before this guard: `rm -rf` one
    /// clone, run any command in an unrelated project, and the surviving
    /// clone's only copy of its personal memories was gone. No registry-derived
    /// predicate can see the sibling — hence the structural rule that personal
    /// memories are never deleted by prune.
    #[tokio::test]
    async fn prune_never_deletes_personal_memories_of_an_invisible_sibling() {
        let registry = InMemoryRegistry::new();
        let gone = TempDir::new().unwrap();
        let shared_id = "shared0000000000";

        // Exactly one row, as the registry actually stores it, and its path is
        // about to vanish.
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(RegistryEntry {
            project_id: shared_id.to_string(),
            project_path: gone
                .path()
                .join("removed-clone")
                .to_string_lossy()
                .to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        // The sibling clone's personal memory — its only copy.
        let personal = paths::personal_memories_dir(shared_id).unwrap();
        async_fs::create_dir_all(&personal).await.unwrap();
        let file = personal.join("sibling-only-copy.md");
        async_fs::write(&file, "---\n---\n").await.unwrap();
        // The index the invisible sibling is still querying through.
        let lance = paths::lancedb_dir(shared_id).unwrap();
        async_fs::create_dir_all(&lance).await.unwrap();

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();

        assert!(
            file.exists(),
            "prune destroyed the only copy of an invisible sibling's personal memories"
        );
        // Kept too. The index is rebuildable in principle, but only by the
        // checkout that owns it — and that checkout is invisible here, so
        // reclaiming it on every sweep leaves a healthy project silently
        // unsearchable until someone re-embeds it by hand.
        assert!(
            lance.exists(),
            "a directory kept for its personal memories must be kept whole"
        );
        // `assert_eq!` on the whole vector, not `contains`: a retained stale
        // directory is seen AGAIN by the orphan sweep (its registry row is
        // gone by then), so it was reported twice — "Kept 2 data director(ies):
        // x, x" — and `contains` could not tell.
        assert_eq!(
            result.retained_irreplaceable,
            vec![shared_id.to_string()],
            "a retained directory must be reported exactly once: {result:?}"
        );
    }

    // The retention rule is enforced at each CALLER, so pinning the predicate
    // alone is not enough: re-inline a `personal/memories`-only check in
    // `prune_stale_projects` and every test above still passes while prune —
    // which runs unattended from `auto_maintain` — deletes every transcript
    // archive on the machine. Claude Code has already pruned the originals.
    #[tokio::test]
    async fn prune_keeps_a_directory_holding_only_transcripts() {
        let registry = InMemoryRegistry::new();

        // An orphan data dir: no registry row at all, no personal memories,
        // nothing but an archived transcript.
        let orphan_id = "transcriptonly00";
        let transcripts = paths::transcript_archive_dir(orphan_id).unwrap();
        async_fs::create_dir_all(&transcripts).await.unwrap();
        let archive = transcripts.join("session-a.jsonl.zst");
        async_fs::write(&archive, b"zstd").await.unwrap();

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();

        assert!(
            archive.exists(),
            "prune destroyed the last copy of an archived transcript"
        );
        assert_eq!(result.orphans_removed, 0);
        assert_eq!(result.retained_irreplaceable, vec![orphan_id.to_string()]);
    }

    // Same rule, the third payload: `lancedb/conversations.lance` holds the
    // curated per-session summaries, which nothing regenerates. A project with
    // `[harvest] archive = false`, or one old enough for archive eviction to
    // have run, has summaries and no transcripts to imply them.
    #[tokio::test]
    async fn prune_keeps_a_directory_holding_only_conversation_summaries() {
        let registry = InMemoryRegistry::new();

        let orphan_id = "summariesonly000";
        let lance = paths::lancedb_dir(orphan_id).unwrap();
        async_fs::create_dir_all(lance.join("conversations.lance"))
            .await
            .unwrap();

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();

        assert!(
            lance.join("conversations.lance").exists(),
            "prune destroyed curated conversation summaries"
        );
        assert_eq!(result.orphans_removed, 0);
        assert_eq!(result.retained_irreplaceable, vec![orphan_id.to_string()]);
    }

    // `doctor` counts what prune will reclaim. If the two ever disagree the
    // user gets a warning they can never clear — the exact failure this
    // branch set out to remove — so assert the NUMBERS agree, not just that
    // each is individually plausible.
    #[tokio::test]
    async fn doctor_orphan_count_agrees_with_what_prune_reclaims() {
        let registry = InMemoryRegistry::new();

        // One genuinely reclaimable orphan: derived data only.
        let dead = "deadindexonly000";
        async_fs::create_dir_all(paths::lancedb_dir(dead).unwrap())
            .await
            .unwrap();
        // One that must be retained.
        let held = "heldpersonal0000";
        let personal = paths::personal_memories_dir(held).unwrap();
        async_fs::create_dir_all(&personal).await.unwrap();
        async_fs::write(
            personal.join("only-copy.md"),
            "---
---
",
        )
        .await
        .unwrap();

        let counted = count_orphan_dirs(&registry).await.unwrap();
        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();

        assert_eq!(
            counted, result.orphans_removed,
            "doctor promised {counted} reclaimable orphan(s), prune reclaimed {}",
            result.orphans_removed
        );
        assert_eq!(counted, 1);
        assert!(personal.join("only-copy.md").exists());
    }

    // Cascade is the highest-blast-radius delete there is, and every cascade
    // test used `purge = true`, so the descendant branch of the retention rule
    // never ran.
    #[tokio::test]
    async fn cascade_delete_without_purge_keeps_a_child_s_personal_memories() {
        let registry = InMemoryRegistry::new();
        let tmp = TempDir::new().unwrap();

        let parent_id = "parentproject000";
        let child_id = "childproject0000";
        let mut reg = registry.load().await.unwrap();
        reg.projects.push(RegistryEntry {
            project_id: parent_id.to_string(),
            project_path: tmp.path().join("parent").to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        reg.projects.push(RegistryEntry {
            project_id: child_id.to_string(),
            project_path: tmp.path().join("child").to_string_lossy().to_string(),
            parent_project_id: Some(parent_id.to_string()),
            subscriptions: vec![],
        });
        registry.save(&reg).await.unwrap();

        let child_personal = paths::personal_memories_dir(child_id).unwrap();
        async_fs::create_dir_all(&child_personal).await.unwrap();
        let file = child_personal.join("child-only-copy.md");
        async_fs::write(
            &file, "---
---
",
        )
        .await
        .unwrap();

        let result = delete_project(&registry, parent_id, true, false)
            .await
            .unwrap();

        assert!(
            file.exists(),
            "cascade delete destroyed a descendant's only copy of its personal memories"
        );
        assert!(result
            .retained_irreplaceable
            .contains(&child_id.to_string()));
    }

    /// The other half of the invariant: a data directory with nothing
    /// authoritative left in it IS reclaimed, so the guard can't degenerate
    /// into "prune never deletes anything".
    #[tokio::test]
    async fn prune_still_reclaims_a_directory_with_no_personal_memories() {
        let registry = InMemoryRegistry::new();
        let projects_dir = paths::global_data_dir().unwrap().join("projects");
        let dead = projects_dir.join("deadbeef00000000");
        async_fs::create_dir_all(dead.join("lancedb"))
            .await
            .unwrap();

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();

        assert!(!dead.exists(), "a directory with no only-copy data must go");
        assert!(result.orphan_ids.contains(&"deadbeef00000000".to_string()));
        assert_eq!(result.orphans_removed, 1);
    }

    /// The live ID must be derived with `compute_project_id` — which prefers
    /// the git remote — not with a path hash. Every other test here builds a
    /// project in a bare `TempDir` where the two coincide, so a "stop reading
    /// .git/config on every prune" optimization would restore the original bug
    /// with the whole suite still green.
    #[tokio::test]
    async fn prune_keeps_the_live_dir_of_a_project_re_keyed_by_a_real_git_remote() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();
        let registry = InMemoryRegistry::new();
        // Registered before the remote exists: the recorded ID is the path hash.
        let store = MemoryStore::init(dir, &registry).await.unwrap();
        let recorded_id = store.project_id.clone();

        async_fs::create_dir_all(dir.join(".git")).await.unwrap();
        async_fs::write(
            dir.join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/rekeyed.git\n",
        )
        .await
        .unwrap();

        let live_id = crate::storage::project_id::compute_project_id(dir);
        assert_ne!(live_id, recorded_id, "the remote must change the ID");

        let personal = paths::personal_memories_dir(&live_id).unwrap();
        async_fs::create_dir_all(&personal).await.unwrap();
        let file = personal.join("remote-only-copy.md");
        async_fs::write(&file, "---\n---\n").await.unwrap();

        prune_stale_projects(&registry, |_| {}).await.unwrap();

        assert!(
            file.exists(),
            "the remote-derived live data dir was swept as an orphan"
        );
    }

    #[test]
    fn protected_ids_cover_both_the_recorded_and_the_live_id() {
        let temp_dir = TempDir::new().unwrap();
        let dir = temp_dir.path();
        let live_id = crate::storage::project_id::compute_project_id(dir);

        let mut reg = Registry::default();
        reg.projects.push(RegistryEntry {
            project_id: "stale00000000000".to_string(),
            project_path: dir.to_string_lossy().to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });

        let protected = crate::storage::protected_project_ids(&reg);
        assert!(protected.contains("stale00000000000"), "recorded ID");
        assert!(protected.contains(&live_id), "ID the path hashes to today");

        // A vanished path contributes only its recorded ID: there is nothing on
        // disk to re-hash, and the entry is the stale-entry case instead.
        let mut gone = Registry::default();
        gone.projects.push(RegistryEntry {
            project_id: "ghost00000000000".to_string(),
            project_path: "/nonexistent/protected-ids-test".to_string(),
            parent_project_id: None,
            subscriptions: vec![],
        });
        let gone_protected = crate::storage::protected_project_ids(&gone);
        assert!(gone_protected.contains("ghost00000000000"), "recorded ID");
        assert!(
            !gone_protected.contains(&crate::storage::project_id::compute_project_id(Path::new(
                "/nonexistent/protected-ids-test"
            ))),
            "a vanished path must not contribute a bogus live ID"
        );

        // Internal stores own a `projects/<id>/` dir but are never project
        // rows — the global store nowhere, groups in `Registry::groups`.
        // Missing them let one ordinary command delete every personal memory
        // in the global store.
        let protected = crate::storage::protected_project_ids(&Registry::default());
        assert!(protected.contains(crate::storage::paths::GLOBAL_PROJECT_ID));
        assert!(protected.contains(&crate::storage::paths::compute_group_id("team")));
    }

    #[tokio::test]
    async fn test_prune_stale_projects_nothing_to_prune() {
        let registry = InMemoryRegistry::new();

        let temp_dir = TempDir::new().unwrap();
        let _store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();
        assert_eq!(result.stale_removed, 0);
        assert!(result.stale_ids.is_empty());

        // Original entry should still be there
        assert_eq!(registry.load().await.unwrap().projects.len(), 1);
    }

    #[tokio::test]
    async fn test_prune_stale_projects_empty_registry() {
        let registry = InMemoryRegistry::new();
        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();
        assert_eq!(result.stale_removed, 0);
        assert!(result.stale_ids.is_empty());
    }

    // ---- link / unlink ----

    #[tokio::test]
    async fn test_link_project_sets_parent() {
        let temp_parent = TempDir::new().unwrap();
        let temp_child = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        let parent = MemoryStore::init(temp_parent.path(), &registry)
            .await
            .unwrap();
        let child = MemoryStore::init(temp_child.path(), &registry)
            .await
            .unwrap();

        link_project(&registry, &child.project_id, &parent.project_id)
            .await
            .unwrap();

        let loaded = registry.load().await.unwrap();
        let child_entry = loaded
            .projects
            .iter()
            .find(|e| e.project_id == child.project_id)
            .unwrap();
        assert_eq!(
            child_entry.parent_project_id.as_deref(),
            Some(parent.project_id.as_str())
        );
    }

    #[tokio::test]
    async fn test_link_project_rejects_self() {
        let temp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp.path(), &registry).await.unwrap();
        let err = link_project(&registry, &store.project_id, &store.project_id)
            .await
            .expect_err("self-link must fail");
        assert!(format!("{err}").to_lowercase().contains("itself"));
    }

    #[tokio::test]
    async fn test_link_project_rejects_cycle() {
        let temp_a = TempDir::new().unwrap();
        let temp_b = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        let a = MemoryStore::init(temp_a.path(), &registry).await.unwrap();
        let b = MemoryStore::init(temp_b.path(), &registry).await.unwrap();

        // b -> a
        link_project(&registry, &b.project_id, &a.project_id)
            .await
            .unwrap();

        // Now try a -> b: this would make b the parent of a, but b already
        // resolves to root `a` via the chain → cycle.
        let err = link_project(&registry, &a.project_id, &b.project_id)
            .await
            .expect_err("cycle must be rejected");
        assert!(format!("{err}").to_lowercase().contains("cycle"));
    }

    #[tokio::test]
    async fn test_link_project_rejects_missing_child() {
        let temp = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let parent = MemoryStore::init(temp.path(), &registry).await.unwrap();
        let err = link_project(&registry, "does-not-exist", &parent.project_id)
            .await
            .expect_err("missing child must fail");
        assert!(format!("{err}").to_lowercase().contains("child"));
    }

    #[tokio::test]
    async fn test_unlink_project_clears_parent() {
        let temp_parent = TempDir::new().unwrap();
        let temp_child = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        let parent = MemoryStore::init(temp_parent.path(), &registry)
            .await
            .unwrap();
        let child = MemoryStore::init(temp_child.path(), &registry)
            .await
            .unwrap();
        link_project(&registry, &child.project_id, &parent.project_id)
            .await
            .unwrap();

        unlink_project(&registry, &child.project_id).await.unwrap();

        let loaded = registry.load().await.unwrap();
        let child_entry = loaded
            .projects
            .iter()
            .find(|e| e.project_id == child.project_id)
            .unwrap();
        assert_eq!(child_entry.parent_project_id, None);
    }

    // ---- cascade delete ----

    #[tokio::test]
    async fn test_delete_project_without_cascade_errors_when_children_exist() {
        let temp_parent = TempDir::new().unwrap();
        let temp_child = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        let parent = MemoryStore::init(temp_parent.path(), &registry)
            .await
            .unwrap();
        let child = MemoryStore::init(temp_child.path(), &registry)
            .await
            .unwrap();
        link_project(&registry, &child.project_id, &parent.project_id)
            .await
            .unwrap();

        let err = delete_project(&registry, &parent.project_id, false, true)
            .await
            .expect_err("must refuse to delete a parent with children by default");
        assert!(format!("{err}").to_lowercase().contains("descendant"));

        // Parent should still be in the registry.
        let loaded = registry.load().await.unwrap();
        assert!(loaded
            .projects
            .iter()
            .any(|e| e.project_id == parent.project_id));
    }

    #[tokio::test]
    async fn test_delete_project_with_cascade_removes_descendants() {
        let temp_root = TempDir::new().unwrap();
        let temp_a = TempDir::new().unwrap();
        let temp_a1 = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        let root = MemoryStore::init(temp_root.path(), &registry)
            .await
            .unwrap();
        let a = MemoryStore::init(temp_a.path(), &registry).await.unwrap();
        let a1 = MemoryStore::init(temp_a1.path(), &registry).await.unwrap();

        // root -> a -> a1
        link_project(&registry, &a.project_id, &root.project_id)
            .await
            .unwrap();
        link_project(&registry, &a1.project_id, &a.project_id)
            .await
            .unwrap();

        let result = delete_project(&registry, &root.project_id, true, true)
            .await
            .unwrap();

        // Both descendants reported.
        let mut cascaded = result.cascaded_ids.clone();
        cascaded.sort();
        let mut expected = vec![a.project_id.clone(), a1.project_id.clone()];
        expected.sort();
        assert_eq!(cascaded, expected);

        let loaded = registry.load().await.unwrap();
        for id in [&root.project_id, &a.project_id, &a1.project_id] {
            assert!(
                !loaded.projects.iter().any(|e| &e.project_id == id),
                "{} should have been removed from registry",
                id
            );
        }
    }

    #[tokio::test]
    async fn test_delete_project_cascade_removes_global_data_dirs() {
        let temp_parent = TempDir::new().unwrap();
        let temp_child = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        let parent = MemoryStore::init(temp_parent.path(), &registry)
            .await
            .unwrap();
        let child = MemoryStore::init(temp_child.path(), &registry)
            .await
            .unwrap();
        link_project(&registry, &child.project_id, &parent.project_id)
            .await
            .unwrap();

        let projects_dir = paths::global_data_dir().unwrap().join("projects");
        let parent_global = projects_dir.join(&parent.project_id);
        let child_global = projects_dir.join(&child.project_id);
        assert!(parent_global.exists());
        assert!(child_global.exists());

        delete_project(&registry, &parent.project_id, true, true)
            .await
            .unwrap();

        assert!(!parent_global.exists(), "parent global dir must be removed");
        assert!(!child_global.exists(), "child global dir must be removed");
    }

    // ---- hierarchy scan / repair ----

    #[tokio::test]
    async fn test_scan_hierarchy_issues_healthy_registry() {
        let temp_parent = TempDir::new().unwrap();
        let temp_child = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let parent = MemoryStore::init(temp_parent.path(), &registry)
            .await
            .unwrap();
        let child = MemoryStore::init(temp_child.path(), &registry)
            .await
            .unwrap();
        link_project(&registry, &child.project_id, &parent.project_id)
            .await
            .unwrap();

        let reg = registry.load().await.unwrap();
        let issues = scan_hierarchy_issues(&reg);
        assert_eq!(issues.total(), 0);
    }

    #[tokio::test]
    async fn test_scan_hierarchy_issues_detects_dangling() {
        let temp_child = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let child = MemoryStore::init(temp_child.path(), &registry)
            .await
            .unwrap();
        // Hand-craft a dangling parent link (parent ID not in registry).
        let mut reg = registry.load().await.unwrap();
        reg.projects
            .iter_mut()
            .find(|e| e.project_id == child.project_id)
            .unwrap()
            .parent_project_id = Some("nonexistent-parent-id".to_string());
        registry.save(&reg).await.unwrap();

        let reg = registry.load().await.unwrap();
        let issues = scan_hierarchy_issues(&reg);
        assert_eq!(issues.dangling, vec![child.project_id.clone()]);
        assert!(issues.stale_parent.is_empty());
        assert!(issues.cycle_members.is_empty());
    }

    #[tokio::test]
    async fn test_scan_hierarchy_issues_detects_stale_parent() {
        let temp_parent = TempDir::new().unwrap();
        let temp_child = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let parent = MemoryStore::init(temp_parent.path(), &registry)
            .await
            .unwrap();
        let child = MemoryStore::init(temp_child.path(), &registry)
            .await
            .unwrap();
        link_project(&registry, &child.project_id, &parent.project_id)
            .await
            .unwrap();

        // Remove parent's .engramdb/ to simulate a stale root.
        async_fs::remove_dir_all(temp_parent.path().join(".engramdb"))
            .await
            .unwrap();

        let reg = registry.load().await.unwrap();
        let issues = scan_hierarchy_issues(&reg);
        assert_eq!(issues.stale_parent, vec![child.project_id.clone()]);
    }

    #[tokio::test]
    async fn test_scan_hierarchy_issues_detects_cycle() {
        let temp_a = TempDir::new().unwrap();
        let temp_b = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let a = MemoryStore::init(temp_a.path(), &registry).await.unwrap();
        let b = MemoryStore::init(temp_b.path(), &registry).await.unwrap();

        // Hand-craft cycle: a -> b, b -> a.
        let mut reg = registry.load().await.unwrap();
        for entry in reg.projects.iter_mut() {
            if entry.project_id == a.project_id {
                entry.parent_project_id = Some(b.project_id.clone());
            } else if entry.project_id == b.project_id {
                entry.parent_project_id = Some(a.project_id.clone());
            }
        }
        registry.save(&reg).await.unwrap();

        let reg = registry.load().await.unwrap();
        let issues = scan_hierarchy_issues(&reg);
        let mut cycle = issues.cycle_members.clone();
        cycle.sort();
        let mut expected = vec![a.project_id.clone(), b.project_id.clone()];
        expected.sort();
        assert_eq!(cycle, expected);
    }

    #[tokio::test]
    async fn test_repair_hierarchy_clears_broken_links() {
        let temp_child = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let child = MemoryStore::init(temp_child.path(), &registry)
            .await
            .unwrap();
        // Dangling parent.
        let mut reg = registry.load().await.unwrap();
        reg.projects
            .iter_mut()
            .find(|e| e.project_id == child.project_id)
            .unwrap()
            .parent_project_id = Some("ghost".to_string());
        registry.save(&reg).await.unwrap();

        let repaired = repair_hierarchy(&registry).await.unwrap();
        assert_eq!(repaired.dangling, vec![child.project_id.clone()]);

        let reg = registry.load().await.unwrap();
        let child_entry = reg
            .projects
            .iter()
            .find(|e| e.project_id == child.project_id)
            .unwrap();
        assert_eq!(child_entry.parent_project_id, None);
    }

    #[tokio::test]
    async fn test_repair_hierarchy_noop_when_healthy() {
        let registry = InMemoryRegistry::new();
        let temp = TempDir::new().unwrap();
        let _store = MemoryStore::init(temp.path(), &registry).await.unwrap();

        let repaired = repair_hierarchy(&registry).await.unwrap();
        assert_eq!(repaired.total(), 0);
    }

    #[tokio::test]
    async fn test_prune_repairs_orphaned_children_after_stale_parent_removal() {
        // Parent's .engramdb/ is gone → stale → prune removes the parent.
        // Child was linked to parent → after removal, child's parent_project_id
        // points to a registry ID that no longer exists. Prune must clear it.
        let temp_parent = TempDir::new().unwrap();
        let temp_child = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let parent = MemoryStore::init(temp_parent.path(), &registry)
            .await
            .unwrap();
        let child = MemoryStore::init(temp_child.path(), &registry)
            .await
            .unwrap();
        link_project(&registry, &child.project_id, &parent.project_id)
            .await
            .unwrap();

        // Make parent stale: remove its .engramdb/.
        async_fs::remove_dir_all(temp_parent.path().join(".engramdb"))
            .await
            .unwrap();

        let result = prune_stale_projects(&registry, |_| {}).await.unwrap();
        assert!(result.stale_ids.contains(&parent.project_id));
        assert_eq!(result.hierarchy_cleared, vec![child.project_id.clone()]);

        let reg = registry.load().await.unwrap();
        let child_entry = reg
            .projects
            .iter()
            .find(|e| e.project_id == child.project_id)
            .unwrap();
        assert_eq!(child_entry.parent_project_id, None);
    }

    /// A linked worktree is registered as a sub-project and deliberately has no
    /// `.engramdb/` of its own — its storage lives at the parent. A bare
    /// `<path>/.engramdb` liveness test therefore counts it stale, which is how
    /// `doctor` came to report "stale: 4" for entries `prune` then declined to
    /// remove: two copies of the predicate had drifted apart.
    ///
    /// This pins the contract both call sites now share: a sub-project whose
    /// directory still exists is ALIVE even with no `.engramdb/`, and a root
    /// project without one is stale.
    #[test]
    fn sub_project_without_local_engramdb_dir_is_alive() {
        let temp_dir = TempDir::new().unwrap();
        let worktree = temp_dir.path().join("feature-worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        // Deliberately no `.engramdb/` — that is the whole point of a worktree.

        let entry = |id: &str, path: &std::path::Path, parent: Option<&str>| RegistryEntry {
            project_id: id.to_string(),
            project_path: path.to_string_lossy().to_string(),
            parent_project_id: parent.map(str::to_string),
            subscriptions: Vec::new(),
        };

        let sub = entry("child00000000000", &worktree, Some("parent0000000000"));
        assert!(
            registry_entry_alive(&sub),
            "an existing worktree directory must be alive without its own .engramdb/"
        );

        // Same directory, but registered as a ROOT project: no `.engramdb/`
        // means genuinely stale.
        let root = entry("root000000000000", &worktree, None);
        assert!(
            !registry_entry_alive(&root),
            "a root project with no .engramdb/ is stale"
        );

        // A sub-project whose directory is gone is stale regardless.
        let removed = entry(
            "gone000000000000",
            &temp_dir.path().join("deleted-worktree"),
            Some("parent0000000000"),
        );
        assert!(
            !registry_entry_alive(&removed),
            "a sub-project whose directory no longer exists is stale"
        );
    }
}
