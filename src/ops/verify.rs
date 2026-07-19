//! Verification write path (§10.4): the human/agent counterpart to doctor's
//! epistemic checks.
//!
//! `verify_memory` stamps `verified_at = now` — which the fact freshness
//! anchor (§7.3) consumes directly — and, when the memory sits in
//! `NeedsReview` *because doctor flagged it* (§10.1 invalidated-path or
//! §10.3 derived-from cascade), resets it to `Active` and removes those
//! doctor findings. A `NeedsReview` set by a human or another flow is left
//! untouched: verify confirms accuracy against the code, it does not
//! adjudicate arbitrary reviews.

use crate::storage::MemoryStore;
use crate::types::Status;
use anyhow::Result;
use chrono::Utc;

/// Machine-readable origin tag doctor `--fix` writes into the challenge
/// record when the §10.1 invalidated-path check flips a memory to
/// `NeedsReview`. `verify` matches on this prefix to know the review was
/// doctor-initiated.
pub const DOCTOR_ORIGIN_INVALIDATED_PATH: &str = "[doctor:invalidated-path]";

/// Origin tag for the §10.3 derived-from cascade check (see
/// [`DOCTOR_ORIGIN_INVALIDATED_PATH`]).
pub const DOCTOR_ORIGIN_DERIVED_FROM: &str = "[doctor:derived-from]";

/// True when a challenge-evidence string carries a doctor origin tag whose
/// finding `verify` may clear (§10.1 / §10.3).
pub fn is_doctor_review_finding(evidence: &str) -> bool {
    evidence.starts_with(DOCTOR_ORIGIN_INVALIDATED_PATH)
        || evidence.starts_with(DOCTOR_ORIGIN_DERIVED_FROM)
}

/// Result of a verify operation.
#[derive(Debug)]
pub struct VerifyResult {
    pub id: String,
    /// Whether a doctor-initiated `NeedsReview` was reset to `Active`.
    pub review_cleared: bool,
}

/// Stamp `verified_at = now`; clear a doctor-initiated `NeedsReview`.
pub async fn verify_memory(store: &MemoryStore, id: &str) -> Result<VerifyResult> {
    let mut review_cleared = false;
    let saved = store
        .update_with(id, |memory| {
            memory.verified_at = Some(Utc::now());
            if memory.status == Status::NeedsReview
                && memory
                    .challenges
                    .iter()
                    .any(|c| is_doctor_review_finding(&c.evidence))
            {
                memory
                    .challenges
                    .retain(|c| !is_doctor_review_finding(&c.evidence));
                // Only fully resolve the review when no other (non-doctor)
                // findings remain pending against it.
                if memory.challenges.is_empty() {
                    memory.status = Status::Active;
                }
                review_cleared = true;
            }
            Ok(())
        })
        .await?;

    Ok(VerifyResult {
        id: saved.id,
        review_cleared,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Challenge, Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    async fn store() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        let s = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        (tmp, s)
    }

    fn memory(id: &str) -> Memory {
        let mut m = Memory::new(MemoryType::Hazard, "S", "C", Provenance::human());
        m.id = id.to_string();
        m
    }

    #[tokio::test]
    async fn verify_stamps_verified_at() {
        let (_t, store) = store().await;
        store.create(&memory("v-1")).await.unwrap();

        let before = Utc::now();
        let result = verify_memory(&store, "v-1").await.unwrap();
        assert!(!result.review_cleared);

        let m = store.get("v-1").await.unwrap();
        assert!(m.verified_at.is_some());
        assert!(m.verified_at.unwrap() >= before);
        assert_eq!(m.status, Status::Active);
    }

    #[tokio::test]
    async fn verify_clears_doctor_initiated_review() {
        let (_t, store) = store().await;
        let mut m = memory("v-2");
        m.add_challenge(Challenge::new(format!(
            "{DOCTOR_ORIGIN_INVALIDATED_PATH} invalidation paths changed since last verification"
        )));
        // add_challenge flips status to Challenged; doctor's --fix writes
        // NeedsReview explicitly, so mirror that here (set AFTER the add).
        m.status = Status::NeedsReview;
        store.create(&m).await.unwrap();

        let result = verify_memory(&store, "v-2").await.unwrap();
        assert!(result.review_cleared);

        let m = store.get("v-2").await.unwrap();
        assert_eq!(m.status, Status::Active);
        assert!(m.challenges.is_empty());
        assert!(m.verified_at.is_some());
    }

    #[tokio::test]
    async fn verify_leaves_human_review_pending() {
        let (_t, store) = store().await;
        let mut m = memory("v-3");
        m.add_challenge(Challenge::new("manually flagged by reviewer"));
        m.status = Status::NeedsReview;
        store.create(&m).await.unwrap();

        let result = verify_memory(&store, "v-3").await.unwrap();
        assert!(!result.review_cleared);

        let m = store.get("v-3").await.unwrap();
        assert_eq!(m.status, Status::NeedsReview, "non-doctor review persists");
        assert_eq!(m.challenges.len(), 1);
        assert!(m.verified_at.is_some(), "verified_at stamps regardless");
    }

    #[tokio::test]
    async fn verify_mixed_findings_clears_doctor_keeps_rest() {
        let (_t, store) = store().await;
        let mut m = memory("v-4");
        m.add_challenge(Challenge::new(format!(
            "{DOCTOR_ORIGIN_DERIVED_FROM} a source this memory was derived from is invalid"
        )));
        m.add_challenge(Challenge::new("independent human dispute"));
        m.status = Status::NeedsReview;
        store.create(&m).await.unwrap();

        let result = verify_memory(&store, "v-4").await.unwrap();
        assert!(result.review_cleared);

        let m = store.get("v-4").await.unwrap();
        assert_eq!(
            m.status,
            Status::NeedsReview,
            "status stays while non-doctor findings remain"
        );
        assert_eq!(m.challenges.len(), 1);
        assert_eq!(m.challenges[0].evidence, "independent human dispute");
    }
}
