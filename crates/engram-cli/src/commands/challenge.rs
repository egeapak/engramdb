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
    /// The memory the challenged one contradicts: also propose a
    /// `contradicts` edge between them (status `proposed`, the evidence as
    /// its note) for a reviewer to confirm, direct or reject.
    pub against: Option<String>,
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
    // Validate the edge before recording the challenge: a bad `--against`
    // must fail the whole command, not leave a challenge that a retry
    // would then record twice.
    if let Some(against) = &params.against {
        engramdb::ops::links::check_link(
            &store,
            engramdb::ops::links::challenge_edge(&params.id, against, &params.evidence),
        )
        .await
        .map_err(|e| anyhow::anyhow!("--against: {e:#}"))?;
    }
    let result = challenge_memory(
        &store,
        &params.id,
        &params.evidence,
        params.source_file.as_deref(),
        engramdb::ops::Surface::Cli,
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
    if let Some(against) = params.against {
        let edge = engramdb::ops::links::link(
            &store,
            engramdb::ops::links::challenge_edge(&result.memory.id, &against, &params.evidence),
            engramdb::ops::Surface::Cli,
        )
        .await
        .map_err(|e| {
            anyhow::anyhow!("the challenge was recorded, but the contradicts edge was not: {e:#}")
        })?;
        formatter.print_link_result(&edge);
    }
    Ok(())
}
