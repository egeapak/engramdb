//! Challenge a memory's validity.

use crate::storage::MemoryStore;
use crate::types::{Challenge, Memory};
use anyhow::Result;

/// Result of a challenge operation.
pub struct ChallengeResult {
    pub challenged: bool,
    pub memory: Memory,
}

/// Challenge a memory by adding evidence against it.
///
/// Adds a challenge to the memory, sets its status to Challenged,
/// and persists the change.
pub fn challenge_memory(
    store: &MemoryStore,
    id: &str,
    evidence: &str,
    source_file: Option<&str>,
) -> Result<ChallengeResult> {
    let mut memory = store.get(id)?;

    let mut challenge = Challenge::new(evidence);
    if let Some(sf) = source_file {
        challenge = challenge.with_source_file(sf);
    }

    memory.add_challenge(challenge);

    // MemoryUpdate doesn't support challenges field, so delete and recreate
    store.delete(&memory.id)?;
    store.create(&memory)?;

    Ok(ChallengeResult {
        challenged: true,
        memory,
    })
}
