//! Retrieval engine and filtering for EngramDB

pub mod engine;
pub mod filters;

// Re-export main types and functions
pub use engine::{DetailLevel, RetrievalEngine, RetrievalQuery, RetrievalResult, ScoredMemory};
pub use filters::{apply_index_filters, SearchFilters};
