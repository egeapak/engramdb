//! Unified memory query operation.
//!
//! Thin wrapper delegating to [`RetrievalEngine::query`]. See there for the
//! semantics of [`RetrievalMode::Rank`] vs [`RetrievalMode::Filter`].

use crate::retrieval::engine::{RetrievalEngine, RetrievalQuery, RetrievalResult};
use anyhow::Result;

/// Query memories via the retrieval engine.
pub async fn query_memories(
    engine: &RetrievalEngine,
    query: &RetrievalQuery,
) -> Result<RetrievalResult> {
    let result = engine.query(query).await?;
    Ok(result)
}
