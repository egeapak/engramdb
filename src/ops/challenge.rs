//! Challenge a memory's validity.

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
