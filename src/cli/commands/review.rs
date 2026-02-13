//! Interactive review of memories needing attention.

use crate::cli::output::OutputFormatter;
use crate::cli::prompter::Prompter;
use crate::ops::{self, parse_memory_type, review_memories, ReviewParams};
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::Result;
use std::path::Path;

/// Run interactive review of challenged/needs-review memories.
///
/// Presents each memory that needs review with options to keep, update, delete, or skip.
///
/// # Arguments
/// * `dir` - The directory containing the EngramDB store
/// * `registry` - The registry backend to use for project registration
/// * `scope` - Optional logical scope filter
/// * `type_str` - Optional memory type filter
/// * `challenged_only` - Only show Status::Challenged memories
/// * `stale_only` - Only show Status::NeedsReview memories
/// * `formatter` - Output formatter for success/error messages
#[allow(clippy::too_many_arguments)]
pub async fn run_review(
    dir: &Path,
    registry: &dyn RegistryBackend,
    scope: Option<String>,
    type_str: Option<String>,
    challenged_only: bool,
    stale_only: bool,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;

    let type_filter = type_str.as_deref().map(parse_memory_type).transpose()?;

    let params = ReviewParams {
        scope,
        max_results: None,
        type_filter,
        challenged_only,
        stale_only,
    };

    let memories = review_memories(&store, &params).await?;

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
        let answer = prompter.select("Action:", &options);

        match answer.as_deref() {
            Ok("Keep (reset to Active)") => {
                ops::resolve_memory(
                    &store,
                    ops::ResolveParams {
                        id: memory.id.clone(),
                        action: ops::ResolveAction::Keep,
                        updated_content: None,
                        updated_summary: None,
                    },
                )
                .await?;
                formatter.print_success(&format!(
                    "Kept memory {} as Active.",
                    &memory.id[..8.min(memory.id.len())]
                ));
            }
            Ok("Update") => {
                let new_summary = prompter.text("New summary (enter to keep):", None)?;
                let new_content = prompter.text("New content (enter to keep):", None)?;

                ops::resolve_memory(
                    &store,
                    ops::ResolveParams {
                        id: memory.id.clone(),
                        action: ops::ResolveAction::Update,
                        updated_content: if new_content.is_empty() {
                            None
                        } else {
                            Some(new_content)
                        },
                        updated_summary: if new_summary.is_empty() {
                            None
                        } else {
                            Some(new_summary)
                        },
                    },
                )
                .await?;
                formatter.print_success(&format!(
                    "Updated memory {}.",
                    &memory.id[..8.min(memory.id.len())]
                ));
            }
            Ok("Delete") => {
                ops::resolve_memory(
                    &store,
                    ops::ResolveParams {
                        id: memory.id.clone(),
                        action: ops::ResolveAction::Delete,
                        updated_content: None,
                        updated_summary: None,
                    },
                )
                .await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::prompter::MockPrompter;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Challenge, Memory, MemoryType, Provenance, Status};
    use tempfile::TempDir;

    /// Helper to create a store with a challenged memory ready for review.
    async fn setup_review_store(
        dir: &std::path::Path,
        registry: &dyn RegistryBackend,
    ) -> (MemoryStore, String) {
        let store = MemoryStore::init(dir, registry).await.unwrap();
        let mut memory = Memory::new(
            MemoryType::Decision,
            "Test memory",
            "Test content",
            Provenance::human(),
        );
        memory.status = Status::Challenged;
        memory.add_challenge(Challenge::new("Outdated"));
        let id = memory.id.clone();
        store.create(&memory).await.unwrap();
        (store, id)
    }

    #[tokio::test]
    async fn test_review_keep_action() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (store, id) = setup_review_store(temp_dir.path(), &registry).await;
        let formatter = OutputFormatter::new(None, false, true);

        let prompter = MockPrompter::new(vec!["Keep (reset to Active)"]);

        run_review(
            temp_dir.path(),
            &registry,
            None,
            None,
            false,
            false,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.status, Status::Active);
        assert!(memory.challenges.is_empty());
    }

    #[tokio::test]
    async fn test_review_update_action() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (store, id) = setup_review_store(temp_dir.path(), &registry).await;
        let formatter = OutputFormatter::new(None, false, true);

        // select "Update", then text for new summary, text for new content
        let prompter = MockPrompter::new(vec!["Update", "New summary", "New content"]);

        run_review(
            temp_dir.path(),
            &registry,
            None,
            None,
            false,
            false,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        let memory = store.get(&id).await.unwrap();
        assert_eq!(memory.status, Status::Active);
        assert_eq!(memory.summary, "New summary");
        assert_eq!(memory.content, "New content");
    }

    #[tokio::test]
    async fn test_review_delete_action() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let (store, id) = setup_review_store(temp_dir.path(), &registry).await;
        let formatter = OutputFormatter::new(None, false, true);

        let prompter = MockPrompter::new(vec!["Delete"]);

        run_review(
            temp_dir.path(),
            &registry,
            None,
            None,
            false,
            false,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        assert!(store.get(&id).await.is_err());
    }

    #[tokio::test]
    async fn test_review_skip_and_quit() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();

        // Create two challenged memories
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let mut m1 = Memory::new(
            MemoryType::Decision,
            "Memory 1",
            "Content 1",
            Provenance::human(),
        );
        m1.status = Status::Challenged;
        m1.add_challenge(Challenge::new("Challenge 1"));
        let id1 = m1.id.clone();
        store.create(&m1).await.unwrap();

        let mut m2 = Memory::new(
            MemoryType::Convention,
            "Memory 2",
            "Content 2",
            Provenance::human(),
        );
        m2.status = Status::Challenged;
        m2.add_challenge(Challenge::new("Challenge 2"));
        let id2 = m2.id.clone();
        store.create(&m2).await.unwrap();

        let formatter = OutputFormatter::new(None, false, true);

        // Skip first, Quit on second
        let prompter = MockPrompter::new(vec!["Skip", "Quit"]);

        run_review(
            temp_dir.path(),
            &registry,
            None,
            None,
            false,
            false,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        // Both memories should still exist and still be challenged
        let mem1 = store.get(&id1).await.unwrap();
        assert_eq!(mem1.status, Status::Challenged);
        let mem2 = store.get(&id2).await.unwrap();
        assert_eq!(mem2.status, Status::Challenged);
    }

    #[tokio::test]
    async fn test_review_empty_list() {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        let formatter = OutputFormatter::new(None, false, true);

        // No prompts needed since the list is empty
        let prompter = MockPrompter::new(vec![]);

        let result = run_review(
            temp_dir.path(),
            &registry,
            None,
            None,
            false,
            false,
            &formatter,
            &prompter,
        )
        .await;

        assert!(result.is_ok());
    }
}
