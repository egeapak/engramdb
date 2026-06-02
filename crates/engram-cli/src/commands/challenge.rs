//! Challenge a memory's validity.

use crate::output::OutputFormatter;
use anyhow::Result;
use engramdb::ops::challenge_memory;
use engramdb::storage::MemoryStore;
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
    global: bool,
    params: ChallengeParams,
    formatter: &OutputFormatter,
) -> Result<()> {
    let store = if global {
        MemoryStore::open_global().await?
    } else {
        MemoryStore::open(dir).await?
    };
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
