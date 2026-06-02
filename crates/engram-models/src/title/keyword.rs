//! Keyword extraction-based title generator using RAKE algorithm.
//!
//! Extracts the top key phrases from the input text and joins them
//! into a short title. Lightweight — no model download required.

use super::TitleGenerator;
use anyhow::Result;
use async_trait::async_trait;

/// Stop words for English RAKE extraction.
///
/// A minimal set covering the most common English stop words.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "and", "or", "but", "in", "on", "at", "to", "for", "of", "with", "by",
    "from", "is", "it", "as", "be", "was", "were", "been", "are", "this", "that", "these", "those",
    "i", "we", "you", "he", "she", "they", "me", "us", "him", "her", "them", "my", "our", "your",
    "his", "its", "their", "what", "which", "who", "whom", "when", "where", "why", "how", "all",
    "each", "every", "both", "few", "more", "most", "other", "some", "such", "no", "not", "only",
    "same", "so", "than", "too", "very", "can", "will", "just", "should", "now", "do", "did",
    "does", "done", "had", "has", "have", "if", "then", "else", "about", "up", "out", "into",
    "over", "after", "before", "between", "under", "above", "through", "during", "would", "could",
    "may", "might", "must", "shall", "also", "any", "here", "there",
];

/// Title generator based on keyword extraction (RAKE algorithm).
///
/// Extracts key phrases from text and combines the top ones into a title.
/// This is a lightweight approach that requires no ML model downloads.
pub struct KeywordTitleGenerator {
    max_words: usize,
}

impl Default for KeywordTitleGenerator {
    fn default() -> Self {
        Self { max_words: 4 }
    }
}

impl KeywordTitleGenerator {
    /// Create a new keyword-based title generator.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Simple RAKE-inspired keyword extraction.
///
/// Splits text into candidate phrases at stop word boundaries,
/// scores each phrase by the sum of word degrees divided by frequencies,
/// and returns the top-scored phrases.
fn extract_keywords(text: &str, max_phrases: usize) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }

    let stop_set: std::collections::HashSet<&str> = STOP_WORDS.iter().copied().collect();

    // Tokenize: lowercase, keep only alphanumeric words
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();

    // Build candidate phrases: sequences of non-stop words
    let mut phrases: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();

    for word in &words {
        let lower = word.to_lowercase();
        if stop_set.contains(lower.as_str()) || lower.len() <= 1 {
            if !current.is_empty() {
                phrases.push(current.clone());
                current.clear();
            }
        } else {
            current.push(lower);
        }
    }
    if !current.is_empty() {
        phrases.push(current);
    }

    if phrases.is_empty() {
        return Vec::new();
    }

    // Calculate word frequency and degree
    let mut freq: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    let mut degree: std::collections::HashMap<String, f64> = std::collections::HashMap::new();

    for phrase in &phrases {
        let len = phrase.len() as f64;
        for word in phrase {
            *freq.entry(word.clone()).or_default() += 1.0;
            *degree.entry(word.clone()).or_default() += len - 1.0;
        }
    }

    // Word score = (degree + freq) / freq
    let mut word_score: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for (word, f) in &freq {
        let d = degree.get(word).unwrap_or(&0.0);
        word_score.insert(word.clone(), (d + f) / f);
    }

    // Phrase score = sum of word scores
    let mut scored: Vec<(String, f64)> = phrases
        .iter()
        .map(|phrase| {
            let score: f64 = phrase
                .iter()
                .map(|w| word_score.get(w).unwrap_or(&0.0))
                .sum();
            let text = phrase.join(" ");
            (text, score)
        })
        .collect();

    // Deduplicate phrases
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut seen = std::collections::HashSet::new();
    scored.retain(|(text, _)| seen.insert(text.clone()));

    scored
        .into_iter()
        .take(max_phrases)
        .map(|(text, _)| text)
        .collect()
}

/// Capitalize each word in a phrase for title case.
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[async_trait]
impl TitleGenerator for KeywordTitleGenerator {
    async fn generate(&self, text: &str) -> Result<String> {
        let keywords = extract_keywords(text, 3);

        if keywords.is_empty() {
            return Ok(String::new());
        }

        // Join top keywords and truncate to max_words
        let combined: Vec<&str> = keywords
            .iter()
            .flat_map(|k| k.split_whitespace())
            .take(self.max_words)
            .collect();

        Ok(title_case(&combined.join(" ")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_keywords_basic() {
        let text = "Use PostgreSQL for the database backend instead of SQLite";
        let keywords = extract_keywords(text, 3);
        assert!(!keywords.is_empty());
        // Should extract meaningful phrases like "postgresql", "database backend", "sqlite"
    }

    #[test]
    fn test_extract_keywords_empty() {
        assert!(extract_keywords("", 3).is_empty());
        assert!(extract_keywords("   ", 3).is_empty());
        assert!(extract_keywords("the a an", 3).is_empty());
    }

    #[test]
    fn test_title_case() {
        assert_eq!(title_case("hello world"), "Hello World");
        assert_eq!(title_case("postgresql database"), "Postgresql Database");
    }

    #[tokio::test]
    async fn test_keyword_generator() {
        let gen = KeywordTitleGenerator::new();
        let title = gen
            .generate("Use PostgreSQL for the database backend instead of SQLite")
            .await
            .unwrap();
        assert!(!title.is_empty());
        assert!(title.split_whitespace().count() <= 4);
    }

    #[tokio::test]
    async fn test_keyword_generator_empty_input() {
        let gen = KeywordTitleGenerator::new();
        let title = gen.generate("").await.unwrap();
        assert!(title.is_empty());
    }

    #[tokio::test]
    async fn test_keyword_generator_only_stop_words() {
        let gen = KeywordTitleGenerator::new();
        let title = gen
            .generate("the and or but in on at to for")
            .await
            .unwrap();
        assert!(title.is_empty());
    }
}
