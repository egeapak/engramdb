//! The JSON API. Every endpoint is a GET and every one is a pure read: see
//! the crate docs ("No writes on GET") and [`crate::stores::ReadStore`].
//!
//! Memory text, titles, challenge evidence, tags and paths are returned as
//! data, unescaped: HTML escaping is the frontend's job (it renders through
//! `textContent` only), and escaping here as well would double-escape.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::{DateTime, Duration, Utc};
use engramdb::ops::parsing::{parse_epistemic, parse_memory_type, parse_status};
use engramdb::ops::usage::{DailyUsage, UsageCounts};
use engramdb::storage::lance_index::IndexFilterable;
use engramdb::types::{Memory, Status};
use serde::Serialize;

use crate::projects::{self, Location};
use crate::stores::ReadStore;
use crate::{ApiError, AppState};

type ApiResult = Result<Response, ApiError>;

/// A row comparator for the memories table's sort.
type RowCmp<'a> = Box<dyn Fn(&&IndexFilterable, &&IndexFilterable) -> std::cmp::Ordering + 'a>;

/// Serialize `body` as a JSON response with `no-store`.
pub fn json_response<T: Serialize + ?Sized>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_vec(body) {
        Ok(bytes) => {
            let mut r = (status, bytes).into_response();
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

fn ok<T: Serialize>(body: &T) -> ApiResult {
    Ok(json_response(StatusCode::OK, body))
}

/// The query string as a map (last value wins; empty values dropped).
fn query(raw: Option<String>) -> HashMap<String, String> {
    raw.map(|q| {
        form_urlencoded::parse(q.as_bytes())
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, v)| (k.into_owned(), v.trim().to_string()))
            .collect()
    })
    .unwrap_or_default()
}

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/session", get(session))
        .route("/api/projects", get(list_projects))
        .route("/api/projects/{id}/stats", get(project_stats))
        .route("/api/projects/{id}/memories", get(list_memories))
        .route("/api/projects/{id}/memories/{mid}", get(memory_detail))
        .route("/api/projects/{id}/doctor", get(project_doctor))
}

async fn open(state: &AppState, id: &str) -> Result<(Location, Arc<ReadStore>), ApiError> {
    let loc = projects::locate(id, state.current.as_ref()).await?;
    let store = state.stores.get(&loc).await?;
    Ok((loc, store))
}

// ---------------------------------------------------------------------------
// /api/session
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Session<'a> {
    version: &'static str,
    allow_writes: bool,
    current_project: Option<&'a str>,
    /// For the `X-EngramDB-CSRF` header on non-GET calls (phase 4).
    csrf: &'a str,
    schema_version: &'static str,
}

async fn session(State(state): State<Arc<AppState>>) -> ApiResult {
    ok(&Session {
        version: env!("CARGO_PKG_VERSION"),
        allow_writes: state.allow_writes,
        current_project: state.current.as_ref().map(|c| c.id.as_str()),
        csrf: state.security.csrf(),
        schema_version: engramdb::storage::manifest::CURRENT_SCHEMA_VERSION,
    })
}

async fn list_projects(State(state): State<Arc<AppState>>) -> ApiResult {
    ok(&projects::catalog(state.current.as_ref()).await?)
}

// ---------------------------------------------------------------------------
// Shared row shapes
// ---------------------------------------------------------------------------

/// The newest read of any kind. The three signals are never summed into one
/// count, but "when was it last read at all" is a fair question.
fn last_read(c: &UsageCounts) -> Option<DateTime<Utc>> {
    [c.last_returned_at, c.last_injected_at, c.last_fetched_at]
        .into_iter()
        .flatten()
        .max()
}

#[derive(Serialize, Default, Clone)]
struct UsageCols {
    returned: u64,
    injected: u64,
    fetched: u64,
    updated: u64,
    challenged: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_read_at: Option<DateTime<Utc>>,
}

impl From<&UsageCounts> for UsageCols {
    fn from(c: &UsageCounts) -> Self {
        Self {
            returned: c.returned,
            injected: c.injected,
            fetched: c.fetched,
            updated: c.updated,
            challenged: c.challenged,
            last_read_at: last_read(c),
        }
    }
}

/// The first segment of a memory's first logical scope: its "line" on the
/// route map. `None` for a memory with no logical scope.
fn line_of(logical: &[String]) -> Option<String> {
    logical
        .first()
        .and_then(|l| l.split('.').next())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Active => "active",
        Status::NeedsReview => "needs_review",
        Status::Challenged => "challenged",
    }
}

#[derive(Serialize)]
struct MemoryRow {
    id: String,
    summary: String,
    #[serde(rename = "type")]
    type_: String,
    epistemic: &'static str,
    status: &'static str,
    visibility: String,
    criticality: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<String>,
    logical: Vec<String>,
    physical: Vec<String>,
    tags: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invalidated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<DateTime<Utc>>,
    usage: UsageCols,
}

/// A unit enum's serde name (`"decision"`, `"shared"`), so the API speaks
/// the same words as the memory files and the MCP tools.
fn wire<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

fn row(e: &IndexFilterable, usage: Option<&UsageCounts>) -> MemoryRow {
    MemoryRow {
        id: e.id.clone(),
        summary: e.summary.clone(),
        type_: wire(&e.type_),
        epistemic: e.epistemic.as_str(),
        status: status_str(e.status),
        visibility: wire(&e.visibility).to_lowercase(),
        criticality: e.criticality,
        line: line_of(&e.logical),
        logical: e.logical.clone(),
        physical: e.physical.clone(),
        tags: e.tags.clone(),
        created_at: e.created_at,
        updated_at: e.updated_at,
        invalidated_at: e.invalidated_at,
        expires_at: e.expires_at,
        usage: usage.map(UsageCols::from).unwrap_or_default(),
    }
}

fn is_invalidated(e: &IndexFilterable, now: DateTime<Utc>) -> bool {
    e.invalidated_at.is_some_and(|t| t <= now)
}

// ---------------------------------------------------------------------------
// /api/projects/{id}/memories
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Count {
    name: String,
    count: usize,
}

fn counts(map: BTreeMap<String, usize>) -> Vec<Count> {
    let mut v: Vec<Count> = map
        .into_iter()
        .map(|(name, count)| Count { name, count })
        .collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    v
}

#[derive(Serialize)]
struct Facets {
    types: Vec<Count>,
    lines: Vec<Count>,
    tags: Vec<Count>,
}

#[derive(Serialize)]
struct MemoryPage {
    /// Memories in the store (invalidated ones included).
    total: usize,
    /// Memories matching the filters.
    matched: usize,
    items: Vec<MemoryRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    facets: Facets,
    /// Usage log lines that could not be read (torn or from a newer binary).
    #[serde(skip_serializing_if = "is_zero")]
    usage_lines_skipped: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 500;

async fn list_memories(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    let q = query(raw);
    let (_, store) = open(&state, &id).await?;
    let all = store.list().await?;
    let usage = store.usage_all().await?;
    let now = Utc::now();

    // Validate every filter up front: a typo is a 400, never "no results".
    let kinds = q
        .get("kind")
        .map(|k| {
            k.split(',')
                .map(|s| parse_memory_type(s.trim()))
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .transpose()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let epistemic = q
        .get("epistemic")
        .map(|k| {
            k.split(',')
                .map(|s| parse_epistemic(s.trim()))
                .collect::<anyhow::Result<Vec<_>>>()
        })
        .transpose()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let status = q.get("status").map(String::as_str);
    let status_filter = match status {
        None | Some("all") | Some("invalidated") => None,
        Some(s) => Some(parse_status(s).map_err(|e| ApiError::bad_request(e.to_string()))?),
    };
    let lines: Option<Vec<&str>> = q.get("line").map(|l| l.split(',').map(str::trim).collect());
    let scope = q.get("scope");
    let tag = q.get("tag");
    let text = q.get("q").map(|s| s.to_lowercase());

    let total = all.len();
    let mut facet_types = BTreeMap::new();
    let mut facet_lines = BTreeMap::new();
    let mut facet_tags = BTreeMap::new();
    for e in all.iter().filter(|e| !is_invalidated(e, now)) {
        *facet_types.entry(wire(&e.type_)).or_insert(0) += 1;
        *facet_lines
            .entry(line_of(&e.logical).unwrap_or_default())
            .or_insert(0) += 1;
        for t in &e.tags {
            *facet_tags.entry(t.clone()).or_insert(0) += 1;
        }
    }
    let mut tags = counts(facet_tags);
    tags.truncate(30);

    let mut matched: Vec<&IndexFilterable> = all
        .iter()
        .filter(|e| match status {
            Some("invalidated") => is_invalidated(e, now),
            Some("all") => true,
            _ => !is_invalidated(e, now),
        })
        .filter(|e| status_filter.is_none_or(|s| e.status == s))
        .filter(|e| kinds.as_ref().is_none_or(|k| k.contains(&e.type_)))
        .filter(|e| epistemic.as_ref().is_none_or(|k| k.contains(&e.epistemic)))
        .filter(|e| {
            lines.as_ref().is_none_or(|ls| {
                // `-` names "no logical scope", so it survives a comma join.
                let l = line_of(&e.logical).unwrap_or_else(|| "-".to_string());
                ls.contains(&l.as_str())
            })
        })
        .filter(|e| {
            scope.is_none_or(|s| {
                e.physical.iter().any(|p| p.contains(s.as_str()))
                    || e.logical.iter().any(|l| l.contains(s.as_str()))
            })
        })
        .filter(|e| tag.is_none_or(|t| e.tags.iter().any(|x| x == t)))
        .filter(|e| {
            text.as_ref().is_none_or(|t| {
                e.summary.to_lowercase().contains(t)
                    || e.id.starts_with(t.as_str())
                    || e.tags.iter().any(|x| x.to_lowercase().contains(t))
                    || e.logical.iter().any(|x| x.to_lowercase().contains(t))
                    || e.physical.iter().any(|x| x.to_lowercase().contains(t))
            })
        })
        .collect();

    let sort = q.get("sort").map(String::as_str).unwrap_or("criticality");
    let u = |e: &IndexFilterable| usage.by_memory.get(&e.id).map(|m| &m.counts);
    let num = |e: &IndexFilterable, f: fn(&UsageCounts) -> u64| u(e).map(f).unwrap_or(0);
    let cmp: RowCmp = match sort {
        "criticality" => Box::new(|a, b| a.criticality.total_cmp(&b.criticality)),
        "created" => Box::new(|a, b| a.created_at.cmp(&b.created_at)),
        "updated" => Box::new(|a, b| a.updated_at.cmp(&b.updated_at)),
        "summary" => Box::new(|a, b| a.summary.to_lowercase().cmp(&b.summary.to_lowercase())),
        "type" => Box::new(|a, b| wire(&a.type_).cmp(&wire(&b.type_))),
        "line" => Box::new(|a, b| line_of(&a.logical).cmp(&line_of(&b.logical))),
        "returned" => Box::new(move |a, b| num(a, |c| c.returned).cmp(&num(b, |c| c.returned))),
        "injected" => Box::new(move |a, b| num(a, |c| c.injected).cmp(&num(b, |c| c.injected))),
        "fetched" => Box::new(move |a, b| num(a, |c| c.fetched).cmp(&num(b, |c| c.fetched))),
        "updated_n" => Box::new(move |a, b| num(a, |c| c.updated).cmp(&num(b, |c| c.updated))),
        "last_read" => {
            Box::new(move |a, b| u(a).and_then(last_read).cmp(&u(b).and_then(last_read)))
        }
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown sort {other:?}: use criticality, created, updated, summary, type, \
                 line, returned, injected, fetched, updated_n or last_read"
            )))
        }
    };
    let text_sort = matches!(sort, "summary" | "type" | "line");
    let desc = match q.get("dir").map(String::as_str) {
        Some("asc") => false,
        Some("desc") => true,
        None => !text_sort,
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "unknown dir {other:?}: use asc or desc"
            )))
        }
    };
    matched.sort_by(|a, b| {
        let o = cmp(a, b);
        let o = if desc { o.reverse() } else { o };
        o.then_with(|| a.id.cmp(&b.id))
    });

    let offset: usize = match q.get("cursor") {
        Some(c) => c
            .parse()
            .map_err(|_| ApiError::bad_request("cursor must come from next_cursor"))?,
        None => 0,
    };
    let limit: usize = match q.get("limit") {
        Some(l) => l
            .parse::<usize>()
            .map_err(|_| ApiError::bad_request("limit must be a number"))?
            .clamp(1, MAX_LIMIT),
        None => DEFAULT_LIMIT,
    };
    let matched_n = matched.len();
    let items: Vec<MemoryRow> = matched
        .iter()
        .skip(offset)
        .take(limit)
        .map(|e| row(e, u(e)))
        .collect();
    let next = offset + items.len();
    ok(&MemoryPage {
        total,
        matched: matched_n,
        items,
        next_cursor: (next < matched_n).then(|| next.to_string()),
        facets: Facets {
            types: counts(facet_types),
            lines: counts(facet_lines),
            tags,
        },
        usage_lines_skipped: usage.skipped_lines + usage.unknown_lines,
    })
}

// ---------------------------------------------------------------------------
// /api/projects/{id}/stats
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AttentionItem {
    id: String,
    summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<String>,
    /// Why it is listed, as data for the frontend to word.
    reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<usize>,
}

#[derive(Serialize)]
struct UsageTotals {
    returned: u64,
    injected: u64,
    fetched: u64,
    updated: u64,
    challenged: u64,
    /// Memories with at least one read of any kind.
    used_memories: usize,
    reads_last_7_days: u64,
    reads_previous_7_days: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_read_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct StatsPage {
    project: Option<projects::ProjectView>,
    total: usize,
    live: usize,
    invalidated: usize,
    expired: usize,
    by_type: Vec<Count>,
    by_status: Vec<Count>,
    by_epistemic: Vec<Count>,
    by_line: Vec<Count>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oldest: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    newest: Option<DateTime<Utc>>,
    avg_criticality: f64,
    usage: UsageTotals,
    /// Oldest first; the last 30 days, today included.
    series: Vec<DailyUsage>,
    attention: Vec<AttentionItem>,
    schema: engramdb::storage::SchemaState,
    #[serde(skip_serializing_if = "Option::is_none")]
    staleness: Option<String>,
}

/// A memory nobody has read for this long, and older than this, is stale.
const STALE_DAYS: i64 = 30;
const ATTENTION_PER_KIND: usize = 12;

async fn project_stats(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let (_, store) = open(&state, &id).await?;
    let stats = store.stats().await?;
    let all = store.list().await?;
    let usage = store.usage_all().await?;
    let series = store.series(None).await?;
    let now = Utc::now();

    let live: Vec<&IndexFilterable> = all.iter().filter(|e| !is_invalidated(e, now)).collect();
    let mut by_epistemic = BTreeMap::new();
    let mut by_line = BTreeMap::new();
    for e in &live {
        *by_epistemic
            .entry(e.epistemic.as_str().to_string())
            .or_insert(0) += 1;
        *by_line
            .entry(line_of(&e.logical).unwrap_or_default())
            .or_insert(0) += 1;
    }

    let mut totals = UsageTotals {
        returned: 0,
        injected: 0,
        fetched: 0,
        updated: 0,
        challenged: 0,
        used_memories: 0,
        reads_last_7_days: 0,
        reads_previous_7_days: 0,
        last_read_at: None,
    };
    for m in usage.by_memory.values() {
        let c = &m.counts;
        totals.returned += c.returned;
        totals.injected += c.injected;
        totals.fetched += c.fetched;
        totals.updated += c.updated;
        totals.challenged += c.challenged;
        if c.returned + c.injected + c.fetched > 0 {
            totals.used_memories += 1;
        }
        totals.last_read_at = totals.last_read_at.max(last_read(c));
    }
    let reads = |d: &DailyUsage| d.returned + d.injected + d.fetched;
    let n = series.len();
    totals.reads_last_7_days = series.iter().skip(n.saturating_sub(7)).map(reads).sum();
    totals.reads_previous_7_days = series
        .iter()
        .skip(n.saturating_sub(14))
        .take(7.min(n.saturating_sub(7)))
        .map(reads)
        .sum();

    // Needs attention: challenged (with the newest evidence), needs review,
    // then stale.
    let mut attention = Vec::new();
    let challenged: Vec<String> = live
        .iter()
        .filter(|e| e.status == Status::Challenged)
        .take(ATTENTION_PER_KIND)
        .map(|e| e.id.clone())
        .collect();
    let mut loaded: HashMap<String, Memory> =
        store.get_batch(&challenged).await?.into_iter().collect();
    for cid in &challenged {
        let Some(e) = live.iter().find(|e| &e.id == cid) else {
            continue;
        };
        let m = loaded.remove(cid);
        let newest = m
            .as_ref()
            .and_then(|m| m.challenges.iter().max_by_key(|c| c.timestamp));
        attention.push(AttentionItem {
            id: e.id.clone(),
            summary: e.summary.clone(),
            line: line_of(&e.logical),
            reason: "challenged",
            at: newest.map(|c| c.timestamp),
            detail: newest.map(|c| c.evidence.clone()),
            count: m.as_ref().map(|m| m.challenges.len()),
        });
    }
    for e in live
        .iter()
        .filter(|e| e.status == Status::NeedsReview)
        .take(ATTENTION_PER_KIND)
    {
        attention.push(AttentionItem {
            id: e.id.clone(),
            summary: e.summary.clone(),
            line: line_of(&e.logical),
            reason: "needs_review",
            at: Some(e.updated_at),
            detail: None,
            count: None,
        });
    }
    let cutoff = now - Duration::days(STALE_DAYS);
    let mut stale: Vec<(&IndexFilterable, Option<DateTime<Utc>>)> = live
        .iter()
        .filter(|e| e.status == Status::Active && e.created_at < cutoff)
        .map(|e| {
            let lr = usage
                .by_memory
                .get(&e.id)
                .and_then(|m| last_read(&m.counts));
            (*e, lr)
        })
        .filter(|(_, lr)| lr.is_none_or(|t| t < cutoff))
        .collect();
    // Never read first (oldest first), then the longest unread.
    stale.sort_by(|a, b| match (a.1, b.1) {
        (None, None) => a.0.created_at.cmp(&b.0.created_at),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (Some(x), Some(y)) => x.cmp(&y),
    });
    for (e, lr) in stale.into_iter().take(ATTENTION_PER_KIND) {
        attention.push(AttentionItem {
            id: e.id.clone(),
            summary: e.summary.clone(),
            line: line_of(&e.logical),
            reason: if lr.is_some() { "stale" } else { "never_read" },
            at: Some(lr.unwrap_or(e.created_at)),
            detail: None,
            count: None,
        });
    }

    let project = projects::catalog(state.current.as_ref())
        .await
        .ok()
        .and_then(|c| c.projects.into_iter().find(|p| p.id == id));
    ok(&StatsPage {
        project,
        total: stats.total,
        live: live.len(),
        invalidated: all.len() - live.len(),
        expired: stats.expired,
        by_type: counts(stats.by_type.iter().map(|(t, n)| (wire(t), *n)).collect()),
        by_status: counts(
            stats
                .by_status
                .iter()
                .map(|(s, n)| (status_str(*s).to_string(), *n))
                .collect(),
        ),
        by_epistemic: counts(by_epistemic),
        by_line: counts(by_line),
        oldest: stats.oldest,
        newest: stats.newest,
        avg_criticality: stats.avg_criticality,
        usage: totals,
        series,
        attention,
        schema: store.schema_state().await,
        staleness: store.staleness().await,
    })
}

// ---------------------------------------------------------------------------
// /api/projects/{id}/memories/{mid}
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct Link {
    id: String,
    /// `None` when the other memory is not in this store (deleted, or in
    /// another store).
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    found: bool,
}

#[derive(Serialize)]
struct TimelineEntry {
    at: DateTime<Utc>,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct MemoryDetail {
    store_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<String>,
    memory: Memory,
    usage: UsageCounts,
    series: Vec<DailyUsage>,
    /// Newest first. Built from the memory's own lifecycle fields and the
    /// usage rollup's last-seen times; the raw per-event log is not read
    /// here (the rollup is the cheap read).
    timeline: Vec<TimelineEntry>,
    supersedes: Vec<Link>,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<Link>,
}

fn timeline(m: &Memory, u: &UsageCounts) -> Vec<TimelineEntry> {
    let mut t = vec![TimelineEntry {
        at: m.created_at,
        kind: "created",
        detail: m.provenance.reason.clone(),
    }];
    if m.updated_at > m.created_at {
        t.push(TimelineEntry {
            at: m.updated_at,
            kind: "updated",
            detail: None,
        });
    }
    if let Some(v) = m.verified_at {
        t.push(TimelineEntry {
            at: v,
            kind: "verified",
            detail: None,
        });
    }
    for c in &m.challenges {
        t.push(TimelineEntry {
            at: c.timestamp,
            kind: "challenged",
            detail: Some(c.evidence.clone()),
        });
    }
    if let Some(v) = m.invalidated_at {
        t.push(TimelineEntry {
            at: v,
            kind: "invalidated",
            detail: m.superseded_by.clone(),
        });
    }
    if let Some(v) = m.expires_at {
        t.push(TimelineEntry {
            at: v,
            kind: "expires",
            detail: None,
        });
    }
    for (at, kind) in [
        (u.last_returned_at, "last_returned"),
        (u.last_injected_at, "last_injected"),
        (u.last_fetched_at, "last_fetched"),
    ] {
        if let Some(at) = at {
            t.push(TimelineEntry {
                at,
                kind,
                detail: None,
            });
        }
    }
    t.sort_by_key(|e| std::cmp::Reverse(e.at));
    t
}

async fn memory_detail(
    State(state): State<Arc<AppState>>,
    Path((id, mid)): Path<(String, String)>,
) -> ApiResult {
    let (_, store) = open(&state, &id).await?;
    let memory = store.get(&mid).await?;
    let usage = store
        .usage_of(&memory.id)
        .await?
        .by_memory
        .remove(&memory.id)
        .map(|u| u.counts)
        .unwrap_or_default();
    let series = store.series(Some(&memory.id)).await?;

    let mut wanted: Vec<String> = memory.supersedes.clone();
    if let Some(s) = &memory.superseded_by {
        wanted.push(s.clone());
    }
    let found: HashMap<String, String> = store
        .get_batch(&wanted)
        .await?
        .into_iter()
        .map(|(id, m)| (id, m.summary))
        .collect();
    let link = |id: &String| Link {
        id: id.clone(),
        summary: found.get(id).cloned(),
        found: found.contains_key(id),
    };
    let supersedes = memory.supersedes.iter().map(link).collect();
    let superseded_by = memory.superseded_by.as_ref().map(link);
    ok(&MemoryDetail {
        store_id: store.store_id().to_string(),
        line: line_of(&memory.logical),
        timeline: timeline(&memory, &usage),
        usage,
        series,
        supersedes,
        superseded_by,
        memory,
    })
}

// ---------------------------------------------------------------------------
// /api/projects/{id}/doctor
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct DoctorPage {
    result: engramdb::ops::DoctorResult,
    schema: engramdb::storage::SchemaState,
    #[serde(skip_serializing_if = "Option::is_none")]
    staleness: Option<String>,
    /// Another live checkout shares this project id (see `doctor`).
    #[serde(skip_serializing_if = "Option::is_none")]
    checkout_conflict: Option<String>,
}

async fn project_doctor(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let (_, store) = open(&state, &id).await?;
    ok(&DoctorPage {
        result: store.doctor().await?,
        schema: store.schema_state().await,
        staleness: store.staleness().await,
        checkout_conflict: store
            .checkout_conflict()
            .await
            .map(|p| p.display().to_string()),
    })
}
