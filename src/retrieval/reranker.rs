//! Cross-encoder reranking abstraction (re-exported from `engram-models`).
//!
//! The trait plus its concrete `fastembed`-backed loader now live in
//! [`engram_models::rerank`], next to the embedding / NLI / T5 model loaders.
//! The retrieval layer keeps only this consuming seam so
//! `crate::retrieval::reranker::{Reranker, RerankScore, LocalReranker}` keeps
//! resolving for the engine, `ops`, and the daemon's `RemoteReranker` impl.

pub use engram_models::rerank::{RerankScore, Reranker};
// `LocalReranker` is the `fastembed` in-process loader — only present when the
// ONNX Runtime stack is compiled in.
#[cfg(feature = "onnxruntime")]
pub use engram_models::rerank::LocalReranker;
// `TractReranker` is the pure-Rust fp32 BGE loader for the Intel-Mac / tract
// build (used only when ONNX Runtime is absent).
#[cfg(feature = "tract")]
pub use engram_models::rerank::TractReranker;
