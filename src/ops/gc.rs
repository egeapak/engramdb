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

/// A GC deletion candidate identified during the planning phase.
#[derive(Debug, Clone)]
pub struct GcCandidate {
    pub id: String,
    /// `updated_at` observed at scoring time. Execution skips the deletion
    /// if the memory has been modified since (the score is stale).
    pub updated_at: DateTime<Utc>,
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

    let mut candidates = Vec::new();
    for (_, memory) in &loaded {
        let context = ScoringContext::scope_only(None, &[]);
        let breakdown = composite_score(memory, &context, config, now);

        if breakdown.final_score < threshold {
            candidates.push(GcCandidate {
                id: memory.id.clone(),
                updated_at: memory.updated_at,
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

    for candidate in &plan.candidates {
        let context = ScoringContext::scope_only(None, &[]);
        let deleted = store
            .delete_if(&candidate.id, |memory| {
                memory.updated_at == candidate.updated_at
                    && composite_score(memory, &context, config, now).final_score < plan.threshold
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
    })
}

/// Run garbage collection on memories below threshold.
///
/// Identifies memories with effective relevance below the threshold
/// and optionally deletes them (with per-candidate re-validation under
/// the write lock — see [`execute_gc_plan`]). Reports stale index entries
/// (IDs in the index with no backing data) so callers can trigger a reindex.
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
        });
    }

    execute_gc_plan(store, config, plan).await
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
