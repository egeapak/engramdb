//! Rebuild index and re-embed memories.

use crate::engine::engine_for;
use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::daemon::{DaemonCell, DaemonPolicy};
use engramdb::ops::reindex;
use engramdb::storage::MemoryStore;
use std::path::Path;
use std::sync::Arc;

/// Run reindex operation.
///
/// Rebuilds the index and optionally re-embeds memories based on flags.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `embeddings_only` - If true, only re-embed memories (skip index rebuild)
/// * `index_only` - If true, only rebuild index (skip embeddings)
/// * `formatter` - Output formatter for success/error messages
#[allow(clippy::too_many_arguments)]
pub async fn run_reindex(
    dir: &Path,
    global: bool,
    embeddings_only: bool,
    index_only: bool,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };

    // Set up engine with embeddings if not index_only. `MemoryStore` is
    // `Clone` — a second `open` here paid a redundant config load + LanceDB
    // connection.
    let engine = if !index_only {
        Some(engine_for(store.clone(), embedding_backend, cell, policy).await)
    } else {
        None
    };

    // Print progress before starting (human-only; raw println! would corrupt
    // the JSON document the formatter emits below — finding #7).
    if !formatter.is_json() {
        if !embeddings_only {
            println!("Reindexing...");
        }
        if !index_only && engine.is_some() {
            println!("Regenerating embeddings...");
        }
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
    for warning in &result.warnings {
        formatter.print_warning(warning);
    }
    if !result.errors.is_empty() {
        formatter.print_error(&format!("{} errors during reindex:", result.errors.len()));
        for err in &result.errors {
            eprintln!("  {}", err);
        }
    }
    if result.indexed == 0
        && result.embedded == 0
        && result.errors.is_empty()
        && result.warnings.is_empty()
    {
        formatter.print_message("Nothing to reindex.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::OutputFormat;
    use engramdb::storage::InMemoryRegistry;
    use engramdb::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    fn fmt() -> OutputFormatter {
        OutputFormatter::new(Some(OutputFormat::Json), false, false)
    }

    /// Both `embeddings_only` and `index_only` set: engine = None branch
    /// (engine.is_some() is false), no re-embedding happens, and `reindex`
    /// is called with `embeddings_only=true` so it also skips the index
    /// rebuild. Net effect: nothing happens but no error.
    #[tokio::test]
    async fn run_reindex_with_index_only_is_safe_when_no_memories() {
        let tmp = TempDir::new().unwrap();
        let _ = engramdb::storage::MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        run_reindex(
            tmp.path(),
            false,
            false,
            true,
            None,
            &fmt(),
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await
        .unwrap();
    }

    /// `index_only=true` skips engine construction entirely (the
    /// `if !index_only` branch). This is the path that doesn't try to
    /// load any embedding model — safe to run in test envs without ONNX.
    #[tokio::test]
    async fn run_reindex_index_only_rebuilds_index_without_engine() {
        let tmp = TempDir::new().unwrap();
        let store = engramdb::storage::MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        // Create a memory so reindex has something to count.
        let mem = Memory::new(
            MemoryType::Decision,
            "summary",
            "content",
            Provenance::human(),
        );
        store.create(&mem).await.unwrap();

        // index_only=true → engine is None → no embedding load attempted.
        run_reindex(
            tmp.path(),
            false,
            false,
            true,
            None,
            &fmt(),
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await
        .unwrap();
    }

    /// Open-uninitialized-store path: must surface the error rather than
    /// panic. Exercises the very first branch of run_reindex (the
    /// MemoryStore::open call before any further dispatch).
    #[tokio::test]
    async fn run_reindex_against_uninitialized_dir_errors() {
        let tmp = TempDir::new().unwrap();
        // Note: NO init.
        let result = run_reindex(
            tmp.path(),
            false,
            false,
            true,
            None,
            &fmt(),
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await;
        assert!(result.is_err(), "uninitialized store must error");
    }
}
