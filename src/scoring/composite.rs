use chrono::{DateTime, Utc};

use crate::types::{EngramConfig, Memory, Status};

use super::decay::effective_relevance;
use super::trust::trust_weight_from_config;

/// Breakdown of composite score components.
///
/// All values are raw (unweighted) scores for transparency.
#[derive(Debug, Clone, Default)]
pub struct ScoreBreakdown {
    /// The final composite score
    pub final_score: f64,
    /// Raw semantic (cosine) similarity score (if available)
    pub semantic: Option<f64>,
    /// Raw keyword match score (if available — only set for search, not retrieve)
    pub keyword: Option<f64>,
    /// Raw cross-encoder rerank score (if reranking was applied)
    pub rerank: Option<f64>,
    /// Effective relevance score (criticality * decay)
    pub relevance: f64,
    /// Scope proximity score
    pub scope: f64,
    /// Trust weight based on provenance (used as multiplier)
    pub trust: f64,
    /// Decay factor (1.0 = no decay)
    pub decay: f64,
    /// Raw criticality value
    pub criticality: f64,
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
///    - base = 0.35*semantic + 0.45*(criticality*decay) + 0.20*scope
///
/// 2. **With query, no embeddings** (query is Some, semantic_score is None):
///    - Uses `config.retrieval.scoring.degraded` weights
///    - base = 0.70*(criticality*decay) + 0.30*scope
///
/// 3. **Scope-only** (no query):
///    - Uses `config.retrieval.scoring.scope_only` weights
///    - base = 0.50*(criticality*decay) + 0.50*scope
///
/// Then: `score = base * trust_weight`
///
/// Challenge penalty: if memory.status == Status::Challenged, `score *= 0.7`
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

    // Calculate base score (trust is a multiplier, not a weighted component)
    let mut score = weights.relevance * relevance + weights.scope * scope_score;

    // Add semantic component if available (store raw score in breakdown)
    let raw_semantic = if let (Some(semantic_weight), Some(semantic_score)) =
        (weights.semantic, context.semantic_score)
    {
        score += semantic_weight * semantic_score;
        Some(semantic_score)
    } else {
        None
    };

    // Apply trust as a multiplier on the entire base score
    score *= trust;

    // Apply challenge penalty if memory is challenged
    if memory.status == Status::Challenged {
        score *= 0.7;
    }

    ScoreBreakdown {
        final_score: score,
        semantic: raw_semantic,
        keyword: None,
        rerank: None,
        relevance,
        scope: scope_score,
        trust,
        decay: decay_factor_value,
        criticality: memory.criticality,
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
        // Semantic should store the raw cosine similarity
        assert_eq!(breakdown.semantic, Some(0.9));
        // keyword should be None for retrieve
        assert!(breakdown.keyword.is_none());
        // criticality should be raw value
        assert_eq!(breakdown.criticality, 0.8);
        // base = 0.35*0.9 + 0.45*0.8 + 0.20*1.0 = 0.315 + 0.36 + 0.20 = 0.875
        // * trust(1.0) = 0.875
        assert!((breakdown.final_score - 0.875).abs() < 0.05);
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
        // base = 0.70*0.8 + 0.30*1.0 = 0.56 + 0.30 = 0.86
        // * trust(1.0) = 0.86
        assert!((breakdown.final_score - 0.86).abs() < 0.05);
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
        // base = 0.50*0.8 + 0.50*1.0 = 0.40 + 0.50 = 0.90
        // * trust(1.0) = 0.90
        assert!((breakdown.final_score - 0.90).abs() < 0.05);
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

        // relevance = criticality(0.8) * decay(~0.5) = ~0.4
        // base = 0.50*0.4 + 0.50*1.0 = 0.70
        // * trust(1.0) = 0.70
        assert!(breakdown.final_score < 0.9);
        assert!(breakdown.final_score > 0.0);
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

        // With criticality=0.0, relevance component is 0.0
        // base = 0.50*0.0 + 0.50*1.0 = 0.50
        // * trust(1.0) = 0.50
        assert!((breakdown.final_score - 0.50).abs() < 0.05);
        assert!((breakdown.relevance - 0.0).abs() < 0.01);
        assert_eq!(breakdown.criticality, 0.0);
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

        // base = 0.50*0.8 + 0.50*0.0 = 0.40
        // * trust(1.0) = 0.40
        assert!((breakdown.final_score - 0.40).abs() < 0.05);
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

        // base = 0.35*1.0 + 0.45*1.0 + 0.20*1.0 = 1.0
        // * trust(1.0) = 1.0
        assert!((breakdown.final_score - 1.0).abs() < 0.05);
        assert!(breakdown.final_score <= 1.1);
        assert_eq!(breakdown.decay, 1.0);
        assert_eq!(breakdown.semantic, Some(1.0));
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
    fn test_trust_multiplier_reduces_score_proportionally() {
        let config = EngramConfig::default();
        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let now = Utc::now();

        // Human provenance (trust = 1.0)
        let human_memory = create_test_memory();
        let human_breakdown = composite_score(&human_memory, &context, &config, now);

        // Agent provenance (trust = 0.85)
        let mut agent_memory = create_test_memory();
        agent_memory.provenance = Provenance::agent("test-agent");
        let agent_breakdown = composite_score(&agent_memory, &context, &config, now);

        // Inferred provenance (trust = 0.6)
        let mut inferred_memory = create_test_memory();
        inferred_memory.provenance = Provenance::inferred();
        let inferred_breakdown = composite_score(&inferred_memory, &context, &config, now);

        // Imported provenance (trust = 0.7)
        let mut imported_memory = create_test_memory();
        imported_memory.provenance = Provenance::imported();
        let imported_breakdown = composite_score(&imported_memory, &context, &config, now);

        // Trust is a multiplier, so scores should be exactly proportional
        let human_score = human_breakdown.final_score;
        assert!(
            (agent_breakdown.final_score - human_score * 0.85).abs() < 0.001,
            "agent score {} should be {} (human * 0.85)",
            agent_breakdown.final_score,
            human_score * 0.85
        );
        assert!(
            (imported_breakdown.final_score - human_score * 0.7).abs() < 0.001,
            "imported score {} should be {} (human * 0.7)",
            imported_breakdown.final_score,
            human_score * 0.7
        );
        assert!(
            (inferred_breakdown.final_score - human_score * 0.6).abs() < 0.001,
            "inferred score {} should be {} (human * 0.6)",
            inferred_breakdown.final_score,
            human_score * 0.6
        );

        // Verify ordering: human > agent > imported > inferred
        assert!(human_score > agent_breakdown.final_score);
        assert!(agent_breakdown.final_score > imported_breakdown.final_score);
        assert!(imported_breakdown.final_score > inferred_breakdown.final_score);
    }

    #[test]
    fn test_trust_multiplier_with_semantic() {
        let config = EngramConfig::default();
        let context = ScoringContext::with_semantic(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "oauth authentication".to_string(),
            0.9,
        );
        let now = Utc::now();

        let human_memory = create_test_memory();
        let human_score = composite_score(&human_memory, &context, &config, now).final_score;

        let mut inferred_memory = create_test_memory();
        inferred_memory.provenance = Provenance::inferred();
        let inferred_score = composite_score(&inferred_memory, &context, &config, now).final_score;

        // Inferred should be exactly 0.6x of human (trust multiplier applies to full base)
        assert!(
            (inferred_score - human_score * 0.6).abs() < 0.001,
            "with semantic: inferred {} should be {} (human * 0.6)",
            inferred_score,
            human_score * 0.6
        );
    }

    #[test]
    fn test_trust_multiplier_combined_with_challenge() {
        let config = EngramConfig::default();
        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let now = Utc::now();

        // Human, active
        let human_score =
            composite_score(&create_test_memory(), &context, &config, now).final_score;

        // Inferred + challenged: score = base * 0.6 * 0.7
        let mut challenged_inferred = create_test_memory();
        challenged_inferred.provenance = Provenance::inferred();
        challenged_inferred.status = Status::Challenged;
        let ci_score = composite_score(&challenged_inferred, &context, &config, now).final_score;

        assert!(
            (ci_score - human_score * 0.6 * 0.7).abs() < 0.001,
            "challenged inferred {} should be {} (human * 0.6 * 0.7)",
            ci_score,
            human_score * 0.6 * 0.7
        );
    }

    #[test]
    fn test_breakdown_keyword_none_for_retrieve() {
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

        // keyword should always be None for retrieve (composite_score)
        assert!(breakdown.keyword.is_none());
        // criticality should be the raw value
        assert_eq!(breakdown.criticality, 0.8);
        // relevance should be criticality * decay
        assert_eq!(breakdown.relevance, 0.8 * breakdown.decay);
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

    #[test]
    fn test_composite_score_rerank_is_none() {
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

        // composite_score never sets rerank — it's populated later by the engine
        assert!(breakdown.rerank.is_none());
    }
}
