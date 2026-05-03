//! Aggregate snapshot persistence for the runtime stats collector.
//!
//! Each project's counters live as a JSON file at
//! `<global_data_dir>/projects/{project_id}/stats.json`. The persistence
//! layer is **aggregate-only** — we don't keep an event log. The file is
//! rewritten via tempfile + atomic rename on every flush.
//!
//! On server startup we load every project's snapshot back into the
//! `StatsCollector`. Counters and ring buffers survive restarts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::storage::paths::{global_data_dir, GLOBAL_PROJECT_ID};
use crate::telemetry::collector::{ProjectStats, StatsCollector};

/// On-disk representation of one project's snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSnapshot {
    /// Schema version. Bump when the on-disk shape changes incompatibly.
    pub version: u32,
    /// When the snapshot was last written.
    pub written_at: DateTime<Utc>,
    /// When the *first* counter on this project was recorded. Reported as
    /// `since` in the runtime stats payload so consumers see a stable window.
    pub since: DateTime<Utc>,
    pub stats: ProjectStats,
}

const SCHEMA_VERSION: u32 = 1;

/// Returns the path of `stats.json` for the given project ID.
///
/// Project layout mirrors `lancedb_dir` / `personal_dir` (`paths.rs`):
///   `<global_data_dir>/projects/{id}/stats.json`
/// or, for the global store:
///   `<global_data_dir>/global/stats.json`
pub fn snapshot_path(project_id: &str) -> Result<PathBuf> {
    let root = global_data_dir().context("resolving global data dir")?;
    let dir = if project_id == GLOBAL_PROJECT_ID {
        root.join("global")
    } else {
        root.join("projects").join(project_id)
    };
    Ok(dir.join("stats.json"))
}

/// Where every project's stats files live.
fn projects_root() -> Result<PathBuf> {
    Ok(global_data_dir()
        .context("resolving global data dir")?
        .join("projects"))
}

/// Atomically write the snapshot for a single project.
pub async fn write_snapshot(project_id: &str, snap: &PersistedSnapshot) -> Result<()> {
    let path = snapshot_path(project_id)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating dir {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(snap).context("serializing snapshot")?;
    write_atomic(&path, &bytes).await
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("stats path has no parent: {}", path.display()))?;
    let tmp = tempfile::NamedTempFile::new_in(parent).context("creating tempfile")?;
    let tmp_path = tmp.path().to_path_buf();
    tokio::fs::write(&tmp_path, bytes)
        .await
        .with_context(|| format!("writing tempfile {}", tmp_path.display()))?;
    tmp.persist(path)
        .map_err(|e| anyhow::anyhow!("atomic rename failed: {e}"))?;
    Ok(())
}

/// Read one project's snapshot, or `Ok(None)` if it doesn't exist or is
/// older than the configured retention window.
pub async fn read_snapshot(
    project_id: &str,
    retention_days: Option<u64>,
) -> Result<Option<PersistedSnapshot>> {
    let path = snapshot_path(project_id)?;
    if !tokio::fs::try_exists(&path).await.unwrap_or(false) {
        return Ok(None);
    }
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    let snap: PersistedSnapshot = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                "stats snapshot at {} is corrupt ({e}); ignoring",
                path.display()
            );
            return Ok(None);
        }
    };
    if snap.version != SCHEMA_VERSION {
        tracing::warn!(
            "stats snapshot at {} has schema version {} (expected {}); ignoring",
            path.display(),
            snap.version,
            SCHEMA_VERSION
        );
        return Ok(None);
    }
    if let Some(days) = retention_days {
        let age = Utc::now().signed_duration_since(snap.written_at);
        if age.num_days() > days as i64 {
            tracing::info!(
                "stats snapshot at {} is older than retention ({} days); ignoring",
                path.display(),
                days
            );
            return Ok(None);
        }
    }
    Ok(Some(snap))
}

/// Load every project's persisted snapshot from disk and hydrate the
/// collector. Snapshots with a version mismatch or corrupt JSON are skipped
/// with a `warn!` log line but never abort startup.
pub async fn hydrate_collector(collector: &Arc<StatsCollector>) -> Result<()> {
    let retention = collector.config().retention_days;
    let mut by_project: HashMap<String, ProjectStats> = HashMap::new();
    let mut earliest_since: Option<DateTime<Utc>> = None;

    // Iterate <global_data_dir>/projects/*/stats.json
    let root = match projects_root() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("stats: cannot resolve projects root: {e}");
            return Ok(());
        }
    };
    if tokio::fs::try_exists(&root).await.unwrap_or(false) {
        let mut rd = tokio::fs::read_dir(&root)
            .await
            .with_context(|| format!("reading {}", root.display()))?;
        while let Some(entry) = rd.next_entry().await.transpose() {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("stats hydrate: dir entry error: {e}");
                    continue;
                }
            };
            let pid = entry.file_name().to_string_lossy().to_string();
            if let Ok(Some(snap)) = read_snapshot(&pid, retention).await {
                earliest_since = Some(match earliest_since {
                    Some(s) if s < snap.since => s,
                    _ => snap.since,
                });
                by_project.insert(pid, snap.stats);
            }
        }
    }

    // Also hydrate the global store, which lives at a different path.
    if let Ok(Some(snap)) = read_snapshot(GLOBAL_PROJECT_ID, retention).await {
        earliest_since = Some(match earliest_since {
            Some(s) if s < snap.since => s,
            _ => snap.since,
        });
        by_project.insert(GLOBAL_PROJECT_ID.to_string(), snap.stats);
    }

    if !by_project.is_empty() {
        let since = earliest_since.unwrap_or_else(Utc::now);
        collector.restore_from(since, by_project);
    }
    Ok(())
}

/// Spawn a background task that periodically flushes the collector's
/// per-project snapshots to disk.
///
/// Cancellation: the task exits cleanly when the collector is dropped (the
/// `Weak` upgrade returns `None`).
pub fn spawn_flush_task(collector: Arc<StatsCollector>) -> tokio::task::JoinHandle<()> {
    let interval_secs = collector.config().flush_interval_secs;
    let weak = Arc::downgrade(&collector);
    drop(collector);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        // First tick fires immediately; skip it so we don't double-write at
        // startup right after `hydrate_collector` ran.
        tick.tick().await;
        loop {
            tick.tick().await;
            let Some(c) = weak.upgrade() else {
                return; // collector dropped → server shut down
            };
            if !c.enabled() {
                continue;
            }
            flush_once(&c).await;
        }
    })
}

/// Write each project's current snapshot to disk. Errors are logged but
/// never propagated — telemetry must never break a tool call.
pub async fn flush_once(collector: &Arc<StatsCollector>) {
    let projects = collector.snapshot_for_persistence();
    let since = collector.since();
    let written_at = Utc::now();
    for (pid, stats) in projects {
        let snap = PersistedSnapshot {
            version: SCHEMA_VERSION,
            written_at,
            since,
            stats,
        };
        if let Err(e) = write_snapshot(&pid, &snap).await {
            tracing::warn!("stats flush for project {pid} failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::collector::StatsCollector;
    use crate::types::config::StatsConfig;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Per-test unique project ID. Tests share the process-global
    /// `ENGRAMDB_DATA_DIR` (set by `test_isolation`) so we avoid any env-var
    /// mutation here and instead key on a unique ID per test invocation.
    fn unique_pid(prefix: &str) -> String {
        static N: AtomicU64 = AtomicU64::new(0);
        format!("test-{}-{}", prefix, N.fetch_add(1, Ordering::Relaxed))
    }

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let pid = unique_pid("rt");
        let collector = StatsCollector::new(StatsConfig::default());
        collector.record_tool_call(&pid, "query", 12.5, true);
        collector.record_query_outcome(&pid, true, "full");
        flush_once(&collector).await;

        let read = read_snapshot(&pid, None).await.unwrap().unwrap();
        assert_eq!(read.version, SCHEMA_VERSION);
        assert_eq!(read.stats.total_calls, 1);
        assert_eq!(read.stats.queries.total, 1);
        assert_eq!(read.stats.queries.hits, 1);
    }

    #[tokio::test]
    async fn hydrate_round_trip() {
        let a = unique_pid("hyd-a");
        let b = unique_pid("hyd-b");
        // Write
        {
            let c = StatsCollector::new(StatsConfig::default());
            c.record_tool_call(&a, "query", 10.0, true);
            c.record_tool_call(&a, "query", 20.0, true);
            c.record_tool_call(&b, "create", 30.0, false);
            flush_once(&c).await;
        }
        // Read back into a fresh collector
        let fresh = StatsCollector::new(StatsConfig::default());
        hydrate_collector(&fresh).await.unwrap();

        let snap_a = fresh.snapshot(&a, false);
        assert_eq!(
            snap_a.view.usage.total_calls, 2,
            "proj-A total_calls restored"
        );
        assert_eq!(
            snap_a.view.usage.by_tool.get("query").copied(),
            Some(2),
            "proj-A query count restored"
        );
        let snap_b = fresh.snapshot(&b, false);
        assert_eq!(snap_b.view.usage.total_calls, 1);
        assert_eq!(
            snap_b.view.usage.errors_by_tool.get("create").copied(),
            Some(1)
        );
    }

    #[tokio::test]
    async fn read_snapshot_returns_none_for_missing_file() {
        let pid = unique_pid("miss");
        let read = read_snapshot(&pid, None).await.unwrap();
        assert!(read.is_none());
    }

    #[tokio::test]
    async fn read_snapshot_drops_stale_when_retention_set() {
        let pid = unique_pid("stale");
        let stale_snap = PersistedSnapshot {
            version: SCHEMA_VERSION,
            written_at: Utc::now() - chrono::Duration::days(40),
            since: Utc::now() - chrono::Duration::days(40),
            stats: ProjectStats::default(),
        };
        write_snapshot(&pid, &stale_snap).await.unwrap();
        let read = read_snapshot(&pid, Some(30)).await.unwrap();
        assert!(read.is_none(), "stale snapshot should be dropped");
        let read = read_snapshot(&pid, None).await.unwrap();
        assert!(read.is_some(), "without retention, even old snapshot loads");
    }
}
