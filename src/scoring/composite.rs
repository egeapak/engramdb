use chrono::{DateTime, Utc};

use crate::types::{EngramConfig, Memory, Status};

use super::decay::effective_relevance;
use super::trust::trust_weight_from_config;

/// Context for scoring a memory during retrieval.
#[derive(Debug, Clone)]
pub struct ScoringContext {
    /// Current file path (if any)
    pub path: Option<String>,

    /// Current logical scope tags
    pub logical: Vec<String>,

    /// Search query (if any)
    pub query: Option<String>,

    /// Semantic similarity score from vector search (if available)
    pub semantic_score: Option<f64>,

    /// Whether embeddings are available for this retrieval
    pub embeddings_available: bool,
}

impl ScoringContext {
    /// Create a new ScoringContext for scope-only retrieval
    pub fn scope_only(path: Option<String>, logical: Vec<String>) -> Self {
        Self {
            path,
            logical,
            query: None,
            semantic_score: None,
            embeddings_available: false,
        }
    }

    /// Create a new ScoringContext with a query (degraded mode, no embeddings)
    pub fn with_query_degraded(
        path: Option<String>,
        logical: Vec<String>,
        query: String,
    ) -> Self {
        Self {
            path,
            logical,
            query: Some(query),
            semantic_score: None,
            embeddings_available: false,
        }
    }

    /// Create a new ScoringContext with a query and semantic score (full mode)
    pub fn with_semantic(
        path: Option<String>,
        logical: Vec<String>,
        query: String,
        semantic_score: f64,
    ) -> Self {
        Self {
            path,
            logical,
            query: Some(query),
            semantic_score: Some(semantic_score),
            embeddings_available: true,
        }
    }
}

/// Calculate the composite score for a memory in a given context.
///
/// The scoring operates in three modes:
///
/// 1. **With query + embeddings** (semantic_score is Some):
///    - Uses `config.retrieval.scoring.with_query` weights
///    - score = semantic * semantic_score + relevance * relevance + scope * scope_proximity + trust * trust
///
/// 2. **With query, no embeddings** (query is Some, semantic_score is None):
///    - Uses `config.retrieval.scoring.degraded` weights
///    - score = relevance * relevance + scope * scope_proximity + trust * trust
///
/// 3. **Scope-only** (no query):
///    - Uses `config.retrieval.scoring.scope_only` weights
///    - score = relevance * relevance + scope * scope_proximity + trust * trust
///
/// Challenge penalty: if memory.status == Status::Challenged, multiply final score by (1.0 - challenge_penalty)
///
/// # Arguments
/// * `memory` - The memory to score
/// * `context` - Scoring context (current scope, query, semantic score)
/// * `config` - EngramDB configuration
/// * `now` - Current timestamp
///
/// # Returns
/// Composite score from 0.0 to 1.0+
pub fn composite_score(
    memory: &Memory,
    context: &ScoringContext,
    config: &EngramConfig,
    now: DateTime<Utc>,
) -> f64 {
    // Calculate component scores
    let relevance = effective_relevance(memory, now);

    let scope_score = crate::scope::scope_proximity(
        &memory.physical,
        &memory.logical,
        context.path.as_deref(),
        &context.logical,
    );

    let trust = trust_weight_from_config(memory.provenance.source, &config.trust_weights);

    // Determine which weights to use based on context
    let weights = if context.semantic_score.is_some() {
        // Mode 1: With query + embeddings
        &config.retrieval.scoring.with_query
    } else if context.query.is_some() {
        // Mode 2: With query, no embeddings (degraded)
        &config.retrieval.scoring.degraded
    } else {
        // Mode 3: Scope-only
        &config.retrieval.scoring.scope_only
    };

    // Calculate base score
    let mut score = weights.relevance * relevance
        + weights.scope * scope_score
        + weights.trust * trust;

    // Add semantic component if available
    if let (Some(semantic_weight), Some(semantic_score)) =
        (weights.semantic, context.semantic_score)
    {
        score += semantic_weight * semantic_score;
    }

    // Apply challenge penalty if memory is challenged
    if memory.status == Status::Challenged {
        // Default challenge penalty is 0.3
        let challenge_penalty = 0.3;
        score *= 1.0 - challenge_penalty;
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Decay, MemoryType, Provenance, Visibility};
    use chrono::Duration;

    fn create_test_memory() -> Memory {
        Memory {
            id: "test-id".to_string(),
            type_: MemoryType::Decision,
            summary: "Test memory".to_string(),
            content: "Test content".to_string(),
            details: None,
            physical: vec!["src/api/auth.rs".to_string()],
            logical: vec!["auth.oauth".to_string()],
            tags: vec![],
            criticality: 0.8,
            decay: Some(Decay::none()),
            provenance: Provenance::human(),
            confidence: 0.9,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            accessed_at: Utc::now(),
            expires_at: None,
        }
    }

    #[test]
    fn test_composite_score_with_semantic() {
        let memory = create_test_memory();
        let context = ScoringContext::with_semantic(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "oauth authentication".to_string(),
            0.9,
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let score = composite_score(&memory, &context, &config, now);

        // Should be > 0 and use with_query weights
        assert!(score > 0.0);
        // Semantic should contribute: 0.5 * 0.9 = 0.45
        // Relevance: 0.3 * 0.8 = 0.24
        // Scope: 0.15 * 1.0 = 0.15 (exact match)
        // Trust: 0.05 * 1.0 = 0.05
        // Total: ~0.89
        assert!((score - 0.89).abs() < 0.1);
    }

    #[test]
    fn test_composite_score_degraded() {
        let memory = create_test_memory();
        let context = ScoringContext::with_query_degraded(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "oauth authentication".to_string(),
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let score = composite_score(&memory, &context, &config, now);

        // Should be > 0 and use degraded weights
        assert!(score > 0.0);
        // No semantic component
        // Relevance: 0.6 * 0.8 = 0.48
        // Scope: 0.3 * 1.0 = 0.30
        // Trust: 0.1 * 1.0 = 0.10
        // Total: ~0.88
        assert!((score - 0.88).abs() < 0.1);
    }

    #[test]
    fn test_composite_score_scope_only() {
        let memory = create_test_memory();
        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let score = composite_score(&memory, &context, &config, now);

        // Should be > 0 and use scope_only weights
        assert!(score > 0.0);
        // Relevance: 0.5 * 0.8 = 0.4
        // Scope: 0.4 * 1.0 = 0.4
        // Trust: 0.1 * 1.0 = 0.1
        // Total: ~0.9
        assert!((score - 0.9).abs() < 0.1);
    }

    #[test]
    fn test_composite_score_challenged_penalty() {
        let mut memory = create_test_memory();
        memory.status = Status::Challenged;

        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let score = composite_score(&memory, &context, &config, now);
        let score_without_challenge = composite_score(
            &create_test_memory(),
            &context,
            &config,
            now,
        );

        // Should be 70% of non-challenged score (30% penalty)
        assert!((score - score_without_challenge * 0.7).abs() < 0.01);
    }

    #[test]
    fn test_composite_score_with_decay() {
        let mut memory = create_test_memory();
        memory.created_at = Utc::now() - Duration::days(7);
        memory.decay = Some(Decay::exponential(Duration::days(7)));

        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let score = composite_score(&memory, &context, &config, now);

        // Relevance should be affected by decay: 0.8 * 0.5 = 0.4
        // Total score should be lower than without decay
        assert!(score < 0.9);
        assert!(score > 0.0);
    }
}
