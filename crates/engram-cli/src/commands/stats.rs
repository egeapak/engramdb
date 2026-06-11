//! Display statistics about the memory store.

use crate::output::{OutputFormatter, Stats};
use anyhow::Result;
#[cfg(feature = "ollama")]
use engramdb::embeddings::{OllamaProvider, ALL_MINILM, MXBAI_EMBED_LARGE, NOMIC_EMBED_TEXT};
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
    let cfg =
        engramdb::storage::config::load_config(&store.project_dir.join(".engramdb/config.toml"))
            .await
            .unwrap_or_default();
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

    // Print embeddings status
    println!();
    let config_path = store.project_dir.join(".engramdb/config.toml");
    let config = engramdb::storage::config::load_config(&config_path)
        .await
        .unwrap_or_default();
    let model = config.embeddings.provider.as_str();
    let backend = engramdb::ops::resolve_backend(config.embeddings.backend, embedding_backend);
    print_embeddings_status(model, backend).await;

    if challenged_count > 0 || needs_review_count > 0 {
        println!();
        println!("Health Warnings:");
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
    let cfg = engramdb::storage::config::load_config(&dir.join(".engramdb").join("config.toml"))
        .await
        .unwrap_or_default();
    let socket = engramdb::daemon::resolve_socket(None, &cfg.daemon);
    if let Some(s) = engramdb::daemon::query_status(&socket).await? {
        formatter.print_success(&format!("Embedding daemon: running (pid {})", s.pid));
        println!("  socket:        {}", socket.display());
        println!("  protocol:      v{}", s.version);
        println!("  uptime:        {}s", s.uptime_secs);
        println!("  idle:          {}s", s.idle_secs);
        println!("  model bundles: {}", s.bundles_loaded);
        println!("  requests (cumulative across restarts):");
        println!("    embed:       {}", s.requests_embed);
        println!("    classify:    {}", s.requests_classify);
        println!("    rerank:      {}", s.requests_rerank);
        println!("    meta:        {}", s.requests_meta);
        println!("    status:      {}", s.requests_status);
        println!("    title:       {}", s.requests_title);
        println!("    total:       {}", s.requests_total);
        return Ok(());
    }

    match engramdb::daemon::metrics::load_latest().await {
        Some(p) => {
            formatter.print_message("Embedding daemon: not running (last persisted snapshot)");
            println!("  requests (cumulative across restarts):");
            for row in persisted_snapshot_rows(&p.snapshot) {
                println!("{row}");
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
async fn print_embeddings_status(model: &str, backend: EmbeddingBackend) {
    let onnx_spec = match model {
        "onnx" | "all-minilm" => None, // uses default OnnxProvider::try_new()
        "nomic-embed-text" => Some(ONNX_NOMIC_EMBED_TEXT),
        "mxbai-embed-large" => Some(ONNX_MXBAI_EMBED_LARGE),
        other => {
            println!("Embeddings: Not available (unknown provider '{}')", other);
            return;
        }
    };

    let display_name = match model {
        "onnx" => "all-minilm",
        other => other,
    };

    // Check ONNX if backend allows it
    if backend != EmbeddingBackend::Ollama {
        let available = match &onnx_spec {
            None => OnnxProvider::try_new().is_some(),
            Some(spec) => OnnxProvider::try_with_model(spec.clone()).is_some(),
        };
        if available {
            println!("Embeddings: Available ({} via ONNX)", display_name);
            return;
        }
        if backend == EmbeddingBackend::Onnx {
            println!("Embeddings: Not available (run 'engramdb init' to download model)");
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
                    println!("Embeddings: Available ({} via Ollama)", display_name);
                    return;
                }
                Ok(false) => {
                    println!("Embeddings: Not available (run 'engramdb init' to download model)");
                    return;
                }
                Err(_) => {}
            }
        }
    }

    println!("Embeddings: Not available (run 'engramdb init' to download model)");
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
