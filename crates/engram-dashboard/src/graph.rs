//! `GET /api/projects/{id}/graph`: the graph view's memories and relations.
//!
//! Built on the phase-3 read API ([`engramdb::ops::links::load_graph`]) over
//! the read-only store, joined with the memories projection for each node's
//! top scope and with the usage rollup for its reads. Like every other endpoint it
//! is a pure read: a store without an `edges` table answers with no edges and
//! nothing is created.
//!
//! **Bounded.** A store can hold thousands of memories; the page lays them
//! out on the server (`layout`) and draws them on a canvas or in SVG,
//! which stays responsive to a couple of thousand memories. Past `limit` (default [`DEFAULT_LIMIT`], at
//! most [`MAX_LIMIT`]) the memories most worth drawing are kept — the most
//! connected first, then the most read, then the most critical — and the
//! response says how many it left out (`total_nodes` vs `shown_nodes`), so a
//! partial graph never passes for the whole store. `top_scope=` narrows to
//! some top-level scopes instead (the same parameter as `/memories`).
//!
//! **Totals before the limit.** `scopes` (memories per top scope and per
//! sub-scope) and `scope_edges` (relations per scope pair, by kind and
//! status) are counted over *every* memory the status filters admit, before
//! `top_scope=` and before the limit, so the page's scope overview of a
//! large store is never a picture of the truncated part. With `top_scope=`,
//! a relation leaving the opened scope (one end in it, the other a memory in
//! another top scope) is not dropped silently either: it is counted into
//! `stubs`, one per (drawn memory, far top scope), and into `stub_scopes`,
//! one per far top scope, over every memory of the opened scope (drawn or
//! not, so a tag's total is the scope's, not the drawn part's). A dangling
//! relation (its far end deleted) or one into another store has no far top
//! scope, so neither total counts it: it is drawn as a stub off its memory
//! as before, and counted in `counts.dangling` / `counts.external`.
//!
//! **Summary only.** `summary_over=N` answers a store with more than N
//! admitted memories with the totals alone (`summary: true`, no nodes, no
//! edges, no usage read and no far store probed): what the scope overview
//! draws, without paying for the graph it does not draw. At or under N the
//! answer is the ordinary graph, so one request both decides and serves.
//!
//! **Layout.** `layout=` (with `layout_aspect=` and `layout_width=`, as the
//! page rounds them; see [`crate::layout::LayoutRequest`]) adds `layout`:
//! the layout of exactly this response, computed here ([`crate::layout`]),
//! which the page draws as sent (it has no layout code of its own). A
//! `summary` response carries the scope overview when `layout=` names
//! `scope-overview`; a graph carries scope-force grouped by sub-scope when
//! `layout=` names `opened`, `top_scope=` names one scope and more than
//! `layout_open_over=N` memories pass the status filters (the page's opened
//! scope, with `dirs` for its tags, from the overview of the same box),
//! else the style's own by top scope when `layout=` names `scope-force` or
//! `grid`; anything else carries none. The response always waits for its layout:
//! computed on the blocking pool, once per input at a time, at most
//! `layout::cache::MAX_COMPUTING` at once with `MAX_QUEUED` more waiting
//! (a request for a new layout past that is a 503 `layout_busy` with
//! `Retry-After`), and cached by the input's digest (in memory and under
//! the OS cache dir, never in a store: `layout::cache`). A layout that
//! fails is a 500 `layout_failed`, never a response without one.
//!
//! **Far ends.** A same-store edge whose target is gone is returned with
//! `state: "dangling"` (drawn as a stub to a deleted-memory mark); one into
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

use crate::api::{color_slots, ok, open, query, status_str, top_scope_of, wire, ApiResult};
use crate::layout::{
    cache::Flight, overview::OverviewInput, Cached, EdgeFacts, ForceInput, Groups, LayoutInput,
    LayoutRequest, NodeFacts,
};
use crate::{ApiError, AppState};

/// Memories drawn when `limit` is not given.
pub const DEFAULT_LIMIT: usize = 800;
/// The most memories one response carries.
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
    top_scope: Option<String>,
    /// See [`crate::api::color_slots`].
    #[serde(skip_serializing_if = "Option::is_none")]
    color_slot: Option<u32>,
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
struct Scope {
    /// `""` is "no logical scope".
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<u32>,
    /// Memories in the scope the status filters admit, before `top_scope=`
    /// and the limit (module docs).
    count: usize,
    /// Of those, the memories in this response.
    shown: usize,
    /// Its sub-scopes (the first two segments of a memory's first logical
    /// scope, `storage.lock`), by name; the top scope's own memories are in
    /// `count` only.
    subs: Vec<SubScope>,
}

#[derive(Serialize)]
struct SubScope {
    name: String,
    count: usize,
}

/// Relations counted together: in total, by kind and status (wire names,
/// so a newer binary's kind is counted rather than dropped), and the two
/// totals the overview marks. A display filter (kinds, proposed, rejected)
/// recomputes from `kinds`.
#[derive(Serialize, Default)]
pub(crate) struct Tally {
    pub(crate) count: usize,
    /// kind → status → relations.
    pub(crate) kinds: BTreeMap<String, BTreeMap<String, usize>>,
    /// `contradicts` relations not rejected.
    pub(crate) contradictions: usize,
    /// `proposed` relations of any kind.
    pub(crate) proposed: usize,
}

impl Tally {
    pub(crate) fn add(&mut self, e: &GraphEdge) {
        self.count += 1;
        *self
            .kinds
            .entry(e.kind.as_str().to_string())
            .or_default()
            .entry(e.status.as_str().to_string())
            .or_insert(0) += 1;
        if e.kind == LinkKind::Contradicts && e.status != LinkStatus::Rejected {
            self.contradictions += 1;
        }
        if e.status == LinkStatus::Proposed {
            self.proposed += 1;
        }
    }
}

/// The relations between two top scopes (`""`: no logical scope). A
/// symmetric kind is counted once per unordered pair (`a` ≤ `b`,
/// `directed: false`); a directed one per ordered pair (`a` the source's
/// scope). `a == b` counts the relations inside one scope.
#[derive(Serialize)]
struct ScopeEdge {
    a: String,
    b: String,
    directed: bool,
    #[serde(flatten)]
    tally: Tally,
}

/// The relations between one drawn memory of the opened scope and the
/// memories of another top scope (`top_scope=` only).
#[derive(Serialize)]
struct Stub {
    node: String,
    scope: String,
    #[serde(flatten)]
    tally: Tally,
}

/// Every relation between the opened scope and another top scope.
#[derive(Serialize)]
struct StubScope {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<u32>,
    #[serde(flatten)]
    tally: Tally,
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
    /// Edges left out because an end is a memory this response does not
    /// draw (over the limit, or outside the `top_scope` filter), while the
    /// other is one the filters admit; neither a relation with both ends
    /// outside `top_scope=` nor one counted in `stub_scopes` (drawn end or not).
    hidden: usize,
}

#[derive(Serialize)]
struct GraphPage {
    /// Totals only (`summary_over=`): no `nodes`, `edges` or stubs, and no
    /// far store was probed.
    summary: bool,
    /// Memories the filters admit, before the limit.
    total_nodes: usize,
    shown_nodes: usize,
    truncated: bool,
    limit: usize,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    /// Every top scope the status filters admit, before `top_scope=` and the
    /// limit (module docs), so a scope picker can offer them all and the
    /// overview is never truncated.
    scopes: Vec<Scope>,
    /// Relations between top scopes, over the same memories as `scopes`.
    scope_edges: Vec<ScopeEdge>,
    /// `top_scope=` only: relations leaving the opened scope, per drawn
    /// memory and far top scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    stubs: Option<Vec<Stub>>,
    /// `top_scope=` only: the same, per far top scope, over the whole
    /// opened scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    stub_scopes: Option<Vec<StubScope>>,
    counts: Counts,
}

/// The page as JSON with `layout` (the encoded layout's text, module docs)
/// spliced in as its last field: the layout is kept as text so its doubles
/// reach the page bit for bit (`layout::Cached`).
fn respond(page: &GraphPage, layout: Option<crate::layout::cache::Encoded>) -> ApiResult {
    let Some(layout) = layout else {
        return ok(page);
    };
    let mut bytes = serde_json::to_vec(page).map_err(|e| ApiError::internal(e.to_string()))?;
    debug_assert_eq!(bytes.last(), Some(&b'}'));
    bytes.pop();
    bytes.extend_from_slice(b",\"layout\":");
    bytes.extend_from_slice(layout.as_bytes());
    bytes.push(b'}');
    let mut r = axum::response::IntoResponse::into_response((axum::http::StatusCode::OK, bytes));
    let h = r.headers_mut();
    h.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    h.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(r)
}

/// scope-force's input for `nodes` / `edges`, as `graph/input.js` builds
/// it from this response (`crate::layout` module docs).
fn force_input(nodes: &[Node], edges: &[Edge], req: &LayoutRequest, groups: Groups) -> ForceInput {
    let facts: Vec<NodeFacts<'_>> = nodes
        .iter()
        .map(|n| NodeFacts {
            id: &n.id,
            top_scope: n.top_scope.as_deref(),
            scope: n.scope.as_deref(),
            summary: &n.summary,
            reads: n.usage.returned + n.usage.injected + n.usage.fetched,
            degree: n.degree,
            slot: n.color_slot,
        })
        .collect();
    let ends: Vec<EdgeFacts<'_>> = edges
        .iter()
        .map(|e| EdgeFacts {
            src: &e.src,
            dst: &e.dst,
            cross_store: e.dst_store.is_some(),
        })
        .collect();
    crate::layout::build_force_input(&facts, &ends, groups, req.aspect, req.width)
}

/// The overview's input for a summary's scopes, as
/// `graph/input.js::overviewInput` builds it.
fn overview_input(
    scopes: &[Scope],
    scope_edges: &[ScopeEdge],
    req: &LayoutRequest,
) -> OverviewInput {
    let scopes: Vec<(&str, usize)> = scopes.iter().map(|l| (l.name.as_str(), l.count)).collect();
    let rows: Vec<(&str, &str, usize)> = scope_edges
        .iter()
        .map(|r| (r.a.as_str(), r.b.as_str(), r.tally.count))
        .collect();
    crate::layout::build_overview_input(&scopes, &rows, req.aspect, req.width)
}

/// A layout that could not be computed (module docs): the page shows it.
fn layout_failed(why: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        "layout_failed",
        format!("the graph layout failed: {why}"),
    )
}

/// What the cache holds for `input` (digest `key`): from memory, else
/// computed (or read from disk) on the blocking pool once per key at a time,
/// waiting for a computing slot behind at most `MAX_QUEUED` others (module
/// docs). The computation runs in its own task, so a request that gives up
/// still leaves its layout cached.
async fn cached_layout(
    state: &Arc<AppState>,
    input: &Arc<LayoutInput>,
    key: &str,
) -> Result<Arc<Cached>, ApiError> {
    if let Some(v) = state.layouts.get_mem(key) {
        return Ok(v);
    }
    let mut rx = match state.layouts.begin(key) {
        Flight::Full => {
            return Err(ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "layout_busy",
                "the dashboard is laying out other graphs; try again in a moment",
            ))
        }
        Flight::Wait(rx) => rx,
        Flight::Lead(lead) => {
            let rx = lead.subscribe();
            let (st, inp, k) = (Arc::clone(state), Arc::clone(input), key.to_string());
            tokio::spawn(async move {
                let permit = lead.permit().await;
                let st2 = Arc::clone(&st);
                let done = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    st2.layouts.fill(&k, || inp.run(&k))
                })
                .await;
                // A panic drops the lead unlanded: its waiters see the
                // channel close.
                if let Ok(r) = done {
                    st.layouts.land(lead, r.map_err(Arc::from));
                }
            });
            rx
        }
    };
    let outcome = match rx.wait_for(Option::is_some).await {
        Ok(v) => v.clone(),
        Err(_) => None,
    };
    match outcome {
        Some(Ok(v)) => Ok(v),
        Some(Err(why)) => Err(layout_failed(why)),
        None => Err(layout_failed("the layout computation stopped")),
    }
}

/// The encoded layout for `input` ([`cached_layout`]), finished for the
/// request's width on the blocking pool (the scope names' placement grows
/// with their length, and they come from memory files), with an opened
/// scope's `dirs`.
async fn layout_of(
    state: &Arc<AppState>,
    input: LayoutInput,
    groups: Groups,
    dirs: Option<serde_json::Value>,
) -> Result<crate::layout::cache::Encoded, ApiError> {
    let input = Arc::new(input);
    let key = input.digest();
    let cached = cached_layout(state, &input, &key).await?;
    tokio::task::spawn_blocking(move || input.render(&cached, &key, groups, dirs.as_ref()))
        .await
        .map_err(layout_failed)?
        .map_err(layout_failed)
}

/// A memory's sub-scope: the first two segments of its first logical scope,
/// when it has two.
pub(crate) fn sub_scope_of(logical: &[String]) -> Option<String> {
    let first = logical.first()?;
    let mut parts = first.splitn(3, '.');
    let top = parts.next().filter(|s| !s.is_empty())?;
    let sub = parts.next().filter(|s| !s.is_empty())?;
    Some(format!("{top}.{sub}"))
}

/// Scope totals as [`Scope`]s in slot order (the store-wide rank), "no
/// scope" last.
fn scopes_of(
    totals: BTreeMap<String, (usize, BTreeMap<String, usize>)>,
    shown: &HashMap<String, usize>,
    slots: &HashMap<String, u32>,
) -> Vec<Scope> {
    let mut scopes: Vec<Scope> = totals
        .into_iter()
        .map(|(name, (count, subs))| Scope {
            slot: slots.get(&name).copied(),
            shown: shown.get(&name).copied().unwrap_or(0),
            subs: subs
                .into_iter()
                .map(|(name, count)| SubScope { name, count })
                .collect(),
            name,
            count,
        })
        .collect();
    scopes.sort_by_key(|l| (l.slot.is_none(), l.slot, l.name.clone()));
    scopes
}

pub(crate) fn flag(
    q: &HashMap<String, String>,
    name: &str,
    default: bool,
) -> Result<bool, ApiError> {
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
pub(crate) async fn foreign_states(
    edges: &[&GraphEdge],
) -> HashMap<String, Option<HashSet<String>>> {
    let mut by_store: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in edges {
        if let Some(s) = &e.dst_store {
            by_store.entry(s.clone()).or_default().push(e.dst.clone());
        }
    }
    probe_stores(by_store).await
}

/// Which of `ids` exist in each far store (`None`: it would not open).
///
/// Each probe runs off the runtime workers through
/// [`crate::projects::off_runtime`] — opening a store stats its directory
/// and reads the registry synchronously, and on a hung mount that would
/// stall a runtime worker where no timeout could fire — keyed by the store,
/// so a hung one has one probe in flight at most and is not retried for
/// [`crate::projects::PROBE_RETRY_AFTER`] after it timed out. They all run
/// at once, each bounded by [`crate::projects::ROW_TIMEOUT`], so the whole
/// step takes one timeout at most, not one per store; at most
/// [`MAX_FOREIGN_STORES`] are probed. A store that was not probed, timed out
/// or panicked stays out of the map, and the caller reads a missing entry as
/// "could not check" (`unknown`), never as dangling.
pub(crate) async fn probe_stores(
    by_store: BTreeMap<String, Vec<String>>,
) -> HashMap<String, Option<HashSet<String>>> {
    let mut out = HashMap::new();
    let mut probes = tokio::task::JoinSet::new();
    for (i, (sid, ids)) in by_store.into_iter().enumerate() {
        if i >= MAX_FOREIGN_STORES {
            out.insert(sid, None);
            continue;
        }
        probes.spawn(async move {
            let key = format!("store:{sid}");
            let probe = crate::projects::off_runtime(key, crate::projects::ROW_TIMEOUT, {
                let sid = sid.clone();
                async move {
                    let other = engramdb::ops::links::open_store_by_id(&sid, true)
                        .await
                        .ok()?;
                    other.batch_exists(&ids).await.ok()
                }
            })
            .await;
            (sid, probe)
        });
    }
    while let Some(done) = probes.join_next().await {
        if let Ok((sid, Ok(found))) = done {
            out.insert(sid, found);
        }
    }
    out
}

/// Which memories a capped response keeps: the most connected, then the
/// most read, then the most critical, then by id (a total order, so the same
/// store gives the same graph).
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
                            "unknown relation kind {s:?} (known: {})",
                            LinkKind::known_list()
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let filter = GraphFilter {
        kinds,
        // The review queue is the point of the graph: proposals are shown
        // unless asked not to be.
        include_proposed: flag(&q, "include_proposed", true)?,
        include_rejected: flag(&q, "include_rejected", false)?,
        include_invalidated: flag(&q, "include_invalidated", false)?,
    };
    let scopes_wanted: Option<Vec<&str>> = q
        .get("top_scope")
        .map(|l| l.split(',').map(str::trim).collect());
    let limit: usize = match q.get("limit") {
        Some(l) => l
            .parse::<usize>()
            .map_err(|_| ApiError::bad_request("limit must be a number"))?
            .clamp(1, MAX_LIMIT),
        None => DEFAULT_LIMIT,
    };

    // `summary_over=N`: when more than N memories pass the filters, answer
    // with the totals only (module docs).
    let layout_req = LayoutRequest::parse(&q)?;
    let summary_over: Option<usize> = q
        .get("summary_over")
        .map(|n| {
            n.parse::<usize>()
                .map_err(|_| ApiError::bad_request("summary_over must be a number"))
        })
        .transpose()?;

    let (_, store) = open(&state, &id).await?;
    let all = store.list().await?;
    let graph = store.graph_of(&all, &filter).await?;
    let slots = color_slots(&all);
    let rows: HashMap<&str, &IndexFilterable> = all.iter().map(|e| (e.id.as_str(), e)).collect();

    // Every memory the status filters admit, with its top scope (`""`:
    // none): what `scopes` and `scope_edges` count, before `top_scope=` and
    // the limit.
    let mut top_of: HashMap<&str, String> = HashMap::with_capacity(graph.nodes.len());
    let mut totals: BTreeMap<String, (usize, BTreeMap<String, usize>)> = BTreeMap::new();
    for n in &graph.nodes {
        if let Some(e) = rows.get(n.id.as_str()) {
            let top = top_scope_of(&e.logical).unwrap_or_default();
            let t = totals.entry(top.clone()).or_default();
            t.0 += 1;
            if let Some(sub) = sub_scope_of(&e.logical) {
                *t.1.entry(sub).or_insert(0) += 1;
            }
            top_of.insert(e.id.as_str(), top);
        }
    }
    // The page's `storeTotal`: what decides an opened scope.
    let store_total: usize = totals.values().map(|t| t.0).sum();
    // Same-store relations between two of them (a dangling or cross-store
    // one has no far scope to count it under).
    let scope_pair = |e: &GraphEdge| -> Option<(&str, &str)> {
        if e.dst_store.is_some() {
            return None;
        }
        Some((
            top_of.get(e.src.as_str())?.as_str(),
            top_of.get(e.dst.as_str())?.as_str(),
        ))
    };
    let mut pairs: BTreeMap<(String, String, bool), Tally> = BTreeMap::new();
    for e in &graph.edges {
        let Some((a, b)) = scope_pair(e) else {
            continue;
        };
        let (a, b) = if e.symmetric && b < a { (b, a) } else { (a, b) };
        pairs
            .entry((a.to_string(), b.to_string(), !e.symmetric))
            .or_default()
            .add(e);
    }
    let scope_edges: Vec<ScopeEdge> = pairs
        .into_iter()
        .map(|((a, b, directed), tally)| ScopeEdge {
            a,
            b,
            directed,
            tally,
        })
        .collect();
    // Memories the filters admit.
    let admitted: Vec<&IndexFilterable> = graph
        .nodes
        .iter()
        .filter_map(|n| rows.get(n.id.as_str()).copied())
        .filter(|e| {
            scopes_wanted.as_ref().is_none_or(|ls| {
                let l = top_scope_of(&e.logical).unwrap_or_else(|| "-".to_string());
                ls.contains(&l.as_str())
            })
        })
        .collect();
    if summary_over.is_some_and(|n| admitted.len() > n) {
        let scopes = scopes_of(totals, &HashMap::new(), &slots);
        let layout = match layout_req {
            Some(req) if req.overview => {
                let input = overview_input(&scopes, &scope_edges, &req);
                Some(layout_of(&state, LayoutInput::Overview(input), Groups::Top, None).await?)
            }
            _ => None,
        };
        let page = GraphPage {
            summary: true,
            total_nodes: admitted.len(),
            shown_nodes: 0,
            truncated: true,
            limit,
            nodes: Vec::new(),
            edges: Vec::new(),
            scopes,
            scope_edges,
            stubs: None,
            stub_scopes: None,
            counts: Counts {
                dangling: graph.dangling,
                external: graph.external,
                ..Counts::default()
            },
        };
        return respond(&page, layout);
    }
    let usage = store.usage_all().await?;
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

    // An opened scope's relations to other top scopes (module docs).
    let (stubs, stub_scopes) = match &scopes_wanted {
        None => (None, None),
        Some(_) => {
            let mut per_node: BTreeMap<(&str, &str), Tally> = BTreeMap::new();
            let mut per_scope: BTreeMap<&str, Tally> = BTreeMap::new();
            for e in &graph.edges {
                let Some((a, b)) = scope_pair(e) else {
                    continue;
                };
                let src_in = admitted_ids.contains(e.src.as_str());
                let dst_in = admitted_ids.contains(e.dst.as_str());
                let (inside, far) = match (src_in, dst_in) {
                    (true, false) => (e.src.as_str(), b),
                    (false, true) => (e.dst.as_str(), a),
                    _ => continue,
                };
                per_scope.entry(far).or_default().add(e);
                if shown.contains(inside) {
                    per_node.entry((inside, far)).or_default().add(e);
                }
            }
            (
                Some(
                    per_node
                        .into_iter()
                        .map(|((node, scope), tally)| Stub {
                            node: node.to_string(),
                            scope: scope.to_string(),
                            tally,
                        })
                        .collect::<Vec<_>>(),
                ),
                Some(
                    per_scope
                        .into_iter()
                        .map(|(name, tally)| StubScope {
                            slot: slots.get(name).copied(),
                            name: name.to_string(),
                            tally,
                        })
                        .collect::<Vec<_>>(),
                ),
            )
        }
    };

    // Whether a relation left out of `edges` counts as `hidden`: one end is
    // a memory this view admits, and it is not already counted in an opened
    // scope's `stub_scopes` (a relation with both ends outside the view is
    // not this view's).
    let touches_view = |e: &GraphEdge| -> bool {
        let src_in = admitted_ids.contains(e.src.as_str());
        let dst_in = e.dst_store.is_none() && admitted_ids.contains(e.dst.as_str());
        if !src_in && !dst_in {
            return false;
        }
        // Counted in `stub_scopes` (drawn end or not), so not hidden too.
        let stubbed = scopes_wanted.is_some() && src_in != dst_in && scope_pair(e).is_some();
        !stubbed
    };
    // Edges between shown memories, plus stubs (dangling, external) from one.
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
            if !keep && touches_view(e) {
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

    let mut scope_count: HashMap<String, usize> = HashMap::new();
    let nodes: Vec<Node> = ranked
        .iter()
        .map(|(e, _, _)| {
            let top_scope = top_scope_of(&e.logical);
            *scope_count
                .entry(top_scope.clone().unwrap_or_default())
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
                color_slot: top_scope.as_ref().and_then(|l| slots.get(l).copied()),
                top_scope,
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
    let scopes = scopes_of(totals, &scope_count, &slots);
    // An opened scope (the page's `opened`) groups by sub-scope; else the
    // style's own scope-force, by top scope (module docs).
    let opened = layout_req.is_some_and(|req| {
        req.open_over.is_some_and(|n| store_total > n)
            && scopes_wanted.as_ref().is_some_and(|ls| ls.len() == 1)
    });
    let groups = match layout_req {
        Some(_) if opened => Some(Groups::Sub),
        Some(req) if req.force => Some(Groups::Top),
        _ => None,
    };
    let layout = match (layout_req, groups) {
        (Some(req), Some(groups)) if !nodes.is_empty() => {
            // An opened scope's tags point where each far scope lies on the
            // overview of the same box (the same cached overview).
            let dirs = match (&stub_scopes, &scopes_wanted) {
                (Some(far), Some(ls)) if groups == Groups::Sub && !far.is_empty() => {
                    let input = Arc::new(LayoutInput::Overview(overview_input(
                        &scopes,
                        &scope_edges,
                        &req,
                    )));
                    let key = input.digest();
                    match &*cached_layout(&state, &input, &key).await? {
                        Cached::Overview(text) => {
                            let me = if ls[0] == "-" { "" } else { ls[0] };
                            let names: Vec<&str> = far.iter().map(|f| f.name.as_str()).collect();
                            crate::layout::overview_dirs(text, me, &names)
                        }
                        Cached::Force(_) | Cached::Grid(_) => None,
                    }
                }
                _ => None,
            };
            let input = force_input(&nodes, &edges, &req, groups);
            Some(layout_of(&state, LayoutInput::Force(input), groups, dirs).await?)
        }
        // Grid's own layout, by top scope (never an opened scope's).
        (Some(req), None) if req.grid && !nodes.is_empty() => {
            let input = force_input(&nodes, &edges, &req, Groups::Top);
            Some(layout_of(&state, LayoutInput::Grid(input), Groups::Top, None).await?)
        }
        _ => None,
    };

    let page = GraphPage {
        summary: false,
        total_nodes,
        shown_nodes: nodes.len(),
        truncated: nodes.len() < total_nodes,
        limit,
        nodes,
        edges,
        scopes,
        scope_edges,
        stubs,
        stub_scopes,
        counts,
    };
    respond(&page, layout)
}
