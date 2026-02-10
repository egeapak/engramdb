use crate::cli::output::OutputFormatter;
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

pub fn run_init(dir: &Path, formatter: &OutputFormatter) -> Result<()> {
    MemoryStore::init(dir)?;
    formatter.print_success(&format!("Initialized EngramDB store in {}", dir.display()));
    Ok(())
}
