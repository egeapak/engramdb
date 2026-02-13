//! Keyword search operation.

use crate::retrieval::engine::{RetrievalEngine, ScoredMemory};
use crate::retrieval::filters::SearchFilters;
use anyhow::Result;

/// Search memories by keyword.
pub async fn search_memories(
    engine: &RetrievalEngine,
    query: &str,
    filters: &SearchFilters,
    max_results: Option<usize>,
) -> Result<Vec<ScoredMemory>> {
    let mut results = engine.search(query, filters).await?;
    if let Some(max) = max_results {
        results.truncate(max);
    }
    Ok(results)
}
