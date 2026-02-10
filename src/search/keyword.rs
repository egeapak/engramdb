//! Keyword-based search for memories

use crate::types::Memory;

/// Perform keyword search on a collection of memories
///
/// Returns a vector of (index, score) tuples where:
/// - index: the index into the memories slice
/// - score: relevance score from 0.0 to 1.0
///
/// # Algorithm
/// 1. Tokenize query into lowercase words
/// 2. For each memory, tokenize summary, content, and tags
/// 3. Count weighted matches:
///    - Summary match: 3x weight
///    - Tag match: 2x weight
///    - Content match: 1x weight
/// 4. Score = weighted_matches / (total_query_tokens * 3) to normalize to [0, 1]
/// 5. Filter out zero scores and sort by score descending
///
/// # Arguments
/// * `query` - The search query string
/// * `memories` - Slice of memories to search
///
/// # Returns
/// Vector of (index, relevance_score) tuples, sorted by score descending
pub fn keyword_search(query: &str, memories: &[Memory]) -> Vec<(usize, f64)> {
    // Tokenize query into lowercase words
    let query_tokens: Vec<String> = tokenize(query);

    if query_tokens.is_empty() {
        return vec![];
    }

    let mut results: Vec<(usize, f64)> = memories
        .iter()
        .enumerate()
        .filter_map(|(idx, memory)| {
            let score = calculate_keyword_score(&query_tokens, memory);
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

/// Calculate keyword match score for a single memory.
///
/// Weights:
/// - Summary match: 3x
/// - Tag match: 2x
/// - Content match: 1x
///
/// Score is normalized to [0.0, 1.0] based on maximum possible weighted matches.
fn calculate_keyword_score(query_tokens: &[String], memory: &Memory) -> f64 {
    // Tokenize memory fields
    let summary_tokens = tokenize(&memory.summary);
    let content_tokens = tokenize(&memory.content);
    let tag_tokens: Vec<String> = memory.tags.iter().flat_map(|tag| tokenize(tag)).collect();

    let mut weighted_matches = 0.0;

    for token in query_tokens {
        // Check summary (3x weight)
        if summary_tokens.contains(token) {
            weighted_matches += 3.0;
        }

        // Check tags (2x weight)
        if tag_tokens.contains(token) {
            weighted_matches += 2.0;
        }

        // Check content (1x weight)
        if content_tokens.contains(token) {
            weighted_matches += 1.0;
        }
    }

    // Normalize to [0, 1] range
    // Maximum possible score is query_tokens.len() * 3.0 (if all tokens match in summary)
    let max_score = query_tokens.len() as f64 * 3.0;
    if max_score > 0.0 {
        (weighted_matches / max_score).min(1.0)
    } else {
        0.0
    }
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

        // Memory 1 should score highest (summary match = 3x weight)
        assert_eq!(results[0].0, 0);

        // Memory 3 should score higher than memory 2 (tag 2x > content 1x)
        assert_eq!(results[1].0, 2);
        assert_eq!(results[2].0, 1);
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
}
