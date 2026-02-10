//! Compress memories (v1: manual listing, no LLM summarization).

use crate::cli::output::OutputFormatter;
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

/// Run memory compression.
///
/// In v1, this lists eligible memories but does not perform LLM-based
/// summarization. Users can manually create summary memories.
pub fn run_compress(
    dir: &Path,
    scope: Option<String>,
    threshold: Option<f64>,
    confirm: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    let entries = store.list()?;

    // Filter by scope if provided
    let filtered: Vec<_> = entries
        .iter()
        .filter(|e| {
            if let Some(ref scope) = scope {
                e.logical.iter().any(|s| s == scope)
            } else {
                true
            }
        })
        .collect();

    let threshold = threshold.unwrap_or(0.2);

    if filtered.is_empty() {
        formatter.print_message("No memories found for compression.");
        return Ok(());
    }

    if !confirm {
        formatter.print_message(&format!(
            "Found {} memories in scope (threshold: {:.2}).",
            filtered.len(),
            threshold
        ));
        formatter.print_message(
            "LLM-based compression is not yet available. Use --confirm to list candidates.",
        );
        formatter.print_message(
            "You can manually create summary memories with 'engramdb add --type context'.",
        );
    } else {
        formatter.print_message(&format!(
            "Compression candidates ({} memories, threshold {:.2}):\n",
            filtered.len(),
            threshold
        ));
        for entry in &filtered {
            let id_short = &entry.id[..8.min(entry.id.len())];
            println!("  {} {:?}  {}", id_short, entry.type_, entry.summary);
        }
        formatter.print_message(
            "\nManual compression: create a new summary memory and delete the originals.",
        );
    }

    Ok(())
}
