//! Display effective config values and store vocabulary.
//!
//! The CLI counterpart of the MCP `config` tool: it surfaces the limits and
//! thresholds that govern the other commands (summary/content sizing,
//! retrieval/search thresholds, which optional features are on) plus the
//! store's most-used tags. Same JSON shape as the MCP tool so scripts can
//! consume either interchangeably.

use crate::output::OutputFormatter;
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
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!("Limits");
    println!(
        "  summary max chars:      {}",
        view.limits.summary_max_chars
    );
    println!(
        "  content soft target:    {} tokens",
        view.limits.content_soft_token_target
    );
    println!(
        "  embedding chunk window: {} tokens (content is chunked; nothing is truncated)",
        view.limits.embedding_chunk_tokens
    );

    println!();
    println!("Retrieval / search");
    println!(
        "  default max results:    {}",
        view.retrieval.default_max_results
    );
    println!(
        "  relevance threshold:    {}",
        view.retrieval.relevance_threshold
    );
    println!(
        "  search threshold:       {}",
        view.retrieval.search_threshold
    );
    println!(
        "  search semantic weight: {}",
        view.retrieval.search_semantic_weight
    );
    println!(
        "  include expired:        {}",
        view.retrieval.include_expired
    );

    println!();
    println!("Features");
    println!(
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
    println!(
        "  contradiction check:    {}",
        if view.features.contradiction_detection_enabled {
            "on"
        } else {
            "off"
        }
    );
    println!(
        "  title strategy:         {}",
        serde_json::to_value(view.features.title_strategy)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    );

    println!();
    println!("Embedding");
    println!("  provider:               {}", view.embedding.provider);
    println!("  dimensions:             {}", view.embedding.dimensions);

    println!();
    if tags.is_empty() {
        println!("Top tags: (none yet)");
    } else {
        println!("Top tags (most used first)");
        for t in &tags {
            println!("  {:<24} {}", t.tag, t.count);
        }
    }

    Ok(())
}
