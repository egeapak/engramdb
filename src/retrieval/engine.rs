//! Core retrieval engine implementation.
//!
//! The retrieval engine orchestrates the process of finding relevant memories based on
//! contextual queries. It combines multiple retrieval strategies including scope matching,
//! semantic search (when embeddings are available), and keyword search.

use super::filters::{apply_index_filters, SearchFilters};
use crate::embeddings::EmbeddingProvider;
use crate::scoring::{composite_score, ScoreBreakdown, ScoringContext};
use crate::storage::{MemoryStore, Result};
use crate::types::{EngramConfig, Memory, MemoryType};
use crate::vector::{VectorMetadata, VectorStore};
use chrono::Utc;
use std::collections::HashMap;

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
/// and relevance scoring. The engine can operate with or without embeddings, gracefully
/// degrading to keyword-only search when semantic search is unavailable.
pub struct RetrievalEngine {
    store: MemoryStore,
    config: EngramConfig,
    embedding_provider: Option<Box<dyn EmbeddingProvider>>,
    vector_store: Option<Box<dyn VectorStore>>,
}

impl RetrievalEngine {
    /// Create a new retrieval engine.
    ///
    /// Embeddings are not enabled by default. Use [`with_embeddings`](Self::with_embeddings)
    /// to add semantic search capabilities.
    pub fn new(store: MemoryStore, config: EngramConfig) -> Self {
        Self {
            store,
            config,
            embedding_provider: None,
            vector_store: None,
        }
    }

    /// Add embedding support to the retrieval engine.
    ///
    /// Enables semantic search capabilities. Returns self for method chaining.
    pub fn with_embeddings(
        mut self,
        provider: Box<dyn EmbeddingProvider>,
        vector_store: Box<dyn VectorStore>,
    ) -> Self {
        self.embedding_provider = Some(provider);
        self.vector_store = Some(vector_store);
        self
    }

    /// Check if embeddings are available.
    ///
    /// Returns true if both an embedding provider and vector store are configured.
    pub fn embeddings_available(&self) -> bool {
        self.embedding_provider.is_some() && self.vector_store.is_some()
    }

    /// Embed and upsert a memory into the vector store.
    ///
    /// Generates an embedding for the memory's content and stores it in the vector database
    /// for future semantic search. Does nothing if embeddings are not available.
    ///
    /// # Arguments
    /// * `memory` - The memory to embed
    pub fn embed_memory(&self, memory: &Memory) -> anyhow::Result<()> {
        if let (Some(provider), Some(vs)) = (&self.embedding_provider, &self.vector_store) {
            let text = format!("{} {}", memory.summary, memory.content);
            let vector = provider.embed(&text)?;
            let metadata = VectorMetadata {
                type_: format!("{:?}", memory.type_).to_lowercase(),
                criticality: memory.criticality,
                physical: memory.physical.clone(),
                logical: memory.logical.clone(),
                tags: memory.tags.clone(),
            };
            vs.upsert(&memory.id, vector, metadata)?;
        }
        Ok(())
    }

    /// Remove a memory from the vector store.
    ///
    /// Deletes the embedding associated with this memory ID. Does nothing if
    /// embeddings are not available.
    ///
    /// # Arguments
    /// * `id` - The memory ID to remove
    pub fn remove_from_vector_store(&self, id: &str) -> anyhow::Result<()> {
        if let Some(vs) = &self.vector_store {
            vs.delete(id)?;
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
    pub fn retrieve(&self, query: &RetrievalQuery) -> Result<RetrievalResult> {
        // Step 1: Load all index entries
        let all_entries = self.store.list()?;

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
            if let (Some(provider), Some(vs)) = (&self.embedding_provider, &self.vector_store) {
                if let Ok(query_vector) = provider.embed(q) {
                    let limit = query
                        .max_results
                        .unwrap_or(self.config.retrieval.max_results)
                        * 3;
                    if let Ok(matches) = vs.search(query_vector, limit) {
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
        let mut scored_memories: Vec<ScoredMemory> = filtered_entries
            .iter()
            .filter_map(|entry| {
                // Load full memory
                let memory = self.store.get(&entry.id).ok()?;

                // Skip expired memories unless include_expired is true
                let include_expired = query
                    .include_expired
                    .unwrap_or(self.config.retrieval.include_expired);
                if memory.is_expired() && !include_expired {
                    return None;
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

                Some(ScoredMemory {
                    memory,
                    score: breakdown.final_score,
                    score_breakdown: breakdown,
                })
            })
            .collect();

        // Step 5: Filter by relevance threshold
        let relevance_threshold = self.config.retrieval.relevance_threshold;
        scored_memories.retain(|sm| sm.score >= relevance_threshold);

        // Step 6: Sort by score descending
        scored_memories.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

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
    pub fn search(&self, query_text: &str, filters: &SearchFilters) -> Result<Vec<ScoredMemory>> {
        use crate::scoring::{decay_factor, trust_weight_from_config};
        use crate::types::Status;

        let now = Utc::now();
        let semantic_weight = self.config.search.semantic_weight;
        let threshold = self.config.search.threshold;

        // Step 1: Load all index entries
        let all_entries = self.store.list()?;

        // Step 2: Apply filters
        let filtered_entries = apply_index_filters(all_entries, filters);

        // Load full memories
        let memories: Vec<Memory> = filtered_entries
            .iter()
            .filter_map(|entry| self.store.get(&entry.id).ok())
            .collect();

        // Step 3: Run keyword search (raw unbounded scores)
        let keyword_results = crate::search::keyword_search(query_text, &memories);

        // Step 4: Get semantic scores if embeddings available
        let semantic_scores: HashMap<String, f64> =
            if let (Some(provider), Some(vs)) = (&self.embedding_provider, &self.vector_store) {
                if let Ok(query_vector) = provider.embed(query_text) {
                    vs.search(query_vector, memories.len())
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

        Ok(scored_memories)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_engine_new() {
        use crate::types::EngramConfig;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();
        let config = EngramConfig::default();

        let engine = RetrievalEngine::new(store, config);
        assert!(!engine.embeddings_available());
    }

    #[test]
    fn test_retrieve_empty_store() {
        use crate::types::EngramConfig;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();
        let config = EngramConfig::default();

        let engine = RetrievalEngine::new(store, config);
        let query = RetrievalQuery::default();
        let result = engine.retrieve(&query).unwrap();

        assert_eq!(result.memories.len(), 0);
        assert_eq!(result.total, 0);
    }

    #[test]
    fn test_retrieve_returns_scored_memories() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

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

        store.create(&memory1).unwrap();
        store.create(&memory2).unwrap();

        let engine = RetrievalEngine::new(store, config);

        // Retrieve with path matching memory1 more closely
        let query = RetrievalQuery {
            path: Some("src/auth/handlers.rs".to_string()),
            ..Default::default()
        };

        let result = engine.retrieve(&query).unwrap();

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

    #[test]
    fn test_retrieve_filters_by_type() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

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

        store.create(&decision).unwrap();
        store.create(&context).unwrap();

        let engine = RetrievalEngine::new(store, config);

        // Filter by Decision type only
        let query = RetrievalQuery {
            types: Some(vec![MemoryType::Decision]),
            ..Default::default()
        };

        let result = engine.retrieve(&query).unwrap();

        assert_eq!(result.memories.len(), 1);
        assert_eq!(result.memories[0].memory.type_, MemoryType::Decision);
    }

    #[test]
    fn test_retrieve_respects_max_results() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

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
            store.create(&memory).unwrap();
        }

        let engine = RetrievalEngine::new(store, config);

        // Retrieve with max_results=2
        let query = RetrievalQuery {
            max_results: Some(2),
            ..Default::default()
        };

        let result = engine.retrieve(&query).unwrap();

        assert_eq!(result.memories.len(), 2);
        assert_eq!(result.total, 5);
    }

    #[test]
    fn test_retrieve_excludes_expired() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use chrono::{Duration, Utc};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

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

        store.create(&expired).unwrap();
        store.create(&active).unwrap();

        let engine = RetrievalEngine::new(store, config);

        // Retrieve with include_expired=false
        let query = RetrievalQuery {
            include_expired: Some(false),
            ..Default::default()
        };

        let result = engine.retrieve(&query).unwrap();

        assert_eq!(result.memories.len(), 1);
        assert_eq!(result.memories[0].memory.summary, "Active memory");
    }

    #[test]
    fn test_retrieve_detail_level_summary() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

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

        store.create(&memory).unwrap();

        let engine = RetrievalEngine::new(store, config);

        // Retrieve with detail_level=Summary
        let query = RetrievalQuery {
            detail_level: DetailLevel::Summary,
            ..Default::default()
        };

        let result = engine.retrieve(&query).unwrap();

        assert_eq!(result.memories.len(), 1);
        assert_eq!(result.memories[0].memory.summary, "Test memory");
        assert_eq!(result.memories[0].memory.content, "");
        assert_eq!(result.memories[0].memory.details, None);
    }

    #[test]
    fn test_retrieve_score_breakdown() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

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

        store.create(&memory).unwrap();

        let engine = RetrievalEngine::new(store, config);

        let query = RetrievalQuery {
            path: Some("src/test.rs".to_string()),
            ..Default::default()
        };

        let result = engine.retrieve(&query).unwrap();

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

    #[test]
    fn test_retrieve_query_mode_scope_only() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

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

        store.create(&memory).unwrap();

        let engine = RetrievalEngine::new(store, config);

        let query = RetrievalQuery::default();

        let result = engine.retrieve(&query).unwrap();

        assert_eq!(result.query_mode, "scope_only");
    }

    #[test]
    fn test_retrieve_query_mode_degraded() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();

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

        store.create(&memory).unwrap();

        let engine = RetrievalEngine::new(store, config);

        let query = RetrievalQuery {
            query: Some("test".to_string()),
            ..Default::default()
        };

        let result = engine.retrieve(&query).unwrap();

        // Without embeddings configured, should be degraded mode
        assert_eq!(result.query_mode, "degraded");
    }

    #[test]
    fn test_search_keyword_integration() {
        use crate::types::{EngramConfig, Memory, MemoryType, Provenance, Visibility};
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let store = MemoryStore::init(temp_dir.path()).unwrap();
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

        store.create(&memory1).unwrap();
        store.create(&memory2).unwrap();

        let engine = RetrievalEngine::new(store, config);

        // Search for "authentication"
        let filters = SearchFilters::default();
        let results = engine.search("authentication", &filters).unwrap();

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
}
