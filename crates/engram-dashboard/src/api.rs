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

/// Each line's colour slot: its rank in the whole store (invalidated
/// memories included, so a status filter never recolours a line), by memory
/// count, most first, then by name. The frontend paints `slot % 7` from its
/// palette and varies the pattern by `slot / 7`, so the seven biggest lines
/// get seven distinct solid colours and an eighth never repeats one exactly.
///
/// **The one place a slot is decided**: the list, its facets, the overview
/// and the detail page all call this over the same rows, so one line has one
/// colour on every page. A slot can move when line sizes change rank.
pub(crate) fn line_slots(all: &[IndexFilterable]) -> HashMap<String, u32> {
    let mut n: HashMap<String, usize> = HashMap::new();
    for e in all {
        if let Some(l) = line_of(&e.logical) {
            *n.entry(l).or_insert(0) += 1;
        }
    }
    let mut v: Vec<(String, usize)> = n.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter()
        .enumerate()
        .map(|(i, (l, _))| (l, i as u32))
        .collect()
}

fn slot_of(slots: &HashMap<String, u32>, line: Option<&String>) -> Option<u32> {
    line.and_then(|l| slots.get(l).copied())
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
    /// See [`line_slots`]; absent with `line`.
    #[serde(skip_serializing_if = "Option::is_none")]
    line_slot: Option<u32>,
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

fn row(
    e: &IndexFilterable,
    usage: Option<&UsageCounts>,
    slots: &HashMap<String, u32>,
) -> MemoryRow {
    let line = line_of(&e.logical);
    MemoryRow {
        line_slot: slot_of(slots, line.as_ref()),
        id: e.id.clone(),
        summary: e.summary.clone(),
        type_: wire(&e.type_),
        epistemic: e.epistemic.as_str(),
        status: status_str(e.status),
        visibility: wire(&e.visibility).to_lowercase(),
        criticality: e.criticality,
        line,
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
    /// Line counts only: see [`line_slots`].
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<u32>,
}

fn counts(map: BTreeMap<String, usize>) -> Vec<Count> {
    let mut v: Vec<Count> = map
        .into_iter()
        .map(|(name, count)| Count {
            name,
            count,
            slot: None,
        })
        .collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    v
}

/// [`counts`] for lines (`""` is "no logical scope"), with their slots.
fn line_counts(map: BTreeMap<String, usize>, slots: &HashMap<String, u32>) -> Vec<Count> {
    let mut v = counts(map);
    for c in &mut v {
        c.slot = slots.get(&c.name).copied();
    }
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
    /// Opaque; pass back as `cursor` with the same sort and dir.
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    /// Over the memories the status filter admits, before the other filters.
    facets: Facets,
    /// The index is empty while the manifest records memories: a reindex is
    /// rebuilding it elsewhere, and the page should say so, not "no memories".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    rebuilding: bool,
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
    let slots = line_slots(&all);
    let admitted = |e: &&IndexFilterable| -> bool {
        let by_validity = match status {
            Some("invalidated") => is_invalidated(e, now),
            Some("all") => true,
            _ => !is_invalidated(e, now),
        };
        by_validity && status_filter.is_none_or(|s| e.status == s)
    };
    let mut facet_types = BTreeMap::new();
    let mut facet_lines = BTreeMap::new();
    let mut facet_tags = BTreeMap::new();
    for e in all.iter().filter(admitted) {
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

    let matched: Vec<&IndexFilterable> = all
        .iter()
        .filter(admitted)
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
    let num = |e: &IndexFilterable, f: fn(&UsageCounts) -> u64| {
        SortKey::Num(u(e).map(f).unwrap_or(0) as f64)
    };
    let micros = |t: DateTime<Utc>| SortKey::Num(t.timestamp_micros() as f64);
    let key = |e: &IndexFilterable| -> Option<SortKey> {
        Some(match sort {
            "criticality" => SortKey::Num(e.criticality),
            "created" => micros(e.created_at),
            "updated" => micros(e.updated_at),
            "summary" => SortKey::Text(e.summary.to_lowercase()),
            "type" => SortKey::Text(wire(&e.type_)),
            "line" => SortKey::Text(line_of(&e.logical).unwrap_or_default()),
            "returned" => num(e, |c| c.returned),
            "injected" => num(e, |c| c.injected),
            "fetched" => num(e, |c| c.fetched),
            "updated_n" => num(e, |c| c.updated),
            // Never read sorts below every real time (all are after 1970).
            "last_read" => SortKey::Num(
                u(e).and_then(last_read)
                    .map(|t| t.timestamp_micros() as f64)
                    .unwrap_or(-1.0),
            ),
            _ => return None,
        })
    };
    if !matches!(
        sort,
        "criticality"
            | "created"
            | "updated"
            | "summary"
            | "type"
            | "line"
            | "returned"
            | "injected"
            | "fetched"
            | "updated_n"
            | "last_read"
    ) {
        return Err(ApiError::bad_request(format!(
            "unknown sort {sort:?}: use criticality, created, updated, summary, type, \
             line, returned, injected, fetched, updated_n or last_read"
        )));
    }
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
    let mut keyed: Vec<(SortKey, &IndexFilterable)> = matched
        .into_iter()
        .filter_map(|e| key(e).map(|k| (k, e)))
        .collect();
    keyed.sort_by(|a, b| order(&a.0, &a.1.id, &b.0, &b.1.id, desc));

    // Keyset paging: the cursor is the last row's (sort key, id), and the
    // next page starts strictly after it. An offset would skip or repeat
    // rows whenever an earlier row moved (a usage sort moves with every
    // read); a key does not depend on what came before.
    let start = match q.get("cursor") {
        Some(c) => {
            let cur = Cursor::decode(c)
                .ok_or_else(|| ApiError::bad_request("cursor must come from next_cursor"))?;
            if cur.sort != sort || cur.desc != desc {
                return Err(ApiError::bad_request(
                    "cursor belongs to another sort; start again without it",
                ));
            }
            keyed.partition_point(|(k, e)| {
                order(k, &e.id, &cur.key, &cur.id, desc) != std::cmp::Ordering::Greater
            })
        }
        None => 0,
    };
    let limit: usize = match q.get("limit") {
        Some(l) => l
            .parse::<usize>()
            .map_err(|_| ApiError::bad_request("limit must be a number"))?
            .clamp(1, MAX_LIMIT),
        None => DEFAULT_LIMIT,
    };
    let matched_n = keyed.len();
    let page = &keyed[start.min(matched_n)..(start + limit).min(matched_n)];
    let next_cursor = (start + page.len() < matched_n)
        .then(|| page.last())
        .flatten()
        .map(|(k, e)| {
            Cursor {
                sort: sort.to_string(),
                desc,
                key: k.clone(),
                id: e.id.clone(),
            }
            .encode()
        });
    let items: Vec<MemoryRow> = page.iter().map(|(_, e)| row(e, u(e), &slots)).collect();
    let rebuilding = total == 0 && store.manifest_memory_count().await.unwrap_or(0) > 0;
    ok(&MemoryPage {
        total,
        matched: matched_n,
        items,
        next_cursor,
        facets: Facets {
            types: counts(facet_types),
            lines: line_counts(facet_lines, &slots),
            tags,
        },
        rebuilding,
        usage_lines_skipped: usage.skipped_lines + usage.unknown_lines,
    })
}

/// A memories-table sort value.
#[derive(Clone, PartialEq, Serialize, serde::Deserialize)]
#[serde(untagged)]
enum SortKey {
    Num(f64),
    Text(String),
}

fn cmp_key(a: &SortKey, b: &SortKey) -> std::cmp::Ordering {
    match (a, b) {
        (SortKey::Num(x), SortKey::Num(y)) => x.total_cmp(y),
        (SortKey::Text(x), SortKey::Text(y)) => x.cmp(y),
        (SortKey::Num(_), SortKey::Text(_)) => std::cmp::Ordering::Less,
        (SortKey::Text(_), SortKey::Num(_)) => std::cmp::Ordering::Greater,
    }
}

/// The table order: the key in the asked direction, then the id ascending,
/// so equal keys still have one total order for the cursor to point into.
fn order(ka: &SortKey, ida: &str, kb: &SortKey, idb: &str, desc: bool) -> std::cmp::Ordering {
    let o = cmp_key(ka, kb);
    let o = if desc { o.reverse() } else { o };
    o.then_with(|| ida.cmp(idb))
}

/// `next_cursor`: the last row's position, hex-encoded JSON (URL-safe, and
/// opaque to the frontend).
#[derive(Serialize, serde::Deserialize)]
struct Cursor {
    sort: String,
    desc: bool,
    key: SortKey,
    id: String,
}

impl Cursor {
    fn encode(&self) -> String {
        serde_json::to_vec(self)
            .unwrap_or_default()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn decode(s: &str) -> Option<Self> {
        if !s.len().is_multiple_of(2) || s.len() > 4096 {
            return None;
        }
        let bytes: Option<Vec<u8>> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
            .collect();
        serde_json::from_slice(&bytes?).ok()
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    line_slot: Option<u32>,
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
    /// Per signal, never summed: a retrieval, a hook injection and a fetch
    /// by id are three different things (see `stats` in the CLI reference).
    last_7_days: Signals,
    previous_7_days: Signals,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_read_at: Option<DateTime<Utc>>,
}

/// The three read signals over a window, side by side.
#[derive(Serialize, Default, Debug, PartialEq, Eq)]
struct Signals {
    returned: u64,
    injected: u64,
    fetched: u64,
}

impl Signals {
    fn of(days: &[DailyUsage]) -> Self {
        days.iter().fold(Self::default(), |mut a, d| {
            a.returned += d.returned;
            a.injected += d.injected;
            a.fetched += d.fetched;
            a
        })
    }
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
    /// See `MemoryPage::rebuilding`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    rebuilding: bool,
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

    let slots = line_slots(&all);
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
        last_7_days: Signals::default(),
        previous_7_days: Signals::default(),
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
    let n = series.len();
    totals.last_7_days = Signals::of(&series[n.saturating_sub(7)..]);
    totals.previous_7_days = Signals::of(&series[n.saturating_sub(14)..n.saturating_sub(7)]);

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
            line_slot: slot_of(&slots, line_of(&e.logical).as_ref()),
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
            line_slot: slot_of(&slots, line_of(&e.logical).as_ref()),
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
            line_slot: slot_of(&slots, line_of(&e.logical).as_ref()),
            reason: if lr.is_some() { "stale" } else { "never_read" },
            at: Some(lr.unwrap_or(e.created_at)),
            detail: None,
            count: None,
        });
    }

    // This store's row only: probing every registered project per page
    // load would put the slowest mount on the machine in front of it.
    let project = projects::view(&id, state.current.as_ref())
        .await
        .ok()
        .flatten();
    let rebuilding = all.is_empty() && store.manifest_memory_count().await.unwrap_or(0) > 0;
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
        by_line: line_counts(by_line, &slots),
        oldest: stats.oldest,
        newest: stats.newest,
        avg_criticality: stats.avg_criticality,
        usage: totals,
        series,
        attention,
        rebuilding,
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
    /// See [`line_slots`].
    #[serde(skip_serializing_if = "Option::is_none")]
    line_slot: Option<u32>,
    memory: Memory,
    /// Exact (a raw scan of this memory's events); the list's counts come
    /// from the rollup and can lag these until its next refresh.
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
    let slots = line_slots(&store.list().await?);
    let line = line_of(&memory.logical);

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
        line_slot: slot_of(&slots, line.as_ref()),
        line,
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
