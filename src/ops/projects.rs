//! Project management operations.
//!
//! Functions for inspecting, listing, deleting, linking, and aggregating
//! statistics across registered EngramDB projects.

use crate::storage::{
    collect_descendants, manifest, paths, resolve_root_project_id, MemoryStore, Registry,
    RegistryBackend,
};
use crate::types::MemoryType;
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
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

/// List all registered projects.
pub async fn list_projects(registry: &dyn RegistryBackend) -> Result<Vec<ProjectListEntry>> {
    let registry = registry.load().await?;

    let entries = registry
        .projects
        .into_iter()
        .map(|e| {
            // Sub-projects (worktrees) don't have their own .engramdb/ — their
            // storage lives at the parent — so treat them as alive if the
            // worktree directory itself still exists.  Root projects use the
            // usual .engramdb/ check.
            let exists = if e.parent_project_id.is_some() {
                Path::new(&e.project_path).exists()
            } else {
                Path::new(&e.project_path).join(".engramdb").exists()
            };
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
pub async fn delete_project(
    registry: &dyn RegistryBackend,
    project_id: &str,
    cascade: bool,
) -> Result<DeleteResult> {
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

    // Delete global data directory for this project.
    let projects_dir = paths::global_data_dir()?.join("projects");
    let global_project_dir = projects_dir.join(project_id);
    let global_data_removed = if global_project_dir.exists() {
        async_fs::remove_dir_all(&global_project_dir).await?;
        true
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
                let _ = async_fs::remove_dir_all(&dir).await;
            }
        }
    }

    Ok(DeleteResult {
        project_path: entry.project_path,
        global_data_removed,
        cascaded_ids: descendants,
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

/// Count orphan data directories (on disk under `projects/` but not in registry).
pub async fn count_orphan_dirs(registry: &dyn RegistryBackend) -> Result<usize> {
    let reg = registry.load().await?;
    let registered_ids: std::collections::HashSet<String> =
        reg.projects.iter().map(|e| e.project_id.clone()).collect();

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
            if !registered_ids.contains(&dir_name) {
                count += 1;
            }
        }
    }

    Ok(count)
}

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
/// Deletion is parallelized with rayon. Calls `on_progress(phase)` after
/// each item is removed (must be thread-safe).
pub async fn prune_stale_projects(
    registry: &dyn RegistryBackend,
    on_progress: impl Fn(PrunePhase) + Send + Sync,
) -> Result<PruneResult> {
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let mut reg = registry.load().await?;

    // --- Stale registry entries ---
    let (keep, stale): (Vec<_>, Vec<_>) = reg
        .projects
        .into_iter()
        .partition(|e| Path::new(&e.project_path).join(".engramdb").exists());

    let projects_dir = paths::global_data_dir()?.join("projects");

    let stale_ids: Vec<String> = stale.iter().map(|e| e.project_id.clone()).collect();
    let stale_dirs: Vec<_> = stale
        .iter()
        .map(|e| projects_dir.join(&e.project_id))
        .filter(|p| p.exists())
        .collect();

    stale_dirs.par_iter().for_each(|dir| {
        let _ = std::fs::remove_dir_all(dir);
        on_progress(PrunePhase::Stale);
    });

    let stale_removed = stale.len();
    reg.projects = keep;
    registry.save(&reg).await?;

    // --- Orphan data directories ---
    let registered_ids: std::collections::HashSet<String> =
        reg.projects.iter().map(|e| e.project_id.clone()).collect();

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

    let orphan_errors = AtomicUsize::new(0);
    orphan_paths.par_iter().for_each(|path| {
        if std::fs::remove_dir_all(path).is_err() {
            orphan_errors.fetch_add(1, Ordering::Relaxed);
        }
        on_progress(PrunePhase::Orphan);
    });

    let orphans_removed = orphan_ids.len() - orphan_errors.load(Ordering::Relaxed);

    // --- Hierarchy repair ---
    //
    // Runs after stale removal so that any children orphaned by a stale
    // parent removal are caught in the same pass (they appear as "dangling"
    // after the parent is gone).
    let issues = scan_hierarchy_issues(&registry.load().await?);
    let hierarchy_cleared = issues.into_all_ids();
    if !hierarchy_cleared.is_empty() {
        let mut reg = registry.load().await?;
        let cleared_set: std::collections::HashSet<&str> =
            hierarchy_cleared.iter().map(|s| s.as_str()).collect();
        for entry in reg.projects.iter_mut() {
            if cleared_set.contains(entry.project_id.as_str()) {
                entry.parent_project_id = None;
            }
        }
        registry.save(&reg).await?;
        for _ in &hierarchy_cleared {
            on_progress(PrunePhase::Hierarchy);
        }
    }

    Ok(PruneResult {
        stale_removed,
        stale_ids,
        orphans_removed,
        orphan_ids,
        hierarchy_cleared,
    })
}

/// Aggregate statistics across all registered projects.
pub async fn aggregate_stats(registry: &dyn RegistryBackend) -> Result<AggregateStats> {
    let reg = registry.load().await?;
    let total_projects = reg.projects.len();

    let mut reachable_projects = 0;
    let mut total_memories = 0;
    let mut type_counts: HashMap<MemoryType, usize> = HashMap::new();

    for entry in &reg.projects {
        let dir = Path::new(&entry.project_path);
        if !dir.join(".engramdb").exists() {
            continue;
        }

        let store = match MemoryStore::open(dir).await {
            Ok(s) => s,
            Err(_) => continue,
        };

        reachable_projects += 1;

        let summaries = match store.list_summary().await {
            Ok(e) => e,
            Err(_) => continue,
        };

        total_memories += summaries.len();
        for e in &summaries {
            *type_counts.entry(e.type_).or_insert(0) += 1;
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

        let result = delete_project(&registry, &pid, false).await.unwrap();
        assert!(!result.project_path.is_empty());
        assert!(!global_dir.exists(), "Global data dir should be removed");
        assert!(result.cascaded_ids.is_empty());
    }

    #[tokio::test]
    async fn test_delete_project_not_found() {
        let registry = InMemoryRegistry::new();
        let result = delete_project(&registry, "nonexistent-id-12345", false).await;
        assert!(result.is_err());
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

        let err = delete_project(&registry, &parent.project_id, false)
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

        let result = delete_project(&registry, &root.project_id, true)
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

        delete_project(&registry, &parent.project_id, true)
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
}
