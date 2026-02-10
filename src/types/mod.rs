/// Core data model types for EngramDB
mod challenge;
pub mod config;
mod decay;
mod memory;
mod provenance;

// Re-export all public types
pub use challenge::Challenge;
pub use config::{
    EngramConfig, LogicalBonusConfig, RetrievalConfig, ScopeProximityConfig, ScoringConfig,
    ScoringWeights, ThresholdsConfig, TrustWeights,
};
pub use decay::{Decay, DecayStrategy};
pub use memory::{Memory, MemoryType, MemoryUpdate, Status, Visibility};
pub use provenance::{Provenance, ProvenanceSource};
