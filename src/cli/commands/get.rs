//! Get a single memory by ID.

use crate::cli::output::OutputFormatter;
use crate::ops::get_memory;
use crate::storage::{paths, MemoryStore, RegistryBackend};
use crate::types::Visibility;
use anyhow::Result;
use std::path::Path;
use tokio::fs;

/// Retrieve and display a single memory by ID.
///
/// Supports prefix matching for the memory ID.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `id` - The memory ID or prefix
/// * `full` - Show complete details without truncation
/// * `raw` - Output the raw markdown file contents
/// * `path_only` - Print the memory's file path instead of content
/// * `formatter` - Output formatter for displaying the memory
pub async fn run_get(
    dir: &Path,
    registry: &dyn RegistryBackend,
    id: &str,
    full: bool,
    raw: bool,
    path_only: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
    if let Ok(Some(warning)) = store.check_staleness().await {
        formatter.print_warning(&warning);
    }
    let memory = get_memory(&store, id).await?;

    // Handle --path flag: print file path and exit
    if path_only {
        let file_path = match memory.visibility {
            Visibility::Shared => paths::memories_dir(dir).join(format!("{}.md", memory.id)),
            Visibility::Personal => {
                paths::personal_memories_dir(&store.project_id)?.join(format!("{}.md", memory.id))
            }
        };
        println!("{}", file_path.display());
        return Ok(());
    }

    // Handle --raw flag: read and print raw markdown file
    if raw {
        let file_path = match memory.visibility {
            Visibility::Shared => paths::memories_dir(dir).join(format!("{}.md", memory.id)),
            Visibility::Personal => {
                paths::personal_memories_dir(&store.project_id)?.join(format!("{}.md", memory.id))
            }
        };
        let content = fs::read_to_string(&file_path).await?;
        print!("{}", content);
        return Ok(());
    }

    // Handle --full flag: show complete details without truncation
    if full {
        formatter.print_memory_full(&memory);
    } else {
        formatter.print_memory(&memory);
    }

    Ok(())
}
