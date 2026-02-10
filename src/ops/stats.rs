//! Compute store statistics operation.

use crate::storage::MemoryStore;
use crate::types::{MemoryType, Status};
use anyhow::Result;
use std::collections::{HashMap, HashSet};

/// Statistics about the memory store.
pub struct StoreStats {
    pub total: usize,
    pub by_type: Vec<(MemoryType, usize)>,
    pub by_status: Vec<(Status, usize)>,
    pub logical_scopes: Vec<String>,
    pub avg_criticality: f64,
}

/// Compute statistics for the memory store.
pub fn compute_stats(store: &MemoryStore) -> Result<StoreStats> {
    let entries = store.list()?;
    let total = entries.len();

    let mut type_counts: HashMap<MemoryType, usize> = HashMap::new();
    for entry in &entries {
        *type_counts.entry(entry.type_).or_insert(0) += 1;
    }
    let mut by_type: Vec<_> = type_counts.into_iter().collect();
    by_type.sort_by_key(|(t, _)| format!("{:?}", t));

    let mut status_counts: HashMap<Status, usize> = HashMap::new();
    for entry in &entries {
        *status_counts.entry(entry.status).or_insert(0) += 1;
    }
    let mut by_status: Vec<_> = status_counts.into_iter().collect();
    by_status.sort_by_key(|(s, _)| format!("{:?}", s));

    let logical_scopes: Vec<String> = entries
        .iter()
        .flat_map(|e| e.logical.iter().cloned())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let avg_criticality = if total > 0 {
        entries.iter().map(|e| e.criticality).sum::<f64>() / total as f64
    } else {
        0.0
    };

    Ok(StoreStats {
        total,
        by_type,
        by_status,
        logical_scopes,
        avg_criticality,
    })
}
