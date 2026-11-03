//! Automatic edge proposals: NLI contradictions and embedding neighbours.
//!
//! Both proposers run on what an ingest already computed
//! ([`crate::retrieval::IngestFindings`]: the NLI pairs and the vector
//! neighbours of a memory that was just embedded), so they cost no model call
//! of their own and never run on a hook path — hooks do not create memories.
//! They run on `create` (the MCP background ingest and the CLI's inline one)
//! and on an MCP `update`, which is exactly where NLI already runs.
//!
//! Every proposal goes through [`super::links::link`] with `status:
//! proposed`, so it inherits the edge rules there — the personal-id rule
//! picks the file (a symmetric edge from a committed memory to a personal one
//! is stored on the personal side), one copy per edge, the per-file
//! [`super::links::MAX_LINKS_PER_MEMORY`] guard. On top of that:
//!
//! - **Never onto an existing relation.** A proposal is skipped when *any*
//!   edge already joins the two memories, of any kind and any status, in
//!   either file. That is the phase-3 "a proposal never overwrites" rule
//!   widened on purpose: a `rejected` edge is a reviewer's "no" and must not
//!   come back, a `supersedes` left by a resolved contradiction already says
//!   what the pair is, and a `related_to` on top of a `contradicts` is noise.
//! - **A per-memory cap.** At most `[links].max_proposals_per_memory` pending
//!   automatic proposals (`origin: nli | similarity`, `status: proposed`) may
//!   touch one memory; a proposal is skipped when either end is at it, so a
//!   noisy model cannot flood a committed file. Contradictions are proposed
//!   before neighbours, so they win the slots.
//! - **No score change.** A proposed edge is not read by scoring (phase 5
//!   reads confirmed edges only), the same rule as `NeedsReview`.
//!
//! Each written proposal records a `linked` usage event from
//! [`Surface::Maintenance`]: it is an automatic write, not a request from the
//! front end that created the memory (whose own `created` event is recorded
//! already). Everything here is best-effort — a failed proposal is logged at
//! `warn` and never fails the create that triggered it; the loss is declared
//! here and in [`ProposalReport::failed`].

use std::collections::HashMap;

use serde::Serialize;

use super::links::{link, LinkKind, LinkOrigin, LinkParams, LinkStatus};
use super::usage::Surface;
use crate::retrieval::{IngestFindings, IngestFollowUp};
use crate::storage::MemoryStore;
use crate::types::LinksConfig;

/// Why a candidate pair was not proposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// An edge of some kind already joins the two (rejected ones included).
    Existing,
    /// One end already has `max_proposals_per_memory` pending proposals.
    Capped,
    /// The candidate is the memory itself.
    SelfLoop,
}

/// What one proposer pass did, for logs and tests.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProposalReport {
    /// `(other id, kind)` of each edge written.
    pub proposed: Vec<(String, LinkKind)>,
    pub skipped: Vec<(String, SkipReason)>,
    /// `(other id, error)` of each write that failed (logged, not retried).
    pub failed: Vec<(String, String)>,
}

/// One candidate edge from `memory_id` to `other`.
struct Candidate {
    other: String,
    kind: LinkKind,
    confidence: f64,
    origin: LinkOrigin,
    note: String,
}

fn is_auto_pending(origin: &LinkOrigin, status: &LinkStatus) -> bool {
    *status == LinkStatus::Proposed && matches!(origin, LinkOrigin::Nli | LinkOrigin::Similarity)
}

/// Pending automatic proposals touching `id`, and the same-store ids it is
/// already joined to by any edge.
async fn neighbourhood(
    store: &MemoryStore,
    id: &str,
) -> anyhow::Result<(usize, std::collections::HashSet<String>)> {
    let rows = store.edges_touching(id).await?;
    let mut pending = 0;
    let mut joined = std::collections::HashSet::new();
    for r in rows {
        if is_auto_pending(&r.origin, &r.status) {
            pending += 1;
        }
        if r.dst_store.is_none() {
            let other = if r.src_id == id { r.dst_id } else { r.src_id };
            joined.insert(other);
        }
    }
    Ok((pending, joined))
}

async fn propose(
    store: &MemoryStore,
    memory_id: &str,
    candidates: Vec<Candidate>,
    cap: usize,
) -> ProposalReport {
    let mut report = ProposalReport::default();
    if candidates.is_empty() || cap == 0 {
        return report;
    }
    let (mut own_pending, joined) = match neighbourhood(store, memory_id).await {
        Ok(v) => v,
        Err(e) => {
            // Without the existing edges the no-overwrite rule cannot be
            // checked, so propose nothing rather than guess.
            tracing::warn!(
                memory_id,
                "edge proposals skipped: cannot read edges: {e:#}"
            );
            for c in candidates {
                report.failed.push((c.other, format!("{e:#}")));
            }
            return report;
        }
    };
    // Pending counts of the other ends, read once each.
    let mut other_pending: HashMap<String, usize> = HashMap::new();
    for c in candidates {
        if c.other == memory_id {
            report.skipped.push((c.other, SkipReason::SelfLoop));
            continue;
        }
        if joined.contains(&c.other) || report.proposed.iter().any(|(o, _)| *o == c.other) {
            report.skipped.push((c.other, SkipReason::Existing));
            continue;
        }
        if own_pending >= cap {
            report.skipped.push((c.other, SkipReason::Capped));
            continue;
        }
        let theirs = match other_pending.get(&c.other) {
            Some(n) => *n,
            None => match neighbourhood(store, &c.other).await {
                Ok((n, their_joined)) => {
                    if their_joined.contains(memory_id) {
                        // Joined from their side only (a row our read missed
                        // cannot exist, but a concurrent writer can add one).
                        report.skipped.push((c.other, SkipReason::Existing));
                        continue;
                    }
                    other_pending.insert(c.other.clone(), n);
                    n
                }
                Err(e) => {
                    tracing::warn!(memory_id, other = %c.other, "edge proposal skipped: {e:#}");
                    report.failed.push((c.other, format!("{e:#}")));
                    continue;
                }
            },
        };
        if theirs >= cap {
            report.skipped.push((c.other, SkipReason::Capped));
            continue;
        }
        let params = LinkParams {
            src: memory_id.to_string(),
            kind: c.kind.clone(),
            dst: c.other.clone(),
            dst_store: None,
            confidence: Some(c.confidence.clamp(0.0, 1.0)),
            origin: c.origin,
            status: Some(LinkStatus::Proposed),
            note: Some(c.note),
        };
        match link(store, params, Surface::Maintenance).await {
            Ok(r) if r.created => {
                own_pending += 1;
                *other_pending.entry(c.other.clone()).or_insert(0) += 1;
                report.proposed.push((c.other, c.kind));
            }
            // `link` answered without writing: the same edge exists (a
            // concurrent writer got there between our read and its plan).
            Ok(_) => report.skipped.push((c.other, SkipReason::Existing)),
            Err(e) => {
                tracing::warn!(memory_id, other = %c.other, "edge proposal not written: {e:#}");
                report.failed.push((c.other, format!("{e:#}")));
            }
        }
    }
    report
}

/// Propose a `contradicts` edge (origin `nli`, the contradiction probability
/// as its confidence) for each `(existing id, probability)` pair.
pub async fn propose_contradictions(
    store: &MemoryStore,
    memory_id: &str,
    contradictions: &[(String, f64)],
    cfg: &LinksConfig,
) -> ProposalReport {
    if !cfg.contradictions_enabled() {
        return ProposalReport::default();
    }
    let candidates = contradictions
        .iter()
        .map(|(other, p)| Candidate {
            other: other.clone(),
            kind: LinkKind::Contradicts,
            confidence: *p,
            origin: LinkOrigin::Nli,
            note: format!("NLI contradiction probability {p:.2}"),
        })
        .collect();
    propose(store, memory_id, candidates, cfg.max_proposals_per_memory).await
}

/// Propose `duplicates` (at or above `duplicate_threshold`) or `related_to`
/// (at or above `related_threshold`) for each `(neighbour id, similarity)`,
/// best first. Neighbours below both thresholds are ignored.
pub async fn propose_similar(
    store: &MemoryStore,
    memory_id: &str,
    neighbours: &[(String, f64)],
    cfg: &LinksConfig,
) -> ProposalReport {
    if !cfg.similar_enabled() {
        return ProposalReport::default();
    }
    let mut sorted: Vec<&(String, f64)> = neighbours
        .iter()
        .filter(|(_, s)| s.is_finite() && *s >= cfg.related_threshold)
        .collect();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let candidates = sorted
        .into_iter()
        .take(cfg.similar_candidates)
        .map(|(other, score)| {
            let kind = if *score >= cfg.duplicate_threshold {
                LinkKind::Duplicates
            } else {
                LinkKind::RelatedTo
            };
            Candidate {
                other: other.clone(),
                kind,
                confidence: *score,
                origin: LinkOrigin::Similarity,
                note: format!("embedding similarity {score:.2}"),
            }
        })
        .collect();
    propose(store, memory_id, candidates, cfg.max_proposals_per_memory).await
}

/// Both proposers over one ingest's findings, contradictions first (they get
/// the capped slots before neighbours do).
pub async fn apply_ingest_findings(
    store: &MemoryStore,
    memory_id: &str,
    findings: &IngestFindings,
    cfg: &LinksConfig,
) -> (ProposalReport, ProposalReport) {
    let contradictions: Vec<(String, f64)> = findings
        .contradictions
        .iter()
        .map(|(id, r)| (id.clone(), f64::from(r.contradiction)))
        .collect();
    let c = propose_contradictions(store, memory_id, &contradictions, cfg).await;
    let neighbours: Vec<(String, f64)> = findings
        .neighbours
        .iter()
        .map(|m| (m.id.clone(), m.score))
        .collect();
    let s = propose_similar(store, memory_id, &neighbours, cfg).await;
    if !c.proposed.is_empty() || !s.proposed.is_empty() {
        tracing::debug!(
            memory_id,
            contradictions = c.proposed.len(),
            similar = s.proposed.len(),
            "proposed memory edges"
        );
    }
    (c, s)
}

/// The [`IngestFollowUp`] `create` / `update` hand to
/// [`crate::retrieval::RetrievalEngine::spawn_ingest`].
pub fn follow_up(cfg: LinksConfig) -> IngestFollowUp {
    Box::new(move |store, memory, findings| {
        Box::pin(async move {
            apply_ingest_findings(&store, &memory.id, &findings, &cfg).await;
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::links::{links_of, set_link_status, EdgeRef, LinkFilter};
    use crate::ops::usage::Surface;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Memory, MemoryType, Provenance, Visibility};
    use tempfile::TempDir;

    async fn store() -> (TempDir, MemoryStore) {
        let tmp = TempDir::new().unwrap();
        let store = MemoryStore::init(tmp.path(), &InMemoryRegistry::new())
            .await
            .unwrap();
        (tmp, store)
    }

    async fn mem(store: &MemoryStore, summary: &str, visibility: Visibility) -> String {
        let mut m = Memory::new(MemoryType::Decision, summary, "body", Provenance::human());
        m.visibility = visibility;
        store.create(&m).await.unwrap()
    }

    fn cfg() -> LinksConfig {
        LinksConfig::default()
    }

    async fn all_edges(store: &MemoryStore, id: &str) -> Vec<crate::ops::links::EdgeView> {
        let v = links_of(store, id, &LinkFilter::all()).await.unwrap();
        v.outbound.into_iter().chain(v.inbound).collect()
    }

    #[tokio::test]
    async fn nli_contradiction_becomes_a_proposed_edge_with_the_score() {
        let (_t, store) = store().await;
        let old = mem(&store, "use sqlite", Visibility::Shared).await;
        let new = mem(&store, "use postgres", Visibility::Shared).await;
        let r = propose_contradictions(&store, &new, &[(old.clone(), 0.91)], &cfg()).await;
        assert_eq!(r.proposed, vec![(old.clone(), LinkKind::Contradicts)]);
        let edges = all_edges(&store, &new).await;
        assert_eq!(edges.len(), 1);
        let e = &edges[0];
        assert_eq!(e.kind, LinkKind::Contradicts);
        assert_eq!(e.origin, LinkOrigin::Nli);
        assert_eq!(e.status, LinkStatus::Proposed);
        assert!((e.confidence - 0.91).abs() < 1e-9);
        // Written into the new memory's file, recorded as `linked`.
        assert_eq!(store.get(&new).await.unwrap().links.len(), 1);
        assert!(store.get(&old).await.unwrap().links.is_empty());
    }

    #[tokio::test]
    async fn similarity_picks_the_kind_by_threshold_and_ignores_weak_neighbours() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let dup = mem(&store, "dup", Visibility::Shared).await;
        let rel = mem(&store, "rel", Visibility::Shared).await;
        let weak = mem(&store, "weak", Visibility::Shared).await;
        let r = propose_similar(
            &store,
            &a,
            &[
                (rel.clone(), 0.85),
                (weak.clone(), 0.5),
                (dup.clone(), 0.97),
            ],
            &cfg(),
        )
        .await;
        assert_eq!(
            r.proposed,
            vec![
                (dup.clone(), LinkKind::Duplicates),
                (rel.clone(), LinkKind::RelatedTo)
            ]
        );
        for e in all_edges(&store, &a).await {
            assert_eq!(e.origin, LinkOrigin::Similarity);
            assert_eq!(e.status, LinkStatus::Proposed);
        }
    }

    #[tokio::test]
    async fn the_cap_bounds_pending_proposals_on_both_ends() {
        let (_t, store) = store().await;
        let mut c = cfg();
        c.max_proposals_per_memory = 2;
        let hub = mem(&store, "hub", Visibility::Shared).await;
        let others: Vec<String> = futures_ids(&store, 4).await;
        let pairs: Vec<(String, f64)> = others.iter().map(|o| (o.clone(), 0.85)).collect();
        let r = propose_similar(&store, &hub, &pairs, &c).await;
        assert_eq!(r.proposed.len(), 2);
        assert_eq!(
            r.skipped
                .iter()
                .filter(|(_, why)| *why == SkipReason::Capped)
                .count(),
            2
        );

        // The other end's cap counts too: `others[0]` already takes part in
        // one proposal; with a cap of 1 it takes no second one.
        c.max_proposals_per_memory = 1;
        let fresh = mem(&store, "fresh", Visibility::Shared).await;
        let r = propose_similar(&store, &fresh, &[(others[0].clone(), 0.85)], &c).await;
        assert!(r.proposed.is_empty());
        assert_eq!(r.skipped, vec![(others[0].clone(), SkipReason::Capped)]);

        // Reviewing a proposal frees its slot.
        set_link_status(
            &store,
            EdgeRef {
                src: hub.clone(),
                kind: LinkKind::RelatedTo,
                dst: others[0].clone(),
                dst_store: None,
            },
            LinkStatus::Confirmed,
            None,
            Surface::Cli,
        )
        .await
        .unwrap();
        let r = propose_similar(&store, &fresh, &[(others[0].clone(), 0.85)], &c).await;
        assert_eq!(r.proposed.len(), 1);

        // Off switches.
        c.max_proposals_per_memory = 0;
        let r = propose_similar(&store, &fresh, &[(others[3].clone(), 0.99)], &c).await;
        assert!(r.proposed.is_empty() && r.skipped.is_empty());
        let off = LinksConfig {
            propose_similar: false,
            propose_contradictions: false,
            ..cfg()
        };
        assert!(
            propose_similar(&store, &fresh, &[(others[3].clone(), 0.99)], &off)
                .await
                .proposed
                .is_empty()
        );
        assert!(
            propose_contradictions(&store, &fresh, &[(others[3].clone(), 0.99)], &off)
                .await
                .proposed
                .is_empty()
        );
    }

    async fn futures_ids(store: &MemoryStore, n: usize) -> Vec<String> {
        let mut v = Vec::new();
        for i in 0..n {
            v.push(mem(store, &format!("other {i}"), Visibility::Shared).await);
        }
        v
    }

    #[tokio::test]
    async fn never_overwrites_and_never_re_proposes_a_rejected_edge() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        let c = mem(&store, "c", Visibility::Shared).await;

        // NLI proposes; the reviewer rejects; NLI sees the pair again, from
        // either side, and proposes nothing.
        propose_contradictions(&store, &a, &[(b.clone(), 0.9)], &cfg()).await;
        set_link_status(
            &store,
            EdgeRef {
                src: a.clone(),
                kind: LinkKind::Contradicts,
                dst: b.clone(),
                dst_store: None,
            },
            LinkStatus::Rejected,
            None,
            Surface::Cli,
        )
        .await
        .unwrap();
        for (x, y) in [(&a, &b), (&b, &a)] {
            let r = propose_contradictions(&store, x, &[(y.clone(), 0.99)], &cfg()).await;
            assert_eq!(r.skipped, vec![(y.clone(), SkipReason::Existing)]);
            // Nor does a similarity proposal stack on top of it.
            let r = propose_similar(&store, x, &[(y.clone(), 0.99)], &cfg()).await;
            assert_eq!(r.skipped, vec![(y.clone(), SkipReason::Existing)]);
        }
        let edges = all_edges(&store, &a).await;
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].status, LinkStatus::Rejected);

        // A confirmed human edge keeps its origin, status and note.
        link(
            &store,
            LinkParams {
                src: a.clone(),
                kind: LinkKind::Supports,
                dst: c.clone(),
                dst_store: None,
                confidence: None,
                origin: LinkOrigin::Human,
                status: None,
                note: Some("hand-made".into()),
            },
            Surface::Cli,
        )
        .await
        .unwrap();
        let r = propose_similar(&store, &c, &[(a.clone(), 0.99)], &cfg()).await;
        assert_eq!(r.skipped, vec![(a.clone(), SkipReason::Existing)]);
        let s = all_edges(&store, &c).await;
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].origin, LinkOrigin::Human);
        assert_eq!(s[0].note.as_deref(), Some("hand-made"));

        // The memory itself is never a candidate.
        let r = propose_similar(&store, &c, &[(c.clone(), 1.0)], &cfg()).await;
        assert_eq!(r.skipped, vec![(c.clone(), SkipReason::SelfLoop)]);
    }

    #[tokio::test]
    async fn a_shared_memory_never_names_a_personal_one() {
        let (_t, store) = store().await;
        let shared = mem(&store, "shared", Visibility::Shared).await;
        let personal = mem(&store, "personal", Visibility::Personal).await;
        let r = propose_contradictions(&store, &shared, &[(personal.clone(), 0.9)], &cfg()).await;
        assert_eq!(r.proposed.len(), 1);
        // Stored on the personal side; the committed file stays clean.
        assert!(store.get(&shared).await.unwrap().links.is_empty());
        let p = store.get(&personal).await.unwrap();
        assert_eq!(p.links.len(), 1);
        assert_eq!(p.links[0].to, shared);
        assert_eq!(p.links[0].origin, LinkOrigin::Nli);
        // And it is still the one edge between them.
        let r = propose_similar(&store, &shared, &[(personal.clone(), 0.99)], &cfg()).await;
        assert_eq!(r.skipped, vec![(personal.clone(), SkipReason::Existing)]);
    }

    #[tokio::test]
    async fn contradictions_take_the_capped_slots_first() {
        let (_t, store) = store().await;
        let new = mem(&store, "new", Visibility::Shared).await;
        let x = mem(&store, "x", Visibility::Shared).await;
        let y = mem(&store, "y", Visibility::Shared).await;
        let c = LinksConfig {
            max_proposals_per_memory: 1,
            ..cfg()
        };
        let findings = IngestFindings {
            contradictions: vec![(
                x.clone(),
                crate::nli::NliResult::from_probs(0.05, 0.05, 0.9),
            )],
            neighbours: vec![crate::storage::lance_index::VectorMatch {
                id: y.clone(),
                score: 0.99,
            }],
        };
        let (cr, sr) = apply_ingest_findings(&store, &new, &findings, &c).await;
        assert_eq!(cr.proposed, vec![(x, LinkKind::Contradicts)]);
        assert_eq!(sr.skipped, vec![(y, SkipReason::Capped)]);
    }

    #[tokio::test]
    async fn a_proposal_records_a_linked_maintenance_event() {
        let (_t, store) = store().await;
        let a = mem(&store, "a", Visibility::Shared).await;
        let b = mem(&store, "b", Visibility::Shared).await;
        propose_similar(&store, &a, &[(b.clone(), 0.85)], &cfg()).await;
        let events = crate::telemetry::usage::load_memory_events(&store.project_dir)
            .unwrap()
            .events;
        let linked: Vec<_> = events
            .iter()
            .filter(|e| e.kind == crate::ops::usage::UsageKind::Linked)
            .collect();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].surface, Surface::Maintenance);
        assert_eq!(linked[0].memory_id, a);
        assert_eq!(linked[0].related_id.as_deref(), Some(b.as_str()));
    }
}
