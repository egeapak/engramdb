use anyhow::Result;
use std::path::Path;
use crate::storage::MemoryStore;
use crate::cli::output::OutputFormatter;

pub fn run_init(dir: &Path, formatter: &OutputFormatter) -> Result<()> {
    MemoryStore::init(dir)?;
    formatter.print_success(&format!("Initialized EngramDB store in {}", dir.display()));
    Ok(())
}
