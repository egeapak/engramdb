//! Reindex operation.

use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;
use anyhow::Result;

/// Result of a reindex operation.
#[derive(Debug)]
pub struct ReindexResult {
    pub indexed: usize,
    pub embedded: usize,
    /// Index rows whose file was unchanged, so the row was left alone.
    ///
    /// Only ever non-zero for an `incremental` run.
    pub rows_skipped: usize,
    /// Memories whose vectors were already current and were left alone.
    ///
    /// Counted rather than inferred: a skip that is not reported is a silent
    /// loss path, and "embedded: 3" on a 900-memory store has to be
    /// distinguishable from a reindex that failed on 897 of them.
    pub skipped: usize,
    pub errors: Vec<String>,
    /// Non-fatal conditions the user must see — e.g. re-embedding was
    /// skipped because no embedding provider was available. Existing
    /// vectors are preserved in that case, but the user asked for a full
    /// reindex and didn't get one, so surfaces (CLI/MCP) must render these.
    pub warnings: Vec<String>,
}

/// What a [`reindex`] call should do.
///
/// A struct rather than two positional `bool`s: `embeddings_only` and `force`
/// are adjacent, same-typed, and mean very different things, which is exactly
/// the shape that gets transposed at a call site and compiles fine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReindexOptions {
    /// Re-embed only; skip the metadata rebuild.
    pub embeddings_only: bool,
    /// Rebuild only the index rows whose file changed since it was indexed.
    ///
    /// Opt-in, and a smaller win than it sounds: deciding whether a file
    /// changed means reading and hashing it, so the I/O is unchanged and only
    /// the parse, the stem derivation and the row write are saved. Mutually
    /// exclusive with `force` in spirit — `force` implies a full rebuild — and
    /// ignored entirely when `embeddings_only` is set, which rebuilds no rows.
    pub incremental: bool,
    /// Re-embed every memory even when its vectors are provably current.
    ///
    /// This is the repair setting. Skipping trusts the digest stored beside
    /// the vectors, so anything that corrupts a chunk row without disturbing
    /// its digest is invisible to the skip predicate — `force` is what fixes
    /// that. `doctor --fix` and `projects repair` therefore always pass it: a
    /// repair that trusts the stamp it is repairing is not a repair.
    pub force: bool,
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
    /// `embeddings_unavailable` is deliberately disqualifying: with no
    /// provider the vector half of the check never ran, so both vector lists
    /// are empty for want of an answer rather than for want of a problem.
    /// Reading that as "current" is the one thing this whole feature exists to
    /// stop — a clean report from a check that did not happen.
    pub fn is_current(&self) -> bool {
        !self.embeddings_unavailable
            && self.not_indexed.is_empty()
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

    // Files from the FILESYSTEM, rows from the index. Reading both sides from
    // `list_ids()` compared the index against itself: `not_indexed` was then
    // empty by construction and `on_disk` was a row count wearing a file
    // count's name, so a memory that had never been indexed — the single
    // loudest thing a dry run should report — was invisible and the plan said
    // "current".
    let file_ids = store.list_file_ids().await?;
    plan.on_disk = file_ids.len();

    let rows = store.index_digests().await?;
    plan.indexed = rows.len();
    let by_id: std::collections::HashMap<&str, &_> =
        rows.iter().map(|r| (r.memory_id.as_str(), r)).collect();

    for id in &file_ids {
        if !by_id.contains_key(id.as_str()) {
            plan.not_indexed.push(id.clone());
        }
    }

    // --- content currency -------------------------------------------------
    // One batched pass, not one `read_memory_bytes` per row: that resolves each
    // id with a full `read_dir` of two directories, so a whole-store check was
    // quadratic in dirent work.
    if !conflicted {
        let (digests, unreadable) = store.file_digests().await?;
        plan.undetermined = unreadable;
        for row in &rows {
            let Some(recorded) = row.content_sha256.as_deref() else {
                plan.without_digest += 1;
                continue;
            };
            // A row with no file is the `stale_entries` problem doctor already
            // reports; counting it as drift too would double-count one fault.
            if let Some(actual) = digests.get(&row.memory_id) {
                if actual.sha256 != recorded {
                    plan.drifted.push(row.memory_id.clone());
                }
            }
        }
    }

    // --- vector currency --------------------------------------------------
    // Gated on the conflict exactly like the content half. Under a shared
    // project id the index holds the OTHER checkout's rows, whose vectors this
    // checkout can neither validate nor rebuild, so reporting them as stale is
    // a permanent finding no reindex clears — the same reason `doctor`,
    // `stats` and `check_staleness` all suppress there.
    if conflicted {
        plan.embeddings_unavailable = engine.is_none_or(|e| !e.embeddings_available());
        finish(&mut plan);
        return Ok(plan);
    }

    let Some(engine) = engine.filter(|e| e.embeddings_available()) else {
        plan.embeddings_unavailable = true;
        finish(&mut plan);
        return Ok(plan);
    };

    let stored = store.embed_digests().await?;
    // One batched load, matching `reindex` itself: a per-id `store.get` is a
    // full directory scan each, which is quadratic over a whole store.
    let loaded = store.get_batch(&file_ids).await?;
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
        // Mirror `reindex`'s own retain predicate exactly. It rebuilds unless
        // BOTH digests exist and agree, so a memory whose expected digest is
        // `None` (it produces no embed text, and reindex will delete its
        // chunks) must read as work to do here too — otherwise the dry run
        // calls a store current that the very next reindex would change.
        let recorded = stored.get(&id).and_then(Option::as_deref);
        let expected = engine.expected_embed_digest(&memory);
        let current = matches!((recorded, &expected), (Some(r), Some(e)) if r == e);
        if !current {
            plan.stale_vectors.push(id);
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
    options: ReindexOptions,
) -> Result<ReindexResult> {
    let ReindexOptions {
        embeddings_only,
        incremental,
        force,
    } = options;
    let mut indexed = 0;
    let mut embedded = 0;
    let mut skipped = 0;
    let mut rows_skipped = 0;
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
        // `force` means "rebuild everything", which includes the rows: a
        // repair that skipped rows would not be a repair.
        if incremental && !force {
            let counts = store.reindex_incremental().await?;
            indexed = counts.indexed;
            rows_skipped = counts.skipped;
        } else {
            indexed = store.reindex().await?;
        }
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
            // A dimension change makes skipping impossible and a table
            // recreation mandatory, in BOTH branches: the chunks table is
            // opened as-is with its stored width, so every upsert below would
            // otherwise fail against the old schema and the run would error on
            // every memory. Detected before anything else because it decides
            // whether this run is a full rebuild.
            let live_dims = engine
                .embedding_fingerprint()
                .map(|f| f.dimensions)
                .unwrap_or(0);
            let stored_dims = store.chunks_table_dimensions().await?;
            let dimension_change =
                live_dims > 0 && stored_dims.is_some_and(|stored| stored != live_dims);
            // Worded from what will actually happen. Under a checkout conflict
            // the recreation below is suppressed so the other clone's vectors
            // survive, and claiming a repair that did not occur sends the user
            // away believing the mismatch is resolved.
            if dimension_change {
                if foreign_checkout.is_none() {
                    warnings.push(format!(
                        "chunks table stored {}-dimension vectors but the provider produces \
                         {live_dims}; recreating the table and re-embedding everything",
                        stored_dims.unwrap_or(0)
                    ));
                } else {
                    warnings.push(format!(
                        "chunks table stored {}-dimension vectors but the provider produces \
                         {live_dims}; the table was NOT recreated because this checkout shares \
                         its project ID with another one, whose vectors would be destroyed. \
                         Re-embedding will fail against the stored width until you reindex from \
                         the registered checkout.",
                        stored_dims.unwrap_or(0)
                    ));
                }
            }

            // `force` and a dimension change are the two ways a run rebuilds
            // every vector unconditionally; everything else may skip.
            let full_rebuild = force || dimension_change;

            // Dropping the chunks table is only safe when every memory is
            // about to be re-embedded — which is exactly `full_rebuild`. When
            // skipping is live, the surviving vectors ARE the result, so
            // clearing them would delete precisely what the skip predicate is
            // about to decide to keep. Nothing is leaked by not clearing:
            // `upsert_chunks_batch` replaces a memory's chunks atomically and
            // drops surplus ones, and the metadata rebuild above prunes chunks
            // orphaned by deleted memories.
            //
            // Suppressed under a checkout conflict either way: the other
            // clone's vectors live in this same table and must survive.
            if full_rebuild && foreign_checkout.is_none() {
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
            // Drop the memories whose vectors are provably already current.
            //
            // The predicate is a digest comparison, nothing more: the digest
            // stored beside a memory's vectors covers its chunk texts, the
            // model id, the dimensions, the composition and the chunk width,
            // so an equal digest means re-embedding would produce byte-for-byte
            // the same vectors. Anything that changes the answer changes the
            // digest — edit the text, switch the model, retune `max_tokens`,
            // flip `metadata_vector` — and the memory is embedded again.
            //
            // Deliberately NOT also requiring `has_embedding`: the presence of
            // a digest entry already covers the provider-was-down case and is
            // atomic with the vectors it describes, whereas `has_embedding` is
            // a separate commit that can lag.
            //
            // Read outside the write lock, which is safe in the only direction
            // that matters: a memory updated between this read and the write is
            // caught by `upsert_chunks_batch`'s re-read under the lock, so the
            // worst case is embedding something that did not need it.
            if !full_rebuild {
                let stored = store.embed_digests().await?;
                let before = to_embed.len();
                to_embed.retain(|memory| {
                    match (
                        stored.get(&memory.id).and_then(Option::as_deref),
                        engine.expected_embed_digest(memory),
                    ) {
                        // Current — leave the vectors exactly as they are.
                        (Some(recorded), Some(expected)) => recorded != expected,
                        // No digest recorded: vectors of unknown provenance, or
                        // none at all. Either way the only way to know is to
                        // rebuild them.
                        _ => true,
                    }
                });
                skipped = before - to_embed.len();
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
        rows_skipped,
        skipped,
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

    /// Deterministic provider that derives each vector from the text and
    /// counts how many texts it was asked to embed.
    ///
    /// Both properties are load-bearing. The count is what proves a skip
    /// actually skipped rather than re-embedding to the same answer, and
    /// text-derived vectors are what make the skip-vs-force equivalence test
    /// mean something — with a constant vector, every possible implementation
    /// passes it.
    struct CountingProvider {
        calls: Arc<std::sync::atomic::AtomicUsize>,
        model: String,
        dims: usize,
        max_tokens: usize,
    }

    impl CountingProvider {
        fn new() -> Self {
            Self {
                calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                model: "onnx/stub".to_string(),
                dims: 384,
                max_tokens: 256,
            }
        }
        fn with_model(mut self, model: &str) -> Self {
            self.model = model.to_string();
            self
        }
        fn with_dims(mut self, dims: usize) -> Self {
            self.dims = dims;
            self
        }
        fn counter(&self) -> Arc<std::sync::atomic::AtomicUsize> {
            Arc::clone(&self.calls)
        }
        fn vector_for(&self, text: &str) -> Vec<f32> {
            // Cheap deterministic spread so different texts differ.
            let mut seed: u64 = 1469598103934665603;
            for b in text.as_bytes() {
                seed ^= *b as u64;
                seed = seed.wrapping_mul(1099511628211);
            }
            (0..self.dims)
                .map(|i| (((seed >> (i % 32)) & 0xff) as f32) / 255.0)
                .collect()
        }
    }

    #[async_trait]
    impl EmbeddingProvider for CountingProvider {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.vector_for(text))
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.calls
                .fetch_add(texts.len(), std::sync::atomic::Ordering::SeqCst);
            Ok(texts.iter().map(|t| self.vector_for(t)).collect())
        }
        fn dimensions(&self) -> usize {
            self.dims
        }
        fn max_tokens(&self) -> usize {
            self.max_tokens
        }
        fn model_id(&self) -> String {
            self.model.clone()
        }
    }

    fn calls(c: &Arc<std::sync::atomic::AtomicUsize>) -> usize {
        c.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A store with `n` memories, plus a handle to open engines against it.
    async fn seeded_store(tmp: &TempDir, n: usize) -> (MemoryStore, Vec<String>) {
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let mut ids = Vec::new();
        for i in 0..n {
            let m = Memory::new(
                MemoryType::Decision,
                format!("summary {i}"),
                format!("content number {i}"),
                Provenance::human(),
            );
            ids.push(m.id.clone());
            store.create(&m).await.unwrap();
        }
        (store, ids)
    }

    fn engine_with(store: MemoryStore, provider: CountingProvider) -> RetrievalEngine {
        RetrievalEngine::new(store, EngramConfig::default())
            .with_embedding_provider(Arc::new(provider))
    }

    fn engine_with_config(
        store: MemoryStore,
        config: EngramConfig,
        provider: CountingProvider,
    ) -> RetrievalEngine {
        RetrievalEngine::new(store, config).with_embedding_provider(Arc::new(provider))
    }

    // ===================================================================
    // Phase 2 — skipping the re-embed when the vectors are already current
    // ===================================================================

    /// The headline: a second reindex over an unchanged store embeds nothing.
    #[tokio::test]
    async fn reindex_skips_unchanged_memories() {
        let tmp = TempDir::new().unwrap();
        let (store, ids) = seeded_store(&tmp, 3).await;

        let first = CountingProvider::new();
        let first_calls = first.counter();
        let engine = engine_with(MemoryStore::open(tmp.path()).await.unwrap(), first);
        let r1 = reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();
        assert_eq!(r1.embedded, 3);
        assert_eq!(r1.skipped, 0, "nothing to skip on a cold store");
        assert!(first_calls.load(std::sync::atomic::Ordering::SeqCst) > 0);

        let second = CountingProvider::new();
        let second_calls = second.counter();
        let engine2 = engine_with(MemoryStore::open(tmp.path()).await.unwrap(), second);
        let r2 = reindex(&store, Some(&engine2), ReindexOptions::default())
            .await
            .unwrap();

        assert_eq!(r2.skipped, 3, "every memory's vectors were already current");
        assert_eq!(r2.embedded, 0);
        assert_eq!(
            calls(&second_calls),
            0,
            "the provider must not be called at all — a skip that still embeds is not a skip"
        );
        // And the vectors are still there.
        for id in &ids {
            assert!(!store.export_chunks(id).await.unwrap().is_empty());
        }
    }

    /// Only the memory whose text changed is re-embedded; its neighbours are
    /// left alone.
    #[tokio::test]
    async fn reindex_reembeds_only_the_changed_memory() {
        let tmp = TempDir::new().unwrap();
        let (store, ids) = seeded_store(&tmp, 3).await;
        let engine = engine_with(
            MemoryStore::open(tmp.path()).await.unwrap(),
            CountingProvider::new(),
        );
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

        store
            .update(
                &ids[1],
                crate::types::MemoryUpdate {
                    content: Some("entirely different content".to_string()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let provider = CountingProvider::new();
        let counter = provider.counter();
        let engine2 = engine_with(MemoryStore::open(tmp.path()).await.unwrap(), provider);
        let r = reindex(&store, Some(&engine2), ReindexOptions::default())
            .await
            .unwrap();

        assert_eq!(r.embedded, 1, "only the edited memory");
        assert_eq!(r.skipped, 2);
        assert!(
            calls(&counter) > 0,
            "the edited memory must actually reach the provider"
        );
    }

    /// The model id is part of the digest, so swapping models re-embeds
    /// everything without anyone having to notice the swap.
    #[tokio::test]
    async fn reindex_reembeds_when_model_changes() {
        let tmp = TempDir::new().unwrap();
        let (store, _) = seeded_store(&tmp, 2).await;
        let engine = engine_with(
            MemoryStore::open(tmp.path()).await.unwrap(),
            CountingProvider::new().with_model("onnx/model-a"),
        );
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

        let engine2 = engine_with(
            MemoryStore::open(tmp.path()).await.unwrap(),
            CountingProvider::new().with_model("onnx/model-b"),
        );
        let r = reindex(&store, Some(&engine2), ReindexOptions::default())
            .await
            .unwrap();
        assert_eq!(r.embedded, 2, "a different model invalidates every vector");
        assert_eq!(r.skipped, 0);
    }

    /// `metadata_vector` changes what text is embedded, so flipping it must
    /// re-embed even though the memories are untouched.
    #[tokio::test]
    async fn reindex_reembeds_when_composition_flips() {
        let tmp = TempDir::new().unwrap();
        let (store, _) = seeded_store(&tmp, 2).await;

        let mut cfg = EngramConfig::default();
        cfg.embeddings.metadata_vector = true;
        let engine = engine_with_config(
            MemoryStore::open(tmp.path()).await.unwrap(),
            cfg.clone(),
            CountingProvider::new(),
        );
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

        cfg.embeddings.metadata_vector = false;
        let engine2 = engine_with_config(
            MemoryStore::open(tmp.path()).await.unwrap(),
            cfg,
            CountingProvider::new(),
        );
        let r = reindex(&store, Some(&engine2), ReindexOptions::default())
            .await
            .unwrap();
        assert_eq!(r.embedded, 2, "composition change must invalidate vectors");
        assert_eq!(r.skipped, 0);
    }

    /// `max_tokens` changes the chunk boundaries — the axis the store-level
    /// embedding fingerprint does not capture at all.
    #[tokio::test]
    async fn reindex_reembeds_when_chunk_tokens_change() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        // Content long enough that the chunk width actually changes the split.
        let body = "lorem ipsum dolor sit amet ".repeat(200);
        let m = Memory::new(MemoryType::Decision, "long", &body, Provenance::human());
        store.create(&m).await.unwrap();

        let mut cfg = EngramConfig::default();
        cfg.embeddings.max_tokens = 256;
        let engine = engine_with_config(
            MemoryStore::open(tmp.path()).await.unwrap(),
            cfg.clone(),
            CountingProvider::new(),
        );
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

        cfg.embeddings.max_tokens = 64;
        let engine2 = engine_with_config(
            MemoryStore::open(tmp.path()).await.unwrap(),
            cfg,
            CountingProvider::new(),
        );
        let r = reindex(&store, Some(&engine2), ReindexOptions::default())
            .await
            .unwrap();
        assert_eq!(
            r.embedded, 1,
            "a different chunk width produces different vectors and must not be skipped"
        );
        assert_eq!(r.skipped, 0);
    }

    /// A memory that never got vectors has no digest, and unknown provenance
    /// falls through to "rebuild" rather than to "skip".
    #[tokio::test]
    async fn reindex_reembeds_memory_with_no_vectors() {
        let tmp = TempDir::new().unwrap();
        let (store, ids) = seeded_store(&tmp, 2).await;
        let engine = engine_with(
            MemoryStore::open(tmp.path()).await.unwrap(),
            CountingProvider::new(),
        );
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

        // Drop one memory's vectors, leaving the other's intact.
        store.delete_chunks(&ids[0]).await.unwrap();

        let engine2 = engine_with(
            MemoryStore::open(tmp.path()).await.unwrap(),
            CountingProvider::new(),
        );
        let r = reindex(&store, Some(&engine2), ReindexOptions::default())
            .await
            .unwrap();
        assert_eq!(r.embedded, 1, "the memory with no vectors is rebuilt");
        assert_eq!(r.skipped, 1, "the untouched one is still skipped");
        assert!(!store.export_chunks(&ids[0]).await.unwrap().is_empty());
    }

    /// `--force` is the repair: it embeds everything, skipping nothing.
    #[tokio::test]
    async fn reindex_force_reembeds_everything() {
        let tmp = TempDir::new().unwrap();
        let (store, _) = seeded_store(&tmp, 3).await;
        let engine = engine_with(
            MemoryStore::open(tmp.path()).await.unwrap(),
            CountingProvider::new(),
        );
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

        let provider = CountingProvider::new();
        let counter = provider.counter();
        let engine2 = engine_with(MemoryStore::open(tmp.path()).await.unwrap(), provider);
        let r = reindex(
            &store,
            Some(&engine2),
            ReindexOptions {
                force: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(r.embedded, 3);
        assert_eq!(r.skipped, 0, "force must not skip anything");
        assert!(calls(&counter) > 0);
    }

    /// A dimension change forces the full path in both branches: the chunks
    /// table is recreated at the new width, so nothing can be skipped even
    /// though skipping was not disabled.
    #[tokio::test]
    async fn reindex_with_dimension_change_ignores_skips_and_recreates_table() {
        let tmp = TempDir::new().unwrap();
        let (store, _) = seeded_store(&tmp, 2).await;
        let engine = engine_with(
            MemoryStore::open(tmp.path()).await.unwrap(),
            CountingProvider::new().with_dims(384),
        );
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();
        assert_eq!(store.chunks_table_dimensions().await.unwrap(), Some(384));

        // The store's own `config.toml` is what fixes the LanceDB column
        // width (`LanceIndex::new(.., config.embeddings.dimensions)`), so a
        // real dimension change edits that file. Changing only the engine's
        // in-memory config would leave the table at 384 and every upsert would
        // fail — which is precisely the state this code path exists to repair,
        // so the fixture has to reproduce the user's action, not just the
        // engine's view of it.
        let config_path = tmp.path().join(".engramdb").join("config.toml");
        // Serialized from a real `EngramConfig` rather than hand-written.
        // Appending a second `[embeddings]` section is a TOML duplicate-key
        // error, and a hand-written minimal one omits `provider`/`max_tokens`,
        // which carry no serde default — both parse-fail, and `open` logs the
        // failure and silently falls back to the 384-dimension default, so the
        // test fails for a reason unrelated to the code under test.
        let mut on_disk = EngramConfig::default();
        on_disk.embeddings.dimensions = 128;
        tokio::fs::write(&config_path, toml::to_string(&on_disk).unwrap())
            .await
            .unwrap();
        // Reopen so the store picks up the new width.
        let store = MemoryStore::open(tmp.path()).await.unwrap();

        let mut cfg = EngramConfig::default();
        cfg.embeddings.dimensions = 128;
        let engine2 = engine_with_config(
            MemoryStore::open(tmp.path()).await.unwrap(),
            cfg,
            CountingProvider::new().with_dims(128),
        );
        let r = reindex(&store, Some(&engine2), ReindexOptions::default())
            .await
            .unwrap();

        assert_eq!(r.skipped, 0, "a dimension change cannot skip anything");
        assert_eq!(
            r.embedded, 2,
            "errors={:?} warnings={:?}",
            r.errors, r.warnings
        );
        assert_eq!(
            store.chunks_table_dimensions().await.unwrap(),
            Some(128),
            "the table must be recreated at the new width"
        );
        assert!(
            r.warnings.iter().any(|w| w.contains("dimension")),
            "the recreation must be reported: {:?}",
            r.warnings
        );
    }

    /// **The gate.** Skipping is only sound if it produces the same store a
    /// full rebuild would. Same seed, two paths, byte-identical vectors.
    #[tokio::test]
    async fn reindex_skip_matches_force_result() {
        let build = |force: bool| async move {
            let tmp = TempDir::new().unwrap();
            let (store, ids) = seeded_store(&tmp, 4).await;
            let engine = engine_with(
                MemoryStore::open(tmp.path()).await.unwrap(),
                CountingProvider::new(),
            );
            // First pass populates vectors on both arms identically.
            reindex(&store, Some(&engine), ReindexOptions::default())
                .await
                .unwrap();
            // Change one memory so the two arms have real work to do.
            store
                .update(
                    &ids[2],
                    crate::types::MemoryUpdate {
                        content: Some("changed body".to_string()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let engine2 = engine_with(
                MemoryStore::open(tmp.path()).await.unwrap(),
                CountingProvider::new(),
            );
            reindex(
                &store,
                Some(&engine2),
                ReindexOptions {
                    force,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

            let mut vectors = Vec::new();
            for id in &ids {
                vectors.push(store.export_chunks(id).await.unwrap());
            }
            // TempDir must outlive the reads.
            drop(tmp);
            vectors
        };

        let skipped = build(false).await;
        let forced = build(true).await;
        assert_eq!(
            skipped, forced,
            "a skipping reindex must leave exactly the store a forced one would"
        );
    }

    /// Per-provider-failure resumability: the memories that embedded cleanly
    /// keep their vectors and are skipped on the retry, so a retry after a
    /// provider outage does not start from scratch.
    ///
    /// Not crash-resumability — `ops::reindex` performs one batched write at
    /// the end, so a crash before it writes nothing. That would need the embed
    /// loop chunked, which is a separate change.
    #[tokio::test]
    async fn reindex_resumes_after_provider_failure() {
        let tmp = TempDir::new().unwrap();
        let (store, _) = seeded_store(&tmp, 2).await;

        // A run where the provider is down leaves no vectors and no digests.
        let failing = RetrievalEngine::new(
            MemoryStore::open(tmp.path()).await.unwrap(),
            EngramConfig::default(),
        )
        .with_embedding_provider(Arc::new(FailingEmbeddingProvider));
        let failed = reindex(&store, Some(&failing), ReindexOptions::default())
            .await
            .unwrap();
        assert_eq!(failed.embedded, 0);
        assert_eq!(failed.skipped, 0, "nothing was current, so nothing skipped");

        // The provider recovers: everything is embedded, nothing skipped.
        let engine = engine_with(
            MemoryStore::open(tmp.path()).await.unwrap(),
            CountingProvider::new(),
        );
        let recovered = reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();
        assert_eq!(recovered.embedded, 2);
        assert_eq!(recovered.skipped, 0);

        // A third run now has nothing to do.
        let engine2 = engine_with(
            MemoryStore::open(tmp.path()).await.unwrap(),
            CountingProvider::new(),
        );
        let settled = reindex(&store, Some(&engine2), ReindexOptions::default())
            .await
            .unwrap();
        assert_eq!(settled.skipped, 2);
        assert_eq!(settled.embedded, 0);
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

        let result = reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

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

        let err = reindex(
            &store,
            Some(&engine),
            ReindexOptions {
                embeddings_only: true,
                ..Default::default()
            },
        )
        .await
        .expect_err("embeddings-only without a provider must fail fast");
        assert!(
            err.to_string().contains("embedding provider unavailable"),
            "error must explain the refusal, got: {err}"
        );

        // No engine at all (e.g. embeddings_only combined with index_only)
        // must fail the same way.
        assert!(reindex(
            &store,
            None,
            ReindexOptions {
                embeddings_only: true,
                ..Default::default()
            }
        )
        .await
        .is_err());

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

        let result = reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

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

        let result = reindex(&store_b, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

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

        let result = reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

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

    /// **Review finding.** A file that was never indexed must be reported.
    ///
    /// `on_disk` and `not_indexed` were both derived from `store.list_ids()`,
    /// which reads the LanceDB table — the same table the rows come from. The
    /// two sets were therefore identical by construction: `not_indexed` could
    /// never be non-empty, `on_disk` was a row count wearing a file count's
    /// name, and the single loudest thing a dry run should report was
    /// structurally invisible.
    #[tokio::test]
    async fn dry_run_reports_a_file_that_was_never_indexed() {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let indexed = Memory::new(MemoryType::Decision, "indexed", "C", Provenance::human());
        store.create(&indexed).await.unwrap();

        // A memory file on disk that the index has never seen: written
        // directly, exactly as a `git checkout` of a colleague's memory would
        // arrive.
        let orphan = Memory::new(MemoryType::Decision, "unindexed", "C", Provenance::human());
        let text = crate::storage::memory_file::latest_writer()
            .write(&orphan)
            .unwrap();
        let dir = crate::storage::paths::memories_dir(tmp.path());
        tokio::fs::write(dir.join(format!("{}.md", orphan.id)), text)
            .await
            .unwrap();

        let plan = reindex_dry_run(&store, None).await.unwrap();
        assert_eq!(plan.on_disk, 2, "on_disk must count FILES: {plan:?}");
        assert_eq!(
            plan.not_indexed,
            vec![orphan.id.clone()],
            "the unindexed file must be named: {plan:?}"
        );
        assert!(!plan.is_current());
    }

    /// **Review finding.** With no provider the vector half never ran, so
    /// neither list can mean "nothing wrong" — reporting `current` would be a
    /// clean bill of health for a check that did not happen.
    #[tokio::test]
    async fn dry_run_is_not_current_when_vectors_could_not_be_checked() {
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
        store.reindex().await.unwrap();

        let plan = reindex_dry_run(&store, None).await.unwrap();
        assert!(plan.embeddings_unavailable);
        assert!(plan.drifted.is_empty(), "content really is current");
        assert!(
            !plan.is_current(),
            "an unchecked vector half must not read as current: {plan:?}"
        );
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
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

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
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();

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
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();
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
        reindex(&store, Some(&engine), ReindexOptions::default())
            .await
            .unwrap();
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
