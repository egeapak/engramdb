//! Search memories by keyword.

use crate::cli::output::OutputFormatter;
use crate::ops::parse_memory_type;
use crate::retrieval::filters::SearchFilters;
use crate::storage::MemoryStore;
use anyhow::Result;
use owo_colors::{OwoColorize, Stream};
use std::io::{self, IsTerminal};
use std::path::Path;

/// Parameters for the search command.
pub struct SearchParams {
    pub query: String,
    pub type_filter: Vec<String>,
    pub tags: Vec<String>,
    pub physical: Option<String>,
    pub logical: Vec<String>,
    pub min_criticality: Option<f64>,
    pub max_results: usize,
}

/// Search memories by keyword.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `params` - Search query parameters
/// * `formatter` - Output formatter for displaying results
pub fn run_search(dir: &Path, params: SearchParams, formatter: &OutputFormatter) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    if let Ok(Some(warning)) = store.check_staleness() {
        formatter.print_warning(&warning);
    }
    let config_path = dir.join(".engramdb").join("config.toml");
    let engine = crate::ops::build_engine(store, &config_path);

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

    // Build search filters
    let filters = SearchFilters {
        types,
        tags,
        physical: params.physical,
        logical: params.logical.first().cloned(),
        min_criticality: params.min_criticality,
    };

    // Perform search
    let mut results = crate::ops::search_memories(&engine, &params.query, &filters)?;

    // Apply max_results limit
    results.truncate(params.max_results);

    // Display results
    display_search_results(&results, formatter)?;

    Ok(())
}

/// Display search results in the appropriate format
fn display_search_results(
    results: &[crate::retrieval::engine::ScoredMemory],
    formatter: &OutputFormatter,
) -> Result<()> {
    // Check if we're in JSON mode
    let is_tty = io::stdout().is_terminal();

    if !is_tty {
        // JSON mode
        let json_output = results
            .iter()
            .map(|sm| {
                serde_json::json!({
                    "memory": sm.memory,
                    "score": sm.score,
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        // Pretty mode
        if results.is_empty() {
            formatter.print_message("No memories found.");
        } else {
            formatter.print_message(&format!("Found {} memories:\n", results.len()));

            let use_color = is_tty;

            for sm in results {
                let id_short = &sm.memory.id[..8.min(sm.memory.id.len())];
                let score_str = format!("[{:.2}]", sm.score);
                let type_str = format!("{:?}", sm.memory.type_);

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
            }
        }
    }

    Ok(())
}
