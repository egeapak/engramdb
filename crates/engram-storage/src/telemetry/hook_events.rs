//! Long-term hook call log: the per-project `hook_events` LanceDB table.
//!
//! Hook calls reach this table two ways:
//! - **Daemon path.** When a running daemon executes a hook, it buffers the
//!   event in memory and flushes batches here (plain append: every row is
//!   new by construction).
//! - **In-process path.** A hook that runs in its own process appends a line
//!   to `.engramdb/state/hook_stats.jsonl` ([`super::hooks`]), and a
//!   long-running process later *drains* that file into this table. The
//!   drain uses `merge_insert` on `id`, so ingesting the same file twice (a
//!   crash between the insert and the file delete) adds nothing.
//!
//! **A separate table from `stats_events`, on purpose.** `pre-tool-use` fires
//! on every Read/Write/Edit, far more often than any MCP tool call. In the
//! same table, hook rows would push tool rows out of the 50,000-row startup
//! replay that `stats_events` hydration relies on. A separate table also
//! means binaries that predate it never read it.
//!
//! The table lives in the project's `lancedb/` directory under the global
//! data dir, beside `stats_events`, and shares its connection cache and its
//! refusal to create scaffolding for a project that has no store.
//!
//! **Read model.** Counts, errors and the average cover every row in the
//! `retention_days` window (a scan of four small columns). Percentiles cover
//! the newest [`PERCENTILE_SAMPLES`] samples per (hook, path) group, so
//! `stats` stays fast however many months of events the window holds.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{
    Array, ArrayRef, BooleanArray, Float64Array, RecordBatch, RecordBatchIterator,
    RecordBatchReader, StringArray, TimestampMicrosecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use chrono::{DateTime, Utc};
use futures_util::stream::StreamExt;
use lancedb::query::{ColumnOrdering, ExecutableQuery, QueryBase, Select};
use lancedb::{Connection, Table};
use serde::{Deserialize, Serialize};

use super::persistence::{connect_for, is_in_memory_only};
use super::RingHistogram;

/// Table name, in the project's `lancedb/` directory.
pub const HOOK_EVENTS_TABLE: &str = "hook_events";

/// Percentiles are computed over at most this many of the newest samples per
/// (hook, path) group.
pub const PERCENTILE_SAMPLES: usize = 10_000;

/// Where a hook ran.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum HookPath {
    /// Executed by the shared daemon against its warm store.
    Daemon,
    /// Executed in the hook's own process.
    #[default]
    InProcess,
}

impl HookPath {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Daemon => "daemon",
            Self::InProcess => "in_process",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "daemon" => Some(Self::Daemon),
            "in_process" => Some(Self::InProcess),
            _ => None,
        }
    }
}

/// One hook call, as stored in `hook_events`.
#[derive(Debug, Clone, PartialEq)]
pub struct HookEvent {
    /// Unique per call. The dedupe key for the JSONL drain.
    pub id: String,
    pub ts: DateTime<Utc>,
    pub hook: String,
    pub duration_ms: f64,
    pub ok: bool,
    pub path: HookPath,
    pub session_id: Option<String>,
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("hook", DataType::Utf8, false),
        Field::new("duration_ms", DataType::Float64, false),
        Field::new("ok", DataType::Boolean, false),
        Field::new("path", DataType::Utf8, false),
        Field::new("session_id", DataType::Utf8, true),
    ]))
}

async fn open_table(conn: &Connection) -> Option<Table> {
    conn.open_table(HOOK_EVENTS_TABLE).execute().await.ok()
}

async fn open_or_create(conn: &Connection) -> Result<Table> {
    if let Some(t) = open_table(conn).await {
        return Ok(t);
    }
    let table = match conn
        .create_empty_table(HOOK_EVENTS_TABLE, schema())
        .execute()
        .await
    {
        Ok(t) => t,
        // Another writer created it between our open and our create.
        Err(e) => match open_table(conn).await {
            Some(t) => return Ok(t),
            None => return Err(e).context("creating hook_events table"),
        },
    };
    // `ts` serves the retention prune and the newest-first sample reads;
    // `id` lets the drain's `merge_insert` find matches without a full scan.
    for column in ["ts", "id"] {
        if let Err(e) = table
            .create_index(&[column], lancedb::index::Index::BTree(Default::default()))
            .execute()
            .await
        {
            tracing::warn!("hook_events: failed to create BTree index on {column}: {e}");
        }
    }
    Ok(table)
}

fn to_batch(events: &[HookEvent]) -> Result<RecordBatch> {
    let ts = TimestampMicrosecondArray::from(
        events
            .iter()
            .map(|e| e.ts.timestamp_micros())
            .collect::<Vec<_>>(),
    )
    .with_timezone(Arc::<str>::from("UTC"));
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(
            events.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(ts),
        Arc::new(StringArray::from(
            events.iter().map(|e| e.hook.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(Float64Array::from(
            events.iter().map(|e| e.duration_ms).collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            events.iter().map(|e| e.ok).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            events.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from(
            events
                .iter()
                .map(|e| e.session_id.as_deref())
                .collect::<Vec<_>>(),
        )),
    ];
    RecordBatch::try_new(schema(), arrays).context("building hook_events RecordBatch")
}

/// Write events to the project's `hook_events` table.
///
/// `dedupe = false` is a plain append, for rows that are new by construction
/// (the daemon's own buffer). `dedupe = true` inserts only ids the table does
/// not hold yet, for re-ingestable sources (the JSONL drain).
///
/// Errors (including "this project has no store") are returned; telemetry
/// callers log them and carry on.
pub async fn append_hook_events(
    project_id: &str,
    events: &[HookEvent],
    dedupe: bool,
) -> Result<()> {
    if events.is_empty() || is_in_memory_only(project_id) {
        return Ok(());
    }
    let conn = connect_for(project_id).await?;
    let table = open_or_create(&conn).await?;
    let batch = to_batch(events)?;
    let schema = batch.schema();
    let reader: Box<dyn RecordBatchReader + Send> = Box::new(RecordBatchIterator::new(
        vec![Ok(batch)].into_iter(),
        schema,
    ));
    if dedupe {
        let mut op = table.merge_insert(&["id"]);
        op.when_not_matched_insert_all();
        op.execute(reader)
            .await
            .context("merging hook_events rows")?;
    } else {
        table
            .add(reader)
            .execute()
            .await
            .context("appending hook_events rows")?;
    }
    Ok(())
}

/// Aggregate for one (hook, path) group.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupStats {
    pub calls: u64,
    pub errors: u64,
    pub sum_ms: f64,
    pub oldest: Option<DateTime<Utc>>,
    /// `(ts, duration_ms)`, newest first after [`build_view`] normalizes it;
    /// callers may push in any order.
    pub samples: Vec<(DateTime<Utc>, f64)>,
}

impl GroupStats {
    /// Count one call and keep its sample.
    pub fn add(&mut self, ts: DateTime<Utc>, ms: f64, ok: bool) {
        self.calls += 1;
        if !ok {
            self.errors += 1;
        }
        self.sum_ms += ms;
        self.oldest = Some(self.oldest.map_or(ts, |o| o.min(ts)));
        self.samples.push((ts, ms));
    }
}

/// Groups keyed by (hook name, path).
pub type Groups = BTreeMap<(String, HookPath), GroupStats>;

/// Start of the retention window, or `None` for unlimited retention.
pub fn retention_cutoff(retention_days: Option<u64>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    retention_days.map(|d| now - chrono::Duration::days(d.min(36_500) as i64))
}

fn ts_literal(ts: DateTime<Utc>) -> String {
    format!("TIMESTAMP '{}'", ts.format("%Y-%m-%d %H:%M:%S%.6f%:z"))
}

/// SQL string literal. Hook names come from our own writers, but a drained
/// JSONL file is a file on disk, so quote defensively.
fn str_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Read the table's aggregate for the retention window: counts, errors and
/// sums over every row, plus the newest `sample_cap` samples per group.
///
/// A project without the table (or without a store) reads as empty.
pub async fn load_groups(
    project_id: &str,
    retention_days: Option<u64>,
    sample_cap: usize,
) -> Result<Groups> {
    let mut groups = Groups::new();
    if is_in_memory_only(project_id) {
        return Ok(groups);
    }
    let Ok(conn) = connect_for(project_id).await else {
        return Ok(groups);
    };
    let Some(table) = open_table(&conn).await else {
        return Ok(groups);
    };
    let window =
        retention_cutoff(retention_days, Utc::now()).map(|c| format!("ts >= {}", ts_literal(c)));

    // Pass 1: counts over the whole window. Samples are not kept here; the
    // window can hold far more rows than the percentile cap.
    let mut query = table.query().select(Select::Columns(vec![
        "hook".into(),
        "path".into(),
        "ok".into(),
        "duration_ms".into(),
        "ts".into(),
    ]));
    if let Some(w) = &window {
        query = query.only_if(w.clone());
    }
    let mut stream = query.execute().await.context("scanning hook_events")?;
    while let Some(batch) = stream.next().await {
        let batch = batch.context("reading hook_events batch")?;
        let hook = str_col(&batch, "hook")?;
        let path = str_col(&batch, "path")?;
        let ok = bool_col(&batch, "ok")?;
        let ms = f64_col(&batch, "duration_ms")?;
        let ts = ts_col(&batch)?;
        for i in 0..batch.num_rows() {
            let Some(p) = HookPath::parse(path.value(i)) else {
                continue;
            };
            let Some(t) = DateTime::<Utc>::from_timestamp_micros(ts.value(i)) else {
                continue;
            };
            let g = groups.entry((hook.value(i).to_string(), p)).or_default();
            g.calls += 1;
            if !ok.value(i) {
                g.errors += 1;
            }
            g.sum_ms += ms.value(i);
            g.oldest = Some(g.oldest.map_or(t, |o| o.min(t)));
        }
    }
    drop(stream);

    // Pass 2: newest samples per group, pushed down as ORDER BY .. LIMIT.
    for ((hook, path), g) in groups.iter_mut() {
        let mut filter = format!(
            "hook = {} AND path = {}",
            str_literal(hook),
            str_literal(path.as_str())
        );
        if let Some(w) = &window {
            filter.push_str(" AND ");
            filter.push_str(w);
        }
        let mut stream = table
            .query()
            .select(Select::Columns(vec!["ts".into(), "duration_ms".into()]))
            .only_if(filter)
            .order_by(Some(vec![ColumnOrdering::desc_nulls_last(
                "ts".to_string(),
            )]))
            .limit(sample_cap)
            .execute()
            .await
            .context("reading hook_events samples")?;
        while let Some(batch) = stream.next().await {
            let batch = batch.context("reading hook_events sample batch")?;
            let ts = ts_col(&batch)?;
            let ms = f64_col(&batch, "duration_ms")?;
            for i in 0..batch.num_rows() {
                if let Some(t) = DateTime::<Utc>::from_timestamp_micros(ts.value(i)) {
                    g.samples.push((t, ms.value(i)));
                }
            }
        }
    }
    Ok(groups)
}

fn str_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    batch
        .column_by_name(name)
        .with_context(|| format!("hook_events: missing {name} column"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .with_context(|| format!("hook_events: {name} is not Utf8"))
}

fn bool_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a BooleanArray> {
    batch
        .column_by_name(name)
        .with_context(|| format!("hook_events: missing {name} column"))?
        .as_any()
        .downcast_ref::<BooleanArray>()
        .with_context(|| format!("hook_events: {name} is not Boolean"))
}

fn f64_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Float64Array> {
    batch
        .column_by_name(name)
        .with_context(|| format!("hook_events: missing {name} column"))?
        .as_any()
        .downcast_ref::<Float64Array>()
        .with_context(|| format!("hook_events: {name} is not Float64"))
}

fn ts_col(batch: &RecordBatch) -> Result<&TimestampMicrosecondArray> {
    batch
        .column_by_name("ts")
        .context("hook_events: missing ts column")?
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .context("hook_events: ts is not Timestamp(Microsecond)")
}

/// Latency and outcome counts for one path of one hook.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PathTimingStats {
    pub calls: u64,
    pub errors: u64,
    pub avg: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

/// Latency and outcome counts for one hook, over every path, plus the
/// per-path breakdown.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HookTimingStats {
    pub calls: u64,
    pub errors: u64,
    pub avg: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    /// Keyed by `daemon` / `in_process`. Omitted when empty.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub by_path: BTreeMap<String, PathTimingStats>,
}

/// Per-hook summary, keyed by hook name. Times are milliseconds.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HookStatsView {
    /// Oldest sample included. Counts are "since this instant": rotation,
    /// draining and `retention_days` all bound the history.
    pub since: Option<DateTime<Utc>>,
    pub by_hook: BTreeMap<String, HookTimingStats>,
    #[serde(skip_serializing_if = "is_zero")]
    pub skipped_lines: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl HookStatsView {
    pub fn is_empty(&self) -> bool {
        self.by_hook.is_empty()
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Nearest-rank percentiles over `samples` (the same method as
/// [`RingHistogram`]). Returns `(p50, p95, p99)`.
fn percentiles(samples: &[(DateTime<Utc>, f64)]) -> (f64, f64, f64) {
    let mut hist = RingHistogram::new(samples.len());
    for (_, ms) in samples {
        hist.record(*ms);
    }
    (
        round1(hist.percentile(0.50)),
        round1(hist.percentile(0.95)),
        round1(hist.percentile(0.99)),
    )
}

/// Newest first, capped.
fn newest(samples: &mut Vec<(DateTime<Utc>, f64)>, cap: usize) {
    samples.sort_by_key(|s| std::cmp::Reverse(s.0));
    samples.truncate(cap);
}

/// Turn groups into the per-hook view. Percentiles use the newest
/// `sample_cap` samples of each group, and of each hook across its paths.
pub fn build_view(mut groups: Groups, sample_cap: usize, skipped_lines: u64) -> HookStatsView {
    let mut by_hook: BTreeMap<String, HookTimingStats> = BTreeMap::new();
    let mut hook_samples: BTreeMap<String, Vec<(DateTime<Utc>, f64)>> = BTreeMap::new();
    let mut since: Option<DateTime<Utc>> = None;
    for ((hook, path), g) in groups.iter_mut() {
        if g.calls == 0 {
            continue;
        }
        newest(&mut g.samples, sample_cap);
        if let Some(o) = g.oldest {
            since = Some(since.map_or(o, |s| s.min(o)));
        }
        let (p50, p95, p99) = percentiles(&g.samples);
        let entry = by_hook
            .entry(hook.clone())
            .or_insert_with(|| HookTimingStats {
                calls: 0,
                errors: 0,
                avg: 0.0,
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                by_path: BTreeMap::new(),
            });
        entry.calls += g.calls;
        entry.errors += g.errors;
        // Sum first; divided below once every path is in.
        entry.avg += g.sum_ms;
        entry.by_path.insert(
            path.as_str().to_string(),
            PathTimingStats {
                calls: g.calls,
                errors: g.errors,
                avg: round1(g.sum_ms / g.calls as f64),
                p50,
                p95,
                p99,
            },
        );
        hook_samples
            .entry(hook.clone())
            .or_default()
            .extend(g.samples.iter().copied());
    }
    for (hook, stats) in by_hook.iter_mut() {
        stats.avg = round1(stats.avg / stats.calls as f64);
        let mut samples = hook_samples.remove(hook).unwrap_or_default();
        newest(&mut samples, sample_cap);
        let (p50, p95, p99) = percentiles(&samples);
        stats.p50 = p50;
        stats.p95 = p95;
        stats.p99 = p99;
    }
    HookStatsView {
        since,
        by_hook,
        skipped_lines,
    }
}

/// Delete rows older than `retention_days`.
pub async fn prune_hook_events(project_id: &str, retention_days: u64) -> Result<()> {
    if is_in_memory_only(project_id) {
        return Ok(());
    }
    let conn = connect_for(project_id).await?;
    let Some(table) = open_table(&conn).await else {
        return Ok(());
    };
    let Some(cutoff) = retention_cutoff(Some(retention_days), Utc::now()) else {
        return Ok(());
    };
    table
        .delete(&format!("ts < {}", ts_literal(cutoff)))
        .await
        .with_context(|| format!("pruning hook_events for project {project_id}"))?;
    Ok(())
}

/// Compact fragments and refresh the indexes. Best-effort, like
/// [`super::persistence::optimize_table`].
pub async fn optimize_hook_events(project_id: &str) -> Result<()> {
    if is_in_memory_only(project_id) {
        return Ok(());
    }
    let conn = connect_for(project_id).await?;
    let Some(table) = open_table(&conn).await else {
        return Ok(());
    };
    table
        .optimize(lancedb::table::OptimizeAction::All)
        .await
        .with_context(|| format!("optimizing hook_events for project {project_id}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_pid() -> String {
        static N: AtomicU64 = AtomicU64::new(0);
        format!("test-hookev-{:016x}", N.fetch_add(1, Ordering::Relaxed))
    }

    async fn project() -> String {
        let pid = unique_pid();
        let parent = super::super::persistence::lancedb_root(&pid)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        tokio::fs::create_dir_all(&parent).await.unwrap();
        pid
    }

    fn ev(id: &str, ts: DateTime<Utc>, hook: &str, ms: f64, ok: bool, path: HookPath) -> HookEvent {
        HookEvent {
            id: id.to_string(),
            ts,
            hook: hook.to_string(),
            duration_ms: ms,
            ok,
            path,
            session_id: Some("s".to_string()),
        }
    }

    #[tokio::test]
    async fn append_then_load_groups_by_hook_and_path() {
        let pid = project().await;
        let now = Utc::now();
        let events = vec![
            ev("a", now, "pre-tool-use", 10.0, true, HookPath::Daemon),
            ev("b", now, "pre-tool-use", 20.0, false, HookPath::Daemon),
            ev("c", now, "pre-tool-use", 30.0, true, HookPath::InProcess),
            ev("d", now, "session-end", 5.0, true, HookPath::InProcess),
        ];
        append_hook_events(&pid, &events, false).await.unwrap();
        let groups = load_groups(&pid, Some(90), PERCENTILE_SAMPLES)
            .await
            .unwrap();
        let d = &groups[&("pre-tool-use".to_string(), HookPath::Daemon)];
        assert_eq!((d.calls, d.errors), (2, 1));
        assert_eq!(d.sum_ms, 30.0);
        assert_eq!(d.samples.len(), 2);
        assert_eq!(groups.len(), 3);
    }

    #[tokio::test]
    async fn dedupe_append_ignores_known_ids() {
        let pid = project().await;
        let now = Utc::now();
        let first = vec![
            ev("x1", now, "pre-compact", 1.0, true, HookPath::InProcess),
            ev("x2", now, "pre-compact", 2.0, true, HookPath::InProcess),
        ];
        append_hook_events(&pid, &first, true).await.unwrap();
        // A re-drain of the same file plus one new line.
        let mut again = first.clone();
        again.push(ev("x3", now, "pre-compact", 3.0, true, HookPath::InProcess));
        append_hook_events(&pid, &again, true).await.unwrap();
        let groups = load_groups(&pid, None, PERCENTILE_SAMPLES).await.unwrap();
        assert_eq!(
            groups[&("pre-compact".to_string(), HookPath::InProcess)].calls,
            3
        );
    }

    #[tokio::test]
    async fn retention_window_and_sample_cap_apply() {
        let pid = project().await;
        let now = Utc::now();
        let mut events: Vec<HookEvent> = (0..50)
            .map(|i| {
                ev(
                    &format!("r{i}"),
                    now - chrono::Duration::seconds(i),
                    "pre-tool-use",
                    i as f64,
                    true,
                    HookPath::Daemon,
                )
            })
            .collect();
        events.push(ev(
            "old",
            now - chrono::Duration::days(200),
            "pre-tool-use",
            999.0,
            true,
            HookPath::Daemon,
        ));
        append_hook_events(&pid, &events, false).await.unwrap();
        let groups = load_groups(&pid, Some(90), 10).await.unwrap();
        let g = &groups[&("pre-tool-use".to_string(), HookPath::Daemon)];
        assert_eq!(g.calls, 50, "the 200-day-old row is outside the window");
        assert_eq!(g.samples.len(), 10, "samples are capped");
        // The newest 10 are durations 0..10.
        assert!(
            g.samples.iter().all(|(_, ms)| *ms < 10.0),
            "{:?}",
            g.samples
        );

        prune_hook_events(&pid, 90).await.unwrap();
        let all = load_groups(&pid, None, PERCENTILE_SAMPLES).await.unwrap();
        assert_eq!(
            all[&("pre-tool-use".to_string(), HookPath::Daemon)].calls,
            50
        );
        optimize_hook_events(&pid).await.unwrap();
    }

    #[tokio::test]
    async fn missing_table_or_store_reads_as_empty() {
        let pid = project().await;
        assert!(load_groups(&pid, Some(90), 10).await.unwrap().is_empty());
        // No store at all: reads empty, writes refuse.
        let ghost = unique_pid();
        assert!(load_groups(&ghost, Some(90), 10).await.unwrap().is_empty());
        let now = Utc::now();
        assert!(append_hook_events(
            &ghost,
            &[ev("g", now, "pre-compact", 1.0, true, HookPath::Daemon)],
            false
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn quotes_in_hook_names_do_not_break_queries() {
        let pid = project().await;
        let now = Utc::now();
        append_hook_events(
            &pid,
            &[ev("q", now, "it's", 1.0, true, HookPath::InProcess)],
            false,
        )
        .await
        .unwrap();
        let groups = load_groups(&pid, Some(90), 10).await.unwrap();
        assert_eq!(
            groups[&("it's".to_string(), HookPath::InProcess)]
                .samples
                .len(),
            1
        );
    }

    #[test]
    fn build_view_combines_paths_and_keeps_breakdown() {
        let now = Utc::now();
        let mut groups = Groups::new();
        let d = groups
            .entry(("pre-tool-use".to_string(), HookPath::Daemon))
            .or_default();
        for i in 1..=100 {
            d.add(now - chrono::Duration::seconds(i), i as f64, true);
        }
        let p = groups
            .entry(("pre-tool-use".to_string(), HookPath::InProcess))
            .or_default();
        p.add(now - chrono::Duration::days(1), 500.0, false);
        let view = build_view(groups, PERCENTILE_SAMPLES, 2);
        let h = &view.by_hook["pre-tool-use"];
        assert_eq!((h.calls, h.errors), (101, 1));
        assert_eq!(h.avg, round1((5050.0 + 500.0) / 101.0));
        assert_eq!(h.p99, 100.0);
        assert_eq!(h.by_path["daemon"].p50, 50.0);
        assert_eq!(h.by_path["in_process"].p99, 500.0);
        assert_eq!(view.since, Some(now - chrono::Duration::days(1)));
        assert_eq!(view.skipped_lines, 2);
    }

    #[test]
    fn build_view_percentiles_use_only_the_newest_samples() {
        let now = Utc::now();
        let mut groups = Groups::new();
        let g = groups
            .entry(("pre-compact".to_string(), HookPath::InProcess))
            .or_default();
        // 5 old slow samples, then 5 new fast ones; cap 5 keeps the fast ones.
        for i in 0..5 {
            g.add(now - chrono::Duration::hours(10 + i), 1000.0, true);
        }
        for i in 0..5 {
            g.add(now - chrono::Duration::seconds(i), 1.0, true);
        }
        let view = build_view(groups, 5, 0);
        let h = &view.by_hook["pre-compact"];
        assert_eq!(h.calls, 10, "counts are not capped");
        assert_eq!(h.p99, 1.0, "percentiles use the newest 5 only");
    }
}
