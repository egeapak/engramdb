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
        // JSON is machine-consumed: never prompt (mirrors the doctor --fix
        // interactivity rule) — require an explicit --force instead.
        if formatter.is_json() {
            anyhow::bail!("deletion requires confirmation; re-run with --force in JSON mode");
        }

        // Prompt on stderr so a piped/redirected stdout stays clean.
        eprint!("Delete memory {} ({})? [y/N] ", memory.id, memory.summary);
        io::stderr().flush()?;

        let mut input = String::new();
        let bytes_read = io::stdin().read_line(&mut input)?;

        // EOF (closed stdin): no answer was given, so nothing is deleted —
        // but exit non-zero rather than reporting silent success.
        if bytes_read == 0 {
            anyhow::bail!("could not obtain confirmation (stdin closed); memory not deleted");
        }

        if !input.trim().eq_ignore_ascii_case("y") {
            formatter.print_message("Deletion cancelled");
            return Ok(());
        }
    }

    delete_memory(&store, &memory.id).await?;

    formatter.print_success(&format!("Deleted memory {}", memory.id));
    Ok(())
}
