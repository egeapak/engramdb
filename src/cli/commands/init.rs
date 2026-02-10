//! Initialize a new EngramDB store.

use crate::cli::output::OutputFormatter;
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

/// Initialize a new EngramDB store in the specified directory.
///
/// Creates the `.engram/` directory structure and configuration files.
///
/// # Arguments
/// * `dir` - The directory to initialize the store in
/// * `formatter` - Output formatter for success/error messages
pub fn run_init(dir: &Path, formatter: &OutputFormatter) -> Result<()> {
    MemoryStore::init(dir)?;
    formatter.print_success(&format!("Initialized EngramDB store in {}", dir.display()));
    Ok(())
}
