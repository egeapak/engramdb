//! Automatic, best-effort housekeeping for the main worktree.
//!
//! Both front-ends route memory operations to a project's *main* worktree (see
//! [`crate::storage::worktree`]). When an operation is invoked **directly on the
//! main worktree** — not from a linked worktree — this module runs two cheap,
//! self-healing maintenance passes:
//!
//! 1. **Orphan cleanup** — [`crate::ops::projects::prune_stale_projects`] drops
//!    registry entries whose project is gone, deletes orphan global data dirs,
//!    and repairs broken parent links.
//! 2. **Quick health check** — [`crate::ops::doctor::doctor`] compares the main
//!    project's LanceDB index against the memory files on disk and warns if they
//!    have drifted (the user is told to run `engramdb reindex`).
//!
//! The pass is **throttled** via a timestamp marker under the global data dir so
//! it runs at most once per [`maintenance_interval`] regardless of how many
//! commands or sessions fire, and it is **best-effort**: every failure is logged
//! and swallowed so routine operations are never blocked. Both behaviours can be
//! tuned with environment variables:
//!
//! - `ENGRAMDB_DISABLE_AUTO_MAINTENANCE` (truthy: `1`/`true`/`yes`/`on`) — skip
//!   entirely.
//! - `ENGRAMDB_AUTO_MAINTENANCE_INTERVAL_SECS` — override the throttle window
//!   (used by tests to force or suppress a run).

use crate::ops::doctor::{doctor, DoctorResult};
use crate::ops::projects::{prune_stale_projects, PruneResult};
use crate::storage::{paths, MemoryStore, RegistryBackend};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default minimum interval between automatic maintenance passes (6 hours).
const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Throttle marker file, stored under the global data dir.
const MARKER_FILE: &str = ".last_maintenance";

/// Outcome of an [`auto_maintain`] call.
#[derive(Debug, Default)]
pub struct MaintenanceReport {
    /// Whether the maintenance pass actually ran (false when disabled or
    /// throttled).
    pub ran: bool,
    /// Result of the orphan/stale project cleanup, if it ran and succeeded.
    pub prune: Option<PruneResult>,
    /// Result of the main store's health check, if it ran and succeeded.
    pub doctor: Option<DoctorResult>,
}

/// Whether automatic maintenance has been disabled via the environment.
fn disabled() -> bool {
    std::env::var("ENGRAMDB_DISABLE_AUTO_MAINTENANCE")
        .ok()
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// The throttle window, honouring the env override.
fn maintenance_interval() -> Duration {
    std::env::var("ENGRAMDB_AUTO_MAINTENANCE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_MAINTENANCE_INTERVAL)
}

/// Path to the throttle marker, or `None` when the global data dir can't be
/// resolved (in which case maintenance is skipped rather than risking churn).
fn marker_path() -> Option<PathBuf> {
    paths::global_data_dir().ok().map(|d| d.join(MARKER_FILE))
}

/// Whether enough time has elapsed since the last pass to run again.
async fn maintenance_due(interval: Duration) -> bool {
    let Some(path) = marker_path() else {
        return false;
    };
    match tokio::fs::metadata(&path).await {
        Ok(meta) => match meta.modified() {
            // A clock that ran backwards (`elapsed()` Err) shouldn't wedge
            // maintenance off forever — treat it as due.
            Ok(modified) => modified.elapsed().map(|e| e >= interval).unwrap_or(true),
            Err(_) => true,
        },
        // No marker yet (first run on this machine) → due.
        Err(_) => true,
    }
}

/// Stamp the marker with the current time. Best-effort.
async fn record_maintenance() {
    let Some(path) = marker_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(&path, chrono::Utc::now().to_rfc3339()).await;
}

/// Run automatic maintenance for `dir` (the resolved main project root).
///
/// Callers must only invoke this when operating on the **main** worktree — a
/// linked worktree should just link/consolidate (handled elsewhere) and skip
/// this housekeeping. Throttled and best-effort: never returns an error and
/// never panics, so it is safe to call on the hot path of any command.
pub async fn auto_maintain(dir: &Path, registry: &dyn RegistryBackend) -> MaintenanceReport {
    if disabled() {
        return MaintenanceReport::default();
    }
    if !maintenance_due(maintenance_interval()).await {
        return MaintenanceReport::default();
    }
    // Stamp the marker up-front so a failure can't spin into a hot retry loop
    // and concurrent invocations don't all pile on the same pass.
    record_maintenance().await;

    let mut report = MaintenanceReport {
        ran: true,
        ..Default::default()
    };

    // 1) Clean up orphan/stale projects and repair broken hierarchy links.
    match prune_stale_projects(registry, |_| {}).await {
        Ok(result) => {
            if result.stale_removed > 0
                || result.orphans_removed > 0
                || !result.hierarchy_cleared.is_empty()
            {
                tracing::info!(
                    "engramdb auto-maintenance: removed {} stale project(s), {} orphan data dir(s), cleared {} broken parent link(s)",
                    result.stale_removed,
                    result.orphans_removed,
                    result.hierarchy_cleared.len()
                );
            }
            report.prune = Some(result);
        }
        Err(e) => tracing::warn!("engramdb auto-maintenance: project cleanup failed: {e}"),
    }

    // 2) Quick health check of the main project's store (skip if not yet init'd).
    if paths::project_dir(dir).exists() {
        match MemoryStore::open(dir).await {
            Ok(store) => match doctor(&store).await {
                Ok(result) => {
                    if !result.healthy {
                        tracing::warn!(
                            "engramdb auto-maintenance: store at {} is unhealthy ({} stale index entr(ies), {} orphaned file(s)) — run `engramdb reindex` to repair",
                            dir.display(),
                            result.stale_entries.len(),
                            result.orphaned_files.len()
                        );
                    }
                    report.doctor = Some(result);
                }
                Err(e) => {
                    tracing::warn!("engramdb auto-maintenance: store health check failed: {e}")
                }
            },
            Err(e) => tracing::warn!(
                "engramdb auto-maintenance: could not open store at {}: {e}",
                dir.display()
            ),
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    // Env mutation is process-global, but nextest runs each test in its own
    // process and the `#[ctor]` arm redirects the data dir per-process, so
    // setting these vars directly here can't bleed into another test.
    fn set_interval(secs: &str) {
        std::env::set_var("ENGRAMDB_AUTO_MAINTENANCE_INTERVAL_SECS", secs);
    }

    #[tokio::test]
    async fn auto_maintain_runs_prune_and_doctor_on_healthy_store() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(dir.path(), &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();

        set_interval("0"); // always due
        let report = auto_maintain(dir.path(), &registry).await;
        assert!(report.ran, "a due pass must run");
        assert!(report.prune.is_some(), "cleanup must have run");
        let doctor = report.doctor.expect("doctor must have run");
        assert!(doctor.healthy, "freshly-created store must be healthy");
    }

    #[tokio::test]
    async fn auto_maintain_disabled_by_env() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(dir.path(), &registry).await.unwrap();

        set_interval("0");
        std::env::set_var("ENGRAMDB_DISABLE_AUTO_MAINTENANCE", "1");
        let report = auto_maintain(dir.path(), &registry).await;
        std::env::remove_var("ENGRAMDB_DISABLE_AUTO_MAINTENANCE");
        assert!(!report.ran, "disabled maintenance must be a no-op");
    }

    #[tokio::test]
    async fn auto_maintain_throttled_until_interval_elapses() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(dir.path(), &registry).await.unwrap();

        // First pass with interval 0 → runs and writes the marker.
        set_interval("0");
        let first = auto_maintain(dir.path(), &registry).await;
        assert!(first.ran);

        // Second pass with a huge interval → throttled (marker is fresh).
        set_interval("100000");
        let second = auto_maintain(dir.path(), &registry).await;
        assert!(!second.ran, "a fresh marker must throttle the next pass");
    }
}
