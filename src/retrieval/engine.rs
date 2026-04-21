//! Core retrieval engine implementation.
//!
//! The retrieval engine orchestrates the process of finding relevant memories based on
//! contextual queries. It combines multiple retrieval strategies including scope matching,
//! semantic search (when embeddings are available), and keyword search.

use super::filters::{apply_index_filters, SearchFilters};
use crate::embeddings::EmbeddingProvider;
use crate::nli::{NliProvider, NliResult};
use crate::scoring::{
    composite_score, composite_score_ignore_decay, ScoreBreakdown, ScoringContext,
};
use crate::storage::{MemoryStore, Result};
use crate::types::{EngramConfig, Memory, MemoryType};
use chrono::Utc;
use fastembed::TextRerank;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

    /// Logical scopes - dot-notation domains; contribute to scoring only,
    /// never used as a filter.
    pub logical: Vec<String>,

    /// Search query text (for semantic similarity + keyword search)
    pub query: Option<String>,

    /// Filter by memory types
    pub types: Option<Vec<MemoryType>>,

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
    embedding_provider: Option<Box<dyn EmbeddingProvider>>,
    nli_provider: Option<Box<dyn NliProvider>>,
    reranker: Option<Arc<Mutex<TextRerank>>>,
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
        }
    }

    /// Add an embedding provider to the retrieval engine.
    ///
    /// Enables semantic search capabilities. Vector storage is handled by
    /// the MemoryStore's integrated LanceDB. Returns self for method chaining.
    pub fn with_embedding_provider(mut self, provider: Box<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Add a cross-encoder reranker to the retrieval engine.
    ///
    /// When configured (and `config.rerank.enabled` is true), retrieved results
    /// are re-scored using the cross-encoder before being returned. The reranker
    /// is wrapped in `Arc<Mutex<>>` because `TextRerank::rerank()` requires `&mut self`.
    pub fn with_reranker(mut self, reranker: Arc<Mutex<TextRerank>>) -> Self {
        self.reranker = Some(reranker);
        self
    }

    /// Check if embeddings are available.
    ///
    /// Returns true if an embedding provider is configured. Vector storage
    /// is always available via the MemoryStore's LanceDB.
    pub fn embeddings_available(&self) -> bool {
        self.embedding_provider.is_some()
    }

    /// Add an NLI provider to the retrieval engine.
    ///
    /// Enables automatic contradiction detection between memories.
    /// Returns self for method chaining.
    pub fn with_nli_provider(mut self, provider: Box<dyn NliProvider>) -> Self {
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

        let nli_config = &self.config.nli;

        // Embed the new memory's text to find similar candidates
        let text = format!("{} {}", memory.summary, memory.content);
        let query_vector = embedding_provider.embed(&text).await?;

        // Vector search for similar memories
        let matches = self
            .store
            .vector_search(query_vector, nli_config.max_comparisons)
            .await?;

        // Filter by similarity threshold and exclude self
        let candidates: Vec<_> = matches
            .into_iter()
            .filter(|m| m.score >= nli_config.similarity_threshold && m.id != memory.id)
            .collect();

        if candidates.is_empty() {
            return Ok(vec![]);
        }

        // Load candidate memories
        let mut candidate_memories: Vec<Memory> = Vec::new();

        for candidate in &candidates {
            if let Ok(mem) = self.store.get(&candidate.id).await {
                candidate_memories.push(mem);
            }
        }

        if candidate_memories.is_empty() {
            return Ok(vec![]);
        }

        // Build refs for batch classification directly from source references
        let pair_refs: Vec<(&str, &str)> = candidate_memories
            .iter()
            .map(|m| (memory.summary.as_str(), m.summary.as_str()))
            .collect();

        let results = nli.classify_batch(&pair_refs).await?;

        // Filter by contradiction threshold
        let mut contradictions = Vec::new();
        for (mem, result) in candidate_memories.iter().zip(results) {
            if result.contradiction as f64 >= nli_config.contradiction_threshold {
                contradictions.push((mem.id.clone(), result));
            }
        }

        Ok(contradictions)
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
            let text = format!("{} {}", memory.summary, memory.content);
            let chunks = crate::embeddings::chunk_text(&text, provider.max_tokens());
            if chunks.is_empty() {
                self.store.delete_chunks(&memory.id).await?;
                return Ok(());
            }
            let chunk_refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
            let vectors = provider.embed_batch(&chunk_refs).await?;
            self.store.upsert_chunks(&memory.id, vectors).await?;
        }
        Ok(())
    }

    /// Get a reference to the memory store.
    pub fn store(&self) -> &MemoryStore {
        &self.store
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
        candidates: &mut [ScoredMemory],
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

        // Build document strings for the top N candidates, including details when available
        let documents: Vec<String> = candidates[..top_n]
            .iter()
            .map(|sm| {
                let mut doc = format!("{} {}", sm.memory.summary, sm.memory.content);
                if let Some(ref details) = sm.memory.details {
                    doc.push(' ');
                    doc.push_str(details);
                }
                doc
            })
            .collect();

        // Run cross-encoder in spawn_blocking since it's CPU-bound
        let query_owned = query_text.to_string();
        let rerank_results = tokio::task::spawn_blocking(move || {
            let mut reranker_guard = reranker
                .lock()
                .map_err(|e| anyhow::anyhow!("Failed to acquire reranker lock: {}", e))?;
            let doc_refs: Vec<&String> = documents.iter().collect();
            reranker_guard
                .rerank(&query_owned, doc_refs, false, None)
                .map_err(|e| anyhow::anyhow!("Reranking failed: {}", e))
        })
        .await
        .map_err(|e| anyhow::anyhow!("Rerank task panicked: {}", e))??;

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

                // NOTE: Only final_score and rerank are updated here.
                // Other ScoreBreakdown fields (semantic, relevance, trust, etc.)
                // retain their pre-rerank values for diagnostic transparency.
                candidates[idx].score = blended;
                candidates[idx].score_breakdown.final_score = blended;
                candidates[idx].score_breakdown.rerank = Some(raw_rerank);
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
    pub async fn query(&self, query: &RetrievalQuery) -> Result<RetrievalResult> {
        use crate::search::{keyword_search, normalize_keyword_score, query_token_count};

        // Step 0: Filter-mode requires at least one positive relevance input.
        if query.mode == RetrievalMode::Filter {
            let has_signal = query.query.as_ref().is_some_and(|s| !s.is_empty())
                || !query.logical.is_empty()
                || query.path.as_ref().is_some_and(|s| !s.is_empty())
                || query.tags.as_ref().is_some_and(|t| !t.is_empty());
            if !has_signal {
                return Err(crate::storage::StorageError::Validation(
                    "mode=filter requires at least one of: query, logical, path, tags".to_string(),
                ));
            }
        }

        // Step 1: Load lightweight index entries (6 columns).
        let all_entries = self.store.list_for_filtering().await?;

        // Step 2: Apply filters. Logical is NOT a filter — it only scores.
        let filters = SearchFilters {
            types: query.types.clone(),
            tags: query.tags.clone(),
            physical: query.path.clone(),
            min_criticality: query.min_criticality,
        };
        let filtered_entries = apply_index_filters(all_entries, &filters);

        // Step 2.5: Pre-filter expired entries at the index level.
        let include_expired = query
            .include_expired
            .unwrap_or(self.config.retrieval.include_expired);
        let filtered_entries = if include_expired {
            filtered_entries
        } else {
            let now = Utc::now();
            filtered_entries
                .into_iter()
                .filter(|e| e.expires_at.is_none_or(|exp| exp > now))
                .collect()
        };

        // Step 3: Batch-load surviving memories (single dir scan).
        let ids: Vec<&str> = filtered_entries.iter().map(|e| e.id.as_str()).collect();
        let loaded = self.store.get_batch(&ids).await?;
        let memory_map: HashMap<String, Memory> = loaded.into_iter().collect();

        // Step 4: If query text + embeddings available, get semantic scores.
        let semantic_scores_map: Option<HashMap<String, f64>> = if let Some(ref q) = query.query {
            if let Some(provider) = &self.embedding_provider {
                if let Ok(query_vector) = provider.embed(q).await {
                    let limit = query
                        .max_results
                        .unwrap_or(self.config.retrieval.max_results)
                        * 3;
                    self.store
                        .vector_search(query_vector, limit)
                        .await
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

        let mut scored_memories: Vec<ScoredMemory> = Vec::new();
        let query_path = query.path.as_deref();
        let now = Utc::now();

        for entry in filtered_entries.iter() {
            let memory = match memory_map.get(&entry.id) {
                Some(m) => m,
                None => continue,
            };

            // Build scoring context. Semantic wins when present for this
            // memory; keyword fills in otherwise; fall back to degraded when
            // query text is set but nothing matched; scope_only when no query.
            let sem_score = semantic_scores_map
                .as_ref()
                .and_then(|m| m.get(&memory.id))
                .copied();
            let kw_score = keyword_map.get(&memory.id).copied();

            let context = if let Some(ref q) = query.query {
                if let Some(s) = sem_score {
                    ScoringContext::with_semantic(query_path, &query.logical, q, s)
                } else if let Some(kw) = kw_score {
                    ScoringContext::with_keyword(query_path, &query.logical, q, kw, None)
                } else {
                    ScoringContext::with_query_degraded(query_path, &query.logical, q)
                }
            } else {
                ScoringContext::scope_only(query_path, &query.logical)
            };

            let breakdown = if include_expired {
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
            //   - scope proximity from user-supplied `path` or `logical`
            //     filters (these are scoring signals, not hard filters)
            if query.mode == RetrievalMode::Filter {
                let has_kw = kw_score.is_some_and(|v| v > 0.0);
                let has_tag = query.tags.as_ref().is_some_and(|filter_tags| {
                    !filter_tags.is_empty() && filter_tags.iter().any(|t| memory.tags.contains(t))
                });
                let has_user_scope =
                    (query.path.is_some() || !query.logical.is_empty()) && breakdown.scope > 0.0;
                if !(has_kw || has_tag || has_user_scope) {
                    continue;
                }
            }

            scored_memories.push(ScoredMemory {
                memory: memory.clone(),
                score: breakdown.final_score,
                score_breakdown: breakdown,
            });
        }

        // Step 6: Threshold. Rank mode uses retrieval.relevance_threshold;
        // Filter mode uses the stricter search.threshold when set, since the
        // flow is "find specific memories" rather than "browse context".
        let threshold = match query.mode {
            RetrievalMode::Rank => self.config.retrieval.relevance_threshold,
            RetrievalMode::Filter => self.config.search.threshold.min(1.0),
        };
        if threshold > 0.0 {
            scored_memories.retain(|sm| sm.score >= threshold);
        }

        // Step 7: Sort by score descending.
        scored_memories.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Step 8: Apply reranking if query text is present.
        if let Some(ref q) = query.query {
            if let Err(e) = self.apply_rerank(q, &mut scored_memories).await {
                tracing::warn!("Reranking failed, using original scores: {}", e);
            }
        }

        let total = scored_memories.len();

        // Step 9: Apply max_results limit.
        let max_results = query
            .max_results
            .unwrap_or(self.config.retrieval.max_results);
        scored_memories.truncate(max_results);

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

        Ok(RetrievalResult {
            memories: scored_memories,
            total,
            retrieval_quality: retrieval_quality.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use std::sync::LazyLock;

    /// Shared reranker across all tests in this module to avoid loading the
    /// ~100MB ONNX model once per test (which causes OOM when parallel).
    static SHARED_RERANKER: LazyLock<Option<Arc<Mutex<fastembed::TextRerank>>>> =
        LazyLock::new(|| {
            let cache_dir = crate::storage::paths::model_cache_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from(".cache/engramdb/models"));
            let options = fastembed::RerankInitOptions::default().with_cache_dir(cache_dir);
            fastembed::TextRerank::try_new(options)
                .ok()
                .map(|r| Arc::new(Mutex::new(r)))
        });

    fn try_reranker() -> Option<Arc<Mutex<fastembed::TextRerank>>> {
        SHARED_RERANKER.as_ref().cloned()
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
        let engine = RetrievalEngine::new(store, config).with_nli_provider(Box::new(DummyNli));
        assert!(
            !engine.nli_available(),
            "NLI should not be available when config.nli.enabled is false"
        );

        // Config enabled + provider attached → nli_available should be true
        let store2 = MemoryStore::open(temp_dir.path()).await.unwrap();
        let mut config2 = EngramConfig::default();
        config2.nli.enabled = true;
        let engine2 = RetrievalEngine::new(store2, config2).with_nli_provider(Box::new(DummyNli));
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
        let engine = RetrievalEngine::new(store, config).with_nli_provider(Box::new(PanicNli));

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
}
