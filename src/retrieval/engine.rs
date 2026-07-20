//! Core retrieval engine implementation.
//!
//! The retrieval engine orchestrates the process of finding relevant memories based on
//! contextual queries. It combines multiple retrieval strategies including scope matching,
//! semantic search (when embeddings are available), and keyword search.

use super::filters::{apply_index_filters, build_filter_predicate, SearchFilters};
use super::reranker::Reranker;
use crate::embeddings::EmbeddingProvider;
use crate::nli::{NliProvider, NliResult};
use crate::scoring::{
    composite_score, composite_score_ignore_decay, composite_score_target,
    composite_score_target_ignore_decay, ScoreBreakdown, ScoreTarget, ScoringContext,
};
use crate::storage::{MemoryStore, Result};
use crate::types::{EngramConfig, Epistemic, Memory, MemoryType, Situation};
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

    /// Internal: record the ids of the memories a query returned
    /// (above-threshold survivors) — the §11.3 promotion data source.
    fn record_retrieved_memories(&self, result: &RetrievalResult) {
        if let (Some(stats), Some(pid)) = (self.stats.as_ref(), self.project_id.as_ref()) {
            let ids: Vec<String> = result
                .memories
                .iter()
                .map(|sm| sm.memory.id.clone())
                .collect();
            stats.record_retrieved_memories(pid, &ids, self.session_id.as_deref());
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
    pub fn spawn_ingest(&self, memory: Memory) -> Option<tokio::task::JoinHandle<()>> {
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
        let telemetry = self.ingest_telemetry();

        Some(tokio::spawn(async move {
            if let Err(e) = embed_memory_with(
                provider.as_ref(),
                &store,
                &memory,
                chunk_tokens,
                metadata_vector,
                telemetry.as_ref(),
            )
            .await
            {
                tracing::warn!(
                    memory_id = %memory.id,
                    "Background embed failed: {}",
                    e
                );
                return;
            }

            let nli = match nli_provider {
                Some(n) => n,
                None => return,
            };

            match detect_contradictions_with(
                nli.as_ref(),
                provider.as_ref(),
                &store,
                &nli_config,
                &memory,
            )
            .await
            {
                Ok(contradictions) if !contradictions.is_empty() => {
                    tracing::debug!(
                        memory_id = %memory.id,
                        count = contradictions.len(),
                        "NLI detected contradictions with existing memories"
                    );
                    crate::nli::challenge_for_contradictions(
                        &store,
                        &crate::nli::NewMemoryMeta::from(&memory),
                        &contradictions,
                    )
                    .await;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(
                        memory_id = %memory.id,
                        "Background contradiction detection failed: {}",
                        e
                    );
                }
            }
        }))
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

    /// Embed arbitrary text with the engine's embedding provider. `None`
    /// when no provider is configured or the embed fails (logged). Used by
    /// the §11.4 consolidation pass; NOT a retrieval path.
    pub async fn embed_text(&self, text: &str) -> Option<Vec<f32>> {
        let provider = self.embedding_provider.as_ref()?;
        match provider.embed(text).await {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::debug!("embed_text failed (non-fatal): {e}");
                None
            }
        }
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
    /// otherwise) → keyword search → composite scoring → threshold →
    /// optional cross-encoder rerank. Per memory, the composite weights are
    /// chosen from the query evidence:
    ///
    /// | keyword | semantic                  | weights (defaults)                  |
    /// |---------|---------------------------|-------------------------------------|
    /// | yes     | yes (incl. missed top-k)  | `with_keyword`: 0.45kw 0.30sem 0.25rel |
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
        use crate::search::{keyword_search, normalize_keyword_score, query_token_count};

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
        let all_entries = self
            .store
            .list_for_filtering_where(pushdown_predicate)
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
        if query.query.is_none() && query.mode == RetrievalMode::Rank {
            return self
                .rank_scope_only_from_index(
                    query,
                    query_path.as_deref(),
                    &filtered_entries,
                    query_started,
                )
                .await;
        }

        // Step 3: Batch-load surviving memories (single dir scan). Needed on the
        // query / Filter paths for keyword search and result materialization.
        // The scalar-filter pushdown (Step 1) shrinks this batch when those
        // filters are selective.
        let ids: Vec<&str> = filtered_entries.iter().map(|e| e.id.as_str()).collect();
        let loaded = self.store.get_batch(&ids).await?;
        let memory_map: HashMap<String, Memory> = loaded.into_iter().collect();

        // Step 4: If query text + embeddings available, get semantic scores.
        let semantic_scores_map: Option<HashMap<String, f64>> = if let Some(ref q) = query.query {
            if let Some(provider) = &self.embedding_provider {
                let t_embed = std::time::Instant::now();
                let embed_result = provider.embed(q).await;
                self.record_stage("embed", t_embed.elapsed().as_secs_f64() * 1000.0);

                if let Ok(query_vector) = embed_result {
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
                    //
                    // Narrowing happens in two places and both must count: the
                    // Rust-side filters (visible as `filtered < total`) and the
                    // LanceDB predicate pushdown (which narrows the scan before
                    // `total_entry_count` is measured, so `filtered == total`
                    // even though the store holds more rows).
                    let narrowed = pushdown_applied || filtered_entries.len() < total_entry_count;
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
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
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
        // scan. It covers exactly the memories being scored (memory_map's ids
        // come from filtered_entries), which is all `embedded_ids` is consulted
        // for. The migration keeps the column authoritative (a reindex rebuilds
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
            let memories_vec: Vec<&Memory> = memory_map.values().collect();
            let kw_results = keyword_search(q, &memories_vec);
            let num_tokens = query_token_count(q);
            kw_results
                .into_iter()
                .map(|(idx, raw)| {
                    let norm = normalize_keyword_score(raw, num_tokens);
                    (memories_vec[idx].id.clone(), norm)
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
        let mut candidates: Vec<ScoredCandidate> = Vec::new();
        let query_path = query_path.as_deref();
        let now = Utc::now();

        let t_score = std::time::Instant::now();
        for entry in filtered_entries.iter() {
            let memory = match memory_map.get(&entry.id) {
                Some(m) => m,
                None => continue,
            };

            // Gather query evidence for this memory. Semantic comes from the
            // vector top-k when present; an embedded memory that missed the
            // top-k window counts as weak evidence (sem = 0.0); a memory with
            // no chunks at all keeps sem = None (no evidence). See Step 4.5.
            let sem_score = semantic_scores_map
                .as_ref()
                .and_then(|m| m.get(&memory.id))
                .copied()
                .or_else(|| {
                    embedded_ids
                        .as_ref()
                        .is_some_and(|ids| ids.contains(&memory.id))
                        .then_some(0.0)
                });
            let kw_score = keyword_map.get(&memory.id).copied();

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
            .with_situation(query.situation);

            // Expired memories (only present when `include_expired` is set —
            // otherwise Step 2.5b already dropped them) are scored ignoring
            // decay: their decay factor is at/near floor by definition, so
            // normal scoring would zero them out and including them would be
            // pointless. Active memories keep normal decay scoring regardless
            // of `include_expired` — a half-decayed active memory must not
            // rank as fresh just because the caller asked to also see expired
            // ones.
            let breakdown = if memory.is_expired_at(now) {
                composite_score_ignore_decay(memory, &context, &self.config, now)
            } else {
                composite_score(memory, &context, &self.config, now)
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
                    !filter_tags.is_empty() && filter_tags.iter().any(|t| memory.tags.contains(t))
                });
                // `query_path` (not `query.path`): an empty-string path was
                // normalized away in Step 1.5 and must not count as a
                // sufficiency signal — it filtered nothing.
                let user_scope_supplied = query_path.is_some() || !query.logical.is_empty();
                if !(has_kw || has_tag || user_scope_supplied) {
                    continue;
                }
            }

            candidates.push(ScoredCandidate {
                id: memory.id.clone(),
                score: breakdown.final_score,
                breakdown,
            });
        }

        self.record_stage("score", t_score.elapsed().as_secs_f64() * 1000.0);

        // Step 6: Threshold. Rank mode uses retrieval.relevance_threshold;
        // Filter mode uses the stricter search.threshold when set, since the
        // flow is "find specific memories" rather than "browse context".
        let threshold = match query.mode {
            RetrievalMode::Rank => self.config.retrieval.relevance_threshold,
            RetrievalMode::Filter => filter_threshold(self.config.search.threshold),
        };
        if threshold > 0.0 {
            candidates.retain(|c| c.score >= threshold);
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
        // it reads memory text from `memory_map` by id, so it needs no cloned
        // memories.
        // Only timed when the reranker is actually configured + enabled to
        // avoid recording bogus 0ms samples for the no-op path.
        if let Some(ref q) = query.query {
            if self.reranking_available() && !candidates.is_empty() {
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

        let total = candidates.len();

        // Step 9: Apply max_results limit, THEN materialize survivors. Cloning
        // the memory is deferred to here so only the returned memories are
        // cloned, not every scored candidate (finding performance-0).
        let max_results = query
            .max_results
            .unwrap_or(self.config.retrieval.max_results);
        candidates.truncate(max_results);

        let mut scored_memories: Vec<ScoredMemory> = candidates
            .into_iter()
            .filter_map(|c| {
                // Every candidate id came from `memory_map`, so the lookup
                // always hits; `filter_map` guards defensively without panicking.
                memory_map.get(&c.id).map(|memory| ScoredMemory {
                    memory: memory.clone(),
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
        };

        self.record_stage(
            "query.total",
            query_started.elapsed().as_secs_f64() * 1000.0,
        );
        self.record_query_outcome(!result.memories.is_empty(), &result.retrieval_quality);
        self.record_retrieved_memories(&result);

        Ok(result)
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
    async fn rank_scope_only_from_index(
        &self,
        query: &RetrievalQuery,
        ctx_path: Option<&str>,
        filtered_entries: &[crate::storage::IndexForFiltering],
        query_started: std::time::Instant,
    ) -> Result<RetrievalResult> {
        let now = Utc::now();

        let t_score = std::time::Instant::now();
        let mut candidates: Vec<ScoredCandidate> = filtered_entries
            .iter()
            .map(|e| {
                let target = ScoreTarget {
                    created_at: e.created_at,
                    decay: &e.decay,
                    criticality: e.criticality,
                    physical: &e.physical,
                    logical: &e.logical,
                    provenance_source: e.provenance_source,
                    status: e.status,
                    epistemic: e.epistemic,
                    verified_at: e.verified_at,
                };
                let context = ScoringContext::scope_only(ctx_path, &query.logical)
                    .with_situation(query.situation);
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
            })
            .collect();
        self.record_stage("score", t_score.elapsed().as_secs_f64() * 1000.0);

        let threshold = self.config.retrieval.relevance_threshold;
        if threshold > 0.0 {
            candidates.retain(|c| c.score >= threshold);
        }
        candidates.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let total = candidates.len();
        let max_results = query
            .max_results
            .unwrap_or(self.config.retrieval.max_results);
        candidates.truncate(max_results);

        // Materialize ONLY the survivors — the whole point of this path.
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
        };
        self.record_stage(
            "query.total",
            query_started.elapsed().as_secs_f64() * 1000.0,
        );
        self.record_query_outcome(!result.memories.is_empty(), &result.retrieval_quality);
        self.record_retrieved_memories(&result);
        Ok(result)
    }
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
) -> anyhow::Result<()> {
    let t = std::time::Instant::now();
    let chunks = embedding_texts(memory, chunk_tokens, metadata_vector);
    if let Some(t_) = telemetry {
        t_.record("create.chunk_text", t.elapsed().as_secs_f64() * 1000.0);
    }

    if chunks.is_empty() {
        store.delete_chunks(&memory.id).await?;
        return Ok(());
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
    // upsert (afterwards our own chunks make the store non-empty).
    let stamp_first_embed = matches!(store.embedding_fingerprint().await, Ok(None))
        && matches!(store.has_any_chunks().await, Ok(false));

    let t = std::time::Instant::now();
    let vectors = provider.embed_batch(&chunk_refs).await?;
    if let Some(t_) = telemetry {
        t_.record("create.embed_batch", t.elapsed().as_secs_f64() * 1000.0);
    }

    let t = std::time::Instant::now();
    // Freshness-guarded: this runs in detached ingest tasks with no ordering
    // guarantee, so an embed of an older snapshot must not overwrite newer
    // vectors, and an embed racing a delete must not re-insert orphan chunks.
    let written = store
        .upsert_chunks_if_current(&memory.id, vectors, memory.updated_at)
        .await?;
    if !written {
        tracing::debug!(
            memory_id = %memory.id,
            "skipped chunk upsert: memory changed or was deleted since this snapshot"
        );
    }
    if let Some(t_) = telemetry {
        t_.record("create.upsert_chunks", t.elapsed().as_secs_f64() * 1000.0);
    }
    if written && stamp_first_embed {
        let fingerprint = crate::storage::manifest::EmbeddingFingerprint {
            model: provider.model_id(),
            dimensions: provider.dimensions(),
            // Mirrors `EmbeddingsConfig::composition_id` — `metadata_vector`
            // here IS `config.embeddings.metadata_vector` at every call site.
            composition: metadata_vector
                .then(|| crate::storage::manifest::COMPOSITION_METADATA_V1.to_string()),
        };
        // Best-effort: a failed stamp only means the Untracked warning can
        // still appear; never fail the embed over it.
        if let Err(e) = store.set_embedding_fingerprint(fingerprint).await {
            tracing::warn!("failed to stamp embedding fingerprint on first embed: {e}");
        }
    }
    Ok(())
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

    let mut contradictions = Vec::new();
    for (mem, result) in candidate_memories.iter().zip(results) {
        if result.contradiction as f64 >= nli_config.contradiction_threshold {
            contradictions.push((mem.id.clone(), result));
        }
    }

    Ok(contradictions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;

    // Finding #4: the Filter-mode threshold must be bounded to [0, 1] regardless
    // of how the config was constructed, so the relevance gate can never be
    // silently disabled (negative) or turned into a filter-everything NaN.
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
    /// ~100MB ONNX model once per test (which causes OOM when parallel).
    /// Built through the `engram-models` loader (default BGE reranker base) so
    /// the core crate needs no direct `fastembed` dependency, even in tests.
    ///
    /// The `fastembed` loader (`LocalReranker`) only exists with `onnxruntime`;
    /// on a pure-`tract` build there is no in-process reranker, so `try_reranker`
    /// returns `None` and the reranker tests skip (they `return` early on `None`).
    #[cfg(feature = "onnxruntime")]
    fn try_reranker() -> Option<Arc<dyn Reranker>> {
        use crate::retrieval::reranker::LocalReranker;
        use std::sync::LazyLock;
        static SHARED_RERANKER: LazyLock<Option<Arc<dyn Reranker>>> =
            LazyLock::new(|| LocalReranker::load("bge-reranker-base").ok());
        SHARED_RERANKER.clone()
    }

    #[cfg(not(feature = "onnxruntime"))]
    fn try_reranker() -> Option<Arc<dyn Reranker>> {
        None
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
        use crate::types::{
            Decay, EngramConfig, Memory, MemoryType, Provenance, Status, Visibility,
        };
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
            (MemoryType::Hazard, 0.50, false), // below min_criticality
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
            let mut memory =
                Memory::new(MemoryType::Decision, summary, content, Provenance::human());
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
            async fn classify_batch(
                &self,
                _pairs: &[(&str, &str)],
            ) -> anyhow::Result<Vec<NliResult>> {
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
            async fn classify_batch(
                &self,
                _pairs: &[(&str, &str)],
            ) -> anyhow::Result<Vec<NliResult>> {
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
    // `onnxruntime` build (there is no in-process ONNX on a pure-`tract` build).
    #[cfg(feature = "onnxruntime")]
    #[tokio::test]
    async fn test_detect_contradictions_end_to_end_finds_real_contradiction() {
        use crate::embeddings::OnnxProvider;
        use crate::nli::OnnxNliProvider;
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let Some(embedding) = OnnxProvider::try_new() else {
            eprintln!("skipping: ONNX embedding model unavailable");
            return;
        };
        let Some(nli) = OnnxNliProvider::try_new("cross-encoder/nli-deberta-v3-xsmall") else {
            eprintln!("skipping: NLI model unavailable");
            return;
        };

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

        let Some(embedding) = OnnxProvider::try_new() else {
            eprintln!("skipping: ONNX embedding model unavailable");
            return;
        };

        // NLI shouldn't be called — we route through the similarity
        // threshold gate first. Use a dummy provider that panics if hit.
        struct ShouldNotBeCalled;
        #[async_trait::async_trait]
        impl NliProvider for ShouldNotBeCalled {
            async fn classify(&self, _p: &str, _h: &str) -> anyhow::Result<NliResult> {
                panic!("NLI must not be called when candidates are filtered out");
            }
            async fn classify_batch(
                &self,
                _pairs: &[(&str, &str)],
            ) -> anyhow::Result<Vec<NliResult>> {
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
        let results_no_rerank =
            filter_query(&engine_no_rerank, "database migration strategy").await;

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

        // The final score must follow the combined with_keyword formula
        // (scope and trust multipliers are 1.0 here: no scope context, human
        // provenance), proving both signals actually moved the score.
        let w = &config.retrieval.scoring.with_keyword;
        let expected = w.keyword.unwrap() * kw
            + w.semantic.unwrap() * sem
            + w.relevance * combined.score_breakdown.relevance;
        assert!(
            (combined.score - expected).abs() < 0.01,
            "combined score {} should match 0.45*kw + 0.30*sem + 0.25*rel = {}",
            combined.score,
            expected
        );

        // The semantic-only memory keeps the with_query path.
        let semantic_only = by_id(&sem_only.id);
        assert!(semantic_only.score_breakdown.keyword.is_none());
        assert!(semantic_only.score_breakdown.semantic.unwrap() > 0.9);
        // And the two regimes produce different scores for the same sem/crit.
        assert!(
            (combined.score - semantic_only.score).abs() > 0.05,
            "keyword evidence must change the score: combined={} sem_only={}",
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
}

#[cfg(test)]
mod epistemic_retrieval_tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Provenance, Visibility};
    use tempfile::TempDir;

    async fn seeded_engine() -> (TempDir, RetrievalEngine) {
        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        // One memory per class (types chosen so the class is the DIAGONAL
        // default — files stay pre-epistemic-shaped), plus one invalidated.
        for (id, type_) in [
            ("mem-fact", MemoryType::Hazard),
            ("mem-obs", MemoryType::Debug),
            ("mem-dec", MemoryType::Decision),
        ] {
            let mut m = Memory::new(type_, id, "content", Provenance::human());
            m.id = id.to_string();
            m.visibility = Visibility::Shared;
            m.criticality = 0.8;
            m.decay = Some(crate::types::Decay::none());
            store.create(&m).await.unwrap();
        }
        let mut gone = Memory::new(
            MemoryType::Hazard,
            "mem-invalidated",
            "content",
            Provenance::human(),
        );
        gone.id = "mem-invalidated".to_string();
        gone.visibility = Visibility::Shared;
        gone.criticality = 0.8;
        gone.decay = Some(crate::types::Decay::none());
        gone.invalidated_at = Some(Utc::now() - chrono::Duration::days(1));
        store.create(&gone).await.unwrap();

        let mut config = EngramConfig::default();
        config.retrieval.relevance_threshold = 0.0;
        (temp_dir, RetrievalEngine::new(store, config))
    }

    fn ids(result: &RetrievalResult) -> Vec<&str> {
        let mut v: Vec<&str> = result
            .memories
            .iter()
            .map(|m| m.memory.id.as_str())
            .collect();
        v.sort();
        v
    }

    #[tokio::test]
    async fn epistemic_hard_filter_narrows_both_modes() {
        let (_tmp, engine) = seeded_engine().await;

        // Rank mode.
        let result = engine
            .query(&RetrievalQuery {
                epistemic: Some(vec![Epistemic::Observation]),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ids(&result), vec!["mem-obs"]);

        // Two classes.
        let result = engine
            .query(&RetrievalQuery {
                epistemic: Some(vec![Epistemic::Observation, Epistemic::Decision]),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ids(&result), vec!["mem-dec", "mem-obs"]);

        // Filter mode (tags signal to satisfy Step 0) — class filter still
        // applies. Use a query that keyword-matches everything instead.
        let result = engine
            .query(&RetrievalQuery {
                mode: RetrievalMode::Filter,
                query: Some("content".to_string()),
                epistemic: Some(vec![Epistemic::Decision]),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(ids(&result), vec!["mem-dec"]);
    }

    #[tokio::test]
    async fn invalidated_excluded_by_default_included_on_optin() {
        let (_tmp, engine) = seeded_engine().await;

        // Default: the invalidated memory never surfaces.
        let result = engine.query(&RetrievalQuery::default()).await.unwrap();
        assert_eq!(ids(&result), vec!["mem-dec", "mem-fact", "mem-obs"]);

        // Opt-in: it is scored and returned like any other memory.
        let result = engine
            .query(&RetrievalQuery {
                include_invalidated: Some(true),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            ids(&result),
            vec!["mem-dec", "mem-fact", "mem-invalidated", "mem-obs"]
        );

        // A future-dated (scheduled) invalidation is still valid now → included
        // by default, exactly like a future expires_at.
        let store = engine.store();
        store
            .update_with("mem-fact", |m| {
                m.invalidated_at = Some(Utc::now() + chrono::Duration::days(30));
                Ok(())
            })
            .await
            .unwrap();
        let result = engine.query(&RetrievalQuery::default()).await.unwrap();
        assert_eq!(ids(&result), vec!["mem-dec", "mem-fact", "mem-obs"]);
    }

    #[tokio::test]
    async fn situation_reweights_rank_order() {
        let (_tmp, engine) = seeded_engine().await;

        // Neutral: all three active memories tie (same criticality/trust) and
        // every breakdown records a neutral multiplier.
        let neutral = engine.query(&RetrievalQuery::default()).await.unwrap();
        for m in &neutral.memories {
            assert_eq!(m.score_breakdown.situation_multiplier, 1.0);
        }

        // session_start: fact (1.0) > decision (0.92) > observation (0.8).
        let result = engine
            .query(&RetrievalQuery {
                situation: Some(Situation::SessionStart),
                ..Default::default()
            })
            .await
            .unwrap();
        let ordered: Vec<&str> = result
            .memories
            .iter()
            .map(|m| m.memory.id.as_str())
            .collect();
        assert_eq!(ordered, vec!["mem-fact", "mem-dec", "mem-obs"]);
        let mults: Vec<f64> = result
            .memories
            .iter()
            .map(|m| m.score_breakdown.situation_multiplier)
            .collect();
        assert!((mults[0] - 1.0).abs() < 1e-9);
        assert!((mults[1] - 0.92).abs() < 1e-9);
        assert!((mults[2] - 0.8).abs() < 1e-9);

        // debugging: observation on top.
        let result = engine
            .query(&RetrievalQuery {
                situation: Some(Situation::Debugging),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(result.memories[0].memory.id, "mem-obs");
    }
}
