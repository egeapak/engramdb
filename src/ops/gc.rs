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
        let context = ScoringContext::scope_only(None, vec![]);
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
