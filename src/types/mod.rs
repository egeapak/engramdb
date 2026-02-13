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
mod memory;
mod provenance;

// Re-export all public types
pub use challenge::Challenge;
pub use config::{
    EmbeddingBackend, EmbeddingsConfig, EngramConfig, LogicalBonusConfig, NliConfig, RerankConfig,
    RetrievalConfig, ScopeProximityConfig, ScoringConfig, ScoringWeights, SearchConfig,
    ThresholdsConfig, TrustWeights,
};
pub use decay::{Decay, DecayStrategy};
pub use memory::{Memory, MemoryType, MemoryUpdate, Status, Visibility};
pub use provenance::{Provenance, ProvenanceSource};
