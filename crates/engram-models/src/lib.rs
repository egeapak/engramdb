//! ML model layer for EngramDB.
//!
//! Confines the ONNX / embedding model stack (`ort`, `fastembed`, `tokenizers`,
//! `hf-hub`, `ndarray`, and the optional Ollama backend) to a single crate:
//!
//! - [`embeddings`] — the `EmbeddingProvider` trait and its ONNX / Ollama impls.
//! - [`nli`] — natural-language-inference contradiction detection plus the
//!   challenge-writing flow that the retrieval layer drives.
//! - [`title`] — automatic title generation (keyword extraction or T5-small).
//!
//! Re-exported by the top-level `engramdb` crate under its historical
//! `embeddings` / `nli` / `title` module paths.

pub mod embeddings;
pub mod nli;
pub mod title;

// Test isolation: link `engram-test-support` so its `#[ctor]` redirects the
// global data/config dirs before any test runs (the nli/challenge tests build
// real `MemoryStore`s). The `arm()` reference prevents dead-stripping.
#[cfg(test)]
#[ctor::ctor]
fn arm_test_isolation() {
    engram_test_support::arm();
}
