//! Graph and usage evidence for retrieval (phase 5): the `contradicts`
//! penalty, the `supports` boost, the usage boost and one-hop link
//! expansion. Every one of them is **off by default**, and off means *no
//! lookup at all*: [`RetrievalEngine::query`](super::RetrievalEngine::query)
//! calls into this module only when a knob asks for it, so a default query
//! performs exactly the I/O it did before these existed.
//!
//! # Cost
//!
//! Per query, with a knob on:
//!
//! - **Edges**: one `edges`-table query for the whole candidate set
//!   ([`MemoryStore::edges_touching_any`]), never one per candidate. Whether
//!   an endpoint is live is read off its own index row when it is a
//!   candidate; only far ends outside the candidate set are looked up, in one
//!   more query ([`MemoryStore::list_for_filtering_by_ids`]). Expansion reuses that snapshot when the scoring knobs
//!   already loaded it, and otherwise loads edges for the top hits only.
//! - **Usage**: [`load_current_usage`](crate::telemetry::usage::load_current_usage)
//!   (the rollup table plus the undrained log) once per
//!   [`USAGE_CACHE_TTL`] per engine. Usage moves slowly and is already
//!   approximate (distinct-session counts are an upper bound), so a
//!   half-minute-old view costs nothing that matters.
//!
//! # Declared losses
//!
//! - **A failed read means no signal, never an error.** An unreadable edges
//!   table scores as if no edge existed; an unreadable usage view as if no
//!   memory had usage. Both are logged at debug level. A query must never
//!   fail because evidence *about* its results could not be read.
//! - **Cross-store edges carry no signal and are not expanded.** Whether the
//!   far end is live would need another store opened per query, and an
//!   unknown end is not evidence (the same rule `links` applies to
//!   `Unknown` vs `Dangling`). Only same-store edges count.
//! - **The usage view can be [`USAGE_CACHE_TTL`] stale** within one engine.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::scoring::{ScoreSignals, UsageSignal};
use crate::storage::edge_index::EdgeRow;
use crate::storage::{IndexForFiltering, MemoryStore};
use crate::types::{LinkKind, LinkStatus};

/// How long one engine reuses a usage view.
pub const USAGE_CACHE_TTL: Duration = Duration::from_secs(30);

/// Edge kinds link expansion follows, in either direction.
///
/// - `supports`, `derived_from`: the evidence for a hit, or what rests on it.
/// - `related_to`: the explicit "see also".
///
/// Not followed: `contradicts` (a contradiction is not context to add; its
/// effect is the penalty), `duplicates` (the same content again, which only
/// spends the budget), `supersedes` (one end is invalidated, so it is hidden
/// by default anyway, and the live end supersedes rather than explains).
/// Only confirmed edges are followed; a proposed or rejected edge never is.
pub fn expansion_follows(kind: &LinkKind) -> bool {
    matches!(
        kind,
        LinkKind::Supports | LinkKind::DerivedFrom | LinkKind::RelatedTo
    )
}

/// A memory is *live* when it exists in the store and is neither
/// invalidated nor expired now: what a default query would still serve.
fn is_live(entry: &IndexForFiltering, now: DateTime<Utc>) -> bool {
    entry.invalidated_at.is_none_or(|t| t > now) && entry.expires_at.is_none_or(|t| t > now)
}

/// The confirmed same-store edges touching a set of memories, plus which of
/// their endpoints are live.
#[derive(Debug, Default)]
pub struct GraphSnapshot {
    edges: Vec<EdgeRow>,
    live: HashSet<String>,
}

impl GraphSnapshot {
    /// Load the edges touching `entries` — one query — and, when
    /// `with_liveness`, which endpoints are live. An endpoint among `entries`
    /// is judged from its own row; only the others are looked up, in one
    /// more query (`IN` lists of at most 500). A failure of either reads as
    /// an empty snapshot (module docs, "Declared losses"). Expansion alone
    /// passes `with_liveness = false`: its neighbours must be in the
    /// filtered candidate set anyway, which is a stricter test.
    pub async fn load(
        store: &MemoryStore,
        entries: &[&IndexForFiltering],
        with_liveness: bool,
        now: DateTime<Utc>,
    ) -> Self {
        let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
        let edges = match store.edges_touching_any(&ids).await {
            Ok(rows) => rows
                .into_iter()
                .filter(|r| r.status == LinkStatus::Confirmed && r.dst_store.is_none())
                .collect::<Vec<_>>(),
            Err(e) => {
                tracing::debug!("graph signals: edges unreadable, scoring without them: {e}");
                return Self::default();
            }
        };
        if edges.is_empty() || !with_liveness {
            return Self {
                edges,
                live: HashSet::new(),
            };
        }
        let known: HashMap<&str, &IndexForFiltering> =
            entries.iter().map(|e| (e.id.as_str(), *e)).collect();
        let mut live: HashSet<String> = HashSet::new();
        let mut unknown: Vec<String> = Vec::new();
        for id in edges
            .iter()
            .flat_map(|r| [r.src_id.as_str(), r.dst_id.as_str()])
            .collect::<HashSet<_>>()
        {
            match known.get(id) {
                Some(e) if is_live(e, now) => {
                    live.insert(id.to_string());
                }
                Some(_) => {}
                None => unknown.push(id.to_string()),
            }
        }
        if !unknown.is_empty() {
            unknown.sort();
            match store.list_for_filtering_by_ids(&unknown).await {
                Ok(found) => {
                    live.extend(found.into_iter().filter(|e| is_live(e, now)).map(|e| e.id))
                }
                Err(e) => {
                    tracing::debug!(
                        "graph signals: liveness unreadable, scoring without edges: {e}"
                    );
                    return Self::default();
                }
            }
        }
        Self { edges, live }
    }

    /// Build a snapshot from rows already in hand (tests).
    #[cfg(test)]
    pub fn from_parts(edges: Vec<EdgeRow>, live: HashSet<String>) -> Self {
        Self { edges, live }
    }

    /// Per-memory `contradicted_by` / `supported_by` for every id in `ids`
    /// that has any, from one pass over the edges.
    ///
    /// - `contradicts` is symmetric: each end is contradicted by the other,
    ///   when the other is live.
    /// - `supports` counts only *incoming* edges (`X supports id`) from a live
    ///   `X`: supporting something is not evidence for oneself.
    pub fn score_signals(&self, ids: &HashSet<&str>) -> HashMap<String, ScoreSignals> {
        let mut out: HashMap<String, ScoreSignals> = HashMap::new();
        for r in &self.edges {
            match r.kind {
                LinkKind::Contradicts => {
                    for (me, other) in [(&r.src_id, &r.dst_id), (&r.dst_id, &r.src_id)] {
                        if me != other
                            && ids.contains(me.as_str())
                            && self.live.contains(other.as_str())
                        {
                            out.entry(me.clone())
                                .or_default()
                                .contradicted_by
                                .push(other.clone());
                        }
                    }
                }
                LinkKind::Supports
                    if r.src_id != r.dst_id
                        && ids.contains(r.dst_id.as_str())
                        && self.live.contains(r.src_id.as_str()) =>
                {
                    out.entry(r.dst_id.clone())
                        .or_default()
                        .supported_by
                        .push(r.src_id.clone());
                }
                _ => {}
            }
        }
        for s in out.values_mut() {
            s.contradicted_by.sort();
            s.contradicted_by.dedup();
            s.supported_by.sort();
            s.supported_by.dedup();
        }
        out
    }

    /// The memories one confirmed, followable edge away from `id`, with the
    /// kind of edge (either direction; see [`expansion_follows`]).
    pub fn neighbours<'a>(&'a self, id: &'a str) -> impl Iterator<Item = (&'a str, &'a LinkKind)> {
        self.edges.iter().filter_map(move |r| {
            if !expansion_follows(&r.kind) || r.src_id == r.dst_id {
                return None;
            }
            if r.src_id == id {
                Some((r.dst_id.as_str(), &r.kind))
            } else if r.dst_id == id {
                Some((r.src_id.as_str(), &r.kind))
            } else {
                None
            }
        })
    }
}

/// Usage counts by full memory id.
pub type UsageMap = HashMap<String, UsageSignal>;

/// A per-engine cache of the store's usage view (module docs).
#[derive(Default)]
pub struct UsageCache {
    slot: Mutex<Option<(Instant, Arc<UsageMap>)>>,
}

impl UsageCache {
    /// The store's usage, from the cache when fresh. A failed read is cached
    /// as empty for the TTL too, so a broken log costs one read per TTL rather
    /// than one per query.
    pub async fn get(&self, store: &MemoryStore) -> Arc<HashMap<String, UsageSignal>> {
        if let Some((at, view)) = self.slot.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if at.elapsed() < USAGE_CACHE_TTL {
                return view.clone();
            }
        }
        let view = Arc::new(load_usage_signals(store).await);
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), view.clone()));
        view
    }
}

/// Every memory's usage counts in `store`, through the side-effect-free read
/// API. Empty on any failure (module docs).
async fn load_usage_signals(store: &MemoryStore) -> HashMap<String, UsageSignal> {
    match crate::telemetry::usage::load_current_usage(&store.project_dir, &store.project_id).await {
        Ok(view) => view
            .by_memory
            .into_iter()
            .map(|(id, u)| {
                (
                    id,
                    UsageSignal {
                        fetched: u.counts.fetched,
                        injected: u.counts.injected,
                        injected_sessions: u.counts.injected_sessions,
                    },
                )
            })
            .collect(),
        Err(e) => {
            tracing::debug!("usage signal: usage unreadable, scoring without it: {e}");
            HashMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::edge_index::EdgeField;
    use crate::types::LinkOrigin;

    fn row(src: &str, kind: LinkKind, dst: &str) -> EdgeRow {
        EdgeRow {
            owner_id: src.into(),
            src_id: src.into(),
            dst_id: dst.into(),
            dst_store: None,
            kind,
            confidence: 1.0,
            origin: LinkOrigin::Human,
            status: LinkStatus::Confirmed,
            created_at: Utc::now(),
            src_invalidated: false,
            note: None,
            field: EdgeField::Links,
        }
    }

    fn live(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn contradicts_penalizes_both_live_ends() {
        let g = GraphSnapshot::from_parts(
            vec![row("a", LinkKind::Contradicts, "b")],
            live(&["a", "b"]),
        );
        let s = g.score_signals(&["a", "b"].into_iter().collect());
        assert_eq!(s["a"].contradicted_by, vec!["b"]);
        assert_eq!(s["b"].contradicted_by, vec!["a"]);
    }

    #[test]
    fn contradiction_with_a_dead_end_carries_no_signal() {
        // b is invalidated, expired or missing: not in `live`.
        let g = GraphSnapshot::from_parts(vec![row("a", LinkKind::Contradicts, "b")], live(&["a"]));
        let s = g.score_signals(&["a", "b"].into_iter().collect());
        assert!(
            !s.contains_key("a"),
            "a dead contradicter must not penalize"
        );
        // b itself is still contradicted by the live a (it is only served
        // under include_invalidated / include_expired).
        assert_eq!(s["b"].contradicted_by, vec!["a"]);
    }

    #[test]
    fn supports_counts_incoming_edges_only() {
        let g =
            GraphSnapshot::from_parts(vec![row("c", LinkKind::Supports, "a")], live(&["a", "c"]));
        let s = g.score_signals(&["a", "c"].into_iter().collect());
        assert_eq!(s["a"].supported_by, vec!["c"]);
        assert!(
            !s.contains_key("c"),
            "supporting is not evidence for oneself"
        );
    }

    #[test]
    fn other_kinds_carry_no_score_signal() {
        let g = GraphSnapshot::from_parts(
            vec![
                row("a", LinkKind::RelatedTo, "b"),
                row("a", LinkKind::Duplicates, "b"),
                row("a", LinkKind::DerivedFrom, "b"),
                row("a", LinkKind::Supersedes, "b"),
            ],
            live(&["a", "b"]),
        );
        assert!(g
            .score_signals(&["a", "b"].into_iter().collect())
            .is_empty());
    }

    #[test]
    fn neighbours_follow_only_expansion_kinds_both_ways() {
        let g = GraphSnapshot::from_parts(
            vec![
                row("a", LinkKind::Supports, "b"),
                row("c", LinkKind::DerivedFrom, "a"),
                row("a", LinkKind::RelatedTo, "d"),
                row("a", LinkKind::Contradicts, "e"),
                row("a", LinkKind::Duplicates, "f"),
                row("a", LinkKind::Supersedes, "g"),
            ],
            HashSet::new(),
        );
        let mut n: Vec<(&str, String)> = g
            .neighbours("a")
            .map(|(id, k)| (id, k.as_str().to_string()))
            .collect();
        n.sort();
        assert_eq!(
            n,
            vec![
                ("b", "supports".to_string()),
                ("c", "derived_from".to_string()),
                ("d", "related_to".to_string()),
            ]
        );
    }
}
