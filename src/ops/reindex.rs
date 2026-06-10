//! Reindex operation.

use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;
use anyhow::Result;

/// Result of a reindex operation.
#[derive(Debug)]
pub struct ReindexResult {
    pub indexed: usize,
    pub embedded: usize,
    pub errors: Vec<String>,
    /// Non-fatal conditions the user must see — e.g. re-embedding was
    /// skipped because no embedding provider was available. Existing
    /// vectors are preserved in that case, but the user asked for a full
    /// reindex and didn't get one, so surfaces (CLI/MCP) must render these.
    pub warnings: Vec<String>,
}

/// Rebuild index and optionally re-embed all memories.
///
/// Behavior matrix:
/// - Full reindex, provider available: rebuild metadata, then drop and
///   recreate the chunks table (picking up any dimension change) and
///   re-embed every memory.
/// - Full reindex, provider unavailable (engine absent or without
///   embeddings): rebuild metadata only. Existing vectors are preserved;
///   if an engine was supplied (the caller wanted re-embedding) a warning
///   is added to the result.
/// - `embeddings_only`, provider available: re-embed every memory in
///   place (per-memory upsert replaces stale chunks atomically).
/// - `embeddings_only`, provider unavailable: error. The caller explicitly
///   asked for vectors; silently reporting `embedded: 0` as success would
///   mask the broken state `doctor` told them to fix.
///
/// When another still-existing checkout owns this project ID (a second clone
/// of the same git remote — see `MemoryStore::checkout_conflict`), every
/// destructive step degrades to non-destructive: the metadata rebuild is
/// upsert-only, the chunks table is never dropped, and only memories backed
/// by a local file are re-embedded. A warning explains the degraded mode.
pub async fn reindex(
    store: &MemoryStore,
    engine: Option<&RetrievalEngine>,
    embeddings_only: bool,
) -> Result<ReindexResult> {
    let mut indexed = 0;
    let mut embedded = 0;
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let embeddings_available = engine.is_some_and(|e| e.embeddings_available());

    // Shared-ID guard: a different, still-existing checkout (second clone of
    // the same remote) owns this project ID. The LanceDB index is shared but
    // only this checkout's memory files are visible here, so every
    // destructive step below must be skipped or scoped to local files —
    // otherwise the other clone's rows and vectors are silently destroyed.
    let foreign_checkout = store.checkout_conflict().await;

    // The user explicitly asked for vectors to be rebuilt; fail fast
    // instead of silently doing nothing. Existing vectors are untouched.
    if embeddings_only && !embeddings_available {
        anyhow::bail!(
            "embedding provider unavailable — refusing to rebuild vectors; \
             existing embeddings are preserved. Fix the model cache (see \
             `engramdb doctor`) and retry."
        );
    }

    // Rebuild index from files (unless embeddings_only). This rebuilds only
    // the metadata table; existing embedding vectors survive.
    if !embeddings_only {
        indexed = store.reindex().await?;
        if let Some(other) = &foreign_checkout {
            warnings.push(format!(
                "this checkout shares its project ID (and index) with another checkout \
                 at {} — reindex ran in non-destructive (upsert-only) mode, so the other \
                 checkout's index rows and vectors were preserved and stale entries were \
                 NOT pruned. Run reindex from the registered checkout for a full rebuild, \
                 or remove it and run `engramdb init` here to take over the registration.",
                other.display(),
            ));
        }
    }

    // Re-embed all memories if engine has embeddings
    if let Some(engine) = engine {
        if embeddings_available {
            // Full reindex with a confirmed provider: drop and recreate the
            // chunks table so stale vectors are fully replaced and a
            // dimension change in config takes effect. Only safe here —
            // every memory is re-embedded immediately below. Under a
            // checkout conflict the table is left in place instead: the
            // per-memory `upsert_chunks` below still replaces this
            // checkout's vectors atomically, while the other clone's
            // vectors survive.
            if !embeddings_only && foreign_checkout.is_none() {
                store.clear_chunks().await?;
            }

            // Under a checkout conflict the shared index also lists the other
            // clone's memories, whose files are not visible here. Re-embed
            // only the ids backed by a local file so they aren't reported as
            // per-memory errors.
            let ids = store.list_ids().await?;
            let ids: Vec<String> = if foreign_checkout.is_some() {
                let local = store
                    .batch_exists(&ids)
                    .await
                    .map_err(|e| anyhow::anyhow!("batch existence check failed: {}", e))?;
                ids.into_iter().filter(|id| local.contains(id)).collect()
            } else {
                ids
            };
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
        } else {
            // The caller wanted re-embedding (an engine was supplied) but no
            // provider is available. The index was rebuilt and existing
            // vectors were preserved — say so loudly instead of reporting
            // `embedded: 0` as quiet success.
            warnings.push(
                "embedding provider unavailable — skipped re-embedding; existing \
                 vectors were preserved. Fix the model cache (see `engramdb doctor`) \
                 and run `engramdb reindex --embeddings-only`."
                    .to_string(),
            );
        }
    }

    Ok(ReindexResult {
        indexed,
        embedded,
        errors,
        warnings,
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

    /// Embedding provider that deterministically succeeds — used to verify
    /// the provider-available path replaces (not duplicates) chunks without
    /// loading any real ONNX model.
    struct StubEmbeddingProvider;

    #[async_trait]
    impl EmbeddingProvider for StubEmbeddingProvider {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.5f32; 384])
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.5f32; 384]).collect())
        }
        fn dimensions(&self) -> usize {
            384
        }
        fn max_tokens(&self) -> usize {
            256
        }
        fn model_id(&self) -> String {
            "onnx/stub-model".to_string()
        }
    }

    /// CRITICAL data-loss guard: a full reindex with NO embedding provider
    /// (offline machine, missing model cache — the exact state `doctor`
    /// tells users to fix by running reindex) must preserve existing
    /// vectors and warn loudly, not silently drop the chunks table and
    /// report success with `embedded: 0`.
    #[tokio::test]
    async fn reindex_without_provider_preserves_chunks_and_warns() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();
        store
            .upsert_chunks(&mem.id, vec![vec![0.25f32; 384]])
            .await
            .unwrap();

        // Engine WITHOUT an embedding provider — the caller wanted a full
        // reindex (engine supplied) but the provider failed to resolve.
        let engine_store = MemoryStore::open(temp_dir.path()).await.unwrap();
        let engine = RetrievalEngine::new(engine_store, EngramConfig::default());
        assert!(!engine.embeddings_available());

        let result = reindex(&store, Some(&engine), false).await.unwrap();

        assert_eq!(result.indexed, 1, "metadata must still be rebuilt");
        assert_eq!(result.embedded, 0);
        assert!(result.errors.is_empty());
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("embedding provider unavailable")),
            "skipped re-embedding must surface as a warning, got: {:?}",
            result.warnings
        );
        let chunks = store.export_chunks(&mem.id).await.unwrap();
        assert_eq!(
            chunks.len(),
            1,
            "existing vectors must survive a reindex without a provider"
        );
        assert_eq!(chunks[0], vec![0.25f32; 384]);
    }

    /// `embeddings_only` is an explicit request to rebuild vectors. With no
    /// provider it must error (not silently no-op), and the existing
    /// vectors must be untouched.
    #[tokio::test]
    async fn embeddings_only_without_provider_errors_and_preserves_chunks() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();
        store
            .upsert_chunks(&mem.id, vec![vec![0.25f32; 384]])
            .await
            .unwrap();

        // Engine without provider.
        let engine_store = MemoryStore::open(temp_dir.path()).await.unwrap();
        let engine = RetrievalEngine::new(engine_store, EngramConfig::default());

        let err = reindex(&store, Some(&engine), true)
            .await
            .expect_err("embeddings-only without a provider must fail fast");
        assert!(
            err.to_string().contains("embedding provider unavailable"),
            "error must explain the refusal, got: {err}"
        );

        // No engine at all (e.g. embeddings_only combined with index_only)
        // must fail the same way.
        assert!(reindex(&store, None, true).await.is_err());

        let chunks = store.export_chunks(&mem.id).await.unwrap();
        assert_eq!(chunks.len(), 1, "vectors must survive the refused reindex");
    }

    /// With a working provider, a full reindex must fully replace stale
    /// vectors — old chunks are dropped and re-embedded, never duplicated —
    /// and the fingerprint is stamped on clean success.
    #[tokio::test]
    async fn reindex_with_provider_replaces_chunks() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();

        // Stale state: three chunks from an old model.
        store
            .upsert_chunks(
                &mem.id,
                vec![vec![0.1f32; 384], vec![0.2f32; 384], vec![0.3f32; 384]],
            )
            .await
            .unwrap();
        // Plus chunks for a memory that no longer exists on disk.
        store
            .upsert_chunks("ghost-id", vec![vec![0.9f32; 384]])
            .await
            .unwrap();

        let engine_store = MemoryStore::open(temp_dir.path()).await.unwrap();
        let engine = RetrievalEngine::new(engine_store, EngramConfig::default())
            .with_embedding_provider(Arc::new(StubEmbeddingProvider));

        let result = reindex(&store, Some(&engine), false).await.unwrap();

        assert_eq!(result.indexed, 1);
        assert_eq!(result.embedded, 1);
        assert!(result.errors.is_empty());
        assert!(result.warnings.is_empty());

        // The short test memory embeds to exactly one chunk — the three
        // stale chunks must be replaced, not appended to.
        let chunks = store.export_chunks(&mem.id).await.unwrap();
        assert_eq!(chunks.len(), 1, "stale chunks must be replaced");
        assert_eq!(chunks[0], vec![0.5f32; 384]);

        // Ghost chunks are gone too.
        assert_eq!(
            store.list_chunk_memory_ids().await.unwrap(),
            vec![mem.id.clone()],
            "only re-embedded memories may remain in the chunks table"
        );

        // Clean success stamps the new model's fingerprint.
        assert_eq!(
            store.embedding_fingerprint().await.unwrap(),
            Some(EmbeddingFingerprint {
                model: "onnx/stub-model".to_string(),
                dimensions: 384,
            })
        );
    }

    /// Create a fake git clone with a fixed remote URL so two directories
    /// compute the same (remote-derived) project ID.
    fn make_clone(root: &std::path::Path, name: &str, remote: &str) -> std::path::PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join(".git").join("config"),
            format!(
                "[remote \"origin\"]\n\turl = https://github.com/example/{}.git\n",
                remote
            ),
        )
        .unwrap();
        dir
    }

    /// CRITICAL data-loss guard: a full reindex (with a working provider)
    /// run from the second clone of the same remote must degrade to
    /// non-destructive mode — the other clone's index rows and vectors
    /// survive, the chunks table is not dropped, only local files are
    /// re-embedded, and a warning explains the degraded mode.
    #[tokio::test]
    async fn reindex_in_second_clone_is_upsert_only_and_warns() {
        let tmp = TempDir::new().unwrap();
        let a = make_clone(tmp.path(), "clone-a", "ops-reindex-conflict");
        let b = make_clone(tmp.path(), "clone-b", "ops-reindex-conflict");
        // The conflict guard reads the global file registry (redirected to a
        // per-process temp dir by the test-isolation arm).
        let registry = crate::storage::FileRegistry::global().unwrap();

        let store_a = MemoryStore::init(&a, &registry).await.unwrap();
        let store_b = MemoryStore::init(&b, &registry).await.unwrap();

        // Clone A's memory file is invisible from B; its index row and
        // vector live in the shared LanceDB table.
        let mem_a = Memory::new(MemoryType::Decision, "A", "C", Provenance::human());
        store_a.create(&mem_a).await.unwrap();
        store_a
            .upsert_chunks(&mem_a.id, vec![vec![0.25f32; 384]])
            .await
            .unwrap();

        let mem_b = Memory::new(MemoryType::Decision, "B", "C", Provenance::human());
        store_b.create(&mem_b).await.unwrap();

        let engine_store = MemoryStore::open(&b).await.unwrap();
        let engine = RetrievalEngine::new(engine_store, EngramConfig::default())
            .with_embedding_provider(Arc::new(StubEmbeddingProvider));

        let result = reindex(&store_b, Some(&engine), false).await.unwrap();

        assert_eq!(result.indexed, 1, "only B's files are scanned");
        assert_eq!(result.embedded, 1, "only B's local memory is re-embedded");
        assert!(
            result.errors.is_empty(),
            "the other clone's ids must not surface as errors: {:?}",
            result.errors
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.contains("another checkout")),
            "the degraded mode must be surfaced as a warning, got: {:?}",
            result.warnings
        );

        // The other clone's row and vector survive — clear_memories,
        // clear_chunks, and the orphan prune were all skipped.
        assert!(store_b.list_ids().await.unwrap().contains(&mem_a.id));
        let chunks = store_b.export_chunks(&mem_a.id).await.unwrap();
        assert_eq!(
            chunks.len(),
            1,
            "clear_chunks must be skipped under a checkout conflict"
        );
        assert_eq!(chunks[0], vec![0.25f32; 384]);

        // B's own memory was re-embedded with the live provider.
        let b_chunks = store_b.export_chunks(&mem_b.id).await.unwrap();
        assert_eq!(b_chunks, vec![vec![0.5f32; 384]]);
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
