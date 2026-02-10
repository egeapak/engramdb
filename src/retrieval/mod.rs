//! Retrieval engine and filtering for EngramDB

pub mod engine;
pub mod filters;

// Re-export main types and functions
pub use engine::{RetrievalEngine, RetrievalQuery, RetrievalResult, ScoredMemory, DetailLevel};
pub use filters::{SearchFilters, apply_index_filters};
