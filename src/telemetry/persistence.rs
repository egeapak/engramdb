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
//! | column            | type     | notes                                    |
//! |-------------------|----------|------------------------------------------|
//! | `ts`              | Utf8     | RFC3339 timestamp                        |
//! | `event_type`      | Utf8     | `tool_call` / `stage` / `query_outcome`  |
//! | `tool`            | Utf8     | tool name (`tool_call` only)             |
//! | `stage`           | Utf8     | stage name (`stage` only)                |
//! | `duration_ms`     | Float64  | nullable; absent for `query_outcome`     |
//! | `success`         | Boolean  | nullable; `tool_call` only               |
//! | `hit`             | Boolean  | nullable; `query_outcome` only           |
//! | `retrieval_quality` | Utf8   | nullable; `query_outcome` only           |
//! | `session_id`      | Utf8     | per-process UUID or `Mcp-Session-Id`     |
//!
//! ## Hydration
//!
//! On server startup we walk every project's LanceDB dir under
//! `<global_data_dir>/projects/*/lancedb`, open the `stats_events` table
//! if it exists, and read up to `STARTUP_REPLAY_CAP` (default 50,000)
//! most-recent rows. Each row is replayed into the in-memory
//! [`StatsCollector`], rebuilding both lifetime counters and the
//! percentile ring buffers.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use arrow_array::{
    Array, ArrayRef, BooleanArray, Float64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use chrono::{DateTime, Utc};
use futures_util::stream::StreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
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

async fn connect_for(project_id: &str) -> Result<Connection> {
    let dir = lancedb_root(project_id)?;
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    let path_str = dir
        .to_str()
        .context("lancedb path is not valid UTF-8")?
        .to_string();
    connect(&path_str)
        .execute()
        .await
        .context("opening LanceDB connection for stats_events")
}

fn events_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("ts", DataType::Utf8, false),
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
        Err(_) => conn
            .create_empty_table(TABLE_NAME, events_schema())
            .execute()
            .await
            .context("creating stats_events table"),
    }
}

/// Append a batch of events to the project's `stats_events` table. Errors
/// are logged at warn level and never propagated — telemetry writes must
/// never break a tool call.
pub async fn append_events(project_id: &str, events: &[EventRow]) {
    if events.is_empty() {
        return;
    }
    if let Err(e) = append_events_inner(project_id, events).await {
        tracing::warn!(
            "stats: failed to append {} events for project {}: {}",
            events.len(),
            project_id,
            e
        );
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
    let mut ts: Vec<String> = Vec::with_capacity(events.len());
    let mut event_type: Vec<String> = Vec::with_capacity(events.len());
    let mut tool: Vec<Option<String>> = Vec::with_capacity(events.len());
    let mut stage: Vec<Option<String>> = Vec::with_capacity(events.len());
    let mut duration_ms: Vec<Option<f64>> = Vec::with_capacity(events.len());
    let mut success: Vec<Option<bool>> = Vec::with_capacity(events.len());
    let mut hit: Vec<Option<bool>> = Vec::with_capacity(events.len());
    let mut retrieval_quality: Vec<Option<String>> = Vec::with_capacity(events.len());
    let mut session_id: Vec<Option<String>> = Vec::with_capacity(events.len());

    for ev in events {
        ts.push(ev.ts.to_rfc3339());
        event_type.push(ev.event_type.as_str().to_string());
        tool.push(ev.tool.clone());
        stage.push(ev.stage.clone());
        duration_ms.push(ev.duration_ms);
        success.push(ev.success);
        hit.push(ev.hit);
        retrieval_quality.push(ev.retrieval_quality.clone());
        session_id.push(ev.session_id.clone());
    }

    let arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(ts)),
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

/// Read up to `cap` rows for the given project, sorted by `ts` desc, returning
/// them in original insertion order (oldest first) so replays stay
/// chronological.
pub async fn load_recent(project_id: &str, cap: usize) -> Result<Vec<EventRow>> {
    let conn = connect_for(project_id).await?;
    let table = match conn.open_table(TABLE_NAME).execute().await {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };

    let limit = cap.max(1);
    let mut stream = table
        .query()
        .limit(limit)
        .execute()
        .await
        .context("querying stats_events table")?;

    let mut events = Vec::new();
    while let Some(batch_result) = stream.next().await {
        let batch = batch_result.context("reading stats_events batch")?;
        events.extend(batch_to_events(&batch)?);
    }
    // LanceDB returns rows in storage order. Sort chronologically; ties
    // preserve the original order via the stable sort.
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
        .downcast_ref::<StringArray>()
        .context("ts column is not Utf8")?;
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
        let ts_str = ts.value(i);
        let parsed_ts = DateTime::parse_from_rfc3339(ts_str)
            .with_context(|| format!("invalid ts {ts_str}"))?
            .with_timezone(&Utc);
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
    let conn = connect_for(project_id).await?;
    let table = match conn.open_table(TABLE_NAME).execute().await {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let cutoff = Utc::now() - chrono::Duration::days(retention_days as i64);
    let predicate = format!("ts < '{}'", cutoff.to_rfc3339());
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
pub fn spawn_flush_task(
    rx: mpsc::UnboundedReceiver<(String /*project_id*/, EventRow)>,
    flush_interval_secs: u64,
    retention_days: Option<u64>,
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

        loop {
            tokio::select! {
                msg = rx.recv() => {
                    match msg {
                        Some((pid, ev)) => {
                            let entry = buf.entry(pid).or_default();
                            entry.push(ev);
                            // Cap per-project buffers — flush early under burst load.
                            if entry.len() >= FLUSH_BATCH_MAX {
                                let pid = entry_owner(&buf, FLUSH_BATCH_MAX).unwrap_or_default();
                                if !pid.is_empty() {
                                    if let Some(events) = buf.remove(&pid) {
                                        append_events(&pid, &events).await;
                                    }
                                }
                            }
                        }
                        None => {
                            // Channel closed — final drain and exit.
                            for (pid, events) in buf.drain() {
                                append_events(&pid, &events).await;
                            }
                            return;
                        }
                    }
                }
                _ = tick.tick() => {
                    let drained: Vec<(String, Vec<EventRow>)> = buf.drain().collect();
                    for (pid, events) in &drained {
                        append_events(pid, events).await;
                    }
                    // Run pruning every ~10 ticks so we don't hammer LanceDB
                    // with deletes.
                    prune_counter += 1;
                    if prune_counter.is_multiple_of(10) {
                        if let Some(days) = retention_days {
                            for (pid, _) in &drained {
                                if let Err(e) = prune_older_than(pid, days).await {
                                    tracing::warn!("stats prune for {pid}: {e}");
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

fn entry_owner(
    buf: &std::collections::HashMap<String, Vec<EventRow>>,
    threshold: usize,
) -> Option<String> {
    buf.iter()
        .find(|(_, v)| v.len() >= threshold)
        .map(|(k, _)| k.clone())
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
        append_events(&pid, &events).await;

        let read = load_recent(&pid, 100).await.unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].tool.as_deref(), Some("query"));
        assert_eq!(read[1].tool.as_deref(), Some("create"));
        assert_eq!(read[1].session_id.as_deref(), Some("sess-A"));
    }

    #[tokio::test]
    async fn load_recent_empty_when_no_table() {
        let pid = unique_pid("empty");
        let read = load_recent(&pid, 10).await.unwrap();
        assert!(read.is_empty());
    }

    #[tokio::test]
    async fn hydrate_replays_into_collector() {
        let pid = unique_pid("hyd");
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
        append_events(&pid, &events).await;

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
        let now = Utc::now();
        let events = vec![
            tool_event(now - chrono::Duration::days(40), "query", 1.0, true, "old"),
            tool_event(now, "query", 2.0, true, "new"),
        ];
        append_events(&pid, &events).await;

        prune_older_than(&pid, 30).await.unwrap();

        let read = load_recent(&pid, 100).await.unwrap();
        assert_eq!(read.len(), 1, "stale row pruned");
        assert_eq!(read[0].session_id.as_deref(), Some("new"));
    }
}
