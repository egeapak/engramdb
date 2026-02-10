//! Scoring engine for EngramDB
//!
//! This module provides functionality to calculate composite scores for memories
//! based on multiple factors including semantic similarity, relevance (criticality + decay),
//! scope proximity, and trust/provenance.
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
//! The composite scoring operates in three modes:
//!
//! 1. **With query + embeddings**: Uses semantic similarity along with other factors
//! 2. **With query, no embeddings** (degraded): Uses keyword search with adjusted weights
//! 3. **Scope-only**: Uses only scope proximity, relevance, and trust
//!
//! # Design Decisions
//!
//! - Challenged memories receive a 30% penalty to their final score
//! - All scores are normalized to [0.0, 1.0] range (though can exceed 1.0 in edge cases)
//! - Decay strategies include None, Linear, Exponential, and Step functions
//! - Trust weights vary by provenance source to reflect confidence in the information
mod composite;
mod decay;
mod trust;

// Re-export public API
pub use composite::{composite_score, ScoreBreakdown, ScoringContext};
pub use decay::{decay_factor, effective_relevance};
pub use trust::{trust_weight, trust_weight_from_config};
