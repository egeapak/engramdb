use super::*;
use crate::scoring::{composite_score, composite_score_ignore_decay};
use crate::storage::InMemoryRegistry;

// Finding #4: the Filter-mode threshold must be bounded to [0, 1] regardless
// of how the config was constructed, so the relevance gate can never be
// silently disabled (negative) or turned into a filter-everything NaN.
#[test]
fn the_ingest_query_is_the_unit_mean_of_the_chunks() {
    assert_eq!(mean_direction(&[]), None);
    assert_eq!(mean_direction(&[vec![3.0, 4.0]]), Some(vec![3.0, 4.0]));
    // Each chunk weighs the same whatever its norm; the result is unit.
    let m = mean_direction(&[vec![10.0, 0.0], vec![0.0, 1.0]]).unwrap();
    assert!((m[0] - m[1]).abs() < 1e-6);
    assert!((m[0] * m[0] + m[1] * m[1] - 1.0).abs() < 1e-6);
    assert_eq!(mean_direction(&[vec![1.0, 0.0], vec![-1.0, 0.0]]), None);
}

#[test]
fn filter_threshold_is_bounded() {
    // POSITIVE: in-range values pass through unchanged.
    assert_eq!(filter_threshold(0.0), 0.0);
    assert_eq!(filter_threshold(0.3), 0.3);
    assert_eq!(filter_threshold(1.0), 1.0);
    // POSITIVE: >1.0 still clamps to 1.0 (unchanged from before).
    assert_eq!(filter_threshold(5.0), 1.0);
    // NEGATIVE (red before fix): a negative must clamp to 0.0, not stay
    // negative (which left the gate disabled).
    assert_eq!(filter_threshold(-0.5), 0.0);
    // NEGATIVE (red before fix): NaN must become 0.0, not propagate.
    assert_eq!(filter_threshold(f64::NAN), 0.0);
}

/// Stub embedding: every text maps to the same unit vector, so any two
/// memories are cosine-1.0 neighbours. No model load.
struct ConstantEmbedding;

#[async_trait::async_trait]
impl EmbeddingProvider for ConstantEmbedding {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        let mut v = vec![0.0f32; 384];
        v[0] = 1.0;
        Ok(v)
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }
    fn dimensions(&self) -> usize {
        384
    }
    fn max_tokens(&self) -> usize {
        256
    }
    fn model_id(&self) -> String {
        "onnx/constant-stub".to_string()
    }
}

/// [`ConstantEmbedding`] with a configurable identity, for asserting that
/// the embed digest is salted by model id and vector width.
struct IdentityEmbedding {
    model: &'static str,
    dims: usize,
}

#[async_trait::async_trait]
impl EmbeddingProvider for IdentityEmbedding {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![0.5f32; self.dims])
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.5f32; self.dims]).collect())
    }
    fn dimensions(&self) -> usize {
        self.dims
    }
    fn max_tokens(&self) -> usize {
        256
    }
    fn model_id(&self) -> String {
        self.model.to_string()
    }
}

/// The embed digest must move when ANY of its four salt components moves —
/// the text, the model, the vector width, the composition, or the chunk
/// budget. Each assertion here is one way stored vectors go stale, and the
/// last is the one the store-wide `EmbeddingFingerprint` does not cover.
#[test]
fn embed_digest_is_salted_by_every_input() {
    let base_texts = vec!["alpha".to_string(), "beta".to_string()];
    let p = IdentityEmbedding {
        model: "onnx/a",
        dims: 384,
    };
    let base = embed_digest(&base_texts, &p, true, 256);

    // Stable for identical inputs — otherwise nothing could ever be skipped.
    assert_eq!(base, embed_digest(&base_texts, &p, true, 256));

    // 1. Text.
    let other_texts = vec!["alpha".to_string(), "GAMMA".to_string()];
    assert_ne!(base, embed_digest(&other_texts, &p, true, 256));

    // 2. Model identity.
    let other_model = IdentityEmbedding {
        model: "onnx/b",
        dims: 384,
    };
    assert_ne!(base, embed_digest(&base_texts, &other_model, true, 256));

    // 3. Vector width.
    let other_dims = IdentityEmbedding {
        model: "onnx/a",
        dims: 768,
    };
    assert_ne!(base, embed_digest(&base_texts, &other_dims, true, 256));

    // 4. Composition (metadata_vector on/off).
    assert_ne!(base, embed_digest(&base_texts, &p, false, 256));

    // 5. Chunk budget — the axis `EmbeddingFingerprint` misses entirely, so
    // without this a `[embeddings].max_tokens` change would re-chunk with
    // nothing detecting it.
    assert_ne!(base, embed_digest(&base_texts, &p, true, 128));
}

/// Field boundaries must not be forgeable by content: two different
/// (model, texts) splits that concatenate to the same bytes must still
/// digest differently.
#[test]
fn embed_digest_field_boundaries_are_unambiguous() {
    let p = IdentityEmbedding {
        model: "onnx/a",
        dims: 384,
    };
    let one = embed_digest(&["x".to_string(), "y".to_string()], &p, true, 256);
    let two = embed_digest(&["xy".to_string()], &p, true, 256);
    assert_ne!(one, two, "chunk boundaries must be part of the identity");
}

/// Stub NLI: everything is a maximal contradiction. No model load.
struct AlwaysContradicts;

#[async_trait::async_trait]
impl NliProvider for AlwaysContradicts {
    async fn classify(
        &self,
        _premise: &str,
        _hypothesis: &str,
    ) -> anyhow::Result<crate::nli::NliResult> {
        Ok(crate::nli::NliResult {
            label: crate::nli::NliLabel::Contradiction,
            entailment: 0.0,
            neutral: 0.0,
            contradiction: 1.0,
        })
    }
    async fn classify_batch(
        &self,
        pairs: &[(&str, &str)],
    ) -> anyhow::Result<Vec<crate::nli::NliResult>> {
        let mut out = Vec::with_capacity(pairs.len());
        for _ in pairs {
            out.push(self.classify("", "").await?);
        }
        Ok(out)
    }
}

/// §14.14: invalidated memories are excluded from the NLI contradiction
/// candidate set. Deterministic stub providers: the existing memory is a
/// perfect vector neighbour and the NLI stub contradicts everything, so
/// the ONLY thing standing between the probe and a challenge is the
/// invalidated-window gate — without it, every create would re-challenge
/// its just-superseded predecessor.
#[tokio::test]
async fn nli_candidates_exclude_invalidated_memories() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let existing = Memory::new(
        MemoryType::Decision,
        "The cache is enabled",
        "body",
        Provenance::human(),
    );
    store.create(&existing).await.unwrap();

    let embed: Arc<dyn EmbeddingProvider> = Arc::new(ConstantEmbedding);
    embed_memory_with(
        embed.as_ref(),
        &store,
        &existing,
        embed.max_tokens(),
        true,
        None,
    )
    .await
    .unwrap();

    let mut config = EngramConfig::default();
    config.nli.enabled = true;
    config.nli.similarity_threshold = 0.0;
    config.nli.contradiction_threshold = 0.5;

    let engine = RetrievalEngine::new(store.clone(), config)
        .with_embedding_provider(Arc::clone(&embed))
        .with_nli_provider(Arc::new(AlwaysContradicts));

    let probe = Memory::new(
        MemoryType::Decision,
        "The cache is disabled",
        "body",
        Provenance::human(),
    );

    // Control: while the existing memory is live, it IS flagged.
    let live = engine.detect_contradictions(&probe).await.unwrap();
    assert_eq!(live.len(), 1, "live neighbour must be flagged");
    assert_eq!(live[0].0, existing.id);

    // Close the window: the same neighbour must vanish from candidates.
    store
        .invalidate_with(&existing.id, None, chrono::Utc::now())
        .await
        .unwrap();
    let after = engine.detect_contradictions(&probe).await.unwrap();
    assert!(
        after.is_empty(),
        "invalidated memory leaked into NLI candidates: {after:?}"
    );
}

// Finding #19: `config.embeddings.max_tokens` is now authoritative for
// chunking (previously dead config — chunking only used the provider spec).
#[test]
fn effective_chunk_tokens_respects_config_capped_at_model() {
    assert_eq!(effective_chunk_tokens(128, 256), 128); // smaller is honoured
    assert_eq!(effective_chunk_tokens(1000, 256), 256); // never exceeds model
    assert_eq!(effective_chunk_tokens(0, 256), 1); // floored at 1
}

fn mem_for_texts(title: Option<&str>, summary: &str, content: &str, tags: &[&str]) -> Memory {
    let mut m = Memory::new(
        crate::types::MemoryType::Context,
        summary,
        content,
        crate::types::Provenance::human(),
    );
    m.title = title.map(str::to_string);
    m.tags = tags.iter().map(|t| t.to_string()).collect();
    m
}

#[test]
fn embedding_texts_metadata_row_plus_content_chunks() {
    let m = mem_for_texts(
        Some("JWT refresh rotation"),
        "Refresh tokens rotate on use",
        "The auth service rotates refresh tokens on every use.",
        &["jwt", "auth"],
    );
    let texts = embedding_texts(&m, 256, true);
    assert_eq!(texts.len(), 2, "metadata row + one content chunk");
    assert_eq!(
        texts[0],
        "JWT refresh rotation. Refresh tokens rotate on use. tags: jwt, auth"
    );
    assert_eq!(
        texts[1],
        "The auth service rotates refresh tokens on every use."
    );
}

#[test]
fn embedding_texts_degrades_without_title_and_tags() {
    let m = mem_for_texts(None, "Just a summary", "Some content here.", &[]);
    let texts = embedding_texts(&m, 256, true);
    assert_eq!(texts, vec!["Just a summary", "Some content here."]);

    // Whitespace-only title and empty tag entries drop out too.
    let m = mem_for_texts(Some("  "), "Summary", "Content.", &["", "  "]);
    let texts = embedding_texts(&m, 256, true);
    assert_eq!(texts, vec!["Summary", "Content."]);
}

#[test]
fn embedding_texts_empty_memory_yields_nothing() {
    let m = mem_for_texts(None, "", "", &[]);
    assert!(embedding_texts(&m, 256, true).is_empty());
    assert!(embedding_texts(&m, 256, false).is_empty());
}

#[test]
fn embedding_texts_metadata_only_when_content_empty() {
    let m = mem_for_texts(Some("Title"), "Summary", "", &["tag"]);
    let texts = embedding_texts(&m, 256, true);
    assert_eq!(texts, vec!["Title. Summary. tags: tag"]);
}

#[test]
fn embedding_texts_legacy_composition_when_disabled() {
    let m = mem_for_texts(
        Some("Title is ignored"),
        "Summary",
        "Content body.",
        &["ignored"],
    );
    let texts = embedding_texts(&m, 256, false);
    assert_eq!(texts, vec!["Summary Content body."]);
}

#[test]
fn embedding_texts_long_content_still_chunks() {
    let long: Vec<String> = (0..400).map(|i| format!("word{i}")).collect();
    let m = mem_for_texts(Some("T"), "S", &long.join(" "), &["x"]);
    let texts = embedding_texts(&m, 256, true);
    // 1 metadata row + 400 words at 192-word blocks (runt-merged tail).
    assert!(texts.len() >= 3, "got {}", texts.len());
    assert_eq!(texts[0], "T. S. tags: x");
    assert!(texts[1].starts_with("word0"));
}

/// Shared reranker across all tests in this module to avoid loading the
/// ONNX model once per test (which causes OOM when parallel). Built through
/// the `engram-models` loader at the shipped default so the core crate needs
/// no direct `fastembed` dependency, even in tests.
///
/// The `fastembed` loader (`LocalReranker`) only exists with `onnxruntime`;
/// on a build without ONNX Runtime there is no in-process reranker, so `try_reranker`
/// returns `None` and the reranker tests skip (they `return` early on `None`).
#[cfg(feature = "onnxruntime")]
fn try_reranker() -> Option<Arc<dyn Reranker>> {
    use crate::retrieval::reranker::LocalReranker;
    use std::sync::LazyLock;
    // The cause is kept so a skip says why, and a run with
    // ENGRAMDB_TEST_REQUIRE_MODELS fails with it.
    static SHARED_RERANKER: LazyLock<std::result::Result<Arc<dyn Reranker>, String>> =
        LazyLock::new(|| {
            LocalReranker::load(crate::types::DEFAULT_RERANK_MODEL).map_err(|e| format!("{e:#}"))
        });
    engram_test_support::model_or_skip("default reranker", SHARED_RERANKER.clone())
}

#[cfg(not(feature = "onnxruntime"))]
fn try_reranker() -> Option<Arc<dyn Reranker>> {
    engram_test_support::model_or_skip(
        "default reranker",
        Err::<Arc<dyn Reranker>, _>("built without the onnxruntime feature"),
    )
}

/// Filter-mode query with only a text query set, mirroring the shape
/// of the old keyword-search API used by these tests.
async fn filter_query(engine: &RetrievalEngine, query_text: &str) -> Vec<ScoredMemory> {
    let query = RetrievalQuery {
        mode: RetrievalMode::Filter,
        query: Some(query_text.to_string()),
        ..Default::default()
    };
    engine.query(&query).await.unwrap().memories
}

/// R2 fast-path equivalence: the no-query Rank path scores straight from the
/// index projection (`ScoreTarget`). Its per-memory breakdown must equal
/// re-scoring the returned (file-loaded) memory with `composite_score` — i.e.
/// projection scoring == file-load scoring — across decay, criticality,
/// logical scope, provenance, and challenged status.
#[tokio::test]
async fn rank_from_index_matches_file_load() {
    use crate::types::{Decay, EngramConfig, Memory, MemoryType, Provenance, Status, Visibility};
    use chrono::{Duration, Utc};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0; // keep all candidates

    let mut a = Memory::new(MemoryType::Decision, "A", "content a", Provenance::human());
    a.visibility = Visibility::Shared;
    a.criticality = 0.9;
    a.logical = vec!["auth.oauth".to_string()];
    store.create(&a).await.unwrap();

    let mut b = Memory::new(
        MemoryType::Context,
        "B",
        "content b",
        Provenance::agent("x"),
    );
    b.visibility = Visibility::Shared;
    b.criticality = 0.5;
    b.created_at = Utc::now() - Duration::days(7);
    b.decay = Some(Decay::exponential(Duration::days(7)));
    store.create(&b).await.unwrap();

    let mut c = Memory::new(MemoryType::Hazard, "C", "content c", Provenance::human());
    c.visibility = Visibility::Shared;
    c.criticality = 0.7;
    c.status = Status::Challenged;
    store.create(&c).await.unwrap();

    let engine = RetrievalEngine::new(store, config.clone());
    let query = RetrievalQuery {
        mode: RetrievalMode::Rank,
        query: None,
        logical: vec!["auth".to_string()],
        ..Default::default()
    };
    let results = engine.query(&query).await.unwrap();
    assert_eq!(results.memories.len(), 3, "all three memories ranked");
    assert_eq!(results.retrieval_quality, "scope_only");

    let now = Utc::now();
    let ctx_logical = vec!["auth".to_string()];
    for sm in &results.memories {
        let ctx = ScoringContext::scope_only(None, &ctx_logical);
        let expected = if sm.memory.is_expired_at(now) {
            composite_score_ignore_decay(&sm.memory, &ctx, &config, now)
        } else {
            composite_score(&sm.memory, &ctx, &config, now)
        };
        // final_score matches within decay-timing drift; the decay-free
        // components match exactly.
        assert!(
            (sm.score - expected.final_score).abs() < 1e-4,
            "projection score {} != file-load score {} for {}",
            sm.score,
            expected.final_score,
            sm.memory.summary
        );
        assert_eq!(sm.score_breakdown.criticality, expected.criticality);
        assert!((sm.score_breakdown.scope - expected.scope).abs() < 1e-9);
    }
}

/// Cross-store equalization (`rerank_merged`) is a no-op when no reranker is
/// configured: it returns `Ok(false)` and leaves the merged scores/order
/// untouched, so the caller keeps the per-store scores.
#[tokio::test]
async fn rerank_merged_is_noop_without_reranker() {
    use crate::scoring::ScoreBreakdown;
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    // A plain engine has no reranker attached (see `with_reranker`).
    let engine = RetrievalEngine::new(store, EngramConfig::default());

    let sm = |summary: &str, score: f64| ScoredMemory {
        memory: Memory::new(
            MemoryType::Decision,
            summary,
            "content",
            Provenance::human(),
        ),
        score,
        score_breakdown: ScoreBreakdown::default(),
    };
    let mut memories = vec![sm("a", 0.9), sm("b", 0.1)];
    let ran = engine
        .rerank_merged("query", &mut memories, 10)
        .await
        .unwrap();
    assert!(!ran, "no reranker configured ⇒ no-op");
    assert_eq!(memories.len(), 2);
    assert_eq!(memories[0].score, 0.9);
    assert_eq!(memories[1].score, 0.1);
}

/// Equalization must be able to *reorder* the merged union, not just no-op:
/// a memory that ranked low by its prior (incomparable) score is lifted to
/// the top when the cross-encoder scores it highest. Uses a deterministic
/// marker reranker so no model load is needed.
#[tokio::test]
async fn rerank_merged_reorders_by_cross_encoder() {
    use crate::retrieval::reranker::{RerankScore, Reranker};
    use crate::scoring::ScoreBreakdown;
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    // Scores any document containing "LIFTME" far above the rest.
    struct MarkerReranker;
    #[async_trait::async_trait]
    impl Reranker for MarkerReranker {
        async fn rerank(
            &self,
            _query: &str,
            documents: &[String],
        ) -> anyhow::Result<Vec<RerankScore>> {
            Ok(documents
                .iter()
                .enumerate()
                .map(|(index, d)| RerankScore {
                    index,
                    score: if d.contains("LIFTME") { 12.0 } else { -12.0 },
                })
                .collect())
        }
    }

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut config = EngramConfig::default();
    config.rerank.enabled = true;
    config.rerank.weight = 1.0; // fully trust the cross-encoder for a clean assert
    let engine =
        RetrievalEngine::new(store, config).with_reranker(std::sync::Arc::new(MarkerReranker));

    let sm = |summary: &str, score: f64| ScoredMemory {
        memory: Memory::new(
            MemoryType::Decision,
            summary,
            "content",
            Provenance::human(),
        ),
        score,
        score_breakdown: ScoreBreakdown::default(),
    };
    // The marked memory has the LOWEST prior score, so a no-op rerank would
    // leave it last.
    let mut memories = vec![
        sm("alpha", 0.9),
        sm("beta", 0.8),
        sm("gamma LIFTME winner", 0.1),
    ];
    let ran = engine
        .rerank_merged("query", &mut memories, 10)
        .await
        .unwrap();
    assert!(ran, "a configured reranker must run");
    assert!(
        memories[0].memory.summary.contains("LIFTME"),
        "the cross-encoder must lift the marked memory to the top despite its low prior score; \
             got order: {:?}",
        memories
            .iter()
            .map(|m| &m.memory.summary)
            .collect::<Vec<_>>()
    );
    // Truncation to `max` still applies (here max=10 > len, so all kept).
    assert_eq!(memories.len(), 3);
}

/// The R2 fast path must honor Step 1.5's empty-path collapse: a no-query
/// Rank with `path: ""` (the natural MCP/hook degenerate) must rank like
/// `path: None`, not zero every scope multiplier and return nothing.
#[tokio::test]
async fn rank_from_index_empty_path_ranks_like_no_path() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut m = Memory::new(MemoryType::Decision, "D", "content", Provenance::human());
    m.visibility = Visibility::Shared;
    m.criticality = 0.9;
    m.physical = vec!["src/**".to_string()];
    store.create(&m).await.unwrap();

    let engine = RetrievalEngine::new(store, EngramConfig::default());
    let base = RetrievalQuery {
        mode: RetrievalMode::Rank,
        query: None,
        ..Default::default()
    };
    let no_path = engine.query(&base).await.unwrap();
    assert_eq!(no_path.retrieval_quality, "scope_only");
    assert_eq!(no_path.memories.len(), 1);

    let empty_path = engine
        .query(&RetrievalQuery {
            path: Some(String::new()),
            ..base
        })
        .await
        .unwrap();
    assert_eq!(empty_path.retrieval_quality, "scope_only");
    assert_eq!(
        empty_path.memories.len(),
        1,
        "empty-string path must not zero the scope multiplier on the fast path"
    );
    assert_eq!(no_path.memories[0].score, empty_path.memories[0].score);
}

/// A fresh store must be stamped with the live provider's fingerprint by
/// its FIRST embed. Only `ops::reindex` ever stamped, so brand-new
/// projects stayed Untracked forever and every MCP query prescribed a
/// pointless `reindex --embeddings-only` ("legacy store").
#[tokio::test]
async fn first_embed_stamps_embedding_fingerprint() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    assert!(store.embedding_fingerprint().await.unwrap().is_none());

    let memory = stub_memory("first ever memory", 0.5);
    store.create(&memory).await.unwrap();
    let engine = RetrievalEngine::new(store.clone(), EngramConfig::default())
        .with_embedding_provider(Arc::new(MarkerEmbeddingProvider));
    engine.embed_memory(&memory).await.unwrap();

    let fp = store
        .embedding_fingerprint()
        .await
        .unwrap()
        .expect("first embed must stamp the fingerprint");
    assert_eq!(fp.model, "stub/marker");
    assert_eq!(fp.dimensions, 384);
    // The default config embeds the metadata-vector composition; the
    // stamp must record it so a later flag flip is detected.
    assert_eq!(
        fp.composition.as_deref(),
        Some(crate::storage::manifest::COMPOSITION_METADATA_V1)
    );
}

/// The `metadata_vector` flag must flow from config through
/// `embed_memory` to the actual embedded texts — off yields the single
/// legacy `"{summary} {content}"` text, on yields metadata row +
/// content chunks. Pins the config plumbing, not just the pure helper.
#[tokio::test]
async fn embed_memory_composition_follows_config_flag() {
    use crate::types::EngramConfig;
    use std::sync::Mutex;
    use tempfile::TempDir;

    struct CapturingProvider(Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl EmbeddingProvider for CapturingProvider {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(vec![0.5f32; 384])
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            let mut captured = self.0.lock().unwrap();
            for t in texts {
                captured.push((*t).to_string());
            }
            Ok(vec![vec![0.5f32; 384]; texts.len()])
        }
        fn dimensions(&self) -> usize {
            384
        }
        fn max_tokens(&self) -> usize {
            256
        }
        fn model_id(&self) -> String {
            "stub/capturing".to_string()
        }
    }

    let mut memory = stub_memory("summary here", 0.5);
    memory.title = Some("A Title".to_string());
    memory.content = "content body".to_string();
    memory.tags = vec!["alpha".to_string(), "beta".to_string()];

    for (flag, expected) in [
        (false, vec!["summary here content body".to_string()]),
        (
            true,
            vec![
                "A Title. summary here. tags: alpha, beta".to_string(),
                "content body".to_string(),
            ],
        ),
    ] {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        store.create(&memory).await.unwrap();

        let provider = Arc::new(CapturingProvider(Mutex::new(Vec::new())));
        let mut config = EngramConfig::default();
        config.embeddings.metadata_vector = flag;
        let engine = RetrievalEngine::new(store.clone(), config)
            .with_embedding_provider(Arc::clone(&provider) as Arc<dyn EmbeddingProvider>);
        engine.embed_memory(&memory).await.unwrap();

        let captured = provider.0.lock().unwrap().clone();
        assert_eq!(captured, expected, "metadata_vector = {flag}");
        let chunks = store.export_chunks(&memory.id).await.unwrap();
        assert_eq!(chunks.len(), expected.len(), "metadata_vector = {flag}");
    }
}

/// The reranker document must carry the same title/tag signal the
/// metadata vector embeds — otherwise candidates surfaced via that
/// vector are re-scored on text without the matched terms and get
/// systematically demoted.
#[test]
fn rerank_document_includes_title_and_tags() {
    let mut m = mem_for_texts(
        Some("JWT rotation"),
        "Tokens rotate",
        "The service rotates credentials.",
        &["jwt", "auth"],
    );
    m.details = Some("Extra details.".to_string());
    assert_eq!(
            rerank_document(&m),
            "JWT rotation. Tokens rotate The service rotates credentials. Extra details. tags: jwt, auth"
        );

    // Absent title/tags degrade to the legacy summary+content(+details).
    let plain = mem_for_texts(None, "Sum", "Body.", &[]);
    assert_eq!(rerank_document(&plain), "Sum Body.");
}

/// A genuinely legacy store — unstamped but holding vectors of unknown
/// model vintage — must NOT be stamped by a later embed: the Untracked
/// warning has to keep pointing at the real remediation (reindex).
#[tokio::test]
async fn embed_leaves_legacy_store_unstamped() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // Pre-existing vectors with no fingerprint = legacy store.
    let old = stub_memory("legacy memory", 0.5);
    store.create(&old).await.unwrap();
    store
        .upsert_chunks(&old.id, vec![vec![0.7f32; 384]])
        .await
        .unwrap();

    let new = stub_memory("new memory", 0.5);
    store.create(&new).await.unwrap();
    let engine = RetrievalEngine::new(store.clone(), EngramConfig::default())
        .with_embedding_provider(Arc::new(MarkerEmbeddingProvider));
    engine.embed_memory(&new).await.unwrap();

    assert!(
        store.embedding_fingerprint().await.unwrap().is_none(),
        "legacy vectors of unknown vintage must keep the store Untracked"
    );
}

// Note: These tests would require a real MemoryStore instance,
// which would need a temp directory setup. For now, we provide
// unit tests for the helper types.

#[test]
fn test_detail_level_default() {
    assert_eq!(DetailLevel::default(), DetailLevel::Content);
}

#[test]
fn test_retrieval_query_default() {
    let query = RetrievalQuery::default();
    assert!(query.path.is_none());
    assert!(query.query.is_none());
    assert!(query.types.is_none());
    assert_eq!(query.detail_level, DetailLevel::Content);
}

// Integration tests with real MemoryStore

#[tokio::test]
async fn test_engine_new() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();

    let engine = RetrievalEngine::new(store, config);
    assert!(!engine.embeddings_available());
}

#[tokio::test]
async fn test_retrieve_empty_store() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();

    let engine = RetrievalEngine::new(store, config);
    let query = RetrievalQuery::default();
    let result = engine.query(&query).await.unwrap();

    assert_eq!(result.memories.len(), 0);
    assert_eq!(result.total, 0);
}

#[tokio::test]
async fn test_retrieve_returns_scored_memories() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // Use a config with lower threshold to ensure memories are returned
    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    // Create two memories with different physical scopes
    let mut memory1 = Memory::new(
        MemoryType::Decision,
        "Memory at src/auth",
        "This is about authentication",
        Provenance::human(),
    );
    memory1.physical = vec!["src/auth/**".to_string()];
    memory1.visibility = Visibility::Shared;
    memory1.criticality = 0.9;

    let mut memory2 = Memory::new(
        MemoryType::Context,
        "Memory at root",
        "This is general context",
        Provenance::human(),
    );
    memory2.physical = vec!["/".to_string()];
    memory2.visibility = Visibility::Shared;
    memory2.criticality = 0.9;

    store.create(&memory1).await.unwrap();
    store.create(&memory2).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    // Retrieve with path matching memory1 more closely
    let query = RetrievalQuery {
        path: Some("src/auth/handlers.rs".to_string()),
        ..Default::default()
    };

    let result = engine.query(&query).await.unwrap();

    // Both should be returned, but the one with matching scope should score higher
    assert_eq!(result.memories.len(), 2);
    assert_eq!(result.total, 2);

    // The first (highest scoring) should be the auth memory
    assert_eq!(
        result.memories[0].memory.physical,
        vec!["src/auth/**".to_string()]
    );
    assert!(result.memories[0].score > result.memories[1].score);
}

#[tokio::test]
async fn test_rank_logical_only_returns_matches_above_threshold() {
    // Regression: a Rank query with only `logical` context (no path) used
    // to multiply by the bare logical bonus (max 0.3), so every result
    // fell below the default relevance_threshold (0.45) and the advertised
    // "rank by logical scope" feature returned nothing. Uses the DEFAULT
    // config on purpose — the threshold is the bug.
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();
    assert!(config.retrieval.relevance_threshold > 0.3); // guard the premise

    // Exact logical match: scope_mult = 0.5 + 0.3 = 0.8 → 0.9 * 0.8 = 0.72
    let mut matching = Memory::new(
        MemoryType::Decision,
        "OAuth flow decision",
        "We use PKCE for the OAuth flow",
        Provenance::human(),
    );
    matching.logical = vec!["auth.oauth".to_string()];
    matching.visibility = Visibility::Shared;
    matching.criticality = 0.9;

    // Parent logical match: scope_mult = 0.5 + 0.2 = 0.7 → 0.9 * 0.7 = 0.63
    let mut parent = Memory::new(
        MemoryType::Convention,
        "Auth conventions",
        "All auth modules follow these conventions",
        Provenance::human(),
    );
    parent.logical = vec!["auth".to_string()];
    parent.visibility = Visibility::Shared;
    parent.criticality = 0.9;

    // Unrelated logical scopes: scope_mult = 0.0 even at criticality 1.0
    let mut unrelated = Memory::new(
        MemoryType::Hazard,
        "Postgres migration hazard",
        "Do not run migrations without a backup",
        Provenance::human(),
    );
    unrelated.logical = vec!["database.postgres".to_string()];
    unrelated.visibility = Visibility::Shared;
    unrelated.criticality = 1.0;

    // No logical scopes: bare floor → 0.8 * 0.5 = 0.40 < 0.45
    let mut unscoped = Memory::new(
        MemoryType::Context,
        "General context",
        "General project context",
        Provenance::human(),
    );
    unscoped.visibility = Visibility::Shared;
    unscoped.criticality = 0.8;

    store.create(&matching).await.unwrap();
    store.create(&parent).await.unwrap();
    store.create(&unrelated).await.unwrap();
    store.create(&unscoped).await.unwrap();

    let engine = RetrievalEngine::new(store, config.clone());

    let query = RetrievalQuery {
        mode: RetrievalMode::Rank,
        logical: vec!["auth.oauth".to_string()],
        ..Default::default()
    };

    let result = engine.query(&query).await.unwrap();

    let ids: Vec<&str> = result
        .memories
        .iter()
        .map(|sm| sm.memory.id.as_str())
        .collect();
    assert!(
        ids.contains(&matching.id.as_str()),
        "exact logical match must be returned, got {ids:?}"
    );
    assert!(
        ids.contains(&parent.id.as_str()),
        "parent logical match must be returned, got {ids:?}"
    );
    assert!(
        !ids.contains(&unrelated.id.as_str()),
        "unrelated logical scopes must be filtered out, got {ids:?}"
    );
    assert!(
        !ids.contains(&unscoped.id.as_str()),
        "unscoped memory at criticality 0.8 must stay below threshold, got {ids:?}"
    );

    // Exact match ranks first and clears the threshold.
    assert_eq!(result.memories[0].memory.id, matching.id);
    for sm in &result.memories {
        assert!(
            sm.score >= config.retrieval.relevance_threshold,
            "returned memory {} scored {} below threshold",
            sm.memory.id,
            sm.score,
        );
    }
}

#[tokio::test]
async fn test_retrieve_filters_by_type() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    // Create memories of different types
    let mut decision = Memory::new(
        MemoryType::Decision,
        "A decision",
        "Decision content",
        Provenance::human(),
    );
    decision.visibility = Visibility::Shared;
    decision.criticality = 0.8;

    let mut context = Memory::new(
        MemoryType::Context,
        "Some context",
        "Context content",
        Provenance::human(),
    );
    context.visibility = Visibility::Shared;
    context.criticality = 0.8;

    store.create(&decision).await.unwrap();
    store.create(&context).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    // Filter by Decision type only
    let query = RetrievalQuery {
        types: Some(vec![MemoryType::Decision]),
        ..Default::default()
    };

    let result = engine.query(&query).await.unwrap();

    assert_eq!(result.memories.len(), 1);
    assert_eq!(result.memories[0].memory.type_, MemoryType::Decision);
}

/// The LanceDB scalar-filter pushdown must be a pure narrowing: for a mixed
/// store (multiple types, varied criticality, some expired), the id set
/// surviving `list_for_filtering_where(predicate)` + the Rust filters must
/// be byte-identical to the pre-pushdown path (`list_for_filtering()` with
/// no predicate + the same Rust filters). If the predicate ever dropped a
/// row the Rust gate would have kept, the result set — and every downstream
/// score — would change.
#[tokio::test]
async fn test_filter_pushdown_matches_rust_path() {
    use crate::retrieval::filters::build_filter_predicate;
    use crate::types::{Memory, MemoryType, Provenance, Visibility};
    use chrono::{Duration, Utc};
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // Mixed store: 3 types, criticality spanning the min bound, and one
    // expired + one active per relevant case.
    let specs: &[(MemoryType, f64, bool)] = &[
        (MemoryType::Decision, 0.95, false),
        (MemoryType::Decision, 0.30, false), // below min_criticality
        (MemoryType::Decision, 0.95, true),  // expired -> excluded by expiry
        (MemoryType::Hazard, 0.99, false),
        (MemoryType::Hazard, 0.50, false),  // below min_criticality
        (MemoryType::Context, 0.99, false), // wrong type
        (MemoryType::Convention, 0.90, false), // wrong type
    ];
    for (i, (ty, crit, expired)) in specs.iter().enumerate() {
        let mut m = Memory::new(
            *ty,
            format!("summary {i}"),
            format!("content {i}"),
            Provenance::human(),
        );
        m.visibility = Visibility::Shared;
        m.criticality = *crit;
        if *expired {
            m.expires_at = Some(Utc::now() - Duration::days(1));
        }
        store.create(&m).await.unwrap();
    }

    // The exact filter the engine would build for this query.
    let types = vec![MemoryType::Decision, MemoryType::Hazard];
    let min_criticality = Some(0.9_f64);
    let include_expired = false;
    let filters = SearchFilters {
        types: Some(types.clone()),
        tags: None,
        physical: None,
        min_criticality,
    };

    // Apply the Rust-only gate (physical/tags none here) plus expiry.
    let apply_rust = |entries: Vec<crate::storage::IndexForFiltering>| -> BTreeSet<String> {
        let now = Utc::now();
        apply_index_filters(entries, &filters)
            .into_iter()
            .filter(|e| include_expired || e.expires_at.is_none_or(|exp| exp > now))
            .map(|e| e.id)
            .collect()
    };

    // Baseline: whole-table scan, filtered entirely in Rust.
    let baseline_ids = apply_rust(store.list_for_filtering().await.unwrap());

    // Pushdown: predicate pre-narrows the scan, then the identical Rust gate.
    let predicate = build_filter_predicate(
        Some(&types),
        None,
        min_criticality,
        (!include_expired).then(Utc::now),
        None,
    );
    assert!(predicate.is_some(), "expected a non-empty predicate");
    let pushdown_entries = store.list_for_filtering_where(predicate).await.unwrap();
    let pushdown_ids = apply_rust(pushdown_entries);

    assert_eq!(
        pushdown_ids, baseline_ids,
        "pushdown predicate must agree with the Rust filter path"
    );
    // Sanity: the selective filter really did select the two high-criticality
    // active Decision/Hazard memories (summary 0 and 3).
    assert_eq!(baseline_ids.len(), 2);
}

#[tokio::test]
async fn test_retrieve_respects_max_results() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    // Create 5 memories
    for i in 0..5 {
        let mut memory = Memory::new(
            MemoryType::Context,
            format!("Memory {}", i),
            format!("Content {}", i),
            Provenance::human(),
        );
        memory.visibility = Visibility::Shared;
        memory.criticality = 0.8;
        store.create(&memory).await.unwrap();
    }

    let engine = RetrievalEngine::new(store, config);

    // Retrieve with max_results=2
    let query = RetrievalQuery {
        max_results: Some(2),
        ..Default::default()
    };

    let result = engine.query(&query).await.unwrap();

    assert_eq!(result.memories.len(), 2);
    assert_eq!(result.total, 5);
}

#[tokio::test]
async fn test_retrieve_excludes_expired() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use chrono::{Duration, Utc};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    // Create an expired memory
    let mut expired = Memory::new(
        MemoryType::Debug,
        "Expired memory",
        "This has expired",
        Provenance::human(),
    );
    expired.expires_at = Some(Utc::now() - Duration::days(1));
    expired.visibility = Visibility::Shared;
    expired.criticality = 0.8;

    // Create an active memory
    let mut active = Memory::new(
        MemoryType::Decision,
        "Active memory",
        "This is active",
        Provenance::human(),
    );
    active.visibility = Visibility::Shared;
    active.criticality = 0.8;

    store.create(&expired).await.unwrap();
    store.create(&active).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    // Retrieve with include_expired=false
    let query = RetrievalQuery {
        include_expired: Some(false),
        ..Default::default()
    };

    let result = engine.query(&query).await.unwrap();

    assert_eq!(result.memories.len(), 1);
    assert_eq!(result.memories[0].memory.summary, "Active memory");
}

#[tokio::test]
async fn test_retrieve_include_expired_scores_without_decay() {
    use crate::types::{Decay, EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use chrono::{Duration, Utc};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let config = EngramConfig::default(); // relevance_threshold = 0.5

    // Expired memory with high criticality → score without decay
    // should exceed threshold: 1.0 * 0.8 * scope_mult(1.0) * trust(1.0) = 0.80
    let mut expired = Memory::new(
        MemoryType::Debug,
        "Decayed memory",
        "This has fully decayed",
        Provenance::human(),
    );
    expired.expires_at = Some(Utc::now() - Duration::seconds(1));
    expired.visibility = Visibility::Shared;
    expired.criticality = 0.8;
    expired.decay = Some(Decay::linear(Duration::seconds(1)).with_floor(0.0));
    expired.created_at = Utc::now() - Duration::seconds(10);

    // Active memory
    let mut active = Memory::new(
        MemoryType::Decision,
        "Active memory",
        "This is active",
        Provenance::human(),
    );
    active.visibility = Visibility::Shared;
    active.criticality = 0.8;

    store.create(&expired).await.unwrap();
    store.create(&active).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    // include_expired=false: expired memory filtered by expires_at
    let query_exclude = RetrievalQuery {
        include_expired: Some(false),
        ..Default::default()
    };
    let result = engine.query(&query_exclude).await.unwrap();
    assert_eq!(result.memories.len(), 1);
    assert_eq!(result.memories[0].memory.summary, "Active memory");

    // include_expired=true: expired memory scored ignoring decay,
    // so its score is based on criticality + scope (not zeroed by decay)
    let query_include = RetrievalQuery {
        include_expired: Some(true),
        ..Default::default()
    };
    let result = engine.query(&query_include).await.unwrap();
    assert_eq!(
            result.memories.len(),
            2,
            "include_expired=true should score expired memories without decay, allowing them to pass threshold"
        );

    // Verify the expired memory's decay is recorded but didn't kill the score
    let expired_result = result
        .memories
        .iter()
        .find(|sm| sm.memory.summary == "Decayed memory")
        .expect("expired memory should be in results");
    assert!(
        expired_result.score_breakdown.decay > 0.99,
        "decay should be near 1.0 (fully decayed, recorded for transparency)"
    );
    assert!(
        expired_result.score > 0.3,
        "score should exceed threshold because decay was ignored for scoring"
    );
}

/// Regression: `include_expired: true` must not disable decay for ALL
/// memories — only the expired ones are scored with
/// `composite_score_ignore_decay`. Before the fix, a half-decayed ACTIVE
/// memory ranked as fresh whenever the caller merely asked to also see
/// expired memories, silently reordering the non-expired results.
#[tokio::test]
async fn test_include_expired_keeps_decay_for_active_memories() {
    use crate::types::{Decay, EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use chrono::{Duration, Utc};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    // ACTIVE memory (no expires_at) that has heavily decayed: 100 half-
    // lives → decay factor ≈ 0. Its score must reflect that decay even
    // when include_expired=true.
    let mut active_decayed = Memory::new(
        MemoryType::Context,
        "Active but decayed",
        "old active content",
        Provenance::human(),
    );
    active_decayed.visibility = Visibility::Shared;
    active_decayed.criticality = 0.8;
    active_decayed.decay = Some(Decay::exponential(Duration::hours(1)).with_floor(0.0));
    active_decayed.created_at = Utc::now() - Duration::hours(100);

    // EXPIRED memory: scored ignoring decay so it can surface at all.
    let mut expired = Memory::new(
        MemoryType::Debug,
        "Expired memory",
        "expired content",
        Provenance::human(),
    );
    expired.visibility = Visibility::Shared;
    expired.criticality = 0.8;
    expired.decay = Some(Decay::linear(Duration::seconds(1)).with_floor(0.0));
    expired.created_at = Utc::now() - Duration::seconds(10);
    expired.expires_at = Some(Utc::now() - Duration::seconds(1));

    store.create(&active_decayed).await.unwrap();
    store.create(&expired).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    // Baseline: include_expired=false → only the active memory, scored
    // WITH decay (near zero).
    let result_excl = engine
        .query(&RetrievalQuery {
            include_expired: Some(false),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result_excl.memories.len(), 1);
    assert_eq!(result_excl.memories[0].memory.id, active_decayed.id);
    let active_score_excl = result_excl.memories[0].score;
    assert!(
        active_score_excl < 0.1,
        "active decayed memory should score near zero, got {active_score_excl}"
    );

    // include_expired=true → both returned; the EXPIRED one is scored
    // ignore-decay, the ACTIVE one keeps its decayed score.
    let result_incl = engine
        .query(&RetrievalQuery {
            include_expired: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result_incl.memories.len(), 2);

    let active_incl = result_incl
        .memories
        .iter()
        .find(|sm| sm.memory.id == active_decayed.id)
        .expect("active memory should be in results");
    let expired_incl = result_incl
        .memories
        .iter()
        .find(|sm| sm.memory.id == expired.id)
        .expect("expired memory should be in results");

    assert!(
        (active_incl.score - active_score_excl).abs() < 1e-9,
        "active memory's decayed score must not change when include_expired=true \
             (got {} vs baseline {})",
        active_incl.score,
        active_score_excl
    );
    assert!(
        expired_incl.score > 0.3,
        "expired memory should be scored ignoring decay, got {}",
        expired_incl.score
    );
    assert!(
        expired_incl.score > active_incl.score,
        "ignore-decay expired memory should outrank the decayed active one here"
    );
}

#[tokio::test]
async fn test_retrieve_detail_level_summary() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    // Create a memory with content and details
    let mut memory = Memory::new(
        MemoryType::Decision,
        "Test memory",
        "This is the content",
        Provenance::human(),
    );
    memory.details = Some("These are the details".to_string());
    memory.visibility = Visibility::Shared;
    memory.criticality = 0.8;

    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    // Retrieve with detail_level=Summary
    let query = RetrievalQuery {
        detail_level: DetailLevel::Summary,
        ..Default::default()
    };

    let result = engine.query(&query).await.unwrap();

    assert_eq!(result.memories.len(), 1);
    assert_eq!(result.memories[0].memory.summary, "Test memory");
    assert_eq!(result.memories[0].memory.content, "");
    assert_eq!(result.memories[0].memory.details, None);
}

#[tokio::test]
async fn test_retrieve_score_breakdown() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    let mut memory = Memory::new(
        MemoryType::Decision,
        "Test memory",
        "This is the content",
        Provenance::human(),
    );
    memory.visibility = Visibility::Shared;
    memory.criticality = 0.8;
    memory.physical = vec!["src/test.rs".to_string()];

    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    let query = RetrievalQuery {
        path: Some("src/test.rs".to_string()),
        ..Default::default()
    };

    let result = engine.query(&query).await.unwrap();

    assert_eq!(result.memories.len(), 1);
    // Verify score_breakdown is populated
    let sm = &result.memories[0];
    assert!(sm.score_breakdown.final_score > 0.0);
    assert_eq!(sm.score, sm.score_breakdown.final_score);
    assert!(sm.score_breakdown.relevance > 0.0);
    assert!(sm.score_breakdown.scope > 0.0);
    assert!(sm.score_breakdown.trust > 0.0);
    assert_eq!(sm.score_breakdown.decay, 0.0); // Fresh, no decay
    assert!(sm.score_breakdown.semantic.is_none()); // No query
}

#[tokio::test]
async fn test_query_rank_retrieval_quality_scope_only() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    let mut memory = Memory::new(
        MemoryType::Decision,
        "Test memory",
        "This is the content",
        Provenance::human(),
    );
    memory.visibility = Visibility::Shared;
    memory.criticality = 0.8;

    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    let query = RetrievalQuery::default();

    let result = engine.query(&query).await.unwrap();

    assert_eq!(result.retrieval_quality, "scope_only");
}

#[tokio::test]
async fn test_query_rank_retrieval_quality_keyword_only() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    let mut memory = Memory::new(
        MemoryType::Decision,
        "Test memory",
        "This is the content",
        Provenance::human(),
    );
    memory.visibility = Visibility::Shared;
    memory.criticality = 0.8;

    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    let query = RetrievalQuery {
        query: Some("test".to_string()),
        ..Default::default()
    };

    let result = engine.query(&query).await.unwrap();

    // Without embeddings configured, should be keyword_only quality
    // (keyword search finds "test" in the memory summary).
    assert_eq!(result.retrieval_quality, "keyword_only");
}

#[tokio::test]
async fn test_search_keyword_integration() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();

    // Create memories with specific keywords
    let mut memory1 = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "This memory discusses authentication and login flows",
        Provenance::human(),
    );
    memory1.visibility = Visibility::Shared;

    let mut memory2 = Memory::new(
        MemoryType::Context,
        "Database schema",
        "This memory is about database tables and relations",
        Provenance::human(),
    );
    memory2.visibility = Visibility::Shared;

    store.create(&memory1).await.unwrap();
    store.create(&memory2).await.unwrap();

    let engine = RetrievalEngine::new(store, config);

    // Search for "authentication"
    let results = filter_query(&engine, "authentication").await;

    // Should find memory1 with higher score
    assert!(!results.is_empty());
    assert_eq!(results[0].memory.summary, "Authentication system");
    // Score should be in [0, 1] (normalized)
    assert!(results[0].score > 0.0);
    assert!(results[0].score <= 1.0);
    // Should have keyword breakdown
    assert!(results[0].score_breakdown.keyword.is_some());
    // Trust should be the real trust weight
    assert_eq!(results[0].score_breakdown.trust, 1.0);
    // Decay should be 0.0 (fresh, no decay)
    assert_eq!(results[0].score_breakdown.decay, 0.0);
}

#[tokio::test]
async fn test_search_applies_trust_multiplier() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut config = EngramConfig::default();
    config.search.threshold = 0.0;

    // Human memory
    let mut human_mem = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about authentication",
        Provenance::human(),
    );
    human_mem.visibility = Visibility::Shared;
    store.create(&human_mem).await.unwrap();

    // Inferred memory with same content
    let mut inferred_mem = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about authentication",
        Provenance::inferred(),
    );
    inferred_mem.visibility = Visibility::Shared;
    store.create(&inferred_mem).await.unwrap();

    let engine = RetrievalEngine::new(store, config);
    let results = filter_query(&engine, "authentication").await;

    assert_eq!(results.len(), 2);
    // Both should have same keyword score, but different trust
    let human_result = results
        .iter()
        .find(|r| r.memory.id == human_mem.id)
        .unwrap();
    let inferred_result = results
        .iter()
        .find(|r| r.memory.id == inferred_mem.id)
        .unwrap();

    assert_eq!(human_result.score_breakdown.trust, 1.0);
    assert_eq!(inferred_result.score_breakdown.trust, 0.6);
    // Inferred should score lower than human (trust multiplier)
    assert!(inferred_result.score < human_result.score);
    // Scores should be in [0, 1]
    assert!(human_result.score <= 1.0);
    assert!(inferred_result.score <= 1.0);
}

#[tokio::test]
async fn test_search_applies_decay() {
    use crate::types::{Decay, EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use chrono::{Duration, Utc};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut config = EngramConfig::default();
    config.search.threshold = 0.0;

    // Fresh memory (no decay)
    let mut fresh = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about authentication",
        Provenance::human(),
    );
    fresh.visibility = Visibility::Shared;
    store.create(&fresh).await.unwrap();

    // Old memory with exponential decay at half-life
    let mut old = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about authentication",
        Provenance::human(),
    );
    old.visibility = Visibility::Shared;
    old.created_at = Utc::now() - Duration::days(7);
    old.decay = Some(Decay::exponential(Duration::days(7)));
    store.create(&old).await.unwrap();

    let engine = RetrievalEngine::new(store, config);
    let results = filter_query(&engine, "authentication").await;

    assert_eq!(results.len(), 2);
    let fresh_result = results.iter().find(|r| r.memory.id == fresh.id).unwrap();
    let old_result = results.iter().find(|r| r.memory.id == old.id).unwrap();

    // Fresh has decay=0.0 (no decay), old has decay~=0.5 (half decayed)
    assert_eq!(fresh_result.score_breakdown.decay, 0.0);
    assert!((old_result.score_breakdown.decay - 0.5).abs() < 0.1);
    // Old should score lower than fresh due to decay
    assert!(old_result.score < fresh_result.score);
    // Scores should be in [0, 1]
    assert!(fresh_result.score <= 1.0);
    assert!(old_result.score <= 1.0);
}

#[tokio::test]
async fn test_search_applies_challenge_penalty() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Status, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut config = EngramConfig::default();
    config.search.threshold = 0.0;

    // Active memory
    let mut active = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about authentication",
        Provenance::human(),
    );
    active.visibility = Visibility::Shared;
    store.create(&active).await.unwrap();

    // Challenged memory with same content
    let mut challenged = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about authentication",
        Provenance::human(),
    );
    challenged.visibility = Visibility::Shared;
    challenged.status = Status::Challenged;
    store.create(&challenged).await.unwrap();

    let engine = RetrievalEngine::new(store, config);
    let results = filter_query(&engine, "authentication").await;

    assert_eq!(results.len(), 2);
    let active_result = results.iter().find(|r| r.memory.id == active.id).unwrap();
    let challenged_result = results
        .iter()
        .find(|r| r.memory.id == challenged.id)
        .unwrap();

    // Challenged should score lower than active (0.7x penalty)
    assert!(challenged_result.score < active_result.score);
    // Both should be in [0, 1]
    assert!(active_result.score <= 1.0);
    assert!(challenged_result.score <= 1.0);
}

#[tokio::test]
async fn test_search_threshold_filters_results() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    // Set threshold to 1.0 (max possible score) so everything is filtered
    config.search.threshold = 1.0;

    let mut memory = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about authentication",
        Provenance::human(),
    );
    memory.visibility = Visibility::Shared;
    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config);
    let results = filter_query(&engine, "authentication").await;

    // Scores are in [0, 1] and a single partial match won't reach 1.0
    assert!(results.is_empty());
}

#[tokio::test]
async fn test_search_threshold_zero_returns_all() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.search.threshold = 0.0;

    let mut memory = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about auth",
        Provenance::human(),
    );
    memory.visibility = Visibility::Shared;
    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config);
    let results = filter_query(&engine, "authentication").await;

    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn test_search_breakdown_has_keyword_and_criticality() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();

    let mut memory = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about authentication",
        Provenance::human(),
    );
    memory.visibility = Visibility::Shared;
    memory.criticality = 0.7;
    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config);
    let results = filter_query(&engine, "authentication").await;

    assert_eq!(results.len(), 1);
    let bd = &results[0].score_breakdown;
    // keyword should be populated with normalized score in [0, 1]
    assert!(bd.keyword.is_some());
    assert!(bd.keyword.unwrap() > 0.0);
    assert!(bd.keyword.unwrap() <= 1.0);
    // final score should be in [0, 1]
    assert!(bd.final_score > 0.0);
    assert!(bd.final_score <= 1.0);
    // criticality should be the raw value
    assert_eq!(bd.criticality, 0.7);
    // relevance should be criticality * (1 - decay), since decay=0 means fresh
    assert_eq!(bd.relevance, 0.7 * (1.0 - bd.decay));
}

#[tokio::test]
async fn test_search_scores_in_unit_interval() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut config = EngramConfig::default();
    config.search.threshold = 0.0; // return everything

    // Create memories with varying keyword overlap
    for (summary, content) in [
        ("auth system login", "authentication password hashing"),
        ("auth password", "bcrypt hashing details"),
        ("database schema", "tables and relations"),
    ] {
        let mut memory = Memory::new(MemoryType::Decision, summary, content, Provenance::human());
        memory.visibility = Visibility::Shared;
        memory.criticality = 0.8;
        store.create(&memory).await.unwrap();
    }

    let engine = RetrievalEngine::new(store, config);
    let results = filter_query(&engine, "auth password hashing").await;

    for sm in &results {
        assert!(
            sm.score >= 0.0 && sm.score <= 1.0,
            "score {} not in [0, 1] for '{}'",
            sm.score,
            sm.memory.summary
        );
    }
}

#[tokio::test]
async fn test_detect_contradictions_no_provider() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();

    let engine = RetrievalEngine::new(store, config);

    let memory = Memory::new(
        MemoryType::Decision,
        "Test memory",
        "Test content",
        Provenance::human(),
    );

    // Without NLI provider, should return empty vec
    let result = engine.detect_contradictions(&memory).await.unwrap();
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_nli_provider_builder() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();

    // NLI not available: no provider, config disabled by default
    let engine = RetrievalEngine::new(store, config);
    assert!(!engine.nli_available());
}

#[tokio::test]
async fn test_nli_available_requires_config_enabled() {
    use crate::nli::{NliProvider, NliResult};
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    /// Dummy NLI provider for testing config gating.
    struct DummyNli;

    #[async_trait::async_trait]
    impl NliProvider for DummyNli {
        async fn classify(&self, _p: &str, _h: &str) -> anyhow::Result<NliResult> {
            Ok(NliResult::from_probs(0.0, 1.0, 0.0))
        }
        async fn classify_batch(&self, _pairs: &[(&str, &str)]) -> anyhow::Result<Vec<NliResult>> {
            Ok(vec![])
        }
    }

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // Config disabled (default) + provider attached → nli_available should be false
    let config = EngramConfig::default();
    let engine = RetrievalEngine::new(store, config).with_nli_provider(Arc::new(DummyNli));
    assert!(
        !engine.nli_available(),
        "NLI should not be available when config.nli.enabled is false"
    );

    // Config enabled + provider attached → nli_available should be true
    let store2 = MemoryStore::open(temp_dir.path()).await.unwrap();
    let mut config2 = EngramConfig::default();
    config2.nli.enabled = true;
    let engine2 = RetrievalEngine::new(store2, config2).with_nli_provider(Arc::new(DummyNli));
    assert!(
        engine2.nli_available(),
        "NLI should be available when config.nli.enabled is true and provider is set"
    );
}

#[tokio::test]
async fn test_detect_contradictions_disabled_by_config() {
    use crate::nli::{NliProvider, NliResult};
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    /// Dummy NLI provider that panics if called — verifying config gating.
    struct PanicNli;

    #[async_trait::async_trait]
    impl NliProvider for PanicNli {
        async fn classify(&self, _p: &str, _h: &str) -> anyhow::Result<NliResult> {
            panic!("NLI should not be called when disabled");
        }
        async fn classify_batch(&self, _pairs: &[(&str, &str)]) -> anyhow::Result<Vec<NliResult>> {
            panic!("NLI should not be called when disabled");
        }
    }

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // Config disabled + provider attached → detect_contradictions should short-circuit
    let config = EngramConfig::default(); // nli.enabled = false
    let engine = RetrievalEngine::new(store, config).with_nli_provider(Arc::new(PanicNli));

    let memory = Memory::new(
        MemoryType::Decision,
        "Test memory",
        "Test content",
        Provenance::human(),
    );

    let result = engine.detect_contradictions(&memory).await.unwrap();
    assert!(
        result.is_empty(),
        "detect_contradictions should return empty when config.nli.enabled is false"
    );
}

/// End-to-end exercise of `detect_contradictions_with` (engine.rs:842):
/// real ONNX embedding provider + real NLI provider + a contradicting
/// memory in the store. Skips cleanly if either model is unavailable.
/// Before this test the entire `detect_contradictions_with` body
/// (vector_search + classify_batch + threshold filter) was at 0%
/// coverage despite CRAP 110.
// Loads the real ONNX embedding + NLI models, so it only exists on an
// `onnxruntime` build.
#[cfg(feature = "onnxruntime")]
#[tokio::test]
async fn test_detect_contradictions_end_to_end_finds_real_contradiction() {
    use crate::embeddings::OnnxProvider;
    use crate::nli::OnnxNliProvider;
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let embedding =
        engram_test_support::require_model!("default ONNX embedding model", OnnxProvider::new());
    let nli = engram_test_support::require_model!(
        "NLI model cross-encoder/nli-deberta-v3-xsmall",
        OnnxNliProvider::new("cross-encoder/nli-deberta-v3-xsmall")
    );

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // Seed an existing memory whose summary directly contradicts the
    // probe summary below.
    let mut existing = Memory::new(
        MemoryType::Decision,
        "The restaurant is open",
        "Body content for the existing memory",
        Provenance::human(),
    );
    existing.visibility = Visibility::Shared;
    store.create(&existing).await.unwrap();

    // Embed the existing memory so vector_search has something to find.
    let embed_arc: Arc<dyn EmbeddingProvider> = Arc::new(embedding);
    embed_memory_with(
        embed_arc.as_ref(),
        &store,
        &existing,
        embed_arc.max_tokens(),
        true,
        None,
    )
    .await
    .unwrap();

    // Make a probe memory not yet in the store.
    let mut probe = Memory::new(
        MemoryType::Decision,
        "The restaurant is closed",
        "Body content for the probe memory",
        Provenance::human(),
    );
    probe.visibility = Visibility::Shared;

    let mut config = EngramConfig::default();
    config.nli.enabled = true;
    // Lenient thresholds: this test must reach the NLI step, not get
    // gated out by an unrelated default.
    config.nli.similarity_threshold = 0.0;
    config.nli.contradiction_threshold = 0.5;

    let engine = RetrievalEngine::new(store, config)
        .with_embedding_provider(Arc::clone(&embed_arc))
        .with_nli_provider(Arc::new(nli));

    let result = engine.detect_contradictions(&probe).await.unwrap();
    // The cross-encoder reliably flags "open" vs "closed" as a
    // contradiction at >0.5 — this is the antonym pair already
    // covered by nli::onnx::tests::test_antonym_contradiction.
    assert!(
        !result.is_empty(),
        "expected at least one contradiction for open vs closed"
    );
    let (id, nli_res) = &result[0];
    assert_eq!(id, &existing.id, "wrong memory flagged");
    assert!(
        nli_res.contradiction > 0.5,
        "contradiction prob too low: {}",
        nli_res.contradiction
    );
}

/// Drive the early-return at detect_contradictions_with where no
/// candidates pass the similarity_threshold. Locks the threshold
/// filter so it can't regress into returning all vector neighbours.
// Loads the real ONNX embedding model, so it only exists on an
// `onnxruntime` build.
#[cfg(feature = "onnxruntime")]
#[tokio::test]
async fn test_detect_contradictions_returns_empty_when_no_similar_candidates() {
    use crate::embeddings::OnnxProvider;
    use crate::nli::{NliProvider, NliResult};
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let embedding =
        engram_test_support::require_model!("default ONNX embedding model", OnnxProvider::new());

    // NLI shouldn't be called — we route through the similarity
    // threshold gate first. Use a dummy provider that panics if hit.
    struct ShouldNotBeCalled;
    #[async_trait::async_trait]
    impl NliProvider for ShouldNotBeCalled {
        async fn classify(&self, _p: &str, _h: &str) -> anyhow::Result<NliResult> {
            panic!("NLI must not be called when candidates are filtered out");
        }
        async fn classify_batch(&self, _pairs: &[(&str, &str)]) -> anyhow::Result<Vec<NliResult>> {
            panic!("NLI must not be called when candidates are filtered out");
        }
    }

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // Seed a memory so vector_search returns at least one match.
    let existing = Memory {
        visibility: Visibility::Shared,
        ..Memory::new(
            MemoryType::Decision,
            "Persisted memory",
            "stored body",
            Provenance::human(),
        )
    };
    store.create(&existing).await.unwrap();
    let embed_arc: Arc<dyn EmbeddingProvider> = Arc::new(embedding);
    embed_memory_with(
        embed_arc.as_ref(),
        &store,
        &existing,
        embed_arc.max_tokens(),
        true,
        None,
    )
    .await
    .unwrap();

    let mut config = EngramConfig::default();
    config.nli.enabled = true;
    // Impossible threshold: nothing can pass → empty candidate set →
    // NLI never invoked → ShouldNotBeCalled.classify_batch unreached.
    config.nli.similarity_threshold = 10.0;

    let engine = RetrievalEngine::new(store, config)
        .with_embedding_provider(Arc::clone(&embed_arc))
        .with_nli_provider(Arc::new(ShouldNotBeCalled));

    let probe = Memory::new(
        MemoryType::Decision,
        "probe summary",
        "probe body",
        Provenance::human(),
    );
    let result = engine.detect_contradictions(&probe).await.unwrap();
    assert!(result.is_empty(), "no candidate survived → empty result");
}

#[tokio::test]
async fn test_retrieve_without_reranker_has_no_rerank_scores() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    let mut memory = Memory::new(
        MemoryType::Decision,
        "Test memory",
        "Test content for reranking",
        Provenance::human(),
    );
    memory.visibility = Visibility::Shared;
    memory.criticality = 0.8;
    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config);
    assert!(!engine.reranking_available());

    let query = RetrievalQuery {
        query: Some("test".to_string()),
        ..Default::default()
    };
    let result = engine.query(&query).await.unwrap();

    assert_eq!(result.memories.len(), 1);
    // Without reranker, rerank score should be None
    assert!(result.memories[0].score_breakdown.rerank.is_none());
}

#[tokio::test]
async fn test_search_without_reranker_has_no_rerank_scores() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();

    let mut memory = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "Details about authentication",
        Provenance::human(),
    );
    memory.visibility = Visibility::Shared;
    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config);
    let results = filter_query(&engine, "authentication").await;

    assert!(!results.is_empty());
    // Without reranker, rerank score should be None
    for r in &results {
        assert!(r.score_breakdown.rerank.is_none());
    }
}

#[tokio::test]
async fn test_reranking_available_reflects_config() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    // Reranking not available: no reranker set, config disabled
    let config = EngramConfig::default();
    let engine = RetrievalEngine::new(store, config);
    assert!(!engine.reranking_available());
}

#[tokio::test]
async fn test_rerank_with_real_reranker() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let reranker = match try_reranker() {
        Some(r) => r,
        None => return,
    };

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;
    config.rerank.enabled = true;
    config.rerank.weight = 0.5;
    config.rerank.top_n = 10;

    // Create memories with different relevance to "rust programming"
    let mut mem1 = Memory::new(
        MemoryType::Decision,
        "Rust language choice",
        "We chose Rust for its memory safety and performance guarantees",
        Provenance::human(),
    );
    mem1.visibility = Visibility::Shared;
    mem1.criticality = 0.8;
    store.create(&mem1).await.unwrap();

    let mut mem2 = Memory::new(
        MemoryType::Context,
        "Database setup",
        "The database uses PostgreSQL with connection pooling",
        Provenance::human(),
    );
    mem2.visibility = Visibility::Shared;
    mem2.criticality = 0.8;
    store.create(&mem2).await.unwrap();

    let engine = RetrievalEngine::new(store, config).with_reranker(reranker);

    assert!(engine.reranking_available());

    let query = RetrievalQuery {
        query: Some("rust programming language".to_string()),
        ..Default::default()
    };

    let result = engine.query(&query).await.unwrap();

    // Both memories should be returned
    assert_eq!(result.memories.len(), 2);

    // At least one should have a rerank score populated
    let has_rerank = result
        .memories
        .iter()
        .any(|m| m.score_breakdown.rerank.is_some());
    assert!(has_rerank, "Rerank scores should be populated");
}

#[tokio::test]
async fn test_rerank_blend_weight_zero_preserves_original_order() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let reranker = match try_reranker() {
        Some(r) => r,
        None => return,
    };

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;
    config.rerank.enabled = true;
    config.rerank.weight = 0.0; // Zero weight: should preserve original scores
    config.rerank.top_n = 10;

    let mut mem1 = Memory::new(
        MemoryType::Decision,
        "High criticality item",
        "Important decision about architecture",
        Provenance::human(),
    );
    mem1.visibility = Visibility::Shared;
    mem1.criticality = 0.9;
    store.create(&mem1).await.unwrap();

    let mut mem2 = Memory::new(
        MemoryType::Context,
        "Lower criticality item",
        "Some context about the project",
        Provenance::human(),
    );
    mem2.visibility = Visibility::Shared;
    mem2.criticality = 0.3;
    store.create(&mem2).await.unwrap();

    // First, get scores without reranker
    let engine_no_rerank = RetrievalEngine::new(
        MemoryStore::open(temp_dir.path()).await.unwrap(),
        config.clone(),
    );

    let query = RetrievalQuery {
        query: Some("architecture decision".to_string()),
        ..Default::default()
    };

    let result_no_rerank = engine_no_rerank.query(&query).await.unwrap();

    // Now with reranker but weight=0.0
    let engine_rerank =
        RetrievalEngine::new(MemoryStore::open(temp_dir.path()).await.unwrap(), config)
            .with_reranker(reranker);

    let result_rerank = engine_rerank.query(&query).await.unwrap();

    // With weight=0.0, blended = 1.0 * original + 0.0 * rerank = original
    // Scores should be identical
    assert_eq!(
        result_no_rerank.memories.len(),
        result_rerank.memories.len()
    );
    for (a, b) in result_no_rerank
        .memories
        .iter()
        .zip(result_rerank.memories.iter())
    {
        assert!(
            (a.score - b.score).abs() < 0.001,
            "Scores should match with weight=0.0: {} vs {}",
            a.score,
            b.score
        );
    }
}

#[tokio::test]
async fn test_search_with_real_reranker() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let reranker = match try_reranker() {
        Some(r) => r,
        None => return,
    };

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.search.threshold = 0.0;
    config.rerank.enabled = true;
    config.rerank.weight = 0.5;
    config.rerank.top_n = 10;

    let mut memory = Memory::new(
        MemoryType::Decision,
        "Authentication system",
        "OAuth2 authentication with JWT tokens for API security",
        Provenance::human(),
    );
    memory.visibility = Visibility::Shared;
    store.create(&memory).await.unwrap();

    let engine = RetrievalEngine::new(store, config).with_reranker(reranker);

    let results = filter_query(&engine, "authentication").await;

    assert!(!results.is_empty());
    // Should have rerank score populated
    assert!(results[0].score_breakdown.rerank.is_some());
}

/// Scenario: Criticality-biased ordering gets corrected by reranking.
///
/// Without reranking, a high-criticality but off-topic memory outranks a
/// low-criticality but on-topic memory (because degraded-mode scoring is
/// dominated by `0.70 * criticality * decay`).
///
/// The cross-encoder sees the actual query+document pair and gives the
/// on-topic memory a much higher score (+6.88) vs the off-topic one (−9.38),
/// flipping the order.
#[tokio::test]
async fn test_rerank_flips_retrieve_order_criticality_bias() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let reranker = match try_reranker() {
        Some(r) => r,
        None => return,
    };

    let temp_dir = TempDir::new().unwrap();

    // ── Step 1: retrieve WITHOUT reranker ──────────────────────────────
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    // Off-topic but HIGH criticality → ranks first in degraded mode
    let mut off_topic = Memory::new(
        MemoryType::Decision,
        "Project setup notes",
        "Initial project configuration and folder structure with cargo init",
        Provenance::human(),
    );
    off_topic.visibility = Visibility::Shared;
    off_topic.criticality = 0.95;
    store.create(&off_topic).await.unwrap();
    let off_topic_id = off_topic.id.clone();

    // On-topic but LOW criticality → ranks second in degraded mode
    let mut on_topic = Memory::new(
            MemoryType::Context,
            "Rust borrow checker",
            "The borrow checker enforces ownership rules at compile time, preventing data races and dangling references without a garbage collector",
            Provenance::human(),
        );
    on_topic.visibility = Visibility::Shared;
    on_topic.criticality = 0.3;
    store.create(&on_topic).await.unwrap();
    let on_topic_id = on_topic.id.clone();

    let query = RetrievalQuery {
        query: Some("how does the borrow checker work in Rust".to_string()),
        ..Default::default()
    };

    let engine_no_rerank = RetrievalEngine::new(store, config.clone());
    let result_no_rerank = engine_no_rerank.query(&query).await.unwrap();

    assert_eq!(result_no_rerank.memories.len(), 2);
    // Without reranking: off-topic (criticality=0.95) is ranked first
    assert_eq!(
        result_no_rerank.memories[0].memory.id, off_topic_id,
        "Without reranking, high-criticality off-topic memory should rank first"
    );
    assert_eq!(result_no_rerank.memories[1].memory.id, on_topic_id);

    // ── Step 2: retrieve WITH reranker (weight = 0.7) ─────────────────
    config.rerank.enabled = true;
    config.rerank.weight = 0.7;
    config.rerank.top_n = 10;

    let store2 = MemoryStore::open(temp_dir.path()).await.unwrap();
    let engine_rerank = RetrievalEngine::new(store2, config).with_reranker(reranker);

    let result_rerank = engine_rerank.query(&query).await.unwrap();

    assert_eq!(result_rerank.memories.len(), 2);
    // With reranking: on-topic memory should now rank first
    assert_eq!(
        result_rerank.memories[0].memory.id, on_topic_id,
        "With reranking, the on-topic memory about borrow checker should rank first"
    );
    assert_eq!(result_rerank.memories[1].memory.id, off_topic_id);

    // Cross-encoder raw score for on-topic should be much higher
    let on_topic_rerank = result_rerank.memories[0]
        .score_breakdown
        .rerank
        .expect("on-topic memory should have rerank score");
    let off_topic_rerank = result_rerank.memories[1]
        .score_breakdown
        .rerank
        .expect("off-topic memory should have rerank score");
    assert!(
        on_topic_rerank > off_topic_rerank,
        "Cross-encoder should rate on-topic ({}) higher than off-topic ({})",
        on_topic_rerank,
        off_topic_rerank,
    );
}

/// Scenario: Keyword-match bias gets corrected by reranking in search.
///
/// Without reranking, a memory with many keyword hits in the summary
/// (summary match = 3 pts each) dominates a memory whose content is more
/// semantically relevant but has fewer keyword hits.
///
/// The cross-encoder sees the full text and correctly prefers the
/// semantically relevant document, flipping the order.
#[tokio::test]
async fn test_rerank_flips_search_order_keyword_bias() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let reranker = match try_reranker() {
        Some(r) => r,
        None => return,
    };

    let temp_dir = TempDir::new().unwrap();

    // ── Step 1: search WITHOUT reranker ────────────────────────────────
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.search.threshold = 0.0;

    // Has keyword "database" in summary (3 pts) + content (1 pt) = 4 pts
    // But content is about config, NOT about migration strategy
    let mut keyword_heavy = Memory::new(
            MemoryType::Context,
            "Database configuration",
            "The application uses environment variables to configure database host, port, and credentials",
            Provenance::human(),
        );
    keyword_heavy.visibility = Visibility::Shared;
    keyword_heavy.criticality = 0.8;
    store.create(&keyword_heavy).await.unwrap();
    let keyword_heavy_id = keyword_heavy.id.clone();

    // Has only keyword "migration" in content (1 pt)
    // But content is ABOUT migration strategy → cross-encoder prefers this
    let mut semantically_relevant = Memory::new(
            MemoryType::Decision,
            "Schema versioning plan",
            "Use sequential migration files to evolve the schema, with up and down functions for rollback support",
            Provenance::human(),
        );
    semantically_relevant.visibility = Visibility::Shared;
    semantically_relevant.criticality = 0.8;
    store.create(&semantically_relevant).await.unwrap();
    let semantically_relevant_id = semantically_relevant.id.clone();

    let engine_no_rerank = RetrievalEngine::new(store, config.clone());
    let results_no_rerank = filter_query(&engine_no_rerank, "database migration strategy").await;

    assert_eq!(results_no_rerank.len(), 2);
    // Without reranking: keyword-heavy doc ranks first (4 pts vs 1 pt)
    assert_eq!(
        results_no_rerank[0].memory.id, keyword_heavy_id,
        "Without reranking, keyword-heavy memory should rank first"
    );
    assert_eq!(results_no_rerank[1].memory.id, semantically_relevant_id);

    // ── Step 2: search WITH reranker (weight = 0.95) ──────────────────
    // Sigmoid normalization is more conservative than min-max, so we use a
    // high weight to ensure the cross-encoder's preference dominates.
    config.rerank.enabled = true;
    config.rerank.weight = 0.95;
    config.rerank.top_n = 10;

    let store2 = MemoryStore::open(temp_dir.path()).await.unwrap();
    let engine_rerank = RetrievalEngine::new(store2, config).with_reranker(reranker);

    let results_rerank = filter_query(&engine_rerank, "database migration strategy").await;

    assert_eq!(results_rerank.len(), 2);
    // With reranking: the semantically relevant doc should now rank first
    assert_eq!(
        results_rerank[0].memory.id, semantically_relevant_id,
        "With reranking, the schema-versioning memory should rank first"
    );
    assert_eq!(results_rerank[1].memory.id, keyword_heavy_id);

    // Cross-encoder raw score for migration doc should be higher
    let relevant_rerank = results_rerank[0]
        .score_breakdown
        .rerank
        .expect("migration doc should have rerank score");
    let keyword_rerank = results_rerank[1]
        .score_breakdown
        .rerank
        .expect("keyword doc should have rerank score");
    assert!(
        relevant_rerank > keyword_rerank,
        "Cross-encoder should rate migration doc ({}) higher than config doc ({})",
        relevant_rerank,
        keyword_rerank,
    );
}

// ─── Filter-mode hierarchical logical filter ─────────────────────────────

/// Regression: the filter-mode `logical` filter was exact string
/// equality, so `logical: ["auth"]` failed to match a memory scoped
/// `auth.oauth` even though logical scopes are documented as dot-notation
/// hierarchies (and rank mode scores them hierarchically). Querying a
/// domain must surface its subdomains.
#[tokio::test]
async fn test_filter_mode_logical_matches_descendants() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();

    let scoped = |logical: &str, summary: &str| {
        let mut m = Memory::new(MemoryType::Decision, summary, "body", Provenance::human());
        m.logical = vec![logical.to_string()];
        m.visibility = Visibility::Shared;
        m.criticality = 0.9;
        m
    };

    let exact = scoped("auth", "Auth conventions");
    let child = scoped("auth.oauth", "OAuth decision");
    let grandchild = scoped("auth.oauth.google", "Google OAuth quirk");
    let unrelated = scoped("billing", "Billing decision");
    for m in [&exact, &child, &grandchild, &unrelated] {
        store.create(m).await.unwrap();
    }

    let engine = RetrievalEngine::new(store, config);
    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Filter,
            logical: vec!["auth".to_string()],
            ..Default::default()
        })
        .await
        .unwrap();

    let ids: Vec<&str> = result
        .memories
        .iter()
        .map(|sm| sm.memory.id.as_str())
        .collect();
    assert!(ids.contains(&exact.id.as_str()), "exact match: {ids:?}");
    assert!(
        ids.contains(&child.id.as_str()),
        "descendant `auth.oauth` must match query `auth`: {ids:?}"
    );
    assert!(
        ids.contains(&grandchild.id.as_str()),
        "deep descendant `auth.oauth.google` must match query `auth`: {ids:?}"
    );
    assert!(
        !ids.contains(&unrelated.id.as_str()),
        "`billing` must not match query `auth`: {ids:?}"
    );
    assert_eq!(result.memories.len(), 3);
}

/// The ancestor direction of the contract: querying `auth.oauth` matches
/// a memory scoped just `auth` (broad memories apply to their
/// subdomains, mirroring rank mode's bidirectional parent↔child bonus)
/// — but NOT the sibling `auth.jwt`, which rank mode only nudges and a
/// hard filter must exclude.
#[tokio::test]
async fn test_filter_mode_logical_matches_ancestors_not_siblings() {
    use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let config = EngramConfig::default();

    let scoped = |logical: &str, summary: &str| {
        let mut m = Memory::new(MemoryType::Decision, summary, "body", Provenance::human());
        m.logical = vec![logical.to_string()];
        m.visibility = Visibility::Shared;
        m.criticality = 0.9;
        m
    };

    let ancestor = scoped("auth", "Auth conventions");
    let exact = scoped("auth.oauth", "OAuth decision");
    let sibling = scoped("auth.jwt", "JWT decision");
    // String prefix but not a segment ancestor.
    let prefix_trap = scoped("authentication", "Unrelated prefix");
    for m in [&ancestor, &exact, &sibling, &prefix_trap] {
        store.create(m).await.unwrap();
    }

    let engine = RetrievalEngine::new(store, config);
    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Filter,
            logical: vec!["auth.oauth".to_string()],
            ..Default::default()
        })
        .await
        .unwrap();

    let ids: Vec<&str> = result
        .memories
        .iter()
        .map(|sm| sm.memory.id.as_str())
        .collect();
    assert!(ids.contains(&exact.id.as_str()), "exact match: {ids:?}");
    assert!(
        ids.contains(&ancestor.id.as_str()),
        "ancestor `auth` must match query `auth.oauth`: {ids:?}"
    );
    assert!(
        !ids.contains(&sibling.id.as_str()),
        "sibling `auth.jwt` must NOT match query `auth.oauth`: {ids:?}"
    );
    assert!(
        !ids.contains(&prefix_trap.id.as_str()),
        "`authentication` is not segment-related to `auth.oauth`: {ids:?}"
    );
    assert_eq!(result.memories.len(), 2);
}

// ─── Semantic-evidence regime tests (stub provider, no ONNX) ────────────

/// Deterministic embedding stub: marker substrings map to fixed vectors so
/// vector-search distances are fully controlled without loading any ONNX
/// model. Distances from the query marker's vector (e0), whether LanceDB
/// reports plain or squared L2 — the ordering is identical:
///   "queryvec" → v[0]=1.0, "closevec" → v[0]=0.4 (near),
///   "fillvec" → v[0]=0.1 (mid), anything else → v[1]=1.0 (orthogonal).
struct MarkerEmbeddingProvider;

/// `embed_memories` (reindex) and `embed_memory_texts` (the consolidation
/// fallback) must compose text exactly like the single-memory write path.
///
/// This is the guard for the whole "one store, two compositions" class of
/// bug: the consolidation pass used to build its own
/// `format!("{summary} {content}")`, which is one tokenizer-truncated
/// vector carrying neither title nor tags, while `create` stored a
/// metadata row plus chunked content. Anything that embeds a Memory must
/// go through `embedding_texts`, and this test fails if a new path
/// re-invents the string.
#[test]
fn every_embedding_path_uses_the_same_composition() {
    let mut memory = Memory::new(
        MemoryType::Decision,
        "Use LanceDB for the unified index",
        "The chunks table stores vectors; the memories table stores metadata.",
        crate::types::Provenance::human(),
    );
    memory.title = Some("LanceDB choice".to_string());
    memory.tags = vec!["storage".to_string(), "index".to_string()];

    let texts = embedding_texts(&memory, 256, true);

    // The metadata row must carry title AND tags — the fields the old
    // hand-rolled composition silently dropped.
    assert!(
        texts[0].contains("LanceDB choice"),
        "metadata row must carry the title, got {:?}",
        texts[0]
    );
    assert!(
        texts[0].contains("storage") && texts[0].contains("index"),
        "metadata row must carry the tags, got {:?}",
        texts[0]
    );
    assert!(
        texts.len() >= 2,
        "content must be embedded separately from metadata, got {texts:?}"
    );
    assert!(
        texts[1..].iter().any(|t| t.contains("chunks table")),
        "content must appear in a chunk, got {texts:?}"
    );

    // The legacy composition (metadata_vector = false) is the ONLY place
    // "{summary} {content}" is correct, and even there it is chunked
    // rather than truncated.
    let legacy = embedding_texts(&memory, 256, false);
    assert!(legacy[0].starts_with("Use LanceDB for the unified index"));
    assert!(legacy[0].contains("chunks table"));
}

/// Long content must be CHUNKED, not truncated — a memory longer than the
/// model's window has to be fully represented across several vectors.
#[test]
fn long_content_is_chunked_not_truncated() {
    let long = (0..600)
        .map(|i| format!("word{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut memory = Memory::new(
        MemoryType::Context,
        "A long memory",
        &long,
        crate::types::Provenance::human(),
    );
    memory.title = Some("Long".to_string());

    let texts = embedding_texts(&memory, 256, true);
    assert!(
        texts.len() > 2,
        "600 words at 256 tokens must produce several content chunks, got {}",
        texts.len()
    );
    // The tail must survive — the failure mode being guarded against is
    // silently dropping everything past the tokenizer's window.
    assert!(
        texts.iter().any(|t| t.contains("word599")),
        "the end of a long memory must be embedded, not truncated away"
    );
}

fn marker_vector(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; 384];
    if text.contains("queryvec") {
        v[0] = 1.0;
    } else if text.contains("closevec") {
        v[0] = 0.4;
    } else if text.contains("fillvec") {
        v[0] = 0.1;
    } else {
        v[1] = 1.0;
    }
    v
}

#[async_trait::async_trait]
impl EmbeddingProvider for MarkerEmbeddingProvider {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(marker_vector(text))
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| marker_vector(t)).collect())
    }
    fn dimensions(&self) -> usize {
        384
    }
    fn max_tokens(&self) -> usize {
        256
    }
    fn model_id(&self) -> String {
        "stub/marker".to_string()
    }
}

// ─── Rank threshold: after rerank, with a marked fallback ───────────────

/// Rank-mode engine over memories whose degraded score is their
/// criticality (no embeddings, a query no memory's text matches), so the
/// threshold arithmetic below is exact.
async fn rank_threshold_fixture(
    criticalities: &[(&str, f64)],
    tweak: impl FnOnce(&mut crate::types::EngramConfig),
) -> (tempfile::TempDir, RetrievalEngine) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    for (summary, c) in criticalities {
        store.create(&stub_memory(summary, *c)).await.unwrap();
    }
    let mut config = crate::types::EngramConfig::default();
    tweak(&mut config);
    (temp_dir, RetrievalEngine::new(store, config))
}

fn rank(query: &str) -> RetrievalQuery {
    RetrievalQuery {
        mode: RetrievalMode::Rank,
        query: Some(query.to_string()),
        ..Default::default()
    }
}

#[tokio::test]
async fn rank_returns_marked_fallback_instead_of_empty() {
    let (_d, engine) = rank_threshold_fixture(
        &[
            ("alpha", 0.30),
            ("beta", 0.20),
            ("gamma", 0.10),
            ("delta", 0.05),
        ],
        |_| {},
    )
    .await;
    let result = engine.query(&rank("unrelated words")).await.unwrap();
    // Nothing clears 0.45: the best 3 (rank_fallback default) come back, marked.
    let got: Vec<&str> = result
        .memories
        .iter()
        .map(|m| m.memory.summary.as_str())
        .collect();
    assert_eq!(got, vec!["alpha", "beta", "gamma"]);
    assert!(result
        .memories
        .iter()
        .all(|m| m.score_breakdown.below_threshold));
    assert_eq!(result.dropped_below_threshold, 1);
    assert_eq!(result.total, 3);
}

#[tokio::test]
async fn rank_fallback_zero_keeps_the_empty_result() {
    let (_d, engine) = rank_threshold_fixture(&[("alpha", 0.30), ("beta", 0.20)], |_| {}).await;
    let mut q = rank("unrelated words");
    q.rank_fallback = Some(0);
    let result = engine.query(&q).await.unwrap();
    assert!(result.memories.is_empty());
    assert_eq!(
        result.dropped_below_threshold, 2,
        "the cut is still reported"
    );

    let (_d, engine) = rank_threshold_fixture(&[("alpha", 0.30), ("beta", 0.20)], |c| {
        c.retrieval.rank_fallback = 1
    })
    .await;
    let result = engine.query(&rank("unrelated words")).await.unwrap();
    assert_eq!(
        result.memories.len(),
        1,
        "config rank_fallback applies when the query has none"
    );
}

#[tokio::test]
async fn rank_with_a_confident_match_returns_no_weak_ones() {
    let (_d, engine) =
        rank_threshold_fixture(&[("alpha", 0.90), ("beta", 0.30), ("gamma", 0.20)], |_| {}).await;
    let result = engine.query(&rank("unrelated words")).await.unwrap();
    assert_eq!(result.memories.len(), 1);
    assert!(!result.memories[0].score_breakdown.below_threshold);
    assert_eq!(result.dropped_below_threshold, 2);
}

/// Regression: the PreToolUse hook (scope-only rank, `situation:
/// file_edit`, `rank_fallback: 0`) injected nothing for a memory at the
/// default criticality (0.5). Scope, trust and situation multiply its
/// score to 0.36–0.44, under the 0.45 threshold, even for an exact path
/// match. A close scope match now clears on its own relevance; the
/// multipliers only order the results. A broad scope (`/`) still needs
/// the full score, or root memories would ride along on every edit.
#[tokio::test]
async fn scope_only_rank_keeps_default_criticality_scope_matches() {
    use crate::types::{Epistemic, Memory, MemoryType, Provenance, Situation, Visibility};

    let temp_dir = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mk = |summary: &str, physical: &[&str], criticality: f64, epistemic: Epistemic| {
        let mut m = Memory::new(
            MemoryType::Context,
            summary,
            "body",
            Provenance::agent("claude"),
        );
        m.visibility = Visibility::Shared;
        m.criticality = criticality;
        m.epistemic = epistemic;
        m.physical = physical.iter().map(|p| p.to_string()).collect();
        m
    };
    for m in [
        mk("exact fact", &["billing/invoice.py"], 0.5, Epistemic::Fact),
        mk("dir decision", &["billing/"], 0.5, Epistemic::Decision),
        mk("dir fact", &["billing/"], 0.5, Epistemic::Fact),
        mk("other dir", &["payments/"], 0.9, Epistemic::Fact),
        mk("unscoped", &[], 0.9, Epistemic::Fact),
        mk("root", &["/"], 0.5, Epistemic::Fact),
        mk("service root", &["services/"], 0.5, Epistemic::Fact),
        mk("root glob", &["**/*.py"], 0.5, Epistemic::Fact),
        mk(
            "low criticality",
            &["billing/invoice.py"],
            0.3,
            Epistemic::Fact,
        ),
    ] {
        store.create(&m).await.unwrap();
    }
    let engine = RetrievalEngine::new(store, crate::types::EngramConfig::default());
    let query = RetrievalQuery {
        mode: RetrievalMode::Rank,
        rank_fallback: Some(0),
        path: Some("billing/invoice.py".to_string()),
        situation: Some(Situation::FileEdit),
        ..Default::default()
    };
    let result = engine.query(&query).await.unwrap();
    let got: Vec<&str> = result
        .memories
        .iter()
        .map(|m| m.memory.summary.as_str())
        .collect();
    assert!(
        result.memories.iter().all(|m| m.score < 0.45),
        "premise: every multiplied score is under the threshold: {:?}",
        result.memories.iter().map(|m| m.score).collect::<Vec<_>>()
    );
    assert_eq!(got.len(), 3, "got {got:?}");
    assert_eq!(got[0], "exact fact", "the exact path match ranks first");
    assert!(got.contains(&"dir decision") && got.contains(&"dir fact"));
    assert!(result
        .memories
        .iter()
        .all(|m| !m.score_breakdown.below_threshold));
    assert_eq!(
        result.dropped_below_threshold, 3,
        "low criticality, and the root-wide `/` and `**/*.py` at 0.5"
    );

    // An ancestor directory matches at any depth: a service-wide memory
    // reaches a file two levels down.
    let deep = RetrievalQuery {
        path: Some("services/notifier/email.py".to_string()),
        ..query.clone()
    };
    let result = engine.query(&deep).await.unwrap();
    let got: Vec<&str> = result
        .memories
        .iter()
        .map(|m| m.memory.summary.as_str())
        .collect();
    assert_eq!(got, vec!["service root"], "{got:?}");
}

/// A logical-only context keeps the stricter rule for unscoped memories:
/// the neutral floor is not a scope match, so they still need the full
/// multiplied score.
#[test]
fn scope_only_gate_needs_a_real_scope_match() {
    use crate::scoring::{composite_score, ScoringContext};
    use crate::types::{Memory, MemoryType, Provenance};

    let config = crate::types::EngramConfig::default();
    let now = chrono::Utc::now();
    let mut m = Memory::new(MemoryType::Context, "s", "b", Provenance::human());
    m.criticality = 0.8;
    let logical = vec!["auth".to_string()];

    let unscoped = composite_score(
        &m,
        &ScoringContext::scope_only(None, &logical),
        &config,
        now,
    );
    assert_eq!(unscoped.gate_score, Some(unscoped.final_score));

    m.logical = vec!["auth.oauth".to_string()];
    let related = composite_score(
        &m,
        &ScoringContext::scope_only(None, &logical),
        &config,
        now,
    );
    assert!(related.final_score < 0.8);
    assert!((related.gate_score.unwrap() - 0.8).abs() < 1e-9);

    let no_context = composite_score(&m, &ScoringContext::scope_only(None, &[]), &config, now);
    assert_eq!(
        no_context.gate_score, None,
        "SessionStart shape is unchanged"
    );
}

#[tokio::test]
async fn filter_mode_has_no_rank_fallback() {
    let (_d, engine) = rank_threshold_fixture(&[("alpha", 0.30)], |_| {}).await;
    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Filter,
            query: Some("unrelated words".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(result.memories.is_empty());
    assert_eq!(result.dropped_below_threshold, 0);
}

#[tokio::test]
async fn scope_only_rank_uses_the_same_fallback() {
    let (_d, engine) = rank_threshold_fixture(&[("alpha", 0.30), ("beta", 0.20)], |_| {}).await;
    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Rank,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.retrieval_quality, "scope_only");
    assert_eq!(result.memories.len(), 2);
    assert!(result
        .memories
        .iter()
        .all(|m| m.score_breakdown.below_threshold));
}

#[tokio::test]
async fn rank_threshold_applies_to_the_reranked_score() {
    use crate::retrieval::reranker::RerankScore;
    // Scores any document containing "RESCUE" far above the rest.
    struct RescueReranker;
    #[async_trait::async_trait]
    impl Reranker for RescueReranker {
        async fn rerank(
            &self,
            _query: &str,
            documents: &[String],
        ) -> anyhow::Result<Vec<RerankScore>> {
            Ok(documents
                .iter()
                .enumerate()
                .map(|(index, d)| RerankScore {
                    index,
                    score: if d.contains("RESCUE") { 12.0 } else { -12.0 },
                })
                .collect())
        }
    }
    let (_d, engine) =
        rank_threshold_fixture(&[("RESCUE the right memory", 0.20), ("noise", 0.40)], |c| {
            c.rerank.enabled = true;
            c.rerank.weight = 0.5;
        })
        .await;
    let engine = engine.with_reranker(Arc::new(RescueReranker));
    let result = engine.query(&rank("unrelated words")).await.unwrap();
    // Pre-rerank 0.20 < 0.45 would have been cut before the cross-encoder
    // saw it; blended 0.5*0.20 + 0.5*sigmoid(12) ≈ 0.60 clears the threshold.
    assert_eq!(
        result.memories.len(),
        1,
        "{:?}",
        result
            .memories
            .iter()
            .map(|m| (&m.memory.summary, m.score))
            .collect::<Vec<_>>()
    );
    assert_eq!(result.memories[0].memory.summary, "RESCUE the right memory");
    assert!(!result.memories[0].score_breakdown.below_threshold);
    assert_eq!(result.dropped_below_threshold, 1);
}

fn stub_memory(summary: &str, criticality: f64) -> crate::types::Memory {
    use crate::types::{Memory, MemoryType, Provenance, Visibility};
    let mut m = Memory::new(MemoryType::Context, summary, "body", Provenance::human());
    m.visibility = Visibility::Shared;
    m.criticality = criticality;
    m
}

/// Regression (Bug A): membership in the vector-search top-k must not
/// decide the weight regime. Before the fix, an embedded memory that
/// missed the top-k over-fetch window fell to the degraded weights
/// (relevance at full weight), so a memory LESS similar to the query
/// outranked an equally critical memory that WAS similar — the in-top-k
/// memory's 1/(1+L2) semantic score tops out well below 1.0, dragging
/// its with_query score under the absent memory's bare criticality.
#[tokio::test]
async fn test_missed_topk_embedded_memory_scores_as_weak_semantic() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    // Semantically close to the query; same criticality as `far`.
    let close = stub_memory("closevec note", 0.8);
    // Orthogonal to the query. With max_results = 2 the over-fetch window
    // is 6, and `close` + 6 fillers are all nearer the query, so `far` is
    // guaranteed to miss the top-k.
    let far = stub_memory("unrelated note", 0.8);

    let mut all = vec![close.clone(), far.clone()];
    for i in 0..6 {
        all.push(stub_memory(&format!("fillvec note {i}"), 0.0));
    }
    for m in &all {
        store.create(m).await.unwrap();
    }

    let engine = RetrievalEngine::new(store, config)
        .with_embedding_provider(Arc::new(MarkerEmbeddingProvider));
    for m in &all {
        engine.embed_memory(m).await.unwrap();
    }

    // "queryvec" appears in no memory text, so keyword evidence is empty
    // and the semantic regime is isolated.
    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Rank,
            query: Some("queryvec".to_string()),
            max_results: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.retrieval_quality, "full");
    assert_eq!(result.memories.len(), 2);
    // Pre-fix order was [far (0.80, degraded), close (~0.70, semantic)].
    assert_eq!(
        result.memories[0].memory.id, close.id,
        "semantically close memory must rank first"
    );
    assert_eq!(
        result.memories[1].memory.id, far.id,
        "dissimilar memory must rank second (above the zero-criticality fillers)"
    );
    // Missed-top-k with chunks = weak evidence, not no evidence.
    assert_eq!(result.memories[1].score_breakdown.semantic, Some(0.0));
    assert!(result.memories[0].score_breakdown.semantic.unwrap() > 0.0);
    assert!(result.memories[0].score > result.memories[1].score);
}

/// A memory with NO embedding chunks (created while embeddings were
/// unavailable) must keep the no-evidence degraded path even when vector
/// search ran for the query — protecting the `None > Some(0.0)` semantics
/// asserted by scoring::composite::tests::test_semantic_none_vs_zero_differ
/// end-to-end through the engine.
#[tokio::test]
async fn test_unembedded_memory_keeps_no_evidence_path() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    let embedded = stub_memory("closevec note", 0.8);
    let unembedded = stub_memory("plain note", 0.8);
    store.create(&embedded).await.unwrap();
    store.create(&unembedded).await.unwrap();

    let engine = RetrievalEngine::new(store, config)
        .with_embedding_provider(Arc::new(MarkerEmbeddingProvider));
    // Only `embedded` gets chunks; `unembedded` simulates a memory created
    // while the embedding backend was down.
    engine.embed_memory(&embedded).await.unwrap();

    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Rank,
            query: Some("queryvec".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.retrieval_quality, "full");
    assert_eq!(result.memories.len(), 2);
    let by_id = |id: &str| {
        result
            .memories
            .iter()
            .find(|sm| sm.memory.id == id)
            .unwrap()
    };

    let un = by_id(&unembedded.id);
    assert!(
        un.score_breakdown.semantic.is_none(),
        "memory without chunks must keep semantic = None"
    );
    // Degraded weights: relevance at full weight → criticality (0.8).
    assert!(
        (un.score - 0.8).abs() < 0.01,
        "no-evidence memory should score ~0.8 (degraded), got {}",
        un.score
    );

    let emb = by_id(&embedded.id);
    assert!(
        emb.score_breakdown.semantic.unwrap() > 0.0,
        "embedded memory in the top-k must carry its semantic score"
    );
}

/// Regression: `max_results` is user-supplied, so `usize::MAX` must not
/// panic in the 3x vector-search over-fetch (debug overflow) or wrap to a
/// tiny window (release). The semantic path runs end-to-end via the stub
/// provider, exercising both the engine's `saturating_mul(3)` + cap and
/// the LanceDB layer's `saturating_mul(5)` + chunk-fetch clamp.
#[tokio::test]
async fn test_max_results_usize_max_does_not_panic() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    let close = stub_memory("closevec note", 0.8);
    let other = stub_memory("plain note", 0.5);
    store.create(&close).await.unwrap();
    store.create(&other).await.unwrap();

    let engine = RetrievalEngine::new(store, config)
        .with_embedding_provider(Arc::new(MarkerEmbeddingProvider));
    engine.embed_memory(&close).await.unwrap();
    engine.embed_memory(&other).await.unwrap();

    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Rank,
            query: Some("queryvec".to_string()),
            max_results: Some(usize::MAX),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.retrieval_quality, "full");
    assert_eq!(result.memories.len(), 2);
}

/// Regression (Bug B): when a memory has BOTH a keyword match and a
/// semantic score, both must contribute via the documented combined
/// with_keyword weights (0.45*kw + 0.30*sem + 0.25*relevance). Before the
/// fix, semantic presence routed scoring to with_semantic and the keyword
/// evidence was discarded, so the combined formula could never fire.
#[tokio::test]
async fn test_keyword_and_semantic_both_contribute() {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    // Both memories embed to the same vector as the query "zebra" (no
    // marker substring → orthogonal default vector), so sem = 1.0 for
    // both; only `both_signals` matches the keyword.
    let both_signals = stub_memory("zebra protocol decision", 0.8);
    let sem_only = stub_memory("samevec protocol decision", 0.8);
    store.create(&both_signals).await.unwrap();
    store.create(&sem_only).await.unwrap();

    let engine = RetrievalEngine::new(store, config.clone())
        .with_embedding_provider(Arc::new(MarkerEmbeddingProvider));
    engine.embed_memory(&both_signals).await.unwrap();
    engine.embed_memory(&sem_only).await.unwrap();

    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Rank,
            query: Some("zebra".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.retrieval_quality, "full");
    assert_eq!(result.memories.len(), 2);
    let by_id = |id: &str| {
        result
            .memories
            .iter()
            .find(|sm| sm.memory.id == id)
            .unwrap()
    };

    // Both signals recorded (pre-fix: keyword was None whenever a
    // semantic score existed).
    let combined = by_id(&both_signals.id);
    let kw = combined
        .score_breakdown
        .keyword
        .expect("keyword evidence must be recorded alongside semantic");
    let sem = combined
        .score_breakdown
        .semantic
        .expect("semantic evidence must be recorded alongside keyword");
    assert!(kw > 0.0);
    assert!(sem > 0.9, "identical vectors should give sem ~1.0: {sem}");

    // The final score is the combined with_keyword formula, or the
    // semantic-only formula when that is higher: a keyword hit never
    // lowers a score (scope and trust multipliers are 1.0 here: no scope
    // context, human provenance).
    let w = &config.retrieval.scoring.with_keyword;
    let with_keyword = w.keyword.unwrap() * kw
        + w.semantic.unwrap() * sem
        + w.relevance * combined.score_breakdown.relevance;
    let q = &config.retrieval.scoring.with_query;
    let semantic_only =
        q.semantic.unwrap() * sem + q.relevance * combined.score_breakdown.relevance;
    let expected = with_keyword.max(semantic_only);
    assert!(
        (combined.score - expected).abs() < 0.01,
        "combined score {} should be max(0.45*kw + 0.30*sem + 0.25*rel, 0.55*sem + 0.45*rel) = {}",
        combined.score,
        expected
    );

    // The semantic-only memory keeps the with_query path.
    let semantic_only = by_id(&sem_only.id);
    assert!(semantic_only.score_breakdown.keyword.is_none());
    assert!(semantic_only.score_breakdown.semantic.unwrap() > 0.9);
    // Keyword evidence never lowers the score for the same sem/crit.
    assert!(
        combined.score >= semantic_only.score - 1e-9,
        "keyword evidence must not lower the score: combined={} sem_only={}",
        combined.score,
        semantic_only.score
    );
}

// ─── Vector-search filter pushdown ───────────────────────────────────────

/// Shared setup for the pushdown tests: 8 untagged "closevec" fillers
/// (nearest to the query, zero criticality) that saturate the
/// `max_results = 2` → window-of-6 vector top-k, plus two tagged
/// memories — "fillvec" (mid similarity) and an orthogonal one — that
/// are both FARTHER from the query than every filler.
async fn pushdown_fixture() -> (
    tempfile::TempDir,
    RetrievalEngine,
    crate::types::Memory,
    crate::types::Memory,
) {
    use crate::types::EngramConfig;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();

    let mut config = EngramConfig::default();
    config.retrieval.relevance_threshold = 0.0;

    let mut tagged_mid = stub_memory("fillvec tagged note", 0.8);
    tagged_mid.tags = vec!["pick".to_string()];
    let mut tagged_far = stub_memory("plain tagged note", 0.8);
    tagged_far.tags = vec!["pick".to_string()];

    let mut all = vec![tagged_mid.clone(), tagged_far.clone()];
    for i in 0..8 {
        all.push(stub_memory(&format!("closevec filler {i}"), 0.0));
    }
    for m in &all {
        store.create(m).await.unwrap();
    }

    let engine = RetrievalEngine::new(store, config)
        .with_embedding_provider(Arc::new(MarkerEmbeddingProvider));
    for m in &all {
        engine.embed_memory(m).await.unwrap();
    }

    (temp_dir, engine, tagged_mid, tagged_far)
}

/// Regression: vector search used to run over the WHOLE chunks table, so
/// with a narrow tag filter the over-fetch window (max_results*3 = 6)
/// was saturated by filtered-out fillers and the surviving candidates
/// degraded to weak (sem = 0.0) evidence — losing their real semantic
/// ordering. With pushdown, the filtered survivors keep real scores.
#[tokio::test]
async fn test_vector_search_pushdown_keeps_real_semantic_scores() {
    let (_dir, engine, tagged_mid, tagged_far) = pushdown_fixture().await;

    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Rank,
            query: Some("queryvec".to_string()),
            tags: Some(vec!["pick".to_string()]),
            max_results: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.retrieval_quality, "full");
    assert_eq!(result.memories.len(), 2, "both tagged memories survive");

    // Real semantic evidence for both survivors (pre-fix: Some(0.0)).
    let by_id = |id: &str| {
        result
            .memories
            .iter()
            .find(|sm| sm.memory.id == id)
            .unwrap()
    };
    let mid_sem = by_id(&tagged_mid.id)
        .score_breakdown
        .semantic
        .expect("survivor must carry semantic evidence");
    let far_sem = by_id(&tagged_far.id)
        .score_breakdown
        .semantic
        .expect("survivor must carry semantic evidence");
    assert!(
        mid_sem > 0.0,
        "filtered survivor must get its REAL semantic score, got {mid_sem}"
    );
    assert!(far_sem > 0.0, "got {far_sem}");
    assert!(
        mid_sem > far_sem,
        "semantic ordering must be preserved within the filter set: \
             mid={mid_sem} far={far_sem}"
    );
    // And the ranking follows: the more-similar survivor ranks first.
    assert_eq!(
        result.memories[0].memory.id, tagged_mid.id,
        "more-similar survivor must rank first"
    );
}

/// Regression: a filter that is ENTIRELY served by the LanceDB predicate
/// pushdown (here `types`) must still restrict the vector search. After
/// the pushdown landed, `total_entry_count` counted post-predicate rows,
/// so `filtered == total` and the restriction never fired — the window
/// saturated with filtered-out fillers and both survivors degraded to
/// weak (sem = 0.0) evidence, exactly the mis-ranking the restriction
/// exists to prevent. Tags (the sibling test above) never regressed
/// because they are filtered Rust-side.
#[tokio::test]
async fn test_vector_search_pushdown_only_filter_still_restricts() {
    use crate::types::MemoryType;

    let (_dir, engine, mut tagged_mid, mut tagged_far) = pushdown_fixture().await;

    // Re-type the two tagged memories as hazards so a `types` filter —
    // which is pushed down into the scan predicate — is the only
    // narrowing signal. The 8 closevec fillers stay Context.
    for m in [&mut tagged_mid, &mut tagged_far] {
        m.type_ = MemoryType::Hazard;
        let mut update = crate::types::MemoryUpdate::new();
        update.type_ = Some(MemoryType::Hazard);
        engine.store().update(&m.id, update).await.unwrap();
        engine.embed_memory(m).await.unwrap();
    }

    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Rank,
            query: Some("queryvec".to_string()),
            types: Some(vec![MemoryType::Hazard]),
            max_results: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.memories.len(), 2, "both hazards survive");
    let by_id = |id: &str| {
        result
            .memories
            .iter()
            .find(|sm| sm.memory.id == id)
            .unwrap()
    };
    let mid_sem = by_id(&tagged_mid.id)
        .score_breakdown
        .semantic
        .expect("survivor must carry semantic evidence");
    let far_sem = by_id(&tagged_far.id)
        .score_breakdown
        .semantic
        .expect("survivor must carry semantic evidence");
    assert!(
        mid_sem > 0.0,
        "pushdown-filtered survivor must get its REAL semantic score, got {mid_sem}"
    );
    assert!(
        mid_sem > far_sem,
        "semantic ordering must be preserved within the filter set: \
             mid={mid_sem} far={far_sem}"
    );
    assert_eq!(
        result.memories[0].memory.id, tagged_mid.id,
        "more-similar survivor must rank first"
    );
}

/// Fallback: with no filters the candidate set isn't narrowed, so the
/// whole-store search runs unchanged — fillers (nearest to the query)
/// own the window and the top results, with real semantic scores, while
/// out-of-window memories keep the weak (sem = 0.0) evidence path
/// covered by `test_missed_topk_embedded_memory_scores_as_weak_semantic`.
#[tokio::test]
async fn test_vector_search_no_filters_falls_back_to_whole_store() {
    let (_dir, engine, _tagged_mid, _tagged_far) = pushdown_fixture().await;

    let result = engine
        .query(&RetrievalQuery {
            mode: RetrievalMode::Rank,
            query: Some("queryvec".to_string()),
            max_results: Some(2),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(result.retrieval_quality, "full");
    assert_eq!(result.memories.len(), 2);
    for sm in &result.memories {
        assert!(
            sm.memory.summary.contains("closevec"),
            "whole-store search: nearest fillers must win the top-k, got {}",
            sm.memory.summary
        );
        assert!(
            sm.score_breakdown.semantic.unwrap() > 0.0,
            "in-window memories carry real semantic scores"
        );
    }
}
/// [`ConstantEmbedding`] that counts its calls, or always fails.
struct CountingEmbedding {
    embeds: std::sync::atomic::AtomicUsize,
    batches: std::sync::atomic::AtomicUsize,
    fail: bool,
}

impl CountingEmbedding {
    fn new(fail: bool) -> Arc<Self> {
        Arc::new(Self {
            embeds: Default::default(),
            batches: Default::default(),
            fail,
        })
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for CountingEmbedding {
    async fn embed(&self, t: &str) -> anyhow::Result<Vec<f32>> {
        self.embeds
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("stub embed failure");
        }
        ConstantEmbedding.embed(t).await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.batches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("stub embed failure");
        }
        ConstantEmbedding.embed_batch(texts).await
    }
    fn dimensions(&self) -> usize {
        384
    }
    fn max_tokens(&self) -> usize {
        256
    }
    fn model_id(&self) -> String {
        "onnx/constant-stub".to_string()
    }
}

/// [`ConstantEmbedding`] whose `embed_batch` signals that it started and
/// then waits to be released: a pause point inside a detached ingest.
struct PausingEmbedding {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl PausingEmbedding {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        })
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for PausingEmbedding {
    async fn embed(&self, t: &str) -> anyhow::Result<Vec<f32>> {
        ConstantEmbedding.embed(t).await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.entered.notify_one();
        self.release.notified().await;
        ConstantEmbedding.embed_batch(texts).await
    }
    fn dimensions(&self) -> usize {
        384
    }
    fn max_tokens(&self) -> usize {
        256
    }
    fn model_id(&self) -> String {
        "onnx/constant-stub".to_string()
    }
}

/// Start a detached ingest of a fresh memory, pause it inside the embed,
/// apply `write` to the memory, release, and return the store.
async fn ingest_racing_a_write(
    write: impl FnOnce(&mut Memory) + Send + 'static,
) -> (tempfile::TempDir, MemoryStore, String) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let m = Memory::new(
        MemoryType::Decision,
        "racing summary",
        "racing body",
        crate::types::Provenance::human(),
    );
    store.create(&m).await.unwrap();
    let provider = PausingEmbedding::new();
    let dyn_provider: Arc<dyn EmbeddingProvider> = provider.clone();
    let engine = RetrievalEngine::new(store.clone(), EngramConfig::default())
        .with_embedding_provider(dyn_provider);
    let handle = engine.spawn_ingest(m.clone(), None).unwrap();
    provider.entered.notified().await;
    store
        .update_with(&m.id, move |mem| {
            write(mem);
            Ok(())
        })
        .await
        .unwrap();
    provider.release.notify_one();
    handle.await.unwrap();
    (temp_dir, store, m.id)
}

/// A metadata-only write (here a link) between the snapshot and the
/// chunk write bumps `updated_at` but changes no vector: the vectors must
/// still be written. The `updated_at` guard dropped them.
#[tokio::test]
async fn embed_survives_a_metadata_write_during_ingest() {
    let (_t, store, id) = ingest_racing_a_write(|m| {
        m.links.push(crate::types::Link::new(
            "some-other-memory",
            crate::types::LinkKind::RelatedTo,
            crate::types::LinkOrigin::Human,
        ));
    })
    .await;
    assert!(store.has_chunks_for(&id).await.unwrap(), "vectors kept");
    let row = store
        .list_for_filtering()
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.id == id)
        .unwrap();
    assert!(row.has_embedding);
}

/// A detached ingest is counted from the moment `spawn_ingest` returns
/// until its task ends, so the MCP shutdown drain can wait for it.
#[tokio::test]
async fn spawn_ingest_is_counted_until_it_finishes() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let m = Memory::new(
        MemoryType::Decision,
        "counted",
        "body",
        crate::types::Provenance::human(),
    );
    store.create(&m).await.unwrap();
    let provider = PausingEmbedding::new();
    let dyn_provider: Arc<dyn EmbeddingProvider> = provider.clone();
    let engine = RetrievalEngine::new(store.clone(), EngramConfig::default())
        .with_embedding_provider(dyn_provider);
    let before = super::super::pending_ingests();
    let handle = engine.spawn_ingest(m.clone(), None).unwrap();
    assert_eq!(super::super::pending_ingests(), before + 1);
    provider.entered.notified().await;
    assert_eq!(super::super::pending_ingests(), before + 1);
    provider.release.notify_one();
    assert_eq!(
        super::super::drain_ingests(std::time::Duration::from_secs(30)).await,
        0
    );
    handle.await.unwrap();
    assert_eq!(super::super::pending_ingests(), before);
    assert!(store.has_chunks_for(&m.id).await.unwrap());
}

/// A real text change in between still refuses the stale vectors.
#[tokio::test]
async fn embed_is_dropped_when_the_text_changed_during_ingest() {
    let (_t, store, id) = ingest_racing_a_write(|m| {
        m.content = "a different body".to_string();
    })
    .await;
    assert!(
        !store.has_chunks_for(&id).await.unwrap(),
        "stale vectors must not be written"
    );
}

async fn ingest_fixture(
    provider: Arc<CountingEmbedding>,
    similar_candidates: usize,
) -> (
    tempfile::TempDir,
    MemoryStore,
    RetrievalEngine,
    Vec<String>,
    Memory,
) {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
        .await
        .unwrap();
    let mut ids = Vec::new();
    for summary in ["one", "two", "three", "four"] {
        let m = Memory::new(
            MemoryType::Decision,
            summary,
            "body",
            crate::types::Provenance::human(),
        );
        store.create(&m).await.unwrap();
        embed_memory_with(&ConstantEmbedding, &store, &m, 256, true, None)
            .await
            .unwrap();
        ids.push(m.id);
    }
    // One neighbour is history: never proposed.
    store
        .invalidate_with(&ids[3], None, chrono::Utc::now())
        .await
        .unwrap();
    let mut config = EngramConfig::default();
    config.links.similar_candidates = similar_candidates;
    let dyn_provider: Arc<dyn EmbeddingProvider> = provider;
    let engine = RetrievalEngine::new(store.clone(), config).with_embedding_provider(dyn_provider);
    let probe = Memory::new(
        MemoryType::Decision,
        "probe",
        "body",
        crate::types::Provenance::human(),
    );
    store.create(&probe).await.unwrap();
    (temp_dir, store, engine, ids, probe)
}

/// The ingest's neighbour search reuses the vector its own embed made:
/// one `embed_batch`, no `embed`. The follow-up gets live neighbours only,
/// never the memory itself, at most `similar_candidates`.
#[tokio::test]
async fn spawn_ingest_hands_live_neighbours_to_the_follow_up_without_a_second_embed() {
    let provider = CountingEmbedding::new(false);
    let (_t, _store, engine, ids, probe) = ingest_fixture(Arc::clone(&provider), 2).await;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let follow_up: IngestFollowUp = Box::new(move |_store, memory, findings| {
        Box::pin(async move {
            let _ = tx.send((memory.id, findings));
        })
    });
    engine
        .spawn_ingest(probe.clone(), Some(follow_up))
        .unwrap()
        .await
        .unwrap();
    let (id, findings) = rx.await.expect("follow-up ran");
    assert_eq!(id, probe.id);
    assert_eq!(findings.neighbours.len(), 2, "{:?}", findings.neighbours);
    for n in &findings.neighbours {
        assert!(n.id != probe.id && n.id != ids[3], "{n:?}");
        assert!(ids[..3].contains(&n.id));
    }
    assert!(findings.nli_cleared.is_empty(), "NLI is off");
    assert_eq!(
        provider.batches.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(provider.embeds.load(std::sync::atomic::Ordering::SeqCst), 0);

    // The inline path: `embed_for_ingest` + `ingest_findings`, the same
    // single embed.
    let query = engine.embed_for_ingest(&probe).await.unwrap().unwrap();
    let findings = engine.ingest_findings(&probe, query).await;
    assert_eq!(findings.neighbours.len(), 2);
    assert_eq!(provider.embeds.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn spawn_ingest_skips_the_follow_up_when_the_embed_fails() {
    let provider = CountingEmbedding::new(true);
    let (_t, _store, engine, _ids, probe) = ingest_fixture(provider, 5).await;
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    let follow_up: IngestFollowUp = Box::new(move |_s, _m, _f| {
        Box::pin(async move {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    });
    engine
        .spawn_ingest(probe, Some(follow_up))
        .unwrap()
        .await
        .unwrap();
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
}

/// With NLI on, a neighbour it classifies as not contradictory is
/// `nli_cleared` (eligible for `duplicates`); a contradictory one is not.
#[tokio::test]
async fn ingest_findings_records_which_neighbours_nli_cleared() {
    let provider = CountingEmbedding::new(false);
    let (_t, store, _engine, _ids, probe) = ingest_fixture(Arc::clone(&provider), 5).await;
    let mut config = EngramConfig::default();
    config.nli.enabled = true;
    config.nli.similarity_threshold = 0.0;
    let dyn_provider: Arc<dyn EmbeddingProvider> = provider;
    let contradicting = RetrievalEngine::new(store.clone(), config.clone())
        .with_embedding_provider(Arc::clone(&dyn_provider))
        .with_nli_provider(Arc::new(AlwaysContradicts));
    let q = contradicting
        .embed_for_ingest(&probe)
        .await
        .unwrap()
        .unwrap();
    let f = contradicting.ingest_findings(&probe, q).await;
    assert_eq!(f.contradictions.len(), 3);
    assert!(f.nli_cleared.is_empty());
}
