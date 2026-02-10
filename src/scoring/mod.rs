/// Scoring engine for EngramDB
///
/// This module provides functionality to calculate composite scores for memories
/// based on multiple factors including semantic similarity, relevance (criticality + decay),
/// scope proximity, and trust/provenance.

mod composite;
mod decay;
mod trust;

// Re-export public API
pub use composite::{composite_score, ScoringContext};
pub use decay::{decay_factor, effective_relevance};
pub use trust::{trust_weight, trust_weight_from_config};
