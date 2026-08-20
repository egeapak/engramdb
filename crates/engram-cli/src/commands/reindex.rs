//! Rebuild index and re-embed memories.

use crate::engine::engine_for;
use crate::output::{errln, outln, OutputFormatter};
use anyhow::Result;
use engramdb::daemon::{DaemonCell, DaemonPolicy};
use engramdb::ops::reindex;
use engramdb::storage::MemoryStore;
use std::path::Path;
use std::sync::Arc;

/// Which reindex to run, and over what.
///
/// `run_reindex` carried ten positional arguments behind a
/// `too_many_arguments` allow; the repo's convention for that is a
/// `<Cmd>Params` struct (`AddParams`, `QueryParams`, `UpdateParams`,
/// `DiscoverParams`), which also makes the five call sites read as named
/// fields rather than a row of bare booleans.
pub struct ReindexParams {
    pub global: bool,
    /// Only re-embed; skip the index rebuild.
    pub embeddings_only: bool,
    /// Only rebuild the index; skip embeddings.
    pub index_only: bool,
    /// Rebuild the conversation search rows instead of memories.
    pub archive_only: bool,
    /// Report what would change and write nothing.
    pub dry_run: bool,
    pub embedding_backend: Option<engramdb::types::EmbeddingBackend>,
}

impl ReindexParams {
    /// A plain full reindex — the shape most callers want.
    pub fn full(
        global: bool,
        embedding_backend: Option<engramdb::types::EmbeddingBackend>,
    ) -> Self {
        Self {
            global,
            embeddings_only: false,
            index_only: false,
            archive_only: false,
            dry_run: false,
            embedding_backend,
        }
    }
}

/// Run reindex operation.
///
/// Rebuilds the index and optionally re-embeds memories based on `params`.
pub async fn run_reindex(
    dir: &Path,
    registry: &dyn engramdb::storage::RegistryBackend,
    params: ReindexParams,
    formatter: &OutputFormatter,
    cell: &Arc<DaemonCell>,
    policy: DaemonPolicy,
) -> Result<()> {
    let ReindexParams {
        global,
        embeddings_only,
        index_only,
        archive_only,
        dry_run,
        embedding_backend,
    } = params;

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
    //
    // A dry run always wants one: the stale-vector half of the report is
    // exactly what needs a live provider to compute, and `--index-only`
    // narrows what a *rebuild* would touch, not what the report may look at.
    let engine = if !index_only || dry_run {
        Some(engine_for(store.clone(), embedding_backend, cell, policy).await)
    } else {
        None
    };

    if dry_run {
        return report_dry_run(&store, engine.as_ref(), formatter).await;
    }

    // Print progress before starting (human-only; raw println! would corrupt
    // the JSON document the formatter emits below — finding #7).
    //
    // `wants_human_stdout`, not `!is_json()`: `doctor --fix` calls this with a
    // delegate formatter that deliberately is NOT JSON, so the narrower guard
    // selected these lines and printed them onto doctor's JSON stdout.
    if formatter.wants_human_stdout() {
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

/// Render what a reindex would rebuild, having changed nothing.
///
/// Output goes through the formatter, never a bare print macro, and the human
/// lines are gated on `wants_human_stdout` rather than `!is_json()` — the same
/// rule the rebuild path documents, because `doctor --fix` reaches this module
/// with a delegate formatter that is deliberately not JSON.
async fn report_dry_run(
    store: &MemoryStore,
    engine: Option<&engramdb::retrieval::RetrievalEngine>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let plan = engramdb::ops::reindex_dry_run(store, engine).await?;

    if formatter.is_json() {
        outln!(
            formatter,
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "dry_run": true,
                "current": plan.is_current(),
                "on_disk": plan.on_disk,
                "indexed": plan.indexed,
                "not_indexed": plan.not_indexed,
                "drifted": plan.drifted,
                "stale_vectors": plan.stale_vectors,
                "not_embedded": plan.not_embedded,
                "undetermined": plan.undetermined,
                "without_digest": plan.without_digest,
                "embeddings_unavailable": plan.embeddings_unavailable,
            }))?
        );
        return Ok(());
    }

    if formatter.wants_human_stdout() {
        outln!(
            formatter,
            "Dry run — nothing was changed. {} memories on disk, {} indexed.",
            plan.on_disk,
            plan.indexed
        );
    }

    // Each line names the ids, capped, because "3 drifted" without saying
    // which is a report the user cannot act on without re-deriving it.
    let sections: [(&str, &Vec<String>); 5] = [
        ("not indexed", &plan.not_indexed),
        ("changed since indexing", &plan.drifted),
        ("vectors out of date", &plan.stale_vectors),
        ("not embedded", &plan.not_embedded),
        ("could not be checked", &plan.undetermined),
    ];
    // Header and ids go to the SAME stream, both stdout. Routing the header
    // through `print_warning` put it on stderr while its ids stayed on stdout,
    // so anyone reading stdout alone — a pipe, a redirect, a log — got a bare
    // list of uuids with nothing saying what was wrong with them. This is a
    // report the user asked for, not a diagnostic aside.
    if formatter.wants_human_stdout() {
        for (label, ids) in sections {
            if ids.is_empty() {
                continue;
            }
            outln!(formatter, "{} {}:", ids.len(), label);
            for id in ids.iter().take(DRY_RUN_ID_LIMIT) {
                outln!(formatter, "  {}", id);
            }
            if ids.len() > DRY_RUN_ID_LIMIT {
                // Naming the cap rather than trailing off: a truncated list
                // that does not say it is truncated reads as the whole answer.
                outln!(formatter, "  ... and {} more", ids.len() - DRY_RUN_ID_LIMIT);
            }
        }
    }

    if plan.embeddings_unavailable {
        formatter.print_warning(
            "no embedding provider available — vector currency was not checked. \
             Run `engramdb doctor` to fix the model cache.",
        );
    }
    if plan.without_digest > 0 {
        formatter.print_message(&format!(
            "{} rows predate the content digest; a reindex backfills them.",
            plan.without_digest
        ));
    }
    if plan.is_current() {
        formatter.print_success("Index is current with the files on disk.");
    } else {
        formatter.print_message("Run 'engramdb reindex' to rebuild.");
    }
    Ok(())
}

/// How many ids a dry run lists per category before summarizing the rest.
/// Enough to act on directly; short enough that a badly drifted store does not
/// scroll the terminal.
const DRY_RUN_ID_LIMIT: usize = 20;

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
        outln!(
            formatter,
            "Rebuilding conversation rows from stored transcript copies..."
        );
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
        outln!(
            formatter,
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
            ReindexParams {
                index_only: true,
                ..ReindexParams::full(false, None)
            },
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
            ReindexParams {
                index_only: true,
                ..ReindexParams::full(false, None)
            },
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
            ReindexParams {
                index_only: true,
                ..ReindexParams::full(false, None)
            },
            &fmt(),
            &Arc::new(DaemonCell::new()),
            DaemonPolicy::InProcess,
        )
        .await;
        assert!(result.is_err(), "uninitialized store must error");
    }
}
