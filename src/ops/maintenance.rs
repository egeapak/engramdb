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
//! it runs at most once per the resolved interval regardless of how many
//! commands or sessions fire, and it is **best-effort**: every failure is logged
//! and swallowed so routine operations are never blocked.
//!
//! Both behaviours are configured via the `[maintenance]` section of
//! `config.toml` ([`crate::types::MaintenanceConfig`]), with override ladders:
//!
//! - **enabled**: `--no-maintenance` CLI flag (`cli_skip`) >
//!   `ENGRAMDB_DISABLE_AUTO_MAINTENANCE` env (truthy: `1`/`true`/`yes`/`on`) >
//!   `config.enabled`.
//! - **interval**: `ENGRAMDB_AUTO_MAINTENANCE_INTERVAL_SECS` env >
//!   `config.interval_secs`.

use crate::ops::doctor::{doctor, DoctorResult};
use crate::ops::projects::{prune_stale_projects, PruneResult};
use crate::storage::{paths, MemoryStore, RegistryBackend};
use crate::types::MaintenanceConfig;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

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
    /// Result of LanceDB compaction/version pruning, if it ran and succeeded.
    pub optimize: Option<crate::storage::IndexOptimizeStats>,
    /// §11.3 promotion pass result, if it ran and succeeded.
    pub promotion: Option<crate::ops::task::PromotionReport>,
    /// §11.4 consolidation pass result, if an engine was supplied and the
    /// pass ran (it skips gracefully with no providers).
    pub consolidation: Option<crate::ops::compress::ConsolidationReport>,
}

/// Effective auto-maintenance status, for diagnostics (`engramdb doctor`).
///
/// Reflects the same override ladders [`auto_maintain`] applies — env over
/// config — so the report matches what an actual command-path pass would do
/// (with `cli_skip = false`, since `--no-maintenance` scopes to one invocation,
/// not the configured state). Best-effort: an unreadable marker reads as "never
/// run".
#[derive(Debug, Clone)]
pub struct MaintenanceStatus {
    /// Whether auto-maintenance is currently enabled (after the override ladder).
    pub enabled: bool,
    /// The resolved throttle window between passes.
    pub interval: Duration,
    /// When the last pass ran (from the marker), if it has ever run on this
    /// machine.
    pub last_run: Option<SystemTime>,
}

/// Resolve the effective [`MaintenanceStatus`] for the given config.
pub async fn maintenance_status(config: &MaintenanceConfig) -> MaintenanceStatus {
    let last_run = match marker_path() {
        Some(path) => tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|m| m.modified().ok()),
        None => None,
    };
    MaintenanceStatus {
        enabled: resolve_enabled(config, false),
        interval: resolve_interval(config),
        last_run,
    }
}

/// Whether maintenance is disabled via the `ENGRAMDB_DISABLE_AUTO_MAINTENANCE`
/// environment variable (truthy: `1`/`true`/`yes`/`on`).
fn env_disabled() -> bool {
    std::env::var("ENGRAMDB_DISABLE_AUTO_MAINTENANCE")
        .ok()
        .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// The throttle window override from the environment, if set and parseable.
fn env_interval_override() -> Option<Duration> {
    std::env::var("ENGRAMDB_AUTO_MAINTENANCE_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Resolve whether the maintenance pass should run, applying the override
/// ladder: CLI flag > env var > config.
fn resolve_enabled(config: &MaintenanceConfig, cli_skip: bool) -> bool {
    if cli_skip || env_disabled() {
        return false;
    }
    config.enabled
}

/// Resolve the throttle window: env override wins over the config value.
fn resolve_interval(config: &MaintenanceConfig) -> Duration {
    env_interval_override().unwrap_or_else(|| Duration::from_secs(config.interval_secs))
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
/// `config` is the project's `[maintenance]` section and `cli_skip` is the
/// `--no-maintenance` flag (always `false` for the MCP server, which has no
/// such flag). Callers must only invoke this when operating on the **main**
/// worktree — a linked worktree should just link/consolidate (handled
/// elsewhere) and skip this housekeeping. Throttled and best-effort: never
/// returns an error and never panics, so it is safe to call on the hot path of
/// any command.
pub async fn auto_maintain(
    dir: &Path,
    registry: &dyn RegistryBackend,
    config: &MaintenanceConfig,
    cli_skip: bool,
) -> MaintenanceReport {
    auto_maintain_with_engine(dir, registry, config, cli_skip, None).await
}

/// [`auto_maintain`] with an optional engine for the provider-dependent
/// lifecycle jobs (§11.4 consolidation). Without one, consolidation is
/// skipped (graceful-skip contract); promotion runs regardless (it needs
/// only the telemetry log and the store).
pub async fn auto_maintain_with_engine(
    dir: &Path,
    registry: &dyn RegistryBackend,
    config: &MaintenanceConfig,
    cli_skip: bool,
    engine: Option<&crate::retrieval::engine::RetrievalEngine>,
) -> MaintenanceReport {
    if !resolve_enabled(config, cli_skip) {
        return MaintenanceReport::default();
    }
    if !maintenance_due(resolve_interval(config)).await {
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
            Ok(store) => {
                match doctor(&store).await {
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
                }

                // 3) Compact fragments and prune old dataset versions. Every
                // mutation commits a new immutable LanceDB version, so a
                // create/update-heavy workload that never runs `gc`/`reindex`
                // (the only other optimize() callers) grows disk monotonically.
                // This pass is already throttled and best-effort, which is
                // exactly the contract optimize() wants.
                match store.optimize().await {
                    Ok(stats) => {
                        if stats.old_versions_removed > 0 || stats.fragments_removed > 0 {
                            tracing::info!(
                                "engramdb auto-maintenance: compacted {} fragment(s), pruned {} old version(s)",
                                stats.fragments_removed,
                                stats.old_versions_removed
                            );
                        }
                        report.optimize = Some(stats);
                    }
                    Err(e) => {
                        tracing::warn!("engramdb auto-maintenance: index optimize failed: {e}")
                    }
                }
            }
            Err(e) => tracing::warn!(
                "engramdb auto-maintenance: could not open store at {}: {e}",
                dir.display()
            ),
        }

        // 4) Lifecycle jobs (§11.5: all lifecycle work rides the throttled
        // maintenance pass). Promotion needs only telemetry + store;
        // consolidation additionally needs providers and runs only when the
        // caller supplied an engine.
        if let Ok(store) = MemoryStore::open(dir).await {
            let full_config = crate::storage::config::load_config_or_default(
                &dir.join(".engramdb").join("config.toml"),
            )
            .await;
            match crate::ops::task::promote_reconfirmed_memories(&store, &full_config).await {
                Ok(promo) => {
                    for (id, summary, sessions) in &promo.suggestions {
                        tracing::info!(
                            "engramdb auto-maintenance: memory {id} ('{summary}') was retrieved                              in {sessions} later sessions — promote to project-wide?                              (set [epistemic] auto_promote = true to automate)"
                        );
                    }
                    if !promo.promoted.is_empty() {
                        tracing::info!(
                            "engramdb auto-maintenance: auto-promoted {} memor(ies) to                              project-wide",
                            promo.promoted.len()
                        );
                    }
                    report.promotion = Some(promo);
                }
                Err(e) => tracing::warn!("engramdb auto-maintenance: promotion pass failed: {e}"),
            }

            if let Some(engine) = engine {
                let apply = full_config.epistemic.auto_consolidate;
                match crate::ops::compress::consolidation_pass(&store, engine, &full_config, apply)
                    .await
                {
                    Ok(cons) => {
                        for cluster in &cons.clusters {
                            tracing::info!(
                                "engramdb auto-maintenance: {} similar observations could                                  consolidate into a fact: {:?}",
                                cluster.source_ids.len(),
                                cluster.summaries
                            );
                        }
                        report.consolidation = Some(cons);
                    }
                    Err(e) => {
                        tracing::warn!("engramdb auto-maintenance: consolidation failed: {e}")
                    }
                }
            }
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

    /// A config with `interval_secs` so a pass is always due — lets most tests
    /// drive the throttle entirely through config, never touching the
    /// process-global env vars (only the two env-precedence tests below do).
    fn cfg(interval_secs: u64) -> MaintenanceConfig {
        MaintenanceConfig {
            enabled: true,
            interval_secs,
        }
    }

    #[tokio::test]
    async fn auto_maintain_runs_prune_and_doctor_on_healthy_store() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(dir.path(), &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();

        let report = auto_maintain(dir.path(), &registry, &cfg(0), false).await;
        assert!(report.ran, "a due pass must run");
        assert!(report.prune.is_some(), "cleanup must have run");
        let doctor = report.doctor.expect("doctor must have run");
        assert!(doctor.healthy, "freshly-created store must be healthy");
    }

    #[tokio::test]
    async fn auto_maintain_skips_doctor_for_uninitialized_store() {
        // No `.engramdb/` at `dir` → the store can't be opened, so the health
        // check is skipped, but cleanup still runs and nothing errors.
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        let report = auto_maintain(dir.path(), &registry, &cfg(0), false).await;
        assert!(report.ran, "maintenance runs even without a store");
        assert!(report.prune.is_some(), "cleanup always runs");
        assert!(
            report.doctor.is_none(),
            "doctor must be skipped for an uninitialized store"
        );
    }

    #[tokio::test]
    async fn auto_maintain_reports_unhealthy_store_without_erroring() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(dir.path(), &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        let id = store.create(&mem).await.unwrap();

        // Delete the file behind the store's back → a stale index entry, so the
        // quick doctor must flag the store unhealthy (but auto_maintain must
        // still complete without erroring).
        let file = dir
            .path()
            .join(".engramdb")
            .join("memories")
            .join(format!("{id}.md"));
        tokio::fs::remove_file(&file).await.unwrap();

        let report = auto_maintain(dir.path(), &registry, &cfg(0), false).await;
        assert!(report.ran);
        let doctor = report.doctor.expect("doctor must have run");
        assert!(
            !doctor.healthy,
            "stale index entry must be reported unhealthy"
        );
    }

    #[tokio::test]
    async fn auto_maintain_disabled_by_cli_flag() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(dir.path(), &registry).await.unwrap();

        // cli_skip=true (the --no-maintenance flag) forces it off even though
        // config is enabled and the pass would otherwise be due.
        let report = auto_maintain(dir.path(), &registry, &cfg(0), true).await;
        assert!(!report.ran, "--no-maintenance must skip the pass");
    }

    #[tokio::test]
    async fn auto_maintain_disabled_by_config() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(dir.path(), &registry).await.unwrap();

        let disabled = MaintenanceConfig {
            enabled: false,
            interval_secs: 0,
        };
        let report = auto_maintain(dir.path(), &registry, &disabled, false).await;
        assert!(!report.ran, "config.enabled=false must skip the pass");
    }

    #[tokio::test]
    async fn auto_maintain_throttled_until_interval_elapses() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(dir.path(), &registry).await.unwrap();

        // First pass: interval 0 → due → runs and writes the marker.
        let first = auto_maintain(dir.path(), &registry, &cfg(0), false).await;
        assert!(first.ran);

        // Second pass: a huge interval → throttled (marker is fresh).
        let second = auto_maintain(dir.path(), &registry, &cfg(100_000), false).await;
        assert!(!second.ran, "a fresh marker must throttle the next pass");
    }

    // --- Env-precedence tests ---
    //
    // These mutate process-global env vars. They are isolated by nextest's
    // process-per-test model (each test is its own process; the `#[ctor]` arm
    // also redirects the data dir per-process), so no save/restore is needed —
    // that is exactly why this project mandates nextest over `cargo test`.

    #[tokio::test]
    async fn auto_maintain_disabled_by_env_over_enabled_config() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(dir.path(), &registry).await.unwrap();

        std::env::set_var("ENGRAMDB_DISABLE_AUTO_MAINTENANCE", "1");
        // config is enabled, but the env var must win and disable it.
        let report = auto_maintain(dir.path(), &registry, &cfg(0), false).await;
        assert!(!report.ran, "env disable must override an enabled config");
    }

    #[tokio::test]
    async fn auto_maintain_env_interval_overrides_config() {
        let dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(dir.path(), &registry).await.unwrap();

        // config interval is huge (would never be due), but the env override of
        // 0 makes the pass due → it runs.
        std::env::set_var("ENGRAMDB_AUTO_MAINTENANCE_INTERVAL_SECS", "0");
        let report = auto_maintain(dir.path(), &registry, &cfg(100_000), false).await;
        assert!(report.ran, "env interval override must win over config");
    }
}
