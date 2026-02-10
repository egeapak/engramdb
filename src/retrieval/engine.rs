//! Retrieval engine for EngramDB

use crate::storage::{MemoryStore, Result};
use crate::types::{Memory, MemoryType, EngramConfig};
use crate::scoring::{composite_score, ScoringContext};
use crate::embeddings::EmbeddingProvider;
use crate::vector::{VectorStore, VectorMetadata};
use super::filters::{SearchFilters, apply_index_filters};
use chrono::Utc;
use std::collections::HashMap;

/// Detail level for retrieved memories
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailLevel {
    /// Just summaries from index (minimal data)
    Summary,
    /// Summary + content (default)
    Content,
    /// Everything including details field
    Full,
}

impl Default for DetailLevel {
    fn default() -> Self {
        DetailLevel::Content
    }
}

/// Query parameters for retrieval
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

/// A memory with its computed relevance score
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    pub memory: Memory,
    pub score: f64,
}

/// Result of a retrieval operation
#[derive(Debug, Clone)]
pub struct RetrievalResult {
    /// Retrieved memories with scores, sorted by score descending
    pub memories: Vec<ScoredMemory>,

    /// Total number of memories before limit was applied
    pub total: usize,
}

/// Main retrieval engine for EngramDB
pub struct RetrievalEngine {
    store: MemoryStore,
    config: EngramConfig,
    embedding_provider: Option<Box<dyn EmbeddingProvider>>,
    vector_store: Option<Box<dyn VectorStore>>,
}

impl RetrievalEngine {
    /// Create a new retrieval engine
    pub fn new(store: MemoryStore, config: EngramConfig) -> Self {
        Self {
            store,
            config,
            embedding_provider: None,
            vector_store: None,
        }
    }

    /// Add embedding support to the retrieval engine
    pub fn with_embeddings(
        mut self,
        provider: Box<dyn EmbeddingProvider>,
        vector_store: Box<dyn VectorStore>,
    ) -> Self {
        self.embedding_provider = Some(provider);
        self.vector_store = Some(vector_store);
        self
    }

    /// Check if embeddings are available
    pub fn embeddings_available(&self) -> bool {
        self.embedding_provider.is_some() && self.vector_store.is_some()
    }

    /// Embed and upsert a memory into the vector store
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

    /// Remove a memory from the vector store
    pub fn remove_from_vector_store(&self, id: &str) -> anyhow::Result<()> {
        if let Some(vs) = &self.vector_store {
            vs.delete(id)?;
        }
        Ok(())
    }

    /// Get a reference to the store
    pub fn store(&self) -> &MemoryStore {
        &self.store
    }

    /// Get a mutable reference to the store
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
                    let limit = query.max_results.unwrap_or(self.config.retrieval.max_results) * 3;
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

        // Step 3 & 4: Load memories and calculate scores
        let mut scored_memories: Vec<ScoredMemory> = filtered_entries
            .iter()
            .filter_map(|entry| {
                // Load full memory
                let memory = self.store.get(&entry.id).ok()?;

                // Skip expired memories unless include_expired is true
                let include_expired = query.include_expired.unwrap_or(self.config.retrieval.include_expired);
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
                    ScoringContext::scope_only(
                        query.path.clone(),
                        query.logical.clone(),
                    )
                };

                let score = composite_score(&memory, &context, &self.config, Utc::now());

                Some(ScoredMemory { memory, score })
            })
            .collect();

        // Step 5: Filter by relevance threshold
        let relevance_threshold = self.config.retrieval.relevance_threshold;
        scored_memories.retain(|sm| sm.score >= relevance_threshold);

        // Step 6: Sort by score descending
        scored_memories.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Track total before applying limit
        let total = scored_memories.len();

        // Step 7: Apply max_results limit
        let max_results = query.max_results.unwrap_or(self.config.retrieval.max_results);
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
        })
    }

    /// Perform keyword-based search
    ///
    /// # Algorithm
    /// 1. Load all memories
    /// 2. Apply filters
    /// 3. Run keyword search
    /// 4. Sort by keyword score descending
    /// 5. Return results
    pub fn search(&self, query_text: &str, filters: &SearchFilters) -> Result<Vec<ScoredMemory>> {
        // Step 1: Load all index entries
        let all_entries = self.store.list()?;

        // Step 2: Apply filters
        let filtered_entries = apply_index_filters(all_entries, filters);

        // Load full memories
        let memories: Vec<Memory> = filtered_entries
            .iter()
            .filter_map(|entry| self.store.get(&entry.id).ok())
            .collect();

        // Step 3: Run keyword search
        let keyword_results = crate::search::keyword_search(query_text, &memories);

        // Step 3.5: If embeddings available, combine with semantic results
        let scored_memories: Vec<ScoredMemory> = if let (Some(provider), Some(vs)) =
            (&self.embedding_provider, &self.vector_store)
        {
            // Get semantic scores
            let semantic_scores: HashMap<String, f64> =
                if let Ok(query_vector) = provider.embed(query_text) {
                    vs.search(query_vector, memories.len())
                        .unwrap_or_default()
                        .into_iter()
                        .map(|m| (m.id, m.score))
                        .collect()
                } else {
                    HashMap::new()
                };

            // Combine keyword + semantic scores
            let mut combined: HashMap<usize, f64> = HashMap::new();
            for (idx, kw_score) in &keyword_results {
                combined.insert(*idx, *kw_score * 0.5);
            }
            for (idx, memory) in memories.iter().enumerate() {
                if let Some(&sem_score) = semantic_scores.get(&memory.id) {
                    *combined.entry(idx).or_insert(0.0) += sem_score * 0.5;
                }
            }

            let mut results: Vec<ScoredMemory> = combined
                .into_iter()
                .map(|(idx, score)| ScoredMemory {
                    memory: memories[idx].clone(),
                    score,
                })
                .collect();
            results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
            results
        } else {
            // Keyword-only
            keyword_results
                .into_iter()
                .map(|(idx, score)| ScoredMemory {
                    memory: memories[idx].clone(),
                    score,
                })
                .collect()
        };

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

    // Integration tests would go here, testing against a real MemoryStore
    // with test data. These would require more complex setup.
}
