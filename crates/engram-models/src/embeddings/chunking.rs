//! Text chunking for embedding providers with token limits.

/// Trailing chunks shorter than this many words are rebalanced with their
/// predecessor rather than embedded on their own (see [`chunk_text`]).
const RUNT_MAX_WORDS: usize = 32;

/// Split text into chunks that fit within a provider's token limit.
///
/// Uses a conservative word budget (`max_tokens * 3 / 4`) to approximate
/// token boundaries without a full tokenizer. For 256 max tokens this
/// yields ~192 words per chunk.
///
/// A trailing "runt" chunk — shorter than `min(32, max_words / 4)` words —
/// is **rebalanced** with the preceding chunk: the last two chunks are
/// re-split at the midpoint of their combined words. A tiny tail fragment
/// carries too little context to embed meaningfully but still competes in
/// max-score aggregation (the embedding-strategy benchmark measured
/// runt-merge as the only chunker change with a strictly positive sign
/// profile; see `docs/contributors/embedding-analysis.md`, E5). Rebalancing
/// rather than appending keeps both halves *under* the word budget — the
/// word-budget heuristic already runs hot against the true token limit for
/// dense text (E3), so appending a runt to an already-full chunk would push
/// it past the provider's silent truncation and drop the runt's text from
/// the index entirely (review finding). The threshold scales down with tiny
/// budgets (`max_words / 4`) so degenerate `max_tokens` values keep their
/// exact-split semantics.
///
/// Returns an empty vec if the input is empty or whitespace-only.
/// Returns a single-element vec if the text fits in one chunk.
pub fn chunk_text(text: &str, max_tokens: usize) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }

    let max_words = (max_tokens * 3 / 4).max(1);

    if words.len() <= max_words {
        return vec![words.join(" ")];
    }

    let mut chunks: Vec<String> = words
        .chunks(max_words)
        .map(|chunk| chunk.join(" "))
        .collect();

    let runt_threshold = RUNT_MAX_WORDS.min(max_words / 4);
    if chunks.len() >= 2 {
        let tail_words = words.len() % max_words;
        if tail_words > 0 && tail_words < runt_threshold {
            // Re-split the last full chunk + runt at their midpoint: both
            // halves stay <= max_words (combined < 2 * max_words), so no
            // text can cross the provider's truncation boundary.
            chunks.pop();
            chunks.pop();
            let tail_start = words.len() - max_words - tail_words;
            let combined = &words[tail_start..];
            let half = combined.len().div_ceil(2);
            chunks.push(combined[..half].join(" "));
            chunks.push(combined[half..].join(" "));
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_input() {
        assert!(chunk_text("", 256).is_empty());
        assert!(chunk_text("   ", 256).is_empty());
        assert!(chunk_text("\n\t  ", 256).is_empty());
    }

    #[test]
    fn test_short_text_single_chunk() {
        let text = "hello world this is a short text";
        let chunks = chunk_text(text, 256);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello world this is a short text");
    }

    #[test]
    fn test_long_text_multi_chunk() {
        // 256 tokens * 3/4 = 192 words per chunk. 400 words = 192 + 192 + 16;
        // the 16-word tail is a runt (<32), so the last two chunks rebalance
        // to 104 + 104. No chunk exceeds the budget, no tiny tail vector.
        let words: Vec<String> = (0..400).map(|i| format!("word{}", i)).collect();
        let text = words.join(" ");
        let chunks = chunk_text(&text, 256);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].split_whitespace().count(), 192);
        assert_eq!(chunks[1].split_whitespace().count(), 104);
        assert_eq!(chunks[2].split_whitespace().count(), 104);
        // Rebalancing must preserve every word in order.
        assert_eq!(chunks.join(" "), text);

        // A tail at or above the 32-word runt threshold stays its own chunk.
        let words: Vec<String> = (0..192 + 192 + 32).map(|i| format!("word{}", i)).collect();
        let text = words.join(" ");
        let chunks = chunk_text(&text, 256);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[2].split_whitespace().count(), 32);
    }

    #[test]
    fn test_exact_boundary() {
        // Exactly max_words words should produce a single chunk
        let max_tokens = 256;
        let max_words = max_tokens * 3 / 4; // 192
        let words: Vec<String> = (0..max_words).map(|i| format!("w{}", i)).collect();
        let text = words.join(" ");
        let chunks = chunk_text(&text, max_tokens);
        assert_eq!(chunks.len(), 1);

        // One more word is a 1-word runt: the two chunks rebalance to
        // 97 + 96 instead of 192 + 1 (a single word carries no context but
        // would still compete in max-score aggregation) — and instead of
        // 192+1 appended, which would cross the token-truncation boundary.
        let words: Vec<String> = (0..max_words + 1).map(|i| format!("w{}", i)).collect();
        let text = words.join(" ");
        let chunks = chunk_text(&text, max_tokens);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].split_whitespace().count(), 97);
        assert_eq!(chunks[1].split_whitespace().count(), 96);
        assert_eq!(chunks.join(" "), text);

        // First tail width that survives as its own chunk: the runt
        // threshold (min(32, 192/4) = 32).
        let words: Vec<String> = (0..max_words + 32).map(|i| format!("w{}", i)).collect();
        let text = words.join(" ");
        let chunks = chunk_text(&text, max_tokens);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].split_whitespace().count(), max_words);
        assert_eq!(chunks[1].split_whitespace().count(), 32);
    }

    #[test]
    fn exact_multiple_tail_is_not_a_runt() {
        // words.len() an exact multiple of max_words: tail_words == 0, the
        // rebalance must not fire (a full-width tail is not a runt).
        let words: Vec<String> = (0..384).map(|i| format!("w{}", i)).collect();
        let chunks = chunk_text(&words.join(" "), 256);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].split_whitespace().count(), 192);
        assert_eq!(chunks[1].split_whitespace().count(), 192);
    }

    #[test]
    fn runt_threshold_scales_away_for_tiny_budgets() {
        // max_tokens=4 → max_words=3 → threshold min(32, 3/4)=0: exact-split
        // semantics preserved, nothing ever rebalances (see test_small_max_tokens).
        let chunks = chunk_text("a b c d", 4);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1], "d");
    }

    #[test]
    fn test_small_max_tokens() {
        let text = "one two three four five";
        // max_tokens=4 => max_words = 3
        let chunks = chunk_text(text, 4);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "one two three");
        assert_eq!(chunks[1], "four five");
    }

    // Finding #10 (documented/accepted): `chunk_text` uses a word-count budget
    // and cannot, without a real tokenizer, guarantee a chunk stays under the
    // model's *token* limit for dense text (code/URLs/CJK) or a single
    // whitespace-free blob. That residual is SAFE: fastembed truncates an
    // over-long chunk rather than erroring, so the worst case is lost trailing
    // tokens, never a crash or corruption. This test pins the safe handling of
    // a single huge whitespace-free token (one chunk, no panic), and documents
    // that tokenizer-accurate chunking is intentionally out of scope here.
    #[test]
    fn single_whitespace_free_blob_is_one_chunk_not_a_panic() {
        let blob = "x".repeat(10_000); // no whitespace → one "word"
        let chunks = chunk_text(&blob, 256);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 10_000);
    }
}
