//! Cross-encoder reranking abstraction (re-exported from `engram-models`).
//!
//! The trait plus its concrete `fastembed`-backed loader now live in
//! [`engram_models::rerank`], next to the embedding / NLI / T5 model loaders.
//! The retrieval layer keeps only this consuming seam so
//! `crate::retrieval::reranker::{Reranker, RerankScore, LocalReranker}` keeps
//! resolving for the engine, `ops`, and the daemon's `RemoteReranker` impl.

pub use engram_models::rerank::{RerankScore, Reranker};
// `LocalReranker` is the `fastembed` in-process loader — only present when the
// ONNX Runtime stack is compiled in (a pure-`tract` build has no reranker).
#[cfg(feature = "onnxruntime")]
pub use engram_models::rerank::LocalReranker;
