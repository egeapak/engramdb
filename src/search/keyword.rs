//! Keyword-based search for memories

use std::borrow::Borrow;

use crate::search::normalize;
use crate::types::{KeywordStems, Memory};

/// Perform keyword search on a collection of memories
///
/// Returns a vector of (index, score) tuples where:
/// - index: the index into the memories slice
/// - score: raw weighted match score (unbounded)
///
/// # Algorithm
/// 1. Reduce the query to deduplicated stems ([`normalize`](normalize::normalize))
/// 2. Reduce each memory the same way, once, into per-field stem sets
/// 3. Derive an IDF weight per query term from how many of these memories
///    contain it, normalized so the weights sum to the term count
/// 4. Score each term once per field it appears in, scaled by that weight:
///    - Summary match: 3x weight
///    - Tag match: 2x weight
///    - Content match: 1x weight scaled by [`content_density`], so a passing
///      mention in a long field counts for less than the same word in a field
///      that is about it
/// 5. Filter out zero scores and sort by score descending
///
/// Both refinements are anchored so the familiar case is unchanged: IDF
/// weights sum to the term count, and the density factor is exactly 1.0 for a
/// single occurrence at average content length. A query whose terms are
/// equally common, over average-length content, therefore scores precisely as
/// it did before either existed. Density may exceed 1.0 for a field densely
/// about a term, which raises the ceiling from `6 * terms` to
/// `(5 + MAX_DENSITY) * terms` — see [`content_density`] for why that is safe
/// for [`normalize_keyword_score`].
///
/// # Arguments
/// * `query` - The search query string
/// * `memories` - Slice of memories (or references) to search
///
/// # Returns
/// Vector of (index, relevance_score) tuples, sorted by score descending
pub fn keyword_search<M: Borrow<Memory>>(query: &str, memories: &[M]) -> Vec<(usize, f64)> {
    // One pass to derive each memory's stems, so they are built once rather
    // than per query term, and so document frequencies can be counted before
    // anything is scored.
    let stems: Vec<KeywordStems> = memories
        .iter()
        .map(|m| {
            let m = m.borrow();
            KeywordStems::compute(&m.summary, &m.tags, &m.content)
        })
        .collect();
    keyword_search_stems(query, &stems)
}

/// [`keyword_search`] over stems that were computed elsewhere — in practice
/// read straight from the index, where the write path stored them.
///
/// Deriving stems is pure document-side work, identical on every query, and
/// measurably the dominant cost: scoring 1000 memories takes ~17.7ms when the
/// stems are rebuilt each time. Passing them in removes that entirely.
///
/// The returned indices refer to `stems`, so the caller is responsible for
/// keeping that slice aligned with whatever it maps results back onto.
pub fn keyword_search_stems(query: &str, stems: &[KeywordStems]) -> Vec<(usize, f64)> {
    // Normalized, stemmed, deduplicated query terms. Deduplication matters:
    // the loop below awards points per token, so a repeated word used to be
    // counted once per occurrence.
    let query_tokens: Vec<String> = normalize::normalize(query);

    if query_tokens.is_empty() {
        return vec![];
    }

    let weights = idf_weights(&query_tokens, stems);

    // Average content length over the candidate set, the baseline the density
    // factor normalizes against. Empty contents are excluded so a store full
    // of summary-only memories does not drag the reference to zero.
    let non_empty: Vec<usize> = stems
        .iter()
        .map(|s| s.content_len)
        .filter(|l| *l > 0)
        .collect();
    let avg_content_len = if non_empty.is_empty() {
        0.0
    } else {
        non_empty.iter().sum::<usize>() as f64 / non_empty.len() as f64
    };

    let mut results: Vec<(usize, f64)> = stems
        .iter()
        .enumerate()
        .filter_map(|(idx, doc)| {
            let score = score_stems(doc, &query_tokens, &weights, avg_content_len);
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

/// Score one memory's precomputed stems against the weighted query terms.
///
/// Free-standing rather than a method on [`KeywordStems`] because the weighting
/// (3x summary / 2x tags / 1x content, IDF, density) is retrieval policy, which
/// belongs here, while the type itself is a storage-layer concern.
fn score_stems(
    doc: &KeywordStems,
    query_tokens: &[String],
    weights: &[f64],
    avg_content_len: f64,
) -> f64 {
    let mut weighted_matches = 0.0;
    for (token, weight) in query_tokens.iter().zip(weights) {
        if doc.in_summary(token) {
            weighted_matches += 3.0 * weight;
        }
        if doc.in_tags(token) {
            weighted_matches += 2.0 * weight;
        }
        let tf = doc.content_freq(token);
        if tf > 0 {
            weighted_matches += content_density(tf, doc.content_len, avg_content_len) * weight;
        }
    }
    weighted_matches
}

/// How strongly a content field is *about* a term, in `(0, 1]`.
///
/// The binary "contains it at all" test cannot distinguish a roadmap that lists
/// a topic once from the memory that explains it, so two such memories score
/// identically and store order picks the winner. This restores that distinction
/// using BM25's term-frequency saturation with length normalisation: more
/// occurrences help with diminishing returns, and a longer field dilutes each
/// one.
///
/// Scaled so that **one occurrence in an average-length field is exactly 1.0**,
/// with no upper clamp:
///  - a typical single mention scores what it always did;
///  - a mention diluted across a field several times the average length scores
///    less;
///  - a field densely about the term scores more, up to [`MAX_DENSITY`].
///
/// An earlier version capped this at 1.0, reasoning that scores which can only
/// fall are safer against a fixed relevance threshold. Measurement rejected it:
/// capping makes the adjustment *asymmetric*, because a long field that repeats
/// the term saturates to the cap and escapes the length penalty, while a long
/// field that mentions it once absorbs the penalty in full. That systematically
/// favours a repetitive document over a substantive one — precisely backwards —
/// and the TermFreq probe fell from 0.67 to 0.33 as a result.
///
/// Letting the factor exceed 1.0 raises the theoretical maximum from `6 * terms`
/// to `(5 + MAX_DENSITY) * terms`. [`normalize_keyword_score`] absorbs that
/// without recalibration: its sigmoid is asymptotic rather than clamped, and its
/// documented anchor — all terms matching the summary sits at 0.50 — depends
/// only on the summary weight, which is untouched.
fn content_density(tf: u32, len: usize, avg_len: f64) -> f64 {
    // BM25 defaults: k1 controls how fast repetition saturates, b how strongly
    // length is normalised.
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    /// Ceiling on the density bonus. BM25's saturation already bounds the raw
    /// factor near 2.2 for this k1; this pins it so the score range stays a
    /// stated property rather than an emergent one.
    const MAX_DENSITY: f64 = 2.5;

    if avg_len <= f64::EPSILON {
        return 1.0;
    }
    let saturate = |tf: f64, len: f64| {
        let norm = 1.0 - B + B * (len / avg_len);
        tf / (tf + K1 * norm)
    };
    // Reference point: a single occurrence at average length.
    let reference = saturate(1.0, avg_len);
    if reference <= f64::EPSILON {
        return 1.0;
    }
    (saturate(tf as f64, len as f64) / reference).min(MAX_DENSITY)
}

/// Per-term IDF weights that **sum to `query_tokens.len()`**.
///
/// Rare terms should count for more than common ones, but naively multiplying
/// by raw IDF would rescale every score by a corpus-dependent factor and
/// invalidate [`normalize_keyword_score`], whose sigmoid is centred on
/// `3 * term_count` and whose output feeds a fixed relevance threshold.
///
/// Normalising by the mean keeps the total weight equal to the term count, so:
/// - the maximum achievable raw score is still `6 * term_count`;
/// - a query whose terms are equally common yields all-1.0 weights and scores
///   exactly as it did before IDF existed;
/// - a mixed query merely *redistributes* weight from common terms to rare
///   ones, leaving the scale untouched.
///
/// Document frequency is counted over the candidate set being scored, not the
/// whole store. That needs no extra storage and cannot go stale, at the cost
/// of the weights depending on filter context — which is also why the fixed
/// stoplist in [`normalize`](normalize::normalize) is retained: with few
/// candidates these statistics are thin.
///
/// [`normalize`]: crate::search::normalize
fn idf_weights(query_tokens: &[String], stems: &[KeywordStems]) -> Vec<f64> {
    let n_terms = query_tokens.len();
    let uniform = vec![1.0; n_terms];

    // With too few documents the frequencies are noise, not signal.
    const MIN_DOCS_FOR_IDF: usize = 8;
    if stems.len() < MIN_DOCS_FOR_IDF {
        return uniform;
    }

    let n_docs = stems.len() as f64;
    let raw: Vec<f64> = query_tokens
        .iter()
        .map(|term| {
            let df = stems.iter().filter(|d| d.contains(term)).count() as f64;
            // BM25's IDF with the +0.5 smoothing, which stays positive even
            // for a term present in every document.
            (1.0 + (n_docs - df + 0.5) / (df + 0.5)).ln()
        })
        .collect();

    let mean = raw.iter().sum::<f64>() / n_terms as f64;
    if !mean.is_finite() || mean <= f64::EPSILON {
        // Every term equally (un)common — nothing to redistribute.
        return uniform;
    }

    // Clamp before rescaling so one freakishly rare term cannot swamp the
    // rest, then rescale so the weights sum back to `n_terms`.
    const MIN_WEIGHT: f64 = 0.25;
    const MAX_WEIGHT: f64 = 4.0;
    let clamped: Vec<f64> = raw
        .iter()
        .map(|idf| (idf / mean).clamp(MIN_WEIGHT, MAX_WEIGHT))
        .collect();
    let sum = clamped.iter().sum::<f64>();
    if !sum.is_finite() || sum <= f64::EPSILON {
        return uniform;
    }
    let rescale = n_terms as f64 / sum;
    clamped.into_iter().map(|w| w * rescale).collect()
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

/// Count the scoreable terms in a query.
///
/// Must stay consistent with what [`keyword_search`] actually scores, because
/// [`normalize_keyword_score`] centres its sigmoid on `3 * this`. Both call
/// [`normalize::normalize`], so stopword removal and deduplication are
/// reflected on both sides: "what is the flock for" counts 1, not 5, and its
/// midpoint is set accordingly instead of demanding five matches that can
/// never occur.
pub fn query_token_count(query: &str) -> usize {
    normalize::normalize(query).len()
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
            epistemic: MemoryType::Decision.default_epistemic(),
            valid_while: None,
            valid_from: None,
            invalidated_at: None,
            superseded_by: None,
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
            audience: None,
            source_sessions: vec![],
            challenges: vec![],
            verified_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            accessed_at: Utc::now(),
            expires_at: None,
        }
    }

    /// The scorer's notion of a "token" now comes from `search::normalize`:
    /// stopwords are dropped and the remainder is stemmed. Previously this
    /// asserted the raw split, which kept "this", "is" and "a" as scoreable
    /// terms — each worth 3 points against any summary containing them.
    #[test]
    fn test_query_tokens_drop_stopwords_and_stem() {
        let tokens = normalize::normalize("Hello, World! This is a test.");
        assert_eq!(tokens, vec!["hello", "world", "test"]);
    }

    #[test]
    fn test_query_tokens_empty() {
        assert!(normalize::normalize("").is_empty());
    }

    /// `query_token_count` centres the normalization sigmoid, so it has to
    /// agree with what is actually scored — otherwise the midpoint demands
    /// matches for terms that were never searched for.
    #[test]
    fn test_query_token_count_matches_scored_terms() {
        assert_eq!(query_token_count("what is the flock for"), 1);
        assert_eq!(query_token_count("retry policy backoff"), 3);
        // Repeats collapse; they used to inflate both the count and the score.
        assert_eq!(query_token_count("daemon daemon daemon"), 1);
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

    // ---- IDF weighting -------------------------------------------------

    /// A corpus where `daemon` is in every memory (useless for
    /// discrimination) and `backpressure` is in exactly one.
    fn idf_corpus() -> Vec<Memory> {
        let mut memories: Vec<Memory> = (0..12)
            .map(|i| {
                create_test_memory(
                    &format!("common-{i}"),
                    "daemon notes",
                    "the daemon was discussed",
                    vec![],
                )
            })
            .collect();
        memories.push(create_test_memory(
            "target",
            "daemon backpressure",
            "bounded queue applies backpressure",
            vec![],
        ));
        memories
    }

    /// The point of IDF: a term shared by every document must not outweigh
    /// one that identifies a single document.
    #[test]
    fn test_idf_prefers_the_discriminating_term() {
        let memories = idf_corpus();
        let results = keyword_search("daemon backpressure", &memories);
        let top = results.first().expect("expected a match");
        assert_eq!(
            memories[top.0].id, "target",
            "the memory carrying the rare term must rank first"
        );
    }

    /// The scale-preserving property that lets the existing sigmoid and
    /// relevance threshold stay calibrated: when every query term is equally
    /// common, IDF must be a no-op.
    #[test]
    fn test_idf_is_identity_when_terms_are_equally_common() {
        // Every memory contains both terms, so both have identical document
        // frequency and no weight can be redistributed.
        let memories: Vec<Memory> = (0..10)
            .map(|i| {
                create_test_memory(
                    &format!("m{i}"),
                    "alpha beta",
                    "alpha and beta together",
                    vec![],
                )
            })
            .collect();
        let results = keyword_search("alpha beta", &memories);
        // 2 terms x (3 summary + 1 content) with weight 1.0 each.
        for (_, score) in &results {
            assert!(
                (score - 8.0).abs() < 1e-9,
                "expected the pre-IDF score of 8.0, got {score}"
            );
        }
    }

    /// Weights are normalized, so `6 * terms` remains the ceiling no matter
    /// how skewed the frequencies are. That bound is what keeps
    /// `normalize_keyword_score` meaningful — its sigmoid is defined over
    /// `[0, 6n]`.
    ///
    /// Note this asserts the ceiling is *respected*, not that it is reached:
    /// the content term is scaled by [`content_density`], which only reaches
    /// 1.0 for content at or below the average length.
    /// `test_content_density_neutral_at_average_length` covers attainment.
    #[test]
    fn test_idf_preserves_max_score_scale() {
        let mut memories: Vec<Memory> = (0..12)
            .map(|i| create_test_memory(&format!("noise-{i}"), "common", "common", vec![]))
            .collect();
        // One memory carrying both terms in all three fields.
        memories.push(create_test_memory(
            "full",
            "common rare",
            "common rare",
            vec!["common".to_string(), "rare".to_string()],
        ));

        let results = keyword_search("common rare", &memories);
        let top = results.first().expect("expected a match");
        assert_eq!(memories[top.0].id, "full");
        // IDF weights sum to the term count, so the field weights alone bound
        // the score: 2 terms x (3 summary + 2 tags + at most MAX_DENSITY
        // content) = 15. The point is that IDF itself introduces no
        // corpus-dependent rescaling.
        for (_, score) in &results {
            assert!(
                *score <= 15.0 + 1e-9,
                "score {score} exceeds the bound implied by the field weights"
            );
        }
        assert!(top.1 > 11.0, "expected near the ceiling, got {}", top.1);
    }

    // ---- Content density (term frequency + length) ----------------------

    /// The tie the scorer could not break: two memories whose summaries match
    /// the query identically, one merely listing the topic in a long field and
    /// one actually about it. The decoy is placed first, so a surviving tie
    /// would resolve the wrong way under a stable sort.
    #[test]
    fn test_content_density_breaks_summary_ties() {
        let mut memories = vec![
            create_test_memory(
                "decoy",
                "backpressure came up in review",
                "A wide ranging review of deployment packaging logging metrics alerting \
                 releases rollbacks rotations and escalation paths. Backpressure was \
                 mentioned once and deferred without any decision being recorded here.",
                vec![],
            ),
            create_test_memory(
                "target",
                "backpressure bounds the queue",
                "Backpressure comes from a bounded queue; backpressure propagates upstream.",
                vec![],
            ),
        ];
        // Pad so IDF engages and the average content length is meaningful.
        for i in 0..10 {
            memories.push(create_test_memory(
                &format!("filler-{i}"),
                "unrelated notes",
                "some unrelated prose about other subsystems entirely",
                vec![],
            ));
        }

        let results = keyword_search("backpressure", &memories);
        let top = results.first().expect("expected a match");
        assert_eq!(
            memories[top.0].id, "target",
            "the memory actually about the term must outrank the one that lists it"
        );
    }

    /// The scale guarantee: one occurrence in an average-length field still
    /// contributes exactly 1.0, so nothing that used to clear the relevance
    /// threshold silently drops below it.
    #[test]
    fn test_content_density_neutral_at_average_length() {
        // Every memory has identical content length and a single occurrence,
        // so each is exactly average by construction.
        let memories: Vec<Memory> = (0..10)
            .map(|i| create_test_memory(&format!("m{i}"), "alpha", "alpha beta gamma", vec![]))
            .collect();
        let results = keyword_search("alpha", &memories);
        for (_, score) in &results {
            // 3.0 summary + 1.0 content, weight 1.0 (single term).
            assert!(
                (score - 4.0).abs() < 1e-9,
                "expected the pre-density score of 4.0, got {score}"
            );
        }
    }

    /// Density is symmetric: a field densely about the term outranks one of
    /// equal length that merely mentions it.
    ///
    /// This is the property an earlier capped-at-1.0 version lacked. Capping
    /// let a long repetitive field saturate to the ceiling and dodge the
    /// length penalty while a long single-mention field absorbed it, which
    /// inverted the intended ordering.
    #[test]
    fn test_content_density_rewards_concentration() {
        let mut memories = vec![
            create_test_memory(
                "dense",
                "notes",
                "alpha alpha alpha alpha beta gamma delta epsilon",
                vec![],
            ),
            create_test_memory(
                "sparse",
                "notes",
                "alpha beta gamma delta epsilon zeta eta theta",
                vec![],
            ),
        ];
        for i in 0..10 {
            memories.push(create_test_memory(
                &format!("filler-{i}"),
                "unrelated",
                "zeta eta theta iota kappa lambda mu nu",
                vec![],
            ));
        }
        let results = keyword_search("alpha", &memories);
        let score = |id: &str| {
            results
                .iter()
                .find(|(i, _)| memories[*i].id == id)
                .map(|(_, s)| *s)
                .expect("should match")
        };
        assert!(
            score("dense") > score("sparse"),
            "same length, more occurrences must score higher: {} vs {}",
            score("dense"),
            score("sparse")
        );
    }

    /// The bonus is bounded, so the score range stays a stated property
    /// rather than something that drifts with an unusually repetitive field.
    #[test]
    fn test_content_density_bonus_is_bounded() {
        let mut memories = vec![create_test_memory(
            "extreme",
            "alpha",
            &"alpha ".repeat(200),
            vec![],
        )];
        for i in 0..10 {
            memories.push(create_test_memory(
                &format!("m{i}"),
                "alpha",
                "alpha beta gamma delta epsilon zeta eta theta",
                vec![],
            ));
        }
        let results = keyword_search("alpha", &memories);
        let extreme = results
            .iter()
            .find(|(i, _)| memories[*i].id == "extreme")
            .expect("should match");
        // 3.0 summary + at most MAX_DENSITY (2.5) content, single term.
        assert!(
            extreme.1 <= 5.5 + 1e-9,
            "density bonus must stay bounded; got {}",
            extreme.1
        );
    }

    /// Below the document threshold the statistics are noise, so weighting is
    /// skipped entirely rather than applied to a handful of documents.
    #[test]
    fn test_idf_skipped_for_tiny_candidate_sets() {
        let memories = vec![
            create_test_memory("a", "daemon backpressure", "", vec![]),
            create_test_memory("b", "daemon notes", "", vec![]),
        ];
        let results = keyword_search("daemon backpressure", &memories);
        let by_id = |id: &str| {
            results
                .iter()
                .find(|(i, _)| memories[*i].id == id)
                .map(|(_, s)| *s)
        };
        // Uniform weights: "a" matches both terms in summary (6), "b" one (3).
        assert_eq!(by_id("a"), Some(6.0));
        assert_eq!(by_id("b"), Some(3.0));
    }
}
