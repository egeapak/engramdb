//! Operations layer for EngramDB.
//!
//! This module provides shared operation functions that both the CLI and MCP server
//! call into. Each operation takes typed input parameters and returns typed output
//! results — no CLI formatting or MCP serialization happens here.

pub mod challenge;
pub mod compress;
pub mod create;
pub mod delete;
pub mod gc;
pub mod get;
pub mod parsing;
pub mod projects;
pub mod reindex;
pub mod resolve;
pub mod retrieve;
pub mod review;
pub mod search;
pub mod stats;
pub mod update;

pub use challenge::{challenge_memory, ChallengeResult};
pub use compress::{
    compress_apply, compress_candidates, CompressApplyResult, CompressCandidate,
    CompressCandidatesResult,
};
pub use create::{create_memory, CreateParams, CreateResult};
pub use delete::delete_memory;
pub use gc::{gc_memories, GcResult};
pub use get::get_memory;
pub use parsing::{parse_decay_strategy, parse_memory_type, parse_status, parse_visibility};
pub use reindex::{reindex, ReindexResult};
pub use resolve::{resolve_memory, ResolveAction, ResolveParams, ResolveResult};
pub use retrieve::retrieve_memories;
pub use review::review_memories;
pub use search::search_memories;
pub use stats::{compute_stats, StoreStats};
pub use update::{update_memory, UpdateParams};

use crate::embeddings::{
    EmbeddingProvider, OllamaProvider, OnnxProvider, ALL_MINILM, MXBAI_EMBED_LARGE,
    NOMIC_EMBED_TEXT,
};
use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;

/// Try to create an embedding provider for the given model name.
/// For models with multiple backends, tries them in priority order.
fn resolve_provider(model: &str) -> Option<Box<dyn EmbeddingProvider>> {
    match model {
        "onnx" | "all-minilm" => {
            // Prefer local ONNX (no server needed), fall back to Ollama
            if let Some(p) = OnnxProvider::try_new() {
                return Some(Box::new(p));
            }
            OllamaProvider::try_new(ALL_MINILM).map(|p| Box::new(p) as _)
        }
        "nomic-embed-text" => OllamaProvider::try_new(NOMIC_EMBED_TEXT).map(|p| Box::new(p) as _),
        "mxbai-embed-large" => OllamaProvider::try_new(MXBAI_EMBED_LARGE).map(|p| Box::new(p) as _),
        other => {
            eprintln!(
                "Warning: unknown embedding model '{}', embeddings disabled",
                other
            );
            None
        }
    }
}

/// Build a `RetrievalEngine` with optional embeddings from a store + config path.
///
/// This is the shared helper that CLI and MCP callers use so they don't duplicate
/// the provider wiring.  Returns an engine that always works for storage operations;
/// embeddings are attached on a best-effort basis.  Vector storage is handled
/// by the MemoryStore's integrated LanceDB.
pub async fn build_engine(store: MemoryStore, config_path: &std::path::Path) -> RetrievalEngine {
    let config = crate::storage::config::load_config(config_path)
        .await
        .unwrap_or_default();
    let mut engine = RetrievalEngine::new(store, config.clone());

    if let Some(provider) = resolve_provider(config.embeddings.provider.as_str()) {
        if provider.dimensions() != config.embeddings.dimensions {
            eprintln!(
                "Warning: provider dimensions ({}) != config dimensions ({})",
                provider.dimensions(),
                config.embeddings.dimensions
            );
        }
        engine = engine.with_embedding_provider(provider);
    }

    engine
}
