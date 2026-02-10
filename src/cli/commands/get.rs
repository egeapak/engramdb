use crate::cli::output::OutputFormatter;
use crate::storage::MemoryStore;
use anyhow::Result;
use std::path::Path;

pub fn run_get(dir: &Path, id: &str, formatter: &OutputFormatter) -> Result<()> {
    let store = MemoryStore::open(dir)?;
    let memory = store.get(id)?;
    formatter.print_memory(&memory);
    Ok(())
}
