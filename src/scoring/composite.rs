use chrono::{DateTime, Utc};

use crate::types::{Decay, EngramConfig, Epistemic, Memory, ProvenanceSource, Situation, Status};

use super::trust::trust_weight_from_config;

/// The exact set of fields [`composite_score`] reads off a memory.
///
/// Introduced so the no-query Rank path can score straight from the LanceDB
/// index projection (`storage::IndexForFiltering`) without reading and parsing
/// the memory's `.md` file — every field here is carried in the memories-table
/// schema (`decay` and the scope/timestamp/provenance/status columns), so an
/// index-derived target scores byte-identically to the file-loaded one.
#[derive(Debug, Clone, Copy)]
pub struct ScoreTarget<'a> {
    pub created_at: DateTime<Utc>,
    pub decay: &'a Option<Decay>,
    pub criticality: f64,
    pub physical: &'a [String],
    pub logical: &'a [String],
    pub provenance_source: ProvenanceSource,
    pub status: Status,
    /// Epistemic class: drives the situation multiplier, the per-class
    /// challenge penalty, and (for facts) the freshness anchor.
    pub epistemic: Epistemic,
    /// Fact freshness anchor (§7.3): for `Fact` targets decay is measured
    /// from `verified_at` when present, so doctor verification refreshes a
    /// decaying fact without new formula terms. Ignored for other classes.
    pub verified_at: Option<DateTime<Utc>>,
}

impl<'a> From<&'a Memory> for ScoreTarget<'a> {
    fn from(m: &'a Memory) -> Self {
        Self {
            created_at: m.created_at,
            decay: &m.decay,
            criticality: m.criticality,
            physical: &m.physical,
            logical: &m.logical,
            provenance_source: m.provenance.source,
            status: m.status,
            epistemic: m.epistemic,
            verified_at: m.verified_at,
        }
    }
}

/// Breakdown of composite score components.
///
/// All values are raw (unweighted) scores for transparency.
///
/// After reranking, `final_score` reflects the blended value while other
/// fields retain their pre-rerank values. The presence of `rerank: Some(...)`
/// signals that `final_score` is blended and not directly reproducible from
/// the breakdown components.
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
    /// Computed scope multiplier: `scope_score` when scope context is present,
    /// or 1.0 when no context is provided. With a `path`, depth decay provides
    /// the floor; with logical-only context, the score is built on
    /// `scope_multiplier_floor` so the bonus alone never caps the multiplier
    /// (see `scope::scope_proximity`).
    pub scope_multiplier: f64,
    /// Raw trust weight based on provenance
    pub trust: f64,
    /// Effective trust multiplier after floor transform:
    /// `floor + (1 - floor) * trust_weight`
    pub trust_multiplier: f64,
    /// Situation-conditioned epistemic-class multiplier:
    /// `floor + (1 - floor) * profile[situation][epistemic]`, or 1.0 when the
    /// query carries no situation (every pre-epistemic query).
    pub situation_multiplier: f64,
    /// Decay amount (0.0 = fresh, 1.0 = fully decayed)
    pub decay: f64,
    /// Raw criticality value
    pub criticality: f64,
}

/// Context for scoring a memory during retrieval.
///
/// All string fields borrow from the caller to avoid per-memory heap
/// allocations in the scoring hot path.
#[derive(Debug, Clone)]
pub struct ScoringContext<'a> {
    /// Current file path (if any)
    pub path: Option<&'a str>,

    /// Current logical scope tags
    pub logical: &'a [String],

    /// Search query (if any)
    pub query: Option<&'a str>,

    /// Normalized keyword match score (if keyword search was used)
    pub keyword_score: Option<f64>,

    /// Semantic similarity score from vector search (if available)
    pub semantic_score: Option<f64>,

    /// Whether embeddings are available for this retrieval
    pub embeddings_available: bool,

    /// The querying agent's situation (§6.2). `None` ⇒ neutral multiplier —
    /// every constructor starts here; thread a value in via
    /// [`ScoringContext::with_situation`].
    pub situation: Option<Situation>,
}

impl<'a> ScoringContext<'a> {
    /// Create a new ScoringContext for scope-only retrieval
    pub fn scope_only(path: Option<&'a str>, logical: &'a [String]) -> Self {
        Self {
            path,
            logical,
            query: None,
            keyword_score: None,
            semantic_score: None,
            embeddings_available: false,
            situation: None,
        }
    }

    /// Create a new ScoringContext with a query (degraded mode, no embeddings)
    pub fn with_query_degraded(
        path: Option<&'a str>,
        logical: &'a [String],
        query: &'a str,
    ) -> Self {
        Self {
            path,
            logical,
            query: Some(query),
            keyword_score: None,
            semantic_score: None,
            embeddings_available: false,
            situation: None,
        }
    }

    /// Create a new ScoringContext with a query and semantic score (full mode)
    pub fn with_semantic(
        path: Option<&'a str>,
        logical: &'a [String],
        query: &'a str,
        semantic_score: f64,
    ) -> Self {
        Self {
            path,
            logical,
            query: Some(query),
            keyword_score: None,
            semantic_score: Some(semantic_score),
            embeddings_available: true,
            situation: None,
        }
    }

    /// Create a new ScoringContext for keyword search (with optional semantic score).
    pub fn with_keyword(
        path: Option<&'a str>,
        logical: &'a [String],
        query: &'a str,
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
            situation: None,
        }
    }

    /// Attach the querying agent's situation (builder-style; `None` clears).
    pub fn with_situation(mut self, situation: Option<Situation>) -> Self {
        self.situation = situation;
        self
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
/// 1. **With keyword** (keyword_score is Some; semantic optional):
///    - Uses `config.retrieval.scoring.with_keyword` weights
///    - base = 0.45*keyword + 0.30*semantic + 0.25*(criticality*decay)
///    - When semantic_score is None, the semantic weight drops out and the
///      remaining weights renormalize (≈ 0.64*keyword + 0.36*relevance).
///      The retrieval engine passes both signals when both matched, so the
///      combined formula above is the normal full-evidence path.
///
/// 2. **With query + embeddings** (semantic_score is Some, no keyword):
///    - Uses `config.retrieval.scoring.with_query` weights
///    - base = 0.55*semantic + 0.45*(criticality*decay)
///    - `semantic_score = Some(0.0)` is valid ("checked, found nothing" —
///      e.g. an embedded memory that missed the vector-search top-k) and
///      consumes the semantic weight at zero, scoring strictly lower than
///      `None` (no evidence, mode 3).
///
/// 3. **With query, no semantic evidence** (query is Some, both scores None —
///    embeddings unavailable, or the memory was never embedded):
///    - Uses `config.retrieval.scoring.degraded` weights
///    - base = 1.0*(criticality*decay)
///
/// 4. **Scope-only** (no query):
///    - Uses `config.retrieval.scoring.scope_only` weights
///    - base = 1.0*(criticality*decay)
///
/// Then: `score = base * scope_multiplier * trust_multiplier`
///
/// Scope multiplier: when scope context is provided,
/// `scope_multiplier = scope_score`. With a `path`, depth decay provides the
/// floor for matching scopes (non-matching → 0.0) and logical adds a bonus of
/// up to 0.3. With logical-only context (no path), the logical bonus rides on
/// `scope_multiplier_floor` (default 0.5): related memories get
/// `floor + bonus`, unscoped memories get the bare floor, and memories whose
/// logical scopes are unrelated get 0.0. When no context is provided,
/// scope_multiplier = 1.0 (neutral).
///
/// Trust multiplier: `trust_floor + (1 - trust_floor) * trust_weight` (default floor=0.5).
///
/// Challenge penalty: if memory.status == Status::Challenged, `score -= challenge_penalty` (default 0.10)
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
    context: &ScoringContext<'_>,
    config: &EngramConfig,
    now: DateTime<Utc>,
) -> ScoreBreakdown {
    composite_score_inner(memory.into(), context, config, now, false)
}

/// Like [`composite_score`] but scores a [`ScoreTarget`] (e.g. built from the
/// LanceDB index projection) instead of a full `Memory`. Produces an identical
/// breakdown for the same field values — the no-query Rank path uses this to
/// avoid loading every candidate's `.md` file.
pub fn composite_score_target(
    target: ScoreTarget<'_>,
    context: &ScoringContext<'_>,
    config: &EngramConfig,
    now: DateTime<Utc>,
) -> ScoreBreakdown {
    composite_score_inner(target, context, config, now, false)
}

/// Like [`composite_score`] but ignores time-based decay when scoring.
///
/// The real decay factor is still recorded in the breakdown for transparency,
/// but `relevance` uses `criticality` directly (as if decay = 1.0).
/// This allows expired memories to be scored by scope and criticality alone,
/// so the relevance threshold still filters out irrelevant results.
pub fn composite_score_ignore_decay(
    memory: &Memory,
    context: &ScoringContext<'_>,
    config: &EngramConfig,
    now: DateTime<Utc>,
) -> ScoreBreakdown {
    composite_score_inner(memory.into(), context, config, now, true)
}

/// [`composite_score_target`] variant that ignores time-based decay (for
/// scoring expired memories by scope + criticality alone).
pub fn composite_score_target_ignore_decay(
    target: ScoreTarget<'_>,
    context: &ScoringContext<'_>,
    config: &EngramConfig,
    now: DateTime<Utc>,
) -> ScoreBreakdown {
    composite_score_inner(target, context, config, now, true)
}

fn composite_score_inner(
    target: ScoreTarget<'_>,
    context: &ScoringContext<'_>,
    config: &EngramConfig,
    now: DateTime<Utc>,
    ignore_decay: bool,
) -> ScoreBreakdown {
    // Calculate decay factor (always computed for the breakdown). Fact
    // freshness anchor (§7.3): facts decay from their last verification when
    // one exists — doctor's `verified_at` writes become score-relevant.
    // Observations/decisions keep `created_at` (re-verifying an observation
    // is modeled by updating it; decisions are premise-bound, not time-bound).
    let decay_anchor = match target.epistemic {
        Epistemic::Fact => target.verified_at.unwrap_or(target.created_at),
        _ => target.created_at,
    };
    let decay_factor_value = super::decay::decay_factor(decay_anchor, now, target.decay);

    // Calculate relevance: when ignoring decay, use criticality directly.
    // (Otherwise `criticality * decay_factor`, i.e. `effective_relevance`.)
    let relevance = if ignore_decay {
        target.criticality
    } else {
        target.criticality * decay_factor_value
    };

    let scope_score = crate::scope::scope_proximity(
        target.physical,
        target.logical,
        context.path,
        context.logical,
        config.retrieval.scoring.depth_decay_base,
        config.retrieval.scoring.depth_decay_floor,
        config.retrieval.scoring.scope_multiplier_floor,
    );

    let trust = trust_weight_from_config(target.provenance_source, &config.trust_weights);

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

    // Apply scope as a post-multiplier.
    // When scope context is provided: multiplier = scope_score directly.
    // (With a path, depth decay provides the floor; with logical-only context,
    // scope_proximity builds on scope_multiplier_floor so the bonus alone
    // never caps the multiplier at 0.3.)
    // When no context: multiplier = 1.0 (neutral, doesn't penalize global searches)
    let has_scope_context = context.path.is_some() || !context.logical.is_empty();
    let scope_multiplier = if has_scope_context { scope_score } else { 1.0 };
    score *= scope_multiplier;

    // Apply trust as a floor-transformed multiplier on the entire base score
    let trust_floor = config.retrieval.scoring.trust_multiplier_floor;
    let trust_multiplier = trust_floor + (1.0 - trust_floor) * trust;
    score *= trust_multiplier;

    // Apply the situation multiplier (§7.1) after trust, before the challenge
    // penalty. Mode-independent post-multiplier; `None` situation (every
    // pre-epistemic query) is exactly neutral. The floor transform and
    // non-finite-to-neutral clamping live in `SituationConfig::multiplier`.
    let situation_multiplier = match context.situation {
        Some(situation) => config
            .retrieval
            .scoring
            .situation
            .multiplier(situation, target.epistemic),
        None => 1.0,
    };
    score *= situation_multiplier;

    // Apply challenge penalty as flat subtraction, sized per epistemic class
    // (§7.2): observation 0.20 / fact 0.15 / decision 0.05 by default; a
    // legacy `Flat` config value applies to all classes.
    if target.status == Status::Challenged {
        score -= config
            .retrieval
            .scoring
            .challenge_penalty
            .penalty_for(target.epistemic);
    }

    // Safety clamp to [0, 1]. `f64::clamp` propagates NaN, and `criticality`
    // is parsed from untrusted files with `f64::parse` (which accepts "NaN" /
    // "inf"), so a non-finite value would otherwise corrupt ranking order.
    // Treat any non-finite score as 0.0 so a malformed memory sinks to the
    // bottom rather than comparing Equal to everything during the sort.
    score = if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    };

    ScoreBreakdown {
        final_score: score,
        semantic: raw_semantic,
        keyword: raw_keyword,
        rerank: None,
        relevance,
        scope: scope_score,
        scope_multiplier,
        trust,
        trust_multiplier,
        situation_multiplier,
        decay: 1.0 - decay_factor_value,
        criticality: target.criticality,
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
            epistemic: MemoryType::Decision.default_epistemic(),
            valid_while: None,
            valid_from: None,
            invalidated_at: None,
            superseded_by: None,
            summary: "Test memory".to_string(),
            title: None,
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
    fn test_composite_score_nan_decay_floor_breakdown_stays_finite() {
        // `Decay.floor` is unvalidated file data: a NaN floor used to flow
        // through `decay_factor` into both `relevance` and the breakdown's
        // `decay` diagnostic. After clamping (NaN → 0.0) the whole breakdown
        // must stay finite.
        let mut memory = create_test_memory();
        memory.created_at = Utc::now() - Duration::days(15);
        memory.decay = Some(Decay::linear(Duration::days(10)).with_floor(f64::NAN));

        let context = ScoringContext::scope_only(None, &[]);
        let config = EngramConfig::default();
        let breakdown = composite_score(&memory, &context, &config, Utc::now());

        assert!(breakdown.final_score.is_finite());
        assert!(breakdown.relevance.is_finite());
        assert!(breakdown.decay.is_finite());
        // Past TTL with NaN→0.0 floor: fully decayed.
        assert_eq!(breakdown.decay, 1.0);
        assert_eq!(breakdown.relevance, 0.0);
    }

    #[test]
    fn test_composite_score_oversized_decay_floor_clamped() {
        // floor = 5.0 on an expired-by-TTL memory must not produce
        // relevance = criticality * 5 (perma-pinned at the clamp).
        let mut memory = create_test_memory();
        memory.created_at = Utc::now() - Duration::days(15);
        memory.decay = Some(Decay::linear(Duration::days(10)).with_floor(5.0));

        let context = ScoringContext::scope_only(None, &[]);
        let config = EngramConfig::default();
        let breakdown = composite_score(&memory, &context, &config, Utc::now());

        // Clamped floor = 1.0 → decay factor 1.0 → relevance = criticality.
        assert_eq!(breakdown.relevance, memory.criticality);
        assert!(breakdown.final_score <= 1.0);
    }

    #[test]
    fn test_composite_score_with_semantic() {
        let memory = create_test_memory();
        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::with_semantic(
            Some("src/api/auth.rs"),
            &logical,
            "oauth authentication",
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
        // base = 0.55*0.9 + 0.45*0.8 = 0.495 + 0.36 = 0.855
        // * scope_mult(1.0) * trust_mult(1.0) = 0.855
        assert!((breakdown.final_score - 0.855).abs() < 0.01);
    }

    #[test]
    fn test_composite_score_degraded() {
        let memory = create_test_memory();
        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::with_query_degraded(
            Some("src/api/auth.rs"),
            &logical,
            "oauth authentication",
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
        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);
        let breakdown_without_challenge =
            composite_score(&create_test_memory(), &context, &config, now);

        // Decision-class memory: per-class default penalty is 0.05.
        assert!(
            (breakdown.final_score - (breakdown_without_challenge.final_score - 0.05)).abs() < 0.01
        );
    }

    #[test]
    fn test_composite_score_with_decay() {
        let mut memory = create_test_memory();
        memory.created_at = Utc::now() - Duration::days(7);
        memory.decay = Some(Decay::exponential(Duration::days(7)));

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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
        let logical = vec!["completely.different.scope".to_string()];
        let context = ScoringContext::scope_only(Some("completely/different/path.rs"), &logical);
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // base = 1.0*0.8 = 0.80
        // * scope_mult(0.0) * trust(1.0) = 0.0
        assert!((breakdown.final_score - 0.0).abs() < 0.01);
        assert!((breakdown.scope - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_composite_score_boundary_max_values() {
        let mut memory = create_test_memory();
        memory.criticality = 1.0;
        memory.confidence = 1.0;

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::with_semantic(
            Some("src/api/auth.rs"),
            &logical,
            "oauth authentication",
            1.0,
        );
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // base = 0.55*1.0 + 0.45*1.0 = 1.0
        // * scope_mult(1.0) * trust_mult(1.0) = 1.0
        assert!((breakdown.final_score - 1.0).abs() < 0.01);
        assert!(breakdown.final_score <= 1.1);
        assert_eq!(breakdown.decay, 0.0); // 0.0 = fresh, no decay
        assert_eq!(breakdown.semantic, Some(1.0));
    }

    #[test]
    fn test_score_breakdown_structure() {
        let memory = create_test_memory();
        let logical = vec!["auth.oauth".to_string()];
        let context =
            ScoringContext::with_semantic(Some("src/api/auth.rs"), &logical, "test query", 0.8);
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
        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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

        // Trust is floor-transformed: effective = 0.5 + 0.5 * raw_trust
        // human: 0.5 + 0.5*1.0 = 1.0, agent: 0.5 + 0.5*0.85 = 0.925
        // imported: 0.5 + 0.5*0.7 = 0.85, inferred: 0.5 + 0.5*0.6 = 0.80
        let human_score = human_breakdown.final_score;
        // base score = 0.80 (criticality), scope_mult = 1.0
        // human: 0.80 * 1.0 = 0.80, agent: 0.80 * 0.925 = 0.74
        let expected_agent = human_score * 0.925;
        let expected_imported = human_score * 0.85;
        let expected_inferred = human_score * 0.80;
        assert!(
            (agent_breakdown.final_score - expected_agent).abs() < 0.001,
            "agent score {} should be {} (human * 0.925)",
            agent_breakdown.final_score,
            expected_agent
        );
        assert!(
            (imported_breakdown.final_score - expected_imported).abs() < 0.001,
            "imported score {} should be {} (human * 0.85)",
            imported_breakdown.final_score,
            expected_imported
        );
        assert!(
            (inferred_breakdown.final_score - expected_inferred).abs() < 0.001,
            "inferred score {} should be {} (human * 0.80)",
            inferred_breakdown.final_score,
            expected_inferred
        );

        // Verify ordering: human > agent > imported > inferred
        assert!(human_score > agent_breakdown.final_score);
        assert!(agent_breakdown.final_score > imported_breakdown.final_score);
        assert!(imported_breakdown.final_score > inferred_breakdown.final_score);
    }

    #[test]
    fn test_trust_multiplier_with_semantic() {
        let config = EngramConfig::default();
        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::with_semantic(
            Some("src/api/auth.rs"),
            &logical,
            "oauth authentication",
            0.9,
        );
        let now = Utc::now();

        let human_memory = create_test_memory();
        let human_score = composite_score(&human_memory, &context, &config, now).final_score;

        let mut inferred_memory = create_test_memory();
        inferred_memory.provenance = Provenance::inferred();
        let inferred_score = composite_score(&inferred_memory, &context, &config, now).final_score;

        // Inferred trust_mult = 0.5 + 0.5*0.6 = 0.80, human = 1.0
        // So inferred should be 0.80x of human
        assert!(
            (inferred_score - human_score * 0.80).abs() < 0.001,
            "with semantic: inferred {} should be {} (human * 0.80)",
            inferred_score,
            human_score * 0.80
        );
    }

    #[test]
    fn test_trust_multiplier_combined_with_challenge() {
        let config = EngramConfig::default();
        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
        let now = Utc::now();

        // Human, active
        let human_score =
            composite_score(&create_test_memory(), &context, &config, now).final_score;

        // Inferred + challenged: score = base * trust_mult(0.80) - 0.05
        // (Decision-class per-class penalty).
        let mut challenged_inferred = create_test_memory();
        challenged_inferred.provenance = Provenance::inferred();
        challenged_inferred.status = Status::Challenged;
        let ci_score = composite_score(&challenged_inferred, &context, &config, now).final_score;

        let expected = human_score * 0.80 - 0.05;
        assert!(
            (ci_score - expected).abs() < 0.001,
            "challenged inferred {} should be {} (human * 0.80 - 0.05)",
            ci_score,
            expected
        );
    }

    #[test]
    fn test_breakdown_keyword_none_for_retrieve() {
        let memory = create_test_memory();
        let logical = vec!["auth.oauth".to_string()];
        let context =
            ScoringContext::with_semantic(Some("src/api/auth.rs"), &logical, "test query", 0.8);
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
        let logical = vec!["auth.oauth".to_string()];
        let context =
            ScoringContext::with_query_degraded(Some("src/api/auth.rs"), &logical, "test query");
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
        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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
        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
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

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
        let config = EngramConfig::default();
        let now = Utc::now();

        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        // base = 1.0*0.8 = 0.80, scope_mult=1.0, trust_mult=1.0
        // challenged penalty (decision class): 0.80 - 0.05 = 0.75
        assert!(
            (ignore.final_score - 0.75).abs() < 0.01,
            "challenged ignore_decay: expected ~0.75, got {}",
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

        let context = ScoringContext::scope_only(Some("completely/different/path.rs"), &[]);
        let config = EngramConfig::default();
        let now = Utc::now();

        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        // relevance = 0.3, base = 1.0*0.3 = 0.3
        // scope_mult = 0.0 (non-matching path) → 0.3*0.0 = 0.0
        // Below default threshold
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

        let logical = vec!["auth.oauth".to_string()];
        let context =
            ScoringContext::with_semantic(Some("src/api/auth.rs"), &logical, "test query", 0.9);
        let config = EngramConfig::default();
        let now = Utc::now();

        let normal = composite_score(&memory, &context, &config, now);
        let ignore = composite_score_ignore_decay(&memory, &context, &config, now);

        // Normal: relevance=0.0 → base = 0.55*0.9 + 0.45*0.0 = 0.495
        // * scope_mult(1.0) * trust_mult(1.0) = 0.495
        // Ignore: relevance=0.8 → base = 0.55*0.9 + 0.45*0.8 = 0.855
        // * scope_mult(1.0) * trust_mult(1.0) = 0.855
        assert!(ignore.final_score > normal.final_score);
        assert!((ignore.final_score - 0.855).abs() < 0.01);
    }

    #[test]
    fn test_composite_score_with_keyword() {
        let memory = create_test_memory();
        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::with_keyword(
            Some("src/api/auth.rs"),
            &logical,
            "authentication",
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
        let logical = vec!["auth.oauth".to_string()];
        let context =
            ScoringContext::with_semantic(Some("src/api/auth.rs"), &logical, "test query", 0.0);
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // with_query weights: 0.55*0.0 + 0.45*0.8 = 0.36
        // * scope_mult(1.0) * trust_mult(1.0) = 0.36
        assert!(
            (breakdown.final_score - 0.36).abs() < 0.01,
            "semantic=0.0 should use with_query weights, got {}",
            breakdown.final_score,
        );
        // Semantic is recorded as Some(0.0) in the breakdown
        assert_eq!(breakdown.semantic, Some(0.0));
    }

    #[test]
    fn test_composite_score_rerank_is_none() {
        let memory = create_test_memory();
        let logical = vec!["auth.oauth".to_string()];
        let context =
            ScoringContext::with_semantic(Some("src/api/auth.rs"), &logical, "test query", 0.8);
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
            let context = ScoringContext::scope_only(Some(path), &[]);
            composite_score(&memory, &context, &config, now).final_score
        };

        let exact = score_for_scope("src/api/auth.rs"); // scope=1.0 → mult=1.0
        let same_dir = score_for_scope("src/api/other.rs"); // scope=0.82 (depth 1) → mult=0.82
        let no_match = score_for_scope("completely/different.rs"); // scope=0.0 → mult=0.0

        assert!(exact > same_dir, "exact {} > same_dir {}", exact, same_dir);
        assert!(
            same_dir > no_match,
            "same_dir {} > no_match {}",
            same_dir,
            no_match
        );

        // Verify the multiplier values
        // exact: base=0.8 * scope(1.0) * trust(1.0) = 0.80
        assert!((exact - 0.8).abs() < 0.01);
        // no_match: base=0.8 * scope(0.0) * trust(1.0) = 0.0
        assert!((no_match - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_semantic_none_vs_zero_differ() {
        let memory = create_test_memory();
        let config = EngramConfig::default();
        let now = Utc::now();

        // sem=None → degraded mode, base = 1.0*0.8 = 0.80
        let logical = vec!["auth.oauth".to_string()];
        let ctx_none =
            ScoringContext::with_query_degraded(Some("src/api/auth.rs"), &logical, "test query");
        let score_none = composite_score(&memory, &ctx_none, &config, now).final_score;

        // sem=Some(0.0) → with_query mode, base = 0.55*0.0 + 0.45*0.8 = 0.36
        let logical = vec!["auth.oauth".to_string()];
        let ctx_zero =
            ScoringContext::with_semantic(Some("src/api/auth.rs"), &logical, "test query", 0.0);
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
    fn test_challenge_is_flat_penalty() {
        let config = EngramConfig::default();
        let now = Utc::now();

        // Human with exact scope match — high base
        let mut high_base = create_test_memory();
        high_base.status = Status::Challenged;
        let logical = vec!["auth.oauth".to_string()];
        let ctx_high = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
        let high_active = composite_score(&create_test_memory(), &ctx_high, &config, now);
        let high_challenged = composite_score(&high_base, &ctx_high, &config, now);
        let high_diff = high_active.final_score - high_challenged.final_score;

        // Inferred with same-dir scope — lower base but still nonzero
        let mut low_base = create_test_memory();
        low_base.provenance = Provenance::inferred();
        low_base.status = Status::Challenged;
        let ctx_low = ScoringContext::scope_only(Some("src/api/other.rs"), &[]);
        let mut low_active = create_test_memory();
        low_active.provenance = Provenance::inferred();
        let low_active_bd = composite_score(&low_active, &ctx_low, &config, now);
        let low_challenged_bd = composite_score(&low_base, &ctx_low, &config, now);
        let low_diff = low_active_bd.final_score - low_challenged_bd.final_score;

        // Both should lose exactly the decision-class penalty (0.05) — the
        // subtraction is flat with respect to base score, only class-sized.
        assert!(
            (high_diff - 0.05).abs() < 0.001,
            "high base challenge diff {} should be 0.05",
            high_diff,
        );
        assert!(
            (low_diff - 0.05).abs() < 0.001,
            "low base challenge diff {} should be 0.05",
            low_diff,
        );
    }

    #[test]
    fn test_trust_floor_prevents_extreme_compounding() {
        let config = EngramConfig::default();
        let now = Utc::now();

        // Worst case with scope match: inferred + root-scoped + challenged, base=1.0
        // Memory has physical=["src/api/auth.rs"], query path is same-dir → scope=0.82
        let mut memory = create_test_memory();
        memory.criticality = 1.0;
        memory.provenance = Provenance::inferred();
        memory.status = Status::Challenged;

        let context = ScoringContext::scope_only(Some("src/api/other.rs"), &[]);

        let breakdown = composite_score(&memory, &context, &config, now);

        // base=1.0 * scope_mult(0.82) * trust_mult(0.5+0.5*0.6=0.80) - 0.10
        // = 1.0 * 0.82 * 0.80 - 0.10 = 0.656 - 0.10 = 0.556
        assert!(
            breakdown.final_score > 0.0,
            "worst case with scope match should be > 0, got {}",
            breakdown.final_score,
        );
        // Trust floor (0.5) prevents inferred from being crushed to near-zero
        assert!(
            breakdown.trust_multiplier >= 0.5,
            "trust multiplier {} should be >= trust floor 0.5",
            breakdown.trust_multiplier,
        );
    }

    #[test]
    fn test_no_semantic_discontinuity() {
        let memory = create_test_memory();
        let config = EngramConfig::default();
        let now = Utc::now();

        let score_at = |sem: f64| {
            let logical = vec!["auth.oauth".to_string()];
            let ctx =
                ScoringContext::with_semantic(Some("src/api/auth.rs"), &logical, "test query", sem);
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
        let context = ScoringContext::with_keyword(None, &[], "auth", 0.8, Some(0.9));
        let breakdown = composite_score(&memory, &context, &config, now);

        // scope_multiplier should be 1.0 (neutral)
        assert!(
            (breakdown.scope_multiplier - 1.0).abs() < f64::EPSILON,
            "scope_multiplier should be 1.0 without context, got {}",
            breakdown.scope_multiplier,
        );
    }

    #[test]
    fn test_challenge_with_keyword_mode() {
        let config = EngramConfig::default();
        let now = Utc::now();

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::with_keyword(
            Some("src/api/auth.rs"),
            &logical,
            "authentication",
            0.7,
            Some(0.9),
        );

        let active_memory = create_test_memory();
        let active_score = composite_score(&active_memory, &context, &config, now).final_score;

        let mut challenged_memory = create_test_memory();
        challenged_memory.status = Status::Challenged;
        let challenged_score =
            composite_score(&challenged_memory, &context, &config, now).final_score;

        // Penalty should be exactly the decision-class 0.05
        assert!(
            (active_score - challenged_score - 0.05).abs() < 0.001,
            "keyword mode challenge diff {} should be 0.05",
            active_score - challenged_score,
        );
    }

    #[test]
    fn test_trust_zero_with_floor() {
        let mut config = EngramConfig::default();
        config.trust_weights.inferred = 0.0;

        let mut memory = create_test_memory();
        memory.provenance = Provenance::inferred();

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // trust_multiplier = floor + (1 - floor) * 0.0 = 0.5
        assert!(
            (breakdown.trust_multiplier - 0.5).abs() < 0.001,
            "trust_multiplier should be 0.5 (floor), got {}",
            breakdown.trust_multiplier,
        );
        // base = 1.0*0.8 = 0.8, scope_mult=1.0, trust_mult=0.5 → 0.4
        assert!(
            (breakdown.final_score - 0.4).abs() < 0.01,
            "score should be ~0.4, got {}",
            breakdown.final_score,
        );
    }

    #[test]
    fn test_logical_only_exact_match_exceeds_default_threshold() {
        // Regression: a rank query with only `logical` context used to use the
        // bare logical bonus (max 0.3) as the multiplier, so every result fell
        // below the default 0.45 relevance threshold.
        let memory = create_test_memory(); // logical = ["auth.oauth"], crit 0.8
        let current_logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(None, &current_logical);
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // scope_mult = 0.5 floor + 0.3 exact bonus = 0.8
        assert!(
            (breakdown.scope_multiplier - 0.8).abs() < 0.001,
            "logical-only exact match multiplier should be 0.8, got {}",
            breakdown.scope_multiplier,
        );
        // base = 1.0*0.8 (criticality) * scope_mult(0.8) * trust(1.0) = 0.64
        assert!((breakdown.final_score - 0.64).abs() < 0.01);
        assert!(
            breakdown.final_score >= config.retrieval.relevance_threshold,
            "exact logical match ({}) must clear the default threshold ({})",
            breakdown.final_score,
            config.retrieval.relevance_threshold,
        );
    }

    #[test]
    fn test_logical_only_partial_match_in_range() {
        // Parent-scope match: floor + 0.2 bonus = 0.7 multiplier.
        let memory = create_test_memory(); // logical = ["auth.oauth"]
        let current_logical = vec!["auth.oauth.google".to_string()];
        let context = ScoringContext::scope_only(None, &current_logical);
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        assert!(
            (breakdown.scope_multiplier - 0.7).abs() < 0.001,
            "parent match multiplier should be 0.7, got {}",
            breakdown.scope_multiplier,
        );
        // 0.8 * 0.7 = 0.56 — above the 0.45 threshold
        assert!((breakdown.final_score - 0.56).abs() < 0.01);
        assert!(breakdown.final_score >= config.retrieval.relevance_threshold);
    }

    #[test]
    fn test_logical_only_mismatch_scores_zero() {
        // Memory declares logical scopes unrelated to the query: suppressed,
        // even at maximum criticality.
        let mut memory = create_test_memory(); // logical = ["auth.oauth"]
        memory.criticality = 1.0;
        let current_logical = vec!["database.postgres".to_string()];
        let context = ScoringContext::scope_only(None, &current_logical);
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        assert_eq!(breakdown.scope_multiplier, 0.0);
        assert!((breakdown.final_score - 0.0).abs() < f64::EPSILON);
        assert!(breakdown.final_score < config.retrieval.relevance_threshold);
    }

    #[test]
    fn test_logical_only_unscoped_memory_gets_neutral_floor() {
        // Memory with no logical scopes under a logical-only query gets the
        // bare floor: it ranks below any related memory, and clears the
        // default threshold only at very high criticality (0.5 * 1.0 = 0.50).
        let mut memory = create_test_memory();
        memory.logical = vec![];
        let current_logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(None, &current_logical);
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        assert!(
            (breakdown.scope_multiplier - 0.5).abs() < 0.001,
            "unscoped memory multiplier should be the 0.5 floor, got {}",
            breakdown.scope_multiplier,
        );
        // 0.8 * 0.5 = 0.40 — below the default 0.45 threshold
        assert!((breakdown.final_score - 0.40).abs() < 0.01);
        assert!(breakdown.final_score < config.retrieval.relevance_threshold);
    }

    #[test]
    fn test_non_finite_criticality_yields_finite_score() {
        // Regression (found by `cargo fuzz run composite_score`): criticality is
        // parsed from untrusted files via `f64::parse`, which accepts "NaN" and
        // "inf". `f64::clamp` propagates NaN, so such a memory used to produce a
        // non-finite final_score and corrupt ranking order. It must now sink to 0.0.
        let logical: Vec<String> = vec![];
        let context = ScoringContext::scope_only(None, &logical);
        let config = EngramConfig::default();
        let now = Utc::now();

        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut memory = create_test_memory();
            memory.criticality = bad;
            let breakdown = composite_score(&memory, &context, &config, now);
            assert!(
                breakdown.final_score.is_finite(),
                "criticality {bad} produced non-finite final_score {}",
                breakdown.final_score,
            );
            assert_eq!(breakdown.final_score, 0.0);
        }
    }

    #[test]
    fn test_challenge_plus_zero_criticality_clamped() {
        let mut memory = create_test_memory();
        memory.criticality = 0.0;
        memory.status = Status::Challenged;

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);
        let config = EngramConfig::default();
        let now = Utc::now();

        let breakdown = composite_score(&memory, &context, &config, now);

        // base=0.0, after challenge penalty: 0.0 - 0.05 = -0.05 → clamped to 0.0
        assert!(
            (breakdown.final_score - 0.0).abs() < f64::EPSILON,
            "zero crit + challenged should clamp to 0.0, got {}",
            breakdown.final_score,
        );
    }

    #[test]
    fn test_custom_trust_floor_changes_scores() {
        let now = Utc::now();

        let mut memory = create_test_memory();
        memory.provenance = Provenance::inferred(); // raw trust = 0.6

        let logical = vec!["auth.oauth".to_string()];
        let context = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical);

        // floor=0.0: trust_multiplier = 0.0 + 1.0*0.6 = 0.6 (raw value)
        let mut config_floor_zero = EngramConfig::default();
        config_floor_zero.retrieval.scoring.trust_multiplier_floor = 0.0;
        let bd_zero = composite_score(&memory, &context, &config_floor_zero, now);
        assert!(
            (bd_zero.trust_multiplier - 0.6).abs() < 0.001,
            "floor=0.0: trust_mult should be 0.6, got {}",
            bd_zero.trust_multiplier,
        );

        // floor=1.0: trust_multiplier = 1.0 + 0.0*0.6 = 1.0 (trust disabled)
        let mut config_floor_one = EngramConfig::default();
        config_floor_one.retrieval.scoring.trust_multiplier_floor = 1.0;
        let bd_one = composite_score(&memory, &context, &config_floor_one, now);
        assert!(
            (bd_one.trust_multiplier - 1.0).abs() < 0.001,
            "floor=1.0: trust_mult should be 1.0, got {}",
            bd_one.trust_multiplier,
        );

        // With floor=1.0, trust is effectively disabled — scores should be higher
        assert!(bd_one.final_score > bd_zero.final_score);
    }
}

#[cfg(test)]
mod epistemic_tests {
    use super::*;
    use crate::types::{Epistemic, MemoryType, Provenance, Situation, Visibility};
    use chrono::Duration;

    fn test_memory() -> Memory {
        let mut m = Memory::new(
            MemoryType::Decision,
            "Test memory",
            "Test content",
            Provenance::human(),
        );
        m.id = "epi-score".into();
        m.criticality = 0.8;
        m.decay = Some(Decay::none());
        m.visibility = Visibility::Shared;
        m
    }

    #[test]
    fn situation_multiplier_applies_after_trust() {
        let config = EngramConfig::default();
        let now = Utc::now();
        let memory = test_memory(); // Decision class

        let neutral_ctx = ScoringContext::scope_only(None, &[]);
        let neutral = composite_score(&memory, &neutral_ctx, &config, now);
        assert_eq!(neutral.situation_multiplier, 1.0);

        // session_start × decision: profile 0.8 → mult = 0.6 + 0.4*0.8 = 0.92
        let ctx =
            ScoringContext::scope_only(None, &[]).with_situation(Some(Situation::SessionStart));
        let bd = composite_score(&memory, &ctx, &config, now);
        assert!((bd.situation_multiplier - 0.92).abs() < 1e-9);
        assert!(
            (bd.final_score - neutral.final_score * 0.92).abs() < 1e-9,
            "situation acts as a pure post-multiplier: {} vs {}",
            bd.final_score,
            neutral.final_score * 0.92
        );

        // file_edit × decision: profile 1.0 → neutral multiplier even with a
        // situation set.
        let ctx = ScoringContext::scope_only(None, &[]).with_situation(Some(Situation::FileEdit));
        let bd = composite_score(&memory, &ctx, &config, now);
        assert!((bd.situation_multiplier - 1.0).abs() < 1e-9);

        // session_start × observation is the strongest default down-weight:
        // 0.6 + 0.4*0.5 = 0.8.
        let mut obs = test_memory();
        obs.epistemic = Epistemic::Observation;
        let ctx =
            ScoringContext::scope_only(None, &[]).with_situation(Some(Situation::SessionStart));
        let bd = composite_score(&obs, &ctx, &config, now);
        assert!((bd.situation_multiplier - 0.8).abs() < 1e-9);
    }

    #[test]
    fn situation_multiplier_identical_across_modes() {
        // The multiplier is mode-independent: the ratio situated/neutral is the
        // same in scope_only, degraded, with_query, and with_keyword modes.
        let config = EngramConfig::default();
        let now = Utc::now();
        let memory = test_memory();
        let logical: Vec<String> = vec![];

        let contexts: Vec<(ScoringContext<'_>, ScoringContext<'_>)> = vec![
            (
                ScoringContext::scope_only(None, &logical),
                ScoringContext::scope_only(None, &logical)
                    .with_situation(Some(Situation::SessionStart)),
            ),
            (
                ScoringContext::with_query_degraded(None, &logical, "q"),
                ScoringContext::with_query_degraded(None, &logical, "q")
                    .with_situation(Some(Situation::SessionStart)),
            ),
            (
                ScoringContext::with_semantic(None, &logical, "q", 0.9),
                ScoringContext::with_semantic(None, &logical, "q", 0.9)
                    .with_situation(Some(Situation::SessionStart)),
            ),
            (
                ScoringContext::with_keyword(None, &logical, "q", 0.7, Some(0.9)),
                ScoringContext::with_keyword(None, &logical, "q", 0.7, Some(0.9))
                    .with_situation(Some(Situation::SessionStart)),
            ),
        ];
        for (neutral_ctx, situated_ctx) in contexts {
            let neutral = composite_score(&memory, &neutral_ctx, &config, now);
            let situated = composite_score(&memory, &situated_ctx, &config, now);
            assert!(
                (situated.final_score - neutral.final_score * 0.92).abs() < 1e-9,
                "mode-independent post-multiplier violated: {} vs {}",
                situated.final_score,
                neutral.final_score * 0.92
            );
        }
    }

    #[test]
    fn per_class_challenge_penalty() {
        let config = EngramConfig::default();
        let now = Utc::now();
        let ctx = ScoringContext::scope_only(None, &[]);

        for (class, expected_penalty) in [
            (Epistemic::Fact, 0.15),
            (Epistemic::Observation, 0.20),
            (Epistemic::Decision, 0.05),
        ] {
            let mut active = test_memory();
            active.epistemic = class;
            let mut challenged = active.clone();
            challenged.status = Status::Challenged;

            let a = composite_score(&active, &ctx, &config, now).final_score;
            let c = composite_score(&challenged, &ctx, &config, now).final_score;
            assert!(
                (a - c - expected_penalty).abs() < 1e-9,
                "{class}: challenge diff {} should be {expected_penalty}",
                a - c
            );
        }
    }

    #[test]
    fn legacy_flat_challenge_penalty_applies_to_all_classes() {
        let mut config = EngramConfig::default();
        config.retrieval.scoring.challenge_penalty = crate::types::ChallengePenalty::Flat(0.10);
        let now = Utc::now();
        let ctx = ScoringContext::scope_only(None, &[]);

        for class in [Epistemic::Fact, Epistemic::Observation, Epistemic::Decision] {
            let mut active = test_memory();
            active.epistemic = class;
            let mut challenged = active.clone();
            challenged.status = Status::Challenged;
            let a = composite_score(&active, &ctx, &config, now).final_score;
            let c = composite_score(&challenged, &ctx, &config, now).final_score;
            assert!((a - c - 0.10).abs() < 1e-9);
        }
    }

    #[test]
    fn fact_freshness_anchor_uses_verified_at() {
        // A decaying fact re-verified recently must score as fresh; the same
        // fact unverified decays from created_at. Non-facts ignore verified_at.
        let config = EngramConfig::default();
        let now = Utc::now();
        let ctx = ScoringContext::scope_only(None, &[]);

        let mut fact = test_memory();
        fact.epistemic = Epistemic::Fact;
        fact.created_at = now - Duration::days(14);
        fact.decay = Some(Decay::exponential(Duration::days(7)));

        let unverified = composite_score(&fact, &ctx, &config, now);

        let mut verified = fact.clone();
        verified.verified_at = Some(now);
        let verified_bd = composite_score(&verified, &ctx, &config, now);

        assert!(
            verified_bd.final_score > unverified.final_score,
            "verification must refresh a decaying fact: {} vs {}",
            verified_bd.final_score,
            unverified.final_score
        );
        assert!(
            (verified_bd.decay - 0.0).abs() < 1e-6,
            "anchored at now ⇒ fresh"
        );

        // Observation with the same verified_at keeps the created_at anchor.
        let mut obs = fact.clone();
        obs.epistemic = Epistemic::Observation;
        obs.verified_at = Some(now);
        let obs_bd = composite_score(&obs, &ctx, &config, now);
        assert!(
            (obs_bd.decay - unverified.decay).abs() < 1e-6,
            "non-fact classes must ignore verified_at"
        );
    }

    #[test]
    fn score_target_parity_with_memory() {
        // §4.2 parity invariant: composite_score(&memory) and
        // composite_score_target(target built from the same field values)
        // produce identical breakdowns — including the new epistemic fields.
        let config = EngramConfig::default();
        let now = Utc::now();

        let mut memory = test_memory();
        memory.epistemic = Epistemic::Fact;
        memory.created_at = now - Duration::days(30);
        memory.verified_at = Some(now - Duration::days(2));
        memory.decay = Some(Decay::exponential(Duration::days(7)));
        memory.status = Status::Challenged;
        memory.physical = vec!["src/api/**".into()];
        memory.logical = vec!["auth".into()];

        let target = ScoreTarget {
            created_at: memory.created_at,
            decay: &memory.decay,
            criticality: memory.criticality,
            physical: &memory.physical,
            logical: &memory.logical,
            provenance_source: memory.provenance.source,
            status: memory.status,
            epistemic: memory.epistemic,
            verified_at: memory.verified_at,
        };

        let logical = vec!["auth".to_string()];
        let ctx = ScoringContext::scope_only(Some("src/api/auth.rs"), &logical)
            .with_situation(Some(Situation::Debugging));

        let from_memory = composite_score(&memory, &ctx, &config, now);
        let from_target = composite_score_target(target, &ctx, &config, now);

        assert_eq!(from_memory.final_score, from_target.final_score);
        assert_eq!(from_memory.relevance, from_target.relevance);
        assert_eq!(from_memory.decay, from_target.decay);
        assert_eq!(
            from_memory.situation_multiplier,
            from_target.situation_multiplier
        );
        assert_eq!(from_memory.trust_multiplier, from_target.trust_multiplier);
        assert_eq!(from_memory.scope_multiplier, from_target.scope_multiplier);
    }
}
