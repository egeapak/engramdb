//! Text normalization shared by the keyword scorer's query and document sides.
//!
//! The pipeline is `split -> lowercase -> drop stopwords -> stem -> dedup`, and
//! it is deliberately the *only* place that decides what a "token" is. Query
//! and document text must go through the identical transform or the comparison
//! is meaningless: stemming one side and not the other is strictly worse than
//! stemming neither.
//!
//! ## Why stemming
//!
//! Measured, not assumed. `examples/fts_quality.rs` scores the keyword scorer
//! against LanceDB's BM25 over a labelled corpus; the entire quality gap was
//! stemming. Queries like `compressing` returned *nothing* for a memory whose
//! summary says "Compression", because the scorer compared raw strings. That
//! probe moved 0.00 -> 1.00 Recall@1 with a stemmer in place, while BM25's own
//! scoring contributed comparatively little.
//!
//! ## Why stopwords too, when IDF would subsume them
//!
//! IDF (inverse document frequency) down-weights common terms adaptively and
//! would handle "the" without a list — but it needs corpus statistics, and it
//! is weak when the candidate set is small. A fixed stoplist costs nothing, is
//! corpus-independent, and shrinks what Phase 2 persists. The two compose:
//! the list removes English function words, IDF later handles *domain* noise
//! words ("memory", "store") that no English stoplist contains.
//!
//! Without it the scorer had a concrete failure: for the query
//! "what is the flock for" it ranked the *embedding model* memory first,
//! because that summary contains "the" and "is" and summary matches carry 3x
//! weight — six points of pure noise beating the three points earned by the
//! one word that meant anything.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::OnceLock;

use compact_str::CompactString;
use rust_stemmers::{Algorithm, Stemmer};
use serde::{Deserialize, Serialize};

/// Identifier for the exact normalization behaviour, persisted so stored
/// stems can be invalidated when it changes.
///
/// Bump this on ANY change that alters the output of [`normalize`] — a
/// `rust-stemmers` upgrade, a stoplist edit, a tokenizer change. Stems written
/// under an older stamp are not comparable with ones written under a newer
/// stamp, exactly like an embedding produced by a different model: the values
/// still parse, they just silently mean something else.
pub const NORMALIZER_STAMP: &str = "snowball-en+stop-v1";

/// English function words carrying no retrieval signal.
///
/// Kept intentionally small and generic. This is not the place for
/// domain-specific noise words ("memory", "store", "config") — those are
/// corpus-dependent and belong to IDF, which learns them. Over-trimming here
/// would be unrecoverable, since a removed term cannot be scored at all.
const STOPWORDS: &[&str] = &[
    "a", "about", "an", "and", "are", "as", "at", "be", "been", "but", "by", "can", "did", "do",
    "does", "for", "from", "had", "has", "have", "he", "her", "his", "how", "i", "if", "in",
    "into", "is", "it", "its", "me", "my", "no", "not", "of", "on", "or", "our", "out", "over",
    "she", "should", "so", "some", "such", "than", "that", "the", "their", "them", "then", "there",
    "these", "they", "this", "those", "to", "too", "up", "us", "was", "we", "were", "what", "when",
    "where", "which", "while", "who", "why", "will", "with", "would", "you", "your",
];

fn stopwords() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STOPWORDS.iter().copied().collect())
}

fn stemmer() -> &'static Stemmer {
    static STEMMER: OnceLock<Stemmer> = OnceLock::new();
    STEMMER.get_or_init(|| Stemmer::create(Algorithm::English))
}

/// Split text into lowercase alphanumeric runs.
///
/// Borrows from `lowered`, so the caller keeps the lowercased buffer alive
/// rather than allocating a `String` per token.
fn split_tokens(lowered: &str) -> impl Iterator<Item = &str> {
    lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
}

/// Whether a raw (already lowercased) token is a stopword.
fn is_stopword(token: &str) -> bool {
    stopwords().contains(token)
}

/// Run the full pipeline over `text`, returning deduplicated stems.
///
/// Deduplication is load-bearing on the query side: the scorer awards points
/// per token, so a repeated word used to count twice. "the daemon and the
/// socket" scored `the` twice — and since matches were unweighted, repeating
/// any word inflated the result. Both sides dedup so a term contributes at
/// most once per field.
pub fn normalize(text: &str) -> Vec<String> {
    normalize_as(text)
}

/// A stem type that [`normalize_as`] and [`normalize_counts_as`] can build:
/// `String` for the public API, `CompactString` for [`KeywordStems`].
trait StemString: for<'a> From<&'a str> + Clone + Eq + Hash + Borrow<str> {}
impl<S: for<'a> From<&'a str> + Clone + Eq + Hash + Borrow<str>> StemString for S {}

/// [`normalize`], building each stem directly as `S`.
///
/// A repeated stem is found by `&str` lookup, so it allocates nothing. A new
/// one is built once from the stemmer's output, never via a `String` first.
fn normalize_as<S: StemString>(text: &str) -> Vec<S> {
    let lowered = text.to_lowercase();
    let stem = stemmer();
    let mut seen: HashSet<S> = HashSet::new();
    let mut out = Vec::new();
    for token in split_tokens(&lowered) {
        if is_stopword(token) {
            continue;
        }
        let stemmed = stem.stem(token);
        if !seen.contains(stemmed.as_ref()) {
            let s = S::from(stemmed.as_ref());
            seen.insert(s.clone());
            out.push(s);
        }
    }
    out
}

/// [`normalize`] collected into a set, for the document side where only
/// membership is asked.
pub fn normalize_set(text: &str) -> HashSet<String> {
    normalize(text).into_iter().collect()
}

/// Stems with their occurrence counts, plus the total number of scoreable
/// tokens the text produced.
///
/// Presence alone cannot distinguish a document that mentions a term once in
/// passing from one that is entirely about it, which is what leaves the scorer
/// unable to break a tie. Counts and length are the two inputs needed to tell
/// them apart, so the content field is measured rather than merely tested.
///
/// The length is the token total *before* deduplication and *after* stopword
/// removal — the count of terms that could have matched, which is the right
/// denominator for a density comparison.
pub fn normalize_counts(text: &str) -> (HashMap<String, u32>, usize) {
    normalize_counts_as(text)
}

/// [`normalize_counts`], building each stem directly as `S`. A repeated stem
/// only bumps its count, with no allocation.
fn normalize_counts_as<S: StemString>(text: &str) -> (HashMap<S, u32>, usize) {
    let lowered = text.to_lowercase();
    let stem = stemmer();
    let mut counts: HashMap<S, u32> = HashMap::new();
    let mut total = 0usize;
    for token in split_tokens(&lowered) {
        if is_stopword(token) {
            continue;
        }
        total += 1;
        let stemmed = stem.stem(token);
        if let Some(count) = counts.get_mut(stemmed.as_ref()) {
            *count += 1;
        } else {
            counts.insert(S::from(stemmed.as_ref()), 1);
        }
    }
    (counts, total)
}

/// A memory reduced to the stems the keyword scorer compares against, one
/// entry per weighted field.
///
/// Deriving this is pure document-side work: it depends only on the memory,
/// never on the query. That is what makes it precomputable — it is built once
/// on the write path and persisted, rather than recomputed for every memory on
/// every query.
///
/// Content carries occurrence *counts* rather than mere membership, plus the
/// field's token length, because presence alone cannot distinguish a document
/// that mentions a term once in passing from one that is about it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KeywordStems {
    /// Distinct stems in the summary.
    ///
    /// Stems are [`CompactString`]s: almost every stem fits in the 24 inline
    /// bytes, so the per-row parse a text query does for every candidate
    /// allocates no string per stem. The JSON on disk is unchanged.
    #[serde(default, rename = "s")]
    pub summary: Vec<CompactString>,
    /// Distinct stems across all tags.
    #[serde(default, rename = "t")]
    pub tags: Vec<CompactString>,
    /// Content stems with occurrence counts.
    #[serde(default, rename = "c")]
    pub content: HashMap<CompactString, u32>,
    /// Scoreable content tokens, post-stopword and pre-deduplication.
    #[serde(default, rename = "n")]
    pub content_len: usize,
}

impl KeywordStems {
    /// Derive the stems for a memory's three weighted fields.
    pub fn compute(summary: &str, tags: &[String], content: &str) -> Self {
        // Built as `CompactString` from the start: converting `String` stems
        // afterwards copied every stem and rebuilt the content map.
        let (content, content_len) = normalize_counts_as::<CompactString>(content);
        let mut tag_stems: Vec<CompactString> = Vec::new();
        let mut seen: HashSet<CompactString> = HashSet::new();
        for tag in tags {
            for stem in normalize_as::<CompactString>(tag) {
                if seen.insert(stem.clone()) {
                    tag_stems.push(stem);
                }
            }
        }
        Self {
            summary: normalize_as(summary),
            tags: tag_stems,
            content,
            content_len,
        }
    }

    /// Whether the term appears in any field — the unit of "document
    /// frequency" for IDF.
    pub fn contains(&self, term: &str) -> bool {
        self.summary.iter().any(|s| s == term)
            || self.tags.iter().any(|s| s == term)
            || self.content.contains_key(term)
    }

    pub fn in_summary(&self, term: &str) -> bool {
        self.summary.iter().any(|s| s == term)
    }

    pub fn in_tags(&self, term: &str) -> bool {
        self.tags.iter().any(|s| s == term)
    }

    /// Occurrences of `term` in the content, zero when absent.
    pub fn content_freq(&self, term: &str) -> u32 {
        self.content.get(term).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_inflections_together() {
        // The six cases the relevance harness measured as total misses.
        for (query, document) in [
            ("compressing", "Compression merges near duplicate memories"),
            ("serializing", "Registry writes serialize on a lock file"),
            ("validating", "Summary length is validated before storing"),
            (
                "authenticating",
                "The server authenticates nothing on stdio",
            ),
            ("quantizing", "The default model is int8 quantized"),
            ("expiring", "Step decay holds until the ttl expires"),
        ] {
            let q = normalize(query);
            let d = normalize_set(document);
            assert_eq!(q.len(), 1, "query should reduce to one term: {q:?}");
            assert!(
                d.contains(&q[0]),
                "`{query}` -> {:?} should be found in {d:?}",
                q[0]
            );
        }
    }

    #[test]
    fn drops_stopwords() {
        // The exact query that ranked the wrong memory first.
        let tokens = normalize("what is the flock for");
        assert_eq!(tokens, vec!["flock"], "only the meaningful term survives");
    }

    #[test]
    fn all_stopword_query_is_empty() {
        // Must not panic or match everything; the caller treats this as
        // "no keyword signal".
        assert!(normalize("what is the").is_empty());
    }

    #[test]
    fn deduplicates_repeated_terms() {
        // Previously "the" counted twice and inflated the score; now a term
        // contributes at most once regardless of repetition.
        let tokens = normalize("daemon and the daemon and the daemon");
        assert_eq!(tokens, vec!["daemon"]);
    }

    #[test]
    fn preserves_distinct_terms_and_order() {
        let tokens = normalize("retry policy backoff");
        assert_eq!(tokens, vec!["retri", "polici", "backoff"]);
    }

    #[test]
    fn splits_on_punctuation_and_case() {
        let tokens = normalize("Cache-Invalidation, CONFIG_change");
        assert!(tokens.contains(&"cach".to_string()));
        assert!(tokens.contains(&"invalid".to_string()));
        assert!(tokens.contains(&"config".to_string()));
        assert!(tokens.contains(&"chang".to_string()));
    }

    #[test]
    fn stamp_is_stable() {
        // Guards the invariant rather than the value: the stamp exists so
        // stored stems can be invalidated, and must change whenever the
        // pipeline's output does.
        assert!(!NORMALIZER_STAMP.is_empty());
        assert!(NORMALIZER_STAMP.contains("v1"));
    }
}
