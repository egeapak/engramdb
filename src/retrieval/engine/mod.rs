//! Core retrieval engine implementation.
//!
//! The retrieval engine orchestrates the process of finding relevant memories based on
//! contextual queries. It combines multiple retrieval strategies including scope matching,
//! semantic search (when embeddings are available), and keyword search.

use super::filters::{apply_index_filters, build_filter_predicate, SearchFilters};
use super::reranker::Reranker;
use super::signals::{GraphSnapshot, UsageCache};
use crate::embeddings::EmbeddingProvider;
use crate::nli::{NliProvider, NliResult};
use crate::scoring::{
    composite_score_target, composite_score_target_ignore_decay, LinkVia, ScoreBreakdown,
    ScoreSignals, ScoreTarget, ScoringContext,
};
use crate::storage::{MemoryStore, Result};
use crate::types::{EngramConfig, Epistemic, KeywordStems, Memory, MemoryType, Situation};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;

/// Upper bound on the vector-search over-fetch window (unique memories
/// requested from LanceDB per semantic query).
///
/// The window is `max_results * 3`, but `max_results` is user-supplied and
/// unvalidated; without a cap a huge value would turn one query into a full
/// scan-and-sort of every chunk. Memories outside the window are not dropped
/// — they are scored with weak semantic evidence (`sem = 0.0`, see Step 4.5
/// in [`RetrievalEngine::query`]) — so the cap bounds work, not recall.
const VECTOR_SEARCH_WINDOW_CAP: usize = 1000;

/// Maximum size of the filtered candidate set that gets pushed down into the
/// vector search as a `memory_id IN (...)` predicate.
///
/// When the index filters (type/tag/path/criticality, filter-mode logical,
/// expiry) narrow the candidates to at most this many memories, the vector
/// search is restricted to them so the top-k window is spent entirely on
/// real candidates — otherwise narrow filters let filtered-out memories
/// saturate the window and survivors degrade to weak (`sem = 0.0`) evidence.
/// Above this size the predicate would be a huge SQL `IN` list for little
/// gain (a large candidate set can't be saturated easily), so we fall back
/// to the whole-store search.
const VECTOR_RESTRICT_MAX_IDS: usize = 500;

/// Candidate count at or above which the per-memory scoring and keyword-stem
/// passes go through the rayon pool.
///
/// Both are pure per-row maps over shared read-only state, so they parallelize
/// exactly; the only question is whether the work is worth entering the pool
/// for. At ~0.5µs of scoring per row the crossover is a few dozen rows, and
/// most queries in a small store are below it — where the sequential branch
/// keeps the previous behaviour byte for byte and costs nothing. Measured
/// gains start around 100 rows (~1.5x) and reach ~3.5x at 5,000
/// (`benches/parallel_simd.rs`).
const PARALLEL_SCORE_MIN: usize = 64;

/// Texts per `embed_batch` call in [`RetrievalEngine::embed_texts`].
///
/// The gain from batching is flat well before this (2.5x at 64, and the ONNX
/// backend pads every row in a batch to the longest one, so very wide batches
/// start wasting compute on padding). The bound matters more for the other two
/// backends: the daemon puts a whole batch in one socket message and Ollama in
/// one HTTP body, and the consolidation pass can hand over 500 texts.
const EMBED_BATCH_CHUNK: usize = 64;

/// Detail level for retrieved memories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailLevel {
    /// Just summaries from index (minimal data)
    Summary,
    /// Summary + content (default)
    #[default]
    Content,
    /// Everything including details field
    Full,
}

/// Mode controlling whether query returns every passing memory (Rank) or
/// only those with a positive relevance signal (Filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RetrievalMode {
    /// Return every memory passing the type/tag/criticality filters, ranked
    /// by composite score. The "context-aware browsing" flow.
    #[default]
    Rank,

    /// Require at least one positive relevance signal (keyword, semantic,
    /// scope proximity, or tag match). Memories with zero signal are dropped.
    /// Used for specific-term lookups.
    Filter,
}

/// Query parameters for retrieval.
#[derive(Debug, Clone, Default)]
pub struct RetrievalQuery {
    /// Mode: Rank (browse by context) or Filter (require positive signal).
    pub mode: RetrievalMode,

    /// Physical scope - current file path
    pub path: Option<String>,

    /// Logical scopes - dot-notation domains. In Rank mode they contribute
    /// to scoring only; in Filter mode they act as a hard hierarchical
    /// filter (see Step 2.5a in [`RetrievalEngine::query`]).
    pub logical: Vec<String>,

    /// Search query text (for semantic similarity + keyword search)
    pub query: Option<String>,

    /// Filter by memory types
    pub types: Option<Vec<MemoryType>>,

    /// Filter by epistemic class (hard filter, index-level, both modes —
    /// exactly like `types`).
    pub epistemic: Option<Vec<Epistemic>>,

    /// Include invalidated memories (closed validity windows, §2.4). Default
    /// false: excluded index-level via
    /// `invalidated_at IS NULL OR invalidated_at > now`, mirroring expiry —
    /// a future-dated (scheduled) invalidation is still returned.
    pub include_invalidated: Option<bool>,

    /// The querying agent's situation (§6.2), threaded into every scoring
    /// context. `None` ⇒ neutral multiplier (all pre-epistemic behavior).
    pub situation: Option<Situation>,

    /// Filter by tags
    pub tags: Option<Vec<String>>,

    /// Minimum criticality threshold
    pub min_criticality: Option<f64>,

    /// Maximum number of results to return
    pub max_results: Option<usize>,

    /// Include expired memories
    pub include_expired: Option<bool>,

    /// Detail level for results
    pub detail_level: DetailLevel,

    /// Rank mode only: overrides `retrieval.rank_fallback` (how many weak
    /// matches to return, marked `below_threshold`, when nothing clears the
    /// threshold). Hooks pass `Some(0)`: they inject "relevant memories" and
    /// must never present a weak match as one.
    pub rank_fallback: Option<usize>,

    /// One-hop link expansion: add up to this many neighbours of the direct
    /// hits, reached over confirmed `supports` / `derived_from` /
    /// `related_to` edges (see `retrieval::signals::expansion_follows`).
    /// `None` ⇒ `retrieval.expand_links` (default 0 = off); capped at
    /// [`crate::types::MAX_EXPAND_LINKS`]. Hooks pass `Some(0)`: expansion
    /// costs an edges query, and the hooks' latency budget is per tool call.
    ///
    /// A neighbour is added only if this query would have been allowed to
    /// return it: it must be in the candidate set that every hard filter
    /// (type, tags, epistemic class, criticality, expiry, validity, the
    /// filter-mode scope filters) already narrowed. It skips the relevance
    /// signal and threshold — that is the point — and is marked with
    /// `ScoreBreakdown::via`.
    pub expand_links: Option<usize>,

    /// Ids that keep their place in the ranked cut but are neither loaded
    /// nor returned: the caller already has them (a hook's "already shown
    /// this session"). Ranking, the `max_results` cut and `total` are those
    /// of the same query without it, so a skipped id never lets a
    /// lower-ranked memory into the result in its place — the result is the
    /// unskipped result minus the skipped ids, without reading their files.
    /// Meant for queries without link expansion (a skipped hit still
    /// expands, and its neighbour would then be returned without it).
    pub skip_load: Option<std::collections::HashSet<String>>,
}

/// A memory with its computed relevance score.
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    /// The memory data
    pub memory: Memory,
    /// Relevance score (0.0 to 1.0+, higher is more relevant)
    pub score: f64,
    /// Detailed breakdown of score components
    pub score_breakdown: ScoreBreakdown,
}

/// A scored candidate carrying only what threshold/sort/rerank/truncate need,
/// so the full [`Memory`] is *not* cloned for every candidate — only the
/// survivors are materialized into [`ScoredMemory`] after truncation.
///
/// `id` (a 16-char store id) is cloned instead of the whole memory (content,
/// details, scope vecs); the actual memory is looked up from `memory_map` at
/// materialization time. See finding performance-0.
struct ScoredCandidate {
    id: String,
    score: f64,
    breakdown: ScoreBreakdown,
}

/// Result of a retrieval operation.
#[derive(Debug, Clone)]
pub struct RetrievalResult {
    /// Retrieved memories with scores, sorted by score descending
    pub memories: Vec<ScoredMemory>,

    /// Total number of memories before limit was applied
    pub total: usize,

    /// Retrieval-quality label describing which scoring signals were
    /// available for the query. One of:
    /// - `"full"`           — query text + embeddings contributed
    /// - `"keyword_only"`   — query text, keyword matched, no embeddings
    /// - `"no_query_signals"` — query text provided but nothing matched
    /// - `"scope_only"`     — no query text; scope / filter-only ranking
    pub retrieval_quality: String,

    /// Rank mode: candidates removed by the relevance threshold. Always
    /// reported, so the cut is a declared loss, never a silent one. When it
    /// equals every candidate, `memories` holds the rank fallback instead
    /// (each marked `below_threshold`).
    pub dropped_below_threshold: usize,
}

/// Main retrieval engine for EngramDB.
///
/// Coordinates memory retrieval by combining storage access, optional semantic search,
/// optional cross-encoder reranking, and relevance scoring. The engine can operate
/// with or without embeddings and reranking, gracefully degrading when unavailable.
pub struct RetrievalEngine {
    store: MemoryStore,
    config: EngramConfig,
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    nli_provider: Option<Arc<dyn NliProvider>>,
    reranker: Option<Arc<dyn Reranker>>,
    /// Cached abstractive (T5) title generator. `Some` only when
    /// `title.strategy = "t5"`; the model session (or pool) is loaded once
    /// into the provider bundle so `create` doesn't rebuild it per call.
    title_provider: Option<Arc<dyn crate::title::TitleGenerator>>,
    /// Optional runtime stats collector. When `Some`, the engine pushes
    /// stage timings and query outcomes to it. `None` disables all telemetry.
    stats: Option<Arc<crate::telemetry::StatsCollector>>,
    /// Project ID used as the partition key for telemetry. Required whenever
    /// `stats` is `Some`; ignored otherwise.
    project_id: Option<String>,
    /// Optional session ID — recorded with every event, used to compute
    /// followup rate and unique-session count.
    session_id: Option<String>,
    /// Handle on the process-wide, per-store usage views the usage boost
    /// reads, refreshed at most every `signals::USAGE_CACHE_TTL` (read only
    /// when `usage_weight > 0`).
    usage_cache: UsageCache,
}

impl RetrievalEngine {
    /// Create a new retrieval engine.
    ///
    /// Embeddings are not enabled by default. Use [`with_embedding_provider`](Self::with_embedding_provider)
    /// to add semantic search capabilities. Vector storage is handled by the
    /// MemoryStore's integrated LanceDB.
    pub fn new(store: MemoryStore, config: EngramConfig) -> Self {
        Self {
            store,
            config,
            embedding_provider: None,
            nli_provider: None,
            reranker: None,
            title_provider: None,
            stats: None,
            project_id: None,
            session_id: None,
            usage_cache: UsageCache,
        }
    }

    /// Attach a runtime stats collector. The engine will push stage timings
    /// (`embed`, `vector_search`, `score`, `rerank`, `query.total`,
    /// `create.chunk_text`, `create.embed_batch`, `create.upsert_chunks`)
    /// and query outcomes to it. Returns self for method chaining.
    pub fn with_stats(mut self, stats: Arc<crate::telemetry::StatsCollector>) -> Self {
        self.stats = Some(stats);
        self
    }

    /// Set the project ID used as the partition key for telemetry. No effect
    /// unless [`with_stats`](Self::with_stats) is also called. Returns self
    /// for method chaining.
    pub fn with_project_id(mut self, project_id: String) -> Self {
        self.project_id = Some(project_id);
        self
    }

    /// Bind the session ID for telemetry events emitted from this engine.
    /// Used by the collector to compute followup rate and unique-session
    /// count. Returns self for method chaining.
    pub fn with_session_id(mut self, session_id: Option<String>) -> Self {
        self.session_id = session_id;
        self
    }

    /// Internal: record a stage timing if telemetry is wired up.
    fn record_stage(&self, stage: &'static str, ms: f64) {
        if let (Some(stats), Some(pid)) = (self.stats.as_ref(), self.project_id.as_ref()) {
            stats.record_stage(pid, stage, ms, self.session_id.as_deref());
        }
    }

    /// Internal: record a query outcome if telemetry is wired up.
    fn record_query_outcome(&self, hit: bool, quality: &str) {
        if let (Some(stats), Some(pid)) = (self.stats.as_ref(), self.project_id.as_ref()) {
            stats.record_query_outcome(pid, hit, quality, self.session_id.as_deref());
        }
    }

    /// Build an [`IngestTelemetry`] from this engine when telemetry is
    /// wired up. Used by both `embed_memory` (synchronous) and
    /// `spawn_ingest` (spawned task) so create-side stage timings flow
    /// through one path.
    pub(crate) fn ingest_telemetry(&self) -> Option<IngestTelemetry> {
        match (self.stats.as_ref(), self.project_id.as_ref()) {
            (Some(stats), Some(pid)) => Some(IngestTelemetry {
                stats: stats.clone(),
                project_id: pid.clone(),
                session_id: self.session_id.clone(),
            }),
            _ => None,
        }
    }

    /// Add an embedding provider to the retrieval engine.
    ///
    /// Enables semantic search capabilities. Vector storage is handled by
    /// the MemoryStore's integrated LanceDB. Returns self for method chaining.
    pub fn with_embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Add a cross-encoder reranker to the retrieval engine.
    ///
    /// When configured (and `config.rerank.enabled` is true), retrieved results
    /// are re-scored using the cross-encoder before being returned. The reranker
    /// may run in-process or be delegated to the shared embedding daemon.
    pub fn with_reranker(mut self, reranker: Arc<dyn Reranker>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Attach a cached abstractive (T5) title generator.
    ///
    /// Wired only when `title.strategy = "t5"`. The create path uses this
    /// pre-loaded session/pool instead of rebuilding T5 per call; keyword
    /// and `none` titling never reach here.
    pub fn with_title_provider(mut self, provider: Arc<dyn crate::title::TitleGenerator>) -> Self {
        self.title_provider = Some(provider);
        self
    }

    /// The cached abstractive title generator, if one is wired (i.e.
    /// `title.strategy = "t5"` and the model loaded). `None` ⇒ the create
    /// path falls back to building the configured strategy ad hoc.
    pub fn title_generator(&self) -> Option<&Arc<dyn crate::title::TitleGenerator>> {
        self.title_provider.as_ref()
    }

    /// Check if embeddings are available.
    ///
    /// Returns true if an embedding provider is configured. Vector storage
    /// is always available via the MemoryStore's LanceDB.
    pub fn embeddings_available(&self) -> bool {
        self.embedding_provider.is_some()
    }

    /// Configured maximum summary length (`[content].summary_max_chars`), used
    /// by the create/update/compress paths to validate/truncate summaries.
    pub fn summary_max_chars(&self) -> usize {
        self.config.content.summary_max_chars
    }

    /// What this memory's embed digest **would** be if it were embedded right
    /// now, without running the model.
    ///
    /// This is the whole basis of `reindex --dry-run`'s stale-vector report:
    /// comparing it to the digest stored alongside the vectors answers "are
    /// these vectors still the ones this text, this model and this chunk width
    /// produce?" — and answers it for the price of a hash.
    ///
    /// Composition is deliberately routed through the *same* [`embedding_texts`]
    /// and [`effective_chunk_tokens`] the write path uses. A second, parallel
    /// derivation here would drift from the real one and the dry run would
    /// confidently report the wrong answer; that is exactly the failure mode
    /// this digest exists to detect, so it must not be reintroduced by the
    /// detector.
    ///
    /// `None` when no embedding provider is configured — with nothing to embed
    /// with there is no expected digest, which is a different statement from
    /// "the vectors are stale".
    pub fn expected_embed_digest(&self, memory: &Memory) -> Option<String> {
        let provider = self.embedding_provider.as_ref()?;
        let chunk_tokens =
            effective_chunk_tokens(self.config.embeddings.max_tokens, provider.max_tokens());
        let metadata_vector = self.config.embeddings.metadata_vector;
        let texts = embedding_texts(memory, chunk_tokens, metadata_vector);
        // An empty memory has its chunks deleted rather than embedded, so it
        // has no digest to match against.
        if texts.is_empty() {
            return None;
        }
        Some(embed_digest(
            &texts,
            provider.as_ref(),
            metadata_vector,
            chunk_tokens,
        ))
    }

    /// Identity of the embedding model in use, for stamping the store
    /// after a (re)embed. `None` when embeddings are disabled. Includes the
    /// embed-text composition (from `embeddings.metadata_vector`) so a later
    /// composition flip is detected the same way a model swap is.
    pub fn embedding_fingerprint(&self) -> Option<crate::storage::EmbeddingFingerprint> {
        self.embedding_provider
            .as_ref()
            .map(|p| crate::storage::EmbeddingFingerprint {
                model: p.model_id(),
                dimensions: p.dimensions(),
                composition: self.config.embeddings.composition_id(),
            })
    }

    /// Add an NLI provider to the retrieval engine.
    ///
    /// Enables automatic contradiction detection between memories.
    /// Returns self for method chaining.
    pub fn with_nli_provider(mut self, provider: Arc<dyn NliProvider>) -> Self {
        self.nli_provider = Some(provider);
        self
    }

    /// Check if NLI contradiction detection is available and enabled.
    pub fn nli_available(&self) -> bool {
        self.nli_provider.is_some() && self.config.nli.enabled
    }

    /// Check if reranking is available and enabled.
    pub fn reranking_available(&self) -> bool {
        self.reranker.is_some() && self.config.rerank.enabled
    }

    /// Detect contradictions between a new memory and existing similar memories.
    ///
    /// Uses vector search to find similar candidates, then NLI classification
    /// to detect contradictions. Returns a list of (memory_id, NliResult) pairs
    /// where the contradiction probability exceeds the configured threshold.
    ///
    /// Returns an empty vec if NLI is disabled, unavailable, or embeddings are missing.
    pub async fn detect_contradictions(
        &self,
        memory: &Memory,
    ) -> anyhow::Result<Vec<(String, NliResult)>> {
        let nli = match &self.nli_provider {
            Some(p) if self.config.nli.enabled => p,
            _ => return Ok(vec![]),
        };

        let embedding_provider = match &self.embedding_provider {
            Some(p) => p,
            None => return Ok(vec![]),
        };

        detect_contradictions_with(
            nli.as_ref(),
            embedding_provider.as_ref(),
            &self.store,
            &self.config.nli,
            memory,
        )
        .await
    }

    /// Chunk, embed, and upsert a memory's vectors into the store.
    ///
    /// Splits the memory's text into chunks that fit the provider's token limit,
    /// batch-embeds them, and stores the resulting vectors in the LanceDB chunks
    /// table. Does nothing if no embedding provider is configured.
    ///
    /// # Arguments
    /// * `memory` - The memory to embed
    pub async fn embed_memory(&self, memory: &Memory) -> anyhow::Result<()> {
        if let Some(provider) = &self.embedding_provider {
            let chunk_tokens =
                effective_chunk_tokens(self.config.embeddings.max_tokens, provider.max_tokens());
            embed_memory_with(
                provider.as_ref(),
                &self.store,
                memory,
                chunk_tokens,
                self.config.embeddings.metadata_vector,
                self.ingest_telemetry().as_ref(),
            )
            .await?;
        }
        Ok(())
    }

    /// [`Self::embed_memory`], returning the vector an ingest searches with
    /// (the unit mean of the memory's chunk vectors, [`mean_direction`]) so
    /// [`Self::ingest_findings`] needs no embed of its own. `Ok(None)` when no
    /// provider is configured or the memory has no text to embed.
    pub async fn embed_for_ingest(&self, memory: &Memory) -> anyhow::Result<Option<Vec<f32>>> {
        let Some(provider) = &self.embedding_provider else {
            return Ok(None);
        };
        let chunk_tokens =
            effective_chunk_tokens(self.config.embeddings.max_tokens, provider.max_tokens());
        embed_memory_with(
            provider.as_ref(),
            &self.store,
            memory,
            chunk_tokens,
            self.config.embeddings.metadata_vector,
            self.ingest_telemetry().as_ref(),
        )
        .await
    }

    /// The vectors for a memory that is **not written yet**, so the caller can
    /// write file, chunks and row in one [`MemoryStore::create_with_chunks`]
    /// (one memories-table commit instead of `create` and then the chunk
    /// upsert's `has_embedding` flip). Nothing is written here; pass the
    /// result's `fingerprint` to [`Self::stamp_first_embed`] once the create
    /// has committed. `Ok(None)` when no provider is configured or the memory
    /// has no text to embed — the caller then creates without vectors.
    pub async fn embed_unwritten(&self, memory: &Memory) -> anyhow::Result<Option<NewVectors>> {
        let Some(provider) = &self.embedding_provider else {
            return Ok(None);
        };
        let chunk_tokens =
            effective_chunk_tokens(self.config.embeddings.max_tokens, provider.max_tokens());
        let metadata_vector = self.config.embeddings.metadata_vector;
        let telemetry = self.ingest_telemetry();
        let t = std::time::Instant::now();
        let chunks = embedding_texts(memory, chunk_tokens, metadata_vector);
        if let Some(t_) = telemetry.as_ref() {
            t_.record("create.chunk_text", t.elapsed().as_secs_f64() * 1000.0);
        }
        if chunks.is_empty() {
            return Ok(None);
        }
        // Decided before the write, as in `embed_memory_with`: afterwards our
        // own chunks make the store non-empty.
        let unstamped = matches!(self.store.embedding_fingerprint().await, Ok(None))
            && matches!(self.store.has_any_chunks().await, Ok(false));
        let chunk_refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        let t = std::time::Instant::now();
        let vectors = provider.embed_batch(&chunk_refs).await?;
        if let Some(t_) = telemetry.as_ref() {
            t_.record("create.embed_batch", t.elapsed().as_secs_f64() * 1000.0);
        }
        let query = mean_direction(&vectors);
        let embed_sha256 = embed_digest(&chunks, provider.as_ref(), metadata_vector, chunk_tokens);
        Ok(Some(NewVectors {
            vectors,
            embed_sha256,
            query,
            fingerprint: unstamped
                .then(|| first_embed_fingerprint(provider.as_ref(), metadata_vector)),
        }))
    }

    /// Record the embedding model on a store whose first vectors a
    /// [`Self::embed_unwritten`] create just wrote (see `embed_memory_with`
    /// for why). Best-effort, like that path.
    pub async fn stamp_first_embed(
        &self,
        fingerprint: Option<crate::storage::manifest::EmbeddingFingerprint>,
    ) {
        if let Some(fingerprint) = fingerprint {
            if let Err(e) = self
                .store
                .stamp_embedding_fingerprint_if_unset(fingerprint)
                .await
            {
                tracing::warn!("failed to stamp embedding fingerprint on first embed: {e}");
            }
        }
    }

    /// [`Self::embed_memory`] for many memories: one batched inference and
    /// one batched chunk write for the whole set.
    ///
    /// Per-memory `embed_memory` costs a small `embed_batch` (one memory's
    /// chunks — far below the width where batching pays), a write lock, a full
    /// directory scan and two LanceDB commits, *each*. Over a whole store that
    /// is quadratic. This flattens every memory's chunk texts into one
    /// inference stream and one `upsert_chunks_batch`.
    ///
    /// Composition is [`embedding_texts`] — byte-identical to the single
    /// version, so vectors written here and by `create` are interchangeable.
    ///
    /// Returns `(embedded_count, per_memory_errors)`. A memory that produced
    /// no text (entirely empty) has its chunks deleted, matching
    /// `embed_memory`. `Ok((0, vec![]))` when no provider is configured.
    pub async fn embed_memories(&self, memories: &[Memory]) -> (usize, Vec<(String, String)>) {
        let Some(provider) = &self.embedding_provider else {
            return (0, Vec::new());
        };
        if memories.is_empty() {
            return (0, Vec::new());
        }
        let chunk_tokens =
            effective_chunk_tokens(self.config.embeddings.max_tokens, provider.max_tokens());
        let metadata_vector = self.config.embeddings.metadata_vector;

        // Flatten every memory's chunk texts into one stream, remembering how
        // many belong to each so the vectors can be handed back out.
        let mut texts: Vec<String> = Vec::new();
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(memories.len());
        for memory in memories {
            let start = texts.len();
            texts.extend(embedding_texts(memory, chunk_tokens, metadata_vector));
            spans.push((start, texts.len()));
        }

        let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vectors = self.embed_texts(&text_refs).await;

        let mut entries: Vec<crate::storage::lance_index::ChunkWrite> =
            Vec::with_capacity(memories.len());
        let mut errors: Vec<(String, String)> = Vec::new();
        for (memory, (start, end)) in memories.iter().zip(spans) {
            // A single failed text fails that memory only — its vectors would
            // otherwise be a partial, silently-wrong representation.
            let mut chunks = Vec::with_capacity(end - start);
            let mut failed = false;
            for slot in &vectors[start..end] {
                match slot {
                    Some(v) => chunks.push(v.clone()),
                    None => {
                        failed = true;
                        break;
                    }
                }
            }
            if failed {
                errors.push((memory.id.clone(), "embedding failed".to_string()));
                continue;
            }
            // The digest covers this memory's own texts, computed from the same
            // slice of the flattened stream that produced its vectors.
            let digest = embed_digest(
                &texts[start..end],
                provider.as_ref(),
                metadata_vector,
                chunk_tokens,
            );
            entries.push((memory.id.clone(), chunks, Some(digest)));
        }

        // Guarded on what the vectors depend on: the memory, re-read under
        // the write lock, must still embed to the same texts (same digest). A
        // metadata-only write racing the embed no longer drops the vectors;
        // a text change or a delete still does, and is reported below.
        let sent: Vec<String> = entries.iter().map(|(id, _, _)| id.clone()).collect();
        let guard_provider = Arc::clone(provider);
        let still_current = move |current: &Memory, digest: Option<&str>| {
            let texts = embedding_texts(current, chunk_tokens, metadata_vector);
            digest.is_some_and(|d| {
                embed_digest(
                    &texts,
                    guard_provider.as_ref(),
                    metadata_vector,
                    chunk_tokens,
                ) == d
            })
        };
        match self
            .store
            .upsert_chunks_batch_if(entries, still_current)
            .await
        {
            Ok(written) => {
                // Declare the loss rather than report success for vectors that
                // were dropped (this also keeps the fingerprint unstamped).
                let written_set: std::collections::HashSet<&str> =
                    written.iter().map(String::as_str).collect();
                for id in sent.iter().filter(|id| !written_set.contains(id.as_str())) {
                    errors.push((
                        id.clone(),
                        "changed while it was being embedded; run reindex again".to_string(),
                    ));
                }
                (written.len(), errors)
            }
            Err(e) => {
                errors.push(("*".to_string(), format!("batched chunk write failed: {e}")));
                (0, errors)
            }
        }
    }

    /// Embed memories with the **write path's** composition and return the
    /// vectors, without writing them.
    ///
    /// For callers that need a memory's vectors but found none in the chunk
    /// table — currently `ops::compress::consolidation_pass`'s fallback. Using
    /// [`embedding_texts`] rather than an ad-hoc string is the point: the
    /// vectors it returns are directly comparable with the stored ones, which
    /// a hand-rolled `"{summary} {content}"` would not be (that form is one
    /// tokenizer-truncated vector, with no metadata row).
    ///
    /// Memories that fail to embed are simply absent from the result.
    pub async fn embed_memory_texts(
        &self,
        memories: &[Memory],
    ) -> std::collections::HashMap<String, Vec<Vec<f32>>> {
        let mut out = std::collections::HashMap::new();
        let Some(provider) = &self.embedding_provider else {
            return out;
        };
        if memories.is_empty() {
            return out;
        }
        let chunk_tokens =
            effective_chunk_tokens(self.config.embeddings.max_tokens, provider.max_tokens());
        let metadata_vector = self.config.embeddings.metadata_vector;

        let mut texts: Vec<String> = Vec::new();
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(memories.len());
        for memory in memories {
            let start = texts.len();
            texts.extend(embedding_texts(memory, chunk_tokens, metadata_vector));
            spans.push((start, texts.len()));
        }
        let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vectors = self.embed_texts(&text_refs).await;

        for (memory, (start, end)) in memories.iter().zip(spans) {
            let chunks: Option<Vec<Vec<f32>>> = vectors[start..end].iter().cloned().collect();
            if let Some(chunks) = chunks {
                if !chunks.is_empty() {
                    out.insert(memory.id.clone(), chunks);
                }
            }
        }
        out
    }

    /// Spawn embedding + contradiction detection for a freshly-created memory
    /// in the background and return immediately.
    ///
    /// The spawned task:
    ///   1. Embeds the memory and upserts vectors into the chunks table.
    ///   2. If NLI is enabled, vector-searches for similar candidates,
    ///      classifies them, and writes a challenge to each existing memory
    ///      whose contradiction probability exceeds the threshold.
    ///
    /// All errors inside the task are logged via `tracing` — they never
    /// propagate back to the caller. Returns `None` when no embedding
    /// provider is configured (nothing to do).
    ///
    /// Used by the MCP `create` tool so the agent isn't blocked on embedding
    /// model inference and NLI classification during memory ingestion.
    ///
    /// `follow_up`, when given, runs last with the [`IngestFindings`] (NLI
    /// contradictions and embedding neighbours): it is how `ops` writes the
    /// proposed edges without this layer depending up on `ops`. It is not run
    /// when the embed itself failed.
    pub fn spawn_ingest(
        &self,
        memory: Memory,
        follow_up: Option<IngestFollowUp>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let provider = Arc::clone(self.embedding_provider.as_ref()?);
        let chunk_tokens =
            effective_chunk_tokens(self.config.embeddings.max_tokens, provider.max_tokens());
        let store = self.store.clone();
        let metadata_vector = self.config.embeddings.metadata_vector;
        let nli_provider = if self.config.nli.enabled {
            self.nli_provider.as_ref().map(Arc::clone)
        } else {
            None
        };
        let nli_config = self.config.nli.clone();
        let links_config = self.config.links.clone();
        let telemetry = self.ingest_telemetry();

        // Counted before the spawn, so a shutdown drain that starts right
        // after this call already waits for it (`ingest_tracker`).
        let guard = super::ingest_tracker::track();
        Some(tokio::spawn(async move {
            let _guard = guard;
            let query = match embed_memory_with(
                provider.as_ref(),
                &store,
                &memory,
                chunk_tokens,
                metadata_vector,
                telemetry.as_ref(),
            )
            .await
            {
                Ok(Some(q)) => q,
                // Nothing to embed: no vector to search with either.
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(
                        memory_id = %memory.id,
                        "Background embed failed: {}",
                        e
                    );
                    return;
                }
            };

            let findings = ingest_findings_with(
                nli_provider.as_deref(),
                &store,
                &nli_config,
                &links_config,
                &memory,
                query,
            )
            .await;
            if !findings.contradictions.is_empty() {
                tracing::debug!(
                    memory_id = %memory.id,
                    count = findings.contradictions.len(),
                    "NLI detected contradictions with existing memories"
                );
                crate::nli::challenge_for_contradictions(
                    &store,
                    &crate::nli::NewMemoryMeta::from(&memory),
                    &findings.contradictions,
                )
                .await;
            }
            if let Some(follow_up) = follow_up {
                follow_up(store, memory, findings).await;
            }
        }))
    }

    /// What an ingest found next to `memory` — its NLI contradictions (when
    /// `[nli]` is on) and its embedding neighbours above
    /// `[links].related_threshold` (when the similarity proposer is on) — from
    /// one vector search with `query`, the vector [`Self::embed_for_ingest`]
    /// just computed: no embed of its own. Never fails: a failure is logged
    /// and leaves the part it would have filled empty.
    pub async fn ingest_findings(&self, memory: &Memory, query: Vec<f32>) -> IngestFindings {
        let nli = match &self.nli_provider {
            Some(p) if self.config.nli.enabled => Some(p.as_ref()),
            _ => None,
        };
        ingest_findings_with(
            nli,
            &self.store,
            &self.config.nli,
            &self.config.links,
            memory,
            query,
        )
        .await
    }

    /// Get a reference to the memory store.
    pub fn store(&self) -> &MemoryStore {
        &self.store
    }

    /// Get a reference to the engine's configuration (the ops layer reads
    /// `[epistemic]` defaults from here, e.g. the off-diagonal Observation
    /// decay in `ops::create`).
    pub fn config(&self) -> &EngramConfig {
        &self.config
    }

    /// Score and expand with `from`'s graph and usage knobs
    /// (`[retrieval.scoring]` `contradicts_penalty` / `supports_boost` /
    /// `usage_weight`, `[retrieval]` `expand_links` / `expand_links_decay`)
    /// instead of this store's own.
    ///
    /// The cross-store fan-in calls it on every group / global engine with
    /// the querying project's config: those knobs are a property of the
    /// query, and a shared store scored by its own (normally default) knobs
    /// would rank systematically differently from the project's memories in
    /// the same merged list. The evidence itself (edges, usage) stays the
    /// shared store's own.
    pub fn adopt_graph_knobs(&mut self, from: &EngramConfig) {
        let s = &from.retrieval.scoring;
        let mine = &mut self.config.retrieval.scoring;
        mine.contradicts_penalty = s.contradicts_penalty;
        mine.supports_boost = s.supports_boost;
        mine.usage_weight = s.usage_weight;
        self.config.retrieval.expand_links = from.retrieval.expand_links;
        self.config.retrieval.expand_links_decay = from.retrieval.expand_links_decay;
    }

    /// Embed arbitrary text with the engine's embedding provider. `None`
    /// when no provider is configured or the embed fails (logged). Used by
    /// the §11.4 consolidation pass; NOT a retrieval path.
    pub async fn embed_text(&self, text: &str) -> Option<Vec<f32>> {
        match self.embed_text_result(text).await {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::debug!("embed_text failed (non-fatal): {e}");
                None
            }
        }
    }

    /// [`Self::embed_text`], keeping the failure instead of logging it away.
    ///
    /// `embed_text` collapses two very different states into one `None` — "no
    /// backend is configured" and "the backend was asked and did not answer" —
    /// and drops the provider's error into a `debug!` line nobody sees. That
    /// is the right trade for the consolidation pass, which only wants a vector
    /// if one is cheap. It is the wrong trade wherever the outcome is reported
    /// to a person: `harvest search` told users "no embedding provider
    /// available" for an Ollama server that was simply not running, because on
    /// a default build the `Auto` backend always constructs an Ollama provider
    /// (construction contacts nothing) and only the *call* fails.
    pub async fn embed_text_result(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let Some(provider) = self.embedding_provider.as_ref() else {
            anyhow::bail!("no embedding backend is configured");
        };
        provider.embed(text).await
    }

    /// [`Self::embed_text`] for many texts at once, one embedding slot per
    /// input in input order (`None` where that text could not be embedded).
    ///
    /// Embedding N texts through N `embed_text` awaits pays the per-invocation
    /// overhead N times — the provider mutex, the `spawn_blocking` hop, the
    /// tokenizer setup and the ONNX session entry for the local backend; a
    /// whole HTTP round trip for Ollama; a whole socket round trip for the
    /// daemon — and gives ONNX Runtime N separate single-row matmuls instead
    /// of one padded batch. Measured on the default quantized all-MiniLM:
    /// 511ms for 64 texts one at a time vs 208ms batched (2.5x); at 8 texts
    /// it is 1.6x (`benches/parallel_simd.rs`, `embed_batching/*`).
    ///
    /// Chunked at [`EMBED_BATCH_CHUNK`] rather than handed over whole: the
    /// daemon backend puts the entire batch in one socket message and Ollama
    /// in one HTTP body, so an unbounded batch is an unbounded message.
    ///
    /// A failed chunk falls back to embedding its texts individually, so one
    /// unembeddable text costs only itself — the same granularity the
    /// `embed_text`-per-item loop had. Failures are logged, never fatal.
    pub async fn embed_texts(&self, texts: &[&str]) -> Vec<Option<Vec<f32>>> {
        let Some(provider) = self.embedding_provider.as_ref() else {
            return vec![None; texts.len()];
        };
        let mut out: Vec<Option<Vec<f32>>> = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(EMBED_BATCH_CHUNK) {
            match provider.embed_batch(chunk).await {
                // A provider that returns the wrong count is broken in a way
                // that would silently misalign vectors with memories, which is
                // far worse than a slow path — fall through to per-text.
                Ok(vectors) if vectors.len() == chunk.len() => {
                    out.extend(vectors.into_iter().map(Some));
                }
                Ok(vectors) => {
                    tracing::debug!(
                        "embed_batch returned {} vectors for {} texts; falling back to per-text",
                        vectors.len(),
                        chunk.len()
                    );
                    for text in chunk {
                        out.push(self.embed_text(text).await);
                    }
                }
                Err(e) => {
                    tracing::debug!("embed_batch failed (non-fatal): {e}; retrying per-text");
                    for text in chunk {
                        out.push(self.embed_text(text).await);
                    }
                }
            }
        }
        out
    }

    /// Contradiction probabilities for text pairs via the NLI provider.
    /// `None` when NLI is unavailable/disabled or classification fails.
    pub async fn nli_contradictions(&self, pairs: &[(&str, &str)]) -> Option<Vec<f32>> {
        if !self.config.nli.enabled {
            return None;
        }
        let nli = self.nli_provider.as_ref()?;
        match nli.classify_batch(pairs).await {
            Ok(results) => Some(results.into_iter().map(|r| r.contradiction).collect()),
            Err(e) => {
                tracing::debug!("nli_contradictions failed (non-fatal): {e}");
                None
            }
        }
    }

    /// Get a mutable reference to the memory store.
    pub fn store_mut(&mut self) -> &mut MemoryStore {
        &mut self.store
    }

    /// Apply cross-encoder reranking to scored candidates.
    ///
    /// Takes the top `config.rerank.top_n` candidates, scores them with the
    /// cross-encoder, blends the rerank score with the original score, and
    /// re-sorts. Candidates beyond `top_n` are left unchanged.
    async fn apply_rerank(
        &self,
        query_text: &str,
        candidates: &mut [ScoredCandidate],
        memory_map: &HashMap<String, Memory>,
    ) -> anyhow::Result<()> {
        let reranker = match &self.reranker {
            Some(r) if self.config.rerank.enabled => Arc::clone(r),
            _ => return Ok(()),
        };

        if candidates.is_empty() {
            return Ok(());
        }

        let top_n = self.config.rerank.top_n.min(candidates.len());
        let weight = self.config.rerank.weight.clamp(0.0, 1.0);

        // Skip the expensive cross-encoder call when weight=0 (original scores only)
        if weight < f64::EPSILON {
            return Ok(());
        }

        // Build document strings for the top N candidates, including details when
        // available. The memory is fetched from `memory_map` by id (the
        // candidate carries only the id, not a cloned memory); every candidate
        // id came from `memory_map` in the scoring loop, so a miss is
        // impossible — treat it defensively as an empty document rather than
        // panicking.
        let documents: Vec<String> = candidates[..top_n]
            .iter()
            .map(|c| match memory_map.get(&c.id) {
                Some(memory) => rerank_document(memory),
                None => String::new(),
            })
            .collect();

        // Score query+document pairs with the cross-encoder. The reranker
        // runs in-process on a blocking thread, or is delegated to the shared
        // embedding daemon — the engine doesn't care which.
        let rerank_results = reranker.rerank(query_text, &documents).await?;

        // Original scores are already in [0, 1] (from composite_score).
        // Cross-encoder logits are unbounded, so normalize with sigmoid.
        for result in &rerank_results {
            let idx = result.index;
            if idx < top_n {
                let raw_rerank = result.score as f64;

                // When weight is 0, preserve original scores exactly.
                let blended = if weight < f64::EPSILON {
                    candidates[idx].score
                } else {
                    let norm_rerank = 1.0 / (1.0 + (-raw_rerank).exp());
                    (1.0 - weight) * candidates[idx].score + weight * norm_rerank
                };

                // The blend happens AFTER composite_score's non-finite guard,
                // so a NaN cross-encoder logit (in-process model or daemon
                // socket) would otherwise write NaN into the final score and
                // scramble the ordering. Keep the original score instead.
                if !blended.is_finite() {
                    continue;
                }

                // NOTE: Only final_score and rerank are updated here.
                // Other ScoreBreakdown fields (semantic, relevance, trust, etc.)
                // retain their pre-rerank values for diagnostic transparency.
                candidates[idx].score = blended;
                candidates[idx].breakdown.final_score = blended;
                candidates[idx].breakdown.rerank = Some(raw_rerank);
            }
        }

        // Re-sort all candidates by blended score descending
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(())
    }

    /// Cross-store rerank **equalization** (P2, multi-project memories).
    ///
    /// When a merged result set is assembled from stores embedded with
    /// *different* models (a project and a subscribed group whose embedding
    /// fingerprints differ), their vector-derived scores are computed in
    /// different spaces and are not directly comparable — the merged ranking can
    /// be arbitrary across the store boundary. A single cross-encoder pass over
    /// the merged union re-scores every entry with one model-agnostic scorer,
    /// making the ranking comparable again. The caller runs this only when a
    /// fingerprint actually differs (the same-fingerprint fast path skips it),
    /// so it is not on the common query path.
    ///
    /// Unlike [`apply_rerank`], which reranks only the top `top_n` of a single
    /// store's candidates, this reranks the **whole** `memories` union: a
    /// foreign-store memory that landed low under its own model's scores could
    /// be the genuinely best answer, so equalization must be able to lift it.
    /// No-op (returns `Ok(false)`) when no reranker is configured/enabled or the
    /// blend weight is zero — the caller then keeps the per-store scores.
    /// Re-sorts by the blended score and truncates to `max`. Best-effort: the
    /// caller treats an `Err` as "keep per-store scores".
    pub async fn rerank_merged(
        &self,
        query_text: &str,
        memories: &mut Vec<ScoredMemory>,
        max: usize,
    ) -> anyhow::Result<bool> {
        let reranker = match &self.reranker {
            Some(r) if self.config.rerank.enabled => Arc::clone(r),
            _ => return Ok(false),
        };
        if memories.is_empty() {
            return Ok(false);
        }
        let weight = self.config.rerank.weight.clamp(0.0, 1.0);
        if weight < f64::EPSILON {
            return Ok(false);
        }

        let documents: Vec<String> = memories
            .iter()
            .map(|sm| rerank_document(&sm.memory))
            .collect();
        let rerank_results = reranker.rerank(query_text, &documents).await?;

        // Blend exactly as `apply_rerank`: original scores are in [0, 1], the
        // cross-encoder logit is unbounded so squash it with a sigmoid, and skip
        // any non-finite blend rather than scrambling the order.
        for result in &rerank_results {
            let idx = result.index;
            if idx < memories.len() {
                let raw_rerank = result.score as f64;
                let norm_rerank = 1.0 / (1.0 + (-raw_rerank).exp());
                let blended = (1.0 - weight) * memories[idx].score + weight * norm_rerank;
                if !blended.is_finite() {
                    continue;
                }
                memories[idx].score = blended;
                memories[idx].score_breakdown.final_score = blended;
                memories[idx].score_breakdown.rerank = Some(raw_rerank);
            }
        }

        memories.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        memories.truncate(max);
        Ok(true)
    }

    /// Query memories with unified ranked / filtered retrieval.
    ///
    /// Two modes:
    /// - [`RetrievalMode::Rank`] — returns every memory passing the
    ///   type/tag/criticality/physical filters, scored and sorted.
    /// - [`RetrievalMode::Filter`] — requires at least one positive
    ///   relevance signal (keyword, semantic, scope proximity, or a
    ///   caller-supplied tag match). Memories with zero signal are dropped.
    ///
    /// Filter mode additionally requires at least one of
    /// `query`, `logical`, `path`, or `tags` to be set, otherwise returns
    /// [`StorageError::Validation`]. `min_criticality` alone is **not**
    /// a relevance signal — it is an importance filter only.
    ///
    /// Logical scope is always a scoring signal, never a filter: scoping a
    /// query with `logical = ["workflow.git.pr"]` makes unrelated memories
    /// rank lower but does not exclude them.
    ///
    /// # Scoring pipeline
    ///
    /// Index-level filter → batch load → optional vector search (top-k,
    /// restricted to the filtered candidate set when the filters narrowed it
    /// to at most [`VECTOR_RESTRICT_MAX_IDS`] memories, whole-store
    /// otherwise) → keyword search → composite scoring → filter-mode
    /// threshold → optional cross-encoder rerank → rank-mode threshold (on the
    /// reranked score; when nothing clears it, the best `rank_fallback`
    /// candidates are returned marked `below_threshold`). Per memory, the
    /// composite weights are chosen from the query evidence:
    ///
    /// | keyword | semantic                  | weights (defaults)                  |
    /// |---------|---------------------------|-------------------------------------|
    /// | yes     | yes (incl. missed top-k)  | `with_keyword`: 0.45kw 0.30sem 0.25rel, never below `with_query` |
    /// | yes     | no chunks / no embeddings | `with_keyword`, sem weight renormalized away |
    /// | no      | yes (incl. missed top-k)  | `with_query`: 0.55sem 0.45rel       |
    /// | no      | no chunks / no embeddings | `degraded`: 1.0rel                  |
    /// | — no query text —                   || `scope_only`: 1.0rel               |
    ///
    /// "Missed top-k" means vector search ran and the memory has embedding
    /// chunks but fell outside the over-fetch window: it is scored with
    /// `sem = 0.0` (weak evidence) so low similarity can never outrank high
    /// similarity. Only memories with no chunks at all (created while
    /// embeddings were unavailable) take the no-semantic-evidence paths.
    pub async fn query(&self, query: &RetrievalQuery) -> Result<RetrievalResult> {
        self.query_expanding_from(query, None).await
    }

    /// [`query`](Self::query), with one-hop link expansion seeded from at
    /// most the first `expansion_parents` direct hits (`None` ⇒ every hit
    /// returned). The cross-store fan-in over-fetches each store far past
    /// the caller's `max_results` and then cuts the merge back to it, so it
    /// passes the caller's `max` here: neighbours of hits the cut will drop
    /// must not spend the expansion budget.
    pub async fn query_expanding_from(
        &self,
        query: &RetrievalQuery,
        expansion_parents: Option<usize>,
    ) -> Result<RetrievalResult> {
        use crate::search::{keyword_search_stems, normalize_keyword_score, query_token_count};

        let query_started = std::time::Instant::now();

        // Step 0: Filter-mode requires at least one positive relevance input.
        if query.mode == RetrievalMode::Filter {
            let has_signal = query.query.as_ref().is_some_and(|s| !s.is_empty())
                || !query.logical.is_empty()
                || query.path.as_ref().is_some_and(|s| !s.is_empty())
                || query.tags.as_ref().is_some_and(|t| !t.is_empty());
            if !has_signal {
                // Record the outcome before bailing so the `no_query_signals`
                // bucket reflects this failure class. Without this, the
                // quality-bucket counts silently miss a whole category of
                // user errors.
                self.record_query_outcome(false, "no_query_signals");
                return Err(crate::storage::StorageError::Validation(
                    "mode=filter requires at least one of: query, logical, path, tags".to_string(),
                ));
            }
        }

        // Step 0.5: Resolve the expiry gate up front so it can feed both the
        // LanceDB pushdown predicate (Step 1) and the Rust pre-filter (Step
        // 2.5b). Captured `now` for the predicate is taken before the scan; the
        // Rust re-filter later recomputes its own `now`, which is >= this one,
        // so the predicate is never stricter than the authoritative Rust gate.
        let include_expired = query
            .include_expired
            .unwrap_or(self.config.retrieval.include_expired);
        // Invalidated memories (closed validity windows) are excluded by
        // default, mirroring expiry: pushdown predicate + authoritative Rust
        // gate (Step 2.5c). No config default — only an explicit per-query
        // opt-in includes them.
        let include_invalidated = query.include_invalidated.unwrap_or(false);

        // Step 1: Load lightweight index entries (6 columns).
        //
        // Push the clean scalar filters — memory type, minimum criticality, and
        // (when not including expired) expiry — down into the LanceDB scan so
        // selective filters cut the row set before it streams into Rust. Tags,
        // physical globs, and logical hierarchy stay in Rust and are applied by
        // `apply_index_filters` / the pre-filters below, which remain the
        // authoritative gate — the predicate is a pure narrowing, so results and
        // scores are identical whether or not it fires.
        let pushdown_predicate = build_filter_predicate(
            query.types.as_deref(),
            query.epistemic.as_deref(),
            query.min_criticality,
            (!include_expired).then(Utc::now),
            (!include_invalidated).then(Utc::now),
        );
        // Remember whether the predicate fired: when it did, the scan itself
        // already narrowed the row set, so `total_entry_count` below counts
        // post-predicate rows — NOT the whole store — and can no longer be
        // used on its own to detect "the filters narrowed the candidates".
        let pushdown_applied = pushdown_predicate.is_some();
        // Only a text query reads the stems; every other shape skips the
        // projection's one large column and its per-row JSON parse.
        let projection = if query.query.is_some() {
            crate::storage::FilterProjection::Full
        } else {
            crate::storage::FilterProjection::NoStems
        };
        let all_entries = self
            .store
            .list_for_filtering_where_projected(pushdown_predicate, projection)
            .await?;
        let total_entry_count = all_entries.len();

        // Step 1.5: Relativize the query path against the project root.
        // Physical scopes are stored repo-relative, so an absolute path (the
        // natural thing for an agent to pass over MCP, or a user on the CLI)
        // would silently prefix/glob-match nothing. Doing it here covers
        // every front-end (CLI, MCP, hooks) identically. Relative paths and
        // absolute paths NOT under the project root pass through unchanged —
        // an outside path legitimately matches no repo-relative scope.
        // `Some("")` is normalized to no-path here: Step 0's signal check
        // already treats an empty path as absent, but the scoring path
        // (`composite_score` counts `path.is_some()` as scope context while
        // `calculate_pattern_score` returns 0.0 for an empty current path)
        // would zero out every memory's scope multiplier — a Rank query with
        // an empty-string path silently returned nothing.
        let query_path: Option<String> = query
            .path
            .as_ref()
            .filter(|p| !p.is_empty())
            .map(|p| crate::storage::paths::relativize_path(p, &self.store.project_dir));

        // Step 2: Apply filters. Logical is NOT a filter — it only scores.
        let filters = SearchFilters {
            types: query.types.clone(),
            tags: query.tags.clone(),
            physical: query_path.clone(),
            min_criticality: query.min_criticality,
        };
        let filtered_entries = apply_index_filters(all_entries, &filters);

        // Epistemic-class hard filter, beside the types filter (§6.1). Applied
        // here (not via `SearchFilters`) because only the `IndexForFiltering`
        // projection carries the class column.
        let filtered_entries = if let Some(classes) = query.epistemic.as_ref() {
            if classes.is_empty() {
                filtered_entries
            } else {
                filtered_entries
                    .into_iter()
                    .filter(|e| classes.contains(&e.epistemic))
                    .collect()
            }
        } else {
            filtered_entries
        };

        // Step 2.5a: In filter mode, user-supplied logical scopes act as a
        // hard filter (matching the legacy `search()` contract). Physical is
        // already filtered via `SearchFilters.physical`; logical is not a
        // field on `SearchFilters` because it's a scoring signal in rank
        // mode, so we apply it here explicitly.
        //
        // Matching is hierarchical, not exact: a memory passes when any of
        // its logical scopes lies on the same ancestor chain as any queried
        // scope (equal, descendant, or ancestor). Querying `auth` matches
        // memories scoped `auth.oauth` (the domain includes its subdomains),
        // and querying `auth.oauth` matches a memory scoped `auth` (broad
        // memories apply to their subdomains) — mirroring rank mode's
        // bidirectional parent↔child proximity. Siblings (`auth.jwt` vs
        // `auth.oauth`) do NOT pass. See `scope::logical::hierarchically_related`.
        let filtered_entries = if query.mode == RetrievalMode::Filter && !query.logical.is_empty() {
            filtered_entries
                .into_iter()
                .filter(|e| {
                    e.logical.iter().any(|mem_scope| {
                        query.logical.iter().any(|query_scope| {
                            crate::scope::logical::hierarchically_related(mem_scope, query_scope)
                        })
                    })
                })
                .collect()
        } else {
            filtered_entries
        };

        // Step 2.5b: Pre-filter expired entries at the index level. This is the
        // authoritative expiry gate (`include_expired` resolved in Step 0.5);
        // the Step 1 pushdown predicate only pre-narrowed the scan.
        let filtered_entries = if include_expired {
            filtered_entries
        } else {
            let now = Utc::now();
            filtered_entries
                .into_iter()
                .filter(|e| e.expires_at.is_none_or(|exp| exp > now))
                .collect()
        };

        // Step 2.5c: Pre-filter invalidated entries — the authoritative gate
        // for the default exclusion (§2.4); the Step 1 predicate only
        // pre-narrowed the scan. A future-dated `invalidated_at` (scheduled
        // invalidation) is still valid now and passes, exactly like a future
        // `expires_at`.
        let filtered_entries: Vec<_> = if include_invalidated {
            filtered_entries
        } else {
            let now = Utc::now();
            filtered_entries
                .into_iter()
                .filter(|e| e.invalidated_at.is_none_or(|t| t > now))
                .collect()
        };

        // R2 fast path (finding performance-0): the no-query Rank path (the
        // SessionStart-hook shape) needs nothing off disk to *score* — every
        // scoring input is in the index projection now that `decay` is a column
        // (schema v0.2.0). Score straight from `filtered_entries`, then load
        // ONLY the survivors' `.md` files. On a large store this reads
        // `max_results` files instead of all N. This is a pure optimization:
        // scoring a `ScoreTarget` built from the projection is byte-identical to
        // scoring the loaded `Memory` (asserted by
        // `tests::rank_from_index_matches_file_load`). Query / Filter / keyword
        // / semantic paths still load candidates below (they need content).
        // Step 2.6: graph and usage evidence (phase 5), in one batched lookup
        // for the whole candidate set — and no lookup at all when every knob
        // is off, which is the default.
        let (graph, signals) = self.load_score_signals(&filtered_entries).await;

        if query.query.is_none() && query.mode == RetrievalMode::Rank {
            return self
                .rank_scope_only_from_index(
                    query,
                    query_path.as_deref(),
                    &filtered_entries,
                    query_started,
                    graph,
                    signals.as_ref(),
                    expansion_parents,
                )
                .await;
        }

        // Step 3: Load only the files scoring cannot do without. Every scoring
        // input is in the index projection (`score_target`), so the rows are
        // scored from the index and only the survivors are loaded (Step 9).
        // The one exception is a row with no stored `keyword_stems` (written
        // before the column existed), whose stems a text query has to derive
        // from the file. Such a row whose file does not load is left out of
        // the keyword pass and of scoring, as every row whose file did not
        // load used to be.
        let mut memory_map: HashMap<String, Memory> = HashMap::new();
        if query.query.is_some() {
            let fallback = filtered_entries
                .iter()
                .filter(|e| e.keyword_stems.is_none())
                .map(|e| e.id.as_str());
            self.load_missing(&mut memory_map, fallback).await?;
        }
        let needs_file = |e: &crate::storage::IndexForFiltering| {
            query.query.is_some() && e.keyword_stems.is_none() && !memory_map.contains_key(&e.id)
        };

        // Step 4: If query text + embeddings available, get semantic scores.
        let semantic_scores_map: Option<HashMap<String, f64>> = match query.query {
            Some(ref q) => {
                // Narrowing happens in two places and both must count: the
                // Rust-side filters (visible as `filtered < total`) and the
                // LanceDB predicate pushdown (which narrows the scan before
                // `total_entry_count` is measured, so `filtered == total`
                // even though the store holds more rows).
                let narrowed = pushdown_applied || filtered_entries.len() < total_entry_count;
                self.semantic_scores(q, query, &filtered_entries, narrowed)
                    .await
            }
            None => None,
        };

        // Step 4.5: When vector search ran, fetch the set of memory ids that
        // have embedding chunks at all. A memory WITH chunks that merely
        // missed the top-k over-fetch window must be scored as weak semantic
        // evidence (sem = 0.0, "checked, found nothing"), not dropped into
        // the no-semantic weight regime: that regime renormalizes relevance
        // to full weight, so being LESS similar than the top-k cutoff would
        // otherwise RAISE a memory's score above genuinely similar in-top-k
        // memories (whose 1/(1+L2) scores top out well below 1.0). Memories
        // with no chunks (created while embeddings were unavailable) keep
        // sem = None — the legacy no-evidence path (see
        // scoring::composite::tests::test_semantic_none_vs_zero_differ).
        //
        // The `has_embedding` column (schema v0.2.0, finding performance-3
        // Part B) carries this per memory in the index projection, so the set
        // is read straight from `filtered_entries` with **no** chunks-table
        // scan. It covers exactly the memories being scored (the entries in
        // filtered_entries), which is all `embedded_ids` is consulted for. The migration keeps the column authoritative (a reindex rebuilds
        // it), so there is no half-migrated mis-scoring window.
        let embedded_ids: Option<std::collections::HashSet<String>> =
            if semantic_scores_map.is_some() {
                Some(
                    filtered_entries
                        .iter()
                        .filter(|e| e.has_embedding)
                        .map(|e| e.id.clone())
                        .collect(),
                )
            } else {
                None
            };

        // Step 5: Run keyword search whenever query text is present. Keyword
        // scores power both the "keyword_only" mode and the filter-mode
        // sufficiency check, so we compute them even when embeddings are live.
        let keyword_map: HashMap<String, f64> = if let Some(ref q) = query.query {
            // Prefer the stems the write path already stored (schema v0.5.0).
            // Rebuilding them here is the dominant cost of keyword scoring —
            // ~17.7ms per 1000 memories — and it is identical on every query.
            //
            // Iterate the entries and fall back to the loaded memory only for
            // rows written before the column existed (Step 3 loaded exactly
            // those), so a partially-migrated store degrades in speed and never
            // in correctness.
            // Borrowed from the row on the common path; owned only for the
            // fallback it has to compute.
            fn stems_for<'a>(
                e: &'a crate::storage::IndexForFiltering,
                memory_map: &HashMap<String, Memory>,
            ) -> Option<(&'a str, std::borrow::Cow<'a, KeywordStems>)> {
                let stems = match &e.keyword_stems {
                    Some(s) => std::borrow::Cow::Borrowed(s),
                    None => {
                        let m = memory_map.get(&e.id)?;
                        std::borrow::Cow::Owned(KeywordStems::compute(
                            &m.summary, &m.tags, &m.content,
                        ))
                    }
                };
                Some((e.id.as_str(), stems))
            }
            // The `None` arm is the pre-v0.5.0 fallback: deriving stems is
            // ~16µs per memory (tokenize → stoplist → Snowball stem → dedup
            // across three fields), so on a store that has not been reindexed
            // since the column landed this dominates the query. Parallel above
            // the same threshold as scoring; order-preserving either way,
            // which matters here because `kw_results` indexes back into this
            // vector.
            let scored: Vec<(&str, std::borrow::Cow<'_, KeywordStems>)> =
                if filtered_entries.len() >= PARALLEL_SCORE_MIN {
                    use rayon::prelude::*;
                    filtered_entries
                        .par_iter()
                        .filter_map(|e| stems_for(e, &memory_map))
                        .collect()
                } else {
                    filtered_entries
                        .iter()
                        .filter_map(|e| stems_for(e, &memory_map))
                        .collect()
                };
            // Split, order kept, so `kw_results` indexes both halves alike.
            let (scored_ids, stems): (Vec<&str>, Vec<std::borrow::Cow<'_, KeywordStems>>) =
                scored.into_iter().unzip();
            let kw_results = keyword_search_stems(q, &stems);
            let num_tokens = query_token_count(q);
            kw_results
                .into_iter()
                .map(|(idx, raw)| {
                    let norm = normalize_keyword_score(raw, num_tokens);
                    (scored_ids[idx].to_string(), norm)
                })
                .collect()
        } else {
            HashMap::new()
        };

        // Derive retrieval_quality label.
        let retrieval_quality = if query.query.is_some() {
            if semantic_scores_map.is_some() {
                "full"
            } else if !keyword_map.is_empty() {
                "keyword_only"
            } else {
                "no_query_signals"
            }
        } else {
            "scope_only"
        };

        // Score by reference into lightweight candidates (id + score +
        // breakdown) — NOT full `ScoredMemory` — so we don't clone every
        // memory. Only the survivors of threshold + sort + rerank + truncate
        // are materialized (cloned) into `ScoredMemory` at the end. On a large
        // no-filter Rank query this turns N memory clones into `max_results`.
        let query_path = query_path.as_deref();
        let now = Utc::now();

        let t_score = std::time::Instant::now();
        let score_one = |entry: &crate::storage::IndexForFiltering| {
            if needs_file(entry) {
                return None;
            }

            // Gather query evidence for this memory. Semantic comes from the
            // vector top-k when present; an embedded memory that missed the
            // top-k window counts as weak evidence (sem = 0.0); a memory with
            // no chunks at all keeps sem = None (no evidence). See Step 4.5.
            let sem_score = semantic_scores_map
                .as_ref()
                .and_then(|m| m.get(&entry.id))
                .copied()
                .or_else(|| {
                    embedded_ids
                        .as_ref()
                        .is_some_and(|ids| ids.contains(&entry.id))
                        .then_some(0.0)
                });
            let kw_score = keyword_map.get(&entry.id).copied();

            // Build scoring context. In rank mode the user-supplied path and
            // logical scopes drive the scope proximity signal. In filter mode
            // they have already been applied as hard filters (physical via
            // `SearchFilters`, logical via the pre-filter above), so passing
            // them to the scorer would double-count: scope acts as a
            // post-multiplier and would discount results that already passed
            // the hard filter.
            let (ctx_path, ctx_logical): (Option<&str>, &[String]) = match query.mode {
                RetrievalMode::Rank => (query_path, &query.logical),
                RetrievalMode::Filter => (None, &[]),
            };

            // Pick the scoring constructor from the available evidence:
            //   keyword + semantic → with_keyword(kw, Some(sem)): the
            //     documented combined formula (0.45*kw + 0.30*sem +
            //     0.25*relevance);
            //   keyword only       → with_keyword(kw, None): semantic weight
            //     drops out and the rest renormalizes;
            //   semantic only      → with_semantic (0.55*sem + 0.45*relevance);
            //   neither            → degraded (relevance only);
            //   no query text      → scope_only.
            let context = if let Some(ref q) = query.query {
                match (kw_score, sem_score) {
                    (Some(kw), sem) => {
                        ScoringContext::with_keyword(ctx_path, ctx_logical, q, kw, sem)
                    }
                    (None, Some(s)) => ScoringContext::with_semantic(ctx_path, ctx_logical, q, s),
                    (None, None) => ScoringContext::with_query_degraded(ctx_path, ctx_logical, q),
                }
            } else {
                ScoringContext::scope_only(ctx_path, ctx_logical)
            }
            .with_situation(query.situation)
            .with_signals(signals.as_ref().and_then(|m| m.get(&entry.id)));

            // Expired memories (only present when `include_expired` is set —
            // otherwise Step 2.5b already dropped them) are scored ignoring
            // decay: their decay factor is at/near floor by definition, so
            // normal scoring would zero them out and including them would be
            // pointless. Active memories keep normal decay scoring regardless
            // of `include_expired` — a half-decayed active memory must not
            // rank as fresh just because the caller asked to also see expired
            // ones.
            // Scored from the index row (`score_target`), which carries every
            // input `composite_score` reads from a memory; the expiry test is
            // `Memory::is_expired_at`'s.
            let target = score_target(entry);
            let breakdown = if entry.expires_at.is_some_and(|exp| now > exp) {
                composite_score_target_ignore_decay(target, &context, &self.config, now)
            } else {
                composite_score_target(target, &context, &self.config, now)
            };

            // Filter-mode sufficiency check.
            //
            // Semantic similarity alone is never sufficient: all-MiniLM
            // assigns nontrivial cosine similarity (~0.4) even to
            // unrelated short strings, so semantic-only would surface
            // every memory for any query. Accepted signals:
            //
            //   - keyword match on query text
            //   - tag match on user-supplied `tags` filter
            //   - user supplied `path` or `logical` (already applied as
            //     hard filters above, so any surviving memory matched)
            if query.mode == RetrievalMode::Filter {
                let has_kw = kw_score.is_some_and(|v| v > 0.0);
                let has_tag = query.tags.as_ref().is_some_and(|filter_tags| {
                    !filter_tags.is_empty() && filter_tags.iter().any(|t| entry.tags.contains(t))
                });
                // `query_path` (not `query.path`): an empty-string path was
                // normalized away in Step 1.5 and must not count as a
                // sufficiency signal — it filtered nothing.
                let user_scope_supplied = query_path.is_some() || !query.logical.is_empty();
                if !(has_kw || has_tag || user_scope_supplied) {
                    return None;
                }
            }

            Some(ScoredCandidate {
                id: entry.id.clone(),
                score: breakdown.final_score,
                breakdown,
            })
        };

        // Same independence argument as `rank_scope_only_from_index`: every
        // iteration reads `keyword_map` / `semantic_scores_map` and writes
        // nothing shared. `filter_map` over an indexed parallel
        // iterator keeps input order, so the candidate list is identical to
        // the sequential one, not merely equivalent.
        let mut candidates: Vec<ScoredCandidate> = if filtered_entries.len() >= PARALLEL_SCORE_MIN {
            use rayon::prelude::*;
            filtered_entries.par_iter().filter_map(score_one).collect()
        } else {
            filtered_entries.iter().filter_map(score_one).collect()
        };

        self.record_stage("score", t_score.elapsed().as_secs_f64() * 1000.0);

        // Step 6: Threshold. Rank mode uses retrieval.relevance_threshold;
        // Filter mode uses the stricter search.threshold when set, since the
        // flow is "find specific memories" rather than "browse context".
        // Rank mode applies its threshold after the rerank (Step 8.5): the
        // cut belongs on the blended score, and the pre-rerank composite is
        // not on the scale `relevance_threshold` was set for.
        if query.mode == RetrievalMode::Filter {
            let threshold = filter_threshold(self.config.search.threshold);
            if threshold > 0.0 {
                candidates.retain(|c| c.score >= threshold);
            }
        }

        // Step 7: Sort by score descending.
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Step 8: Apply reranking if query text is present. Rerank runs on the
        // post-situation survivors (§7.1): the situation multiplier applies
        // pre-threshold in Step 5, so the cross-encoder sees (and blends over)
        // class-appropriate candidates and rerank-blend semantics stay
        // untouched. Rerank runs on the full sorted candidate set BEFORE
        // truncation (identical to before);
        // it reads memory text from `memory_map` by id, so the documents it
        // will read — the first `rerank.top_n` candidates — are loaded here.
        // Only timed when the reranker is actually configured + enabled to
        // avoid recording bogus 0ms samples for the no-op path.
        if let Some(ref q) = query.query {
            if self.reranking_available() && !candidates.is_empty() {
                let top_n = self.config.rerank.top_n.min(candidates.len());
                let want = candidates[..top_n].iter().map(|c| c.id.as_str());
                self.load_missing(&mut memory_map, want).await?;
                let t_rerank = std::time::Instant::now();
                let rerank_result = self.apply_rerank(q, &mut candidates, &memory_map).await;
                self.record_stage("rerank", t_rerank.elapsed().as_secs_f64() * 1000.0);
                if let Err(e) = rerank_result {
                    tracing::warn!("Reranking failed, using original scores: {}", e);
                }
            } else if let Err(e) = self.apply_rerank(q, &mut candidates, &memory_map).await {
                tracing::warn!("Reranking failed, using original scores: {}", e);
            }
        }

        // Step 8.5: Rank threshold, on the reranked (sorted) candidates.
        let below_threshold = if query.mode == RetrievalMode::Rank {
            self.apply_rank_threshold(&mut candidates, query)
        } else {
            Vec::new()
        };

        let mut total = candidates.len();

        // Step 9: Apply max_results limit, THEN load the survivors' files —
        // the only files most text queries read (finding performance-4).
        let max_results = query
            .max_results
            .unwrap_or(self.config.retrieval.max_results);
        let mut tail = candidates.split_off(max_results.min(candidates.len()));
        // A skipped id keeps its place and its count, is never loaded, and is
        // removed at the end (`RetrievalQuery::skip_load`).
        let skip = query.skip_load.as_ref();
        let skipped = |id: &str| skip.is_some_and(|s| s.contains(id));
        self.load_missing(
            &mut memory_map,
            candidates
                .iter()
                .map(|c| c.id.as_str())
                .filter(|id| !skipped(id)),
        )
        .await?;
        // A row scored from the index whose file is gone or no longer parses
        // (a drifted store) cannot be returned. Drop it, uncount it, and
        // refill from the cut tail once, with exactly as many as went
        // missing; a refill that fails to load is dropped too, without a
        // second round. Before scores came from the index such a row was
        // never scored at all, so a healthy store answers exactly as before.
        let before = candidates.len();
        candidates.retain(|c| skipped(&c.id) || memory_map.contains_key(&c.id));
        let missing = before - candidates.len();
        if missing > 0 {
            total -= missing;
            let refill: Vec<ScoredCandidate> = tail.drain(..missing.min(tail.len())).collect();
            self.load_missing(
                &mut memory_map,
                refill
                    .iter()
                    .map(|c| c.id.as_str())
                    .filter(|id| !skipped(id)),
            )
            .await?;
            for c in refill {
                if skipped(&c.id) || memory_map.contains_key(&c.id) {
                    candidates.push(c);
                } else {
                    total -= 1;
                }
            }
        }

        // Step 9.5: one-hop link expansion (opt-in). Neighbours come from the
        // filtered candidate set; their files are loaded after it.
        let (mut expanded, new_in_total) = self
            .expand_candidates(
                query,
                &candidates,
                tail,
                &filtered_entries,
                graph,
                signals.as_ref(),
                expansion_parents,
            )
            .await;
        // Expanded neighbours are results the caller sees, so `total` counts
        // them ("Found 3 memories (out of 2 total)" would read as a bug) —
        // only those it did not already count as a cut candidate.
        let mut total = total + new_in_total;
        if !expanded.is_empty() {
            self.load_missing(
                &mut memory_map,
                expanded
                    .iter()
                    .map(|c| c.id.as_str())
                    .filter(|id| !skipped(id)),
            )
            .await?;
            // Counted either way (in the tail or by `new_in_total`), so a
            // neighbour whose file does not load is uncounted.
            let before = expanded.len();
            expanded.retain(|c| skipped(&c.id) || memory_map.contains_key(&c.id));
            total -= before - expanded.len();
        }
        let dropped_below_threshold = dropped_minus_returned(&below_threshold, &expanded);
        if !expanded.is_empty() {
            candidates.extend(expanded);
            sort_candidates(&mut candidates);
        }

        let mut scored_memories: Vec<ScoredMemory> = candidates
            .into_iter()
            .filter(|c| !skipped(&c.id))
            .filter_map(|c| {
                // Every candidate was loaded above; `filter_map` guards
                // defensively without panicking.
                memory_map.remove(&c.id).map(|memory| ScoredMemory {
                    memory,
                    score: c.score,
                    score_breakdown: c.breakdown,
                })
            })
            .collect();

        // Step 10: Strip detail per detail_level.
        for sm in &mut scored_memories {
            match query.detail_level {
                DetailLevel::Summary => {
                    sm.memory.content = String::new();
                    sm.memory.details = None;
                }
                DetailLevel::Content => {
                    sm.memory.details = None;
                }
                DetailLevel::Full => {}
            }
        }

        let result = RetrievalResult {
            memories: scored_memories,
            total,
            retrieval_quality: retrieval_quality.to_string(),
            dropped_below_threshold,
        };

        self.record_stage(
            "query.total",
            query_started.elapsed().as_secs_f64() * 1000.0,
        );
        self.record_query_outcome(!result.memories.is_empty(), &result.retrieval_quality);

        Ok(result)
    }

    /// Step 4 of [`Self::query_expanding_from`]: embed the query text and
    /// run the vector search. `None` when no embedding provider is set or
    /// the embed or the search failed.
    async fn semantic_scores(
        &self,
        q: &str,
        query: &RetrievalQuery,
        filtered_entries: &[crate::storage::IndexForFiltering],
        narrowed: bool,
    ) -> Option<HashMap<String, f64>> {
        let provider = self.embedding_provider.as_ref()?;
        let t_embed = std::time::Instant::now();
        let embed_result = provider.embed(q).await;
        self.record_stage("embed", t_embed.elapsed().as_secs_f64() * 1000.0);

        let query_vector = embed_result.ok()?;
        // Over-fetch 3x so the top-k still has headroom after
        // chunk-level dedup.
        //
        // `max_results` is user-supplied: saturate the multiply
        // (a plain `* 3` panics in debug / wraps in release on
        // huge values) and cap the window — memories beyond it
        // are still returned, just with weak (sem = 0.0)
        // semantic evidence, so a finite cap only bounds work.
        let limit = query
            .max_results
            .unwrap_or(self.config.retrieval.max_results)
            .saturating_mul(3)
            .min(VECTOR_SEARCH_WINDOW_CAP);

        // Filter pushdown: when the index filters actually
        // narrowed the candidate set and it is small enough,
        // restrict the vector search to the surviving ids so
        // filtered-out memories can't saturate the top-k window
        // (which would degrade surviving candidates to weak
        // sem = 0.0 evidence and mis-rank them). With no filters
        // — or a still-large candidate set — fall back to the
        // whole-store search, where the windowing artifact is
        // mild because little or nothing in the window is
        // discarded.
        // `narrowed` says whether the caller's filters cut the candidate
        // set (see the call site).
        let restrict_ids: Option<Vec<String>> = (narrowed
            && filtered_entries.len() <= VECTOR_RESTRICT_MAX_IDS)
            .then(|| filtered_entries.iter().map(|e| e.id.clone()).collect());

        let t_vs = std::time::Instant::now();
        let vs_result = self
            .store
            .vector_search(query_vector, limit, restrict_ids.as_deref())
            .await;
        self.record_stage("vector_search", t_vs.elapsed().as_secs_f64() * 1000.0);
        vs_result
            .ok()
            .map(|matches| matches.into_iter().map(|m| (m.id, m.score)).collect())
    }

    /// Load into `map` the memories among `ids` it does not hold yet, in one
    /// batch (no call when it holds them all).
    async fn load_missing<'a>(
        &self,
        map: &mut HashMap<String, Memory>,
        ids: impl Iterator<Item = &'a str>,
    ) -> Result<()> {
        let want: Vec<&str> = ids.filter(|id| !map.contains_key(*id)).collect();
        if !want.is_empty() {
            #[cfg(test)]
            FILES_REQUESTED.fetch_add(want.len(), std::sync::atomic::Ordering::SeqCst);
            map.extend(self.store.get_batch(&want).await?);
        }
        Ok(())
    }

    /// Rank mode's relevance cut, on candidates already sorted by score.
    ///
    /// Keeps every candidate at or above `retrieval.relevance_threshold`.
    /// When none clears it, keeps the best `rank_fallback` (the query's
    /// override, else the config) and marks each `below_threshold`, instead of
    /// returning nothing: rank mode is "browse by relevance", and an empty
    /// answer sent agents to read the store files directly. Returns how many
    /// candidates were removed, so the caller can report the loss.
    ///
    /// Returns the candidates it removed (moved, not cloned), so a caller can
    /// tell which of them link expansion brought back.
    fn apply_rank_threshold(
        &self,
        candidates: &mut Vec<ScoredCandidate>,
        query: &RetrievalQuery,
    ) -> Vec<ScoredCandidate> {
        let threshold = self.config.retrieval.relevance_threshold;
        if threshold <= 0.0 {
            return Vec::new();
        }
        let fallback = query
            .rank_fallback
            .unwrap_or(self.config.retrieval.rank_fallback);
        let clears = |c: &ScoredCandidate| c.breakdown.gate_score.unwrap_or(c.score) >= threshold;
        if fallback == 0 || candidates.iter().any(clears) {
            let (kept, dropped): (Vec<_>, Vec<_>) =
                std::mem::take(candidates).into_iter().partition(clears);
            *candidates = kept;
            dropped
        } else {
            let dropped = candidates.split_off(fallback.min(candidates.len()));
            for c in candidates.iter_mut() {
                c.breakdown.below_threshold = true;
            }
            dropped
        }
    }

    /// R2 no-query Rank fast path: score every candidate straight from the
    /// index projection ([`ScoreTarget`]) and load only the survivors' files.
    /// Byte-identical to the main `query` path for
    /// `query.query == None && mode == Rank` — every scoring input is carried
    /// in the projection (schema v0.2.0), so this only skips the full file load.
    /// `ctx_path` is the caller's Step-1.5 normalized query path — already
    /// project-relativized and with `Some("")` collapsed to `None`. Deriving it
    /// again here from `query.path` once dropped the empty-string collapse, so
    /// the no-query Rank shape (the SessionStart hook) with `path: ""` zeroed
    /// every scope multiplier and returned nothing.
    #[allow(clippy::too_many_arguments)]
    async fn rank_scope_only_from_index(
        &self,
        query: &RetrievalQuery,
        ctx_path: Option<&str>,
        filtered_entries: &[crate::storage::IndexForFiltering],
        query_started: std::time::Instant,
        graph: Option<GraphSnapshot>,
        signals: Option<&HashMap<String, ScoreSignals>>,
        expansion_parents: Option<usize>,
    ) -> Result<RetrievalResult> {
        let now = Utc::now();

        let t_score = std::time::Instant::now();
        let score_entry = |e: &crate::storage::IndexForFiltering| {
            let target = score_target(e);
            let context = ScoringContext::scope_only(ctx_path, &query.logical)
                .with_situation(query.situation)
                .with_signals(signals.and_then(|m| m.get(&e.id)));
            // Mirror the main loop: expired entries (present only under
            // include_expired) score ignoring decay.
            let breakdown = if e.expires_at.is_some_and(|exp| now > exp) {
                composite_score_target_ignore_decay(target, &context, &self.config, now)
            } else {
                composite_score_target(target, &context, &self.config, now)
            };
            ScoredCandidate {
                id: e.id.clone(),
                score: breakdown.final_score,
                breakdown,
            }
        };
        // This path scores *every* index row — it is the no-query Rank shape
        // the SessionStart/PreToolUse hooks take, where nothing narrows the
        // candidate set first, so scoring is the whole cost rather than a tail
        // behind file loading. Measured at ~3.0ms per 5,000 rows serially and
        // ~0.87ms across four cores (`benches/parallel_simd.rs`, `score/*`).
        //
        // Scoring is a pure function of the row plus shared read-only config,
        // so the rows are independent; `collect` keeps input order, which the
        // sort below then makes irrelevant anyway.
        let mut candidates: Vec<ScoredCandidate> = if filtered_entries.len() >= PARALLEL_SCORE_MIN {
            use rayon::prelude::*;
            filtered_entries.par_iter().map(score_entry).collect()
        } else {
            filtered_entries.iter().map(score_entry).collect()
        };
        self.record_stage("score", t_score.elapsed().as_secs_f64() * 1000.0);

        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let below_threshold = self.apply_rank_threshold(&mut candidates, query);

        let total = candidates.len();
        let max_results = query
            .max_results
            .unwrap_or(self.config.retrieval.max_results);
        let tail = candidates.split_off(max_results.min(candidates.len()));
        let (expanded, new_in_total) = self
            .expand_candidates(
                query,
                &candidates,
                tail,
                filtered_entries,
                graph,
                signals,
                expansion_parents,
            )
            .await;
        let dropped_below_threshold = dropped_minus_returned(&below_threshold, &expanded);
        let total = total + new_in_total;
        if !expanded.is_empty() {
            candidates.extend(expanded);
            sort_candidates(&mut candidates);
        }

        // Materialize ONLY the survivors (and any expanded neighbours) — the
        // whole point of this path — less the ids the caller already holds.
        if let Some(skip) = &query.skip_load {
            candidates.retain(|c| !skip.contains(&c.id));
        }
        let survivor_ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
        let loaded: HashMap<String, Memory> = self
            .store
            .get_batch(&survivor_ids)
            .await?
            .into_iter()
            .collect();
        let mut scored_memories: Vec<ScoredMemory> = candidates
            .into_iter()
            .filter_map(|c| {
                loaded.get(&c.id).map(|memory| ScoredMemory {
                    memory: memory.clone(),
                    score: c.score,
                    score_breakdown: c.breakdown,
                })
            })
            .collect();

        for sm in &mut scored_memories {
            match query.detail_level {
                DetailLevel::Summary => {
                    sm.memory.content = String::new();
                    sm.memory.details = None;
                }
                DetailLevel::Content => {
                    sm.memory.details = None;
                }
                DetailLevel::Full => {}
            }
        }

        let result = RetrievalResult {
            memories: scored_memories,
            total,
            retrieval_quality: "scope_only".to_string(),
            dropped_below_threshold,
        };
        self.record_stage(
            "query.total",
            query_started.elapsed().as_secs_f64() * 1000.0,
        );
        self.record_query_outcome(!result.memories.is_empty(), &result.retrieval_quality);
        Ok(result)
    }

    /// The phase-5 evidence for `entries`: the edge snapshot (kept for link
    /// expansion to reuse) and per-memory [`ScoreSignals`]. `(None, None)`
    /// without any I/O when `contradicts_penalty`, `supports_boost` and
    /// `usage_weight` are all 0 — the default. See `retrieval::signals`.
    async fn load_score_signals(
        &self,
        entries: &[crate::storage::IndexForFiltering],
    ) -> (Option<GraphSnapshot>, Option<HashMap<String, ScoreSignals>>) {
        let scoring = &self.config.retrieval.scoring;
        let edges_on = scoring.edge_signals_enabled();
        let usage_on = scoring.usage_signal_enabled();
        if (!edges_on && !usage_on) || entries.is_empty() {
            return (None, None);
        }
        let ids: std::collections::HashSet<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        let mut map: HashMap<String, ScoreSignals> = HashMap::new();
        let mut graph = None;
        if edges_on {
            let refs: Vec<&crate::storage::IndexForFiltering> = entries.iter().collect();
            let g = GraphSnapshot::load(&self.store, &refs, true, Utc::now()).await;
            map = g.score_signals(&ids);
            graph = Some(g);
        }
        if usage_on {
            let usage = self.usage_cache.get(&self.store).await;
            for id in &ids {
                if let Some(u) = usage.get(*id) {
                    map.entry(id.to_string()).or_default().usage = Some(*u);
                }
            }
        }
        (graph, Some(map))
    }

    /// One-hop link expansion from `hits` (the truncated, sorted result).
    ///
    /// Returns at most `expand_links` neighbours, best first, each marked
    /// with [`LinkVia`], plus how many of them were *not* already counted in
    /// the query's `total` (a neighbour cut only by `max_results` was). A
    /// neighbour must be in `entries` — the candidate set after every hard
    /// filter — so expansion can never return a memory the query itself was
    /// not allowed to return. Weak fallback hits (`below_threshold`) expand
    /// nothing: a lead is not a hit.
    ///
    /// A neighbour is scored as its best parent's score × `expand_links_decay`
    /// × its own situation multiplier, **minus its own challenge and
    /// `contradicts` penalties** — a disputed memory does not ride in at full
    /// strength on a healthy one's relevance — or at its own score when the
    /// query scored it and that is higher. Its breakdown is its own (the
    /// query's, or a scope-only one built with its signals and the query's
    /// situation), so `contradicted_by` / `contradicts_penalty` still say it
    /// is disputed. Only the first `parents` hits (`None` ⇒ all) expand.
    #[allow(clippy::too_many_arguments)]
    async fn expand_candidates(
        &self,
        query: &RetrievalQuery,
        hits: &[ScoredCandidate],
        tail: Vec<ScoredCandidate>,
        entries: &[crate::storage::IndexForFiltering],
        graph: Option<GraphSnapshot>,
        signals: Option<&HashMap<String, ScoreSignals>>,
        parents: Option<usize>,
    ) -> (Vec<ScoredCandidate>, usize) {
        let n = query
            .expand_links
            .unwrap_or(self.config.retrieval.expand_links)
            .min(crate::types::MAX_EXPAND_LINKS);
        let parents: Vec<&ScoredCandidate> = hits
            .iter()
            .take(parents.unwrap_or(usize::MAX))
            .filter(|c| !c.breakdown.below_threshold)
            .collect();
        if n == 0 || parents.is_empty() {
            return (Vec::new(), 0);
        }
        let visible: HashMap<&str, &crate::storage::IndexForFiltering> =
            entries.iter().map(|e| (e.id.as_str(), e)).collect();
        let graph = match graph {
            Some(g) => g,
            None => {
                let refs: Vec<&crate::storage::IndexForFiltering> = parents
                    .iter()
                    .filter_map(|c| visible.get(c.id.as_str()).copied())
                    .collect();
                GraphSnapshot::load(&self.store, &refs, false, Utc::now()).await
            }
        };
        let decay = {
            let d = self.config.retrieval.expand_links_decay;
            if d.is_finite() {
                d.clamp(0.0, 1.0)
            } else {
                0.0
            }
        };
        let present: std::collections::HashSet<&str> = hits.iter().map(|c| c.id.as_str()).collect();
        let tail: HashMap<String, ScoreBreakdown> =
            tail.into_iter().map(|c| (c.id, c.breakdown)).collect();
        let now = Utc::now();

        // The neighbour's own breakdown, computed once per neighbour.
        let mut own_cache: HashMap<&str, ScoreBreakdown> = HashMap::new();
        let mut best: HashMap<&str, ScoredCandidate> = HashMap::new();
        for parent in parents {
            for (nid, kind) in graph.neighbours(&parent.id) {
                if present.contains(nid) {
                    continue;
                }
                let Some(entry) = visible.get(nid) else {
                    continue;
                };
                let scored_by_query = tail.contains_key(nid);
                let own = own_cache
                    .entry(nid)
                    .or_insert_with(|| match tail.get(nid) {
                        Some(b) => b.clone(),
                        None => {
                            // Not scored by this query (it failed filter
                            // mode's relevance test, or the rank threshold
                            // dropped it): its own query-independent
                            // components, with its own signals and the
                            // query's situation, so the breakdown still
                            // explains — and still flags — the memory.
                            let target = score_target(entry);
                            let ctx = ScoringContext::scope_only(None, &[])
                                .with_situation(query.situation)
                                .with_signals(signals.and_then(|m| m.get(nid)));
                            composite_score_target(target, &ctx, &self.config, now)
                        }
                    })
                    .clone();
                let challenge = if entry.status == crate::types::Status::Challenged {
                    self.config
                        .retrieval
                        .scoring
                        .challenge_penalty
                        .penalty_for(entry.epistemic)
                } else {
                    0.0
                };
                let decayed = parent.score * decay * own.situation_multiplier
                    - challenge
                    - own.contradicts_penalty.unwrap_or(0.0);
                let decayed = if decayed.is_finite() {
                    decayed.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let score = if scored_by_query {
                    decayed.max(own.final_score)
                } else {
                    decayed
                };
                if best.get(nid).is_some_and(|b| b.score >= score) {
                    continue;
                }
                let mut breakdown = own;
                breakdown.final_score = score;
                breakdown.gate_score = None;
                breakdown.below_threshold = false;
                breakdown.via = Some(LinkVia {
                    from: parent.id.clone(),
                    kind: kind.clone(),
                });
                best.insert(
                    nid,
                    ScoredCandidate {
                        id: nid.to_string(),
                        score,
                        breakdown,
                    },
                );
            }
        }
        let mut out: Vec<ScoredCandidate> = best.into_values().collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        out.truncate(n);
        let new_in_total = out.iter().filter(|c| !tail.contains_key(&c.id)).count();
        (out, new_in_total)
    }
}

/// `dropped_below_threshold` for a query: the candidates the rank threshold
/// removed, less any link expansion brought back — a neighbour the caller is
/// shown was not dropped.
/// How many memory files the text-query path asked the store for, so a test
/// can prove it reads only the survivors. Process-wide; nextest runs each test
/// in its own process.
#[cfg(test)]
pub(crate) static FILES_REQUESTED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The scoring inputs of an index row: what `ScoreTarget::from(&Memory)`
/// takes from the loaded memory, carried in the projection (schema v0.2.0),
/// so a row scores identically without its file
/// (`tests::rank_from_index_matches_file_load`).
fn score_target(e: &crate::storage::IndexForFiltering) -> ScoreTarget<'_> {
    ScoreTarget {
        created_at: e.created_at,
        decay: &e.decay,
        criticality: e.criticality,
        physical: &e.physical,
        logical: &e.logical,
        provenance_source: e.provenance_source,
        status: e.status,
        epistemic: e.epistemic,
        verified_at: e.verified_at,
    }
}

fn dropped_minus_returned(dropped: &[ScoredCandidate], expanded: &[ScoredCandidate]) -> usize {
    if expanded.is_empty() || dropped.is_empty() {
        return dropped.len();
    }
    let back: std::collections::HashSet<&str> = expanded.iter().map(|c| c.id.as_str()).collect();
    dropped
        .iter()
        .filter(|c| !back.contains(c.id.as_str()))
        .count()
}

/// Sort by score descending, stably (ties keep their order, so a direct hit
/// stays ahead of an expanded neighbour with the same score).
fn sort_candidates(candidates: &mut [ScoredCandidate]) {
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Chunk, embed, and upsert a memory's vectors into the store.
///
/// Free-function form of [`RetrievalEngine::embed_memory`] that takes borrowed
/// providers/store so it can run inside a `tokio::spawn`ed task that owns its
/// own `Arc` clones rather than the full engine.
/// Per-call telemetry handle for ingest paths. Carries the collector,
/// project_id, and session_id so both `embed_memory` (synchronous on the
/// engine) and `spawn_ingest` (spawned, owned data) can record the same
/// stage timings.
#[derive(Clone)]
pub(crate) struct IngestTelemetry {
    pub stats: Arc<crate::telemetry::StatsCollector>,
    pub project_id: String,
    pub session_id: Option<String>,
}

impl IngestTelemetry {
    fn record(&self, stage: &'static str, ms: f64) {
        self.stats
            .record_stage(&self.project_id, stage, ms, self.session_id.as_deref());
    }
}

/// The effective Filter-mode relevance threshold, defensively bounded to
/// `[0, 1]`.
///
/// Config validation already rejects negative/NaN `search.threshold`, but a
/// config built programmatically (tests, internal callers) bypasses
/// `validate()`. A raw negative value left the gate `if threshold > 0.0`
/// permanently false (the filter silently returned everything) and a NaN made
/// every `score >= NaN` comparison false (filtering everything out). Clamping
/// here guarantees a sane bound regardless of how the config was constructed
/// (finding #4).
fn filter_threshold(raw: f64) -> f64 {
    if raw.is_nan() {
        0.0
    } else {
        raw.clamp(0.0, 1.0)
    }
}

/// The token budget used to chunk text before embedding.
///
/// `config.embeddings.max_tokens` lets an operator request *smaller* chunks,
/// but it can never exceed the model's real token limit (going higher would
/// just be silently truncated by the model). Taking the min makes the config
/// field actually authoritative instead of dead (finding #19), while staying
/// safe. Floored at 1 so chunking always makes progress.
fn effective_chunk_tokens(config_max_tokens: usize, provider_max_tokens: usize) -> usize {
    config_max_tokens.min(provider_max_tokens).max(1)
}

/// Build the texts whose vectors represent `memory` in the chunk table.
///
/// With `metadata_vector` **on** (the default), the memory is represented by
/// a dedicated metadata row — `"{title}. {summary}. tags: {tags}"` — plus the
/// content chunked on its own. Title/tag signal becomes reachable by vector
/// search (it was previously absent from the embeddings entirely) while
/// content chunks stay undiluted; the store's max-score aggregation picks
/// whichever row matches best. Benchmarked as the `fieldvec` variant in
/// `docs/contributors/embedding-analysis.md` (E1): MRR@10 ~0.75 → 0.89.
///
/// With it **off**, the legacy composition: `"{summary} {content}"` chunked
/// as one text.
///
/// Empty fields degrade gracefully (a missing title / empty tags just drop
/// out of the metadata row); an entirely empty memory yields no texts, which
/// the caller treats as "delete any stored chunks".
pub(crate) fn embedding_texts(
    memory: &Memory,
    chunk_tokens: usize,
    metadata_vector: bool,
) -> Vec<String> {
    if !metadata_vector {
        let text = format!("{} {}", memory.summary, memory.content);
        return crate::embeddings::chunk_text(&text, chunk_tokens);
    }

    // The metadata row is normally one chunk; chunk_text guards the
    // pathological case (enormous tag lists) the same way content is guarded.
    let meta = metadata_row(memory).unwrap_or_default();
    let mut texts = crate::embeddings::chunk_text(&meta, chunk_tokens);
    texts.extend(crate::embeddings::chunk_text(&memory.content, chunk_tokens));
    texts
}

/// The memory's tags, trimmed and comma-joined; `None` when there are none.
fn joined_tags(memory: &Memory) -> Option<String> {
    let tags = memory
        .tags
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    (!tags.is_empty()).then_some(tags)
}

/// The metadata row embedded alongside content chunks:
/// `"{title}. {summary}. tags: {tags}"`, with absent fields dropping out.
/// `None` when every part is empty.
fn metadata_row(memory: &Memory) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(title) = memory
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        parts.push(title.to_string());
    }
    let summary = memory.summary.trim();
    if !summary.is_empty() {
        parts.push(summary.to_string());
    }
    if let Some(tags) = joined_tags(memory) {
        parts.push(format!("tags: {tags}"));
    }
    (!parts.is_empty()).then(|| parts.join(". "))
}

/// The identity of the text a memory's stored vectors were computed from.
///
/// SHA-256 over the embed texts, **salted** with everything else that decides
/// what those vectors are: the model id, the vector width, the composition, and
/// the chunk-token budget. One column and one comparison then cover all four
/// ways vectors go stale — the text changed, the model was swapped, the
/// composition flipped, or `[embeddings].max_tokens` was retuned.
///
/// The first three are also tracked store-wide by `EmbeddingFingerprint`; the
/// fourth is not, so a `max_tokens` change re-chunks today with nothing
/// detecting it. Salting rather than deferring to the store fingerprint also
/// makes the answer *per memory*, which is what lets a reindex interrupted by
/// provider failures resume, and what lets a dry run name individual memories.
///
/// Salting has a second, quieter payoff: a digest is self-validating across
/// stores. Worktree consolidation copies a digest computed under the worktree's
/// own config; if any salt component differs, the copied value is simply not
/// what this store's engine would compute, and the memory re-embeds — with no
/// config comparison anywhere.
///
/// NOT covered, deliberately and with a known limit: the ONNX **runtime** that
/// executed the model. `model_id()` names the weights, not the build that ran
/// them, and this repo has a measured incident where a bad prebuilt runtime
/// embedded the same text to unrelated vectors under an unchanged model id
/// (see `docs/contributors/embedding-model-alternatives.md`, R6/R9). That is
/// exactly why skipping re-embeds is opt-in rather than the default: a plain
/// `reindex` must stay the repair for vectors that are wrong for reasons no
/// digest can see.
pub(crate) fn embed_digest(
    texts: &[String],
    provider: &dyn EmbeddingProvider,
    metadata_vector: bool,
    chunk_tokens: usize,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    // NUL-separated, and length-prefixed for the texts.
    //
    // The separator alone is NOT injective: U+0000 is valid UTF-8 and survives
    // `read_to_string`, so a memory containing a literal NUL could otherwise
    // make two different chunk splits hash identically — a collision in the
    // direction that matters (stale vectors read as current). Prefixing each
    // text with its byte length removes the ambiguity regardless of content.
    hasher.update(provider.model_id().as_bytes());
    hasher.update([0]);
    hasher.update(provider.dimensions().to_string().as_bytes());
    hasher.update([0]);
    hasher.update(
        if metadata_vector {
            crate::storage::manifest::COMPOSITION_METADATA_V1
        } else {
            "legacy"
        }
        .as_bytes(),
    );
    hasher.update([0]);
    hasher.update(chunk_tokens.to_string().as_bytes());
    for text in texts {
        hasher.update([0]);
        hasher.update(text.len().to_le_bytes());
        hasher.update([0]);
        hasher.update(text.as_bytes());
    }
    // `sha2` 0.11's `finalize` returns a `hybrid_array::Array` with no
    // `LowerHex`, so `{:x}` does not compile — the same gotcha every other
    // digest site in this codebase documents.
    crate::types::hex::lower_hex(&hasher.finalize())
}

/// The document string the cross-encoder reranker scores against the query.
///
/// Mirrors the embedding-side composition ([`embedding_texts`] /
/// [`metadata_row`]): title and tags are included so a candidate surfaced
/// via its metadata vector is re-scored on text that still contains the
/// matched signal — otherwise the reranker would systematically demote
/// exactly the title/tag matches the metadata vector was added to surface
/// (review finding).
fn rerank_document(memory: &Memory) -> String {
    let mut doc = String::new();
    if let Some(title) = memory
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        doc.push_str(title);
        doc.push_str(". ");
    }
    doc.push_str(&memory.summary);
    doc.push(' ');
    doc.push_str(&memory.content);
    if let Some(ref details) = memory.details {
        doc.push(' ');
        doc.push_str(details);
    }
    if let Some(tags) = joined_tags(memory) {
        doc.push_str(&format!(" tags: {tags}"));
    }
    doc
}

async fn embed_memory_with(
    provider: &dyn EmbeddingProvider,
    store: &MemoryStore,
    memory: &Memory,
    chunk_tokens: usize,
    metadata_vector: bool,
    telemetry: Option<&IngestTelemetry>,
) -> anyhow::Result<Option<Vec<f32>>> {
    let t = std::time::Instant::now();
    let chunks = embedding_texts(memory, chunk_tokens, metadata_vector);
    if let Some(t_) = telemetry {
        t_.record("create.chunk_text", t.elapsed().as_secs_f64() * 1000.0);
    }

    if chunks.is_empty() {
        store.delete_chunks(&memory.id).await?;
        return Ok(None);
    }
    let chunk_refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();

    // First-embed fingerprint stamp. Only `ops::reindex` ever stamped the
    // manifest, so a store was born unstamped and STAYED unstamped through
    // ordinary use — every query on a brand-new project then warned
    // "no recorded embedding model (legacy store) — run
    // `engramdb reindex --embeddings-only`". The vectors written here are by
    // definition the current provider's, so if the store is unstamped and
    // held no vectors before this write, this IS the moment its embedding
    // model becomes known — record it. A genuinely legacy store (unstamped
    // WITH pre-existing vectors of unknown vintage) must stay unstamped so
    // the warning keeps pointing at the real remediation. Checked before the
    // upsert (afterwards our own chunks make the store non-empty) — so it is
    // only a precondition, and the stamp below re-checks "still unstamped".
    let stamp_first_embed = matches!(store.embedding_fingerprint().await, Ok(None))
        && matches!(store.has_any_chunks().await, Ok(false));

    let t = std::time::Instant::now();
    let vectors = provider.embed_batch(&chunk_refs).await?;
    if let Some(t_) = telemetry {
        t_.record("create.embed_batch", t.elapsed().as_secs_f64() * 1000.0);
    }
    // The ingest's search vector (see `RetrievalEngine::embed_for_ingest`).
    let query = mean_direction(&vectors);

    let t = std::time::Instant::now();
    // Freshness-guarded: this runs in detached ingest tasks with no ordering
    // guarantee, so an embed of an older snapshot must not overwrite newer
    // vectors, and an embed racing a delete must not re-insert orphan chunks.
    //
    // The guard compares what the vectors depend on — the texts this memory
    // embeds to, under the same provider and config — not `updated_at`: a
    // link, challenge, verify or `harvest mark --memory` write racing the
    // embed bumps `updated_at` without changing a single vector, and used to
    // drop them for good (nothing retries a detached ingest). Equal texts
    // under one provider and config is exactly an equal `embed_digest`. A
    // memory edited to other text (or to none) or deleted is still refused.
    let digest = embed_digest(&chunks, provider, metadata_vector, chunk_tokens);
    let written = store
        .upsert_chunks_if(&memory.id, vectors, Some(digest), move |current| {
            embedding_texts(current, chunk_tokens, metadata_vector) == chunks
        })
        .await?;
    if !written {
        tracing::info!(
            memory_id = %memory.id,
            "skipped chunk upsert: the memory's text changed, or it was deleted, since this snapshot"
        );
    }
    if let Some(t_) = telemetry {
        t_.record("create.upsert_chunks", t.elapsed().as_secs_f64() * 1000.0);
    }
    if written && stamp_first_embed {
        let fingerprint = first_embed_fingerprint(provider, metadata_vector);
        // Conditional, not `set_embedding_fingerprint`: `stamp_first_embed`
        // was decided before the embed and the upsert, and the store may have
        // been stamped since (a reindex for a new model, a concurrent first
        // embed). Overwriting that newer stamp with this ingest's model made
        // the manifest misdescribe the vectors; the store re-checks under its
        // lock instead. Best-effort: a failed stamp only means the Untracked
        // warning can still appear; never fail the embed over it.
        if let Err(e) = store
            .stamp_embedding_fingerprint_if_unset(fingerprint)
            .await
        {
            tracing::warn!("failed to stamp embedding fingerprint on first embed: {e}");
        }
    }
    Ok(query)
}

/// What [`RetrievalEngine::embed_unwritten`] computed for a memory that is
/// not written yet: its chunk vectors, their `embed_sha256`, the ingest's
/// search vector, and the fingerprint to stamp if these are the store's first
/// vectors.
pub struct NewVectors {
    pub vectors: Vec<Vec<f32>>,
    pub embed_sha256: String,
    pub query: Option<Vec<f32>>,
    pub fingerprint: Option<crate::storage::manifest::EmbeddingFingerprint>,
}

/// The fingerprint a first embed records for `provider`.
fn first_embed_fingerprint(
    provider: &dyn EmbeddingProvider,
    metadata_vector: bool,
) -> crate::storage::manifest::EmbeddingFingerprint {
    crate::storage::manifest::EmbeddingFingerprint {
        model: provider.model_id(),
        dimensions: provider.dimensions(),
        // Mirrors `EmbeddingsConfig::composition_id` — `metadata_vector`
        // here IS `config.embeddings.metadata_vector` at every call site.
        composition: metadata_vector
            .then(|| crate::storage::manifest::COMPOSITION_METADATA_V1.to_string()),
    }
}

/// The unit-length mean of a memory's chunk vectors: the search vector an
/// ingest uses for its neighbours. It stands for the whole memory the way the
/// separate `summary content` embed it replaces did; searching with the
/// metadata row alone scored templated memories (same title shape, same
/// tags) as near-duplicates and flooded the proposers. `None` for no vectors
/// or a zero mean.
fn mean_direction(vectors: &[Vec<f32>]) -> Option<Vec<f32>> {
    let first = vectors.first()?;
    if vectors.len() == 1 {
        return Some(first.clone());
    }
    let mut mean = vec![0.0f32; first.len()];
    let dims = first.len();
    for v in vectors.iter().filter(|v| v.len() == dims) {
        // Each chunk counts once whatever its own norm.
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if n > 0.0 && n.is_finite() {
            for (m, x) in mean.iter_mut().zip(v) {
                *m += x / n;
            }
        }
    }
    let n = mean.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 && n.is_finite() {
        mean.iter_mut().for_each(|x| *x /= n);
        Some(mean)
    } else {
        None
    }
}

/// What [`RetrievalEngine::ingest_findings`] found next to a new memory.
#[derive(Debug, Clone, Default)]
pub struct IngestFindings {
    /// `(existing id, NLI result)` above `[nli].contradiction_threshold`.
    pub contradictions: Vec<(String, NliResult)>,
    /// Live neighbours at or above `[links].related_threshold`, best first,
    /// at most `[links].similar_candidates`. Scores are the vector search's.
    pub neighbours: Vec<crate::storage::lance_index::VectorMatch>,
    /// Neighbours NLI classified and found *not* contradictory. Only these
    /// may be proposed as `duplicates`: a negated pair ("X is on" / "X is
    /// off") embeds almost identically, so similarity alone cannot tell a
    /// duplicate from a contradiction. Empty when NLI is off.
    pub nli_cleared: std::collections::HashSet<String>,
}

/// The step `spawn_ingest` runs last (see there). Boxed so `ops` can hand in
/// a closure over its own config without this layer naming `ops` types.
pub type IngestFollowUp = Box<
    dyn FnOnce(
            MemoryStore,
            Memory,
            IngestFindings,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send,
>;

/// [`RetrievalEngine::ingest_findings`] in free-function form, for the
/// spawned ingest task. One vector search with the vector the ingest's embed
/// just produced, sized for whichever of the two consumers asks for more
/// candidates.
async fn ingest_findings_with(
    nli: Option<&dyn NliProvider>,
    store: &MemoryStore,
    nli_config: &crate::types::NliConfig,
    links_config: &crate::types::LinksConfig,
    memory: &Memory,
    query: Vec<f32>,
) -> IngestFindings {
    let want_similar = links_config.similar_enabled();
    if nli.is_none() && !want_similar {
        return IngestFindings::default();
    }
    // Neighbours are filtered after the search (the memory itself, then
    // invalidated ones), so the similarity side over-fetches: +1 for the
    // memory itself and as many again for history. Bounded by
    // `similar_candidates` (validated <= 50).
    let similar_limit = 2 * links_config.similar_candidates + 1;
    let limit = match (nli.is_some(), want_similar) {
        (true, true) => nli_config.max_comparisons.max(similar_limit),
        (true, false) => nli_config.max_comparisons,
        _ => similar_limit,
    };
    let matches = match store.vector_search(query, limit, None).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(memory_id = %memory.id, "ingest neighbour search failed: {e}");
            return IngestFindings::default();
        }
    };
    let mut findings = IngestFindings::default();
    if let Some(nli) = nli {
        let nli_matches: Vec<_> = matches
            .iter()
            .take(nli_config.max_comparisons)
            .cloned()
            .collect();
        match classify_among(nli, store, nli_config, memory, nli_matches).await {
            Ok(all) => {
                for (id, result) in all {
                    if f64::from(result.contradiction) >= nli_config.contradiction_threshold {
                        findings.contradictions.push((id, result));
                    } else {
                        findings.nli_cleared.insert(id);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(memory_id = %memory.id, "Background contradiction detection failed: {e}")
            }
        }
    }
    if want_similar {
        let close: Vec<_> = matches
            .into_iter()
            .filter(|m| m.id != memory.id && m.score >= links_config.related_threshold)
            .collect();
        // Invalidated neighbours are history, not relations worth proposing.
        let ids: Vec<&str> = close.iter().map(|m| m.id.as_str()).collect();
        let now = Utc::now();
        let live: std::collections::HashSet<String> = store
            .get_batch(&ids)
            .await
            .map(|v| {
                v.into_iter()
                    .filter(|(_, m)| !m.is_invalidated_at(now))
                    .map(|(id, _)| id)
                    .collect()
            })
            .unwrap_or_default();
        findings.neighbours = close
            .into_iter()
            .filter(|m| live.contains(&m.id))
            .take(links_config.similar_candidates)
            .collect();
    }
    findings
}

/// Vector-search for similar memories and run NLI classification, returning
/// the (memory_id, NliResult) pairs that exceed the contradiction threshold.
///
/// Free-function form of [`RetrievalEngine::detect_contradictions`] for use
/// inside spawned tasks.
async fn detect_contradictions_with(
    nli: &dyn NliProvider,
    embedding_provider: &dyn EmbeddingProvider,
    store: &MemoryStore,
    nli_config: &crate::types::NliConfig,
    memory: &Memory,
) -> anyhow::Result<Vec<(String, NliResult)>> {
    let text = format!("{} {}", memory.summary, memory.content);
    let query_vector = embedding_provider.embed(&text).await?;

    let matches = store
        .vector_search(query_vector, nli_config.max_comparisons, None)
        .await?;
    contradictions_among(nli, store, nli_config, memory, matches).await
}

/// The NLI half of [`detect_contradictions_with`], over vector matches the
/// caller already has.
async fn contradictions_among(
    nli: &dyn NliProvider,
    store: &MemoryStore,
    nli_config: &crate::types::NliConfig,
    memory: &Memory,
    matches: Vec<crate::storage::lance_index::VectorMatch>,
) -> anyhow::Result<Vec<(String, NliResult)>> {
    Ok(classify_among(nli, store, nli_config, memory, matches)
        .await?
        .into_iter()
        .filter(|(_, r)| f64::from(r.contradiction) >= nli_config.contradiction_threshold)
        .collect())
}

/// Every live candidate among `matches` (above `[nli].similarity_threshold`,
/// not the memory itself) with its NLI result, contradictory or not.
async fn classify_among(
    nli: &dyn NliProvider,
    store: &MemoryStore,
    nli_config: &crate::types::NliConfig,
    memory: &Memory,
    matches: Vec<crate::storage::lance_index::VectorMatch>,
) -> anyhow::Result<Vec<(String, NliResult)>> {
    let candidates: Vec<_> = matches
        .into_iter()
        .filter(|m| m.score >= nli_config.similarity_threshold && m.id != memory.id)
        .collect();

    if candidates.is_empty() {
        return Ok(vec![]);
    }

    // Single batched load (one dir scan) instead of a per-candidate
    // `store.get` (a full dir scan each) — this runs on every create/update
    // with NLI enabled. Ids missing on disk are silently skipped, matching
    // the old per-get error swallowing. Invalidated memories (closed
    // validity windows, §2.4) are excluded from the contradiction candidate
    // set (§14.14): a new memory that contradicts an already-invalidated one
    // is the EXPECTED outcome of supersession, not a dispute worth
    // challenging.
    let ingest_now = Utc::now();
    let candidate_ids: Vec<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
    let candidate_memories: Vec<Memory> = store
        .get_batch(&candidate_ids)
        .await
        .map(|loaded| {
            loaded
                .into_iter()
                .map(|(_, m)| m)
                .filter(|m| !m.is_invalidated_at(ingest_now))
                .collect()
        })
        .unwrap_or_default();

    if candidate_memories.is_empty() {
        return Ok(vec![]);
    }

    let pair_refs: Vec<(&str, &str)> = candidate_memories
        .iter()
        .map(|m| (memory.summary.as_str(), m.summary.as_str()))
        .collect();

    let results = nli.classify_batch(&pair_refs).await?;
    Ok(candidate_memories
        .iter()
        .map(|m| m.id.clone())
        .zip(results)
        .collect())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod epistemic_retrieval_tests;

#[cfg(test)]
mod graph_signal_tests;

#[cfg(test)]
mod text_query_pin_tests;
