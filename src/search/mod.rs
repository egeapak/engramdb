//! Keyword search functionality for EngramDB
//!
//! This module provides TF-IDF style keyword search as a fallback when embeddings
//! are unavailable or for quick filtering operations.
//!
//! # Key Components
//!
//! - [`keyword_search`]: Main search function using weighted term matching
//!
//! # Algorithm
//!
//! The search uses a weighted term frequency approach:
//! - Summary matches: 3x weight
//! - Tag matches: 2x weight
//! - Content matches: 1x weight
//!
//! Scores are normalized to [0.0, 1.0] range and results are sorted by descending score.
//!
//! # Relation to Other Modules
//!
//! This module is used in degraded mode when embeddings are not available, as specified
//! in the [`crate::scoring`] module. It provides a fallback search capability that doesn't
//! require semantic understanding.

pub mod keyword;

// Re-export main functions
pub use keyword::{keyword_search, normalize_keyword_score, query_token_count};
