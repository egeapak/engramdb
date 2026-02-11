//! Challenge a memory's validity.

use crate::cli::output::OutputFormatter;
use crate::ops::challenge_memory;
use crate::storage::{MemoryStore, RegistryBackend};
use anyhow::Result;
use std::path::Path;

/// Parameters for the challenge command.
pub struct ChallengeParams {
    pub id: String,
    pub evidence: String,
    pub source_file: Option<String>,
}

/// Challenge a memory by providing counter-evidence.
pub async fn run_challenge(
    dir: &Path,
    registry: &dyn RegistryBackend,
    params: ChallengeParams,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = MemoryStore::open(dir, registry).await?;
    let result = challenge_memory(
        &store,
        &params.id,
        &params.evidence,
        params.source_file.as_deref(),
    )
    .await?;

    if result.challenged {
        formatter.print_success(&format!(
            "Challenged memory {} (status: {:?}, {} total challenges)",
            result.memory.id,
            result.memory.status,
            result.memory.challenges.len()
        ));
    }
    Ok(())
}
