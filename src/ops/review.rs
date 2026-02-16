//! List memories needing review.

use crate::storage::MemoryStore;
use crate::types::{Memory, MemoryType, Status};
use anyhow::Result;

/// Parameters for reviewing memories.
pub struct ReviewParams {
    pub scope: Option<String>,
    pub max_results: Option<usize>,
    pub type_filter: Option<MemoryType>,
    pub challenged_only: bool,
    pub stale_only: bool,
}

/// List memories that need review (status = NeedsReview or Challenged).
///
/// Returns memories sorted by criticality descending.
pub async fn review_memories(store: &MemoryStore, params: &ReviewParams) -> Result<Vec<Memory>> {
    let entries = store.list_summary().await?;

    // Filter candidates at the index level, then batch-load
    let candidate_ids: Vec<String> = entries
        .iter()
        .filter(|e| e.status == Status::NeedsReview || e.status == Status::Challenged)
        .filter(|e| {
            params
                .scope
                .as_ref()
                .is_none_or(|s| e.logical.iter().any(|l| l == s))
        })
        .map(|e| e.id.clone())
        .collect();

    let loaded = store.get_batch(&candidate_ids).await?;
    let mut memories: Vec<Memory> = loaded.into_iter().map(|(_, m)| m).collect();

    // Apply type filter
    if let Some(type_filter) = params.type_filter {
        memories.retain(|m| m.type_ == type_filter);
    }

    // Apply status filters
    if params.challenged_only {
        memories.retain(|m| matches!(m.status, Status::Challenged));
    } else if params.stale_only {
        memories.retain(|m| matches!(m.status, Status::NeedsReview));
    }

    // Sort by criticality descending
    memories.sort_by(|a, b| {
        b.criticality
            .partial_cmp(&a.criticality)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if let Some(max) = params.max_results {
        memories.truncate(max);
    }

    Ok(memories)
}
