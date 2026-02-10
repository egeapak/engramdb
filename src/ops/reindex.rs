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
pub fn reindex(
    store: &MemoryStore,
    engine: Option<&RetrievalEngine>,
    embeddings_only: bool,
) -> Result<ReindexResult> {
    let mut indexed = 0;
    let mut embedded = 0;
    let mut errors = Vec::new();

    // Rebuild index from files (unless embeddings_only)
    if !embeddings_only {
        indexed = store.reindex()?;
    }

    // Re-embed all memories if engine has embeddings
    if let Some(engine) = engine {
        if engine.embeddings_available() {
            let entries = store.list()?;
            for entry in &entries {
                match store.get(&entry.id) {
                    Ok(memory) => match engine.embed_memory(&memory) {
                        Ok(()) => embedded += 1,
                        Err(e) => errors.push(format!("{}: {}", entry.id, e)),
                    },
                    Err(e) => errors.push(format!("{}: {}", entry.id, e)),
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
