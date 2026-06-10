//! Rebuild index and re-embed memories.

use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::ops::{reindex, DaemonCell, DaemonPolicy};
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
pub async fn run_reindex(
    dir: &Path,
    global: bool,
    embeddings_only: bool,
    index_only: bool,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
) -> Result<()> {
    run_reindex_with_daemon(
        dir,
        global,
        embeddings_only,
        index_only,
        embedding_backend,
        formatter,
        None,
        DaemonPolicy::InProcess,
    )
    .await
}

/// Like [`run_reindex`] but routes model resolution through the shared daemon cell.
#[allow(clippy::too_many_arguments)]
pub async fn run_reindex_with_daemon(
    dir: &Path,
    global: bool,
    embeddings_only: bool,
    index_only: bool,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
    cell: Option<&Arc<DaemonCell>>,
    policy: DaemonPolicy,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
    let config_path = store.project_dir.join(".engramdb").join("config.toml");

    // Set up engine with embeddings if not index_only
    let engine = if !index_only {
        let engine_store = if global {
            MemoryStore::open_global().await?
        } else {
            MemoryStore::open(dir).await?
        };
        if let Some(c) = cell {
            let config = engramdb::storage::config::load_config(&config_path)
                .await
                .unwrap_or_default();
            let project_dir = engine_store.project_dir.clone();
            let providers = engramdb::ops::resolve_providers(
                c,
                &config,
                embedding_backend,
                &project_dir,
                policy,
            )
            .await;
            Some(engramdb::ops::assemble_engine(
                engine_store,
                config,
                providers,
            ))
        } else {
            Some(engramdb::ops::build_engine(engine_store, &config_path, embedding_backend).await)
        }
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

        run_reindex(tmp.path(), false, false, true, None, &fmt())
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
        run_reindex(tmp.path(), false, false, true, None, &fmt())
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
        let result = run_reindex(tmp.path(), false, false, true, None, &fmt()).await;
        assert!(result.is_err(), "uninitialized store must error");
    }
}
