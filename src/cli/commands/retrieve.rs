//! Retrieve memories by context.

use crate::cli::output::OutputFormatter;
use crate::ops::parse_memory_type;
use crate::retrieval::engine::{DetailLevel, RetrievalQuery};
use crate::storage::MemoryStore;
use anyhow::Result;
use owo_colors::{OwoColorize, Stream};
use std::io::{self, IsTerminal};
use std::path::Path;

/// Parameters for the retrieve command.
pub struct RetrieveParams {
    pub path: Option<String>,
    pub logical: Vec<String>,
    pub query: Option<String>,
    pub type_filter: Vec<String>,
    pub tags: Vec<String>,
    pub min_criticality: Option<f64>,
    pub max_results: usize,
    pub detail_level: Option<String>,
    pub include_expired: bool,
    pub show_scores: bool,
}

/// Retrieve memories based on context and query.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `params` - Retrieval query parameters
/// * `formatter` - Output formatter for displaying results
pub async fn run_retrieve(
    dir: &Path,
    params: RetrieveParams,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir).await?;
    if let Ok(Some(warning)) = store.check_staleness().await {
        formatter.print_warning(&warning);
    }
    let config_path = dir.join(".engramdb").join("config.toml");
    let engine = crate::ops::build_engine(store, &config_path).await;

    // Parse type filters
    let types = if !params.type_filter.is_empty() {
        let parsed_types: Result<Vec<_>> = params
            .type_filter
            .iter()
            .map(|s| parse_memory_type(s))
            .collect();
        Some(parsed_types?)
    } else {
        None
    };

    // Parse tags
    let tags = if !params.tags.is_empty() {
        Some(params.tags)
    } else {
        None
    };

    // Parse detail_level
    let detail_level = if let Some(ref level_str) = params.detail_level {
        match level_str.to_lowercase().as_str() {
            "summary" => DetailLevel::Summary,
            "content" => DetailLevel::Content,
            "full" => DetailLevel::Full,
            _ => {
                return Err(anyhow::anyhow!(
                    "Invalid detail level: {}. Must be summary, content, or full",
                    level_str
                ))
            }
        }
    } else {
        DetailLevel::Content
    };

    // Build retrieval query
    let query = RetrievalQuery {
        path: params.path,
        logical: params.logical,
        query: params.query,
        types,
        tags,
        min_criticality: params.min_criticality,
        max_results: Some(params.max_results),
        include_expired: Some(params.include_expired),
        detail_level,
    };

    // Perform retrieval
    let result = crate::ops::retrieve_memories(&engine, &query).await?;

    // Display results
    display_retrieval_result(&result, params.show_scores, formatter)?;

    Ok(())
}

/// Display retrieval results in the appropriate format
fn display_retrieval_result(
    result: &crate::retrieval::engine::RetrievalResult,
    show_scores: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    // Check if we're in JSON mode by checking if formatter would use JSON
    let is_tty = io::stdout().is_terminal();

    if !is_tty {
        // JSON mode
        let json_output = serde_json::json!({
            "memories": result.memories.iter().map(|sm| {
                serde_json::json!({
                    "memory": sm.memory,
                    "score": sm.score,
                })
            }).collect::<Vec<_>>(),
            "total": result.total,
        });
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        // Pretty mode
        if result.memories.is_empty() {
            formatter.print_message("No memories found.");
        } else {
            formatter.print_message(&format!(
                "Found {} memories (out of {} total):\n",
                result.memories.len(),
                result.total
            ));

            let use_color = is_tty;

            for sm in &result.memories {
                let id_short = &sm.memory.id[..8.min(sm.memory.id.len())];
                let type_str = format!("{:?}", sm.memory.type_);

                if show_scores {
                    let score_str = format!("[{:.2}]", sm.score);
                    if use_color {
                        println!(
                            "  {} {} {}  {}",
                            score_str.if_supports_color(Stream::Stdout, |text| text.green()),
                            id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                            type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                            sm.memory.summary
                        );
                    } else {
                        println!(
                            "  {} {} {}  {}",
                            score_str, id_short, type_str, sm.memory.summary
                        );
                    }
                } else if use_color {
                    println!(
                        "  {} {}  {}",
                        id_short.if_supports_color(Stream::Stdout, |text| text.cyan()),
                        type_str.if_supports_color(Stream::Stdout, |text| text.yellow()),
                        sm.memory.summary
                    );
                } else {
                    println!("  {} {}  {}", id_short, type_str, sm.memory.summary);
                }
            }
        }
    }

    Ok(())
}
