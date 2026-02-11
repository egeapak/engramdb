//! Search memories by keyword.

use crate::cli::output::OutputFormatter;
use crate::ops::parse_memory_type;
use crate::retrieval::filters::SearchFilters;
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::Result;
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
/// * `registry` - The registry backend to use for project registration
/// * `params` - Search query parameters
/// * `formatter` - Output formatter for displaying results
pub async fn run_search(
    dir: &Path,
    registry: &dyn RegistryBackend,
    params: SearchParams,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
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

    // Build search filters
    let filters = SearchFilters {
        types,
        tags,
        physical: params.physical,
        logical: params.logical.first().cloned(),
        min_criticality: params.min_criticality,
    };

    // Perform search
    let mut results = crate::ops::search_memories(&engine, &params.query, &filters).await?;

    // Apply max_results limit
    results.truncate(params.max_results);

    // Display results
    formatter.print_search_results(&results);

    Ok(())
}
