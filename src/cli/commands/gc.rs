//! Garbage collection command.

use crate::cli::output::OutputFormatter;
use crate::ops::gc_memories;
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::Result;
use std::path::Path;

/// Run garbage collection.
///
/// Default mode is dry-run (shows what would be deleted).
/// Use --confirm to actually delete.
pub async fn run_gc(
    dir: &Path,
    registry: &dyn RegistryBackend,
    confirm: bool,
    threshold: Option<f64>,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
    let config_path = dir.join(".engramdb").join("config.toml");
    let config = crate::storage::config::load_config(&config_path).await?;

    let dry_run = !confirm;
    let result = gc_memories(&store, &config, dry_run, threshold).await?;

    if result.count == 0 {
        formatter.print_message("No memories eligible for garbage collection.");
    } else if dry_run {
        formatter.print_message(&format!(
            "Found {} memories eligible for removal (dry run):",
            result.count
        ));
        for id in &result.removed {
            let id_short = &id[..13.min(id.len())];
            match store.get(id).await {
                Ok(memory) => {
                    println!(
                        "  {} {:8}  {} (criticality: {:.2})",
                        id_short,
                        format!("{:?}", memory.type_),
                        memory.summary,
                        memory.criticality
                    );
                }
                Err(_) => {
                    println!("  {}", id_short);
                }
            }
        }
        formatter.print_message("\nRun with --confirm to delete these memories.");
    } else {
        formatter.print_success(&format!("Removed {} memories.", result.count));
    }

    Ok(())
}
