//! Core retrieval engine implementation.
//!
//! The retrieval engine orchestrates the process of finding relevant memories based on
//! contextual queries. It combines multiple retrieval strategies including scope matching,
//! semantic search (when embeddings are available), and keyword search.

use super::filters::{apply_index_filters, SearchFilters};
use crate::embeddings::EmbeddingProvider;
use crate::nli::{NliProvider, NliResult};
use crate::scoring::{composite_score, ScoreBreakdown, ScoringContext};
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

/// Query parameters for retrieval.
#[derive(Debug, Clone, Default)]
pub struct RetrievalQuery {
    /// Physical scope - current file path
    pub path: Option<String>,

    /// Logical scopes - dot-notation domains
    pub logical: Vec<String>,

    /// Search query text (for semantic similarity)
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

    /// Query mode used: "with_embeddings", "degraded", or "scope_only"
    pub query_mode: String,
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

        let nli_pairs: Vec<(String, String)> = candidate_memories
            .iter()
            .map(|m| (memory.summary.clone(), m.summary.clone()))
            .collect();

        if nli_pairs.is_empty() {
            return Ok(vec![]);
        }

        // Build refs for batch classification
        let pair_refs: Vec<(&str, &str)> = nli_pairs
            .iter()
            .map(|(p, h)| (p.as_str(), h.as_str()))
            .collect();

        let results = nli.classify_batch(&pair_refs).await?;

        // Filter by contradiction threshold
        let mut contradictions = Vec::new();
        for (mem, result) in candidate_memories.iter().zip(results.into_iter()) {
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

        // Find min/max for normalization
        let scores: Vec<f64> = rerank_results.iter().map(|r| r.score as f64).collect();
        let min_score = scores.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_score = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let score_range = max_score - min_score;

        // Map rerank results back to candidates by original index
        for result in &rerank_results {
            let idx = result.index;
            if idx < top_n {
                let raw_rerank = result.score as f64;

                // Normalize to [0, 1]
                let normalized = if score_range > f64::EPSILON {
                    (raw_rerank - min_score) / score_range
                } else {
                    1.0 // All scores equal — treat as maximum
                };

                // Blend: (1 - weight) * original + weight * normalized_rerank
                let original = candidates[idx].score;
                let blended = (1.0 - weight) * original + weight * normalized;

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

    /// Retrieve memories based on a query
    ///
    /// # Algorithm
    /// 1. Load all index entries from store
    /// 2. Apply filters (type, tags, min_criticality, expired status)
    /// 3. For each remaining entry, load the full memory
    /// 4. Calculate composite score
    /// 5. Filter memories below relevance_threshold (unless include_expired overrides)
    /// 6. Sort by score descending
    /// 7. Take top max_results
    /// 8. Strip details based on detail_level
    /// 9. Return RetrievalResult with total count before limit
    pub async fn retrieve(&self, query: &RetrievalQuery) -> Result<RetrievalResult> {
        // Step 1: Load lightweight index entries (6 columns)
        let all_entries = self.store.list_for_filtering().await?;

        // Step 2: Apply filters
        let filters = SearchFilters {
            types: query.types.clone(),
            tags: query.tags.clone(),
            physical: query.path.clone(),
            logical: query.logical.first().cloned(), // Use first logical scope for filtering
            min_criticality: query.min_criticality,
        };

        let filtered_entries = apply_index_filters(all_entries, &filters);

        // Step 2.5: If query text provided and embeddings available, get semantic scores
        let semantic_scores_map: Option<HashMap<String, f64>> = if let Some(ref q) = query.query {
            if let Some(provider) = &self.embedding_provider {
                if let Ok(query_vector) = provider.embed(q).await {
                    let limit = query
                        .max_results
                        .unwrap_or(self.config.retrieval.max_results)
                        * 3;
                    if let Ok(matches) = self.store.vector_search(query_vector, limit).await {
                        Some(matches.into_iter().map(|m| (m.id, m.score)).collect())
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // Determine query mode for the entire retrieval
        let query_mode = if query.query.is_some() {
            if semantic_scores_map.is_some() {
                "with_embeddings"
            } else {
                "degraded"
            }
        } else {
            "scope_only"
        };

        // Step 3 & 4: Load memories and calculate scores
        let mut scored_memories: Vec<ScoredMemory> = Vec::new();
        for entry in filtered_entries.iter() {
            // Load full memory
            let memory = match self.store.get(&entry.id).await {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Skip expired memories unless include_expired is true
            let include_expired = query
                .include_expired
                .unwrap_or(self.config.retrieval.include_expired);
            if memory.is_expired() && !include_expired {
                continue;
            }

            // Calculate composite score
            let context = if let Some(ref q) = query.query {
                if let Some(ref semantic_scores) = semantic_scores_map {
                    if let Some(&sem_score) = semantic_scores.get(&memory.id) {
                        // Full mode: query + embeddings
                        ScoringContext::with_semantic(
                            query.path.clone(),
                            query.logical.clone(),
                            q.clone(),
                            sem_score,
                        )
                    } else {
                        // Memory not in vector results, degraded mode
                        ScoringContext::with_query_degraded(
                            query.path.clone(),
                            query.logical.clone(),
                            q.clone(),
                        )
                    }
                } else {
                    // No embeddings available, degraded mode
                    ScoringContext::with_query_degraded(
                        query.path.clone(),
                        query.logical.clone(),
                        q.clone(),
                    )
                }
            } else {
                // Scope-only retrieval
                ScoringContext::scope_only(query.path.clone(), query.logical.clone())
            };

            let breakdown = composite_score(&memory, &context, &self.config, Utc::now());

            scored_memories.push(ScoredMemory {
                memory,
                score: breakdown.final_score,
                score_breakdown: breakdown,
            });
        }

        // Step 5: Filter by relevance threshold
        let relevance_threshold = self.config.retrieval.relevance_threshold;
        scored_memories.retain(|sm| sm.score >= relevance_threshold);

        // Step 6: Sort by score descending
        scored_memories.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Step 6.5: Apply reranking if configured and query is present
        if let Some(ref q) = query.query {
            if let Err(e) = self.apply_rerank(q, &mut scored_memories).await {
                eprintln!("Warning: reranking failed, using original scores: {}", e);
            }
        }

        // Track total before applying limit
        let total = scored_memories.len();

        // Step 7: Apply max_results limit
        let max_results = query
            .max_results
            .unwrap_or(self.config.retrieval.max_results);
        scored_memories.truncate(max_results);

        // Step 8: Strip details based on detail_level
        for sm in &mut scored_memories {
            match query.detail_level {
                DetailLevel::Summary => {
                    // Keep only summary, clear content and details
                    sm.memory.content = String::new();
                    sm.memory.details = None;
                }
                DetailLevel::Content => {
                    // Keep summary and content, clear details
                    sm.memory.details = None;
                }
                DetailLevel::Full => {
                    // Keep everything
                }
            }
        }

        Ok(RetrievalResult {
            memories: scored_memories,
            total,
            query_mode: query_mode.to_string(),
        })
    }

    /// Perform keyword-based search with quality signals.
    ///
    /// # Scoring formula
    /// ```text
    /// content_score = keyword_raw + cosine_similarity * semantic_weight
    /// score = content_score * decay_factor * trust_weight
    /// if challenged: score *= 0.7
    /// ```
    ///
    /// # Algorithm
    /// 1. Load all memories
    /// 2. Apply filters
    /// 3. Run keyword search
    /// 4. If embeddings available, get semantic scores
    /// 5. Combine: content = keyword_raw + semantic * semantic_weight
    /// 6. Apply decay, trust, challenge penalty
    /// 7. Apply threshold, sort, return
    pub async fn search(
        &self,
        query_text: &str,
        filters: &SearchFilters,
    ) -> Result<Vec<ScoredMemory>> {
        use crate::scoring::{decay_factor, trust_weight_from_config};
        use crate::types::Status;

        let now = Utc::now();
        let semantic_weight = self.config.search.semantic_weight;
        let threshold = self.config.search.threshold;

        // Step 1: Load lightweight index entries (6 columns)
        let all_entries = self.store.list_for_filtering().await?;

        // Step 2: Apply filters
        let filtered_entries = apply_index_filters(all_entries, filters);

        // Load full memories
        let mut memories: Vec<Memory> = Vec::new();
        for entry in filtered_entries.iter() {
            if let Ok(memory) = self.store.get(&entry.id).await {
                memories.push(memory);
            }
        }

        // Step 3: Run keyword search (raw unbounded scores)
        let keyword_results = crate::search::keyword_search(query_text, &memories);

        // Step 4: Get semantic scores if embeddings available
        let semantic_scores: HashMap<String, f64> = if let Some(provider) = &self.embedding_provider
        {
            if let Ok(query_vector) = provider.embed(query_text).await {
                self.store
                    .vector_search(query_vector, memories.len())
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|m| (m.id, m.score))
                    .collect()
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        };

        // Step 5: Build a map of keyword scores by index
        let keyword_map: HashMap<usize, f64> = keyword_results.into_iter().collect();

        // Step 6: Score every memory that has either keyword or semantic match
        let mut scored_memories: Vec<ScoredMemory> = Vec::new();

        for (idx, memory) in memories.iter().enumerate() {
            let kw_score = keyword_map.get(&idx).copied().unwrap_or(0.0);
            let sem_score = semantic_scores.get(&memory.id).copied().unwrap_or(0.0);

            // Skip memories with no matches at all
            if kw_score == 0.0 && sem_score == 0.0 {
                continue;
            }

            // content = keyword_raw + semantic * semantic_weight
            let content_score = kw_score + sem_score * semantic_weight;

            // Quality signals
            let decay = decay_factor(memory.created_at, now, &memory.decay);
            let trust =
                trust_weight_from_config(memory.provenance.source, &self.config.trust_weights);

            // score = content * decay * trust
            let mut score = content_score * decay * trust;

            // Challenge penalty
            if memory.status == Status::Challenged {
                score *= 0.7;
            }

            // Compute informational relevance (criticality * decay)
            let relevance = memory.criticality * decay;

            let breakdown = ScoreBreakdown {
                final_score: score,
                semantic: if sem_score > 0.0 {
                    Some(sem_score)
                } else {
                    None
                },
                keyword: if kw_score > 0.0 { Some(kw_score) } else { None },
                rerank: None,
                relevance,
                scope: 0.0,
                trust,
                decay,
                criticality: memory.criticality,
            };

            scored_memories.push(ScoredMemory {
                memory: memory.clone(),
                score,
                score_breakdown: breakdown,
            });
        }

        // Step 7: Apply threshold
        if threshold > 0.0 {
            scored_memories.retain(|sm| sm.score >= threshold);
        }

        // Sort by score descending
        scored_memories.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Apply reranking if configured
        if let Err(e) = self.apply_rerank(query_text, &mut scored_memories).await {
            eprintln!("Warning: reranking failed, using original scores: {}", e);
        }

        Ok(scored_memories)
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
            let cache_dir = dirs::cache_dir()
                .unwrap_or_else(|| std::path::PathBuf::from(".cache"))
                .join("engramdb")
                .join("models");
            let options = fastembed::RerankInitOptions::default().with_cache_dir(cache_dir);
            fastembed::TextRerank::try_new(options)
                .ok()
                .map(|r| Arc::new(Mutex::new(r)))
        });

    fn try_reranker() -> Option<Arc<Mutex<fastembed::TextRerank>>> {
        SHARED_RERANKER.as_ref().cloned()
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
        let result = engine.retrieve(&query).await.unwrap();

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

        let result = engine.retrieve(&query).await.unwrap();

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

        let result = engine.retrieve(&query).await.unwrap();

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

        let result = engine.retrieve(&query).await.unwrap();

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

        let result = engine.retrieve(&query).await.unwrap();

        assert_eq!(result.memories.len(), 1);
        assert_eq!(result.memories[0].memory.summary, "Active memory");
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

        let result = engine.retrieve(&query).await.unwrap();

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

        let result = engine.retrieve(&query).await.unwrap();

        assert_eq!(result.memories.len(), 1);
        // Verify score_breakdown is populated
        let sm = &result.memories[0];
        assert!(sm.score_breakdown.final_score > 0.0);
        assert_eq!(sm.score, sm.score_breakdown.final_score);
        assert!(sm.score_breakdown.relevance > 0.0);
        assert!(sm.score_breakdown.scope > 0.0);
        assert!(sm.score_breakdown.trust > 0.0);
        assert_eq!(sm.score_breakdown.decay, 1.0); // No decay
        assert!(sm.score_breakdown.semantic.is_none()); // No query
    }

    #[tokio::test]
    async fn test_retrieve_query_mode_scope_only() {
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

        let result = engine.retrieve(&query).await.unwrap();

        assert_eq!(result.query_mode, "scope_only");
    }

    #[tokio::test]
    async fn test_retrieve_query_mode_degraded() {
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

        let result = engine.retrieve(&query).await.unwrap();

        // Without embeddings configured, should be degraded mode
        assert_eq!(result.query_mode, "degraded");
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
        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).await.unwrap();

        // Should find memory1 with higher score
        assert!(!results.is_empty());
        assert_eq!(results[0].memory.summary, "Authentication system");
        // Raw keyword score: summary(3) + content(1) = 4.0, * decay(1.0) * trust(1.0) = 4.0
        assert!(results[0].score > 0.0);
        // Should have keyword breakdown
        assert!(results[0].score_breakdown.keyword.is_some());
        // Trust should be the real trust weight
        assert_eq!(results[0].score_breakdown.trust, 1.0);
        // Decay should be real
        assert_eq!(results[0].score_breakdown.decay, 1.0);
    }

    #[tokio::test]
    async fn test_search_applies_trust_multiplier() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let config = EngramConfig::default();

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
        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).await.unwrap();

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
        // Inferred should score exactly 0.6x of human
        assert!(
            (inferred_result.score - human_result.score * 0.6).abs() < 0.001,
            "inferred {} should be {} (human * 0.6)",
            inferred_result.score,
            human_result.score * 0.6
        );
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
        let config = EngramConfig::default();

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
        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).await.unwrap();

        assert_eq!(results.len(), 2);
        let fresh_result = results.iter().find(|r| r.memory.id == fresh.id).unwrap();
        let old_result = results.iter().find(|r| r.memory.id == old.id).unwrap();

        // Fresh has decay=1.0, old has decay~=0.5
        assert_eq!(fresh_result.score_breakdown.decay, 1.0);
        assert!((old_result.score_breakdown.decay - 0.5).abs() < 0.1);
        // Old should score roughly half of fresh
        assert!(old_result.score < fresh_result.score);
        assert!(
            (old_result.score - fresh_result.score * old_result.score_breakdown.decay).abs() < 0.01
        );
    }

    #[tokio::test]
    async fn test_search_applies_challenge_penalty() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Status, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let config = EngramConfig::default();

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
        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).await.unwrap();

        assert_eq!(results.len(), 2);
        let active_result = results.iter().find(|r| r.memory.id == active.id).unwrap();
        let challenged_result = results
            .iter()
            .find(|r| r.memory.id == challenged.id)
            .unwrap();

        // Challenged should be exactly 0.7x of active
        assert!(
            (challenged_result.score - active_result.score * 0.7).abs() < 0.001,
            "challenged {} should be {} (active * 0.7)",
            challenged_result.score,
            active_result.score * 0.7
        );
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
        // Set a high threshold that filters out low-scoring results
        config.search.threshold = 100.0;

        let mut memory = Memory::new(
            MemoryType::Decision,
            "Authentication system",
            "Details about authentication",
            Provenance::human(),
        );
        memory.visibility = Visibility::Shared;
        store.create(&memory).await.unwrap();

        let engine = RetrievalEngine::new(store, config);
        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).await.unwrap();

        // Keyword score is at most ~4.0, well below threshold of 100
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
        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).await.unwrap();

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
        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).await.unwrap();

        assert_eq!(results.len(), 1);
        let bd = &results[0].score_breakdown;
        // keyword should be populated with raw score
        assert!(bd.keyword.is_some());
        // "authentication" matches in summary(3) + content(1) = 4.0
        assert_eq!(bd.keyword.unwrap(), 4.0);
        // criticality should be the raw value
        assert_eq!(bd.criticality, 0.7);
        // relevance should be criticality * decay
        assert_eq!(bd.relevance, 0.7 * bd.decay);
        // scope should be 0.0 for search
        assert_eq!(bd.scope, 0.0);
        // semantic should be None (no embeddings)
        assert!(bd.semantic.is_none());
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
        let store2 = MemoryStore::open(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
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
        let result = engine.retrieve(&query).await.unwrap();

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
        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).await.unwrap();

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

        let result = engine.retrieve(&query).await.unwrap();

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
            MemoryStore::open(temp_dir.path(), &InMemoryRegistry::new())
                .await
                .unwrap(),
            config.clone(),
        );

        let query = RetrievalQuery {
            query: Some("architecture decision".to_string()),
            ..Default::default()
        };

        let result_no_rerank = engine_no_rerank.retrieve(&query).await.unwrap();

        // Now with reranker but weight=0.0
        let engine_rerank = RetrievalEngine::new(
            MemoryStore::open(temp_dir.path(), &InMemoryRegistry::new())
                .await
                .unwrap(),
            config,
        )
        .with_reranker(reranker);

        let result_rerank = engine_rerank.retrieve(&query).await.unwrap();

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

        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).await.unwrap();

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
        let result_no_rerank = engine_no_rerank.retrieve(&query).await.unwrap();

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

        let store2 = MemoryStore::open(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let engine_rerank = RetrievalEngine::new(store2, config).with_reranker(reranker);

        let result_rerank = engine_rerank.retrieve(&query).await.unwrap();

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

        let filters = SearchFilters::default();
        let engine_no_rerank = RetrievalEngine::new(store, config.clone());
        let results_no_rerank = engine_no_rerank
            .search("database migration strategy", &filters)
            .await
            .unwrap();

        assert_eq!(results_no_rerank.len(), 2);
        // Without reranking: keyword-heavy doc ranks first (4 pts vs 1 pt)
        assert_eq!(
            results_no_rerank[0].memory.id, keyword_heavy_id,
            "Without reranking, keyword-heavy memory should rank first"
        );
        assert_eq!(results_no_rerank[1].memory.id, semantically_relevant_id);

        // ── Step 2: search WITH reranker (weight = 0.8) ───────────────────
        config.rerank.enabled = true;
        config.rerank.weight = 0.8;
        config.rerank.top_n = 10;

        let store2 = MemoryStore::open(temp_dir.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        let engine_rerank = RetrievalEngine::new(store2, config).with_reranker(reranker);

        let results_rerank = engine_rerank
            .search("database migration strategy", &filters)
            .await
            .unwrap();

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
