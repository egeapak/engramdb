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
    /// Raw scope proximity score (before multiplier transform)
    pub scope: f64,
    /// Computed scope multiplier: `floor + (1 - floor) * scope_score` when
    /// scope context is present, or 1.0 when no context is provided.
    pub scope_multiplier: f64,
    /// Trust weight based on provenance (used as multiplier)
    pub trust: f64,
    /// Decay amount (0.0 = fresh, 1.0 = fully decayed)
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

    /// Normalized keyword match score (if keyword search was used)
    pub keyword_score: Option<f64>,

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
            keyword_score: None,
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
            keyword_score: None,
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
            keyword_score: None,
            semantic_score: Some(semantic_score),
            embeddings_available: true,
        }
    }

    /// Create a new ScoringContext for keyword search (with optional semantic score).
    pub fn with_keyword(
        path: Option<String>,
        logical: Vec<String>,
        query: String,
        keyword_score: f64,
        semantic_score: Option<f64>,
    ) -> Self {
        Self {
            path,
            logical,
            query: Some(query),
            keyword_score: Some(keyword_score),
            semantic_score,
            embeddings_available: semantic_score.is_some(),
        }
    }
}

/// Calculate the composite score for a memory in a given context.
///
/// # Embedding availability note
///
/// When the semantic component is absent (no embedding provider), the
/// remaining weights are renormalized so they sum to 1.0. This means
/// the same keyword match can produce different composite scores
/// depending on whether the embedding backend is available. This is
/// intentional — semantic similarity adds a signal that other components
/// cannot replace — but be aware that benchmarks run without ONNX may
/// not predict production ranking exactly.
///
/// The scoring operates in four modes:
///
/// 1. **With keyword** (keyword_score is Some):
///    - Uses `config.retrieval.scoring.with_keyword` weights
///    - base = 0.45*keyword + 0.30*semantic + 0.25*(criticality*decay)
///
/// 2. **With query + embeddings** (semantic_score is Some):
///    - Uses `config.retrieval.scoring.with_query` weights
///    - base = 0.45*semantic + 0.55*(criticality*decay)
///
/// 3. **With query, no embeddings** (query is Some, semantic_score is None):
///    - Uses `config.retrieval.scoring.degraded` weights
///    - base = 1.0*(criticality*decay)
///
/// 4. **Scope-only** (no query):
///    - Uses `config.retrieval.scoring.scope_only` weights
///    - base = 1.0*(criticality*decay)
///
/// Then: `score = base * scope_multiplier * trust_weight`
///
/// Scope multiplier: when scope context is provided,
/// `scope_multiplier = floor + (1 - floor) * scope_score` (default floor=0.5).
/// When no context is provided, scope_multiplier = 1.0 (neutral).
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
    composite_score_inner(memory, context, config, now, false)
}

/// Like [`composite_score`] but ignores time-based decay when scoring.
///
/// The real decay factor is still recorded in the breakdown for transparency,
/// but `relevance` uses `criticality` directly (as if decay = 1.0).
/// This allows expired memories to be scored by scope and criticality alone,
/// so the relevance threshold still filters out irrelevant results.
pub fn composite_score_ignore_decay(
    memory: &Memory,
    context: &ScoringContext,
    config: &EngramConfig,
    now: DateTime<Utc>,
) -> ScoreBreakdown {
    composite_score_inner(memory, context, config, now, true)
}

fn composite_score_inner(
    memory: &Memory,
    context: &ScoringContext,
    config: &EngramConfig,
    now: DateTime<Utc>,
    ignore_decay: bool,
) -> ScoreBreakdown {
    // Calculate decay factor (always computed for the breakdown)
    let decay_factor_value = super::decay::decay_factor(memory.created_at, now, &memory.decay);

    // Calculate relevance: when ignoring decay, use criticality directly
    let relevance = if ignore_decay {
        memory.criticality
    } else {
        effective_relevance(memory, now)
    };

    let scope_score = crate::scope::scope_proximity(
        &memory.physical,
        &memory.logical,
        context.path.as_deref(),
        &context.logical,
    );

    let trust = trust_weight_from_config(memory.provenance.source, &config.trust_weights);

    // Determine which weights to use based on context.
    // Priority: keyword > semantic (any value) > degraded (query but no signals) > scope_only
    let weights = if context.keyword_score.is_some() {
        &config.retrieval.scoring.with_keyword
    } else if context.semantic_score.is_some() {
        // With query + embeddings (semantic=0.0 is valid: "checked, found nothing")
        &config.retrieval.scoring.with_query
    } else if context.query.is_some() {
        // With query, no embeddings (degraded)
        &config.retrieval.scoring.degraded
    } else {
        // Scope-only
        &config.retrieval.scoring.scope_only
    };

    // Dynamic weight accumulation: only add components when both weight and
    // value are present. Track active weight sum for renormalization.
    let mut score = 0.0;
    let mut active_weight_sum = 0.0;

    // Keyword component
    let raw_keyword =
        if let (Some(kw_weight), Some(kw_score)) = (weights.keyword, context.keyword_score) {
            score += kw_weight * kw_score;
            active_weight_sum += kw_weight;
            Some(kw_score)
        } else {
            None
        };

    // Semantic component — always include when Some, even at 0.0.
    // sem=Some(0.0) means "checked, found nothing" and should consume its
    // weight budget at zero, producing a lower score than sem=None (degraded).
    let raw_semantic =
        if let (Some(sem_weight), Some(sem_score)) = (weights.semantic, context.semantic_score) {
            score += sem_weight * sem_score;
            active_weight_sum += sem_weight;
            Some(sem_score)
        } else {
            None
        };

    // Relevance is always active
    score += weights.relevance * relevance;
    active_weight_sum += weights.relevance;

    // Renormalize if active weights don't sum to 1.0
    if (active_weight_sum - 1.0).abs() > f64::EPSILON && active_weight_sum > f64::EPSILON {
        score /= active_weight_sum;
    }

    // Apply scope as a post-multiplier (like trust).
    // When scope context is provided: multiplier = floor + (1 - floor) * scope_score
    // When no context: multiplier = 1.0 (neutral, doesn't penalize global searches)
    let has_scope_context = context.path.is_some() || !context.logical.is_empty();
    let scope_multiplier = if has_scope_context {
        let floor = config.retrieval.scoring.scope_multiplier_floor;
        floor + (1.0 - floor) * scope_score
    } else {
        1.0
    };
    score *= scope_multiplier;

    // Apply trust as a multiplier on the entire base score
    score *= trust;

    // Apply challenge penalty if memory is challenged
    if memory.status == Status::Challenged {
        score *= 0.7;
    }

    // Safety clamp to [0, 1]
    score = score.clamp(0.0, 1.0);

    ScoreBreakdown {
        final_score: score,
        semantic: raw_semantic,
        keyword: raw_keyword,
        rerank: None,
        relevance,
        scope: scope_score,
        scope_multiplier,
        trust,
        decay: 1.0 - decay_factor_value,
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
        // base = 0.45*0.9 + 0.55*0.8 = 0.405 + 0.44 = 0.845
        // * scope_mult(1.0) * trust(1.0) = 0.845
        assert!((breakdown.final_score - 0.845).abs() < 0.01);
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
        // base = 1.0*0.8 = 0.80
        // * scope_mult(1.0) * trust(1.0) = 0.80
        assert!((breakdown.final_score - 0.80).abs() < 0.01);
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
        // base = 1.0*0.8 = 0.80
        // * scope_mult(1.0) * trust(1.0) = 0.80
        assert!((breakdown.final_score - 0.80).abs() < 0.01);
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
        // base = 1.0*0.4 = 0.4, scope_mult=1.0, trust=1.0 → ~0.4
        assert!(breakdown.final_score < 0.9);
        assert!(breakdown.final_score > 0.0);
        assert!((breakdown.decay - 0.5).abs() < 0.1); // 0.5 = half decayed
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
        // base = 1.0*0.0 = 0.0
        // * scope_mult(1.0) * trust(1.0) = 0.0
        assert!((breakdown.final_score - 0.0).abs() < 0.01);
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

        // base = 1.0*0.8 = 0.80
        // * scope_mult(0.5 + 0.5*0.0 = 0.5) * trust(1.0) = 0.40
        assert!((breakdown.final_score - 0.40).abs() < 0.01);
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

        // base = 0.45*1.0 + 0.55*1.0 = 1.0
        // * scope_mult(1.0) * trust(1.0) = 1.0
        assert!((breakdown.final_score - 1.0).abs() < 0.01);
        assert!(breakdown.final_score <= 1.1);
        assert_eq!(breakdown.decay, 0.0); // 0.0 = fresh, no decay
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
        // relevance should be criticality * (1 - decay), since decay=0 means fresh
        assert_eq!(breakdown.relevance, 0.8 * (1.0 - breakdown.decay));
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
    fn test_ignore_decay_fresh_memory_same_as_normal() {
        // When decay=1.0 (fresh), both functions should produce identical scores
        let memory = create_test_memory(); // decay=None → factor=1.0
        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let normal = composite_score(&memory, &context, &config, now);
        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        assert!(
            (normal.final_score - ignore.final_score).abs() < 0.001,
            "fresh memory: normal={} ignore={}",
            normal.final_score,
            ignore.final_score,
        );
        assert_eq!(normal.decay, ignore.decay);
        assert_eq!(normal.relevance, ignore.relevance);
    }

    #[test]
    fn test_ignore_decay_expired_memory_scores_higher() {
        // Fully expired memory: ignore_decay should score much higher
        let mut memory = create_test_memory();
        memory.criticality = 0.8;
        memory.created_at = Utc::now() - Duration::days(15);
        memory.decay = Some(Decay::linear(Duration::days(10))); // expired, floor=0.0

        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let normal = composite_score(&memory, &context, &config, now);
        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        // normal: relevance = 0.8 * 0.0 = 0.0, base = 1.0*0.0 = 0.0
        // scope_mult=1.0, trust=1.0 → 0.0
        assert!((normal.relevance - 0.0).abs() < 0.01);
        assert!((normal.final_score - 0.0).abs() < 0.01);

        // ignore: relevance = 0.8, base = 1.0*0.8 = 0.8
        // scope_mult=1.0, trust=1.0 → 0.8
        assert!((ignore.relevance - 0.8).abs() < 0.01);
        assert!((ignore.final_score - 0.80).abs() < 0.01);

        assert!(ignore.final_score > normal.final_score);
    }

    #[test]
    fn test_ignore_decay_records_real_decay_in_breakdown() {
        // The breakdown should still record the real decay value
        let mut memory = create_test_memory();
        memory.created_at = Utc::now() - Duration::days(7);
        memory.decay = Some(Decay::exponential(Duration::days(7))); // ~0.5 decay

        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        // Real decay should be ~0.5 in the breakdown
        assert!(
            (ignore.decay - 0.5).abs() < 0.1,
            "breakdown decay should be ~0.5, got {}",
            ignore.decay,
        );
        // But relevance should use criticality directly (0.8)
        assert!(
            (ignore.relevance - 0.8).abs() < 0.01,
            "relevance should be 0.8 (ignoring decay), got {}",
            ignore.relevance,
        );
    }

    #[test]
    fn test_ignore_decay_half_decayed_comparison() {
        // Half-decayed: normal uses 0.8*0.5=0.4, ignore uses 0.8
        let mut memory = create_test_memory();
        memory.criticality = 0.8;
        memory.created_at = Utc::now() - Duration::days(7);
        memory.decay = Some(Decay::exponential(Duration::days(7)));

        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let normal = composite_score(&memory, &context, &config, now);
        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        // normal: relevance ≈ 0.8 * 0.5 = 0.4 → base = 1.0*0.4 = 0.4
        // scope_mult=1.0, trust=1.0 → ~0.4
        assert!((normal.relevance - 0.4).abs() < 0.1);
        assert!((normal.final_score - 0.40).abs() < 0.05);

        // ignore: relevance = 0.8 → base = 1.0*0.8 = 0.8
        // scope_mult=1.0, trust=1.0 → 0.8
        assert!((ignore.relevance - 0.8).abs() < 0.01);
        assert!((ignore.final_score - 0.80).abs() < 0.01);
    }

    #[test]
    fn test_ignore_decay_challenge_penalty_still_applies() {
        let mut memory = create_test_memory();
        memory.status = Status::Challenged;
        memory.created_at = Utc::now() - Duration::days(15);
        memory.decay = Some(Decay::linear(Duration::days(10)));

        let context = ScoringContext::scope_only(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        // base = 1.0*0.8 = 0.80, scope_mult=1.0, trust=1.0
        // challenged penalty: 0.80 * 0.7 = 0.56
        assert!(
            (ignore.final_score - 0.56).abs() < 0.01,
            "challenged ignore_decay: expected ~0.63, got {}",
            ignore.final_score,
        );
    }

    #[test]
    fn test_ignore_decay_low_criticality_low_scope_below_threshold() {
        // Even with ignore_decay, low crit + no scope match → low score
        let mut memory = create_test_memory();
        memory.criticality = 0.3;
        memory.physical = vec!["src/api/auth.rs".to_string()];
        memory.created_at = Utc::now() - Duration::days(30);
        memory.decay = Some(Decay::linear(Duration::days(10)));

        let context =
            ScoringContext::scope_only(Some("completely/different/path.rs".to_string()), vec![]);
        let config = EngramConfig::default();
        let now = Utc::now();

        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        // relevance = 0.3, base = 1.0*0.3 = 0.3
        // scope_mult = 0.5 + 0.5*0.0 = 0.5 → 0.3*0.5 = 0.15
        // Below default threshold of 0.3
        assert!(
            ignore.final_score < 0.3,
            "low crit+no scope should still be below threshold: {}",
            ignore.final_score,
        );
    }

    #[test]
    fn test_ignore_decay_with_semantic_mode() {
        // Verify ignore_decay works correctly in semantic (with_query) mode too
        let mut memory = create_test_memory();
        memory.criticality = 0.8;
        memory.created_at = Utc::now() - Duration::days(15);
        memory.decay = Some(Decay::linear(Duration::days(10)));

        let context = ScoringContext::with_semantic(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "test query".to_string(),
            0.9,
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let normal = composite_score(&memory, &context, &config, now);
        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        // Normal: relevance=0.0 → base = 0.45*0.9 + 0.55*0.0 = 0.405
        // * scope_mult(1.0) * trust(1.0) = 0.405
        // Ignore: relevance=0.8 → base = 0.45*0.9 + 0.55*0.8 = 0.845
        // * scope_mult(1.0) * trust(1.0) = 0.845
        assert!(ignore.final_score > normal.final_score);
        assert!((ignore.final_score - 0.845).abs() < 0.01);
    }

    #[test]
    fn test_composite_score_with_keyword() {
        let memory = create_test_memory();
        let context = ScoringContext::with_keyword(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "authentication".to_string(),
            0.7,       // normalized keyword score
            Some(0.9), // semantic score
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // Should use with_keyword weights: kw=0.45, sem=0.30, rel=0.25
        // base = 0.45*0.7 + 0.30*0.9 + 0.25*0.8 = 0.315 + 0.27 + 0.20 = 0.785
        // * scope_mult(1.0) * trust(1.0) = 0.785
        assert!((breakdown.final_score - 0.785).abs() < 0.01);
        assert_eq!(breakdown.keyword, Some(0.7));
        assert_eq!(breakdown.semantic, Some(0.9));
    }

    #[test]
    fn test_composite_score_semantic_zero_stays_in_with_query() {
        let memory = create_test_memory();
        // semantic_score = Some(0.0) should stay in with_query mode (not fall to degraded).
        // sem=Some(0.0) means "checked, found nothing" — it consumes its weight at zero.
        let context = ScoringContext::with_semantic(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "test query".to_string(),
            0.0,
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // with_query weights: 0.45*0.0 + 0.55*0.8 = 0.44
        // * scope_mult(1.0) * trust(1.0) = 0.44
        assert!(
            (breakdown.final_score - 0.44).abs() < 0.01,
            "semantic=0.0 should use with_query weights, got {}",
            breakdown.final_score,
        );
        // Semantic is recorded as Some(0.0) in the breakdown
        assert_eq!(breakdown.semantic, Some(0.0));
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

    #[test]
    fn test_scope_multiplier_distinguishes_levels() {
        let config = EngramConfig::default();
        let now = Utc::now();

        let score_for_scope = |path: &str| {
            let memory = create_test_memory();
            let context = ScoringContext::scope_only(Some(path.to_string()), vec![]);
            composite_score(&memory, &context, &config, now).final_score
        };

        let exact = score_for_scope("src/api/auth.rs"); // scope=1.0 → mult=1.0
        let same_dir = score_for_scope("src/api/other.rs"); // scope=0.85 → mult=0.925
        let no_match = score_for_scope("completely/different.rs"); // scope=0.0 → mult=0.5

        assert!(exact > same_dir, "exact {} > same_dir {}", exact, same_dir);
        assert!(
            same_dir > no_match,
            "same_dir {} > no_match {}",
            same_dir,
            no_match
        );

        // Verify the multiplier values
        assert!((exact / 0.8 - 1.0).abs() < 0.01); // base=0.8, mult=1.0
        assert!((no_match / 0.8 - 0.5).abs() < 0.01); // base=0.8, mult=0.5
    }

    #[test]
    fn test_semantic_none_vs_zero_differ() {
        let memory = create_test_memory();
        let config = EngramConfig::default();
        let now = Utc::now();

        // sem=None → degraded mode, base = 1.0*0.8 = 0.8
        let ctx_none = ScoringContext::with_query_degraded(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "test query".to_string(),
        );
        let score_none = composite_score(&memory, &ctx_none, &config, now).final_score;

        // sem=Some(0.0) → with_query mode, base = 0.45*0.0 + 0.55*0.8 = 0.44
        let ctx_zero = ScoringContext::with_semantic(
            Some("src/api/auth.rs".to_string()),
            vec!["auth.oauth".to_string()],
            "test query".to_string(),
            0.0,
        );
        let score_zero = composite_score(&memory, &ctx_zero, &config, now).final_score;

        // sem=None should score higher than sem=Some(0.0)
        assert!(
            score_none > score_zero,
            "sem=None ({}) should score higher than sem=Some(0.0) ({})",
            score_none,
            score_zero,
        );
    }

    #[test]
    fn test_no_semantic_discontinuity() {
        let memory = create_test_memory();
        let config = EngramConfig::default();
        let now = Utc::now();

        let score_at = |sem: f64| {
            let ctx = ScoringContext::with_semantic(
                Some("src/api/auth.rs".to_string()),
                vec!["auth.oauth".to_string()],
                "test query".to_string(),
                sem,
            );
            composite_score(&memory, &ctx, &config, now).final_score
        };

        let at_zero = score_at(0.0);
        let at_tiny = score_at(0.001);
        let at_small = score_at(0.01);

        // No cliff: scores should be very close and monotonically increasing
        let diff_tiny = (at_tiny - at_zero).abs();
        let diff_small = (at_small - at_zero).abs();
        assert!(
            diff_tiny < 0.01,
            "sem=0.001 ({}) vs sem=0.0 ({}) differ by {} (should be < 0.01)",
            at_tiny,
            at_zero,
            diff_tiny,
        );
        assert!(
            diff_small < 0.01,
            "sem=0.01 ({}) vs sem=0.0 ({}) differ by {} (should be < 0.01)",
            at_small,
            at_zero,
            diff_small,
        );
        assert!(
            at_tiny >= at_zero,
            "scores should be monotonically increasing"
        );
        assert!(
            at_small >= at_tiny,
            "scores should be monotonically increasing"
        );
    }

    #[test]
    fn test_scope_multiplier_neutral_without_context() {
        let memory = create_test_memory();
        let config = EngramConfig::default();
        let now = Utc::now();

        // No scope context at all (like a global search)
        let context =
            ScoringContext::with_keyword(None, vec![], "auth".to_string(), 0.8, Some(0.9));
        let breakdown = composite_score(&memory, &context, &config, now);

        // scope_multiplier should be 1.0 (neutral)
        assert!(
            (breakdown.scope_multiplier - 1.0).abs() < f64::EPSILON,
            "scope_multiplier should be 1.0 without context, got {}",
            breakdown.scope_multiplier,
        );
    }
}
