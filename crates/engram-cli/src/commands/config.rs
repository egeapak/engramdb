//! Display effective config values and store vocabulary.
//!
//! The CLI counterpart of the MCP `config` tool: it surfaces the limits and
//! thresholds that govern the other commands (summary/content sizing,
//! retrieval/search thresholds, which optional features are on) plus the
//! store's most-used tags. Same JSON shape as the MCP tool so scripts can
//! consume either interchangeably.

use crate::output::{outln, OutputFormatter};
use anyhow::Result;
use engramdb::ops::{top_tags, AgentConfigView, DEFAULT_TOP_TAGS};
use engramdb::storage::MemoryStore;
use std::path::Path;

/// Show effective config values and the store's top tags.
///
/// # Arguments
/// * `dir` - The project directory containing the EngramDB store
/// * `global` - Operate on the global (cross-project) store instead
/// * `top_tags_limit` - How many top tags to show (defaults to [`DEFAULT_TOP_TAGS`])
/// * `formatter` - Output formatter (pretty / plain / json)
pub async fn run_config(
    dir: &Path,
    global: bool,
    top_tags_limit: Option<usize>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };

    let config_path = store.project_dir.join(".engramdb/config.toml");
    let config = engramdb::storage::config::load_config_or_default(&config_path).await;
    let view = AgentConfigView::from_config(&config);

    let limit = top_tags_limit.unwrap_or(DEFAULT_TOP_TAGS);
    let tags = top_tags(&store, limit).await?;

    if formatter.is_json() {
        // Single JSON document, same shape as the MCP `config` tool.
        let mut payload = serde_json::to_value(&view)?;
        if let serde_json::Value::Object(ref mut obj) = payload {
            obj.insert("top_tags".to_string(), serde_json::to_value(&tags)?);
        }
        outln!(formatter, "{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    outln!(formatter, "Limits");
    outln!(
        formatter,
        "  summary max chars:      {}",
        view.limits.summary_max_chars
    );
    outln!(
        formatter,
        "  content soft target:    {} tokens",
        view.limits.content_soft_token_target
    );
    outln!(
        formatter,
        "  embedding chunk window: {} tokens (content is chunked; nothing is truncated)",
        view.limits.embedding_chunk_tokens
    );

    outln!(formatter);
    outln!(formatter, "Retrieval / search");
    outln!(
        formatter,
        "  default max results:    {}",
        view.retrieval.default_max_results
    );
    outln!(
        formatter,
        "  relevance threshold:    {}",
        view.retrieval.relevance_threshold
    );
    outln!(
        formatter,
        "  search threshold:       {}",
        view.retrieval.search_threshold
    );
    outln!(
        formatter,
        "  search semantic weight: {}",
        view.retrieval.search_semantic_weight
    );
    outln!(
        formatter,
        "  include expired:        {}",
        view.retrieval.include_expired
    );

    outln!(formatter);
    outln!(formatter, "Features");
    outln!(
        formatter,
        "  rerank:                 {}{}",
        if view.features.rerank_enabled {
            "on"
        } else {
            "off"
        },
        if view.features.rerank_enabled {
            format!(" (top {})", view.features.rerank_top_n)
        } else {
            String::new()
        }
    );
    outln!(
        formatter,
        "  contradiction check:    {}",
        if view.features.contradiction_detection_enabled {
            "on"
        } else {
            "off"
        }
    );
    outln!(
        formatter,
        "  title strategy:         {}",
        serde_json::to_value(view.features.title_strategy)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    );

    outln!(formatter);
    outln!(formatter, "Embedding");
    outln!(
        formatter,
        "  provider:               {}",
        view.embedding.provider
    );
    outln!(
        formatter,
        "  dimensions:             {}",
        view.embedding.dimensions
    );

    outln!(formatter);
    outln!(formatter, "Index currency");
    outln!(
        formatter,
        "  staleness check:        {}{}",
        view.index.staleness_check,
        // The budget only means something at the tier that hashes, so naming
        // it under `counts`/`size` would suggest a bound that is not in play.
        if view.index.staleness_check == "content" {
            format!(" (up to {} bytes)", view.index.staleness_max_bytes)
        } else {
            String::new()
        }
    );

    outln!(formatter);
    if tags.is_empty() {
        outln!(formatter, "Top tags: (none yet)");
    } else {
        outln!(formatter, "Top tags (most used first)");
        for t in &tags {
            outln!(formatter, "  {:<24} {}", t.tag, t.count);
        }
    }

    Ok(())
}
