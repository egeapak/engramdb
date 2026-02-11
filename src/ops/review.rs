//! List memories needing review.

use crate::storage::MemoryStore;
use crate::types::{Memory, Status};
use anyhow::Result;

/// List memories that need review (status = NeedsReview or Challenged).
///
/// Returns memories sorted by criticality descending.
pub async fn review_memories(
    store: &MemoryStore,
    scope: Option<&str>,
    max_results: Option<usize>,
) -> Result<Vec<Memory>> {
    let entries = store.list().await?;

    let mut memories: Vec<Memory> = Vec::new();
    for e in entries.iter() {
        if e.status == Status::NeedsReview || e.status == Status::Challenged {
            if let Some(scope) = scope {
                if !e.logical.iter().any(|s| s == scope) {
                    continue;
                }
            }
            if let Ok(memory) = store.get(&e.id).await {
                memories.push(memory);
            }
        }
    }

    // Sort by criticality descending
    memories.sort_by(|a, b| {
        b.criticality
            .partial_cmp(&a.criticality)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if let Some(max) = max_results {
        memories.truncate(max);
    }

    Ok(memories)
}
