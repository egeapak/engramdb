//! Compress command — lists candidates, directs users to MCP mode for actual compression.

use crate::cli::output::OutputFormatter;
use crate::ops;
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

/// List compression candidates and direct users to MCP mode.
pub fn run_compress(
    dir: &Path,
    scope: Option<String>,
    threshold: Option<f64>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    let result = ops::compress_candidates(&store, scope.as_deref(), threshold)?;

    if result.candidates.is_empty() {
        formatter.print_message("No compression candidates found.");
        return Ok(());
    }

    formatter.print_message(&format!(
        "Compression candidates ({} memories, threshold {:.2}):\n",
        result.total, result.threshold
    ));

    for candidate in &result.candidates {
        let id_short = &candidate.id[..8.min(candidate.id.len())];
        println!(
            "  {} {:8}  {} (criticality: {:.2})",
            id_short, candidate.type_, candidate.summary, candidate.criticality
        );
    }

    formatter.print_message(
        "\nCompression requires an LLM agent. Use MCP mode (engramdb serve) with a connected agent.",
    );

    Ok(())
}
