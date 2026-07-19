//! Garbage collection operation.
//!
//! GC runs in two phases so the destructive part can be re-validated:
//!
//! 1. **Planning** ([`plan_gc`]) — lock-free snapshot: score every memory and
//!    collect those below the threshold as candidates.
//! 2. **Execution** ([`execute_gc_plan`]) — for each candidate, re-read and
//!    re-score the memory *under the per-project write lock* (via
//!    [`MemoryStore::delete_if`]) immediately before deleting. A candidate
//!    that was concurrently updated (criticality raised, challenge resolved —
//!    detected via `updated_at`), no longer scores below the threshold, or
//!    was concurrently deleted is **skipped**, never deleted on the stale
//!    score and never an error.
//!
//! [`gc_memories`] is the one-shot wrapper both CLI and MCP call.

use crate::scoring::{composite_score, ScoringContext};
use crate::storage::MemoryStore;
use crate::types::EngramConfig;
use anyhow::Result;
use chrono::{DateTime, Utc};

/// Why a memory was planned for deletion (drives the execution-time
/// re-validation predicate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcReason {
    /// Composite score below the GC threshold (the pre-epistemic rule).
    LowScore,
    /// Validity window closed longer ago than
    /// `[epistemic] invalidated_retention_days` (§2.4 retention).
    InvalidatedRetention,
}

/// A GC deletion candidate identified during the planning phase.
#[derive(Debug, Clone)]
pub struct GcCandidate {
    pub id: String,
    /// `updated_at` observed at scoring time. Execution skips the deletion
    /// if the memory has been modified since (the score is stale).
    pub updated_at: DateTime<Utc>,
    pub reason: GcReason,
}

/// The lock-free planning snapshot: what GC *would* delete.
#[derive(Debug)]
pub struct GcPlan {
    pub candidates: Vec<GcCandidate>,
    /// IDs found in index but missing from data store. Suggests reindex is needed.
    pub stale_entries: Vec<String>,
    /// The effective threshold the candidates were scored against.
    pub threshold: f64,
}

/// Disk-maintenance figures from the post-deletion optimize pass.
///
/// LanceDB is append-only: every create/update/delete (including the GC
/// deletions themselves) commits a new immutable dataset version, so GC
/// doubles as the maintenance entry point that compacts fragments and prunes
/// old versions. Version pruning uses the lancedb default retention (7 days),
/// which is safe for concurrent MVCC readers.
#[derive(Debug, Clone, Copy, Default)]
pub struct GcMaintenance {
    /// Bytes freed by pruning old index versions (memories + chunks tables).
    pub bytes_removed: u64,
    /// Old index versions pruned.
    pub old_versions_removed: u64,
    /// Fragments merged away by compaction.
    pub fragments_removed: usize,
}

/// Result of a GC operation.
pub struct GcResult {
    /// IDs actually deleted (or, on a dry run, the planned candidates).
    pub removed: Vec<String>,
    pub count: usize,
    /// Candidates that were NOT deleted at execution time: concurrently
    /// deleted, modified since scoring, or re-scored at/above the threshold
    /// under the lock. Always empty on a dry run.
    pub skipped: Vec<String>,
    /// IDs found in index but missing from data store. Suggests reindex is needed.
    pub stale_entries: Vec<String>,
    /// Index optimize stats from the maintenance pass that runs after the
    /// deletion phase. `None` on a dry run (a preview must not mutate the
    /// store, and optimize commits new versions) and when the optimize pass
    /// failed (non-fatal — the deletions above still succeeded).
    pub maintenance: Option<GcMaintenance>,
}

/// Phase 1: score all memories (lock-free) and collect deletion candidates.
pub async fn plan_gc(
    store: &MemoryStore,
    config: &EngramConfig,
    threshold: Option<f64>,
) -> Result<GcPlan> {
    let threshold = threshold.unwrap_or(config.thresholds.gc);
    let ids = store.list_ids().await?;
    let now = Utc::now();

    // Single batched load (one dir scan) instead of a per-ID `get` (one dir
    // scan each). IDs the batch could not load are stale index entries.
    let loaded = store.get_batch(&ids).await?;
    let loaded_ids: std::collections::HashSet<&str> =
        loaded.iter().map(|(id, _)| id.as_str()).collect();
    let stale_entries: Vec<String> = ids
        .iter()
        .filter(|id| !loaded_ids.contains(id.as_str()))
        .cloned()
        .collect();

    // §2.4 retention: memories whose validity window closed longer ago than
    // `[epistemic] invalidated_retention_days` are purged regardless of
    // score. `0` = keep forever.
    let retention_days = config.epistemic.invalidated_retention_days;
    let retention_cutoff =
        (retention_days > 0).then(|| now - chrono::Duration::days(retention_days as i64));

    let mut candidates = Vec::new();
    for (_, memory) in &loaded {
        if let Some(cutoff) = retention_cutoff {
            if memory.invalidated_at.is_some_and(|t| t < cutoff) {
                candidates.push(GcCandidate {
                    id: memory.id.clone(),
                    updated_at: memory.updated_at,
                    reason: GcReason::InvalidatedRetention,
                });
                continue; // one candidacy per memory; retention wins
            }
        }

        let context = ScoringContext::scope_only(None, &[]);
        let breakdown = composite_score(memory, &context, config, now);

        if breakdown.final_score < threshold {
            candidates.push(GcCandidate {
                id: memory.id.clone(),
                updated_at: memory.updated_at,
                reason: GcReason::LowScore,
            });
        }
    }

    Ok(GcPlan {
        candidates,
        stale_entries,
        threshold,
    })
}

/// Phase 2: delete the planned candidates, re-validating each one under the
/// per-project write lock immediately before deletion.
///
/// A candidate is deleted only if, on a fresh read inside the lock, it is
/// still unmodified since scoring (`updated_at` unchanged) **and** still
/// scores below the plan's threshold. Anything else — including a memory
/// that vanished concurrently — is reported in `skipped`.
pub async fn execute_gc_plan(
    store: &MemoryStore,
    config: &EngramConfig,
    plan: GcPlan,
) -> Result<GcResult> {
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    let now = Utc::now();

    // Recompute the retention cutoff for execution-time re-validation.
    let retention_days = config.epistemic.invalidated_retention_days;
    let retention_cutoff =
        (retention_days > 0).then(|| now - chrono::Duration::days(retention_days as i64));

    for candidate in &plan.candidates {
        let context = ScoringContext::scope_only(None, &[]);
        let deleted = store
            .delete_if(&candidate.id, |memory| {
                if memory.updated_at != candidate.updated_at {
                    return false; // modified since planning — stale decision
                }
                match candidate.reason {
                    GcReason::LowScore => {
                        composite_score(memory, &context, config, now).final_score < plan.threshold
                    }
                    // Still invalidated and still past retention (a §2.4
                    // reopening would have bumped updated_at, but re-check
                    // anyway — deletion is irreversible).
                    GcReason::InvalidatedRetention => retention_cutoff
                        .is_some_and(|cutoff| memory.invalidated_at.is_some_and(|t| t < cutoff)),
                }
            })
            .await?;
        if deleted {
            removed.push(candidate.id.clone());
        } else {
            skipped.push(candidate.id.clone());
        }
    }

    let count = removed.len();
    Ok(GcResult {
        removed,
        count,
        skipped,
        stale_entries: plan.stale_entries,
        maintenance: None,
    })
}

/// Post-deletion maintenance: compact + version-prune the LanceDB index and
/// prune/compact this project's telemetry event log.
///
/// Every step is non-fatal — GC's deletions have already succeeded, and
/// maintenance is space reclamation, not correctness. Failures are logged at
/// warn level and reported as `None`.
async fn run_maintenance(store: &MemoryStore, config: &EngramConfig) -> Option<GcMaintenance> {
    // Telemetry event log for this project: prune old rows (when a retention
    // window is configured; the default is 90 days), then compact fragments
    // and prune old table versions. Both are cheap no-ops when the
    // `stats_events` table doesn't exist.
    if let Some(days) = config.stats.retention_days {
        if let Err(e) =
            crate::telemetry::persistence::prune_older_than(&store.project_id, days).await
        {
            tracing::warn!("gc: telemetry prune failed (non-fatal): {e}");
        }
    }
    if let Err(e) = crate::telemetry::persistence::optimize_table(&store.project_id).await {
        tracing::warn!("gc: telemetry optimize failed (non-fatal): {e}");
    }

    // Memories + chunks tables: compaction plus version pruning is where
    // monotonic disk growth from create/update/delete is actually reclaimed.
    match store.optimize().await {
        Ok(stats) => {
            if stats.bytes_removed > 0 || stats.fragments_removed > 0 {
                tracing::info!(
                    "gc: index optimize reclaimed {} bytes ({} old versions, {} fragments compacted)",
                    stats.bytes_removed,
                    stats.old_versions_removed,
                    stats.fragments_removed
                );
            }
            Some(GcMaintenance {
                bytes_removed: stats.bytes_removed,
                old_versions_removed: stats.old_versions_removed,
                fragments_removed: stats.fragments_removed,
            })
        }
        Err(e) => {
            tracing::warn!("gc: index optimize failed (non-fatal): {e}");
            None
        }
    }
}

/// Run garbage collection on memories below threshold.
///
/// Identifies memories with effective relevance below the threshold
/// and optionally deletes them (with per-candidate re-validation under
/// the write lock — see [`execute_gc_plan`]). Reports stale index entries
/// (IDs in the index with no backing data) so callers can trigger a reindex.
///
/// On a real run (not `dry_run`) GC also performs store maintenance after
/// the deletion phase: LanceDB fragment compaction + old-version pruning for
/// the memories/chunks tables, and retention pruning + compaction for the
/// project's telemetry event log. Maintenance is best-effort and never fails
/// the GC; see [`GcResult::maintenance`]. A dry run mutates nothing — no
/// deletions and no maintenance.
pub async fn gc_memories(
    store: &MemoryStore,
    config: &EngramConfig,
    dry_run: bool,
    threshold: Option<f64>,
) -> Result<GcResult> {
    let plan = plan_gc(store, config, threshold).await?;

    if dry_run {
        let removed: Vec<String> = plan.candidates.iter().map(|c| c.id.clone()).collect();
        let count = removed.len();
        return Ok(GcResult {
            removed,
            count,
            skipped: Vec::new(),
            stale_entries: plan.stale_entries,
            maintenance: None,
        });
    }

    let mut result = execute_gc_plan(store, config, plan).await?;
    result.maintenance = run_maintenance(store, config).await;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    async fn init_store(dir: &std::path::Path) -> MemoryStore {
        MemoryStore::init(dir, &InMemoryRegistry::new())
            .await
            .unwrap()
    }

    fn low_criticality_memory(summary: &str) -> Memory {
        let mut m = Memory::new(
            MemoryType::Debug,
            summary,
            "scratch content",
            Provenance::human(),
        );
        // Force below any reasonable threshold so it always becomes a GC candidate.
        m.criticality = 0.01;
        m.confidence = 0.05;
        m
    }

    fn high_criticality_memory(summary: &str) -> Memory {
        let mut m = Memory::new(
            MemoryType::Decision,
            summary,
            "keeper content",
            Provenance::human(),
        );
        m.criticality = 1.0;
        m.confidence = 1.0;
        m
    }

    #[tokio::test]
    async fn gc_empty_store_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let cfg = EngramConfig::default();

        let result = gc_memories(&store, &cfg, false, None).await.unwrap();
        assert_eq!(result.count, 0);
        assert!(result.removed.is_empty());
        assert!(result.skipped.is_empty());
        assert!(result.stale_entries.is_empty());
    }

    #[tokio::test]
    async fn gc_dry_run_does_not_delete() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let cfg = EngramConfig::default();

        let mem = low_criticality_memory("garbage");
        let id = mem.id.clone();
        store.create(&mem).await.unwrap();

        // Threshold above the memory's score → identified as a candidate.
        let result = gc_memories(&store, &cfg, true, Some(0.99)).await.unwrap();
        assert_eq!(result.count, 1);
        assert_eq!(result.removed, vec![id.clone()]);
        assert!(result.skipped.is_empty());

        // CRITICAL: dry_run=true must NOT actually delete. Verify the memory
        // is still readable. (Without this guard, "preview" would silently
        // destroy data.)
        let still_there = store.get(&id).await.unwrap();
        assert_eq!(still_there.id, id);

        // A dry run is a pure preview: no maintenance pass either (optimize
        // commits new index versions, i.e. it mutates the store).
        assert!(
            result.maintenance.is_none(),
            "dry run must not run the optimize/maintenance pass"
        );
    }

    #[tokio::test]
    async fn gc_confirm_deletes_below_threshold() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let cfg = EngramConfig::default();

        let mem = low_criticality_memory("delete me");
        let id = mem.id.clone();
        store.create(&mem).await.unwrap();

        let result = gc_memories(&store, &cfg, false, Some(0.99)).await.unwrap();
        assert_eq!(result.count, 1);
        assert!(result.removed.contains(&id));
        assert!(result.skipped.is_empty());

        // dry_run=false really removed it.
        assert!(store.get(&id).await.is_err());

        // A real run finishes with the maintenance pass (compaction +
        // version pruning); it must succeed even on a tiny fresh store.
        assert!(
            result.maintenance.is_some(),
            "real gc run must complete the optimize/maintenance pass"
        );
    }

    /// The maintenance pass must be safe on an effectively-empty store (no
    /// candidates, no telemetry table) and after a burst of writes — the
    /// optimize call path itself is what's exercised here; actual space
    /// reclamation depends on Lance version-retention timing, so only the
    /// non-fatal Ok contract is asserted.
    #[tokio::test]
    async fn gc_maintenance_runs_on_empty_and_busy_stores() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let cfg = EngramConfig::default();

        // Empty store: deletion phase is a no-op; maintenance still runs.
        let result = gc_memories(&store, &cfg, false, None).await.unwrap();
        assert_eq!(result.count, 0);
        assert!(
            result.maintenance.is_some(),
            "maintenance must succeed on an empty store"
        );

        // A burst of versioned writes (each create/update/delete commits a
        // new Lance version), then gc again.
        for i in 0..5 {
            let mem = low_criticality_memory(&format!("burst {i}"));
            let id = mem.id.clone();
            store.create(&mem).await.unwrap();
            store
                .update_with(&id, |m| {
                    m.content.push_str(" updated");
                    Ok(())
                })
                .await
                .unwrap();
        }
        let result = gc_memories(&store, &cfg, false, Some(0.99)).await.unwrap();
        assert_eq!(result.count, 5, "all burst memories collected");
        // Freshly-written versions are younger than the 7-day prune
        // retention, so bytes_removed is environment-dependent — assert only
        // the non-fatal Ok contract (stats were returned at all).
        result
            .maintenance
            .expect("maintenance must succeed after a write burst");
    }

    /// gc's maintenance pass also enforces `[stats].retention_days` on the
    /// project's telemetry event log — events beyond the (default 90-day)
    /// window are deleted, recent ones survive.
    #[tokio::test]
    async fn gc_maintenance_prunes_old_telemetry_events() {
        use crate::telemetry::{persistence, EventRow, EventType};

        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let cfg = EngramConfig::default();
        assert_eq!(
            cfg.stats.retention_days,
            Some(90),
            "test assumes the finite default retention"
        );

        let now = chrono::Utc::now();
        let mk = |ts: DateTime<Utc>, sid: &str| EventRow {
            ts,
            event_type: EventType::ToolCall,
            tool: Some("query".to_string()),
            stage: None,
            duration_ms: Some(1.0),
            success: Some(true),
            hit: None,
            retrieval_quality: None,
            session_id: Some(sid.to_string()),
            memory_ids: None,
        };
        persistence::append_events(
            &store.project_id,
            &[
                mk(now - chrono::Duration::days(120), "ancient"),
                mk(now, "fresh"),
            ],
            None,
        )
        .await;

        let result = gc_memories(&store, &cfg, false, None).await.unwrap();
        assert!(result.maintenance.is_some());

        let rows = persistence::load_recent(&store.project_id, 100)
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "gc must prune telemetry events beyond retention"
        );
        assert_eq!(rows[0].session_id.as_deref(), Some("fresh"));
    }

    #[tokio::test]
    async fn gc_threshold_keeps_high_score_memories() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let cfg = EngramConfig::default();

        let keeper = high_criticality_memory("keeper");
        let keeper_id = keeper.id.clone();
        store.create(&keeper).await.unwrap();

        // Threshold below the keeper's score → nothing should be a candidate.
        let result = gc_memories(&store, &cfg, false, Some(0.0)).await.unwrap();
        assert_eq!(result.count, 0);
        assert!(result.removed.is_empty());

        // Memory still present.
        assert!(store.get(&keeper_id).await.is_ok());
    }

    #[tokio::test]
    async fn gc_uses_config_threshold_when_explicit_is_none() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let mut cfg = EngramConfig::default();
        // Force config threshold above the memory's score so it must be flagged.
        cfg.thresholds.gc = 0.99;

        let mem = low_criticality_memory("config-driven gc");
        store.create(&mem).await.unwrap();

        let result = gc_memories(&store, &cfg, true, None).await.unwrap();
        assert_eq!(result.count, 1, "config threshold should have caught it");
    }

    /// Stale-entry detection: an ID in the index whose backing data file is
    /// missing must be reported in `stale_entries` (callers run `reindex`)
    /// and must NOT be treated as a deletion candidate. We simulate the
    /// corruption by deleting the .md file behind the index entry.
    #[tokio::test]
    async fn gc_stale_entry_reported_and_not_counted() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let cfg = EngramConfig::default();

        let mem = low_criticality_memory("about to go stale");
        let id = mem.id.clone();
        store.create(&mem).await.unwrap();

        // Nuke the .md file but leave the index entry — the planning batch
        // load will now miss it, and gc routes it to stale_entries.
        let memories_dir = tmp.path().join(".engramdb").join("memories");
        for entry in std::fs::read_dir(&memories_dir).unwrap() {
            let path = entry.unwrap().path();
            if path
                .file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.contains(&id))
                .unwrap_or(false)
            {
                std::fs::remove_file(&path).unwrap();
            }
        }

        let result = gc_memories(&store, &cfg, true, Some(0.99)).await.unwrap();
        assert!(
            result.stale_entries.contains(&id),
            "stale id must be reported"
        );
        assert!(
            !result.removed.contains(&id),
            "stale entries must not be counted as gc candidates"
        );
    }

    /// TOCTOU guard: a candidate whose criticality is raised between the
    /// scoring (planning) phase and the deletion (execution) phase must
    /// survive — the stale score from the snapshot must not destroy the
    /// now-important memory.
    #[tokio::test]
    async fn gc_candidate_updated_between_plan_and_execute_survives() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let cfg = EngramConfig::default();

        let mem = low_criticality_memory("about to become important");
        let id = mem.id.clone();
        store.create(&mem).await.unwrap();

        let plan = plan_gc(&store, &cfg, Some(0.99)).await.unwrap();
        assert!(
            plan.candidates.iter().any(|c| c.id == id),
            "memory must be a candidate at scoring time"
        );

        // Concurrent update: criticality raised after scoring.
        store
            .update_with(&id, |m| {
                m.criticality = 1.0;
                m.confidence = 1.0;
                Ok(())
            })
            .await
            .unwrap();

        let result = execute_gc_plan(&store, &cfg, plan).await.unwrap();
        assert!(
            result.skipped.contains(&id),
            "updated candidate must be skipped, not deleted on the stale score"
        );
        assert!(!result.removed.contains(&id));
        assert_eq!(result.count, 0);

        // The memory survived.
        assert!(store.get(&id).await.is_ok());
    }

    /// Concurrent-delete tolerance: a candidate deleted by someone else
    /// between planning and execution is skipped — the sweep neither fails
    /// nor aborts, and the remaining candidates are still processed.
    #[tokio::test]
    async fn gc_concurrently_deleted_candidate_is_skipped_not_fatal() {
        let tmp = TempDir::new().unwrap();
        let store = init_store(tmp.path()).await;
        let cfg = EngramConfig::default();

        let gone = low_criticality_memory("deleted concurrently");
        let gone_id = gone.id.clone();
        store.create(&gone).await.unwrap();

        let stays_eligible = low_criticality_memory("still garbage");
        let eligible_id = stays_eligible.id.clone();
        store.create(&stays_eligible).await.unwrap();

        let plan = plan_gc(&store, &cfg, Some(0.99)).await.unwrap();
        assert_eq!(plan.candidates.len(), 2);

        // Concurrent delete after scoring.
        store.delete(&gone_id).await.unwrap();

        let result = execute_gc_plan(&store, &cfg, plan).await.unwrap();
        assert!(
            result.skipped.contains(&gone_id),
            "concurrently deleted candidate must be skipped"
        );
        assert!(
            result.removed.contains(&eligible_id),
            "remaining candidates must still be processed after a skip"
        );
        assert_eq!(result.count, 1);
    }
}

#[cfg(test)]
mod invalidated_retention_tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Memory, MemoryType, Provenance};
    use chrono::Duration;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        (tmp, store)
    }

    fn memory(id: &str, criticality: f64) -> Memory {
        let mut m = Memory::new(MemoryType::Hazard, id, "content", Provenance::human());
        m.id = id.to_string();
        m.criticality = criticality;
        m
    }

    #[tokio::test]
    async fn gc_purges_invalidated_past_retention_only() {
        let (_t, store) = setup().await;
        let config = EngramConfig::default(); // retention 180d

        // High-criticality (never a low-score candidate), invalidated 200
        // days ago → retention candidate.
        let mut old = memory("gc-old-invalid", 0.9);
        old.invalidated_at = Some(Utc::now() - Duration::days(200));
        store.create(&old).await.unwrap();

        // Invalidated recently → kept.
        let mut fresh = memory("gc-fresh-invalid", 0.9);
        fresh.invalidated_at = Some(Utc::now() - Duration::days(5));
        store.create(&fresh).await.unwrap();

        // Live memory → kept.
        store.create(&memory("gc-live", 0.9)).await.unwrap();

        let result = gc_memories(&store, &config, false, None).await.unwrap();
        assert_eq!(result.removed, vec!["gc-old-invalid".to_string()]);
        assert!(store.get("gc-fresh-invalid").await.is_ok());
        assert!(store.get("gc-live").await.is_ok());
        assert!(store.get("gc-old-invalid").await.is_err());
    }

    #[tokio::test]
    async fn gc_retention_zero_keeps_forever_and_dry_run_previews() {
        let (_t, store) = setup().await;

        let mut old = memory("gc-keeper", 0.9);
        old.invalidated_at = Some(Utc::now() - Duration::days(10_000));
        store.create(&old).await.unwrap();

        // retention 0 ⇒ keep forever.
        let mut config = EngramConfig::default();
        config.epistemic.invalidated_retention_days = 0;
        let result = gc_memories(&store, &config, false, None).await.unwrap();
        assert!(result.removed.is_empty());
        assert!(store.get("gc-keeper").await.is_ok());

        // Default retention + dry run ⇒ planned but not deleted.
        let config = EngramConfig::default();
        let result = gc_memories(&store, &config, true, None).await.unwrap();
        assert_eq!(result.removed, vec!["gc-keeper".to_string()]);
        assert!(
            store.get("gc-keeper").await.is_ok(),
            "dry run must not delete"
        );
    }

    #[tokio::test]
    async fn gc_reopened_window_survives_execution_revalidation() {
        let (_t, store) = setup().await;
        let config = EngramConfig::default();

        let mut old = memory("gc-reopened", 0.9);
        old.invalidated_at = Some(Utc::now() - Duration::days(200));
        store.create(&old).await.unwrap();

        // Plan sees it as a retention candidate…
        let plan = plan_gc(&store, &config, None).await.unwrap();
        assert!(plan
            .candidates
            .iter()
            .any(|c| c.id == "gc-reopened" && c.reason == GcReason::InvalidatedRetention));

        // …but a §2.4 reopening lands before execution.
        store
            .update_with("gc-reopened", |m| {
                m.invalidated_at = None;
                m.superseded_by = None;
                Ok(())
            })
            .await
            .unwrap();

        let result = execute_gc_plan(&store, &config, plan).await.unwrap();
        assert!(result.removed.is_empty());
        assert_eq!(result.skipped, vec!["gc-reopened".to_string()]);
        assert!(store.get("gc-reopened").await.is_ok());
    }
}
