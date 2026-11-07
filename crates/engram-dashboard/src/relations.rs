//! `GET /api/projects/{id}/relations`: every relation of a store as a
//! filterable, paged list (the Relations tab, which doubles as the review
//! queue), plus a scope × scope matrix of the same set.
//!
//! Built on the same read as `/graph` ([`engramdb::ops::links::load_graph`]
//! over the read-only store, deduplicated: one row per logical relation, a
//! symmetric one seen once), so it is a pure read and creates nothing; a
//! store without an `edges` table lists only the relations its files mirror
//! in (`supersedes`).
//!
//! **Filters.** `kind=` (repeatable, or a comma list), `status=` (repeatable
//! or a comma list of `proposed`, `confirmed`, `rejected`, or `all`; by
//! default every status but `rejected`, which `include_rejected=true` adds —
//! the `/graph` default, and a status a newer binary wrote is in the default
//! set rather than silently dropped), `q=` (case-insensitive text over both
//! ends' summaries, the note and the ids), `include_invalidated=` (relations
//! touching an invalidated memory). The **scope pair** is `from_scope=` /
//! `to_scope=`, exact scope names (`-` is "no logical scope"): a directed
//! kind matches source in `from_scope` and target in `to_scope`, a symmetric
//! kind either order; either one alone constrains just that end (a symmetric
//! kind: either end). `touches=` keeps relations with either end in a scope.
//! `subs=1` makes every scope name a sub-scope (`storage.lock`, the first two
//! segments of a memory's first logical scope; a memory with one segment
//! stays in its top scope), for the matrix and the pair filter alike. A
//! relation whose far end is deleted or in another store has no far scope: a
//! filter on that end does not match it.
//!
//! **Order and paging.** Proposed first, then contradictions, then newest
//! `at`, then the relation's id — a total order (a same-store symmetric
//! relation's id is built from its sorted endpoints, and the edge read keeps
//! the same one of its two copies every time, so neither its id nor its
//! place moves between reads). Keyset paging as on `/memories`:
//! `next_cursor` is the last row's position in that order, and the next page
//! starts strictly after it, so a relation reviewed or created between two
//! pages never makes a later page repeat or skip one that *stayed*. The
//! relation that changed is the declared exception: a review stamps `at` and
//! moves it out of "proposed", so it can be listed again among the confirmed
//! ones, and one created ahead of the cursor is not listed until the walk
//! starts over (the page reloads its list after a review for that reason).
//! The cursor carries a digest of the filters it was made under: passed with
//! different ones it is a 400, never a page of another list. `limit=` is 100
//! by default, at most 500; `q=` at most [`MAX_QUERY_CHARS`] characters;
//! `next_cursor` is `null` on the last page.
//!
//! **The matrix** (first page only, i.e. without `cursor`) counts the
//! relations the kind, status, text and invalidated filters admit, *ignoring*
//! the scope filters, so picking a cell narrows the list but never the
//! matrix. A directed relation is counted in its (source scope, target
//! scope) cell; a symmetric one in both (a, b) and (b, a) — once when a = b —
//! so every cell's count is exactly the list a click on it shows (cells
//! therefore do not sum to the total). Relations without a far scope are not
//! in it; `matrix.unplaced` counts them.
//!
//! **Far ends** are resolved as `/graph` does — `live`, `invalidated`,
//! `dangling`, or for a relation into another store `live`/`dangling` when
//! that store answered and `unknown` when it did not — but only for the rows
//! on the page, so a page costs at most one probe per foreign store on it.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use chrono::{DateTime, Utc};
use engramdb::ops::links::{GraphEdge, GraphFilter, LinkKind, LinkOrigin, LinkStatus};
use engramdb::storage::lance_index::IndexFilterable;
use serde::{Deserialize, Serialize};

use crate::api::{color_slots, hex_json, ok, open, query, top_scope_of, unhex_json, ApiResult};
use crate::graph::{flag, foreign_states, sub_scope_of, Tally};
use crate::{ApiError, AppState};

/// Rows per page when `limit` is not given.
pub const DEFAULT_LIMIT: usize = 100;
/// The most rows one page carries.
pub const MAX_LIMIT: usize = 500;
/// The longest `q=` accepted, in characters.
pub const MAX_QUERY_CHARS: usize = 256;

/// Every value of `key` in the query string, repeated or comma-separated.
fn values(raw: Option<&str>, key: &str) -> Vec<String> {
    raw.map(|q| {
        form_urlencoded::parse(q.as_bytes())
            .filter(|(k, _)| k == key)
            .flat_map(|(_, v)| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
            .collect()
    })
    .unwrap_or_default()
}

/// A scope name from the query: `-` is "no logical scope" (`""`).
fn scope_param(q: &HashMap<String, String>, key: &str) -> Option<String> {
    q.get(key)
        .map(|s| if s == "-" { String::new() } else { s.clone() })
}

/// The parsed filters. [`Self::canonical`] is what a cursor is bound to.
struct Filter {
    kinds: Option<Vec<LinkKind>>,
    /// `None`: every status but `rejected` (module docs).
    statuses: Option<Vec<LinkStatus>>,
    include_rejected: bool,
    include_invalidated: bool,
    from: Option<String>,
    to: Option<String>,
    touches: Option<String>,
    text: Option<String>,
    subs: bool,
}

impl Filter {
    fn parse(raw: Option<&str>, q: &HashMap<String, String>) -> Result<Self, ApiError> {
        let kinds = values(raw, "kind")
            .iter()
            .map(|s| {
                LinkKind::parse_known(s).ok_or_else(|| {
                    ApiError::bad_request(format!(
                        "unknown relation kind {s:?} (known: {})",
                        LinkKind::known_list()
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let status = values(raw, "status");
        let statuses = if status.iter().any(|s| s == "all") {
            Some(Vec::new()) // every status; see `admits`
        } else if status.is_empty() {
            None
        } else {
            Some(
                status
                    .iter()
                    .map(|s| {
                        LinkStatus::parse_known(s).ok_or_else(|| {
                            ApiError::bad_request(format!(
                                "unknown status {s:?} (known: {}, all)",
                                LinkStatus::known_list()
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )
        };
        Ok(Self {
            kinds: (!kinds.is_empty()).then_some(kinds),
            statuses,
            include_rejected: flag(q, "include_rejected", false)?,
            include_invalidated: flag(q, "include_invalidated", false)?,
            from: scope_param(q, "from_scope"),
            to: scope_param(q, "to_scope"),
            touches: scope_param(q, "touches"),
            text: match q.get("q") {
                Some(t) if t.chars().count() > MAX_QUERY_CHARS => {
                    return Err(ApiError::bad_request(format!(
                        "q is longer than {MAX_QUERY_CHARS} characters"
                    )))
                }
                t => t.map(|s| s.to_lowercase()),
            },
            subs: flag(q, "subs", false)?,
        })
    }

    fn admits_status(&self, s: &LinkStatus) -> bool {
        match &self.statuses {
            Some(v) if v.is_empty() => true,
            Some(v) => v.contains(s),
            None => *s != LinkStatus::Rejected || self.include_rejected,
        }
    }

    /// Whether `load_graph` must read rejected edges at all.
    fn wants_rejected(&self) -> bool {
        self.admits_status(&LinkStatus::Rejected)
    }

    /// Every filter, in one order, so two requests with the same filters
    /// spelled differently share cursors and two with different ones never do.
    fn canonical(&self) -> String {
        let mut kinds: Vec<&str> = self.kinds.iter().flatten().map(|k| k.as_str()).collect();
        kinds.sort_unstable();
        kinds.dedup();
        let statuses: Vec<&str> = [
            LinkStatus::Proposed,
            LinkStatus::Confirmed,
            LinkStatus::Rejected,
        ]
        .iter()
        .filter(|s| self.admits_status(s))
        .map(|s| s.as_str())
        .collect();
        let open_status = match &self.statuses {
            None => "default",
            Some(v) if v.is_empty() => "all",
            Some(_) => "listed",
        };
        serde_json::json!([
            kinds,
            statuses,
            open_status,
            self.include_invalidated,
            self.from,
            self.to,
            self.touches,
            self.text,
            self.subs
        ])
        .to_string()
    }
}

/// `next_cursor`: the last row's sort key and the filters it belongs to.
#[derive(Serialize, Deserialize)]
struct Cursor {
    /// [`digest`] of the filters.
    f: String,
    key: Key,
}

/// The list order (module docs): ascending on every field.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    /// 0 proposed, 1 anything else.
    p: u8,
    /// 0 contradicts, 1 anything else.
    c: u8,
    /// `at`, negated (newest first), in microseconds.
    t: i64,
    id: String,
}

/// One end of a relation, as the list shows it.
#[derive(Serialize)]
struct End {
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    /// Its first logical scope in full (`storage.lock`).
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_scope: Option<String>,
    /// See [`crate::api::color_slots`].
    #[serde(skip_serializing_if = "Option::is_none")]
    color_slot: Option<u32>,
    invalidated: bool,
    /// Its scope at the asked level (`subs=1`: the sub-scope), as the
    /// matrix and the pair filter name it, so a page can orient a
    /// symmetric relation to a picked cell.
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
}

#[derive(Serialize)]
struct Row {
    /// `src|kind|dst|dst_store`: unique per relation, stable across reads.
    id: String,
    src: String,
    dst: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dst_store: Option<String>,
    kind: LinkKind,
    symmetric: bool,
    status: LinkStatus,
    origin: LinkOrigin,
    confidence: f64,
    at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// `live`, `invalidated`, `dangling` or `unknown` (module docs).
    state: &'static str,
    /// The two ends' summaries again, under the names the review controls
    /// read (`web/js/views/review.js`).
    #[serde(skip_serializing_if = "Option::is_none")]
    src_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dst_summary: Option<String>,
    from: End,
    to: End,
}

#[derive(Serialize)]
struct MatrixScope {
    /// `""` is "no logical scope".
    name: String,
    /// Its top scope (the name itself at the top level).
    top: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<u32>,
}

#[derive(Serialize)]
struct Cell {
    from: String,
    to: String,
    #[serde(flatten)]
    tally: Tally,
}

#[derive(Serialize)]
struct Matrix {
    /// `top` or `sub` (`subs=1`).
    level: &'static str,
    /// The scopes with at least one counted relation, in the store's colour
    /// order (sub-scopes grouped under their top scope).
    scopes: Vec<MatrixScope>,
    cells: Vec<Cell>,
    /// Relations the matrix filters admit, each once.
    relations: usize,
    /// Of those, the ones with no far scope (deleted or in another store).
    unplaced: usize,
}

#[derive(Serialize, Default)]
struct Counts {
    /// Of `total`: proposed ones, and `contradicts` ones not rejected.
    proposed: usize,
    contradictions: usize,
}

#[derive(Serialize)]
struct RelationsPage {
    /// Relations every filter admits.
    total: usize,
    limit: usize,
    items: Vec<Row>,
    /// Opaque; pass back as `cursor` with the same filters.
    next_cursor: Option<String>,
    counts: Counts,
    /// Without `cursor` only (module docs).
    #[serde(skip_serializing_if = "Option::is_none")]
    matrix: Option<Matrix>,
    /// Memories the invalidated filter admits: how big the store's graph is.
    memories: usize,
}

/// `a|kind|b|store`. A same-store symmetric relation is named by its
/// sorted endpoints (as `EdgeRow::edge_key` keys it), so the id does not
/// depend on which of its two files' copies the read kept.
fn relation_id(e: &GraphEdge) -> String {
    let (a, b) = if e.symmetric && e.dst_store.is_none() && e.dst < e.src {
        (&e.dst, &e.src)
    } else {
        (&e.src, &e.dst)
    };
    format!(
        "{a}|{}|{b}|{}",
        e.kind.as_str(),
        e.dst_store.as_deref().unwrap_or("")
    )
}

/// A short, stable digest of [`Filter::canonical`] for the cursor (FNV-1a,
/// 64 bits): a cursor carries this, not the filter text, so its length does
/// not grow with `q=` or the scope names.
fn digest(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn key_of(e: &GraphEdge) -> Key {
    Key {
        p: u8::from(e.status != LinkStatus::Proposed),
        c: u8::from(e.kind != LinkKind::Contradicts),
        t: -e.created_at.timestamp_micros(),
        id: relation_id(e),
    }
}

pub(crate) async fn project_relations(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    let q = query(raw.clone());
    let f = Filter::parse(raw.as_deref(), &q)?;
    let limit: usize = match q.get("limit") {
        Some(l) => l
            .parse::<usize>()
            .map_err(|_| ApiError::bad_request("limit must be a number"))?
            .clamp(1, MAX_LIMIT),
        None => DEFAULT_LIMIT,
    };
    let canonical = digest(&f.canonical());
    let after: Option<Key> = match q.get("cursor") {
        Some(c) => {
            let cur: Cursor = unhex_json(c)
                .ok_or_else(|| ApiError::bad_request("cursor must come from next_cursor"))?;
            if cur.f != canonical {
                return Err(ApiError::bad_request(
                    "cursor belongs to other filters; start again without it",
                ));
            }
            Some(cur.key)
        }
        None => None,
    };

    let (_, store) = open(&state, &id).await?;
    let all = store.list().await?;
    let graph = store
        .graph_of(
            &all,
            &GraphFilter {
                kinds: f.kinds.clone(),
                include_proposed: true,
                include_rejected: f.wants_rejected(),
                include_invalidated: f.include_invalidated,
            },
        )
        .await?;
    let slots = color_slots(&all);
    let rows: HashMap<&str, &IndexFilterable> = all.iter().map(|e| (e.id.as_str(), e)).collect();
    let now = Utc::now();
    // A memory's scope at the asked level (module docs).
    let group = |e: &IndexFilterable| -> String {
        let top = top_scope_of(&e.logical).unwrap_or_default();
        if f.subs {
            sub_scope_of(&e.logical).unwrap_or(top)
        } else {
            top
        }
    };
    let groups: HashMap<&str, String> = all.iter().map(|e| (e.id.as_str(), group(e))).collect();
    let far_group = |e: &GraphEdge| -> Option<&str> {
        if e.dst_store.is_some() {
            return None;
        }
        groups.get(e.dst.as_str()).map(String::as_str)
    };

    // The matrix set: every filter but the scope ones.
    // Each summary lowercased once per request, not once per relation end.
    let lower: HashMap<&str, String> = match &f.text {
        Some(_) => all
            .iter()
            .map(|e| (e.id.as_str(), e.summary.to_lowercase()))
            .collect(),
        None => HashMap::new(),
    };
    let text_ok = |e: &GraphEdge| -> bool {
        let Some(t) = &f.text else { return true };
        let summary = |id: &str| lower.get(id);
        e.note
            .as_ref()
            .is_some_and(|n| n.to_lowercase().contains(t))
            || e.src.starts_with(t.as_str())
            || e.dst.starts_with(t.as_str())
            || summary(&e.src).is_some_and(|s| s.contains(t))
            || (e.dst_store.is_none() && summary(&e.dst).is_some_and(|s| s.contains(t)))
    };
    let admitted: Vec<&GraphEdge> = graph
        .edges
        .iter()
        .chain(graph.dangling_edges.iter())
        .filter(|e| f.admits_status(&e.status))
        .filter(|e| text_ok(e))
        .collect();

    let scope_ok = |e: &GraphEdge| -> bool {
        let a = groups.get(e.src.as_str()).map(String::as_str);
        let b = far_group(e);
        let is =
            |x: Option<&str>, want: &Option<String>| want.as_deref().is_none_or(|w| x == Some(w));
        if let Some(t) = &f.touches {
            if a != Some(t.as_str()) && b != Some(t.as_str()) {
                return false;
            }
        }
        let forward = is(a, &f.from) && is(b, &f.to);
        forward || (e.symmetric && is(b, &f.from) && is(a, &f.to))
    };
    let mut matched: Vec<(Key, &GraphEdge)> = admitted
        .iter()
        .filter(|e| scope_ok(e))
        .map(|e| (key_of(e), *e))
        .collect();
    matched.sort_by(|a, b| a.0.cmp(&b.0));

    let mut counts = Counts::default();
    for (_, e) in &matched {
        if e.status == LinkStatus::Proposed {
            counts.proposed += 1;
        }
        if e.kind == LinkKind::Contradicts && e.status != LinkStatus::Rejected {
            counts.contradictions += 1;
        }
    }
    let start = after
        .as_ref()
        .map(|k| matched.partition_point(|(x, _)| x <= k))
        .unwrap_or(0);
    let page = &matched[start.min(matched.len())..(start + limit).min(matched.len())];
    let next_cursor = (start + page.len() < matched.len())
        .then(|| page.last())
        .flatten()
        .map(|(k, _)| {
            hex_json(&Cursor {
                f: canonical.clone(),
                key: k.clone(),
            })
        });

    let page_edges: Vec<&GraphEdge> = page.iter().map(|(_, e)| *e).collect();
    let foreign = foreign_states(&page_edges).await;
    let end = |id: &str| -> End {
        match rows.get(id) {
            Some(r) => {
                let top_scope = top_scope_of(&r.logical);
                End {
                    summary: Some(r.summary.clone()),
                    scope: r.logical.first().cloned(),
                    color_slot: top_scope.as_ref().and_then(|t| slots.get(t).copied()),
                    top_scope,
                    invalidated: r.invalidated_at.is_some_and(|t| t <= now),
                    group: groups.get(id).cloned(),
                }
            }
            None => End {
                summary: None,
                scope: None,
                top_scope: None,
                color_slot: None,
                invalidated: false,
                group: None,
            },
        }
    };
    let items: Vec<Row> = page_edges
        .iter()
        .map(|e| {
            let state = match &e.dst_store {
                Some(s) => match foreign.get(s) {
                    Some(Some(found)) if found.contains(&e.dst) => "live",
                    Some(Some(_)) => "dangling",
                    _ => "unknown",
                },
                None => match rows.get(e.dst.as_str()) {
                    None => "dangling",
                    Some(r) if r.invalidated_at.is_some_and(|t| t <= now) => "invalidated",
                    Some(_) => "live",
                },
            };
            let from = end(&e.src);
            let to = if e.dst_store.is_some() {
                end("")
            } else {
                end(&e.dst)
            };
            Row {
                id: relation_id(e),
                src: e.src.clone(),
                dst: e.dst.clone(),
                dst_store: e.dst_store.clone(),
                kind: e.kind.clone(),
                symmetric: e.symmetric,
                status: e.status.clone(),
                origin: e.origin.clone(),
                confidence: e.confidence,
                at: e.created_at,
                note: e.note.clone(),
                state,
                src_summary: from.summary.clone(),
                dst_summary: to.summary.clone(),
                from,
                to,
            }
        })
        .collect();

    let matrix = after.is_none().then(|| {
        let mut cells: BTreeMap<(&str, &str), Tally> = BTreeMap::new();
        let mut unplaced = 0;
        for e in &admitted {
            let (Some(a), Some(b)) = (groups.get(e.src.as_str()), far_group(e)) else {
                unplaced += 1;
                continue;
            };
            cells.entry((a.as_str(), b)).or_default().add(e);
            if e.symmetric && a != b {
                cells.entry((b, a.as_str())).or_default().add(e);
            }
        }
        let mut names: Vec<&str> = cells.keys().flat_map(|(a, b)| [*a, *b]).collect();
        names.sort_unstable();
        names.dedup();
        let top_of = |name: &str| name.split('.').next().unwrap_or_default().to_string();
        let mut scopes: Vec<MatrixScope> = names
            .into_iter()
            .map(|name| {
                let top = top_of(name);
                MatrixScope {
                    slot: slots.get(&top).copied(),
                    name: name.to_string(),
                    top,
                }
            })
            .collect();
        // The store's colour order, sub-scopes under their top scope, "no
        // scope" last.
        scopes.sort_by(|x, y| {
            (x.top.is_empty(), x.slot.is_none(), x.slot, &x.top, &x.name).cmp(&(
                y.top.is_empty(),
                y.slot.is_none(),
                y.slot,
                &y.top,
                &y.name,
            ))
        });
        Matrix {
            level: if f.subs { "sub" } else { "top" },
            scopes,
            cells: cells
                .into_iter()
                .map(|((from, to), tally)| Cell {
                    from: from.to_string(),
                    to: to.to_string(),
                    tally,
                })
                .collect(),
            relations: admitted.len(),
            unplaced,
        }
    });

    ok(&RelationsPage {
        total: matched.len(),
        limit,
        items,
        next_cursor,
        counts,
        matrix,
        memories: graph.nodes.len(),
    })
}
