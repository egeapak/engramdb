//! Context-based memory retrieval operation.

use crate::retrieval::engine::{RetrievalEngine, RetrievalQuery, RetrievalResult};
use anyhow::Result;

/// Retrieve memories based on context query.
///
/// This delegates to the RetrievalEngine which handles scope matching,
/// semantic search, and composite scoring.
pub async fn retrieve_memories(
    engine: &RetrievalEngine,
    query: &RetrievalQuery,
) -> Result<RetrievalResult> {
    let result = engine.retrieve(query).await?;
    Ok(result)
}
