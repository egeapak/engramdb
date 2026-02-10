//! Retrieve memories by context.

use crate::cli::output::OutputFormatter;
use crate::embeddings::OnnxProvider;
use crate::ops::parse_memory_type;
use crate::retrieval::engine::{DetailLevel, RetrievalEngine, RetrievalQuery};
use crate::storage::MemoryStore;
use crate::vector::LanceDbStore;
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
}

/// Retrieve memories based on context and query.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `params` - Retrieval query parameters
/// * `formatter` - Output formatter for displaying results
pub fn run_retrieve(dir: &Path, params: RetrieveParams, formatter: &OutputFormatter) -> Result<()> {
    // Open store and load config
    let store = MemoryStore::open(dir)?;
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config(&config_path)?;

    // Create retrieval engine
    let mut engine = RetrievalEngine::new(store, config);

    // Try to add embeddings (optional - proceed without if they fail)
    if let (Some(provider), Ok(lancedb_path)) = (
        OnnxProvider::try_new(),
        std::fs::canonicalize(crate::storage::paths::lancedb_dir(
            &engine.store().project_id,
        )),
    ) {
        if let Ok(vector_store) = LanceDbStore::new(lancedb_path, "memories".to_string(), 384) {
            engine = engine.with_embeddings(Box::new(provider), Box::new(vector_store));
        }
    }

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

    // Build retrieval query
    let query = RetrievalQuery {
        path: params.path,
        logical: params.logical,
        query: params.query,
        types,
        tags,
        min_criticality: params.min_criticality,
        max_results: Some(params.max_results),
        include_expired: None,
        detail_level: DetailLevel::Content,
    };

    // Perform retrieval
    let result = crate::ops::retrieve_memories(&engine, &query)?;

    // Display results
    display_retrieval_result(&result, formatter)?;

    Ok(())
}

/// Display retrieval results in the appropriate format
fn display_retrieval_result(
    result: &crate::retrieval::engine::RetrievalResult,
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
