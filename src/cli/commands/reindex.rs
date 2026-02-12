//! Rebuild index and re-embed memories.

use crate::cli::output::OutputFormatter;
use crate::ops::reindex;
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::Result;
use std::path::Path;

/// Run reindex operation.
///
/// Rebuilds the index and optionally re-embeds memories based on flags.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `embeddings_only` - If true, only re-embed memories (skip index rebuild)
/// * `index_only` - If true, only rebuild index (skip embeddings)
/// * `formatter` - Output formatter for success/error messages
pub async fn run_reindex(
    dir: &Path,
    registry: &dyn RegistryBackend,
    embeddings_only: bool,
    index_only: bool,
    embedding_backend: Option<crate::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
    let config_path = dir.join(".engramdb").join("config.toml");

    // Set up engine with embeddings if not index_only
    let engine = if !index_only {
        Some(
            crate::ops::build_engine(
                MemoryStore::open(dir, registry).await?,
                &config_path,
                embedding_backend,
            )
            .await,
        )
    } else {
        None
    };

    // Print progress before starting
    if !embeddings_only {
        println!("Reindexing...");
    }
    if !index_only && engine.is_some() {
        println!("Regenerating embeddings...");
    }

    let result = reindex(&store, engine.as_ref(), embeddings_only).await?;

    // Print results
    if result.indexed > 0 {
        formatter.print_success(&format!(
            "Done. Rebuilt index with {} entries.",
            result.indexed
        ));
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
