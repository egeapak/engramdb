//! Daemon request metrics, persisted to the global LanceDB store.
//!
//! Counters are cumulative across daemon restarts: at startup the daemon seeds
//! its in-memory atomics from the most recent persisted snapshot, and it
//! appends a fresh snapshot row periodically and on graceful shutdown. The
//! data lives in a `daemon_metrics` table inside the *global* store's LanceDB
//! directory — the same place global-project memories live — so `engramdb
//! stats --daemon` can report figures even when no daemon is currently running.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{
    Array, ArrayRef, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
    TimestampMicrosecondArray,
};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use chrono::Utc;
use futures_util::stream::StreamExt;
use lancedb::connect;
use lancedb::query::ExecutableQuery;

use crate::storage::paths::global_lancedb_dir;

const TABLE_NAME: &str = "daemon_metrics";

/// Snapshot rows older than this are deleted on every persist. The newest
/// row is all that's ever read back (counters are cumulative), so 30 days of
/// history is generous headroom for debugging while keeping the table — which
/// otherwise gains a row every 300 s plus one per daemon exit — bounded
/// (~8.6 k rows max at the periodic cadence).
const SNAPSHOT_RETENTION_DAYS: i64 = 30;

/// Live per-op request counters. Seeded from the last persisted snapshot at
/// daemon startup so totals are cumulative across daemon restarts.
#[derive(Debug, Default)]
pub struct Counters {
    embed: AtomicU64,
    classify: AtomicU64,
    rerank: AtomicU64,
    meta: AtomicU64,
    status: AtomicU64,
    title: AtomicU64,
}

/// A point-in-time view of the cumulative counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct MetricsSnapshot {
    pub embed: u64,
    pub classify: u64,
    pub rerank: u64,
    pub meta: u64,
    pub status: u64,
    pub title: u64,
}

impl MetricsSnapshot {
    pub fn total(&self) -> u64 {
        self.embed + self.classify + self.rerank + self.meta + self.status + self.title
    }
}

impl Counters {
    /// Build counters pre-loaded with a persisted baseline so totals continue
    /// across restarts.
    pub fn seeded(base: MetricsSnapshot) -> Self {
        Self {
            embed: AtomicU64::new(base.embed),
            classify: AtomicU64::new(base.classify),
            rerank: AtomicU64::new(base.rerank),
            meta: AtomicU64::new(base.meta),
            status: AtomicU64::new(base.status),
            title: AtomicU64::new(base.title),
        }
    }

    pub fn incr_embed(&self) {
        self.embed.fetch_add(1, Ordering::Relaxed);
    }
    pub fn incr_classify(&self) {
        self.classify.fetch_add(1, Ordering::Relaxed);
    }
    pub fn incr_rerank(&self) {
        self.rerank.fetch_add(1, Ordering::Relaxed);
    }
    pub fn incr_meta(&self) {
        self.meta.fetch_add(1, Ordering::Relaxed);
    }
    pub fn incr_status(&self) {
        self.status.fetch_add(1, Ordering::Relaxed);
    }
    pub fn incr_title(&self) {
        self.title.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            embed: self.embed.load(Ordering::Relaxed),
            classify: self.classify.load(Ordering::Relaxed),
            rerank: self.rerank.load(Ordering::Relaxed),
            meta: self.meta.load(Ordering::Relaxed),
            status: self.status.load(Ordering::Relaxed),
            title: self.title.load(Ordering::Relaxed),
        }
    }
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("pid", DataType::Utf8, false),
        Field::new("uptime_secs", DataType::Int64, false),
        Field::new("embed", DataType::Int64, false),
        Field::new("classify", DataType::Int64, false),
        Field::new("rerank", DataType::Int64, false),
        Field::new("meta", DataType::Int64, false),
        Field::new("status", DataType::Int64, false),
        Field::new("title", DataType::Int64, false),
        Field::new("total", DataType::Int64, false),
    ]))
}

async fn open_table_at(dir: &std::path::Path) -> Result<lancedb::Table> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.to_str().context("lancedb path is not UTF-8")?;
    let conn = connect(path)
        .execute()
        .await
        .context("opening LanceDB connection")?;
    match conn.open_table(TABLE_NAME).execute().await {
        Ok(t) => {
            // One-time migration: a table created before a counter column
            // (e.g. `title`) was added has a narrower row shape, so the
            // batch we append would fail the schema check and metrics would
            // silently stop persisting. These are non-critical cumulative
            // daemon counters, so drop & recreate rather than do an Arrow
            // column migration — the daemon simply re-seeds from 0.
            let want = schema();
            let have = t.schema().await.context("reading daemon_metrics schema")?;
            let compatible = have.fields().len() == want.fields().len()
                && have
                    .fields()
                    .iter()
                    .zip(want.fields().iter())
                    .all(|(a, b)| a.name() == b.name());
            if compatible {
                Ok(t)
            } else {
                conn.drop_table(TABLE_NAME, &[])
                    .await
                    .context("dropping stale daemon_metrics table")?;
                conn.create_empty_table(TABLE_NAME, want)
                    .execute()
                    .await
                    .context("recreating daemon_metrics table")
            }
        }
        Err(_) => conn
            .create_empty_table(TABLE_NAME, schema())
            .execute()
            .await
            .context("creating daemon_metrics table"),
    }
}

/// Append one cumulative snapshot row. Best-effort — failures are logged, not
/// fatal (the daemon must keep serving even if the metrics store is unwritable).
pub async fn persist(pid: u32, uptime_secs: u64, snap: MetricsSnapshot) {
    let dir = match global_lancedb_dir() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("daemon metrics persist failed: {e}");
            return;
        }
    };
    if let Err(e) = persist_at(&dir, pid, uptime_secs, snap).await {
        tracing::warn!("daemon metrics persist failed: {e}");
    }
}

/// Append a snapshot row to a specific LanceDB directory (testable form),
/// then prune snapshot rows older than [`SNAPSHOT_RETENTION_DAYS`] and
/// compact the table. The just-appended row carries `ts = now`, so the
/// newest snapshot always survives the prune — cumulative seeding across
/// restarts is preserved even after long idle gaps (pruning only runs when
/// a newer row has just landed). Prune/compaction failures never fail the
/// persist itself.
pub(crate) async fn persist_at(
    dir: &std::path::Path,
    pid: u32,
    uptime_secs: u64,
    snap: MetricsSnapshot,
) -> Result<()> {
    // Open the table once and reuse the handle for both the append and the
    // prune, rather than opening a second LanceDB connection just to prune
    // (finding #17).
    let table = open_table_at(dir).await?;
    append_row(&table, Utc::now(), pid, uptime_secs, snap).await?;
    if let Err(e) = prune_snapshots(&table).await {
        tracing::debug!("daemon metrics prune failed (non-fatal): {e}");
    }
    Ok(())
}

/// Append a single snapshot row with an explicit timestamp, without pruning.
/// Test-only: lets tests seed old rows deterministically (production goes
/// through [`persist_at`], which reuses one table handle for append + prune).
#[cfg(test)]
pub(crate) async fn persist_row_at(
    dir: &std::path::Path,
    ts: chrono::DateTime<Utc>,
    pid: u32,
    uptime_secs: u64,
    snap: MetricsSnapshot,
) -> Result<()> {
    let table = open_table_at(dir).await?;
    append_row(&table, ts, pid, uptime_secs, snap).await
}

/// Append one cumulative snapshot row to an already-open table.
async fn append_row(
    table: &lancedb::Table,
    ts: chrono::DateTime<Utc>,
    pid: u32,
    uptime_secs: u64,
    snap: MetricsSnapshot,
) -> Result<()> {
    let schema = schema();
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(
            TimestampMicrosecondArray::from(vec![ts.timestamp_micros()])
                .with_timezone(Arc::<str>::from("UTC")),
        ),
        Arc::new(StringArray::from(vec![pid.to_string()])),
        Arc::new(Int64Array::from(vec![uptime_secs as i64])),
        Arc::new(Int64Array::from(vec![snap.embed as i64])),
        Arc::new(Int64Array::from(vec![snap.classify as i64])),
        Arc::new(Int64Array::from(vec![snap.rerank as i64])),
        Arc::new(Int64Array::from(vec![snap.meta as i64])),
        Arc::new(Int64Array::from(vec![snap.status as i64])),
        Arc::new(Int64Array::from(vec![snap.title as i64])),
        Arc::new(Int64Array::from(vec![snap.total() as i64])),
    ];
    let batch =
        RecordBatch::try_new(schema.clone(), arrays).context("building daemon_metrics batch")?;
    let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
    table
        .add(Box::new(batches))
        .execute()
        .await
        .context("appending daemon_metrics row")?;
    Ok(())
}

/// Delete snapshot rows older than [`SNAPSHOT_RETENTION_DAYS`], then run
/// `Table::optimize()` so the deleted rows' fragments are compacted and old
/// Lance dataset versions (one per append/delete) are reclaimed from disk
/// (version pruning keeps the lancedb-default 7 days, safe for concurrent
/// readers).
///
/// Counts the stale rows first and returns early when there are none, so a
/// steady-state daemon does not issue an empty `delete` + a full-table
/// `optimize` on every ~300 s persist when nothing has aged out (finding #17).
/// Reuses the caller's table handle rather than opening a second connection.
async fn prune_snapshots(table: &lancedb::Table) -> Result<()> {
    let cutoff = Utc::now() - chrono::Duration::days(SNAPSHOT_RETENTION_DAYS);
    let predicate = format!(
        "ts < TIMESTAMP '{}'",
        cutoff.format("%Y-%m-%d %H:%M:%S%.6f%:z")
    );
    let stale = table
        .count_rows(Some(predicate.clone()))
        .await
        .context("counting stale daemon_metrics snapshots")?;
    if stale == 0 {
        return Ok(());
    }
    table
        .delete(&predicate)
        .await
        .context("pruning old daemon_metrics snapshots")?;
    table
        .optimize(lancedb::table::OptimizeAction::All)
        .await
        .context("optimizing daemon_metrics table")?;
    Ok(())
}

/// The most recent persisted snapshot, if any rows exist.
#[derive(Debug, Clone, Copy)]
pub struct PersistedMetrics {
    pub snapshot: MetricsSnapshot,
    pub uptime_secs: u64,
    pub ts_micros: i64,
}

/// Read the newest persisted snapshot. Returns `None` (never errors out to the
/// caller) when the table is absent or unreadable — used both to seed a fresh
/// daemon and to answer `stats --daemon` when no daemon is running.
pub async fn load_latest() -> Option<PersistedMetrics> {
    let dir = global_lancedb_dir().ok()?;
    match load_latest_at(&dir).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("daemon metrics load failed: {e}");
            None
        }
    }
}

/// Read the newest snapshot from a specific LanceDB directory (testable form).
///
/// This is a full table scan: lancedb 0.26's query builder has no
/// `order_by` pushdown (only `limit`/`offset`/`only_if`, none of which can
/// select the max-`ts` row), and a `ts >= cutoff` filter could miss the only
/// surviving row after a long daemon idle gap. The scan stays cheap because
/// `persist_at` prunes rows older than [`SNAPSHOT_RETENTION_DAYS`] on every
/// persist, bounding the table to ~8.6 k rows.
pub(crate) async fn load_latest_at(dir: &std::path::Path) -> Result<Option<PersistedMetrics>> {
    let path = match dir.to_str() {
        Some(p) => p,
        None => return Ok(None),
    };
    let conn = match connect(path).execute().await {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let table = match conn.open_table(TABLE_NAME).execute().await {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    let mut stream = table
        .query()
        .execute()
        .await
        .context("querying daemon_metrics table")?;
    let mut best: Option<PersistedMetrics> = None;
    while let Some(b) = stream.next().await {
        let b = b.context("reading daemon_metrics batch")?;
        let ts = b
            .column_by_name("ts")
            .context("missing ts column")?
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .context("ts column is not Timestamp(Microsecond)")?;
        let col = |name: &str| -> Result<Int64Array> {
            Ok(b.column_by_name(name)
                .with_context(|| format!("missing {name} column"))?
                .as_any()
                .downcast_ref::<Int64Array>()
                .with_context(|| format!("{name} column is not Int64"))?
                .clone())
        };
        let (embed, classify, rerank, meta, status, uptime) = (
            col("embed")?,
            col("classify")?,
            col("rerank")?,
            col("meta")?,
            col("status")?,
            col("uptime_secs")?,
        );
        // `title` was added after the first release. A snapshot row written
        // by an older daemon has no such column; treat it as 0 rather than
        // erroring (back-compat — `stats --daemon` reads this directly,
        // without the table-migration that `open_table_at` does on write).
        let title = b
            .column_by_name("title")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>().cloned());
        for i in 0..b.num_rows() {
            let t = ts.value(i);
            if best.map(|p| t > p.ts_micros).unwrap_or(true) {
                best = Some(PersistedMetrics {
                    ts_micros: t,
                    uptime_secs: uptime.value(i).max(0) as u64,
                    snapshot: MetricsSnapshot {
                        embed: embed.value(i).max(0) as u64,
                        classify: classify.value(i).max(0) as u64,
                        rerank: rerank.value(i).max(0) as u64,
                        meta: meta.value(i).max(0) as u64,
                        status: status.value(i).max(0) as u64,
                        title: title
                            .as_ref()
                            .map(|a| a.value(i).max(0) as u64)
                            .unwrap_or(0),
                    },
                });
            }
        }
    }
    Ok(best)
}
