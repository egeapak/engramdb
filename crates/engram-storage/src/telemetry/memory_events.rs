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
//! **Kinds and surfaces are strings on disk.** A line or row whose kind or
//! surface this binary does not know (a newer binary's) is kept and stored
//! verbatim ([`EventRow`]), and only left out of the counts. A prune folds such
//! rows into the rollup's `other_kinds` before deleting them (below). Parsing strictly
//! would make a drain delete a newer binary's events as "unreadable".
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
//! **Every write to either table holds one per-store lock**
//! (`memory_usage.lock`, beside the store's `lancedb/`): the insert plus the
//! refresh it triggers, the prune, and the reconcile. Each is a
//! read-modify-write of whole rollup rows, and two of them interleaving
//! (a refresh that read a row before a prune folded it, then wrote it back)
//! would put the pruned events' counts back to zero — permanently, since the
//! raw rows are gone. Under the lock a refresh copies `base` and `watermark`
//! through from a read made inside the same critical section, so it can
//! never overwrite a prune's. The drain takes its own claim lock first and
//! this one inside it; nothing takes them in the other order.
//!
//! **Bounded memory.** Every scan streams into per-memory accumulators
//! rather than collecting events: a prune scans only `ts < cutoff`, then the
//! recent window of the memories it folded, in `IN (…)` chunks; a refresh of
//! many ids runs in chunks of [`MAX_IN_LIST`]. The accumulators hold one
//! entry per memory and its distinct session ids.
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
//! - **A clock that jumps forward.** A prune never moves a watermark past the
//!   newest event it folded (plus 1 µs), so a prune run under a wrong clock
//!   cannot hide later events. Events *recorded* under a clock that was ahead
//!   still carry their future timestamps, and a later prune may fold them
//!   early.
//! - **A rollup row can lag its raw rows** when a refresh fails after its
//!   insert committed (the error is returned; the daemon only logs it). The
//!   next event for that memory, or the next maintenance pass's
//!   [`reconcile_rollup`], recomputes it. Exact reads never use the rollup's
//!   totals, only its `base` and `watermark`.
//! - **The daily series covers raw events only.** With `retention_days`
//!   shorter than the series, its oldest days read as zero.
//! - **Kinds and counters a newer binary added.** A prune folds old events of
//!   a kind this binary does not know into the row's `base` under
//!   [`OTHER_KINDS_KEY`] (`{"<kind>": n}`, counts only: no sessions, no
//!   timestamps) instead of deleting them uncounted, and every `base` key
//!   this binary's [`UsageCounts`] does not know is carried through each
//!   rewrite verbatim. A binary that learns a kind must adopt its
//!   `other_kinds` entry into its own counter; until then those counts are
//!   kept but read by nobody.
//! - **An unreadable rollup row.** A `base` that does not parse freezes its
//!   row (warned on every read): it reads as base zero (its lifetime counts
//!   are missing from reads), no refresh or prune rewrites it, and its raw
//!   events are never pruned, so nothing is destroyed and a fixed row picks
//!   up exactly where it was. Every other row works as before.
//! - Metrics do not travel with a clone and are lost with `lancedb/`. Accepted:
//!   counts are churn and never belong in the `.md` files.
//!
//! **Reads never create anything.** Every read connects through
//! [`connect_existing`] and opens tables without creating them, so a
//! read-only consumer leaves a store exactly as it found it. A missing
//! directory or table reads as empty; any other failure is an error, never a
//! silent zero.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::write_lock::{acquire_lock_file, WriteLockGuard};

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
use super::persistence::{connect_existing, connect_for, is_in_memory_only, lancedb_root};

/// Raw event table, in the owning store's `lancedb/` directory.
pub const MEMORY_EVENTS_TABLE: &str = "memory_events";

/// Per-memory rollup table, beside [`MEMORY_EVENTS_TABLE`].
pub const MEMORY_USAGE_TABLE: &str = "memory_usage";

/// Days in the series [`load_daily_series`] returns by default.
pub const SERIES_DAYS: u32 = 30;

/// The most ids one `IN (…)` filter names; longer lists run in chunks.
pub const MAX_IN_LIST: usize = 512;

/// The per-store lock every write to the two tables holds (module docs).
const ROLLUP_LOCK_FILE: &str = "memory_usage.lock";

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
    /// The throttled lifecycle pass (auto-promotion), and the automatic
    /// edge proposers that run after an embed (`ops::link_proposals`).
    #[serde(rename = "maintenance")]
    Maintenance,
    /// `engramdb dashboard`'s edge review (confirm / reject), behind
    /// `--allow-writes`. Its reads record nothing.
    #[serde(rename = "dashboard")]
    Dashboard,
}

impl Surface {
    pub const ALL: [Surface; 8] = [
        Self::Mcp,
        Self::Cli,
        Self::HookSessionStart,
        Self::HookPrompt,
        Self::HookPreTool,
        Self::Daemon,
        Self::Maintenance,
        Self::Dashboard,
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
            Self::Dashboard => "dashboard",
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

/// An event as stored: a `memory_events` row, and what the drain reads off a
/// JSONL line. Kind and surface stay strings, so a line written by a newer
/// binary (a kind or surface this one does not know) is stored verbatim
/// instead of being dropped as unreadable. Unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    pub id: String,
    pub ts: DateTime<Utc>,
    pub memory_id: String,
    pub store_id: String,
    pub kind: String,
    pub surface: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub rank: Option<u32>,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub query_mode: Option<String>,
    #[serde(default)]
    pub related_id: Option<String>,
}

impl From<&MemoryEvent> for EventRow {
    fn from(e: &MemoryEvent) -> Self {
        Self {
            id: e.id.clone(),
            ts: e.ts,
            memory_id: e.memory_id.clone(),
            store_id: e.store_id.clone(),
            kind: e.kind.as_str().to_string(),
            surface: e.surface.as_str().to_string(),
            session_id: e.session_id.clone(),
            rank: e.rank,
            score: e.score,
            query_mode: e.query_mode.clone(),
            related_id: e.related_id.clone(),
        }
    }
}

impl EventRow {
    /// The kind, when this binary knows it. Rows of an unknown kind are kept
    /// in the table but left out of every count.
    pub fn known_kind(&self) -> Option<UsageKind> {
        UsageKind::parse(&self.kind)
    }

    /// The typed event, when both kind and surface are known.
    pub fn to_event(&self) -> Option<MemoryEvent> {
        Some(MemoryEvent {
            id: self.id.clone(),
            ts: self.ts,
            memory_id: self.memory_id.clone(),
            store_id: self.store_id.clone(),
            kind: self.known_kind()?,
            surface: Surface::parse(&self.surface)?,
            session_id: self.session_id.clone(),
            rank: self.rank,
            score: self.score,
            query_mode: self.query_mode.clone(),
            related_id: self.related_id.clone(),
        })
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
    /// `updated` events.
    pub updated: u64,
    /// `challenged` events.
    pub challenged: u64,
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
        self.updated += other.updated;
        self.challenged += other.challenged;
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
                    UsageKind::Updated => c.updated += 1,
                    UsageKind::Challenged => c.challenged += 1,
                    _ => {}
                }
                c.last_written_at = max_ts(c.last_written_at, Some(ts));
            }
        }
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

/// `ts` truncated to the column's precision. Every instant compared against
/// stored timestamps (cutoffs, watermarks) goes through this once, so the
/// Rust comparison and the SQL one (`ts_literal` prints microseconds) agree.
fn trunc_us(ts: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_micros(ts.timestamp_micros()).unwrap_or(ts)
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

/// Open `name`, or `None` when the table does not exist. Any other failure
/// is an error: a read that cannot open a table must not report zeros.
async fn open_existing(conn: &Connection, name: &str) -> Result<Option<Table>> {
    match conn.open_table(name).execute().await {
        Ok(t) => Ok(Some(t)),
        Err(lancedb::Error::TableNotFound { .. }) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("opening {name}")),
    }
}

async fn open_or_create_named(
    conn: &Connection,
    name: &str,
    schema: Arc<Schema>,
    indexed: &[&str],
) -> Result<Table> {
    if let Some(t) = open_existing(conn, name).await? {
        return Ok(t);
    }
    let table = match conn.create_empty_table(name, schema).execute().await {
        Ok(t) => t,
        // Another writer created it between our open and our create.
        Err(e) => match open_existing(conn, name).await? {
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

/// Take the store's rollup lock. The store's data directory must exist (it
/// is created by `MemoryStore::init`); the lock never creates it.
async fn rollup_lock(store_id: &str) -> Result<WriteLockGuard> {
    let root = lancedb_root(store_id)?;
    let data = root
        .parent()
        .with_context(|| format!("{} has no parent", root.display()))?;
    if !data.is_dir() {
        anyhow::bail!(
            "usage stats: store {store_id} has no data directory at {}",
            data.display()
        );
    }
    Ok(acquire_lock_file(data.join(ROLLUP_LOCK_FILE)).await?)
}

#[cfg(test)]
pub(crate) mod test_pause {
    //! A pause point inside a refresh, after it read the rollup rows and
    //! before it writes them: the one window the rollup lock exists to close.
    use std::collections::HashMap;
    use std::sync::{Arc, LazyLock, Mutex};
    use tokio::sync::Notify;

    type Pair = (Arc<Notify>, Arc<Notify>);
    static ARMED: LazyLock<Mutex<HashMap<String, Pair>>> = LazyLock::new(Default::default);

    /// Arm the pause for `store_id`: returns (reached, resume).
    pub(crate) fn arm(store_id: &str) -> Pair {
        let pair = (Arc::new(Notify::new()), Arc::new(Notify::new()));
        ARMED
            .lock()
            .unwrap()
            .insert(store_id.to_string(), pair.clone());
        pair
    }

    pub(crate) async fn point(store_id: &str) {
        let pair = ARMED.lock().unwrap().remove(store_id);
        if let Some((reached, resume)) = pair {
            reached.notify_one();
            resume.notified().await;
        }
    }
}

fn ts_array(values: Vec<Option<i64>>) -> TimestampMicrosecondArray {
    TimestampMicrosecondArray::from(values).with_timezone(Arc::<str>::from("UTC"))
}

fn events_batch(rows: &[EventRow]) -> Result<RecordBatch> {
    let s = |f: fn(&EventRow) -> Option<&str>| -> ArrayRef {
        Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>()))
    };
    let arrays: Vec<ArrayRef> = vec![
        s(|e| Some(e.id.as_str())),
        Arc::new(ts_array(
            rows.iter().map(|e| Some(e.ts.timestamp_micros())).collect(),
        )),
        s(|e| Some(e.memory_id.as_str())),
        s(|e| Some(e.store_id.as_str())),
        s(|e| Some(e.kind.as_str())),
        s(|e| Some(e.surface.as_str())),
        s(|e| e.session_id.as_deref()),
        Arc::new(UInt32Array::from(
            rows.iter().map(|e| e.rank).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            rows.iter()
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
    /// The keys of the stored `base` JSON this binary's [`UsageCounts`] does
    /// not know (a newer binary's counters, and [`OTHER_KINDS_KEY`]), carried
    /// through every rewrite verbatim.
    base_extra: serde_json::Map<String, serde_json::Value>,
    /// The stored `base` did not parse. Its counts are unknown, so the row is
    /// read as base zero, never rewritten, and its raw events never pruned
    /// (module docs, "An unreadable rollup row").
    base_unreadable: bool,
}

/// Key, inside a rollup row's `base` JSON, of the lifetime counts of event
/// kinds this binary does not know, by kind string: `{"<kind>": n}`. A prune
/// folds such events here instead of deleting them uncounted; a binary that
/// learns a kind adopts its entry from here into its own counter.
pub const OTHER_KINDS_KEY: &str = "other_kinds";

/// The JSON keys [`UsageCounts`] reads and writes (all of them, including
/// the optional ones it omits when empty).
const USAGE_COUNTS_KEYS: &[&str] = &[
    "returned",
    "injected",
    "fetched",
    "returned_sessions",
    "injected_sessions",
    "last_returned_at",
    "last_injected_at",
    "last_fetched_at",
    "updated",
    "challenged",
    "last_written_at",
];

/// Parse a stored `base`: the counts this binary knows, and every other key
/// verbatim.
fn parse_base(text: &str) -> Result<(UsageCounts, serde_json::Map<String, serde_json::Value>)> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let counts: UsageCounts = serde_json::from_value(value.clone())?;
    let mut extra = match value {
        serde_json::Value::Object(map) => map,
        _ => anyhow::bail!("base is not an object"),
    };
    extra.retain(|k, _| !USAGE_COUNTS_KEYS.contains(&k.as_str()));
    Ok((counts, extra))
}

/// The `base` JSON to store for `row`, or `None` when there is nothing.
fn base_text(row: &RollupRow) -> Option<String> {
    if row.base.is_empty() && row.base_extra.is_empty() {
        return None;
    }
    let mut value = serde_json::to_value(&row.base).ok()?;
    if let serde_json::Value::Object(map) = &mut value {
        for (k, v) in &row.base_extra {
            map.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    serde_json::to_string(&value).ok()
}

impl RollupRow {
    fn new(store_id: &str) -> Self {
        Self {
            store_id: store_id.to_string(),
            ..Default::default()
        }
    }
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
    let base: Vec<Option<String>> = rows.iter().map(|(_, r)| base_text(r)).collect();
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
        u(|c| c.updated),
        u(|c| c.challenged),
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

/// `column IN (…)` for at most [`MAX_IN_LIST`] ids (callers chunk).
fn id_filter(column: &str, ids: &[String]) -> String {
    debug_assert!(!ids.is_empty() && ids.len() <= MAX_IN_LIST);
    let list: Vec<String> = ids.iter().map(|s| str_literal(s)).collect();
    format!("{column} IN ({})", list.join(", "))
}

fn and(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("({a}) AND ({b})")),
        (a, None) => a,
        (None, b) => b,
    }
}

/// A raw event, as much of it as the reads need. Only known kinds.
#[derive(Debug, Clone)]
pub(crate) struct RawEvent {
    pub memory_id: String,
    pub kind: UsageKind,
    pub ts: DateTime<Utc>,
    pub session_id: Option<String>,
}

/// Stream every row matching `filter` into `visit`, without collecting.
/// Rows of an unknown kind are skipped.
async fn for_each_event(
    table: &Table,
    filter: Option<String>,
    visit: impl FnMut(RawEvent),
) -> Result<()> {
    for_each_event_and_other(table, filter, visit, |_, _, _| {}).await
}

/// [`for_each_event`], handing rows of a kind this binary does not know to
/// `other` as `(memory_id, kind, ts)` instead of skipping them.
async fn for_each_event_and_other(
    table: &Table,
    filter: Option<String>,
    mut visit: impl FnMut(RawEvent),
    mut other: impl FnMut(&str, &str, DateTime<Utc>),
) -> Result<()> {
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
    while let Some(batch) = stream.next().await {
        let batch = batch.context("reading memory_events batch")?;
        let mid = col::<StringArray>(&batch, "memory_id")?;
        let kind = col::<StringArray>(&batch, "kind")?;
        let ts = col::<TimestampMicrosecondArray>(&batch, "ts")?;
        let sid = col::<StringArray>(&batch, "session_id")?;
        for i in 0..batch.num_rows() {
            let Some(t) = DateTime::<Utc>::from_timestamp_micros(ts.value(i)) else {
                continue;
            };
            let Some(k) = UsageKind::parse(kind.value(i)) else {
                other(mid.value(i), kind.value(i), t);
                continue;
            };
            visit(RawEvent {
                memory_id: mid.value(i).to_string(),
                kind: k,
                ts: t,
                session_id: opt_str(sid, i).map(str::to_owned),
            });
        }
    }
    Ok(())
}

/// Rollup rows for `ids` (at most [`MAX_IN_LIST`]), or every row.
async fn load_rollup_rows(
    table: &Table,
    ids: Option<&[String]>,
) -> Result<HashMap<String, RollupRow>> {
    let mut query = table.query();
    if let Some(ids) = ids {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        query = query.only_if(id_filter("memory_id", ids));
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
            // A base that does not parse would silently zero lifetime counts
            // on the next write, so its row is frozen instead: read as base
            // zero, never rewritten, its raw events never pruned. Per row:
            // one bad row must not take the whole store's usage down.
            let (base, base_extra, base_unreadable) = match opt_str(base, i) {
                None => (UsageCounts::default(), Default::default(), false),
                Some(b) => match parse_base(b) {
                    Ok((counts, extra)) => (counts, extra, false),
                    Err(e) => {
                        tracing::warn!(
                            "memory_usage: unreadable base for memory {} ({e}); its usage \
                             is left as it is and its raw events are kept",
                            mid.value(i)
                        );
                        (UsageCounts::default(), Default::default(), true)
                    }
                },
            };
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
                    updated: upd.value(i),
                    challenged: chl.value(i),
                    last_written_at: opt_ts(lw, i),
                },
                watermark: opt_ts(wm, i),
                base,
                base_extra,
                base_unreadable,
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

/// Add `e` to its memory's accumulator unless it is older than that
/// memory's watermark.
fn fold(
    accs: &mut HashMap<String, UsageAccumulator>,
    e: RawEvent,
    watermark: Option<DateTime<Utc>>,
) {
    if watermark.is_some_and(|w| e.ts < w) {
        return;
    }
    accs.entry(e.memory_id)
        .or_default()
        .add(e.kind, e.ts, e.session_id.as_deref());
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
    let rows: Vec<EventRow> = events.iter().map(EventRow::from).collect();
    ingest_rows(store_id, &rows, dedupe).await
}

/// [`ingest_memory_events`] for rows as stored, unknown kinds included.
pub async fn ingest_rows(store_id: &str, rows: &[EventRow], dedupe: bool) -> Result<()> {
    if rows.is_empty() || is_in_memory_only(store_id) {
        return Ok(());
    }
    let conn = connect_for(store_id).await?;
    let table = events_table(&conn).await?;
    let usage = usage_table(&conn).await?;
    // The insert is under the lock too: a prune between our insert and our
    // refresh could otherwise delete an old row it never folded.
    let _lock = rollup_lock(store_id).await?;
    let batch = reader(events_batch(rows)?);
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
    let mut ids: Vec<String> = rows.iter().map(|e| e.memory_id.clone()).collect();
    ids.sort();
    ids.dedup();
    refresh_locked(&table, &usage, store_id, Some(&ids)).await
}

/// Recompute rollup rows from `base + raw events at or after the
/// watermark`, for `ids`. Takes the rollup lock.
pub async fn refresh_rollup(store_id: &str, ids: &[String]) -> Result<()> {
    refresh_entry(store_id, Some(ids)).await
}

/// Recompute every rollup row of the store from its raw rows, writing only
/// rows whose totals disagree: the repair for a refresh that failed after
/// its insert committed. Streams the raw window into per-memory
/// accumulators. The maintenance pass runs it after the prune.
pub async fn reconcile_rollup(store_id: &str) -> Result<()> {
    refresh_entry(store_id, None).await
}

async fn refresh_entry(store_id: &str, ids: Option<&[String]>) -> Result<()> {
    if is_in_memory_only(store_id) {
        return Ok(());
    }
    let Some(conn) = connect_existing(store_id).await? else {
        return Ok(());
    };
    let Some(events) = open_existing(&conn, MEMORY_EVENTS_TABLE).await? else {
        return Ok(());
    };
    let usage = usage_table(&conn).await?;
    let _lock = rollup_lock(store_id).await?;
    refresh_locked(&events, &usage, store_id, ids).await
}

/// The refresh body. The caller holds the rollup lock.
async fn refresh_locked(
    events: &Table,
    usage: &Table,
    store_id: &str,
    ids: Option<&[String]>,
) -> Result<()> {
    match ids {
        Some(ids) => {
            for chunk in ids.chunks(MAX_IN_LIST) {
                refresh_chunk(events, usage, store_id, Some(chunk)).await?;
            }
            Ok(())
        }
        None => refresh_chunk(events, usage, store_id, None).await,
    }
}

async fn refresh_chunk(
    events: &Table,
    usage: &Table,
    store_id: &str,
    ids: Option<&[String]>,
) -> Result<()> {
    let existing = load_rollup_rows(usage, ids).await?;
    #[cfg(test)]
    test_pause::point(store_id).await;
    let mut accs: HashMap<String, UsageAccumulator> = HashMap::new();
    let filter = ids.map(|ids| id_filter("memory_id", ids));
    for_each_event(events, filter, |e| {
        let wm = existing.get(&e.memory_id).and_then(|r| r.watermark);
        fold(&mut accs, e, wm);
    })
    .await?;
    let mut rows = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (id, acc) in &accs {
        seen.insert(id.as_str());
        let current = existing.get(id);
        if current.is_some_and(|c| c.base_unreadable) {
            continue;
        }
        // `base` and `watermark` are copied through from the read above,
        // made under the same lock: a refresh never moves them.
        let mut row = current.cloned().unwrap_or_else(|| RollupRow::new(store_id));
        let mut totals = row.base.clone();
        totals.merge(&acc.finish());
        if current.is_some_and(|c| c.totals == totals) {
            continue;
        }
        row.totals = totals;
        rows.push((id.clone(), row));
    }
    // A row whose raw window is empty (every event pruned) totals its base.
    for (id, row) in &existing {
        if !seen.contains(id.as_str()) && !row.base_unreadable && row.totals != row.base {
            let mut row = row.clone();
            row.totals = row.base.clone();
            rows.push((id.clone(), row));
        }
    }
    write_rollup(usage, rows).await
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
    let Some(conn) = connect_existing(store_id).await? else {
        return Ok(());
    };
    let Some(events) = open_existing(&conn, MEMORY_EVENTS_TABLE).await? else {
        return Ok(());
    };
    let cutoff = trunc_us(cutoff);
    let _lock = rollup_lock(store_id).await?;
    let old_filter = format!("ts < {}", ts_literal(cutoff));
    if events.count_rows(Some(old_filter.clone())).await? == 0 {
        return Ok(());
    }
    let usage = usage_table(&conn).await?;
    // One row per memory: bounded by the store's size, not its traffic.
    let rows = load_rollup_rows(&usage, None).await?;

    // Fold the old events (those not already folded by an earlier prune).
    // A frozen row (unreadable base) is not folded and its raw events are
    // not deleted: folding into a base we cannot read would lose it.
    let frozen: Vec<String> = {
        let mut f: Vec<String> = rows
            .iter()
            .filter(|(_, r)| r.base_unreadable)
            .map(|(id, _)| id.clone())
            .collect();
        f.sort();
        f
    };
    let mut old: HashMap<String, UsageAccumulator> = HashMap::new();
    // Old events of a kind this binary does not know, counted by kind into
    // the base's `other_kinds` rather than deleted uncounted.
    let mut other: HashMap<String, BTreeMap<String, u64>> = HashMap::new();
    // One per closure (each visitor owns its own); combined below.
    let mut newest_known: Option<DateTime<Utc>> = None;
    let mut newest_other: Option<DateTime<Utc>> = None;
    let is_frozen = |id: &str| frozen.binary_search_by(|f| f.as_str().cmp(id)).is_ok();
    for_each_event_and_other(
        &events,
        Some(old_filter),
        |e| {
            if is_frozen(&e.memory_id) {
                return;
            }
            newest_known = max_ts(newest_known, Some(e.ts));
            let wm = rows.get(&e.memory_id).and_then(|r| r.watermark);
            fold(&mut old, e, wm);
        },
        |mid, kind, ts| {
            if is_frozen(mid) {
                return;
            }
            newest_other = max_ts(newest_other, Some(ts));
            if rows
                .get(mid)
                .and_then(|r| r.watermark)
                .is_some_and(|w| ts < w)
            {
                return;
            }
            *other
                .entry(mid.to_string())
                .or_default()
                .entry(kind.to_string())
                .or_default() += 1;
        },
    )
    .await?;
    // Never past the newest event folded: a prune under a clock that jumped
    // ahead must not hide events recorded after the clock came back.
    let cut = match max_ts(newest_known, newest_other) {
        Some(n) => cutoff.min(n + chrono::Duration::microseconds(1)),
        None => cutoff,
    };

    let mut affected: Vec<String> = old.keys().chain(other.keys()).cloned().collect();
    affected.sort();
    affected.dedup();
    let mut out = Vec::with_capacity(affected.len());
    for chunk in affected.chunks(MAX_IN_LIST) {
        let mut recent: HashMap<String, UsageAccumulator> = HashMap::new();
        let filter = and(
            Some(format!("ts >= {}", ts_literal(cut))),
            Some(id_filter("memory_id", chunk)),
        );
        for_each_event(&events, filter, |e| {
            let prev = rows.get(&e.memory_id).and_then(|r| r.watermark);
            fold(&mut recent, e, max_ts(prev, Some(cut)));
        })
        .await?;
        for id in chunk {
            let mut row = rows
                .get(id)
                .cloned()
                .unwrap_or_else(|| RollupRow::new(store_id));
            if let Some(acc) = old.get(id) {
                row.base.merge(&acc.finish());
            }
            if let Some(kinds) = other.get(id) {
                fold_other_kinds(&mut row.base_extra, kinds);
            }
            // Never move a watermark backwards.
            row.watermark = max_ts(row.watermark, Some(cut));
            let mut totals = row.base.clone();
            if let Some(acc) = recent.get(id) {
                totals.merge(&acc.finish());
            }
            row.totals = totals;
            out.push((id.clone(), row));
        }
    }
    // The rollup commits first. A crash before the delete leaves raw rows
    // that are older than every watermark, which every read ignores. Nothing
    // can insert in between: inserts hold the same lock.
    write_rollup(&usage, out).await?;
    let mut delete = format!("ts < {}", ts_literal(cut));
    if !frozen.is_empty() {
        if frozen.len() > MAX_IN_LIST {
            tracing::warn!(
                "memory_events: {} memories of store {store_id} have an unreadable rollup \
                 row; pruning nothing",
                frozen.len()
            );
            return Ok(());
        }
        let list: Vec<String> = frozen.iter().map(|s| str_literal(s)).collect();
        delete = format!("({delete}) AND memory_id NOT IN ({})", list.join(", "));
    }
    events
        .delete(&delete)
        .await
        .with_context(|| format!("pruning memory_events for store {store_id}"))?;
    Ok(())
}

/// Add `kinds` (unknown kind → count) into a base's [`OTHER_KINDS_KEY`]
/// object. An entry (or the whole key) that is not what this binary writes
/// belongs to another binary: it is left as it is, never overwritten.
fn fold_other_kinds(
    extra: &mut serde_json::Map<String, serde_json::Value>,
    kinds: &BTreeMap<String, u64>,
) {
    let slot = extra
        .entry(OTHER_KINDS_KEY.to_string())
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    let serde_json::Value::Object(map) = slot else {
        tracing::warn!("memory_usage: base.{OTHER_KINDS_KEY} is not an object; left as it is");
        return;
    };
    for (kind, n) in kinds {
        let entry = map
            .entry(kind.clone())
            .or_insert_with(|| serde_json::Value::from(0u64));
        if let Some(cur) = entry.as_u64() {
            *entry = serde_json::Value::from(cur.saturating_add(*n));
        }
    }
}

/// Compact both tables. Best-effort, like the other telemetry tables.
pub async fn optimize_memory_events(store_id: &str) -> Result<()> {
    if is_in_memory_only(store_id) {
        return Ok(());
    }
    let Some(conn) = connect_existing(store_id).await? else {
        return Ok(());
    };
    for name in [MEMORY_EVENTS_TABLE, MEMORY_USAGE_TABLE] {
        if let Some(t) = open_existing(&conn, name).await? {
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
    pub fn add(
        &mut self,
        memory_id: &str,
        kind: UsageKind,
        ts: DateTime<Utc>,
        session_id: Option<&str>,
    ) {
        let entry = self.by_memory.entry(memory_id.to_string()).or_default();
        if entry.1.is_some_and(|w| ts < w) {
            return;
        }
        entry.2.add(kind, ts, session_id);
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
/// The whole-store form scans the whole raw window; prefer
/// [`load_rollup_entries`] for a view of every memory.
pub async fn load_table_usage(store_id: &str, memory: Option<&str>) -> Result<TableUsage> {
    let mut out = TableUsage::default();
    if is_in_memory_only(store_id) {
        return Ok(out);
    }
    let Some(conn) = connect_existing(store_id).await? else {
        return Ok(out);
    };
    let ids = memory.map(|m| vec![m.to_string()]);
    let rows = match open_existing(&conn, MEMORY_USAGE_TABLE).await? {
        Some(t) => load_rollup_rows(&t, ids.as_deref()).await?,
        None => HashMap::new(),
    };
    for (id, row) in &rows {
        out.by_memory.insert(
            id.clone(),
            (row.base.clone(), row.watermark, Default::default()),
        );
    }
    if let Some(events) = open_existing(&conn, MEMORY_EVENTS_TABLE).await? {
        let filter = ids.as_deref().map(|ids| id_filter("memory_id", ids));
        for_each_event(&events, filter, |e| {
            out.add(&e.memory_id, e.kind, e.ts, e.session_id.as_deref());
        })
        .await?;
    }
    Ok(out)
}

/// One rollup row as a read sees it: the totals as of the last refresh,
/// and the watermark below which an event is already counted (or ignored).
#[derive(Debug, Clone, PartialEq)]
pub struct RollupEntry {
    pub usage: MemoryUsage,
    pub watermark: Option<DateTime<Utc>>,
}

/// Every rollup row of the store: one row per memory, no raw scan. The
/// cheap read for a view of every memory at once; current to the last
/// drain or flush.
pub async fn load_rollup_entries(store_id: &str) -> Result<BTreeMap<String, RollupEntry>> {
    if is_in_memory_only(store_id) {
        return Ok(BTreeMap::new());
    }
    let Some(conn) = connect_existing(store_id).await? else {
        return Ok(BTreeMap::new());
    };
    let Some(table) = open_existing(&conn, MEMORY_USAGE_TABLE).await? else {
        return Ok(BTreeMap::new());
    };
    Ok(load_rollup_rows(&table, None)
        .await?
        .into_iter()
        .map(|(id, row)| {
            (
                id.clone(),
                RollupEntry {
                    usage: MemoryUsage {
                        memory_id: id,
                        store_id: row.store_id,
                        counts: row.totals,
                    },
                    watermark: row.watermark,
                },
            )
        })
        .collect())
}

/// The rollup's totals as of the last drain, without raw events or the JSONL.
pub async fn load_rollup(store_id: &str) -> Result<BTreeMap<String, MemoryUsage>> {
    Ok(load_rollup_entries(store_id)
        .await?
        .into_iter()
        .map(|(id, e)| (id, e.usage))
        .collect())
}

/// Raw events from the table for a daily series: for `memory`, or the whole
/// store, from the series' first day on.
pub async fn load_series_table(
    store_id: &str,
    memory: Option<&str>,
    series: &mut DailySeries,
) -> Result<()> {
    if is_in_memory_only(store_id) {
        return Ok(());
    }
    let Some(conn) = connect_existing(store_id).await? else {
        return Ok(());
    };
    let Some(events) = open_existing(&conn, MEMORY_EVENTS_TABLE).await? else {
        return Ok(());
    };
    let Some(first) = series.days.first().map(|d| d.day) else {
        return Ok(());
    };
    let since = first.and_hms_opt(0, 0, 0).map(|t| t.and_utc());
    let filter = and(
        since.map(|s| format!("ts >= {}", ts_literal(s))),
        memory.map(|m| format!("memory_id = {}", str_literal(m))),
    );
    for_each_event(&events, filter, |e| series.add(e.kind, e.ts)).await
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
    let Some(conn) = connect_existing(store_id).await? else {
        return Ok(out);
    };
    let Some(events) = open_existing(&conn, MEMORY_EVENTS_TABLE).await? else {
        return Ok(out);
    };
    let filter = and(
        Some(format!(
            "kind = {}",
            str_literal(UsageKind::Returned.as_str())
        )),
        since.map(|s| format!("ts >= {}", ts_literal(s))),
    );
    for_each_event(&events, filter, |e| {
        if let Some(s) = e.session_id {
            out.entry(e.memory_id).or_default().insert(s);
        }
    })
    .await?;
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
        assert_eq!((c.updated, c.challenged), (1, 1));
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

        assert_eq!(raw_rows(&sid).await, 1, "old rows deleted");
        let rollup = load_rollup(&sid).await.unwrap();
        let m1 = &rollup["m1"].counts;
        assert_eq!((m1.injected, m1.injected_sessions, m1.updated), (2, 2, 1));
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

    /// An older binary's prune must not destroy what a newer binary wrote:
    /// old events of a kind it does not know are counted into the base's
    /// `other_kinds` (not deleted uncounted), and base keys it does not know
    /// survive the rewrite verbatim.
    #[tokio::test]
    async fn prune_carries_unknown_kinds_and_unknown_base_keys() {
        let sid = store().await;
        let now = Utc::now();
        let old = now - chrono::Duration::days(200);
        let unknown = |mid: &str| {
            let mut r = EventRow::from(&ev(&sid, mid, UsageKind::Fetched, old, None));
            r.kind = "cited".to_string();
            r
        };
        let mut rows = vec![
            EventRow::from(&ev(&sid, "m1", UsageKind::Fetched, old, None)),
            unknown("m1"),
            unknown("m1"),
            unknown("m2"),
        ];
        rows.push(EventRow::from(&ev(
            &sid,
            "m1",
            UsageKind::Fetched,
            now,
            None,
        )));
        ingest_rows(&sid, &rows, false).await.unwrap();
        // A newer binary's base key on m1.
        let conn = connect_for(&sid).await.unwrap();
        let usage = usage_table(&conn).await.unwrap();
        let mut m1 = rollup_rows(&sid).await["m1"].clone();
        m1.base_extra
            .insert("future_count".to_string(), serde_json::json!(7));
        write_rollup(&usage, vec![("m1".to_string(), m1)])
            .await
            .unwrap();

        prune_memory_events(&sid, 90).await.unwrap();
        assert_eq!(raw_rows(&sid).await, 1, "only the recent event is left");
        let after = rollup_rows(&sid).await;
        let m1 = &after["m1"];
        assert_eq!(m1.base.fetched, 1);
        assert_eq!(m1.totals.fetched, 2);
        assert_eq!(m1.base_extra["future_count"], serde_json::json!(7));
        assert_eq!(
            m1.base_extra[OTHER_KINDS_KEY],
            serde_json::json!({"cited": 2})
        );
        assert_eq!(
            after["m2"].base_extra[OTHER_KINDS_KEY],
            serde_json::json!({"cited": 1})
        );
        // A refresh keeps them too.
        ingest_memory_events(
            &sid,
            &[ev(&sid, "m1", UsageKind::Fetched, now, None)],
            false,
        )
        .await
        .unwrap();
        let again = rollup_rows(&sid).await;
        assert_eq!(again["m1"].base_extra, m1.base_extra);
        assert_eq!(again["m1"].totals.fetched, 3);
    }

    #[test]
    fn usage_counts_keys_list_every_field() {
        let now = Some(Utc::now());
        let full = UsageCounts {
            last_returned_at: now,
            last_injected_at: now,
            last_fetched_at: now,
            last_written_at: now,
            ..Default::default()
        };
        let serde_json::Value::Object(map) = serde_json::to_value(&full).unwrap() else {
            panic!("not an object");
        };
        let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
        keys.sort();
        let mut want = USAGE_COUNTS_KEYS.to_vec();
        want.sort();
        assert_eq!(keys, want);
    }

    /// One unreadable rollup row must not take the store's usage down: reads,
    /// drains and prunes keep working, and that row is frozen — never
    /// rewritten, its raw events never pruned.
    #[tokio::test]
    async fn an_unreadable_base_freezes_its_row_only() {
        let sid = store().await;
        let now = Utc::now();
        let old = now - chrono::Duration::days(200);
        let events = vec![
            ev(&sid, "m1", UsageKind::Fetched, old, None),
            ev(&sid, "m2", UsageKind::Fetched, old, None),
        ];
        ingest_memory_events(&sid, &events, false).await.unwrap();
        prune_memory_events(&sid, 90).await.unwrap();
        // Both folded; now corrupt m1's base.
        let conn = connect_for(&sid).await.unwrap();
        let usage = usage_table(&conn).await.unwrap();
        usage
            .update()
            .only_if("memory_id = 'm1'")
            .column("base", "'{not json'")
            .execute()
            .await
            .unwrap();
        let old2 = now - chrono::Duration::days(150);
        let more = vec![
            ev(&sid, "m1", UsageKind::Fetched, old2, None),
            ev(&sid, "m2", UsageKind::Fetched, old2, None),
        ];
        ingest_memory_events(&sid, &more, false)
            .await
            .expect("a refresh skips the frozen row instead of failing");
        load_rollup(&sid).await.expect("reads still work");
        prune_memory_events(&sid, 90)
            .await
            .expect("a prune skips the frozen row instead of failing");
        assert_eq!(raw_rows(&sid).await, 1, "m1's raw event is kept");
        let rows = rollup_rows(&sid).await;
        assert!(rows["m1"].base_unreadable);
        assert_eq!(
            rows["m2"].base.fetched, 2,
            "the healthy row folds as before"
        );
        let base: Option<String> = {
            let conn = connect_for(&sid).await.unwrap();
            let mut stream = usage_table(&conn)
                .await
                .unwrap()
                .query()
                .only_if("memory_id = 'm1'")
                .execute()
                .await
                .unwrap();
            let batch = stream.next().await.unwrap().unwrap();
            let col = col::<StringArray>(&batch, "base").unwrap();
            opt_str(col, 0).map(str::to_owned)
        };
        assert_eq!(base.as_deref(), Some("{not json"), "never rewritten");
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

    /// The rollup rows, through a fresh table handle (a handle opened earlier
    /// can serve an older version).
    async fn rollup_rows(sid: &str) -> HashMap<String, RollupRow> {
        let conn = connect_for(sid).await.unwrap();
        load_rollup_rows(&usage_table(&conn).await.unwrap(), None)
            .await
            .unwrap()
    }

    async fn raw_rows(sid: &str) -> usize {
        let conn = connect_for(sid).await.unwrap();
        let table = open_existing(&conn, MEMORY_EVENTS_TABLE)
            .await
            .unwrap()
            .unwrap();
        table.count_rows(None).await.unwrap()
    }

    /// The race the rollup lock closes: a refresh that read a row, then a
    /// prune that folds that memory's old events and deletes them, then the
    /// refresh writing its stale row back. Without the lock the prune's base
    /// is overwritten and the pruned counts are gone for good.
    #[tokio::test]
    async fn a_prune_cannot_interleave_with_a_refresh() {
        let sid = store().await;
        let now = Utc::now();
        let old = now - chrono::Duration::days(200);
        let events = vec![
            ev(&sid, "m1", UsageKind::Fetched, old, None),
            ev(&sid, "m1", UsageKind::Fetched, old, None),
            ev(&sid, "m1", UsageKind::Fetched, now, None),
        ];
        ingest_memory_events(&sid, &events, false).await.unwrap();

        let (reached, resume) = test_pause::arm(&sid);
        let s1 = sid.clone();
        let refresh = tokio::spawn(async move { refresh_rollup(&s1, &["m1".to_string()]).await });
        reached.notified().await;
        let s2 = sid.clone();
        let prune = tokio::spawn(async move { prune_memory_events(&s2, 90).await });
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!prune.is_finished(), "the prune waits for the refresh");
        resume.notify_one();
        refresh.await.unwrap().unwrap();
        prune.await.unwrap().unwrap();

        assert_eq!(raw_rows(&sid).await, 1);
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.fetched, 3);
        let exact = load_table_usage(&sid, Some("m1"))
            .await
            .unwrap()
            .finish(&sid);
        assert_eq!(exact["m1"].counts.fetched, 3);
    }

    #[tokio::test]
    async fn concurrent_prunes_fold_once() {
        let sid = store().await;
        let old = Utc::now() - chrono::Duration::days(200);
        let events: Vec<_> = (0..5)
            .map(|_| ev(&sid, "m1", UsageKind::Injected, old, Some("s")))
            .collect();
        ingest_memory_events(&sid, &events, false).await.unwrap();
        let (a, b) = tokio::join!(prune_memory_events(&sid, 90), prune_memory_events(&sid, 90));
        a.unwrap();
        b.unwrap();
        let c = &load_rollup(&sid).await.unwrap()["m1"].counts;
        assert_eq!((c.injected, c.injected_sessions), (5, 1));
        assert_eq!(raw_rows(&sid).await, 0);
    }

    /// A cutoff with sub-microsecond digits: the event at the truncated
    /// instant must be either folded and deleted, or neither.
    #[tokio::test]
    async fn prune_cutoff_is_truncated_to_the_column_precision() {
        let sid = store().await;
        let at = trunc_us(Utc::now() - chrono::Duration::days(100));
        ingest_memory_events(&sid, &[ev(&sid, "m1", UsageKind::Fetched, at, None)], false)
            .await
            .unwrap();
        prune_at(&sid, at + chrono::Duration::nanoseconds(500))
            .await
            .unwrap();
        let exact = load_table_usage(&sid, Some("m1"))
            .await
            .unwrap()
            .finish(&sid);
        assert_eq!(exact["m1"].counts.fetched, 1, "counted exactly once");
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.fetched, 1);
    }

    /// A prune run with a clock far ahead stops its watermark just past the
    /// newest event it folded, so events recorded once the clock is right
    /// again still count.
    #[tokio::test]
    async fn a_future_cutoff_does_not_hide_later_events() {
        let sid = store().await;
        let now = Utc::now();
        ingest_memory_events(
            &sid,
            &[ev(&sid, "m1", UsageKind::Fetched, now, None)],
            false,
        )
        .await
        .unwrap();
        prune_at(&sid, now + chrono::Duration::days(365))
            .await
            .unwrap();
        let later = now + chrono::Duration::seconds(1);
        ingest_memory_events(
            &sid,
            &[ev(&sid, "m1", UsageKind::Fetched, later, None)],
            false,
        )
        .await
        .unwrap();
        let exact = load_table_usage(&sid, Some("m1"))
            .await
            .unwrap()
            .finish(&sid);
        assert_eq!(exact["m1"].counts.fetched, 2);
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.fetched, 2);
    }

    #[tokio::test]
    async fn unknown_kinds_are_stored_but_not_counted() {
        let sid = store().await;
        let known = EventRow::from(&ev(&sid, "m1", UsageKind::Fetched, Utc::now(), None));
        let mut unknown = known.clone();
        unknown.id = uuid::Uuid::now_v7().to_string();
        unknown.kind = "linked_v9".to_string();
        unknown.surface = "dashboard".to_string();
        assert!(unknown.to_event().is_none());
        ingest_rows(&sid, &[known, unknown], true).await.unwrap();
        assert_eq!(raw_rows(&sid).await, 2, "the newer binary's row is kept");
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.fetched, 1);
    }

    #[tokio::test]
    async fn reconcile_repairs_a_row_a_failed_refresh_left_behind() {
        let sid = store().await;
        let now = Utc::now();
        ingest_memory_events(
            &sid,
            &[ev(&sid, "m1", UsageKind::Fetched, now, None)],
            false,
        )
        .await
        .unwrap();
        // Rows inserted without their refresh (as if it had failed).
        let conn = connect_for(&sid).await.unwrap();
        let table = events_table(&conn).await.unwrap();
        let more: Vec<EventRow> = (0..2)
            .map(|_| EventRow::from(&ev(&sid, "m1", UsageKind::Fetched, now, None)))
            .collect();
        table
            .add(reader(events_batch(&more).unwrap()))
            .execute()
            .await
            .unwrap();
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.fetched, 1);
        reconcile_rollup(&sid).await.unwrap();
        assert_eq!(load_rollup(&sid).await.unwrap()["m1"].counts.fetched, 3);
    }

    #[tokio::test]
    async fn refresh_of_more_ids_than_one_filter_holds_runs_in_chunks() {
        let sid = store().await;
        let now = Utc::now();
        let events: Vec<_> = (0..MAX_IN_LIST + 3)
            .map(|i| ev(&sid, &format!("m{i:04}"), UsageKind::Fetched, now, None))
            .collect();
        ingest_memory_events(&sid, &events, false).await.unwrap();
        let rollup = load_rollup(&sid).await.unwrap();
        assert_eq!(rollup.len(), MAX_IN_LIST + 3);
        assert!(rollup.values().all(|u| u.counts.fetched == 1));
    }

    #[tokio::test]
    async fn reads_create_nothing() {
        let ghost = unique_sid();
        let parent = super::super::persistence::lancedb_root(&ghost)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(&parent).unwrap();
        assert!(load_rollup_entries(&ghost).await.unwrap().is_empty());
        assert!(load_table_usage(&ghost, None)
            .await
            .unwrap()
            .by_memory
            .is_empty());
        let mut series = DailySeries::empty(3, Utc::now().date_naive());
        load_series_table(&ghost, None, &mut series).await.unwrap();
        assert!(returned_sessions_table(&ghost, None)
            .await
            .unwrap()
            .is_empty());
        reconcile_rollup(&ghost).await.unwrap();
        prune_memory_events(&ghost, 1).await.unwrap();
        optimize_memory_events(&ghost).await.unwrap();
        assert!(
            std::fs::read_dir(&parent).unwrap().next().is_none(),
            "no lancedb/, no lock file"
        );
    }
}
