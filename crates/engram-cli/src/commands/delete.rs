//! Delete a memory from the store.

use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::ops::{delete_memory, get_memory};
use engramdb::storage::MemoryStore;
use std::io::{self, Write};
use std::path::Path;

/// Delete a memory by ID.
///
/// Prompts for confirmation unless `force` is true. Supports prefix matching for the ID.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `id` - The memory ID or prefix
/// * `force` - Skip confirmation prompt if true
/// * `formatter` - Output formatter for success/error messages
pub async fn run_delete(
    dir: &Path,
    global: bool,
    id: &str,
    force: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };

    // Get the memory to confirm what we're deleting
    let memory = get_memory(&store, id).await?;

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

    delete_memory(&store, &memory.id).await?;

    formatter.print_success(&format!("Deleted memory {}", memory.id));
    Ok(())
}
