//! Memory retrieval engine and filtering.
//!
//! This module provides the core retrieval functionality for EngramDB, combining
//! scope-based filtering, semantic search (via embeddings), and relevance scoring
//! to find the most contextually relevant memories.
//!
//! # Key Components
//!
//! - [`RetrievalEngine`]: Main retrieval coordinator that orchestrates filtering,
//!   scoring, and ranking of memories based on query context.
//! - [`RetrievalQuery`]: Query parameters including scope, filters, and search text.
//! - [`SearchFilters`]: Filter criteria for narrowing down memories by type, tags,
//!   scope, and criticality.
//! - [`DetailLevel`]: Controls how much information is returned (summary, content, or full).
//!
//! # How It Works
//!
//! 1. **Filtering**: Apply index-level filters (type, tags, scope, criticality) to
//!    narrow the candidate set before loading full memories.
//! 2. **Scoring**: Calculate composite relevance scores using scope matching, semantic
//!    similarity (if embeddings available), and memory metadata (criticality, age, etc.).
//! 3. **Ranking**: Sort by score and return top N results based on configured thresholds.
//!
//! # Integration with Other Modules
//!
//! - Uses `storage::MemoryStore` to load memories from disk.
//! - Uses `scoring::composite_score` to rank memories by relevance.
//! - Uses `embeddings::EmbeddingProvider` for semantic search (optional); vectors stored in LanceDB via `MemoryStore`.
//! - Uses `scope::physical` for file path pattern matching.

pub mod engine;
pub mod filters;
pub mod reranker;

// Re-export main types and functions
pub use engine::{DetailLevel, RetrievalEngine, RetrievalQuery, RetrievalResult, ScoredMemory};
pub use filters::{apply_index_filters, build_filter_predicate, Filterable, SearchFilters};
#[cfg(feature = "onnxruntime")]
pub use reranker::LocalReranker;
pub use reranker::{RerankScore, Reranker};
