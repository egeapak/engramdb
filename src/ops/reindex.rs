//! Reindex operation.

use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;
use anyhow::Result;

/// Result of a reindex operation.
pub struct ReindexResult {
    pub indexed: usize,
    pub embedded: usize,
    pub errors: Vec<String>,
}

/// Rebuild index and optionally re-embed all memories.
pub async fn reindex(
    store: &MemoryStore,
    engine: Option<&RetrievalEngine>,
    embeddings_only: bool,
) -> Result<ReindexResult> {
    let mut indexed = 0;
    let mut embedded = 0;
    let mut errors = Vec::new();

    // Rebuild index from files (unless embeddings_only)
    if !embeddings_only {
        indexed = store.reindex().await?;
    }

    // Re-embed all memories if engine has embeddings
    if let Some(engine) = engine {
        if engine.embeddings_available() {
            let ids = store.list_ids().await?;
            for id in &ids {
                match store.get(id).await {
                    Ok(memory) => match engine.embed_memory(&memory).await {
                        Ok(()) => embedded += 1,
                        Err(e) => errors.push(format!("{}: {}", id, e)),
                    },
                    Err(e) => errors.push(format!("{}: {}", id, e)),
                }
            }
        }
    }

    Ok(ReindexResult {
        indexed,
        embedded,
        errors,
    })
}
