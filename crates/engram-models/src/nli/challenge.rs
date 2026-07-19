//! Writing challenges to memories, including the NLI-contradiction flow.
//!
//! These helpers live in the `nli` layer (which depends only on `storage` and
//! `types`) rather than in `ops`, so that `retrieval` — which detects
//! contradictions during background ingestion — can drive the challenge write
//! without depending "upward" on `ops`. The `ops` module re-exports them so the
//! `ops::challenge_*` call sites (CLI/MCP) are unchanged.

use crate::nli::NliResult;
use anyhow::Result;
use chrono::{DateTime, Utc};
use engram_storage::MemoryStore;
use engram_types::{Challenge, Epistemic, Memory, ProvenanceSource, Status};

/// Result of a challenge operation.
pub struct ChallengeResult {
    pub challenged: bool,
    pub memory: Memory,
}

/// The projection of a just-created memory that conflict routing needs
/// (§9.1–9.2). A smaller-than-`Memory` struct so the routing tests can build
/// it without a store; `From<&Memory>` covers the engine ingestion tail.
#[derive(Debug, Clone)]
pub struct NewMemoryMeta {
    pub id: String,
    pub summary: String,
    pub epistemic: Epistemic,
    pub provenance_source: ProvenanceSource,
    pub confidence: f64,
    /// Entrenchment timestamp: `verified_at.unwrap_or(created_at)`.
    pub anchor: DateTime<Utc>,
}

impl From<&Memory> for NewMemoryMeta {
    fn from(m: &Memory) -> Self {
        Self {
            id: m.id.clone(),
            summary: m.summary.clone(),
            epistemic: m.epistemic,
            provenance_source: m.provenance.source,
            confidence: m.confidence,
            anchor: m.verified_at.unwrap_or(m.created_at),
        }
    }
}

/// Trust-class rank for the entrenchment order (§9.2):
/// human > agent > imported > inferred.
fn trust_rank(source: ProvenanceSource) -> u8 {
    match source {
        ProvenanceSource::Human => 3,
        ProvenanceSource::Agent => 2,
        ProvenanceSource::Imported => 1,
        ProvenanceSource::Inferred => 0,
    }
}

/// Which side of a same-class contradiction yields (gets challenged),
/// per the entrenchment order (§9.2): lower trust class first, then older
/// `verified_at.unwrap_or(created_at)`, then lower confidence. Ties
/// challenge the existing memory (status quo: new information wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Yielder {
    Existing,
    New,
}

fn entrenchment_yielder(new: &NewMemoryMeta, existing: &Memory) -> Yielder {
    let existing_anchor = existing.verified_at.unwrap_or(existing.created_at);
    let new_rank = trust_rank(new.provenance_source);
    let existing_rank = trust_rank(existing.provenance.source);
    if new_rank != existing_rank {
        return if existing_rank < new_rank {
            Yielder::Existing
        } else {
            Yielder::New
        };
    }
    if new.anchor != existing_anchor {
        return if existing_anchor < new.anchor {
            Yielder::Existing
        } else {
            Yielder::New
        };
    }
    if new.confidence != existing.confidence {
        return if existing.confidence < new.confidence {
            Yielder::Existing
        } else {
            Yielder::New
        };
    }
    Yielder::Existing
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

/// What the routing table (§9.1) decided for one contradiction pair.
/// Public for the model-free routing tests; the store write happens in
/// [`challenge_for_contradictions`].
#[derive(Debug, Clone, PartialEq)]
pub enum ConflictAction {
    /// Standard challenge against the existing memory with this evidence.
    ChallengeExisting { evidence: String },
    /// Entrenchment picked the NEW memory as the yielding side (§9.2).
    ChallengeNew { evidence: String },
    /// Decision-vs-decision: the existing decision is a supersession
    /// candidate — flip to `NeedsReview` (zero score penalty; "probably
    /// replaced, human confirms") with a challenge record naming the new
    /// memory. The flow never auto-writes `supersedes` or closes windows —
    /// window closure is an explicit agent/human act (§2.4 writers 1–3).
    SupersessionCandidate { evidence: String },
}

/// Route one contradiction through the (new, existing) class table (§9.1).
/// Pure data-in/data-out so the full matrix is testable without NLI models.
pub fn route_contradiction(
    new: &NewMemoryMeta,
    existing: &Memory,
    nli: &NliResult,
) -> ConflictAction {
    let score = nli.contradiction;
    let premise_evidence = || {
        format!(
            "premise may have changed: contradicted by {} (NLI {score:.2})",
            new.id
        )
    };
    let stale_fact_evidence = || {
        format!(
            "stale-fact: contradicted by {} dated {} (NLI {score:.2}): '{}'",
            new.id,
            new.anchor.format("%Y-%m-%d"),
            new.summary
        )
    };
    let standard_evidence = |what: &str| {
        format!(
            "NLI contradiction (score: {score:.2}): new memory '{}' contradicts this {what}",
            new.summary
        )
    };

    match (new.epistemic, existing.epistemic) {
        // Decision vs decision: supersession candidate.
        (Epistemic::Decision, Epistemic::Decision) => ConflictAction::SupersessionCandidate {
            evidence: format!(
                "possibly superseded by {} (NLI {score:.2}) — resolve: supersede \
                 (update --supersedes, which closes this window) or reject",
                new.id
            ),
        },
        // Anything vs decision: mild premise challenge.
        (_, Epistemic::Decision) => ConflictAction::ChallengeExisting {
            evidence: premise_evidence(),
        },
        // Same-class fact/observation pairs: entrenchment picks the yielder.
        (Epistemic::Fact, Epistemic::Fact) | (Epistemic::Observation, Epistemic::Observation) => {
            match entrenchment_yielder(new, existing) {
                Yielder::Existing => ConflictAction::ChallengeExisting {
                    evidence: if new.epistemic == Epistemic::Fact {
                        stale_fact_evidence()
                    } else {
                        standard_evidence("observation")
                    },
                },
                Yielder::New => ConflictAction::ChallengeNew {
                    evidence: format!(
                        "NLI contradiction (score: {score:.2}): contradicts more entrenched \
                         memory '{}' ({})",
                        existing.summary, existing.id
                    ),
                },
            }
        }
        // Observation (or decision) vs fact: stale-fact challenge on the
        // fact, with the contradicting memory's date in evidence.
        (_, Epistemic::Fact) => ConflictAction::ChallengeExisting {
            evidence: stale_fact_evidence(),
        },
        // Fact/decision vs observation: challenge the observation.
        (_, Epistemic::Observation) => ConflictAction::ChallengeExisting {
            evidence: standard_evidence("observation"),
        },
    }
}

/// Apply the routing table (§9.1) to each contradiction, writing challenges
/// / review flags attributed to `new_memory`.
///
/// Best-effort: per-iteration errors are logged via `tracing` and never
/// propagate. Intended to run inside a `tokio::spawn`ed task or as the tail
/// of one.
///
/// Note: every write goes through an atomic read-modify-write under the
/// per-project write lock, so a challenge written by a background NLI task
/// can never erase a concurrent user edit, and vice versa.
pub async fn challenge_for_contradictions(
    store: &MemoryStore,
    new_memory: &NewMemoryMeta,
    contradictions: &[(String, NliResult)],
) {
    for (existing_id, nli_result) in contradictions {
        // Load the existing side for its class/provenance/anchor. Invalidated
        // candidates were already excluded by the caller's candidate set.
        let existing = match store.get(existing_id).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Skipping NLI challenge for {existing_id}: load failed: {e}");
                continue;
            }
        };

        let write_result = match route_contradiction(new_memory, &existing, nli_result) {
            ConflictAction::ChallengeExisting { evidence } => {
                challenge_memory(store, existing_id, &evidence, None)
                    .await
                    .map(|_| ())
            }
            ConflictAction::ChallengeNew { evidence } => {
                challenge_memory(store, &new_memory.id, &evidence, None)
                    .await
                    .map(|_| ())
            }
            ConflictAction::SupersessionCandidate { evidence } => store
                .update_with(existing_id, move |memory| {
                    memory.add_challenge(Challenge::new(evidence.clone()));
                    // add_challenge sets Challenged; supersession candidates
                    // deliberately sit in NeedsReview instead — zero score
                    // penalty, human confirms.
                    memory.status = Status::NeedsReview;
                    Ok(())
                })
                .await
                .map(|_| ())
                .map_err(anyhow::Error::from),
        };
        if let Err(e) = write_result {
            tracing::warn!("Failed to write NLI conflict action for {existing_id}: {e}");
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

#[cfg(test)]
mod routing_tests {
    use super::*;
    use crate::nli::NliLabel;
    use engram_types::{MemoryType, Provenance};

    fn nli(score: f32) -> NliResult {
        NliResult {
            label: NliLabel::Contradiction,
            entailment: 0.05,
            neutral: 0.05,
            contradiction: score,
        }
    }

    fn new_meta(epistemic: Epistemic) -> NewMemoryMeta {
        NewMemoryMeta {
            id: "new-id".into(),
            summary: "new claim".into(),
            epistemic,
            provenance_source: ProvenanceSource::Agent,
            confidence: 0.8,
            anchor: "2026-07-01T00:00:00Z".parse().unwrap(),
        }
    }

    fn existing(epistemic: Epistemic) -> Memory {
        let mut m = Memory::new(
            MemoryType::Context,
            "old claim",
            "content",
            Provenance::agent("test"),
        );
        m.id = "old-id".into();
        m.epistemic = epistemic;
        m.confidence = 0.8;
        m.created_at = "2026-06-01T00:00:00Z".parse().unwrap();
        m
    }

    #[test]
    fn decision_vs_decision_is_supersession_candidate() {
        let action = route_contradiction(
            &new_meta(Epistemic::Decision),
            &existing(Epistemic::Decision),
            &nli(0.9),
        );
        match action {
            ConflictAction::SupersessionCandidate { evidence } => {
                assert!(evidence.contains("new-id"));
                assert!(evidence.contains("supersede"));
            }
            other => panic!("expected supersession candidate, got {other:?}"),
        }
    }

    #[test]
    fn anything_vs_decision_is_premise_challenge() {
        for new_class in [Epistemic::Fact, Epistemic::Observation] {
            let action = route_contradiction(
                &new_meta(new_class),
                &existing(Epistemic::Decision),
                &nli(0.87),
            );
            match action {
                ConflictAction::ChallengeExisting { evidence } => {
                    assert!(
                        evidence.starts_with("premise may have changed: contradicted by new-id"),
                        "premise challenge text: {evidence}"
                    );
                    assert!(evidence.contains("0.87"));
                }
                other => panic!("expected premise challenge, got {other:?}"),
            }
        }
    }

    #[test]
    fn observation_or_decision_vs_fact_is_stale_fact_with_date() {
        for new_class in [Epistemic::Observation, Epistemic::Decision] {
            let action =
                route_contradiction(&new_meta(new_class), &existing(Epistemic::Fact), &nli(0.9));
            match action {
                ConflictAction::ChallengeExisting { evidence } => {
                    assert!(evidence.contains("stale-fact"), "{evidence}");
                    assert!(
                        evidence.contains("2026-07-01"),
                        "stale-fact evidence carries the contradicting memory's date: {evidence}"
                    );
                }
                other => panic!("expected stale-fact challenge, got {other:?}"),
            }
        }
    }

    #[test]
    fn fact_or_decision_vs_observation_challenges_observation() {
        for new_class in [Epistemic::Fact, Epistemic::Decision] {
            let action = route_contradiction(
                &new_meta(new_class),
                &existing(Epistemic::Observation),
                &nli(0.9),
            );
            match action {
                ConflictAction::ChallengeExisting { evidence } => {
                    assert!(evidence.contains("observation"), "{evidence}");
                }
                other => panic!("expected observation challenge, got {other:?}"),
            }
        }
    }

    #[test]
    fn same_class_entrenchment_trust_then_age_then_confidence() {
        // Trust: inferred existing yields to agent new.
        let mut low_trust = existing(Epistemic::Fact);
        low_trust.provenance = Provenance::inferred();
        assert!(matches!(
            route_contradiction(&new_meta(Epistemic::Fact), &low_trust, &nli(0.9)),
            ConflictAction::ChallengeExisting { .. }
        ));

        // Trust: HUMAN existing outranks agent new → the NEW side yields.
        let mut high_trust = existing(Epistemic::Fact);
        high_trust.provenance = Provenance::human();
        assert!(matches!(
            route_contradiction(&new_meta(Epistemic::Fact), &high_trust, &nli(0.9)),
            ConflictAction::ChallengeNew { .. }
        ));

        // Equal trust: older side yields (existing created 2026-06-01 <
        // new anchor 2026-07-01 → existing yields).
        assert!(matches!(
            route_contradiction(
                &new_meta(Epistemic::Observation),
                &existing(Epistemic::Observation),
                &nli(0.9)
            ),
            ConflictAction::ChallengeExisting { .. }
        ));
        // …and a NEWER existing (via verified_at) flips it: new yields.
        let mut newer = existing(Epistemic::Observation);
        newer.verified_at = Some("2026-07-15T00:00:00Z".parse().unwrap());
        assert!(matches!(
            route_contradiction(&new_meta(Epistemic::Observation), &newer, &nli(0.9)),
            ConflictAction::ChallengeNew { .. }
        ));

        // Equal trust + equal anchor: lower confidence yields.
        let mut same_age = existing(Epistemic::Fact);
        same_age.created_at = "2026-07-01T00:00:00Z".parse().unwrap();
        same_age.confidence = 0.4; // below new's 0.8
        assert!(matches!(
            route_contradiction(&new_meta(Epistemic::Fact), &same_age, &nli(0.9)),
            ConflictAction::ChallengeExisting { .. }
        ));

        // Full tie: existing yields (status quo — new information wins).
        let mut tie = existing(Epistemic::Fact);
        tie.created_at = "2026-07-01T00:00:00Z".parse().unwrap();
        tie.confidence = 0.8;
        assert!(matches!(
            route_contradiction(&new_meta(Epistemic::Fact), &tie, &nli(0.9)),
            ConflictAction::ChallengeExisting { .. }
        ));
    }

    #[tokio::test]
    async fn supersession_candidate_write_sets_needs_review_no_penalty_status() {
        use engram_storage::{InMemoryRegistry, MemoryStore};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        let mut old_decision = existing(Epistemic::Decision);
        old_decision.type_ = MemoryType::Decision;
        store.create(&old_decision).await.unwrap();

        challenge_for_contradictions(
            &store,
            &new_meta(Epistemic::Decision),
            &[("old-id".to_string(), nli(0.9))],
        )
        .await;

        let m = store.get("old-id").await.unwrap();
        assert_eq!(
            m.status,
            Status::NeedsReview,
            "supersession candidate sits in NeedsReview, not Challenged"
        );
        assert_eq!(m.challenges.len(), 1);
        assert!(m.challenges[0].evidence.contains("new-id"));
    }

    #[tokio::test]
    async fn challenge_new_write_lands_on_the_new_memory() {
        use engram_storage::{InMemoryRegistry, MemoryStore};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        // Entrenched human fact…
        let mut fact = existing(Epistemic::Fact);
        fact.provenance = Provenance::human();
        store.create(&fact).await.unwrap();
        // …and the (stored) new agent memory that contradicts it.
        let mut new_mem = Memory::new(
            MemoryType::Context,
            "new claim",
            "content",
            Provenance::agent("test"),
        );
        new_mem.id = "new-id".into();
        new_mem.epistemic = Epistemic::Fact;
        store.create(&new_mem).await.unwrap();

        challenge_for_contradictions(
            &store,
            &new_meta(Epistemic::Fact),
            &[("old-id".to_string(), nli(0.9))],
        )
        .await;

        assert_eq!(
            store.get("old-id").await.unwrap().status,
            Status::Active,
            "entrenched side keeps its status"
        );
        let new_mem = store.get("new-id").await.unwrap();
        assert_eq!(new_mem.status, Status::Challenged, "new side yields");
        assert!(new_mem.challenges[0].evidence.contains("old-id"));
    }
}
