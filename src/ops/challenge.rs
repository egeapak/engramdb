//! Challenge a memory's validity.

use crate::nli::NliResult;
use crate::storage::MemoryStore;
use crate::types::{Challenge, Memory, MemoryUpdate, Status};
use anyhow::Result;

/// Result of a challenge operation.
pub struct ChallengeResult {
    pub challenged: bool,
    pub memory: Memory,
}

/// Challenge a memory by adding evidence against it.
///
/// Adds a challenge to the memory, sets its status to Challenged,
/// and persists the change via an in-place update.
pub async fn challenge_memory(
    store: &MemoryStore,
    id: &str,
    evidence: &str,
    source_file: Option<&str>,
) -> Result<ChallengeResult> {
    let mut memory = store.get(id).await?;

    let mut challenge = Challenge::new(evidence);
    if let Some(sf) = source_file {
        challenge = challenge.with_source_file(sf);
    }

    memory.add_challenge(challenge);

    let mut update = MemoryUpdate::new();
    update.status = Some(Status::Challenged);
    update.challenges = Some(memory.challenges.clone());
    store.update(&memory.id, update).await?;

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
/// Note: if two new memories are created concurrently and both contradict
/// the same existing memory, one challenge may be lost due to a
/// read-modify-write race on the challenges vec inside `challenge_memory`.
/// This is acceptable since NLI contradiction detection is advisory, not
/// transactional.
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
    use crate::storage::{InMemoryRegistry, MemoryStore};
    use crate::types::{MemoryType, Provenance};
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
