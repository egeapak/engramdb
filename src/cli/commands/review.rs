//! Interactive review of memories needing attention.

use crate::cli::output::OutputFormatter;
use crate::ops::{delete_memory, review_memories, update_memory, UpdateParams};
use crate::storage::MemoryStore;
use crate::types::Status;
use anyhow::Result;
use inquire::Select;
use std::path::Path;

/// Run interactive review of challenged/needs-review memories.
///
/// Presents each memory that needs review with options to keep, update, delete, or skip.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `scope` - Optional logical scope filter
/// * `type_str` - Optional memory type filter
/// * `challenged_only` - Only show Status::Challenged memories
/// * `stale_only` - Only show Status::NeedsReview memories
/// * `formatter` - Output formatter for success/error messages
pub fn run_review(
    dir: &Path,
    scope: Option<String>,
    type_str: Option<String>,
    challenged_only: bool,
    stale_only: bool,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir)?;

    let mut memories = review_memories(&store, scope.as_deref(), None)?;

    // Apply type filter if provided
    if let Some(ref t) = type_str {
        let type_filter = crate::ops::parse_memory_type(t)?;
        memories.retain(|m| m.type_ == type_filter);
    }

    // Apply status filters
    if challenged_only {
        memories.retain(|m| matches!(m.status, Status::Challenged));
    } else if stale_only {
        memories.retain(|m| matches!(m.status, Status::NeedsReview));
    }

    if memories.is_empty() {
        formatter.print_message("No memories need review.");
        return Ok(());
    }

    formatter.print_message(&format!("{} memories need review:\n", memories.len()));

    for memory in &memories {
        println!("ID: {}", &memory.id[..8.min(memory.id.len())]);
        println!("Type: {:?}", memory.type_);
        println!("Summary: {}", memory.summary);
        println!("Status: {:?}", memory.status);
        println!("Criticality: {:.2}", memory.criticality);

        if !memory.challenges.is_empty() {
            println!("Challenges:");
            for challenge in &memory.challenges {
                println!(
                    "  - {} ({})",
                    challenge.evidence,
                    challenge.timestamp.format("%Y-%m-%d")
                );
                if let Some(ref sf) = challenge.source_file {
                    println!("    Source: {}", sf);
                }
            }
        }
        println!();

        let options = vec!["Keep (reset to Active)", "Update", "Delete", "Skip", "Quit"];
        let answer = Select::new("Action:", options).prompt();

        match answer {
            Ok("Keep (reset to Active)") => {
                update_memory(
                    &store,
                    &memory.id,
                    UpdateParams {
                        status: Some(Status::Active),
                        type_: None,
                        content: None,
                        summary: None,
                        physical: None,
                        logical: None,
                        tags: None,
                        tags_add: None,
                        tags_remove: None,
                        criticality: None,
                        confidence: None,
                        details: None,
                        visibility: None,
                        supersedes: None,
                        decay_strategy: None,
                        decay_half_life: None,
                        decay_ttl: None,
                        decay_floor: None,
                    },
                )?;
                formatter.print_success(&format!(
                    "Kept memory {} as Active.",
                    &memory.id[..8.min(memory.id.len())]
                ));
            }
            Ok("Update") => {
                // For now, just update the content via a simple prompt
                let new_summary = inquire::Text::new("New summary (enter to keep):").prompt()?;
                let new_content = inquire::Text::new("New content (enter to keep):").prompt()?;

                update_memory(
                    &store,
                    &memory.id,
                    UpdateParams {
                        status: Some(Status::Active),
                        summary: if new_summary.is_empty() {
                            None
                        } else {
                            Some(new_summary)
                        },
                        content: if new_content.is_empty() {
                            None
                        } else {
                            Some(new_content)
                        },
                        type_: None,
                        physical: None,
                        logical: None,
                        tags: None,
                        tags_add: None,
                        tags_remove: None,
                        criticality: None,
                        confidence: None,
                        details: None,
                        visibility: None,
                        supersedes: None,
                        decay_strategy: None,
                        decay_half_life: None,
                        decay_ttl: None,
                        decay_floor: None,
                    },
                )?;
                formatter.print_success(&format!(
                    "Updated memory {}.",
                    &memory.id[..8.min(memory.id.len())]
                ));
            }
            Ok("Delete") => {
                delete_memory(&store, &memory.id)?;
                formatter.print_success(&format!(
                    "Deleted memory {}.",
                    &memory.id[..8.min(memory.id.len())]
                ));
            }
            Ok("Skip") => {
                continue;
            }
            Ok("Quit") | Err(_) => {
                break;
            }
            _ => {}
        }
        println!();
    }

    Ok(())
}
