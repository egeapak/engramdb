use anyhow::Result;
use std::path::Path;
use crate::storage::MemoryStore;
use crate::cli::output::OutputFormatter;

pub fn run_get(dir: &Path, id: &str, formatter: &OutputFormatter) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    let memory = store.get(id)?;
    formatter.print_memory(&memory);
    Ok(())
}
