//! Scoring engine for EngramDB
//!
//! This module provides functionality to calculate composite scores for memories
//! based on multiple factors including semantic similarity, keyword match,
//! relevance (criticality * decay), scope proximity, and trust.
//!
//! # Key Components
//!
//! - [`composite_score`]: Main scoring function that combines all factors based on retrieval mode
//! - [`ScoringContext`]: Context for scoring including current scope, query, and semantic score
//! - [`decay_factor`]: Calculates time-based decay using various strategies
//! - [`effective_relevance`]: Combines criticality with decay factor
//! - [`trust_weight`]: Maps provenance source to trust weight
//!
//! # Scoring Modes
//!
//! The composite scoring operates in four modes:
//!
//! 1. **With keyword** (search path): `base = 0.45*keyword + 0.30*semantic + 0.25*relevance`
//! 2. **With query + embeddings**: `base = 0.55*semantic + 0.45*relevance`
//! 3. **Degraded / degraded_keyword** (query, no embeddings): `base = 1.0*relevance`
//! 4. **Scope-only** (no query): `base = 1.0*relevance`
//!
//! Then:
//!
//! ```text
//! score = base * scope_multiplier * trust_multiplier * situation_multiplier
//!              * supports_multiplier * usage_multiplier
//!         - challenge_penalty - contradicts_penalty
//! ```
//!
//! clamped to `[0, 1]` (a non-finite result reads as 0). The two graph terms
//! and the usage term are **off by default** (their knobs are 0, and the
//! engine then attaches no [`ScoreSignals`], so the formula is bit-identical
//! to the one without them).
//!
//! # Post-multipliers
//!
//! - **Scope**: `scope_multiplier = scope_score` whenever scope context is
//!   provided; 1.0 (neutral) when no context is provided. The raw score
//!   depends on the context shape (see `scope::scope_proximity`):
//!   - **Path (± logical)**: depth-decayed physical match (non-matching
//!     scopes → 0.0) plus a logical bonus of up to 0.3, capped at 1.0.
//!   - **Logical-only**: `scope_multiplier_floor + logical_bonus` (default
//!     floor 0.5, capped at 1.0) for related memories, the bare floor for
//!     memories with no logical scopes, 0.0 for memories whose logical scopes
//!     are unrelated. The floor keeps strong logical matches above the
//!     default 0.45 relevance threshold instead of collapsing to the ≤0.3 bonus.
//! - **Trust**: `trust_multiplier = floor + (1 - floor) * trust_weight` (default floor=0.5).
//!   Prevents low-trust memories from being suppressed too aggressively.
//! - **Challenge**: flat subtraction `score -= challenge_penalty` (default 0.10).
//!   Applied uniformly regardless of trust/scope combination.
//! - **Contradicts** (`retrieval.scoring.contradicts_penalty`, default 0):
//!   flat subtraction, once, for a memory with a *confirmed* `contradicts`
//!   edge to a *live* memory (exists, not invalidated, not expired, same
//!   store). Both ends of an undirected contradiction pay it. Proposed edges
//!   never count; a reviewer-chosen direction becomes `supersedes`, whose
//!   target is invalidated and so hidden by the validity filter instead.
//! - **Supports** (`retrieval.scoring.supports_boost`, default 0, ≤ 0.1):
//!   `1 + boost * min(n, 3)` for `n` live confirmed incoming `supports` edges.
//! - **Usage** (`retrieval.scoring.usage_weight` = α, default 0, ≤ 0.2):
//!   `1 + α * min(3, ρ * ln(1 + fetched + 0.3 * injected_sessions))`, with the
//!   exposure ratio `ρ = min(1, (fetched + 1) / (injected + returned + 1))` so a memory
//!   that is injected but never opened cannot reward itself (see
//!   [`usage_term`]).
//! - **Expansion** (`expand_links`) is not a scoring term: an expanded
//!   neighbour's score is its hit's score × `expand_links_decay` × its own
//!   situation multiplier, minus its own challenge and `contradicts`
//!   penalties (or its own query score when higher), and its breakdown — its
//!   own, so a contradiction stays visible — carries [`LinkVia`].
//! - Decay strategies include None, Linear, Exponential, and Step functions.
//! - Final score is clamped to [0.0, 1.0].
mod composite;
mod decay;
mod trust;

// Re-export public API
pub use composite::{
    composite_score, composite_score_ignore_decay, composite_score_target,
    composite_score_target_ignore_decay, supports_multiplier, usage_multiplier, usage_term,
    LinkVia, ScoreBreakdown, ScoreSignals, ScoreTarget, ScoringContext, UsageSignal,
    INJECTED_SESSION_WEIGHT, SUPPORTS_COUNTED_MAX, USAGE_TERM_CAP,
};
pub use decay::{decay_factor, effective_relevance};
pub use trust::{trust_weight, trust_weight_from_config};
