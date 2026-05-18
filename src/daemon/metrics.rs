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

/// Live per-op request counters. Seeded from the last persisted snapshot at
/// daemon startup so totals are cumulative across daemon restarts.
#[derive(Debug, Default)]
pub struct Counters {
    embed: AtomicU64,
    classify: AtomicU64,
    rerank: AtomicU64,
    meta: AtomicU64,
    status: AtomicU64,
}

/// A point-in-time view of the cumulative counters.
#[derive(Debug, Clone, Copy, Default)]
pub struct MetricsSnapshot {
    pub embed: u64,
    pub classify: u64,
    pub rerank: u64,
    pub meta: u64,
    pub status: u64,
}

impl MetricsSnapshot {
    pub fn total(&self) -> u64 {
        self.embed + self.classify + self.rerank + self.meta + self.status
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

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            embed: self.embed.load(Ordering::Relaxed),
            classify: self.classify.load(Ordering::Relaxed),
            rerank: self.rerank.load(Ordering::Relaxed),
            meta: self.meta.load(Ordering::Relaxed),
            status: self.status.load(Ordering::Relaxed),
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
        Ok(t) => Ok(t),
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

/// Append a snapshot row to a specific LanceDB directory (testable form).
pub(crate) async fn persist_at(
    dir: &std::path::Path,
    pid: u32,
    uptime_secs: u64,
    snap: MetricsSnapshot,
) -> Result<()> {
    let table = open_table_at(dir).await?;
    let schema = schema();
    let arrays: Vec<ArrayRef> = vec![
        Arc::new(
            TimestampMicrosecondArray::from(vec![Utc::now().timestamp_micros()])
                .with_timezone(Arc::<str>::from("UTC")),
        ),
        Arc::new(StringArray::from(vec![pid.to_string()])),
        Arc::new(Int64Array::from(vec![uptime_secs as i64])),
        Arc::new(Int64Array::from(vec![snap.embed as i64])),
        Arc::new(Int64Array::from(vec![snap.classify as i64])),
        Arc::new(Int64Array::from(vec![snap.rerank as i64])),
        Arc::new(Int64Array::from(vec![snap.meta as i64])),
        Arc::new(Int64Array::from(vec![snap.status as i64])),
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
                    },
                });
            }
        }
    }
    Ok(best)
}
