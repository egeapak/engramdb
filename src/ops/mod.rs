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

use crate::embeddings::OnnxProvider;
use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;
use crate::vector::LanceDbStore;

/// Build a `RetrievalEngine` with optional embeddings from a store + config path.
///
/// This is the shared helper that CLI and MCP callers use so they don't duplicate
/// the ONNX / LanceDB wiring.  Returns an engine that always works for storage
/// operations; embeddings are attached on a best-effort basis.
pub fn build_engine(store: MemoryStore, config_path: &std::path::Path) -> RetrievalEngine {
    let config = crate::storage::config::load_config(config_path).unwrap_or_default();
    let project_id = store.project_id.clone();
    let mut engine = RetrievalEngine::new(store, config);

    if let Some(provider) = OnnxProvider::try_new() {
        if let Some(lance_path) = crate::storage::paths::lancedb_dir(&project_id)
            .ok()
            .and_then(|p| std::fs::canonicalize(p).ok())
        {
            if let Ok(vector_store) = LanceDbStore::new(lance_path, "memories".to_string(), 384) {
                engine = engine.with_embeddings(Box::new(provider), Box::new(vector_store));
            }
        }
    }

    engine
}
