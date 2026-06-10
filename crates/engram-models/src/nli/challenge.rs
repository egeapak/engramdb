//! Writing challenges to memories, including the NLI-contradiction flow.
//!
//! These helpers live in the `nli` layer (which depends only on `storage` and
//! `types`) rather than in `ops`, so that `retrieval` — which detects
//! contradictions during background ingestion — can drive the challenge write
//! without depending "upward" on `ops`. The `ops` module re-exports them so the
//! `ops::challenge_*` call sites (CLI/MCP) are unchanged.

use crate::nli::NliResult;
use anyhow::Result;
use engram_storage::MemoryStore;
use engram_types::{Challenge, Memory, Status};

/// Result of a challenge operation.
pub struct ChallengeResult {
    pub challenged: bool,
    pub memory: Memory,
}

/// Challenge a memory by adding evidence against it.
///
/// Adds a challenge to the memory, sets its status to Challenged,
/// and persists the change via an atomic read-modify-write
/// ([`MemoryStore::update_with`]): the memory is re-read and mutated inside
/// the per-project write lock, so a challenge written by a background NLI
/// task can never erase a concurrent user edit, and a concurrent edit can
/// never wipe a freshly written challenge.
pub async fn challenge_memory(
    store: &MemoryStore,
    id: &str,
    evidence: &str,
    source_file: Option<&str>,
) -> Result<ChallengeResult> {
    let mut challenge = Challenge::new(evidence);
    if let Some(sf) = source_file {
        challenge = challenge.with_source_file(sf);
    }

    let memory = store
        .update_with(id, move |memory| {
            memory.add_challenge(challenge);
            memory.status = Status::Challenged;
            Ok(())
        })
        .await?;

    Ok(ChallengeResult {
        challenged: true,
        memory,
    })
}

/// Write a challenge to each memory in `contradictions`, attributing the
/// challenge to the new memory whose summary is `new_memory_summary`.
///
/// Best-effort: per-iteration errors are logged via `tracing` and never
/// propagate. Intended to run inside a `tokio::spawn`ed task or as the tail
/// of one.
///
/// Note: `challenge_memory` performs an atomic read-modify-write under the
/// per-project write lock, so concurrent challenges against the same memory
/// (or a challenge racing a user edit) all survive — no challenge or edit is
/// lost to a stale-snapshot overwrite.
pub async fn challenge_for_contradictions(
    store: &MemoryStore,
    new_memory_summary: &str,
    contradictions: &[(String, NliResult)],
) {
    for (existing_id, nli_result) in contradictions {
        let evidence = format!(
            "NLI contradiction detected (score: {:.2}): new memory '{}' contradicts this memory",
            nli_result.contradiction, new_memory_summary
        );
        if let Err(e) = challenge_memory(store, existing_id, &evidence, None).await {
            tracing::warn!(
                "Failed to challenge memory {} for NLI contradiction: {}",
                existing_id,
                e
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_storage::{InMemoryRegistry, MemoryStore};
    use engram_types::{MemoryType, Provenance};
    use tempfile::TempDir;

    async fn setup_test_store() -> (TempDir, MemoryStore) {
        let temp_dir = TempDir::new().unwrap();
        let registry = InMemoryRegistry::new();
        let store = MemoryStore::init(temp_dir.path(), &registry).await.unwrap();
        (temp_dir, store)
    }

    async fn create_test_memory(store: &MemoryStore) -> String {
        let memory = Memory::new(
            MemoryType::Decision,
            "Use SQLite for the database",
            "We decided to use SQLite.",
            Provenance::human(),
        );
        store.create(&memory).await.unwrap()
    }

    #[tokio::test]
    async fn test_challenge_persists_status_and_evidence() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        let result = challenge_memory(&store, &id, "Contradicts PostgreSQL decision", None)
            .await
            .unwrap();

        assert!(result.challenged);
        assert_eq!(result.memory.status, Status::Challenged);
        assert_eq!(result.memory.challenges.len(), 1);
        assert_eq!(
            result.memory.challenges[0].evidence,
            "Contradicts PostgreSQL decision"
        );

        // Verify the change roundtrips through the store
        let reloaded = store.get(&id).await.unwrap();
        assert_eq!(reloaded.status, Status::Challenged);
        assert_eq!(reloaded.challenges.len(), 1);
        assert_eq!(
            reloaded.challenges[0].evidence,
            "Contradicts PostgreSQL decision"
        );
    }

    #[tokio::test]
    async fn test_multiple_challenges_accumulate() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        challenge_memory(&store, &id, "First contradiction", None)
            .await
            .unwrap();
        let result = challenge_memory(&store, &id, "Second contradiction", None)
            .await
            .unwrap();

        assert_eq!(result.memory.challenges.len(), 2);
        assert_eq!(result.memory.challenges[0].evidence, "First contradiction");
        assert_eq!(result.memory.challenges[1].evidence, "Second contradiction");

        // Verify both survive a roundtrip
        let reloaded = store.get(&id).await.unwrap();
        assert_eq!(reloaded.challenges.len(), 2);
    }

    /// Concurrent challenges against the same memory must all survive —
    /// `challenge_memory` uses `update_with`, so each challenge re-reads the
    /// challenges vec inside the per-project write lock instead of racing on
    /// a stale snapshot.
    #[tokio::test]
    async fn test_concurrent_challenges_all_persist() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        let tasks: Vec<_> = (0..4)
            .map(|i| {
                let store = store.clone();
                let id = id.clone();
                tokio::spawn(async move {
                    challenge_memory(&store, &id, &format!("Evidence {}", i), None)
                        .await
                        .unwrap();
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }

        let reloaded = store.get(&id).await.unwrap();
        assert_eq!(
            reloaded.challenges.len(),
            4,
            "a concurrent challenge was lost: {:?}",
            reloaded
                .challenges
                .iter()
                .map(|c| c.evidence.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(reloaded.status, Status::Challenged);
    }

    #[tokio::test]
    async fn test_challenge_with_source_file() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        let result = challenge_memory(&store, &id, "File-based evidence", Some("src/main.rs"))
            .await
            .unwrap();

        assert_eq!(
            result.memory.challenges[0].source_file.as_deref(),
            Some("src/main.rs")
        );

        let reloaded = store.get(&id).await.unwrap();
        assert_eq!(
            reloaded.challenges[0].source_file.as_deref(),
            Some("src/main.rs")
        );
    }

    #[tokio::test]
    async fn test_challenge_preserves_memory_content() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        challenge_memory(&store, &id, "Some evidence", None)
            .await
            .unwrap();

        let reloaded = store.get(&id).await.unwrap();
        assert_eq!(reloaded.summary, "Use SQLite for the database");
        assert_eq!(reloaded.content, "We decided to use SQLite.");
        assert_eq!(reloaded.type_, MemoryType::Decision);
    }

    #[tokio::test]
    async fn test_challenge_preserves_memory_id() {
        let (_temp, store) = setup_test_store().await;
        let id = create_test_memory(&store).await;

        let result = challenge_memory(&store, &id, "Some evidence", None)
            .await
            .unwrap();

        assert_eq!(result.memory.id, id);

        let reloaded = store.get(&id).await.unwrap();
        assert_eq!(reloaded.id, id);
    }
}
