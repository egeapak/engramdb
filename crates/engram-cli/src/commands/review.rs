//! Interactive review of memories needing attention.

use crate::output::{outln, OutputFormatter};
use crate::prompter::Prompter;
use anyhow::Result;
use engramdb::ops::{self, parse_memory_type, review_memories, ReviewParams};
use engramdb::storage::MemoryStore;
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
/// * `stale_after_days` - Recency trigger: also surface active memories not
///   updated in more than N days. `None` reviews only flagged memories.
/// * `formatter` - Output formatter for success/error messages
#[allow(clippy::too_many_arguments)]
pub async fn run_review(
    dir: &Path,
    global: bool,
    scope: Option<String>,
    type_str: Option<String>,
    challenged_only: bool,
    stale_only: bool,
    stale_after_days: Option<u64>,
    formatter: &OutputFormatter,
    prompter: &dyn Prompter,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };

    let type_filter = type_str.as_deref().map(parse_memory_type).transpose()?;

    let params = ReviewParams {
        scope,
        max_results: None,
        type_filter,
        challenged_only,
        stale_only,
        stale_after_days,
    };

    let memories = review_memories(&store, &params).await?;

    if memories.is_empty() {
        formatter.print_message("No memories need review.");
        return Ok(());
    }

    formatter.print_message(&format!("{} memories need review:\n", memories.len()));

    for memory in &memories {
        outln!(
            formatter,
            "ID: {}",
            memory.id.chars().take(8).collect::<String>()
        );
        outln!(formatter, "Type: {:?}", memory.type_);
        outln!(formatter, "Summary: {}", memory.summary);
        outln!(formatter, "Status: {:?}", memory.status);
        outln!(formatter, "Criticality: {:.2}", memory.criticality);

        if !memory.challenges.is_empty() {
            outln!(formatter, "Challenges:");
            for challenge in &memory.challenges {
                outln!(
                    formatter,
                    "  - {} ({})",
                    challenge.evidence,
                    challenge.timestamp.format("%Y-%m-%d")
                );
                if let Some(ref sf) = challenge.source_file {
                    outln!(formatter, "    Source: {}", sf);
                }
            }
        }
        outln!(formatter);

        let options = vec![
            "Keep (reset to Active)",
            "Update",
            "Invalidate (close validity window, keep history)",
            "Delete",
            "Skip",
            "Quit",
        ];
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
                        superseded_by: None,
                    },
                )
                .await?;
                formatter.print_success(&format!(
                    "Kept memory {} as Active.",
                    memory.id.chars().take(8).collect::<String>()
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
                        superseded_by: None,
                    },
                )
                .await?;
                formatter.print_success(&format!(
                    "Updated memory {}.",
                    memory.id.chars().take(8).collect::<String>()
                ));
            }
            Ok("Invalidate (close validity window, keep history)") => {
                let successor =
                    prompter.text("Superseded by (memory id, enter for none):", None)?;
                ops::resolve_memory(
                    &store,
                    ops::ResolveParams {
                        id: memory.id.clone(),
                        action: ops::ResolveAction::Invalidate,
                        updated_content: None,
                        updated_summary: None,
                        superseded_by: if successor.is_empty() {
                            None
                        } else {
                            Some(successor)
                        },
                    },
                )
                .await?;
                formatter.print_success(&format!(
                    "Invalidated memory {} (retained on disk; reopen with `update --clear-invalidated`).",
                    memory.id.chars().take(8).collect::<String>()
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
                        superseded_by: None,
                    },
                )
                .await?;
                formatter.print_success(&format!(
                    "Deleted memory {}.",
                    memory.id.chars().take(8).collect::<String>()
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
        outln!(formatter);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompter::MockPrompter;
    use crate::testutil::{capturing_plain, interaction, snap_command, TempProject};
    use engramdb::storage::{InMemoryRegistry, RegistryBackend};
    use engramdb::types::{Challenge, Memory, MemoryType, Provenance, Status};
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
            false,
            None,
            None,
            false,
            false,
            None,
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
            false,
            None,
            None,
            false,
            false,
            None,
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
            false,
            None,
            None,
            false,
            false,
            None,
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
            false,
            None,
            None,
            false,
            false,
            None,
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
            false,
            None,
            None,
            false,
            false,
            None,
            &formatter,
            &prompter,
        )
        .await;

        assert!(result.is_ok());
    }

    // =================================================================
    // Command-tier snapshots
    //
    // The tests above assert store *state* — that Keep clears the
    // challenges, that Delete removes the file. These assert what the user
    // was actually shown and asked: the per-memory card, the action list and
    // its wording, the follow-up prompts each action opens, and the outcome
    // line. `review`'s whole loop reaches the terminal through `inquire`
    // (prompts) and `outln!` (card), so neither other tier can observe it.
    // See `crate::testutil`.
    // =================================================================

    /// A fixed challenge clock.
    ///
    /// `run_review` renders `challenge.timestamp.format("%Y-%m-%d")` in the
    /// card, so a `Challenge::new` timestamp (`Utc::now()`) would bake today's
    /// date into the snapshot and break the build tomorrow.
    fn pinned_at() -> chrono::DateTime<chrono::Utc> {
        "2025-03-14T09:12:00Z".parse().expect("fixed timestamp")
    }

    /// Seed one challenged memory with a pinned challenge timestamp.
    ///
    /// Distinct criticalities are what make a multi-memory snapshot stable:
    /// `review_memories` sorts by criticality descending, and equal scores
    /// leave the order to the index.
    async fn seed_challenged(
        store: &MemoryStore,
        type_: MemoryType,
        summary: &str,
        content: &str,
        criticality: f64,
        evidence: &str,
        source_file: Option<&str>,
    ) -> String {
        let mut memory = Memory::new(type_, summary, content, Provenance::human());
        memory.status = Status::Challenged;
        memory.criticality = criticality;
        let mut challenge = Challenge::new(evidence);
        challenge.timestamp = pinned_at();
        if let Some(sf) = source_file {
            challenge = challenge.with_source_file(sf);
        }
        memory.add_challenge(challenge);
        let id = memory.id.clone();
        store.create(&memory).await.unwrap();
        id
    }

    /// Keep: the card, the action list, and the "reset to Active" outcome.
    #[tokio::test]
    async fn snap_review_keep() {
        let p = TempProject::new();
        let store = p.init_store().await;
        seed_challenged(
            &store,
            MemoryType::Decision,
            "Embedding vectors are stored in the same LanceDB table as metadata",
            "There is no separate metadata DB; one table holds both.",
            0.80,
            "A reviewer thought a second table had been added for chunks",
            Some("crates/engram-storage/src/lance_index.rs"),
        )
        .await;

        let prompter = MockPrompter::new(vec!["Keep (reset to Active)"]);
        let (formatter, cap) = capturing_plain();

        run_review(
            p.path(),
            false,
            None,
            None,
            false,
            false,
            None,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        snap_command("review_keep", p.path(), interaction(&prompter, &cap));
    }

    /// Update opens two follow-up text prompts. Their wording — and that both
    /// are asked before anything is written — is the point.
    #[tokio::test]
    async fn snap_review_update() {
        let p = TempProject::new();
        let store = p.init_store().await;
        seed_challenged(
            &store,
            MemoryType::Convention,
            "CLI output goes through println! in command handlers",
            "Handlers print directly with println!.",
            0.60,
            "The formatter-output CI job now rejects bare print macros",
            None,
        )
        .await;

        let prompter = MockPrompter::new(vec![
            "Update",
            "All CLI output goes through the outln!/errln! formatter macros",
            "Bare print macros fail the formatter-output CI job; write through \
             OutputFormatter so the renderer snapshots can see the bytes.",
        ]);
        let (formatter, cap) = capturing_plain();

        run_review(
            p.path(),
            false,
            None,
            None,
            false,
            false,
            None,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        snap_command("review_update", p.path(), interaction(&prompter, &cap));
    }

    /// Delete: no confirmation of its own, so the action list is the only
    /// thing between the user and a removed memory.
    #[tokio::test]
    async fn snap_review_delete() {
        let p = TempProject::new();
        let store = p.init_store().await;
        seed_challenged(
            &store,
            MemoryType::Debug,
            "Reranking is slow because the model reloads on every query",
            "Each query built a fresh cross-encoder session.",
            0.35,
            "ProviderCache now loads the reranker once per process",
            Some("src/ops/mod.rs"),
        )
        .await;

        let prompter = MockPrompter::new(vec!["Delete"]);
        let (formatter, cap) = capturing_plain();

        run_review(
            p.path(),
            false,
            None,
            None,
            false,
            false,
            None,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        snap_command("review_delete", p.path(), interaction(&prompter, &cap));
    }

    /// Invalidate — the supersede path. It is the one action with a second
    /// prompt whose answer is another memory's id, and the only outcome line
    /// that tells the user the memory is retained on disk.
    #[tokio::test]
    async fn snap_review_supersede() {
        let p = TempProject::new();
        let store = p.init_store().await;
        seed_challenged(
            &store,
            MemoryType::Decision,
            "The ONNX Runtime is vendored into the release archives",
            "Release artifacts ship a bundled libonnxruntime.",
            0.70,
            "Releases now depend on the package manager's runtime instead",
            Some("docs/contributors/embedding-model-alternatives.md"),
        )
        .await;

        // A real successor, so the prompt is answered with an id the store
        // actually knows. Active, so it is not itself up for review.
        let successor = Memory::new(
            MemoryType::Decision,
            "Release archives hold the binary only; the runtime is a dependency",
            "Homebrew and Scoop declare libonnxruntime as a package dependency.",
            Provenance::human(),
        );
        let successor_id = successor.id.clone();
        store.create(&successor).await.unwrap();

        let prompter = MockPrompter::new(vec![
            "Invalidate (close validity window, keep history)",
            &successor_id,
        ]);
        let (formatter, cap) = capturing_plain();

        run_review(
            p.path(),
            false,
            None,
            None,
            false,
            false,
            None,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        snap_command("review_supersede", p.path(), interaction(&prompter, &cap));
    }

    /// The loop's control flow: `Skip` falls through to the next memory (and
    /// suppresses the trailing blank line), `Quit` breaks out before the
    /// second card is acted on. Two memories with different criticalities, so
    /// the order the cards appear in is the sort order, not the index's.
    #[tokio::test]
    async fn snap_review_skip_then_quit() {
        let p = TempProject::new();
        let store = p.init_store().await;
        seed_challenged(
            &store,
            MemoryType::Hazard,
            "Blocking calls inside the daemon request handler stall every session",
            "The socket server is single-threaded per connection.",
            0.90,
            "A blocking fs read was added to the Status handler",
            Some("src/daemon/server.rs"),
        )
        .await;
        seed_challenged(
            &store,
            MemoryType::Convention,
            "Snapshot ids must be redacted without word boundaries",
            "Ids sit inside filenames, and _ is a word character.",
            0.40,
            "A new filter used \\b and silently skipped embedded ids",
            None,
        )
        .await;

        let prompter = MockPrompter::new(vec!["Skip", "Quit"]);
        let (formatter, cap) = capturing_plain();

        run_review(
            p.path(),
            false,
            None,
            None,
            false,
            false,
            None,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        snap_command(
            "review_skip_then_quit",
            p.path(),
            interaction(&prompter, &cap),
        );
    }

    /// The early return. An empty prompts section is the assertion: nothing
    /// to review must never reach the action list.
    #[tokio::test]
    async fn snap_review_empty() {
        let p = TempProject::new();
        p.init_store().await;

        let prompter = MockPrompter::new(vec![]);
        let (formatter, cap) = capturing_plain();

        run_review(
            p.path(),
            false,
            None,
            None,
            false,
            false,
            None,
            &formatter,
            &prompter,
        )
        .await
        .unwrap();

        snap_command("review_empty", p.path(), interaction(&prompter, &cap));
    }
}
