//! Display statistics about the memory store.

use crate::cli::output::{OutputFormatter, Stats};
use crate::ops::compute_stats;
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

/// Display statistics about the memory store.
///
/// Shows total memory count, breakdown by type and status, logical scopes,
/// and average criticality.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `formatter` - Output formatter for displaying statistics
pub fn run_stats(dir: &Path, formatter: &OutputFormatter) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    let store_stats = compute_stats(&store)?;

    let stats = Stats {
        total: store_stats.total,
        by_type: store_stats.by_type,
        by_status: store_stats.by_status,
        logical_scopes: store_stats.logical_scopes,
        avg_criticality: store_stats.avg_criticality,
    };

    formatter.print_stats(&stats);
    Ok(())
}
