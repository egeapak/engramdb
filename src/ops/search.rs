//! Keyword search operation.

use crate::retrieval::engine::{RetrievalEngine, ScoredMemory};
use crate::retrieval::filters::SearchFilters;
use anyhow::Result;

/// Search memories by keyword.
pub fn search_memories(
    engine: &RetrievalEngine,
    query: &str,
    filters: &SearchFilters,
) -> Result<Vec<ScoredMemory>> {
    let results = engine.search(query, filters)?;
    Ok(results)
}
