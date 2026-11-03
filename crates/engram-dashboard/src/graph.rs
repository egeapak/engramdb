//! `GET /api/projects/{id}/graph`: the route map's nodes and edges.
//!
//! Built on the phase-3 read API ([`engramdb::ops::links::load_graph`]) over
//! the read-only store, joined with the memories projection for each node's
//! line and with the usage rollup for its reads. Like every other endpoint it
//! is a pure read: a store without an `edges` table answers with no edges and
//! nothing is created.
//!
//! **Bounded.** A store can hold thousands of memories; the page lays them
//! out in a Web Worker and draws them in SVG, which stays responsive to a
//! couple of thousand stations. Past `limit` (default [`DEFAULT_LIMIT`], at
//! most [`MAX_LIMIT`]) the stations most worth drawing are kept — the most
//! connected first, then the most read, then the most critical — and the
//! response says how many it left out (`total_nodes` vs `shown_nodes`), so a
//! partial map never passes for the whole store. `line=` narrows to some
//! lines instead.
//!
//! **Far ends.** A same-store edge whose target is gone is returned with
//! `state: "dangling"` (drawn as a stub to a closed station); one into
//! another store is checked by opening that store read-only — `live` or
//! `dangling` when it answered, `unknown` when it could not be opened (or did
//! not answer within [`crate::projects::ROW_TIMEOUT`]): the absence of
//! evidence is not evidence of deletion.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use chrono::{DateTime, Utc};
use engramdb::ops::links::{GraphEdge, GraphFilter, LinkKind, LinkOrigin, LinkStatus};
use engramdb::storage::lance_index::IndexFilterable;
use engramdb::types::Status;
use serde::Serialize;

use crate::api::{line_of, line_slots, ok, open, query, status_str, wire, ApiResult};
use crate::{ApiError, AppState};

/// Stations drawn when `limit` is not given.
pub const DEFAULT_LIMIT: usize = 800;
/// The most stations one response carries.
pub const MAX_LIMIT: usize = 2000;
/// Foreign stores probed per request; edges into any beyond are `unknown`.
const MAX_FOREIGN_STORES: usize = 8;

#[derive(Serialize, Default)]
struct Reads {
    returned: u64,
    injected: u64,
    fetched: u64,
}

#[derive(Serialize)]
struct Node {
    id: String,
    summary: String,
    #[serde(rename = "type")]
    type_: String,
    epistemic: &'static str,
    status: &'static str,
    challenged: bool,
    invalidated: bool,
    visibility: String,
    criticality: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<String>,
    /// See [`crate::api::line_slots`].
    #[serde(skip_serializing_if = "Option::is_none")]
    line_slot: Option<u32>,
    /// The memory's first logical scope in full (`storage.lock`).
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    /// Three signals, never summed (see `stats`).
    usage: Reads,
    /// Edges in this response touching the node.
    degree: usize,
    /// Of those, `proposed` ones (the review queue).
    proposed: usize,
}

#[derive(Serialize)]
struct Edge {
    src: String,
    dst: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dst_store: Option<String>,
    kind: LinkKind,
    symmetric: bool,
    confidence: f64,
    origin: LinkOrigin,
    status: LinkStatus,
    created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// `live`, `invalidated` (the target is shown but its window closed),
    /// `dangling` or `unknown` (see the module docs).
    state: &'static str,
}

#[derive(Serialize)]
struct Line {
    /// `""` is "no logical scope".
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<u32>,
    /// Shown stations on the line.
    count: usize,
}

#[derive(Serialize, Default)]
struct Counts {
    /// Edges in `edges`.
    edges: usize,
    proposed: usize,
    proposed_contradictions: usize,
    /// Same-store edges whose target is gone (all of them, drawn or not).
    dangling: usize,
    /// Edges into another store.
    external: usize,
    /// Of those, edges whose store could not be checked.
    unknown: usize,
    /// Edges left out because an end is a station this response does not
    /// draw (over the limit, or outside the `line` filter).
    hidden: usize,
}

#[derive(Serialize)]
struct GraphPage {
    /// Stations the filters admit, before the limit.
    total_nodes: usize,
    shown_nodes: usize,
    truncated: bool,
    limit: usize,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    lines: Vec<Line>,
    /// Every line the status filters admit, before `line=` and the limit, so
    /// a line picker can offer them all; `count` is its stations there.
    all_lines: Vec<Line>,
    counts: Counts,
}

/// `(name → count)` as [`Line`]s in slot order, "no scope" last.
fn lines_of(counts: BTreeMap<String, usize>, slots: &HashMap<String, u32>) -> Vec<Line> {
    let mut lines: Vec<Line> = counts
        .into_iter()
        .map(|(name, count)| Line {
            slot: slots.get(&name).copied(),
            name,
            count,
        })
        .collect();
    lines.sort_by_key(|l| (l.slot.is_none(), l.slot, l.name.clone()));
    lines
}

fn flag(q: &HashMap<String, String>, name: &str, default: bool) -> Result<bool, ApiError> {
    match q.get(name).map(String::as_str) {
        None => Ok(default),
        Some("1" | "true" | "yes" | "on") => Ok(true),
        Some("0" | "false" | "no" | "off") => Ok(false),
        Some(other) => Err(ApiError::bad_request(format!(
            "{name} must be true or false, not {other:?}"
        ))),
    }
}

/// Probe the stores the external edges point into (module docs).
///
/// Each probe runs on the blocking pool — opening a store stats its
/// directory and reads the registry synchronously, and on a hung mount that
/// would stall a runtime worker where no timeout could fire — and all of
/// them run at once, each bounded by [`crate::projects::ROW_TIMEOUT`], so the
/// whole step takes one timeout at most, not one per store. A probe that
/// times out is `None` (its edges are `unknown`) and its blocking thread is
/// left to finish on its own, as `projects` does.
async fn foreign_states(edges: &[&GraphEdge]) -> HashMap<String, Option<HashSet<String>>> {
    let mut by_store: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in edges {
        if let Some(s) = &e.dst_store {
            by_store.entry(s.clone()).or_default().push(e.dst.clone());
        }
    }
    let mut out = HashMap::new();
    let mut probes = tokio::task::JoinSet::new();
    let runtime = tokio::runtime::Handle::current();
    for (i, (sid, ids)) in by_store.into_iter().enumerate() {
        if i >= MAX_FOREIGN_STORES {
            out.insert(sid, None);
            continue;
        }
        let rt = runtime.clone();
        probes.spawn(async move {
            let probe = tokio::task::spawn_blocking(move || {
                rt.block_on(async {
                    let other = engramdb::ops::links::open_store_by_id(&sid, true)
                        .await
                        .ok()?;
                    other.batch_exists(&ids).await.ok()
                })
                .map(|found| (sid.clone(), found))
                .ok_or(sid)
            });
            tokio::time::timeout(crate::projects::ROW_TIMEOUT, probe).await
        });
    }
    while let Some(done) = probes.join_next().await {
        match done {
            Ok(Ok(Ok(Ok((sid, found))))) => {
                out.insert(sid, Some(found));
            }
            Ok(Ok(Ok(Err(sid)))) => {
                out.insert(sid, None);
            }
            // Timed out or panicked: the store stays out of `out`, and the
            // caller reads a missing entry as "could not check" (`unknown`).
            _ => {}
        }
    }
    out
}

/// Which stations a capped response keeps: the most connected, then the
/// most read, then the most critical, then by id (a total order, so the same
/// store gives the same map).
fn rank(rows: &mut [(&IndexFilterable, usize, u64)]) {
    rows.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then(b.2.cmp(&a.2))
            .then(b.0.criticality.total_cmp(&a.0.criticality))
            .then_with(|| a.0.id.cmp(&b.0.id))
    });
}

pub(crate) async fn project_graph(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    let q = query(raw);
    let kinds = q
        .get("kinds")
        .map(|k| {
            k.split(',')
                .map(|s| {
                    LinkKind::parse_known(s).ok_or_else(|| {
                        ApiError::bad_request(format!(
                            "unknown edge kind {s:?} (known: {})",
                            LinkKind::known_list()
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let filter = GraphFilter {
        kinds,
        // The review queue is the point of the map: proposals are shown
        // unless asked not to be.
        include_proposed: flag(&q, "include_proposed", true)?,
        include_rejected: flag(&q, "include_rejected", false)?,
        include_invalidated: flag(&q, "include_invalidated", false)?,
    };
    let lines_wanted: Option<Vec<&str>> =
        q.get("line").map(|l| l.split(',').map(str::trim).collect());
    let limit: usize = match q.get("limit") {
        Some(l) => l
            .parse::<usize>()
            .map_err(|_| ApiError::bad_request("limit must be a number"))?
            .clamp(1, MAX_LIMIT),
        None => DEFAULT_LIMIT,
    };

    let (_, store) = open(&state, &id).await?;
    let all = store.list().await?;
    let graph = store.graph(&filter).await?;
    let usage = store.usage_all().await?;
    let slots = line_slots(&all);
    let rows: HashMap<&str, &IndexFilterable> = all.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut all_lines: BTreeMap<String, usize> = BTreeMap::new();
    for n in &graph.nodes {
        if let Some(e) = rows.get(n.id.as_str()) {
            *all_lines
                .entry(line_of(&e.logical).unwrap_or_default())
                .or_insert(0) += 1;
        }
    }
    // Stations the filters admit.
    let admitted: Vec<&IndexFilterable> = graph
        .nodes
        .iter()
        .filter_map(|n| rows.get(n.id.as_str()).copied())
        .filter(|e| {
            lines_wanted.as_ref().is_none_or(|ls| {
                let l = line_of(&e.logical).unwrap_or_else(|| "-".to_string());
                ls.contains(&l.as_str())
            })
        })
        .collect();
    let admitted_ids: HashSet<&str> = admitted.iter().map(|e| e.id.as_str()).collect();
    let mut degree: HashMap<&str, usize> = HashMap::new();
    for e in graph.edges.iter().chain(graph.dangling_edges.iter()) {
        if admitted_ids.contains(e.src.as_str()) {
            *degree.entry(e.src.as_str()).or_insert(0) += 1;
        }
        if e.dst_store.is_none() && admitted_ids.contains(e.dst.as_str()) {
            *degree.entry(e.dst.as_str()).or_insert(0) += 1;
        }
    }
    let reads = |id: &str| -> u64 {
        usage
            .by_memory
            .get(id)
            .map(|m| m.counts.returned + m.counts.injected + m.counts.fetched)
            .unwrap_or(0)
    };
    let mut ranked: Vec<(&IndexFilterable, usize, u64)> = admitted
        .iter()
        .map(|e| {
            (
                *e,
                degree.get(e.id.as_str()).copied().unwrap_or(0),
                reads(&e.id),
            )
        })
        .collect();
    rank(&mut ranked);
    let total_nodes = ranked.len();
    ranked.truncate(limit);
    let shown: HashSet<&str> = ranked.iter().map(|(e, _, _)| e.id.as_str()).collect();

    // Edges between shown stations, plus stubs (dangling, external) from one.
    let mut counts = Counts {
        dangling: graph.dangling,
        external: graph.external,
        ..Counts::default()
    };
    let drawable: Vec<&GraphEdge> = graph
        .edges
        .iter()
        .chain(graph.dangling_edges.iter())
        .filter(|e| {
            let keep = shown.contains(e.src.as_str())
                && (e.dst_store.is_some()
                    || shown.contains(e.dst.as_str())
                    || !rows.contains_key(e.dst.as_str()));
            if !keep {
                counts.hidden += 1;
            }
            keep
        })
        .collect();
    let foreign = foreign_states(&drawable).await;
    let now = Utc::now();
    let mut edges = Vec::with_capacity(drawable.len());
    let mut node_degree: HashMap<&str, (usize, usize)> = HashMap::new();
    for e in drawable {
        let state = match &e.dst_store {
            Some(s) => match foreign.get(s) {
                Some(Some(found)) if found.contains(&e.dst) => "live",
                Some(Some(_)) => "dangling",
                _ => {
                    counts.unknown += 1;
                    "unknown"
                }
            },
            None => match rows.get(e.dst.as_str()) {
                None => "dangling",
                Some(r) if r.invalidated_at.is_some_and(|t| t <= now) => "invalidated",
                Some(_) => "live",
            },
        };
        let proposed = e.status == LinkStatus::Proposed;
        if proposed {
            counts.proposed += 1;
            if e.kind == LinkKind::Contradicts {
                counts.proposed_contradictions += 1;
            }
        }
        for end in [
            Some(e.src.as_str()),
            (e.dst_store.is_none()).then_some(e.dst.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            if shown.contains(end) {
                let d = node_degree.entry(end).or_insert((0, 0));
                d.0 += 1;
                if proposed {
                    d.1 += 1;
                }
            }
        }
        edges.push(Edge {
            src: e.src.clone(),
            dst: e.dst.clone(),
            dst_store: e.dst_store.clone(),
            kind: e.kind.clone(),
            symmetric: e.symmetric,
            confidence: e.confidence,
            origin: e.origin.clone(),
            status: e.status.clone(),
            created_at: e.created_at,
            note: e.note.clone(),
            state,
        });
    }
    counts.edges = edges.len();

    let mut line_count: BTreeMap<String, usize> = BTreeMap::new();
    let nodes: Vec<Node> = ranked
        .iter()
        .map(|(e, _, _)| {
            let line = line_of(&e.logical);
            *line_count
                .entry(line.clone().unwrap_or_default())
                .or_insert(0) += 1;
            let u = usage.by_memory.get(&e.id).map(|m| &m.counts);
            let (degree, proposed) = node_degree.get(e.id.as_str()).copied().unwrap_or((0, 0));
            Node {
                id: e.id.clone(),
                summary: e.summary.clone(),
                type_: wire(&e.type_),
                epistemic: e.epistemic.as_str(),
                status: status_str(e.status),
                challenged: e.status == Status::Challenged,
                invalidated: e.invalidated_at.is_some_and(|t| t <= now),
                visibility: wire(&e.visibility).to_lowercase(),
                criticality: e.criticality,
                line_slot: line.as_ref().and_then(|l| slots.get(l).copied()),
                line,
                scope: e.logical.first().cloned(),
                usage: u
                    .map(|c| Reads {
                        returned: c.returned,
                        injected: c.injected,
                        fetched: c.fetched,
                    })
                    .unwrap_or_default(),
                degree,
                proposed,
            }
        })
        .collect();
    // In slot order (the store-wide rank), "no scope" last.
    let lines = lines_of(line_count, &slots);
    let all_lines = lines_of(all_lines, &slots);

    ok(&GraphPage {
        total_nodes,
        shown_nodes: nodes.len(),
        truncated: nodes.len() < total_nodes,
        limit,
        nodes,
        edges,
        lines,
        all_lines,
        counts,
    })
}
