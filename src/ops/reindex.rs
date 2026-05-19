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

            // Stamp the store with the embedding model identity once every
            // memory re-embedded cleanly. On partial failure we leave the
            // fingerprint as-is so the store stays honestly flagged.
            if errors.is_empty() {
                if let Some(fingerprint) = engine.embedding_fingerprint() {
                    store
                        .set_embedding_fingerprint(fingerprint)
                        .await
                        .map_err(|e| {
                            anyhow::anyhow!("failed to stamp embedding fingerprint: {}", e)
                        })?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::EmbeddingProvider;
    use crate::storage::{EmbeddingFingerprint, InMemoryRegistry};
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance};
    use async_trait::async_trait;
    use std::sync::Arc;
    use tempfile::TempDir;

    /// Embedding provider whose every embed attempt fails — used to drive
    /// the reindex partial-failure path deterministically.
    struct FailingEmbeddingProvider;

    #[async_trait]
    impl EmbeddingProvider for FailingEmbeddingProvider {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("forced embed failure")
        }
        async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("forced embed failure")
        }
        fn dimensions(&self) -> usize {
            384
        }
        fn max_tokens(&self) -> usize {
            256
        }
        fn model_id(&self) -> String {
            "onnx/new-model".to_string()
        }
    }

    /// CRITICAL guard: on a partial (here: total) embedding failure, the
    /// store fingerprint must be left exactly as-is — never advanced to the
    /// current model — so a flagged store stays honestly flagged instead of
    /// silently claiming it was re-embedded with the new model.
    #[tokio::test]
    async fn reindex_does_not_stamp_fingerprint_when_embeddings_fail() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();

        // Pre-existing (stale) fingerprint the reindex must NOT overwrite.
        let original = EmbeddingFingerprint {
            model: "onnx/old-model".to_string(),
            dimensions: 384,
        };
        store
            .set_embedding_fingerprint(original.clone())
            .await
            .unwrap();

        // Separate handle for the engine (mirrors MCP: store vs engine.store
        // are distinct handles to the same on-disk store).
        let engine_store = MemoryStore::open(temp_dir.path()).await.unwrap();
        let engine = RetrievalEngine::new(engine_store, EngramConfig::default())
            .with_embedding_provider(Arc::new(FailingEmbeddingProvider));

        let result = reindex(&store, Some(&engine), false).await.unwrap();

        assert_eq!(result.embedded, 0, "no memory should embed successfully");
        assert!(
            !result.errors.is_empty(),
            "the forced failure must surface in errors"
        );
        assert_eq!(
            store.embedding_fingerprint().await.unwrap(),
            Some(original),
            "fingerprint must be unchanged after a failed re-embed"
        );
    }
}
