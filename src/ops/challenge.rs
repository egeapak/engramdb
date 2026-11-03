//! Challenge a memory's validity.
//!
//! The implementation lives in [`crate::nli::challenge`] so the lower
//! `retrieval` layer can drive the NLI-contradiction challenge flow without a
//! circular dependency on `ops`. These re-exports keep the historical
//! `ops::challenge_*` API stable for the CLI and MCP call sites.
//!
//! [`challenge_memory`] is the one wrapper rather than a re-export: an
//! explicit challenge records a `challenged` usage event, which the models
//! crate cannot do without depending up on `ops`. The automatic NLI
//! challenges `challenge_for_contradictions` raises are not recorded: phase 3
//! turns them into proposed `contradicts` edges, which is where they belong.

use crate::storage::MemoryStore;

pub use crate::nli::challenge::{
    challenge_for_contradictions, route_contradiction, ChallengeResult, ConflictAction,
    NewMemoryMeta,
};

/// Challenge a memory (see [`crate::nli::challenge::challenge_memory`]) and
/// record a `challenged` usage event from `surface`.
pub async fn challenge_memory(
    store: &MemoryStore,
    id: &str,
    evidence: &str,
    source_file: Option<&str>,
    surface: super::usage::Surface,
) -> anyhow::Result<ChallengeResult> {
    let result = crate::nli::challenge::challenge_memory(store, id, evidence, source_file).await?;
    super::usage::record_write(
        store,
        &result.memory.id,
        super::usage::UsageKind::Challenged,
        surface,
        None,
    )
    .await;
    Ok(result)
}
