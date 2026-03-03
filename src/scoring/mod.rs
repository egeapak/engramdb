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
//! - **Scope**: `scope_multiplier = floor + (1 - floor) * scope_score` (default floor=0.5).
//!   When no scope context is provided, the multiplier is 1.0 (neutral).
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
