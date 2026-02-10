//! Get a single memory by ID.

use crate::cli::output::OutputFormatter;
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

/// Retrieve and display a single memory by ID.
///
/// Supports prefix matching for the memory ID.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `id` - The memory ID or prefix
/// * `formatter` - Output formatter for displaying the memory
pub fn run_get(dir: &Path, id: &str, formatter: &OutputFormatter) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    let memory = store.get(id)?;
    formatter.print_memory(&memory);
    Ok(())
}
