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

/// What a reindex would change, without changing it.
///
/// Every list holds memory ids, sorted, so the output is deterministic and two
/// runs are diffable.
#[derive(Debug, Default)]
pub struct ReindexPlan {
    /// Memory files found on disk.
    pub on_disk: usize,
    /// Rows currently in the index.
    pub indexed: usize,
    /// Files with no index row — invisible to every query until reindexed.
    pub not_indexed: Vec<String>,
    /// Rows whose file has changed since it was indexed (content-digest
    /// mismatch). The row serves the *old* summary and keyword stems.
    pub drifted: Vec<String>,
    /// Rows with vectors whose embed digest no longer matches what the current
    /// text, model and chunk width would produce.
    ///
    /// Excludes memories with no vectors at all — see [`Self::not_embedded`].
    pub stale_vectors: Vec<String>,
    /// Memories with no vectors, so unreachable by semantic search.
    pub not_embedded: Vec<String>,
    /// Rows whose currency could not be determined — the file could not be
    /// read or hashed. Not clean, not drifted; declared rather than dropped.
    pub undetermined: Vec<String>,
    /// Rows predating schema 0.8.0, which record no content digest. A reindex
    /// backfills them; until then their currency is simply unknown.
    pub without_digest: usize,
    /// Set when no embedding provider was available, so the vector columns of
    /// this plan could not be computed at all.
    pub embeddings_unavailable: bool,
}

impl ReindexPlan {
    /// True when a reindex would change nothing.
    ///
    /// `without_digest` is deliberately excluded: a store that has simply not
    /// been reindexed since 0.8.0 is not *stale*, and calling it so would push
    /// every such user into a rebuild that changes no answer they get.
    pub fn is_current(&self) -> bool {
        self.not_indexed.is_empty()
            && self.drifted.is_empty()
            && self.stale_vectors.is_empty()
            && self.not_embedded.is_empty()
            && self.undetermined.is_empty()
    }
}

/// Compute what [`reindex`] would do, touching nothing.
///
/// This is the unbudgeted authority the read-path staleness check defers to:
/// it hashes every file and, when an engine with a provider is supplied, every
/// memory's would-be embedding input. `doctor` reports the same content drift
/// but cannot report the vector half — computing an embed digest needs
/// `embedding_texts` and the live provider, and no doctor path builds an
/// engine.
///
/// A checkout conflict makes the content comparison meaningless (two checkouts
/// legitimately hold different bytes for one id, and the index holds the
/// union), so the drift columns are left empty there rather than reporting a
/// fault that no reindex can clear.
pub async fn reindex_dry_run(
    store: &MemoryStore,
    engine: Option<&RetrievalEngine>,
) -> Result<ReindexPlan> {
    let mut plan = ReindexPlan::default();
    let conflicted = store.checkout_conflict().await.is_some();

    let ids = store.list_ids().await?;
    plan.on_disk = ids.len();

    let rows = store.index_digests().await?;
    plan.indexed = rows.len();
    let by_id: std::collections::HashMap<&str, &_> =
        rows.iter().map(|r| (r.memory_id.as_str(), r)).collect();

    for id in &ids {
        if !by_id.contains_key(id.as_str()) {
            plan.not_indexed.push(id.clone());
        }
    }

    // --- content currency -------------------------------------------------
    if !conflicted {
        for row in &rows {
            let Some(recorded) = row.content_sha256.as_deref() else {
                plan.without_digest += 1;
                continue;
            };
            match store.read_memory_bytes(&row.memory_id).await {
                // A row with no file is the `stale_entries` problem doctor
                // already reports; counting it as drift too would double-count
                // one fault.
                Ok(None) => {}
                Ok(Some(bytes)) => {
                    if crate::storage::FileDigest::of(&bytes).sha256 != recorded {
                        plan.drifted.push(row.memory_id.clone());
                    }
                }
                Err(e) => {
                    tracing::warn!("cannot determine currency of {}: {e}", row.memory_id);
                    plan.undetermined.push(row.memory_id.clone());
                }
            }
        }
    }

    // --- vector currency --------------------------------------------------
    let Some(engine) = engine.filter(|e| e.embeddings_available()) else {
        plan.embeddings_unavailable = true;
        finish(&mut plan);
        return Ok(plan);
    };

    let stored = store.embed_digests().await?;
    // One batched load, matching `reindex` itself: a per-id `store.get` is a
    // full directory scan each, which is quadratic over a whole store.
    let loaded = store.get_batch(&ids).await?;
    for (id, memory) in loaded {
        // `has_embedding == false` is "not embedded yet", a different state
        // that is reported separately. `create` returns before its detached
        // ingest embeds, so counting those as stale would make every
        // just-created memory report as needing a rebuild.
        let has_vectors = by_id.get(id.as_str()).is_some_and(|r| r.has_embedding);
        if !has_vectors {
            plan.not_embedded.push(id);
            continue;
        }
        // A stored digest of `None` means vectors written before the digest
        // existed: their currency is unknown, and re-embedding is the only way
        // to find out — which is exactly what the user is asking about.
        match (
            stored.get(&id).and_then(Option::as_deref),
            engine.expected_embed_digest(&memory),
        ) {
            (Some(recorded), Some(expected)) if recorded != expected => plan.stale_vectors.push(id),
            (None, Some(_)) => plan.stale_vectors.push(id),
            _ => {}
        }
    }

    finish(&mut plan);
    Ok(plan)
}

/// Sort every list so the plan is deterministic and two runs diff cleanly.
fn finish(plan: &mut ReindexPlan) {
    plan.not_indexed.sort();
    plan.drifted.sort();
    plan.stale_vectors.sort();
    plan.not_embedded.sort();
    plan.undetermined.sort();
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
            } else if embeddings_only && foreign_checkout.is_none() {
                // `--embeddings-only` is the advertised remediation for an
                // embedding-model mismatch — including DIMENSION changes. The
                // chunks table is opened as-is with its stored width, so
                // after a dimension change every upsert below would fail
                // against the old schema and the loop would error on every
                // memory. Since this branch re-embeds everything anyway,
                // recreating the table is as safe as the full path; it is
                // suppressed under a checkout conflict exactly like above
                // (the other clone's vectors must survive).
                let live_dims = engine
                    .embedding_fingerprint()
                    .map(|f| f.dimensions)
                    .unwrap_or(0);
                if live_dims > 0 {
                    if let Some(stored) = store.chunks_table_dimensions().await? {
                        if stored != live_dims {
                            warnings.push(format!(
                                "chunks table stored {stored}-dimension vectors but the \
                                 provider produces {live_dims}; recreating the table before \
                                 re-embedding"
                            ));
                            store.clear_chunks().await?;
                        }
                    }
                }
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
            // Single batched load (one dir scan) instead of a per-ID
            // `store.get` (one full dir scan each — O(N²) dirent work over
            // the whole store, and reindex by definition runs over the whole
            // store). Mirrors the same conversion in `plan_gc`.
            let loaded = store
                .get_batch(&ids)
                .await
                .map_err(|e| anyhow::anyhow!("batch load failed: {}", e))?;
            let mut by_id: std::collections::HashMap<String, _> = loaded.into_iter().collect();
            let mut to_embed: Vec<crate::types::Memory> = Vec::with_capacity(ids.len());
            for id in &ids {
                match by_id.remove(id) {
                    Some(memory) => to_embed.push(memory),
                    None => errors.push(format!("{}: memory file not found", id)),
                }
            }
            // One batched inference + one batched chunk write for the whole
            // store. `embed_memory` per memory was a small `embed_batch`, a
            // write lock, a full directory scan and two LanceDB commits EACH —
            // quadratic over a store, and reindex by definition runs over the
            // whole store. The batched load above (finding #5) was being
            // undone one frame down.
            let (ok, failures) = engine.embed_memories(&to_embed).await;
            embedded += ok;
            errors.extend(failures.into_iter().map(|(id, e)| format!("{}: {}", id, e)));

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

    // Reindex churns the index heavily (table rebuilds plus a per-memory
    // upsert each, every one committing a new Lance version) — reclaim disk
    // opportunistically. Best-effort: maintenance must never fail a reindex
    // that already succeeded.
    if let Err(e) = store.optimize().await {
        tracing::warn!("reindex: index optimize failed (non-fatal): {e}");
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

        // The short test memory embeds to exactly two chunks (metadata row +
        // content chunk under the default `metadata_vector` composition) —
        // the three stale chunks must be replaced, not appended to.
        let chunks = store.export_chunks(&mem.id).await.unwrap();
        assert_eq!(chunks.len(), 2, "stale chunks must be replaced");
        assert!(chunks.iter().all(|c| c == &vec![0.5f32; 384]));

        // Ghost chunks are gone too.
        assert_eq!(
            store.list_chunk_memory_ids().await.unwrap(),
            vec![mem.id.clone()],
            "only re-embedded memories may remain in the chunks table"
        );

        // Clean success stamps the new model's fingerprint, including the
        // composition id for the default metadata-vector configuration.
        assert_eq!(
            store.embedding_fingerprint().await.unwrap(),
            Some(EmbeddingFingerprint {
                model: "onnx/stub-model".to_string(),
                dimensions: 384,
                composition: Some(crate::storage::manifest::COMPOSITION_METADATA_V1.to_string()),
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

        // B's own memory was re-embedded with the live provider (two rows:
        // metadata + content under the default composition).
        let b_chunks = store_b.export_chunks(&mem_b.id).await.unwrap();
        assert_eq!(b_chunks, vec![vec![0.5f32; 384], vec![0.5f32; 384]]);
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
            composition: None,
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

    // ===================================================================
    // reindex --dry-run
    // ===================================================================

    /// Overwrite a memory's file behind the store's back — a hand edit, a
    /// `git checkout`, a restore.
    async fn edit_behind_store(store: &MemoryStore, id: &str, body: &str) {
        let dir = crate::storage::paths::memories_dir(&store.project_dir);
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        while let Ok(Some(e)) = entries.next_entry().await {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if !path.file_stem().unwrap().to_str().unwrap().contains(id) {
                continue;
            }
            let text = tokio::fs::read_to_string(&path).await.unwrap();
            let (fm, _) = text.split_once("\n---\n").unwrap();
            tokio::fs::write(&path, format!("{fm}\n---\n{body}"))
                .await
                .unwrap();
            return;
        }
        panic!("no file for {id}");
    }

    fn stub_engine(store: MemoryStore) -> RetrievalEngine {
        RetrievalEngine::new(store, EngramConfig::default())
            .with_embedding_provider(Arc::new(StubEmbeddingProvider))
    }

    /// A clean store reports nothing to do — and says so with `is_current`,
    /// not merely by having empty lists.
    #[tokio::test]
    async fn dry_run_reports_a_current_store_as_current() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();
        let engine = stub_engine(MemoryStore::open(tmp.path()).await.unwrap());
        // Embed so the vectors exist and carry a current digest.
        reindex(&store, Some(&engine), false).await.unwrap();

        let plan = reindex_dry_run(&store, Some(&engine)).await.unwrap();
        assert!(plan.is_current(), "{plan:?}");
        assert_eq!(plan.on_disk, 1);
        assert_eq!(plan.indexed, 1);
        assert!(plan.drifted.is_empty());
        assert!(plan.stale_vectors.is_empty());
    }

    /// The content half: an edit behind the store shows up as drift, named.
    #[tokio::test]
    async fn dry_run_names_the_memory_whose_file_changed() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();
        let engine = stub_engine(MemoryStore::open(tmp.path()).await.unwrap());
        reindex(&store, Some(&engine), false).await.unwrap();

        edit_behind_store(&store, &mem.id, "an edited body\n").await;

        let plan = reindex_dry_run(&store, Some(&engine)).await.unwrap();
        assert_eq!(plan.drifted, vec![mem.id.clone()]);
        assert!(!plan.is_current());
        // The rebuild was NOT run — a dry run that changed something would be
        // the one bug this command cannot have.
        let after = reindex_dry_run(&store, Some(&engine)).await.unwrap();
        assert_eq!(after.drifted, vec![mem.id], "dry run must not repair");
    }

    /// The vector half, which `doctor` structurally cannot report: the text
    /// changed, so the stored vectors no longer describe it.
    #[tokio::test]
    async fn dry_run_reports_vectors_that_no_longer_match_their_text() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "original", Provenance::human());
        store.create(&mem).await.unwrap();
        let engine = stub_engine(MemoryStore::open(tmp.path()).await.unwrap());
        reindex(&store, Some(&engine), false).await.unwrap();
        assert!(reindex_dry_run(&store, Some(&engine))
            .await
            .unwrap()
            .is_current());

        // Change the content through the store, but do not re-embed: the row
        // is current with the file while the vectors are not.
        store
            .update(
                &mem.id,
                crate::types::MemoryUpdate {
                    content: Some("completely different text".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let plan = reindex_dry_run(&store, Some(&engine)).await.unwrap();
        assert_eq!(
            plan.stale_vectors,
            vec![mem.id.clone()],
            "the embed digest must no longer match the new text: {plan:?}"
        );
        assert!(
            plan.drifted.is_empty(),
            "the row itself was rewritten by the update, so it is current"
        );
    }

    /// **`create` returns before its detached ingest embeds.** A memory with
    /// no vectors is "not embedded yet", not "stale" — counting it as stale
    /// would make every just-created memory report as needing a rebuild.
    #[tokio::test]
    async fn dry_run_separates_never_embedded_from_stale() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();
        // No reindex, so no vectors were ever written.
        let engine = stub_engine(MemoryStore::open(tmp.path()).await.unwrap());

        let plan = reindex_dry_run(&store, Some(&engine)).await.unwrap();
        assert_eq!(plan.not_embedded, vec![mem.id.clone()]);
        assert!(
            plan.stale_vectors.is_empty(),
            "never-embedded must not be reported as stale: {plan:?}"
        );
    }

    /// Without a provider the vector half cannot be computed at all, and the
    /// plan says so rather than reporting an empty list as "all current".
    #[tokio::test]
    async fn dry_run_declares_when_vectors_could_not_be_checked() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store
            .create(&Memory::new(
                MemoryType::Decision,
                "T",
                "C",
                Provenance::human(),
            ))
            .await
            .unwrap();

        let plan = reindex_dry_run(&store, None).await.unwrap();
        assert!(plan.embeddings_unavailable);
        assert!(plan.stale_vectors.is_empty());
        assert!(
            plan.not_embedded.is_empty(),
            "with no provider, nothing can be said about vectors either way"
        );
    }

    /// Rows predating schema 0.8.0 record no digest. That is *unknown*, and
    /// counting it as staleness would push every un-reindexed store into a
    /// rebuild that changes no answer they get.
    #[tokio::test]
    async fn dry_run_counts_undigested_rows_without_calling_them_stale() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mem = Memory::new(MemoryType::Decision, "T", "C", Provenance::human());
        store.create(&mem).await.unwrap();
        let engine = stub_engine(MemoryStore::open(tmp.path()).await.unwrap());
        reindex(&store, Some(&engine), false).await.unwrap();
        crate::storage::test_support::clear_content_digests(&store)
            .await
            .unwrap();

        let plan = reindex_dry_run(&store, Some(&engine)).await.unwrap();
        assert_eq!(plan.without_digest, 1);
        assert!(plan.drifted.is_empty());
        assert!(
            plan.is_current(),
            "an undigested row is unknown, not stale: {plan:?}"
        );
    }
}
