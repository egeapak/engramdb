//! Display statistics about the memory store.

use crate::output::{outln, OutputFormatter, Stats};
use anyhow::Result;
#[cfg(feature = "ollama")]
use engramdb::embeddings::{OllamaProvider, ALL_MINILM, MXBAI_EMBED_LARGE, NOMIC_EMBED_TEXT};
#[cfg(feature = "onnxruntime")]
use engramdb::embeddings::{OnnxProvider, ONNX_MXBAI_EMBED_LARGE, ONNX_NOMIC_EMBED_TEXT};
use engramdb::ops::compute_stats;
use engramdb::storage::MemoryStore;
use engramdb::telemetry::StatsCollector;
use engramdb::types::{EmbeddingBackend, Status};
use std::path::Path;

/// Display statistics about the memory store.
///
/// Shows total memory count, breakdown by type and status, logical scopes,
/// average criticality, and (when available) runtime telemetry hydrated
/// from the persisted per-project snapshot — usage counts, response times,
/// hit rate, zero-result count.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `embedding_backend` - Optional embedding backend selection
/// * `all_projects` - When true, include the cross-project telemetry breakdown
/// * `formatter` - Output formatter for displaying statistics
pub async fn run_stats(
    dir: &Path,
    global: bool,
    daemon: bool,
    embedding_backend: Option<EmbeddingBackend>,
    all_projects: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    if daemon {
        return run_daemon_stats(dir, formatter).await;
    }

    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
    let store_stats = compute_stats(&store).await?;

    // Extract health warning counts before moving data into Stats
    let challenged_count = store_stats
        .by_status
        .iter()
        .find(|(s, _)| matches!(s, Status::Challenged))
        .map(|(_, count)| *count)
        .unwrap_or(0);

    let needs_review_count = store_stats
        .by_status
        .iter()
        .find(|(s, _)| matches!(s, Status::NeedsReview))
        .map(|(_, count)| *count)
        .unwrap_or(0);

    // Hydrate runtime telemetry from the persisted per-project snapshot. The
    // CLI is process-scoped so we won't see in-flight counters from a running
    // MCP server, but we do see counters that the server has flushed to disk
    // (default flush interval 60s + on shutdown).
    let cfg = engramdb::storage::config::load_config_or_default(
        &store.project_dir.join(".engramdb/config.toml"),
    )
    .await;
    let collector = StatsCollector::new(cfg.stats);
    let _ = engramdb::telemetry::persistence::hydrate_collector(&collector).await;
    let project_id = store.project_id.clone();
    let runtime = collector.snapshot(&project_id, all_projects);
    let runtime_present = runtime.view.usage.total_calls > 0
        || runtime.view.queries.total > 0
        || !runtime.view.timings_ms.tool.is_empty()
        || runtime.by_project.as_ref().is_some_and(|m| !m.is_empty());

    let stats = Stats {
        total: store_stats.total,
        by_type: store_stats.by_type,
        by_status: store_stats.by_status,
        by_scope: store_stats.by_scope,
        expired: store_stats.expired,
        oldest: store_stats.oldest,
        newest: store_stats.newest,
        avg_criticality: store_stats.avg_criticality,
        runtime: if runtime_present { Some(runtime) } else { None },
    };

    formatter.print_stats(&stats);

    // The embeddings-status and health-warning sections below are human-only
    // text. In JSON mode they would print raw lines after the JSON document and
    // corrupt it for scripted consumers, so suppress them entirely (finding #7).
    if !formatter.is_json() {
        // Print embeddings status
        outln!(formatter);
        let config_path = store.project_dir.join(".engramdb/config.toml");
        let config = engramdb::storage::config::load_config_or_default(&config_path).await;
        let model = config.embeddings.provider.as_str();
        let backend = engramdb::ops::resolve_backend(config.embeddings.backend, embedding_backend);
        print_embeddings_status(model, backend, formatter).await;

        // "What should I run next" is exactly the question this block answers,
        // and until now it could not mention the most actionable answer of all:
        // that the index is serving text the files no longer contain. Content
        // drift only — the vector half needs an engine, which `stats` has no
        // reason to build; `reindex --dry-run` reports both.
        let drifted_count = drifted_memory_count(&store).await;

        if challenged_count > 0 || needs_review_count > 0 || drifted_count > 0 {
            outln!(formatter);
            outln!(formatter, "Health Warnings:");
            if drifted_count > 0 {
                formatter.print_error(&format!(
                    "  {} {} changed since indexing (run 'engramdb reindex')",
                    drifted_count,
                    if drifted_count == 1 {
                        "memory"
                    } else {
                        "memories"
                    }
                ));
            }
            if challenged_count > 0 {
                formatter.print_error(&format!(
                    "  {} memories are challenged (run 'engramdb review --challenged-only')",
                    challenged_count
                ));
            }
            if needs_review_count > 0 {
                formatter.print_error(&format!(
                    "  {} memories need review (run 'engramdb review --stale-only')",
                    needs_review_count
                ));
            }
        }
    }

    Ok(())
}

/// Show the shared embedding daemon's cumulative request metrics.
///
/// Prefers a live query to the running daemon (authoritative, includes
/// in-flight counts); falls back to the last snapshot persisted to the global
/// LanceDB store when no daemon is currently running.
async fn run_daemon_stats(dir: &Path, formatter: &OutputFormatter) -> Result<()> {
    // `dir` is the dispatcher-resolved project directory (`--dir` or cwd),
    // matching every other command — not a second `current_dir()` lookup
    // that would ignore an explicit `--dir`.
    let cfg = engramdb::storage::config::load_config_or_default(
        &dir.join(".engramdb").join("config.toml"),
    )
    .await;
    let socket = engramdb::daemon::resolve_socket(None, &cfg.daemon);
    // A live query failure (e.g. a protocol-version mismatch with an older
    // daemon) must NOT abort the command — fall back to the persisted snapshot
    // exactly as the not-running case does (findings #8 graceful fallback).
    // `.ok().flatten()` collapses both `Err(_)` and `Ok(None)` to "no live
    // status".
    let live = engramdb::daemon::query_status(&socket).await.ok().flatten();

    if let Some(s) = live {
        if formatter.is_json() {
            // Emit a single JSON object so scripted consumers can parse it
            // (finding #7) — raw println! lines would corrupt the stream.
            outln!(
                formatter,
                "{}",
                crate::output::daemon_status_json(&s, &socket)
            );
            return Ok(());
        }
        formatter.print_success(&format!("Embedding daemon: running (pid {})", s.pid));
        outln!(formatter, "  socket:        {}", socket.display());
        outln!(formatter, "  protocol:      v{}", s.version);
        outln!(formatter, "  uptime:        {}s", s.uptime_secs);
        outln!(formatter, "  idle:          {}s", s.idle_secs);
        outln!(formatter, "  model bundles: {}", s.bundles_loaded);
        outln!(formatter, "  requests (cumulative across restarts):");
        outln!(formatter, "    embed:       {}", s.requests.embed);
        outln!(formatter, "    classify:    {}", s.requests.classify);
        outln!(formatter, "    rerank:      {}", s.requests.rerank);
        outln!(formatter, "    meta:        {}", s.requests.meta);
        outln!(formatter, "    status:      {}", s.requests.status);
        outln!(formatter, "    title:       {}", s.requests.title);
        outln!(formatter, "    total:       {}", s.requests.total);
        return Ok(());
    }

    let persisted = engramdb::daemon::metrics::load_latest().await;
    if formatter.is_json() {
        let requests = persisted.as_ref().map(|p| {
            let s = &p.snapshot;
            serde_json::json!({
                "embed": s.embed, "classify": s.classify, "rerank": s.rerank,
                "meta": s.meta, "status": s.status, "title": s.title, "total": s.total(),
            })
        });
        outln!(
            formatter,
            "{}",
            serde_json::json!({ "running": false, "requests": requests })
        );
        return Ok(());
    }

    match persisted {
        Some(p) => {
            formatter.print_message("Embedding daemon: not running (last persisted snapshot)");
            outln!(formatter, "  requests (cumulative across restarts):");
            for row in persisted_snapshot_rows(&p.snapshot) {
                outln!(formatter, "{row}");
            }
        }
        None => {
            formatter.print_message("Embedding daemon: not running and no metrics persisted yet.");
            formatter.print_message(
                "It is auto-spawned on demand by the next MCP run when [daemon] is enabled.",
            );
        }
    }
    Ok(())
}

/// Render the per-op request rows for a persisted daemon metrics snapshot.
///
/// Every counter in [`MetricsSnapshot`] must appear here — the per-op rows
/// sum to the `total` row (pinned by a unit test), so the `stats --daemon`
/// fallback view never silently drops a counter the live view reports.
fn persisted_snapshot_rows(s: &engramdb::daemon::metrics::MetricsSnapshot) -> Vec<String> {
    vec![
        format!("    embed:       {}", s.embed),
        format!("    classify:    {}", s.classify),
        format!("    rerank:      {}", s.rerank),
        format!("    meta:        {}", s.meta),
        format!("    status:      {}", s.status),
        format!("    title:       {}", s.title),
        format!("    total:       {}", s.total()),
    ]
}

/// Print the embeddings availability status for the given model name and backend.
async fn print_embeddings_status(
    model: &str,
    backend: EmbeddingBackend,
    formatter: &OutputFormatter,
) {
    if !matches!(
        model,
        "onnx" | "all-minilm" | "nomic-embed-text" | "mxbai-embed-large"
    ) {
        outln!(
            formatter,
            "Embeddings: Not available (unknown provider '{}')",
            model
        );
        return;
    }

    let display_name = match model {
        "onnx" => "all-minilm",
        other => other,
    };

    // Check the local ONNX Runtime engine if this build has it and the backend
    // allows it.
    #[cfg(feature = "onnxruntime")]
    if backend != EmbeddingBackend::Ollama {
        let available = match model {
            "nomic-embed-text" => OnnxProvider::try_with_model(ONNX_NOMIC_EMBED_TEXT).is_some(),
            "mxbai-embed-large" => OnnxProvider::try_with_model(ONNX_MXBAI_EMBED_LARGE).is_some(),
            _ => OnnxProvider::try_new().is_some(),
        };
        if available {
            outln!(
                formatter,
                "Embeddings: Available ({} via ONNX)",
                display_name
            );
            return;
        }
        if backend == EmbeddingBackend::Onnx {
            outln!(
                formatter,
                "Embeddings: Not available (run 'engramdb init' to download model)"
            );
            return;
        }
    }

    // Check Ollama if backend allows it
    #[cfg(feature = "ollama")]
    if backend != EmbeddingBackend::Onnx {
        let ollama_spec = match model {
            "onnx" | "all-minilm" => ALL_MINILM,
            "nomic-embed-text" => NOMIC_EMBED_TEXT,
            _ => MXBAI_EMBED_LARGE,
        };
        if let Some(provider) = OllamaProvider::try_new(ollama_spec) {
            match provider.check_model_available().await {
                Ok(true) => {
                    outln!(
                        formatter,
                        "Embeddings: Available ({} via Ollama)",
                        display_name
                    );
                    return;
                }
                Ok(false) => {
                    outln!(
                        formatter,
                        "Embeddings: Not available (run 'engramdb init' to download model)"
                    );
                    return;
                }
                Err(_) => {}
            }
        }
    }

    outln!(
        formatter,
        "Embeddings: Not available (run 'engramdb init' to download model)"
    );
}

/// How many indexed rows no longer match the file they were built from.
///
/// Best-effort by design: `stats` is a report, and a store whose digests
/// cannot be read is not a reason to fail it — the count simply drops to zero
/// and the other warnings still print. `doctor` and `reindex --dry-run` are
/// the surfaces that distinguish "clean" from "could not tell".
///
/// Returns 0 under a checkout conflict, where two checkouts legitimately hold
/// different bytes for one id and the drift is not a fault any reindex clears.
async fn drifted_memory_count(store: &MemoryStore) -> usize {
    if store.checkout_conflict().await.is_some() {
        return 0;
    }
    let Ok(rows) = store.index_digests().await else {
        return 0;
    };
    let mut drifted = 0;
    for row in rows {
        let Some(recorded) = row.content_sha256.as_deref() else {
            continue;
        };
        if let Ok(Some(bytes)) = store.read_memory_bytes(&row.memory_id).await {
            if engramdb::storage::FileDigest::of(&bytes).sha256 != recorded {
                drifted += 1;
            }
        }
    }
    drifted
}

#[cfg(test)]
mod tests {
    use super::*;
    use engramdb::daemon::metrics::MetricsSnapshot;

    /// The `stats --daemon` fallback (persisted-snapshot) view must render
    /// every counter — including `title`, which it used to omit — and the
    /// per-op rows must sum to the `total` row.
    #[test]
    fn persisted_snapshot_rows_include_title_and_sum_to_total() {
        let s = MetricsSnapshot {
            embed: 1,
            classify: 2,
            rerank: 3,
            meta: 4,
            status: 5,
            title: 6,
        };
        let rows = persisted_snapshot_rows(&s);

        assert!(
            rows.iter().any(|r| r.contains("title:")),
            "fallback view must include the title counter: {rows:?}"
        );

        let value = |row: &String| {
            row.split_whitespace()
                .last()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        };
        let (total_row, per_op) = rows.split_last().unwrap();
        assert!(total_row.contains("total:"), "last row must be the total");
        let per_op_sum: u64 = per_op.iter().map(value).sum();
        assert_eq!(
            per_op_sum,
            value(total_row),
            "per-op rows must sum to total"
        );
        assert_eq!(value(total_row), s.total());
    }
}
