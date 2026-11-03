//! Per-memory usage: the `memory_events` LanceDB table and its derived
//! `memory_usage` rollup.
//!
//! Every time a memory is **returned** by a query, **injected** into an
//! agent's context by a hook, **fetched** by id, or written (created,
//! updated, challenged, …), one [`MemoryEvent`] is recorded. Events reach the
//! table two ways, exactly like hook calls ([`super::hook_events`]):
//! - **Daemon path.** Hooks the daemon serves buffer their `injected` events
//!   in memory and flush batches here (plain append: new by construction).
//! - **In-process path.** Every other writer (the CLI, the MCP server, hooks
//!   in their own process) appends a JSONL line under `.engramdb/state/`
//!   ([`super::usage`]), drained here later with `merge_insert` on `id`.
//!
//! **Where the rows live.** Each event is written to the log of the store
//! that *owns* the memory (`store_id`), not the store that asked: a group or
//! global memory surfacing in a project's query lands in the group's or the
//! global store's table. A group store's tables sit in its own
//! `groups/<id>/lancedb/`, the global store's in `global/lancedb/`, a
//! project's beside `stats_events`.
//!
//! **No schema bump.** Both tables are telemetry, created lazily on first
//! write like `hook_events`, never derived from the `.md` files and never
//! touched by the reindex that a schema migration runs. A store that predates
//! them simply has none until something is recorded; every reader treats a
//! missing table as empty.
//!
//! **Three read signals, never summed.** `returned` means a memory survived
//! ranking in a query answer; `injected` that a hook rendered it into an
//! agent's context (only what fit the budget); `fetched` that someone asked
//! for it by id. They measure different things and are reported side by side.
//! Sorting by one breaks ties on the others; nothing adds them.
//!
//! **The rollup** (`memory_usage`, one row per memory) holds lifetime counts.
//! Raw events are pruned at `[stats].retention_days`; the counts survive
//! because a prune first *folds* the events it is about to delete into the
//! row's `base` and moves the row's `watermark` to the cutoff. A row's totals
//! are always `base + (raw events at or after the watermark)`, so a prune that
//! crashed between writing the rollup and deleting the raw rows counts
//! nothing twice: the leftover rows are older than the watermark and ignored.
//!
//! Declared approximations and losses of the rollup:
//! - **Distinct sessions across a prune are summed.** A session with events on
//!   both sides of a watermark is counted once in `base` and once in the raw
//!   window. For sessions shorter than the retention window this needs a
//!   session alive at the instant of a prune.
//! - **An event older than its own watermark when it arrives is not
//!   counted.** It is inserted, ignored by every read, and deleted by the next
//!   prune. Only a JSONL line left undrained for longer than the retention
//!   window can arrive that late.
//! - **The daily series covers raw events only.** With `retention_days`
//!   shorter than the series, its oldest days read as zero.
//! - Metrics do not travel with a clone and are lost with `lancedb/`. Accepted:
//!   counts are churn and never belong in the `.md` files.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{
    Array, ArrayRef, Float64Array, RecordBatch, RecordBatchIterator, RecordBatchReader,
    StringArray, TimestampMicrosecondArray, UInt32Array, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use chrono::{DateTime, NaiveDate, Utc};
use futures_util::stream::StreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{Connection, Table};
use serde::{Deserialize, Serialize};

use super::hook_events::{retention_cutoff, str_literal, ts_literal};
use super::persistence::{connect_for, is_in_memory_only};

/// Raw event table, in the owning store's `lancedb/` directory.
pub const MEMORY_EVENTS_TABLE: &str = "memory_events";

/// Per-memory rollup table, beside [`MEMORY_EVENTS_TABLE`].
pub const MEMORY_USAGE_TABLE: &str = "memory_usage";

/// Days in the series [`load_daily_series`] returns by default.
pub const SERIES_DAYS: u32 = 30;

/// An `IN (…)` list longer than this is replaced by a full scan.
const MAX_IN_LIST: usize = 512;

/// What happened to a memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    /// Survived ranking in a query answer (MCP and CLI `query`).
    Returned,
    /// Rendered into an agent's context by a hook (what fit the budget).
    Injected,
    /// Opened explicitly by id (MCP and CLI `get`).
    Fetched,
    Created,
    Updated,
    Challenged,
    Resolved,
    Verified,
    Promoted,
    /// Folded into a new memory; `related_id` names it.
    Merged,
    Deleted,
    /// Reserved for typed edges (phase 3); nothing records it yet.
    Linked,
}

impl UsageKind {
    pub const ALL: [UsageKind; 12] = [
        Self::Returned,
        Self::Injected,
        Self::Fetched,
        Self::Created,
        Self::Updated,
        Self::Challenged,
        Self::Resolved,
        Self::Verified,
        Self::Promoted,
        Self::Merged,
        Self::Deleted,
        Self::Linked,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Returned => "returned",
            Self::Injected => "injected",
            Self::Fetched => "fetched",
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Challenged => "challenged",
            Self::Resolved => "resolved",
            Self::Verified => "verified",
            Self::Promoted => "promoted",
            Self::Merged => "merged",
            Self::Deleted => "deleted",
            Self::Linked => "linked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// One of the three read signals.
    pub fn is_read(&self) -> bool {
        matches!(self, Self::Returned | Self::Injected | Self::Fetched)
    }
}

/// Which front end recorded an event.
///
/// Hooks keep their hook surface whichever process ran them: *where* a hook
/// ran is a latency question `hook_events` already answers. `daemon` is
/// accepted for rows a future daemon-originated writer may add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Surface {
    #[serde(rename = "mcp")]
    Mcp,
    #[serde(rename = "cli")]
    Cli,
    #[serde(rename = "hook:session_start")]
    HookSessionStart,
    #[serde(rename = "hook:prompt")]
    HookPrompt,
    #[serde(rename = "hook:pre_tool")]
    HookPreTool,
    #[serde(rename = "daemon")]
    Daemon,
    /// The throttled lifecycle pass (auto-promotion).
    #[serde(rename = "maintenance")]
    Maintenance,
}

impl Surface {
    pub const ALL: [Surface; 7] = [
        Self::Mcp,
        Self::Cli,
        Self::HookSessionStart,
        Self::HookPrompt,
        Self::HookPreTool,
        Self::Daemon,
        Self::Maintenance,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Cli => "cli",
            Self::HookSessionStart => "hook:session_start",
            Self::HookPrompt => "hook:prompt",
            Self::HookPreTool => "hook:pre_tool",
            Self::Daemon => "daemon",
            Self::Maintenance => "maintenance",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// One usage event: a row of `memory_events` and a line of the JSONL log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEvent {
    /// UUID v7, unique per event: the drain's dedupe key.
    pub id: String,
    pub ts: DateTime<Utc>,
    /// The memory's full id.
    pub memory_id: String,
    /// The store that owns the memory (and whose log holds this event).
    pub store_id: String,
    pub kind: UsageKind,
    pub surface: Surface,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// 1-based position in a query answer (`returned`, `injected`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    /// The memory's score in that answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// `rank` or `filter`, for `returned`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_mode: Option<String>,
    /// The other memory involved: the new memory for `merged`, the successor
    /// for an invalidating `resolved`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_id: Option<String>,
}

impl MemoryEvent {
    /// A new event stamped now, with a fresh id.
    pub fn new(memory_id: &str, store_id: &str, kind: UsageKind, surface: Surface) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            ts: Utc::now(),
            memory_id: memory_id.to_string(),
            store_id: store_id.to_string(),
            kind,
            surface,
            session_id: None,
            rank: None,
            score: None,
            query_mode: None,
            related_id: None,
        }
    }

    /// Attach a session id; empty ids are dropped.
    pub fn with_session(mut self, session_id: Option<&str>) -> Self {
        self.session_id = session_id.filter(|s| !s.is_empty()).map(str::to_owned);
        self
    }

    /// Attach a 1-based rank and a score. A non-finite score is dropped: JSON
    /// has no NaN, and a line that does not parse costs the whole event.
    pub fn with_rank(mut self, rank: usize, score: f64) -> Self {
        self.rank = u32::try_from(rank).ok();
        self.score = score.is_finite().then_some(score);
        self
    }

    pub fn with_query_mode(mut self, mode: &str) -> Self {
        self.query_mode = Some(mode.to_string());
        self
    }

    pub fn with_related(mut self, related_id: Option<&str>) -> Self {
        self.related_id = related_id.map(str::to_owned);
        self
    }
}

// ---------------------------------------------------------------------------
// Counts
// ---------------------------------------------------------------------------

/// Lifetime usage of one memory. The read signals are kept apart.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageCounts {
    pub returned: u64,
    pub injected: u64,
    pub fetched: u64,
    /// Distinct sessions with a `returned` event.
    pub returned_sessions: u64,
    /// Distinct sessions with an `injected` event.
    pub injected_sessions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_returned_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_injected_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fetched_at: Option<DateTime<Utc>>,
    pub updated_n: u64,
    pub challenged_n: u64,
    /// Newest write of any kind (created, updated, challenged, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_written_at: Option<DateTime<Utc>>,
}

fn max_ts(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

impl UsageCounts {
    /// Add `other` (counts summed, timestamps maxed).
    pub fn merge(&mut self, other: &UsageCounts) {
        self.returned += other.returned;
        self.injected += other.injected;
        self.fetched += other.fetched;
        self.returned_sessions += other.returned_sessions;
        self.injected_sessions += other.injected_sessions;
        self.last_returned_at = max_ts(self.last_returned_at, other.last_returned_at);
        self.last_injected_at = max_ts(self.last_injected_at, other.last_injected_at);
        self.last_fetched_at = max_ts(self.last_fetched_at, other.last_fetched_at);
        self.updated_n += other.updated_n;
        self.challenged_n += other.challenged_n;
        self.last_written_at = max_ts(self.last_written_at, other.last_written_at);
    }

    pub fn is_empty(&self) -> bool {
        *self == UsageCounts::default()
    }
}

/// Counts for one memory with the session sets kept, so events from several
/// sources (raw table, JSONL) combine without counting a session twice.
#[derive(Debug, Clone, Default)]
pub struct UsageAccumulator {
    counts: UsageCounts,
    returned_sessions: HashSet<String>,
    injected_sessions: HashSet<String>,
}

impl UsageAccumulator {
    pub fn add(&mut self, kind: UsageKind, ts: DateTime<Utc>, session_id: Option<&str>) {
        let c = &mut self.counts;
        match kind {
            UsageKind::Returned => {
                c.returned += 1;
                c.last_returned_at = max_ts(c.last_returned_at, Some(ts));
                if let Some(s) = session_id {
                    self.returned_sessions.insert(s.to_string());
                }
            }
            UsageKind::Injected => {
                c.injected += 1;
                c.last_injected_at = max_ts(c.last_injected_at, Some(ts));
                if let Some(s) = session_id {
                    self.injected_sessions.insert(s.to_string());
                }
            }
            UsageKind::Fetched => {
                c.fetched += 1;
                c.last_fetched_at = max_ts(c.last_fetched_at, Some(ts));
            }
            write => {
                match write {
                    UsageKind::Updated => c.updated_n += 1,
                    UsageKind::Challenged => c.challenged_n += 1,
                    _ => {}
                }
                c.last_written_at = max_ts(c.last_written_at, Some(ts));
            }
        }
    }

    pub fn add_event(&mut self, e: &MemoryEvent) {
        self.add(e.kind, e.ts, e.session_id.as_deref());
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    /// The counts, with the session sets turned into numbers.
    pub fn finish(&self) -> UsageCounts {
        let mut c = self.counts.clone();
        c.returned_sessions = self.returned_sessions.len() as u64;
        c.injected_sessions = self.injected_sessions.len() as u64;
        c
    }

    /// The distinct sessions with a `returned` event.
    pub fn returned_session_ids(&self) -> &HashSet<String> {
        &self.returned_sessions
    }
}

/// Usage of one memory, as the read API returns it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MemoryUsage {
    pub memory_id: String,
    pub store_id: String,
    #[serde(flatten)]
    pub counts: UsageCounts,
}

/// One day of usage for a memory or a store (UTC days).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DailyUsage {
    pub day: NaiveDate,
    pub returned: u64,
    pub injected: u64,
    pub fetched: u64,
    /// Write events of every kind.
    pub writes: u64,
}

/// A dense daily series: one entry per day, oldest first, zeros included.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DailySeries {
    pub days: Vec<DailyUsage>,
}

impl DailySeries {
    /// `days` zeroed entries ending at `today`.
    pub fn empty(days: u32, today: NaiveDate) -> Self {
        let days = (0..days.max(1))
            .rev()
            .map(|back| DailyUsage {
                day: today - chrono::Duration::days(back as i64),
                ..Default::default()
            })
            .collect();
        Self { days }
    }

    /// Count one event; events outside the series are ignored.
    pub fn add(&mut self, kind: UsageKind, ts: DateTime<Utc>) {
        let day = ts.date_naive();
        let Some(first) = self.days.first().map(|d| d.day) else {
            return;
        };
        let idx = (day - first).num_days();
        if idx < 0 {
            return;
        }
        let Some(slot) = self.days.get_mut(idx as usize) else {
            return;
        };
        match kind {
            UsageKind::Returned => slot.returned += 1,
            UsageKind::Injected => slot.injected += 1,
            UsageKind::Fetched => slot.fetched += 1,
            _ => slot.writes += 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Tables
// ---------------------------------------------------------------------------

fn ts_type() -> DataType {
    DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
}

fn events_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("ts", ts_type(), false),
        Field::new("memory_id", DataType::Utf8, false),
        Field::new("store_id", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("surface", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, true),
        Field::new("rank", DataType::UInt32, true),
        Field::new("score", DataType::Float64, true),
        Field::new("query_mode", DataType::Utf8, true),
        Field::new("related_id", DataType::Utf8, true),
    ]))
}

fn usage_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("memory_id", DataType::Utf8, false),
        Field::new("store_id", DataType::Utf8, false),
        Field::new("returned_n", DataType::UInt64, false),
        Field::new("injected_n", DataType::UInt64, false),
        Field::new("fetched_n", DataType::UInt64, false),
        Field::new("returned_sessions", DataType::UInt64, false),
        Field::new("injected_sessions", DataType::UInt64, false),
        Field::new("last_returned_at", ts_type(), true),
        Field::new("last_injected_at", ts_type(), true),
        Field::new("last_fetched_at", ts_type(), true),
        Field::new("updated_n", DataType::UInt64, false),
        Field::new("challenged_n", DataType::UInt64, false),
        Field::new("last_written_at", ts_type(), true),
        // Raw events before this instant are folded into `base`.
        Field::new("watermark", ts_type(), true),
        // `UsageCounts` as JSON: what the pruned events contributed.
        Field::new("base", DataType::Utf8, true),
        Field::new("refreshed_at", ts_type(), false),
    ]))
}

async fn open_named(conn: &Connection, name: &str) -> Option<Table> {
    conn.open_table(name).execute().await.ok()
}

async fn open_or_create_named(
    conn: &Connection,
    name: &str,
    schema: Arc<Schema>,
    indexed: &[&str],
) -> Result<Table> {
    if let Some(t) = open_named(conn, name).await {
        return Ok(t);
    }
    let table = match conn.create_empty_table(name, schema).execute().await {
        Ok(t) => t,
        // Another writer created it between our open and our create.
        Err(e) => match open_named(conn, name).await {
            Some(t) => return Ok(t),
            None => return Err(e).with_context(|| format!("creating {name} table")),
        },
    };
    for column in indexed {
        if let Err(e) = table
            .create_index(&[*column], lancedb::index::Index::BTree(Default::default()))
            .execute()
            .await
        {
            tracing::warn!("{name}: failed to create BTree index on {column}: {e}");
        }
    }
    Ok(table)
}

async fn events_table(conn: &Connection) -> Result<Table> {
    // `ts` serves the prune and the series; `id` the drain's merge;
    // `memory_id` the per-memory reads.
    open_or_create_named(
        conn,
        MEMORY_EVENTS_TABLE,
        events_schema(),
        &["ts", "id", "memory_id"],
    )
    .await
}

async fn usage_table(conn: &Connection) -> Result<Table> {
    open_or_create_named(conn, MEMORY_USAGE_TABLE, usage_schema(), &["memory_id"]).await
}

fn ts_array(values: Vec<Option<i64>>) -> TimestampMicrosecondArray {
    TimestampMicrosecondArray::from(values).with_timezone(Arc::<str>::from("UTC"))
}

fn events_batch(events: &[MemoryEvent]) -> Result<RecordBatch> {
    let s = |f: fn(&MemoryEvent) -> Option<&str>| -> ArrayRef {
        Arc::new(StringArray::from(events.iter().map(f).collect::<Vec<_>>()))
    };
    let arrays: Vec<ArrayRef> = vec![
        s(|e| Some(e.id.as_str())),
        Arc::new(ts_array(
            events
                .iter()
                .map(|e| Some(e.ts.timestamp_micros()))
                .collect(),
        )),
        s(|e| Some(e.memory_id.as_str())),
        s(|e| Some(e.store_id.as_str())),
        s(|e| Some(e.kind.as_str())),
        s(|e| Some(e.surface.as_str())),
        s(|e| e.session_id.as_deref()),
        Arc::new(UInt32Array::from(
            events.iter().map(|e| e.rank).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            events
                .iter()
                .map(|e| e.score.filter(|v| v.is_finite()))
                .collect::<Vec<_>>(),
        )),
        s(|e| e.query_mode.as_deref()),
        s(|e| e.related_id.as_deref()),
    ];
    RecordBatch::try_new(events_schema(), arrays).context("building memory_events RecordBatch")
}

/// A rollup row as stored.
#[derive(Debug, Clone, Default, PartialEq)]
struct RollupRow {
    store_id: String,
    totals: UsageCounts,
    watermark: Option<DateTime<Utc>>,
    base: UsageCounts,
}

fn usage_batch(rows: &[(String, RollupRow)]) -> Result<RecordBatch> {
    let now = Utc::now().timestamp_micros();
    let u = |f: fn(&UsageCounts) -> u64| -> ArrayRef {
        Arc::new(UInt64Array::from(
            rows.iter().map(|(_, r)| f(&r.totals)).collect::<Vec<_>>(),
        ))
    };
    let t = |f: fn(&RollupRow) -> Option<DateTime<Utc>>| -> ArrayRef {
        Arc::new(ts_array(
            rows.iter()
                .map(|(_, r)| f(r).map(|v| v.timestamp_micros()))
                .collect(),
        ))
    };
    let base: Vec<Option<String>> = rows
        .iter()
        .map(|(_, r)| {
            (!r.base.is_empty())
                .then(|| serde_json::to_string(&r.base).ok())
                .flatten()
        })
        .collect();
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(
            rows.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            rows.iter()
                .map(|(_, r)| r.store_id.as_str())
                .collect::<Vec<_>>(),
        )),
        u(|c| c.returned),
        u(|c| c.injected),
        u(|c| c.fetched),
        u(|c| c.returned_sessions),
        u(|c| c.injected_sessions),
        t(|r| r.totals.last_returned_at),
        t(|r| r.totals.last_injected_at),
        t(|r| r.totals.last_fetched_at),
        u(|c| c.updated_n),
        u(|c| c.challenged_n),
        t(|r| r.totals.last_written_at),
        t(|r| r.watermark),
        Arc::new(StringArray::from(base)),
        Arc::new(ts_array(vec![Some(now); rows.len()])),
    ];
    RecordBatch::try_new(usage_schema(), arrays).context("building memory_usage RecordBatch")
}

fn reader(batch: RecordBatch) -> Box<dyn RecordBatchReader + Send> {
    let schema = batch.schema();
    Box::new(RecordBatchIterator::new(
        vec![Ok(batch)].into_iter(),
        schema,
    ))
}

fn col<'a, T: 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a T> {
    batch
        .column_by_name(name)
        .with_context(|| format!("memory usage: missing {name} column"))?
        .as_any()
        .downcast_ref::<T>()
        .with_context(|| format!("memory usage: {name} has an unexpected type"))
}

fn opt_ts(a: &TimestampMicrosecondArray, i: usize) -> Option<DateTime<Utc>> {
    (!a.is_null(i))
        .then(|| DateTime::<Utc>::from_timestamp_micros(a.value(i)))
        .flatten()
}

fn opt_str(a: &StringArray, i: usize) -> Option<&str> {
    (!a.is_null(i)).then(|| a.value(i))
}

fn id_filter(column: &str, ids: Option<&[String]>) -> Option<String> {
    let ids = ids?;
    if ids.is_empty() || ids.len() > MAX_IN_LIST {
        return None;
    }
    let list: Vec<String> = ids.iter().map(|s| str_literal(s)).collect();
    Some(format!("{column} IN ({})", list.join(", ")))
}

/// A raw event, as much of it as the reads need.
#[derive(Debug, Clone)]
pub(crate) struct RawEvent {
    pub memory_id: String,
    pub kind: UsageKind,
    pub ts: DateTime<Utc>,
    pub session_id: Option<String>,
}

async fn scan_events(table: &Table, filter: Option<String>) -> Result<Vec<RawEvent>> {
    let mut query = table.query().select(Select::Columns(vec![
        "memory_id".into(),
        "kind".into(),
        "ts".into(),
        "session_id".into(),
    ]));
    if let Some(f) = filter {
        query = query.only_if(f);
    }
    let mut stream = query.execute().await.context("scanning memory_events")?;
    let mut out = Vec::new();
    while let Some(batch) = stream.next().await {
        let batch = batch.context("reading memory_events batch")?;
        let mid = col::<StringArray>(&batch, "memory_id")?;
        let kind = col::<StringArray>(&batch, "kind")?;
        let ts = col::<TimestampMicrosecondArray>(&batch, "ts")?;
        let sid = col::<StringArray>(&batch, "session_id")?;
        for i in 0..batch.num_rows() {
            let (Some(k), Some(t)) = (
                UsageKind::parse(kind.value(i)),
                DateTime::<Utc>::from_timestamp_micros(ts.value(i)),
            ) else {
                continue;
            };
            out.push(RawEvent {
                memory_id: mid.value(i).to_string(),
                kind: k,
                ts: t,
                session_id: opt_str(sid, i).map(str::to_owned),
            });
        }
    }
    Ok(out)
}

async fn load_rollup_rows(
    table: &Table,
    ids: Option<&[String]>,
) -> Result<HashMap<String, RollupRow>> {
    let mut query = table.query();
    if let Some(f) = id_filter("memory_id", ids) {
        query = query.only_if(f);
    }
    let mut stream = query.execute().await.context("scanning memory_usage")?;
    let mut out = HashMap::new();
    while let Some(batch) = stream.next().await {
        let batch = batch.context("reading memory_usage batch")?;
        let mid = col::<StringArray>(&batch, "memory_id")?;
        let sid = col::<StringArray>(&batch, "store_id")?;
        let n = |name: &str| col::<UInt64Array>(&batch, name);
        let t = |name: &str| col::<TimestampMicrosecondArray>(&batch, name);
        let (ret, inj, fet, rs, is, upd, chl) = (
            n("returned_n")?,
            n("injected_n")?,
            n("fetched_n")?,
            n("returned_sessions")?,
            n("injected_sessions")?,
            n("updated_n")?,
            n("challenged_n")?,
        );
        let (lr, li, lf, lw, wm) = (
            t("last_returned_at")?,
            t("last_injected_at")?,
            t("last_fetched_at")?,
            t("last_written_at")?,
            t("watermark")?,
        );
        let base = col::<StringArray>(&batch, "base")?;
        for i in 0..batch.num_rows() {
            let row = RollupRow {
                store_id: sid.value(i).to_string(),
                totals: UsageCounts {
                    returned: ret.value(i),
                    injected: inj.value(i),
                    fetched: fet.value(i),
                    returned_sessions: rs.value(i),
                    injected_sessions: is.value(i),
                    last_returned_at: opt_ts(lr, i),
                    last_injected_at: opt_ts(li, i),
                    last_fetched_at: opt_ts(lf, i),
                    updated_n: upd.value(i),
                    challenged_n: chl.value(i),
                    last_written_at: opt_ts(lw, i),
                },
                watermark: opt_ts(wm, i),
                base: opt_str(base, i)
                    .and_then(|b| serde_json::from_str(b).ok())
                    .unwrap_or_default(),
            };
            out.insert(mid.value(i).to_string(), row);
        }
    }
    Ok(out)
}

async fn write_rollup(table: &Table, rows: Vec<(String, RollupRow)>) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut op = table.merge_insert(&["memory_id"]);
    op.when_matched_update_all(None);
    op.when_not_matched_insert_all();
    op.execute(reader(usage_batch(&rows)?))
        .await
        .context("merging memory_usage rows")?;
    Ok(())
}

/// Accumulate `events` per memory, keeping only those at or after each
/// memory's watermark (and, with `before`, strictly before it).
fn accumulate<'a>(
    events: impl Iterator<Item = &'a RawEvent>,
    watermark: impl Fn(&str) -> Option<DateTime<Utc>>,
    before: Option<DateTime<Utc>>,
) -> HashMap<String, UsageAccumulator> {
    let mut out: HashMap<String, UsageAccumulator> = HashMap::new();
    for e in events {
        if watermark(&e.memory_id).is_some_and(|w| e.ts < w) {
            continue;
        }
        if before.is_some_and(|b| e.ts >= b) {
            continue;
        }
        out.entry(e.memory_id.clone())
            .or_default()
            .add(e.kind, e.ts, e.session_id.as_deref());
    }
    out
}

/// Write `events` to the store's `memory_events` table, then refresh the
/// rollup rows of the memories they name.
///
/// `dedupe = false` is a plain append, for rows that are new by construction
/// (the daemon's own buffer). `dedupe = true` inserts only ids the table does
/// not hold yet, for re-ingestable sources (the JSONL drain). The rollup is
/// recomputed from the raw rows, never incremented, so ingesting the same
/// events twice cannot count them twice.
///
/// Errors (including "this store has no data directory") are returned;
/// telemetry callers log them and carry on.
pub async fn ingest_memory_events(
    store_id: &str,
    events: &[MemoryEvent],
    dedupe: bool,
) -> Result<()> {
    if events.is_empty() || is_in_memory_only(store_id) {
        return Ok(());
    }
    let conn = connect_for(store_id).await?;
    let table = events_table(&conn).await?;
    let batch = reader(events_batch(events)?);
    if dedupe {
        let mut op = table.merge_insert(&["id"]);
        op.when_not_matched_insert_all();
        op.execute(batch).await.context("merging memory_events")?;
    } else {
        table
            .add(batch)
            .execute()
            .await
            .context("appending memory_events")?;
    }
    let mut ids: Vec<String> = events.iter().map(|e| e.memory_id.clone()).collect();
    ids.sort();
    ids.dedup();
    refresh_rollup(store_id, Some(&ids)).await
}

/// Recompute rollup rows from `base + raw events at or after the
/// watermark`: for `ids`, or for every memory with events or a row.
pub async fn refresh_rollup(store_id: &str, ids: Option<&[String]>) -> Result<()> {
    if is_in_memory_only(store_id) {
        return Ok(());
    }
    let conn = connect_for(store_id).await?;
    let Some(events) = open_named(&conn, MEMORY_EVENTS_TABLE).await else {
        return Ok(());
    };
    let usage = usage_table(&conn).await?;
    let mut existing = load_rollup_rows(&usage, ids).await?;
    let raw = scan_events(&events, id_filter("memory_id", ids)).await?;
    let wanted: Option<HashSet<&str>> = ids.map(|ids| ids.iter().map(String::as_str).collect());
    let accs = accumulate(
        raw.iter().filter(|e| {
            wanted
                .as_ref()
                .is_none_or(|w| w.contains(e.memory_id.as_str()))
        }),
        |id| existing.get(id).and_then(|r| r.watermark),
        None,
    );
    let mut rows = Vec::with_capacity(accs.len());
    for (id, acc) in accs {
        let mut row = existing.remove(&id).unwrap_or_else(|| RollupRow {
            store_id: store_id.to_string(),
            ..Default::default()
        });
        let mut totals = row.base.clone();
        totals.merge(&acc.finish());
        row.totals = totals;
        rows.push((id, row));
    }
    write_rollup(&usage, rows).await
}

/// Delete raw events older than `retention_days`, after folding them into
/// the rollup so lifetime counts survive (see the module docs for why the
/// fold-then-delete order counts nothing twice).
pub async fn prune_memory_events(store_id: &str, retention_days: u64) -> Result<()> {
    if is_in_memory_only(store_id) {
        return Ok(());
    }
    let Some(cutoff) = retention_cutoff(Some(retention_days), Utc::now()) else {
        return Ok(());
    };
    prune_at(store_id, cutoff).await
}

async fn prune_at(store_id: &str, cutoff: DateTime<Utc>) -> Result<()> {
    let conn = connect_for(store_id).await?;
    let Some(events) = open_named(&conn, MEMORY_EVENTS_TABLE).await else {
        return Ok(());
    };
    let old_filter = format!("ts < {}", ts_literal(cutoff));
    if events.count_rows(Some(old_filter.clone())).await? == 0 {
        return Ok(());
    }
    let usage = usage_table(&conn).await?;
    let mut rows = load_rollup_rows(&usage, None).await?;
    let raw = scan_events(&events, None).await?;
    let wm = |id: &str| rows.get(id).and_then(|r| r.watermark);
    let old = accumulate(raw.iter(), wm, Some(cutoff));
    let mut ids: HashSet<String> = old.keys().cloned().collect();
    ids.extend(raw.iter().map(|e| e.memory_id.clone()));
    let recent = accumulate(raw.iter().filter(|e| e.ts >= cutoff), wm, None);
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let mut row = rows.remove(&id).unwrap_or_else(|| RollupRow {
            store_id: store_id.to_string(),
            ..Default::default()
        });
        if let Some(acc) = old.get(&id) {
            row.base.merge(&acc.finish());
        }
        // Never move a watermark backwards.
        row.watermark = max_ts(row.watermark, Some(cutoff));
        let mut totals = row.base.clone();
        if let Some(acc) = recent.get(&id) {
            totals.merge(&acc.finish());
        }
        row.totals = totals;
        out.push((id, row));
    }
    // The rollup commits first. A crash before the delete leaves raw rows
    // that are older than every watermark, which every read ignores.
    write_rollup(&usage, out).await?;
    events
        .delete(&old_filter)
        .await
        .with_context(|| format!("pruning memory_events for store {store_id}"))?;
    Ok(())
}

/// Compact both tables. Best-effort, like the other telemetry tables.
pub async fn optimize_memory_events(store_id: &str) -> Result<()> {
    if is_in_memory_only(store_id) {
        return Ok(());
    }
    let conn = connect_for(store_id).await?;
    for name in [MEMORY_EVENTS_TABLE, MEMORY_USAGE_TABLE] {
        if let Some(t) = open_named(&conn, name).await {
            t.optimize(lancedb::table::OptimizeAction::All)
                .await
                .with_context(|| format!("optimizing {name} for store {store_id}"))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// Exact per-memory state read from the tables: the folded base, the
/// watermark, and the raw window as an accumulator (so a caller can add
/// events from another source without counting a session twice).
#[derive(Debug, Default)]
pub struct TableUsage {
    pub by_memory: HashMap<String, (UsageCounts, Option<DateTime<Utc>>, UsageAccumulator)>,
}

impl TableUsage {
    /// Add an event not yet in the table. Ignored when older than the
    /// memory's watermark, exactly as the table's own reads ignore it.
    pub fn add(&mut self, e: &MemoryEvent) {
        let entry = self.by_memory.entry(e.memory_id.clone()).or_default();
        if entry.1.is_some_and(|w| e.ts < w) {
            return;
        }
        entry.2.add_event(e);
    }

    /// Final counts per memory.
    pub fn finish(&self, store_id: &str) -> BTreeMap<String, MemoryUsage> {
        self.by_memory
            .iter()
            .map(|(id, (base, _, acc))| {
                let mut counts = base.clone();
                counts.merge(&acc.finish());
                (
                    id.clone(),
                    MemoryUsage {
                        memory_id: id.clone(),
                        store_id: store_id.to_string(),
                        counts,
                    },
                )
            })
            .collect()
    }
}

/// Read usage from the tables: for `memory` alone, or for every memory with
/// a row or an event. Exact (sessions are counted from the raw window, not
/// taken from the rollup's totals). A store without the tables reads empty.
pub async fn load_table_usage(store_id: &str, memory: Option<&str>) -> Result<TableUsage> {
    let mut out = TableUsage::default();
    if is_in_memory_only(store_id) {
        return Ok(out);
    }
    let Ok(conn) = connect_for(store_id).await else {
        return Ok(out);
    };
    let ids = memory.map(|m| vec![m.to_string()]);
    let rows = match open_named(&conn, MEMORY_USAGE_TABLE).await {
        Some(t) => load_rollup_rows(&t, ids.as_deref()).await?,
        None => HashMap::new(),
    };
    for (id, row) in &rows {
        out.by_memory.insert(
            id.clone(),
            (row.base.clone(), row.watermark, Default::default()),
        );
    }
    if let Some(events) = open_named(&conn, MEMORY_EVENTS_TABLE).await {
        let raw = scan_events(&events, id_filter("memory_id", ids.as_deref())).await?;
        for e in &raw {
            let entry = out.by_memory.entry(e.memory_id.clone()).or_default();
            if entry.1.is_some_and(|w| e.ts < w) {
                continue;
            }
            entry.2.add(e.kind, e.ts, e.session_id.as_deref());
        }
    }
    Ok(out)
}

/// The rollup as of the last drain, without raw events or the JSONL: the
/// cheap read for a view of every memory at once.
pub async fn load_rollup(store_id: &str) -> Result<BTreeMap<String, MemoryUsage>> {
    if is_in_memory_only(store_id) {
        return Ok(BTreeMap::new());
    }
    let Ok(conn) = connect_for(store_id).await else {
        return Ok(BTreeMap::new());
    };
    let Some(table) = open_named(&conn, MEMORY_USAGE_TABLE).await else {
        return Ok(BTreeMap::new());
    };
    Ok(load_rollup_rows(&table, None)
        .await?
        .into_iter()
        .map(|(id, row)| {
            (
                id.clone(),
                MemoryUsage {
                    memory_id: id,
                    store_id: row.store_id,
                    counts: row.totals,
                },
            )
        })
        .collect())
}

/// Raw events from the table for a daily series: for `memory`, or the whole
/// store, from `since` on.
pub async fn load_series_table(
    store_id: &str,
    memory: Option<&str>,
    series: &mut DailySeries,
) -> Result<()> {
    if is_in_memory_only(store_id) {
        return Ok(());
    }
    let Ok(conn) = connect_for(store_id).await else {
        return Ok(());
    };
    let Some(events) = open_named(&conn, MEMORY_EVENTS_TABLE).await else {
        return Ok(());
    };
    let Some(first) = series.days.first().map(|d| d.day) else {
        return Ok(());
    };
    let since = first.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
    let mut filter = since.map(|s| format!("ts >= {}", ts_literal(s)));
    if let Some(m) = memory {
        let by_id = format!("memory_id = {}", str_literal(m));
        filter = Some(match filter {
            Some(f) => format!("{f} AND {by_id}"),
            None => by_id,
        });
    }
    for e in scan_events(&events, filter).await? {
        series.add(e.kind, e.ts);
    }
    Ok(())
}

/// The distinct sessions per memory with a `returned` event at or after
/// `since`, from the table. Used by the §11.3 promotion pass.
pub async fn returned_sessions_table(
    store_id: &str,
    since: Option<DateTime<Utc>>,
) -> Result<HashMap<String, HashSet<String>>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    if is_in_memory_only(store_id) {
        return Ok(out);
    }
    let Ok(conn) = connect_for(store_id).await else {
        return Ok(out);
    };
    let Some(events) = open_named(&conn, MEMORY_EVENTS_TABLE).await else {
        return Ok(out);
    };
    let mut filter = format!("kind = {}", str_literal(UsageKind::Returned.as_str()));
    if let Some(s) = since {
        filter.push_str(&format!(" AND ts >= {}", ts_literal(s)));
    }
    for e in scan_events(&events, Some(filter)).await? {
        if let Some(s) = e.session_id {
            out.entry(e.memory_id).or_default().insert(s);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_sid() -> String {
        static N: AtomicU64 = AtomicU64::new(0);
        format!("test-memev-{:016x}", N.fetch_add(1, Ordering::Relaxed))
    }

    async fn store() -> String {
        let sid = unique_sid();
        let parent = super::super::persistence::lancedb_root(&sid)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        tokio::fs::create_dir_all(&parent).await.unwrap();
        sid
    }

    fn ev(
        store: &str,
        mid: &str,
        kind: UsageKind,
        ts: DateTime<Utc>,
        session: Option<&str>,
    ) -> MemoryEvent {
        let mut e = MemoryEvent::new(mid, store, kind, Surface::Cli).with_session(session);
        e.ts = ts;
        e
    }

    #[test]
    fn kinds_and_surfaces_round_trip_their_names() {
        for k in UsageKind::ALL {
            assert_eq!(UsageKind::parse(k.as_str()), Some(k));
            let json = serde_json::to_string(&k).unwrap();
            assert_eq!(json, format!("\"{}\"", k.as_str()));
        }
        for s in Surface::ALL {
            assert_eq!(Surface::parse(s.as_str()), Some(s));
            let json = serde_json::to_string(&s).unwrap();
            assert_eq!(json, format!("\"{}\"", s.as_str()));
        }
    }

    #[test]
    fn non_finite_scores_are_dropped_and_lines_stay_parseable() {
        let e = MemoryEvent::new("m", "s", UsageKind::Returned, Surface::Mcp)
            .with_rank(1, f64::NAN)
            .with_session(Some(""));
        assert_eq!(e.score, None);
        assert_eq!(e.session_id, None);
        let line = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<MemoryEvent>(&line).unwrap(), e);
    }

    #[test]
    fn accumulator_keeps_signals_apart_and_sessions_distinct() {
        let now = Utc::now();
        let mut acc = UsageAccumulator::default();
        acc.add(UsageKind::Returned, now, Some("a"));
        acc.add(UsageKind::Returned, now, Some("a"));
        acc.add(UsageKind::Returned, now, Some("b"));
        acc.add(UsageKind::Injected, now, Some("a"));
        acc.add(UsageKind::Fetched, now, None);
        acc.add(UsageKind::Updated, now, None);
        acc.add(UsageKind::Challenged, now, None);
        acc.add(UsageKind::Created, now, None);
        let c = acc.finish();
        assert_eq!((c.returned, c.injected, c.fetched), (3, 1, 1));
        assert_eq!((c.returned_sessions, c.injected_sessions), (2, 1));
        assert_eq!((c.updated_n, c.challenged_n), (1, 1));
        assert_eq!(c.last_written_at, Some(now));
    }

    #[test]
    fn daily_series_is_dense_and_ignores_out_of_range_events() {
        let today = Utc::now().date_naive();
        let mut s = DailySeries::empty(3, today);
        assert_eq!(s.days.len(), 3);
        assert_eq!(s.days[2].day, today);
        s.add(UsageKind::Fetched, Utc::now());
        s.add(UsageKind::Created, Utc::now() - chrono::Duration::days(2));
        s.add(UsageKind::Fetched, Utc::now() - chrono::Duration::days(5));
        s.add(UsageKind::Fetched, Utc::now() + chrono::Duration::days(2));
        assert_eq!(s.days[2].fetched, 1);
        assert_eq!(s.days[0].writes, 1);
        assert_eq!(s.days.iter().map(|d| d.fetched).sum::<u64>(), 1);
    }

    #[tokio::test]
    async fn ingest_builds_the_rollup_and_a_reingest_adds_nothing() {
        let sid = store().await;
        let now = Utc::now();
        let events = vec![
            ev(&sid, "m1", UsageKind::Returned, now, Some("s1")),
            ev(&sid, "m1", UsageKind::Returned, now, Some("s2")),
            ev(&sid, "m1", UsageKind::Fetched, now, None),
            ev(&sid, "m2", UsageKind::Injected, now, Some("s1")),
        ];
        ingest_memory_events(&sid, &events, true).await.unwrap();
        ingest_memory_events(&sid, &events, true).await.unwrap();
        let rollup = load_rollup(&sid).await.unwrap();
        let m1 = &rollup["m1"].counts;
        assert_eq!((m1.returned, m1.returned_sessions, m1.fetched), (2, 2, 1));
        assert_eq!(rollup["m2"].counts.injected, 1);
        let table = load_table_usage(&sid, Some("m1"))
            .await
            .unwrap()
            .finish(&sid);
        assert_eq!(table["m1"].counts, *m1);
        assert!(!table.contains_key("m2"), "filtered to one memory");
    }

    #[tokio::test]
    async fn prune_folds_old_events_into_the_base_and_survives_a_crash() {
        let sid = store().await;
        let now = Utc::now();
        let old = now - chrono::Duration::days(200);
        let events = vec![
            ev(&sid, "m1", UsageKind::Injected, old, Some("old-sess")),
            ev(&sid, "m1", UsageKind::Updated, old, None),
            ev(&sid, "m1", UsageKind::Injected, now, Some("new-sess")),
            ev(&sid, "m3", UsageKind::Fetched, old, None),
        ];
        ingest_memory_events(&sid, &events, false).await.unwrap();
        prune_memory_events(&sid, 90).await.unwrap();

        let conn = connect_for(&sid).await.unwrap();
        let table = open_named(&conn, MEMORY_EVENTS_TABLE).await.unwrap();
        assert_eq!(table.count_rows(None).await.unwrap(), 1, "old rows deleted");
        let rollup = load_rollup(&sid).await.unwrap();
        let m1 = &rollup["m1"].counts;
        assert_eq!((m1.injected, m1.injected_sessions, m1.updated_n), (2, 2, 1));
        assert_eq!(
            rollup["m3"].counts.fetched, 1,
            "a fully pruned memory keeps its count"
        );

        // A crash between the rollup write and the delete leaves old raw
        // rows behind: re-insert them, and nothing is counted twice.
        let leftovers = vec![ev(&sid, "m1", UsageKind::Injected, old, Some("old-sess"))];
        ingest_memory_events(&sid, &leftovers, false).await.unwrap();
        let after = load_table_usage(&sid, None).await.unwrap().finish(&sid);
        assert_eq!(after["m1"].counts.injected, 2);
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.injected, 2);
        // And the next prune deletes them without folding them again.
        prune_memory_events(&sid, 90).await.unwrap();
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.injected, 2);
        optimize_memory_events(&sid).await.unwrap();
    }

    #[tokio::test]
    async fn series_and_promotion_sessions_read_the_raw_window() {
        let sid = store().await;
        let now = Utc::now();
        let events = vec![
            ev(&sid, "m1", UsageKind::Returned, now, Some("s1")),
            ev(&sid, "m1", UsageKind::Returned, now, Some("s1")),
            ev(
                &sid,
                "m1",
                UsageKind::Returned,
                now - chrono::Duration::days(1),
                Some("s2"),
            ),
            ev(&sid, "m2", UsageKind::Created, now, None),
            ev(&sid, "m2", UsageKind::Injected, now, Some("s3")),
        ];
        ingest_memory_events(&sid, &events, false).await.unwrap();
        let mut series = DailySeries::empty(SERIES_DAYS, now.date_naive());
        load_series_table(&sid, Some("m1"), &mut series)
            .await
            .unwrap();
        assert_eq!(series.days.last().unwrap().returned, 2);
        assert_eq!(series.days[series.days.len() - 2].returned, 1);
        let mut store_series = DailySeries::empty(SERIES_DAYS, now.date_naive());
        load_series_table(&sid, None, &mut store_series)
            .await
            .unwrap();
        let last = store_series.days.last().unwrap();
        assert_eq!((last.returned, last.injected, last.writes), (2, 1, 1));

        let sessions = returned_sessions_table(&sid, None).await.unwrap();
        assert_eq!(sessions["m1"].len(), 2);
        assert!(!sessions.contains_key("m2"), "injected does not count");
    }

    #[tokio::test]
    async fn missing_tables_or_store_read_as_empty() {
        let sid = store().await;
        assert!(load_rollup(&sid).await.unwrap().is_empty());
        assert!(load_table_usage(&sid, None)
            .await
            .unwrap()
            .by_memory
            .is_empty());
        prune_memory_events(&sid, 90).await.unwrap();
        let ghost = unique_sid();
        assert!(load_rollup(&ghost).await.unwrap().is_empty());
        let e = ev(&ghost, "m", UsageKind::Fetched, Utc::now(), None);
        assert!(ingest_memory_events(&ghost, &[e], false).await.is_err());
    }
}
