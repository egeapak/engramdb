use anyhow::Result;
use std::path::Path;
use std::io::{self, Write};
use crate::storage::MemoryStore;
use crate::cli::output::OutputFormatter;

pub fn run_delete(
    dir: &Path,
    id: &str,
    force: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir)?;

    // Get the memory to confirm what we're deleting
    let memory = store.get(id)?;

    // Confirm deletion unless --force
    if !force {
        print!("Delete memory {} ({})? [y/N] ", memory.id, memory.summary);
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            formatter.print_message("Deletion cancelled");
            return Ok(());
        }
    }

    // Delete the memory
    store.delete(id)?;

    formatter.print_success(&format!("Deleted memory {}", id));
    Ok(())
}
