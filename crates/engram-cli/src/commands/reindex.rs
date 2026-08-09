//! Rebuild index and re-embed memories.

use crate::engine::engine_for;
use crate::output::{errln, outln, OutputFormatter};
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
    registry: &dyn engramdb::storage::RegistryBackend,
    global: bool,
    embeddings_only: bool,
    index_only: bool,
    archive_only: bool,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> Result<()> {
    // A different index entirely: `--archive-only` rebuilds the conversation
    // search rows from the stored transcript copies and never touches a
    // memory, so it returns before the memories store is even opened.
    if archive_only {
        return run_archive_reindex(dir, registry, embedding_backend, formatter, cell, policy)
            .await;
    }
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
            outln!(formatter, "Reindexing...");
        }
        if !index_only && engine.is_some() {
            outln!(formatter, "Regenerating embeddings...");
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
            errln!(formatter, "  {}", err);
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

/// Rebuild every conversation search row from the stored transcript copies.
///
/// The payoff of keeping the copies verbatim: a better reduction, a different
/// embedding model or a changed tool heuristic is a re-derivation away, and
/// only because nothing was dropped at collect time. Curated summaries are
/// preserved — they are the one thing no rebuild can recreate.
async fn run_archive_reindex(
    dir: &Path,
    registry: &dyn engramdb::storage::RegistryBackend,
    embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    formatter: &OutputFormatter,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> Result<()> {
    let scope = engramdb::ops::harvest::session_scope(dir, registry).await?;
    let config = engramdb::storage::config::load_config_or_default(
        &dir.join(".engramdb").join("config.toml"),
    )
    .await;
    let store = MemoryStore::open(dir).await?;
    let engine = engine_for(store, embedding_backend, cell, policy).await;
    if !engine.embeddings_available() {
        anyhow::bail!(
            "embedding provider unavailable — refusing to rebuild the conversation index; \
             the existing rows are preserved. Fix the model cache (see `engramdb doctor`) \
             and retry."
        );
    }
    // Not a plain `open_index`: this is the documented remediation for a
    // conversations table whose stored vector width no longer matches the
    // configured one, and until it recreated that table it went through the
    // very `upsert` the mismatch breaks — so the advertised repair was the one
    // thing that could not repair it. Curated summaries are carried across and
    // re-attached below; they are the one thing a rebuild cannot recreate.
    let (index, carried_summaries) =
        engramdb::ops::harvest_index::open_index_for_rebuild(&scope, config.embeddings.dimensions)
            .await?;
    if !carried_summaries.is_empty() || index.dimensions() != config.embeddings.dimensions {
        formatter.print_warning(&format!(
            "The conversation table stored a different vector width, so it was recreated at {}; \
             {} curated summar{} carried across.",
            config.embeddings.dimensions,
            carried_summaries.len(),
            if carried_summaries.len() == 1 {
                "y is"
            } else {
                "ies are"
            }
        ));
    }

    if !formatter.is_json() {
        println!("Rebuilding conversation rows from stored transcript copies...");
    }
    let report = engramdb::ops::harvest_index::reindex_from_copies(&scope, &index, &engine).await?;
    // After the rows exist, because a summary has nowhere to go without one.
    let mut summary_errors: Vec<String> = Vec::new();
    for (session_id, summary) in &carried_summaries {
        if let Err(e) =
            engramdb::ops::set_conversation_summary(&index, &engine, session_id, summary).await
        {
            summary_errors.push(format!("{session_id}: {e:#}"));
        }
    }

    if formatter.is_json() {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "rebuilt": report.indexed,
                "skipped": report.skipped.iter().map(|s| serde_json::json!({
                    "session_id": s.session_id,
                    "reason": s.reason,
                })).collect::<Vec<_>>(),
                "summaries_carried": carried_summaries.len(),
                "summary_errors": summary_errors,
            }))?
        );
        return Ok(());
    }
    // Named, because a curated summary that failed to land is the one loss a
    // re-run cannot make good.
    for error in &summary_errors {
        formatter.print_warning(&format!("Could not re-attach a carried summary — {error}"));
    }
    if report.is_empty() {
        formatter.print_message(
            "No stored transcript copies to rebuild from. Copies are taken by the SessionEnd \
hook when `[harvest] archive` is on.",
        );
        return Ok(());
    }
    formatter.print_success(&format!(
        "Rebuilt {} conversation row(s).",
        report.indexed.len()
    ));
    // Named, not counted: a conversation missing from search is
    // indistinguishable from one that never mentioned the topic.
    for skipped in &report.skipped {
        formatter.print_warning(&format!("{}: {}", skipped.session_id, skipped.reason));
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
            &InMemoryRegistry::new(),
            false,
            false,
            true,
            false,
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
            &InMemoryRegistry::new(),
            false,
            false,
            true,
            false,
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
            &InMemoryRegistry::new(),
            false,
            false,
            true,
            false,
            None,
            &fmt(),
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await;
        assert!(result.is_err(), "uninitialized store must error");
    }
}
