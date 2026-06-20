//! Text chunking for embedding providers with token limits.

/// Split text into chunks that fit within a provider's token limit.
///
/// Uses a conservative word budget (`max_tokens * 3 / 4`) to approximate
/// token boundaries without a full tokenizer. For 256 max tokens this
/// yields ~192 words per chunk.
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

    words
        .chunks(max_words)
        .map(|chunk| chunk.join(" "))
        .collect()
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
        // 256 tokens * 3/4 = 192 words per chunk
        let words: Vec<String> = (0..400).map(|i| format!("word{}", i)).collect();
        let text = words.join(" ");
        let chunks = chunk_text(&text, 256);
        assert_eq!(chunks.len(), 3); // 192 + 192 + 16
                                     // First chunk should have 192 words
        assert_eq!(chunks[0].split_whitespace().count(), 192);
        // Second chunk should have 192 words
        assert_eq!(chunks[1].split_whitespace().count(), 192);
        // Third chunk gets the remainder
        assert_eq!(chunks[2].split_whitespace().count(), 16);
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

        // One more word should split into two chunks
        let words: Vec<String> = (0..max_words + 1).map(|i| format!("w{}", i)).collect();
        let text = words.join(" ");
        let chunks = chunk_text(&text, max_tokens);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].split_whitespace().count(), max_words);
        assert_eq!(chunks[1].split_whitespace().count(), 1);
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
