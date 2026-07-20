//! Core data model types for EngramDB.
//!
//! This module defines all the fundamental types used throughout EngramDB:
//! - [`Memory`] - the core memory struct with metadata, content, and scope
//! - [`MemoryType`] - categorization of memories (Decision, Hazard, Intent, etc.)
//! - [`Decay`] - time-based relevance decay strategies
//! - [`Provenance`] - source tracking (human, agent, inferred, imported)
//! - [`Challenge`] - validity challenges to memories
//! - [`EngramConfig`] - all configuration for scoring, retrieval, and thresholds
//!
//! These types form the foundation of the memory storage and retrieval system,
//! connecting the storage layer to scoring and retrieval algorithms.

mod challenge;
pub mod config;
mod decay;
pub mod env;
mod epistemic;
mod memory;
mod provenance;
mod title_strategy;

// Re-export all public types
pub use challenge::Challenge;
pub use config::{
    ChallengePenalty, DaemonConfig, EmbeddingBackend, EmbeddingsConfig, EngramConfig,
    EpistemicConfig, HooksConfig, LogicalBonusConfig, MaintenanceConfig, NliConfig,
    ReindexOnModelChange, RerankConfig, RetrievalConfig, ScopeProximityConfig, ScoringConfig,
    ScoringWeights, SearchConfig, SituationConfig, SituationProfile, ThresholdsConfig,
    TrustWeights, CONTENT_SOFT_TOKEN_TARGET, DEFAULT_NLI_MODEL_REPO, MAX_SUMMARY_CHARS,
};
pub use decay::{Decay, DecayStrategy};
pub use env::in_process_override;
pub use epistemic::{Epistemic, Generality, Situation, Validity};
pub use memory::{default_decay, Memory, MemoryType, MemoryUpdate, Status, Visibility};
pub use provenance::{Provenance, ProvenanceSource};
pub use title_strategy::TitleStrategy;
