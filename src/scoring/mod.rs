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
//! Then: `score = base * scope_multiplier * trust_multiplier - challenge_penalty`
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
//! - Decay strategies include None, Linear, Exponential, and Step functions.
//! - Final score is clamped to [0.0, 1.0].
mod composite;
mod decay;
mod trust;

// Re-export public API
pub use composite::{
    composite_score, composite_score_ignore_decay, ScoreBreakdown, ScoringContext,
};
pub use decay::{decay_factor, effective_relevance};
pub use trust::{trust_weight, trust_weight_from_config};
