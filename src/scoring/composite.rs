use chrono::{DateTime, Utc};

use crate::types::{EngramConfig, Memory, Status};

use super::decay::effective_relevance;
use super::trust::trust_weight_from_config;

/// Breakdown of composite score components.
#[derive(Debug, Clone)]
pub struct ScoreBreakdown {
    /// The final composite score (0.0 to 1.0+)
    pub final_score: f64,
    /// Semantic similarity score (if available)
    pub semantic: Option<f64>,
    /// Effective relevance score (criticality * decay)
    pub relevance: f64,
    /// Scope proximity score
    pub scope: f64,
    /// Trust weight based on provenance
    pub trust: f64,
    /// Decay factor (1.0 = no decay)
    pub decay: f64,
}

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
    pub fn with_query_degraded(path: Option<String>, logical: Vec<String>, query: String) -> Self {
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
/// ScoreBreakdown with component scores and final composite score
pub fn composite_score(
    memory: &Memory,
    context: &ScoringContext,
    config: &EngramConfig,
    now: DateTime<Utc>,
) -> ScoreBreakdown {
    // Calculate component scores
    let relevance = effective_relevance(memory, now);

    let scope_score = crate::scope::scope_proximity(
        &memory.physical,
        &memory.logical,
        context.path.as_deref(),
        &context.logical,
    );

    let trust = trust_weight_from_config(memory.provenance.source, &config.trust_weights);

    // Calculate decay factor
    let decay_factor_value = super::decay::decay_factor(memory.created_at, now, &memory.decay);

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
    let mut score =
        weights.relevance * relevance + weights.scope * scope_score + weights.trust * trust;

    // Add semantic component if available
    let semantic_contribution = if let (Some(semantic_weight), Some(semantic_score)) =
        (weights.semantic, context.semantic_score)
    {
        let contrib = semantic_weight * semantic_score;
        score += contrib;
        Some(contrib)
    } else {
        None
    };

    // Apply challenge penalty if memory is challenged
    if memory.status == Status::Challenged {
        // Default challenge penalty is 0.3
        let challenge_penalty = 0.3;
        score *= 1.0 - challenge_penalty;
    }

    ScoreBreakdown {
        final_score: score,
        semantic: semantic_contribution,
        relevance,
        scope: scope_score,
        trust,
        decay: decay_factor_value,
    }
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
            verified_at: None,
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

        let breakdown = composite_score(&memory, &context, &config, now);

        // Should be > 0 and use with_query weights
        assert!(breakdown.final_score > 0.0);
        // Semantic should contribute
        assert!(breakdown.semantic.is_some());
        // Semantic should contribute: 0.5 * 0.9 = 0.45
        // Relevance: 0.3 * 0.8 = 0.24
        // Scope: 0.15 * 1.0 = 0.15 (exact match)
        // Trust: 0.05 * 1.0 = 0.05
        // Total: ~0.89
        assert!((breakdown.final_score - 0.89).abs() < 0.1);
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

        let breakdown = composite_score(&memory, &context, &config, now);

        // Should be > 0 and use degraded weights
        assert!(breakdown.final_score > 0.0);
        // No semantic component
        assert!(breakdown.semantic.is_none());
        // Relevance: 0.6 * 0.8 = 0.48
        // Scope: 0.3 * 1.0 = 0.30
        // Trust: 0.1 * 1.0 = 0.10
        // Total: ~0.88
        assert!((breakdown.final_score - 0.88).abs() < 0.1);
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

        let breakdown = composite_score(&memory, &context, &config, now);

        // Should be > 0 and use scope_only weights
        assert!(breakdown.final_score > 0.0);
        // No semantic component
        assert!(breakdown.semantic.is_none());
        // Relevance: 0.5 * 0.8 = 0.4
        // Scope: 0.4 * 1.0 = 0.4
        // Trust: 0.1 * 1.0 = 0.1
        // Total: ~0.9
        assert!((breakdown.final_score - 0.9).abs() < 0.1);
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

        let breakdown = composite_score(&memory, &context, &config, now);
        let breakdown_without_challenge =
            composite_score(&create_test_memory(), &context, &config, now);

        // Should be 70% of non-challenged score (30% penalty)
        assert!(
            (breakdown.final_score - breakdown_without_challenge.final_score * 0.7).abs() < 0.01
        );
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

        let breakdown = composite_score(&memory, &context, &config, now);

        // Relevance should be affected by decay: 0.8 * 0.5 = 0.4
        // Total score should be lower than without decay
        assert!(breakdown.final_score < 0.9);
        assert!(breakdown.final_score > 0.0);
        // Decay factor should be around 0.5 for exponential decay at half-life
        assert!((breakdown.decay - 0.5).abs() < 0.1);
    }

    #[test]
    fn test_composite_score_needs_review_no_penalty() {
        let mut memory = create_test_memory();
        memory.status = Status::NeedsReview;

        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);
        let breakdown_active = composite_score(&create_test_memory(), &context, &config, now);

        // Status::NeedsReview should NOT get the 0.8x penalty (only Challenged does)
        assert!((breakdown.final_score - breakdown_active.final_score).abs() < 0.01);
    }

    #[test]
    fn test_composite_score_zero_criticality() {
        let mut memory = create_test_memory();
        memory.criticality = 0.0;

        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // With criticality=0.0, relevance component is 0.0, so score should be much lower
        // Scope: 0.4 * 1.0 = 0.4
        // Trust: 0.2 * 1.0 = 0.2
        // Total: ~0.6
        assert!((breakdown.final_score - 0.6).abs() < 0.1);
        assert!((breakdown.relevance - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_composite_score_zero_scope_proximity() {
        let memory = create_test_memory();

        // No scope match -> scope component is 0.0
        let context = ScoringContext::scope_only(
            Some("completely/different/path.rs".to_string()),
            vec!["completely.different.scope".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // Scope component = 0.0, but other components still contribute
        // Relevance: 0.5 * 0.8 = 0.4
        // Trust: 0.1 * 1.0 = 0.1
        // Total: ~0.5
        assert!((breakdown.final_score - 0.5).abs() < 0.1);
        assert!((breakdown.scope - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_composite_score_boundary_max_values() {
        let mut memory = create_test_memory();
        memory.criticality = 1.0;
        memory.confidence = 1.0;

        let context = ScoringContext::with_semantic(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "oauth authentication".to_string(),
            1.0,
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // All components at 1.0 -> final score should be at max
        // Semantic: 0.5 * 1.0 = 0.5
        // Relevance: 0.3 * 1.0 = 0.3
        // Scope: 0.15 * 1.0 = 0.15
        // Trust: 0.05 * 1.0 = 0.05
        // Total: 1.0
        assert!((breakdown.final_score - 1.0).abs() < 0.1);
        assert!(breakdown.final_score <= 1.1); // Reasonable cap
        assert_eq!(breakdown.decay, 1.0); // No decay
    }

    #[test]
    fn test_score_breakdown_structure() {
        let memory = create_test_memory();
        let context = ScoringContext::with_semantic(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "test query".to_string(),
            0.8,
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // Verify all fields are populated
        assert!(breakdown.final_score > 0.0);
        assert!(breakdown.semantic.is_some());
        assert!(breakdown.relevance >= 0.0);
        assert!(breakdown.scope >= 0.0);
        assert!(breakdown.trust >= 0.0);
        assert!(breakdown.decay >= 0.0);
        assert!(breakdown.decay <= 1.0);
    }

    #[test]
    fn test_score_breakdown_degraded_mode() {
        let memory = create_test_memory();
        let context = ScoringContext::with_query_degraded(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "test query".to_string(),
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // In degraded mode, semantic should be None
        assert!(breakdown.semantic.is_none());
        assert!(breakdown.final_score > 0.0);
    }

    #[test]
    fn test_score_breakdown_scope_only_mode() {
        let memory = create_test_memory();
        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // In scope-only mode, semantic should be None
        assert!(breakdown.semantic.is_none());
        assert!(breakdown.final_score > 0.0);
    }
}
