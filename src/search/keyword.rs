//! Keyword-based search for memories

use std::borrow::Borrow;
use std::collections::HashSet;

use crate::types::Memory;

/// Perform keyword search on a collection of memories
///
/// Returns a vector of (index, score) tuples where:
/// - index: the index into the memories slice
/// - score: raw weighted match score (unbounded)
///
/// # Algorithm
/// 1. Tokenize query into lowercase words
/// 2. For each memory, tokenize summary, content, and tags
/// 3. Count weighted matches:
///    - Summary match: 3x weight
///    - Tag match: 2x weight
///    - Content match: 1x weight
/// 4. Score = raw weighted_matches (no normalization)
/// 5. Filter out zero scores and sort by score descending
///
/// # Arguments
/// * `query` - The search query string
/// * `memories` - Slice of memories (or references) to search
///
/// # Returns
/// Vector of (index, relevance_score) tuples, sorted by score descending
pub fn keyword_search<M: Borrow<Memory>>(query: &str, memories: &[M]) -> Vec<(usize, f64)> {
    // Tokenize query into lowercase words
    let query_tokens: Vec<String> = tokenize(query);

    if query_tokens.is_empty() {
        return vec![];
    }

    let mut results: Vec<(usize, f64)> = memories
        .iter()
        .enumerate()
        .filter_map(|(idx, memory)| {
            let score = calculate_keyword_score(&query_tokens, memory.borrow());
            if score > 0.0 {
                Some((idx, score))
            } else {
                None
            }
        })
        .collect();

    // Sort by score descending
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    results
}

/// Normalize a raw keyword score to [0, 1] using a shifted sigmoid.
///
/// The midpoint scales with query length: `k = 3.0 * num_query_tokens`,
/// meaning "every query token matching the summary field" sits at 0.50.
/// All tokens matching all 3 fields (raw = 6 × N) maps to ~0.98.
///
/// Steepness is `k / 4.0`, keeping the same curve shape at any scale.
///
/// Properties (1 token):
/// - raw=0 → ~0.02, raw=1 → ~0.07, raw=3 → 0.50, raw=6 → ~0.98
///
/// Properties (3 tokens):
/// - raw=0 → ~0.02, raw=3 → ~0.07, raw=9 → 0.50, raw=18 → ~0.98
///
/// Monotone, batch-independent, bounded [0, 1].
pub fn normalize_keyword_score(raw: f64, num_query_tokens: usize) -> f64 {
    let n = (num_query_tokens as f64).max(1.0);
    let k = 3.0 * n;
    let steepness = k / 4.0;
    1.0 / (1.0 + (-(raw - k) / steepness).exp())
}

/// Calculate keyword match score for a single memory.
///
/// Weights:
/// - Summary match: 3x
/// - Tag match: 2x
/// - Content match: 1x
///
/// Returns raw weighted matches (unbounded). No normalization is applied.
///
/// Optimized to avoid per-token `String` allocations: lowercases the full
/// text once, then splits into `&str` slices of the lowered buffer.
fn calculate_keyword_score(query_tokens: &[String], memory: &Memory) -> f64 {
    let summary_lower = memory.summary.to_lowercase();
    let content_lower = memory.content.to_lowercase();

    let summary_tokens: HashSet<&str> = split_tokens(&summary_lower).collect();
    let content_tokens: HashSet<&str> = split_tokens(&content_lower).collect();
    let tag_lowers: Vec<String> = memory.tags.iter().map(|t| t.to_lowercase()).collect();
    let tag_tokens: HashSet<&str> = tag_lowers.iter().flat_map(|t| split_tokens(t)).collect();

    let mut weighted_matches = 0.0;

    for token in query_tokens {
        if summary_tokens.contains(token.as_str()) {
            weighted_matches += 3.0;
        }

        if tag_tokens.contains(token.as_str()) {
            weighted_matches += 2.0;
        }

        if content_tokens.contains(token.as_str()) {
            weighted_matches += 1.0;
        }
    }

    weighted_matches
}

/// Split lowercased text into non-empty alphanumeric token slices.
fn split_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
}

/// Count the number of tokens in a query string.
///
/// Uses the same tokenization logic as `keyword_search` so that
/// `num_query_tokens` is consistent with the actual tokens scored.
pub fn query_token_count(query: &str) -> usize {
    let lower = query.to_lowercase();
    split_tokens(&lower).count()
}

/// Tokenize a string into lowercase words.
///
/// Splits on non-alphanumeric characters and filters out empty strings.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MemoryType, Provenance, ProvenanceSource, Status, Visibility};
    use chrono::Utc;

    fn create_test_memory(id: &str, summary: &str, content: &str, tags: Vec<String>) -> Memory {
        Memory {
            id: id.to_string(),
            type_: MemoryType::Decision,
            summary: summary.to_string(),
            title: None,
            content: content.to_string(),
            details: None,
            physical: vec!["/".to_string()],
            logical: vec![],
            tags,
            criticality: 0.5,
            decay: None,
            provenance: Provenance::new(ProvenanceSource::Human),
            confidence: 0.8,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            challenges: vec![],
            verified_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            accessed_at: Utc::now(),
            expires_at: None,
        }
    }

    #[test]
    fn test_tokenize() {
        let tokens = tokenize("Hello, World! This is a test.");
        assert_eq!(tokens, vec!["hello", "world", "this", "is", "a", "test"]);
    }

    #[test]
    fn test_tokenize_empty() {
        let tokens = tokenize("");
        assert_eq!(tokens.len(), 0);
    }

    #[test]
    fn test_keyword_search_summary_match() {
        let memories = vec![
            create_test_memory("1", "Authentication system", "Details about auth", vec![]),
            create_test_memory("2", "Database design", "Details about database", vec![]),
        ];

        let results = keyword_search("authentication", &memories);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 0); // First memory
        assert!(results[0].1 > 0.0);
    }

    #[test]
    fn test_keyword_search_content_match() {
        let memories = vec![
            create_test_memory("1", "System design", "Uses PostgreSQL database", vec![]),
            create_test_memory("2", "API design", "Uses REST principles", vec![]),
        ];

        let results = keyword_search("postgresql", &memories);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 0);
    }

    #[test]
    fn test_keyword_search_tag_match() {
        let memories = vec![
            create_test_memory(
                "1",
                "System design",
                "Details",
                vec!["auth".to_string(), "security".to_string()],
            ),
            create_test_memory("2", "API design", "Details", vec!["rest".to_string()]),
        ];

        let results = keyword_search("security", &memories);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 0);
    }

    #[test]
    fn test_keyword_search_weighted_scoring() {
        let memories = vec![
            create_test_memory(
                "1",
                "auth system design",
                "Details about the system",
                vec![],
            ),
            create_test_memory(
                "2",
                "System overview",
                "Details about auth implementation",
                vec![],
            ),
            create_test_memory(
                "3",
                "API design",
                "System details",
                vec!["auth".to_string()],
            ),
        ];

        let results = keyword_search("auth", &memories);
        assert_eq!(results.len(), 3);

        // Memory 1 should score highest (summary match = 3.0)
        assert_eq!(results[0].0, 0);
        // Memory 3 should score higher than memory 2 (tag 2.0 > content 1.0)
        assert_eq!(results[1].0, 2);
        assert_eq!(results[2].0, 1);
        // Ordering: summary > tag > content
        assert!(results[0].1 > results[1].1);
        assert!(results[1].1 > results[2].1);
    }

    #[test]
    fn test_keyword_search_multiple_tokens() {
        let memories = vec![
            create_test_memory(
                "1",
                "Authentication and authorization",
                "Security details",
                vec![],
            ),
            create_test_memory("2", "Database design", "Authentication mechanisms", vec![]),
            create_test_memory("3", "API design", "REST principles", vec![]),
        ];

        let results = keyword_search("authentication authorization", &memories);

        // Memory 1 should score highest (both tokens in summary)
        assert!(!results.is_empty());
        assert_eq!(results[0].0, 0);
    }

    #[test]
    fn test_keyword_search_no_match() {
        let memories = vec![
            create_test_memory("1", "Database design", "PostgreSQL details", vec![]),
            create_test_memory("2", "API design", "REST principles", vec![]),
        ];

        let results = keyword_search("authentication", &memories);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_keyword_search_empty_query() {
        let memories = vec![create_test_memory("1", "Test memory", "Content", vec![])];

        let results = keyword_search("", &memories);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_keyword_search_case_insensitive() {
        let memories = vec![create_test_memory(
            "1",
            "Authentication System",
            "Details",
            vec![],
        )];

        let results = keyword_search("AUTHENTICATION", &memories);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_keyword_search_sorted_by_score() {
        let memories = vec![
            create_test_memory("1", "System design", "auth details", vec![]),
            create_test_memory("2", "auth system", "Details", vec![]),
            create_test_memory("3", "Design", "Details", vec!["auth".to_string()]),
        ];

        let results = keyword_search("auth", &memories);

        // Should be sorted by score descending
        for i in 1..results.len() {
            assert!(results[i - 1].1 >= results[i].1);
        }
    }

    #[test]
    fn test_keyword_score_unbounded_multi_token() {
        // A memory matching many tokens across summary+tag+content should exceed 1.0
        let memories = vec![create_test_memory(
            "1",
            "auth password hashing",
            "auth password hashing bcrypt",
            vec!["auth".to_string(), "password".to_string()],
        )];

        let results = keyword_search("auth password hashing", &memories);
        assert_eq!(results.len(), 1);

        // Raw scores are unbounded (multi-token accumulates across fields)
        // "auth": summary(3)+content(1)+tag(2)=6, "password": summary(3)+content(1)+tag(2)=6,
        // "hashing": summary(3)+content(1)=4 → total=16
        assert!(
            results[0].1 > 10.0,
            "multi-token multi-field raw score should be > 10.0, got {}",
            results[0].1
        );
    }

    #[test]
    fn test_keyword_single_token_all_fields_match() {
        // Single token matching summary + tag + content should score higher than partial matches
        let memories = vec![create_test_memory(
            "1",
            "security review",
            "security details",
            vec!["security".to_string()],
        )];

        let results = keyword_search("security", &memories);
        assert_eq!(results.len(), 1);
        // All three fields match: summary(3) + tag(2) + content(1) = 6.0
        assert!(
            results[0].1 > 5.0,
            "all-fields match should be > 5.0, got {}",
            results[0].1
        );
    }

    #[test]
    fn test_keyword_tag_content_vs_summary_collision() {
        // tag(2) + content(1) = 3.0, which equals a summary-only match (3.0).
        // This documents the known collision — both score the same at raw=3.0.
        let tag_content = vec![create_test_memory(
            "1",
            "unrelated summary",
            "auth details",
            vec!["auth".to_string()],
        )];
        let summary_only = vec![create_test_memory(
            "2",
            "auth system",
            "unrelated content",
            vec![],
        )];

        let tc_results = keyword_search("auth", &tag_content);
        let so_results = keyword_search("auth", &summary_only);

        assert_eq!(tc_results.len(), 1);
        assert_eq!(so_results.len(), 1);
        assert!(
            (tc_results[0].1 - so_results[0].1).abs() < f64::EPSILON,
            "tag+content ({}) should equal summary-only ({})",
            tc_results[0].1,
            so_results[0].1
        );
        assert!((tc_results[0].1 - 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_normalize_keyword_score_range() {
        // Output should always be in [0, 1] regardless of num_tokens
        for num_tokens in [1, 2, 3, 5] {
            for raw in [0.0, 1.0, 3.0, 6.0, 12.0, 18.0, 50.0, 100.0] {
                let norm = normalize_keyword_score(raw, num_tokens);
                assert!(
                    (0.0..=1.0).contains(&norm),
                    "normalize({}, {}) = {} not in [0, 1]",
                    raw,
                    num_tokens,
                    norm
                );
            }
        }
        // 1-token midpoint: raw=3 (summary match) → 0.50
        assert!((normalize_keyword_score(3.0, 1) - 0.5).abs() < 0.01);
        // 1-token all fields: raw=6 → ~0.98
        assert!(normalize_keyword_score(6.0, 1) > 0.95);
        // 3-token midpoint: raw=9 (all tokens match summary) → 0.50
        assert!((normalize_keyword_score(9.0, 3) - 0.5).abs() < 0.01);
        // 3-token all fields: raw=18 → ~0.98
        assert!(normalize_keyword_score(18.0, 3) > 0.95);
        // Low raw → low normalized
        assert!(normalize_keyword_score(0.0, 1) < 0.05);
    }

    #[test]
    fn test_normalize_keyword_score_monotone() {
        let values: Vec<f64> = vec![0.0, 1.0, 2.0, 4.0, 6.0, 10.0, 15.0, 20.0];
        for num_tokens in [1, 3] {
            let normalized: Vec<f64> = values
                .iter()
                .map(|&v| normalize_keyword_score(v, num_tokens))
                .collect();
            for i in 1..normalized.len() {
                assert!(
                    normalized[i] > normalized[i - 1],
                    "not monotone at num_tokens={}: normalize({}) = {} <= normalize({}) = {}",
                    num_tokens,
                    values[i],
                    normalized[i],
                    values[i - 1],
                    normalized[i - 1]
                );
            }
        }
    }

    #[test]
    fn test_normalize_keyword_score_scales_with_query_length() {
        // Same per-token match quality should give same normalized score
        // 1 token, summary match: raw=3
        let one_token_summary = normalize_keyword_score(3.0, 1);
        // 3 tokens, all match summary: raw=9
        let three_tokens_summary = normalize_keyword_score(9.0, 3);
        assert!(
            (one_token_summary - three_tokens_summary).abs() < 0.01,
            "same per-token quality should give same score: 1t={}, 3t={}",
            one_token_summary,
            three_tokens_summary
        );

        // 1 token, all fields: raw=6
        let one_token_all = normalize_keyword_score(6.0, 1);
        // 3 tokens, all matching all fields: raw=18
        let three_tokens_all = normalize_keyword_score(18.0, 3);
        assert!(
            (one_token_all - three_tokens_all).abs() < 0.01,
            "same per-token quality should give same score: 1t={}, 3t={}",
            one_token_all,
            three_tokens_all
        );

        // 3-token query with only 1 token matching summary (raw=3)
        // should score much lower than 1-token query matching summary (raw=3)
        let partial_3t = normalize_keyword_score(3.0, 3);
        assert!(
            partial_3t < one_token_summary * 0.5,
            "1/3 tokens matching should score much lower: partial={}, full={}",
            partial_3t,
            one_token_summary
        );
    }
}
