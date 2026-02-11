//! Display statistics about the memory store.

use crate::cli::output::{OutputFormatter, Stats};
use crate::embeddings::OnnxProvider;
use crate::ops::compute_stats;
use crate::storage::{MemoryStore, RegistryBackend};
use crate::types::Status;
use anyhow::Result;
use std::path::Path;

/// Display statistics about the memory store.
///
/// Shows total memory count, breakdown by type and status, logical scopes,
/// and average criticality.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `formatter` - Output formatter for displaying statistics
pub async fn run_stats(
    dir: &Path,
    registry: &dyn RegistryBackend,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
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

    let stats = Stats {
        total: store_stats.total,
        by_type: store_stats.by_type,
        by_status: store_stats.by_status,
        by_scope: store_stats.by_scope,
        expired: store_stats.expired,
        oldest: store_stats.oldest,
        newest: store_stats.newest,
        avg_criticality: store_stats.avg_criticality,
    };

    formatter.print_stats(&stats);

    // Print embeddings status
    println!();
    let embeddings_available = OnnxProvider::try_new().is_some();
    if embeddings_available {
        println!("Embeddings: Available");
    } else {
        println!("Embeddings: Not available");
    }

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
