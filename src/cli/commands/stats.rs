use crate::cli::output::{OutputFormatter, Stats};
use crate::storage::MemoryStore;
use crate::types::{MemoryType, Status};
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub fn run_stats(dir: &Path, formatter: &OutputFormatter) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    let entries = store.list()?;

    let total = entries.len();

    // Count by type
    let mut type_counts: HashMap<MemoryType, usize> = HashMap::new();
    for entry in &entries {
        *type_counts.entry(entry.type_).or_insert(0) += 1;
    }
    let mut by_type: Vec<_> = type_counts.into_iter().collect();
    by_type.sort_by_key(|(t, _)| format!("{:?}", t));

    // Count by status
    let mut status_counts: HashMap<Status, usize> = HashMap::new();
    for entry in &entries {
        *status_counts.entry(entry.status).or_insert(0) += 1;
    }
    let mut by_status: Vec<_> = status_counts.into_iter().collect();
    by_status.sort_by_key(|(s, _)| format!("{:?}", s));

    // Collect logical scopes
    let logical_scopes: Vec<String> = entries
        .iter()
        .flat_map(|e| e.logical.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    // Calculate average criticality
    let avg_criticality = if total > 0 {
        entries.iter().map(|e| e.criticality).sum::<f64>() / total as f64
    } else {
        0.0
    };

    let stats = Stats {
        total,
        by_type,
        by_status,
        logical_scopes,
        avg_criticality,
    };

    formatter.print_stats(&stats);
    Ok(())
}
