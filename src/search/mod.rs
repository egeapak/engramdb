//! Keyword search functionality for EngramDB
//!
//! This module provides keyword search, used alongside semantic similarity and
//! as the sole text signal when embeddings are unavailable.
//!
//! # Key Components
//!
//! - [`normalize`]: the one tokenization pipeline, applied to query and
//!   document text alike
//! - [`keyword_search`]: main search function using weighted term matching
//!
//! # Algorithm
//!
//! Both sides are reduced to deduplicated stems ([`normalize`]), then each
//! query term scores once per field it appears in:
//! - Summary matches: 3x weight
//! - Tag matches: 2x weight
//! - Content matches: 1x weight
//!
//! Matching is per distinct term, not per occurrence — repetition never
//! inflates a score. Results are sorted by descending raw score;
//! [`normalize_keyword_score`] maps that to [0.0, 1.0] for the composite
//! formula, centring its sigmoid on the number of scoreable query terms.
//!
//! # Relation to Other Modules
//!
//! This module is used in degraded mode when embeddings are not available, as specified
//! in the [`crate::scoring`] module. It provides a fallback search capability that doesn't
//! require semantic understanding.

pub mod keyword;

/// Text normalization, re-exported from `engram-types`.
///
/// The implementation lives one layer down because the storage crate
/// precomputes stems on the write path and must produce byte-identical output
/// to what this crate computes at query time — and storage cannot depend
/// upward on the core. Re-exporting keeps `crate::search::normalize::*`
/// resolving unchanged.
pub use engram_types::normalize;

// Re-export main functions
pub use keyword::{
    keyword_search, keyword_search_stems, normalize_keyword_score, query_token_count,
};
pub use normalize::{normalize_counts, normalize_set, NORMALIZER_STAMP};
