//! List memories needing review.

use crate::storage::MemoryStore;
use crate::types::{Memory, Status};
use anyhow::Result;

/// List memories that need review (status = NeedsReview or Challenged).
///
/// Returns memories sorted by criticality descending.
pub fn review_memories(
    store: &MemoryStore,
    scope: Option<&str>,
    max_results: Option<usize>,
) -> Result<Vec<Memory>> {
    let entries = store.list()?;

    let mut memories: Vec<Memory> = entries
        .iter()
        .filter(|e| e.status == Status::NeedsReview || e.status == Status::Challenged)
        .filter(|e| {
            if let Some(scope) = scope {
                e.logical.iter().any(|s| s == scope)
            } else {
                true
            }
        })
        .filter_map(|e| store.get(&e.id).ok())
        .collect();

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
