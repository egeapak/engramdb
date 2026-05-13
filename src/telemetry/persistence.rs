//! LanceDB-backed event log for runtime telemetry.
//!
//! Each project has a `stats_events` table stored alongside the project's
//! existing LanceDB tables (memories, chunks). Rows are appended on every
//! tool call, stage timing, and query outcome — the in-memory
//! [`StatsCollector`] writes to LanceDB asynchronously through an mpsc
//! channel so the hot path stays I/O-free.
//!
//! ## Why LanceDB and not a JSON snapshot
//!
//! - LanceDB is already a project dependency and its per-project directory
//!   layout (`<global_data_dir>/projects/{id}/lancedb/`) is the natural
//!   home for any per-project columnar data.
//! - Append-batched Arrow writes match the workload (small rows,
//!   high-frequency). No JSON re-serialization on every flush.
//! - Queryable: future debug commands can `SELECT … FROM stats_events
//!   WHERE …` directly.
//!
//! ## On-disk schema
//!
//! Single table `stats_events` per project, columns:
//!
//! | column            | type                          | notes                                    |
//! |-------------------|-------------------------------|------------------------------------------|
//! | `ts`              | Timestamp(Microsecond, UTC)   | indexed (BTree)                          |
//! | `event_type`      | Utf8                          | `tool_call` / `stage` / `query_outcome`  |
//! | `tool`            | Utf8                          | tool name (`tool_call` only)             |
//! | `stage`           | Utf8                          | stage name (`stage` only)                |
//! | `duration_ms`     | Float64                       | nullable; absent for `query_outcome`     |
//! | `success`         | Boolean                       | nullable; `tool_call` only               |
//! | `hit`             | Boolean                       | nullable; `query_outcome` only           |
//! | `retrieval_quality` | Utf8                        | nullable; `query_outcome` only           |
//! | `session_id`      | Utf8                          | per-process UUID or `Mcp-Session-Id`     |
//!
//! ## Hydration
//!
//! On server startup we walk every project's LanceDB dir under
//! `<global_data_dir>/projects/*/lancedb`, open the `stats_events` table
//! if it exists, and read up to `STARTUP_REPLAY_CAP` (default 50,000)
//! most-recent rows. Each row is replayed into the in-memory
//! [`StatsCollector`], rebuilding both lifetime counters and the
//! percentile ring buffers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use arrow_array::{
    Array, ArrayRef, BooleanArray, Float64Array, RecordBatch, RecordBatchIterator, StringArray,
    TimestampMicrosecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use chrono::{DateTime, Utc};
use futures_util::stream::StreamExt;
use lancedb::query::ExecutableQuery;
use lancedb::{connect, Connection, Table};
use tokio::sync::mpsc;

use crate::storage::paths::{global_data_dir, global_lancedb_dir, lancedb_dir, GLOBAL_PROJECT_ID};
use crate::telemetry::collector::{EventRow, EventType, StatsCollector};

const TABLE_NAME: &str = "stats_events";

/// Cap on rows replayed at startup, per project. Keeps hydration fast for
/// projects with very long event histories. Older rows are still in the
/// table — pruning honors `retention_days`.
const STARTUP_REPLAY_CAP: usize = 50_000;

/// Cap on events drained per flush cycle. Bounds memory under burst load;
/// the channel is unbounded so excess events stay queued for the next tick.
const FLUSH_BATCH_MAX: usize = 1_024;

/// Resolve the LanceDB connection root for a project ID.
fn lancedb_root(project_id: &str) -> Result<PathBuf> {
    if project_id == GLOBAL_PROJECT_ID {
        global_lancedb_dir().context("resolving global lancedb dir")
    } else {
        lancedb_dir(project_id).context("resolving project lancedb dir")
    }
}

/// Cache of LanceDB connections keyed by project_id. LanceDB connections
/// are cheap once open (Arc-shared internally) but each `connect()` call
/// re-reads `_versions/` metadata, which adds up under burst flush.
///
/// `Arc<Connection>` lets us hand out clones to callers; entries are
/// never evicted (a server typically touches a small set of project_ids).
static CONN_CACHE: LazyLock<Mutex<HashMap<String, Arc<Connection>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Open or create the LanceDB connection for a project. Refuses to create
/// a phantom dir for project IDs that don't have an initialized store —
/// the parent path is created by `MemoryStore::init`, so its absence is a
/// signal that this project_id came from a typo or unregistered path
/// override and we shouldn't silently create scaffolding for it.
async fn connect_for(project_id: &str) -> Result<Arc<Connection>> {
    {
        let cache = CONN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(conn) = cache.get(project_id) {
            return Ok(conn.clone());
        }
    }

    let dir = lancedb_root(project_id)?;
    let parent = dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("lancedb dir {} has no parent", dir.display()))?;
    if !tokio::fs::try_exists(parent).await.unwrap_or(false) {
        anyhow::bail!(
            "stats: project {} has no initialized store at {} — refusing to create phantom telemetry dir",
            project_id,
            parent.display()
        );
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    let path_str = dir
        .to_str()
        .context("lancedb path is not valid UTF-8")?
        .to_string();
    let conn = Arc::new(
        connect(&path_str)
            .execute()
            .await
            .context("opening LanceDB connection for stats_events")?,
    );

    let mut cache = CONN_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    Ok(cache.entry(project_id.to_string()).or_insert(conn).clone())
}

fn events_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        // `ts` is a real timestamp column so that `prune_older_than` can use
        // a typed comparison and so a BTree scalar index makes both pruning
        // and recency reads cheap. Stored as microseconds-since-epoch in
        // UTC (chrono::DateTime<Utc> serializes losslessly within
        // microsecond precision).
        Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("event_type", DataType::Utf8, false),
        Field::new("tool", DataType::Utf8, true),
        Field::new("stage", DataType::Utf8, true),
        Field::new("duration_ms", DataType::Float64, true),
        Field::new("success", DataType::Boolean, true),
        Field::new("hit", DataType::Boolean, true),
        Field::new("retrieval_quality", DataType::Utf8, true),
        Field::new("session_id", DataType::Utf8, true),
    ]))
}

async fn ensure_table(conn: &Connection) -> Result<Table> {
    match conn.open_table(TABLE_NAME).execute().await {
        Ok(t) => Ok(t),
        Err(_) => {
            let table = conn
                .create_empty_table(TABLE_NAME, events_schema())
                .execute()
                .await
                .context("creating stats_events table")?;
            // BTree on `ts` makes pruning and recency reads cheap; without
            // it both operations require a full table scan.
            if let Err(e) = table
                .create_index(&["ts"], lancedb::index::Index::BTree(Default::default()))
                .execute()
                .await
            {
                tracing::warn!("stats: failed to create BTree index on ts: {e}");
            }
            Ok(table)
        }
    }
}

/// Synthetic project IDs that should never hit disk. Mirrors the
/// `SYSTEM_PROJECT_ID` constant in `mcp::server`.
const IN_MEMORY_ONLY_IDS: &[&str] = &["__system__"];

fn is_in_memory_only(project_id: &str) -> bool {
    IN_MEMORY_ONLY_IDS.contains(&project_id)
}

/// Append a batch of events to the project's `stats_events` table. Errors
/// are logged at warn level and never propagated — telemetry writes must
/// never break a tool call. The optional collector ref is bumped on
/// failure so operators can see persistence failures in the snapshot.
pub async fn append_events(
    project_id: &str,
    events: &[EventRow],
    collector: Option<&Arc<StatsCollector>>,
) {
    if events.is_empty() || is_in_memory_only(project_id) {
        return;
    }
    if let Err(e) = append_events_inner(project_id, events).await {
        tracing::warn!(
            "stats: failed to append {} events for project {}: {}",
            events.len(),
            project_id,
            e
        );
        if let Some(c) = collector {
            c.record_persistence_failure();
        }
    }
}

async fn append_events_inner(project_id: &str, events: &[EventRow]) -> Result<()> {
    let conn = connect_for(project_id).await?;
    let table = ensure_table(&conn).await?;
    let batch = events_to_batch(events)?;
    let schema = batch.schema();
    let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
    table
        .add(Box::new(batches))
        .execute()
        .await
        .context("appending stats_events rows")?;
    Ok(())
}

fn events_to_batch(events: &[EventRow]) -> Result<RecordBatch> {
    let schema = events_schema();
    let mut ts: Vec<i64> = Vec::with_capacity(events.len());
    let mut event_type: Vec<String> = Vec::with_capacity(events.len());
    let mut tool: Vec<Option<String>> = Vec::with_capacity(events.len());
    let mut stage: Vec<Option<String>> = Vec::with_capacity(events.len());
    let mut duration_ms: Vec<Option<f64>> = Vec::with_capacity(events.len());
    let mut success: Vec<Option<bool>> = Vec::with_capacity(events.len());
    let mut hit: Vec<Option<bool>> = Vec::with_capacity(events.len());
    let mut retrieval_quality: Vec<Option<String>> = Vec::with_capacity(events.len());
    let mut session_id: Vec<Option<String>> = Vec::with_capacity(events.len());

    for ev in events {
        ts.push(ev.ts.timestamp_micros());
        event_type.push(ev.event_type.as_str().to_string());
        tool.push(ev.tool.clone());
        stage.push(ev.stage.clone());
        duration_ms.push(ev.duration_ms);
        success.push(ev.success);
        hit.push(ev.hit);
        retrieval_quality.push(ev.retrieval_quality.clone());
        session_id.push(ev.session_id.clone());
    }

    let ts_array = TimestampMicrosecondArray::from(ts).with_timezone(Arc::<str>::from("UTC"));
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(ts_array),
        Arc::new(StringArray::from(event_type)),
        Arc::new(StringArray::from(tool)),
        Arc::new(StringArray::from(stage)),
        Arc::new(Float64Array::from(duration_ms)),
        Arc::new(BooleanArray::from(success)),
        Arc::new(BooleanArray::from(hit)),
        Arc::new(StringArray::from(retrieval_quality)),
        Arc::new(StringArray::from(session_id)),
    ];

    RecordBatch::try_new(schema, arrays).context("building stats_events RecordBatch")
}

/// Read up to `cap` most-recent rows for the given project, returned in
/// chronological order (oldest first) for safe replay.
///
/// LanceDB's plain `query().limit(N)` returns rows in **storage order** with
/// no recency guarantee, so we must read every row and then truncate after
/// sorting. For typical workloads (≤ a few hundred thousand events) the
/// scan is fast; pruning + retention bound the cost in steady state.
///
/// Returns `Ok(empty)` when no data exists for the project, including the
/// case where the project's parent directory hasn't been initialized.
pub async fn load_recent(project_id: &str, cap: usize) -> Result<Vec<EventRow>> {
    if is_in_memory_only(project_id) {
        return Ok(Vec::new());
    }
    let conn = match connect_for(project_id).await {
        Ok(c) => c,
        // Parent dir doesn't exist (uninitialized project) or other open
        // failure — treat as "no data yet" rather than propagating.
        Err(_) => return Ok(Vec::new()),
    };
    let table = match conn.open_table(TABLE_NAME).execute().await {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };

    let mut stream = table
        .query()
        .execute()
        .await
        .context("querying stats_events table")?;

    let mut events = Vec::new();
    while let Some(batch_result) = stream.next().await {
        let batch = batch_result.context("reading stats_events batch")?;
        events.extend(batch_to_events(&batch)?);
    }
    // Sort chronologically (stable sort preserves insertion order for
    // identical timestamps).
    events.sort_by(|a, b| a.ts.cmp(&b.ts));
    if events.len() > cap {
        let tail_start = events.len() - cap;
        events.drain(0..tail_start);
    }
    Ok(events)
}

fn batch_to_events(batch: &RecordBatch) -> Result<Vec<EventRow>> {
    let ts = batch
        .column_by_name("ts")
        .context("missing ts column")?
        .as_any()
        .downcast_ref::<TimestampMicrosecondArray>()
        .context("ts column is not Timestamp(Microsecond)")?;
    let event_type = batch
        .column_by_name("event_type")
        .context("missing event_type column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("event_type column is not Utf8")?;
    let tool = batch
        .column_by_name("tool")
        .context("missing tool column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("tool column is not Utf8")?;
    let stage = batch
        .column_by_name("stage")
        .context("missing stage column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("stage column is not Utf8")?;
    let duration_ms = batch
        .column_by_name("duration_ms")
        .context("missing duration_ms column")?
        .as_any()
        .downcast_ref::<Float64Array>()
        .context("duration_ms column is not Float64")?;
    let success = batch
        .column_by_name("success")
        .context("missing success column")?
        .as_any()
        .downcast_ref::<BooleanArray>()
        .context("success column is not Boolean")?;
    let hit = batch
        .column_by_name("hit")
        .context("missing hit column")?
        .as_any()
        .downcast_ref::<BooleanArray>()
        .context("hit column is not Boolean")?;
    let retrieval_quality = batch
        .column_by_name("retrieval_quality")
        .context("missing retrieval_quality column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("retrieval_quality column is not Utf8")?;
    let session_id = batch
        .column_by_name("session_id")
        .context("missing session_id column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("session_id column is not Utf8")?;

    let mut rows = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        let micros = ts.value(i);
        let parsed_ts = DateTime::<Utc>::from_timestamp_micros(micros)
            .with_context(|| format!("invalid ts microseconds: {micros}"))?;
        let event_type = EventType::parse_label(event_type.value(i)).unwrap_or(EventType::ToolCall);
        rows.push(EventRow {
            ts: parsed_ts,
            event_type,
            tool: nullable_str(tool, i),
            stage: nullable_str(stage, i),
            duration_ms: nullable_f64(duration_ms, i),
            success: nullable_bool(success, i),
            hit: nullable_bool(hit, i),
            retrieval_quality: nullable_str(retrieval_quality, i),
            session_id: nullable_str(session_id, i),
        });
    }
    Ok(rows)
}

fn nullable_str(arr: &StringArray, i: usize) -> Option<String> {
    if arr.is_null(i) {
        None
    } else {
        Some(arr.value(i).to_string())
    }
}

fn nullable_f64(arr: &Float64Array, i: usize) -> Option<f64> {
    if arr.is_null(i) {
        None
    } else {
        Some(arr.value(i))
    }
}

fn nullable_bool(arr: &BooleanArray, i: usize) -> Option<bool> {
    if arr.is_null(i) {
        None
    } else {
        Some(arr.value(i))
    }
}

/// Delete events older than `retention_days`. No-op when `retention_days`
/// is `None` (the default) — counters cover the entire recorded history.
pub async fn prune_older_than(project_id: &str, retention_days: u64) -> Result<()> {
    if is_in_memory_only(project_id) {
        return Ok(());
    }
    let conn = connect_for(project_id).await?;
    let table = match conn.open_table(TABLE_NAME).execute().await {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
    // Use a typed timestamp comparison so the BTree index can serve the
    // predicate. DataFusion accepts `TIMESTAMP '<rfc3339>'` literals.
    let predicate = format!(
        "ts < TIMESTAMP '{}'",
        cutoff.format("%Y-%m-%d %H:%M:%S%.6f%:z")
    );
    table
        .delete(&predicate)
        .await
        .with_context(|| format!("pruning stats_events for project {project_id}"))?;
    Ok(())
}

/// Walk every project under `<global_data_dir>/projects/*/` plus the global
/// store, replaying recent events into the collector.
pub async fn hydrate_collector(collector: &Arc<StatsCollector>) -> Result<()> {
    let projects_root = global_data_dir()
        .context("resolving global data dir")?
        .join("projects");

    let mut project_ids: Vec<String> = Vec::new();
    if tokio::fs::try_exists(&projects_root).await.unwrap_or(false) {
        let mut rd = tokio::fs::read_dir(&projects_root)
            .await
            .with_context(|| format!("reading {}", projects_root.display()))?;
        while let Some(entry) = rd.next_entry().await.transpose() {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("stats hydrate: dir entry error: {e}");
                    continue;
                }
            };
            // Skip stray non-directory entries and dirs that don't look
            // like initialized project stores (no `lancedb/` subdirectory).
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if !path.join("lancedb").is_dir() {
                continue;
            }
            project_ids.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    project_ids.push(GLOBAL_PROJECT_ID.to_string());

    for pid in project_ids {
        match load_recent(&pid, STARTUP_REPLAY_CAP).await {
            Ok(events) if !events.is_empty() => {
                collector.replay_events(&pid, &events);
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("stats hydrate: load_recent for {pid} failed: {e}");
            }
        }
    }
    Ok(())
}

/// Spawn a background task that drains the collector's event channel and
/// flushes batches into the per-project `stats_events` tables. The task
/// exits cleanly when the collector is dropped (the receiver returns
/// `None`).
///
/// Concurrency model:
/// - The recv loop never `await`s a LanceDB write directly. When a
///   per-project buffer hits `FLUSH_BATCH_MAX`, the loop spawns a sub-task
///   to do the append and goes back to draining the channel. This keeps
///   the unbounded mpsc from accumulating events while a single slow
///   write is in flight.
/// - The interval-tick branch also spawns sub-tasks for each per-project
///   batch, in parallel.
/// - On channel close (collector dropped), the loop awaits all in-flight
///   sub-tasks before exiting so events written just before shutdown
///   land on disk.
pub fn spawn_flush_task(
    rx: mpsc::UnboundedReceiver<(String /*project_id*/, EventRow)>,
    flush_interval_secs: u64,
    retention_days: Option<u64>,
    collector: std::sync::Weak<StatsCollector>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = rx;
        let mut tick = tokio::time::interval(Duration::from_secs(flush_interval_secs.max(1)));
        // Skip the first immediate tick so we don't flush an empty buffer
        // right after startup.
        tick.tick().await;

        let mut buf: std::collections::HashMap<String, Vec<EventRow>> =
            std::collections::HashMap::new();
        let mut prune_counter: u64 = 0;
        let mut in_flight: Vec<tokio::task::JoinHandle<()>> = Vec::new();

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some((pid, ev)) => {
                            let entry = buf.entry(pid.clone()).or_default();
                            entry.push(ev);
                            if entry.len() >= FLUSH_BATCH_MAX {
                                // Flush exactly the project that just tripped
                                // the cap — not whichever happens to come
                                // first in the iteration order.
                                let events = std::mem::take(entry);
                                buf.remove(&pid);
                                in_flight.push(spawn_append(&pid, events, collector.clone()));
                            }
                        }
                        None => {
                            // Channel closed — final drain.
                            for (pid, events) in buf.drain() {
                                in_flight.push(spawn_append(&pid, events, collector.clone()));
                            }
                            // Wait for every in-flight write so events from
                            // this session actually land on disk.
                            for h in in_flight.drain(..) {
                                let _ = h.await;
                            }
                            return;
                        }
                    }
                }
                _ = tick.tick() => {
                    let drained: Vec<(String, Vec<EventRow>)> = buf.drain().collect();
                    let prune_pids: Vec<String> = drained.iter().map(|(p, _)| p.clone()).collect();
                    for (pid, events) in drained {
                        in_flight.push(spawn_append(&pid, events, collector.clone()));
                    }
                    // Run pruning + optimize every ~10 ticks so we don't
                    // hammer LanceDB with maintenance work.
                    prune_counter += 1;
                    if prune_counter.is_multiple_of(10) {
                        if let Some(days) = retention_days {
                            for pid in &prune_pids {
                                if let Err(e) = prune_older_than(pid, days).await {
                                    tracing::warn!("stats prune for {pid}: {e}");
                                }
                            }
                        }
                        // Compact tiny Lance fragments to keep `open_table`
                        // fast over the long haul.
                        for pid in &prune_pids {
                            if let Err(e) = optimize_table(pid).await {
                                tracing::debug!("stats optimize for {pid}: {e}");
                            }
                        }
                    }
                    // Reap any finished in-flight tasks so the vec doesn't
                    // grow unboundedly.
                    in_flight.retain(|h| !h.is_finished());
                }
            }
        }
    })
}

fn spawn_append(
    pid: &str,
    events: Vec<EventRow>,
    collector: std::sync::Weak<StatsCollector>,
) -> tokio::task::JoinHandle<()> {
    let pid = pid.to_string();
    tokio::spawn(async move {
        let c = collector.upgrade();
        append_events(&pid, &events, c.as_ref()).await;
    })
}

/// Run `Table::optimize()` to compact small Lance fragments. Idempotent and
/// safe to call concurrently with appends — Lance handles concurrent
/// versioning. Errors are logged at debug level only (compaction is an
/// optimization, not a correctness requirement).
async fn optimize_table(project_id: &str) -> Result<()> {
    if is_in_memory_only(project_id) {
        return Ok(());
    }
    let conn = connect_for(project_id).await?;
    let table = match conn.open_table(TABLE_NAME).execute().await {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    table
        .optimize(lancedb::table::OptimizeAction::All)
        .await
        .with_context(|| format!("optimizing stats_events for project {project_id}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::collector::{EventRow, EventType, StatsCollector};
    use crate::types::config::StatsConfig;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_pid(prefix: &str) -> String {
        static N: AtomicU64 = AtomicU64::new(0);
        format!("test-{}-{:016x}", prefix, N.fetch_add(1, Ordering::Relaxed))
    }

    /// Tests use synthetic project IDs without going through `MemoryStore::init`,
    /// so we have to pre-create the project directory ourselves — `connect_for`
    /// refuses to create LanceDB scaffolding under a parent that doesn't exist.
    async fn ensure_project_dir(pid: &str) {
        let parent = lancedb_root(pid).unwrap().parent().unwrap().to_path_buf();
        tokio::fs::create_dir_all(&parent).await.unwrap();
    }

    fn tool_event(ts: DateTime<Utc>, tool: &str, ms: f64, success: bool, sid: &str) -> EventRow {
        EventRow {
            ts,
            event_type: EventType::ToolCall,
            tool: Some(tool.to_string()),
            stage: None,
            duration_ms: Some(ms),
            success: Some(success),
            hit: None,
            retrieval_quality: None,
            session_id: Some(sid.to_string()),
        }
    }

    #[tokio::test]
    async fn append_then_load_roundtrip() {
        let pid = unique_pid("rt");
        ensure_project_dir(&pid).await;
        let now = Utc::now();
        let events = vec![
            tool_event(now, "query", 12.5, true, "sess-A"),
            tool_event(
                now + chrono::Duration::seconds(1),
                "create",
                22.0,
                true,
                "sess-A",
            ),
        ];
        append_events(&pid, &events, None).await;

        let read = load_recent(&pid, 100).await.unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].tool.as_deref(), Some("query"));
        assert_eq!(read[1].tool.as_deref(), Some("create"));
        assert_eq!(read[1].session_id.as_deref(), Some("sess-A"));
    }

    #[tokio::test]
    async fn load_recent_empty_when_no_table() {
        let pid = unique_pid("empty");
        ensure_project_dir(&pid).await;
        let read = load_recent(&pid, 10).await.unwrap();
        assert!(read.is_empty());
    }

    #[tokio::test]
    async fn hydrate_replays_into_collector() {
        let pid = unique_pid("hyd");
        ensure_project_dir(&pid).await;
        let now = Utc::now();
        let events = vec![
            tool_event(now, "query", 10.0, true, "sess-X"),
            tool_event(
                now + chrono::Duration::milliseconds(50),
                "query",
                20.0,
                true,
                "sess-X",
            ),
        ];
        append_events(&pid, &events, None).await;

        let collector = StatsCollector::new(StatsConfig::default());
        let recent = load_recent(&pid, 100).await.unwrap();
        collector.replay_events(&pid, &recent);

        let snap = collector.snapshot(&pid, false);
        assert_eq!(snap.view.usage.total_calls, 2);
        let q = snap.view.timings_ms.tool.get("query").unwrap();
        assert_eq!(q.count, 2);
    }

    #[tokio::test]
    async fn prune_drops_old_rows_when_retention_set() {
        let pid = unique_pid("prune");
        ensure_project_dir(&pid).await;
        let now = Utc::now();
        let events = vec![
            tool_event(now - chrono::Duration::days(40), "query", 1.0, true, "old"),
            tool_event(now, "query", 2.0, true, "new"),
        ];
        append_events(&pid, &events, None).await;

        prune_older_than(&pid, 30).await.unwrap();

        let read = load_recent(&pid, 100).await.unwrap();
        assert_eq!(read.len(), 1, "stale row pruned");
        assert_eq!(read[0].session_id.as_deref(), Some("new"));
    }

    /// R4 regression: project IDs without an initialized parent directory
    /// must not trigger creation of phantom telemetry scaffolding.
    #[tokio::test]
    async fn append_skipped_for_uninitialized_project() {
        let pid = unique_pid("ghost"); // intentionally NOT calling ensure_project_dir
        let now = Utc::now();
        let events = vec![tool_event(now, "query", 1.0, true, "g")];
        append_events(&pid, &events, None).await;

        // No table created, no events written.
        let read = load_recent(&pid, 10).await.unwrap();
        assert!(read.is_empty(), "phantom dir suppressed");
    }

    /// End-to-end: events emitted via the collector flow through the
    /// channel, the flush task drains and persists them, and a fresh
    /// collector hydrating from disk sees the same counters. Exercises
    /// the full StatsCollector → mpsc → spawn_flush_task → LanceDB →
    /// hydrate_collector pipeline.
    #[tokio::test]
    async fn flush_task_persists_events_end_to_end() {
        let pid = unique_pid("e2e");
        ensure_project_dir(&pid).await;

        // Build a collector and drive a flush task with a tight tick
        // interval so the test doesn't have to wait long.
        let config = StatsConfig {
            flush_interval_secs: 1,
            ..StatsConfig::default()
        };
        let collector = StatsCollector::new(config.clone());
        let rx = collector.take_receiver().unwrap();
        let handle = spawn_flush_task(
            rx,
            config.flush_interval_secs,
            config.retention_days,
            Arc::downgrade(&collector),
        );

        // Record a few events under the project.
        collector.record_tool_call(&pid, "query", 12.0, true, Some("S"));
        collector.record_tool_call(&pid, "create", 22.0, true, Some("S"));
        collector.record_query_outcome(&pid, true, "full", Some("S"));

        // Drop the collector so the flush task drains and exits cleanly.
        drop(collector);
        let _ = handle.await;

        // Hydrate a fresh collector from disk and assert the event log
        // contains everything we just emitted.
        let fresh = StatsCollector::new(StatsConfig::default());
        let recent = load_recent(&pid, 1024).await.unwrap();
        assert!(
            recent.len() >= 3,
            "expected at least 3 events on disk, got {}",
            recent.len()
        );
        fresh.replay_events(&pid, &recent);

        let snap = fresh.snapshot(&pid, false);
        assert_eq!(snap.view.usage.total_calls, 2);
        assert_eq!(snap.view.queries.total, 1);
        assert_eq!(snap.view.queries.hits, 1);
        assert_eq!(snap.view.usage.unique_sessions, 1);
    }
}
