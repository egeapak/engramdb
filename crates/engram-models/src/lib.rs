//! ML model layer for EngramDB.
//!
//! Confines the ONNX / embedding model stack (`ort`, `fastembed`, `tokenizers`,
//! `hf-hub`, `ndarray`, and the optional Ollama backend) to a single crate:
//!
//! - [`embeddings`] — the `EmbeddingProvider` trait and its ONNX / Ollama impls.
//! - [`nli`] — natural-language-inference contradiction detection plus the
//!   challenge-writing flow that the retrieval layer drives.
//! - [`rerank`] — the cross-encoder `Reranker` trait and its `fastembed` loader.
//! - [`title`] — automatic title generation (keyword extraction or T5-small).
//!
//! Re-exported by the top-level `engramdb` crate under its historical
//! `embeddings` / `nli` / `title` module paths (and `retrieval::reranker` for
//! [`rerank`]).

pub mod embeddings;
pub mod nli;
pub mod rerank;
pub mod title;

/// Fail early, and *recoverably*, when the ONNX Runtime is not usable.
///
/// Every ONNX-backed model loader calls this before touching `ort`. Under the
/// default `load-dynamic` strategy the runtime is `dlopen`ed rather than linked,
/// so it can genuinely be absent — and `ort`'s own reaction to that is a panic
/// (`.expect("Failed to load ONNX Runtime dylib")`) that the release profile's
/// `panic = "abort"` turns into a hard process abort, with no `catch_unwind` to
/// save it.
///
/// Returning an `Err` here instead keeps a missing runtime on the same graceful
/// path as a missing model file: `try_new()`-style constructors map it to
/// `None`, the provider resolves to "no embeddings", and retrieval degrades to
/// keyword search exactly as it does offline.
#[cfg(feature = "onnxruntime")]
pub(crate) fn ensure_onnx_runtime() -> anyhow::Result<()> {
    match engram_onnx::runtime_status() {
        Ok(_) => Ok(()),
        Err(reason) => Err(anyhow::anyhow!(reason)),
    }
}

/// Strip the `SessionBuilder` payload from an `ort` builder error.
///
/// ort rc.12 changed the `SessionBuilder` configuration methods to return
/// `Error<SessionBuilder>` (the error carries the builder back so the caller
/// can recover it). That payload is `!Send`/`!Sync`, so propagating it with `?`
/// into an `anyhow::Result` fails `anyhow`'s `Send + Sync` bound. Converting to
/// the unit-payload `ort::Error` (which is `Send + Sync`) via the crate's
/// `From` impl restores `?`-propagation while preserving the error message/code.
#[cfg(feature = "onnxruntime")]
pub(crate) fn erase_builder_err(
    e: ort::Error<ort::session::builder::SessionBuilder>,
) -> ort::Error {
    e.into()
}

// Test isolation: link `engram-test-support` so its `#[ctor]` redirects the
// global data/config dirs before any test runs (the nli/challenge tests build
// real `MemoryStore`s). The `arm()` reference prevents dead-stripping.
#[cfg(test)]
#[ctor::ctor(unsafe)]
fn arm_test_isolation() {
    engram_test_support::arm();
}
