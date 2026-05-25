//! Garbage collection operation.

use crate::scoring::{composite_score, ScoringContext};
use crate::storage::MemoryStore;
use crate::types::EngramConfig;
use anyhow::Result;
use chrono::Utc;

/// Result of a GC operation.
pub struct GcResult {
    pub removed: Vec<String>,
    pub count: usize,
    /// IDs found in index but missing from data store. Suggests reindex is needed.
    pub stale_entries: Vec<String>,
}

/// Run garbage collection on memories below threshold.
///
/// Identifies memories with effective relevance below the threshold
/// and optionally deletes them. Reports stale index entries (IDs in
/// the index with no backing data) so callers can trigger a reindex.
pub async fn gc_memories(
    store: &MemoryStore,
    config: &EngramConfig,
    dry_run: bool,
    threshold: Option<f64>,
) -> Result<GcResult> {
    let threshold = threshold.unwrap_or(config.thresholds.gc);
    let ids = store.list_ids().await?;
    let now = Utc::now();

    let mut candidates = Vec::new();
    let mut stale_entries = Vec::new();

    for id in &ids {
        let memory = match store.get(id).await {
            Ok(m) => m,
            Err(_) => {
                stale_entries.push(id.clone());
                continue;
            }
        };
        let context = ScoringContext::scope_only(None, &[]);
        let breakdown = composite_score(&memory, &context, config, now);

        if breakdown.final_score < threshold {
            candidates.push(memory.id.clone());
        }
    }

    if !dry_run {
        for id in &candidates {
            store.delete(id).await?;
        }
    }

    let count = candidates.len();
    Ok(GcResult {
        removed: candidates,
        count,
        stale_entries,
    })
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

        // Nuke the .md file but leave the index entry — `store.get(id)` will
        // now fail, the gc loop catches that and routes to stale_entries.
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
}
