//! Rebuild index and re-embed memories.

use crate::cli::output::OutputFormatter;
use crate::embeddings::OnnxProvider;
use crate::ops::reindex;
use crate::retrieval::engine::RetrievalEngine;
use crate::storage::MemoryStore;
use crate::vector::LanceDbStore;
use anyhow::Result;
use std::path::Path;

/// Run reindex operation.
///
/// Rebuilds the index and optionally re-embeds memories based on flags.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `embeddings_only` - If true, only re-embed memories (skip index rebuild)
/// * `index_only` - If true, only rebuild index (skip embeddings)
/// * `formatter` - Output formatter for success/error messages
pub fn run_reindex(
    dir: &Path,
    embeddings_only: bool,
    index_only: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config(&config_path)?;

    // Set up engine with embeddings if not index_only
    let engine = if !index_only {
        let mut engine = RetrievalEngine::new(MemoryStore::open(dir)?, config);
        if let (Some(provider), Ok(lancedb_path)) = (
            OnnxProvider::try_new(),
            std::fs::canonicalize(crate::storage::paths::lancedb_dir(&store.project_id)),
        ) {
            if let Ok(vector_store) = LanceDbStore::new(lancedb_path, "memories".to_string(), 384) {
                engine = engine.with_embeddings(Box::new(provider), Box::new(vector_store));
            }
        }
        Some(engine)
    } else {
        None
    };

    let result = reindex(&store, engine.as_ref(), embeddings_only)?;

    if result.indexed > 0 {
        formatter.print_success(&format!("Indexed {} memories.", result.indexed));
    }
    if result.embedded > 0 {
        formatter.print_success(&format!("Embedded {} memories.", result.embedded));
    }
    if !result.errors.is_empty() {
        formatter.print_error(&format!("{} errors during reindex:", result.errors.len()));
        for err in &result.errors {
            eprintln!("  {}", err);
        }
    }
    if result.indexed == 0 && result.embedded == 0 && result.errors.is_empty() {
        formatter.print_message("Nothing to reindex.");
    }

    Ok(())
}
