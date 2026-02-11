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
}

/// Run garbage collection on memories below threshold.
///
/// Identifies memories with effective relevance below the threshold
/// and optionally deletes them.
pub async fn gc_memories(
    store: &MemoryStore,
    config: &EngramConfig,
    dry_run: bool,
    threshold: Option<f64>,
) -> Result<GcResult> {
    let threshold = threshold.unwrap_or(config.thresholds.gc);
    let entries = store.list().await?;
    let now = Utc::now();

    let mut candidates = Vec::new();

    for entry in &entries {
        let memory = store.get(&entry.id).await?;
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
    })
}
