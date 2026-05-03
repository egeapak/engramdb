//! Runtime telemetry: per-project counters, hit-rate, response timings.
//!
//! The telemetry stack has three layers:
//! - [`RingHistogram`]: fixed-capacity recency-weighted reservoir that exposes
//!   exact percentiles over the buffered samples.
//! - [`StatsCollector`]: process-wide aggregator. Holds a per-project map of
//!   counters (calls, errors, hits, zero-results, retrieval-quality buckets)
//!   and stage timings. All updates take an `&Arc<StatsCollector>` so the
//!   collector can live behind the MCP server with zero contention on hot
//!   paths.
//! - [`StatsScope`]: RAII guard placed at the top of every MCP tool handler.
//!   On drop it records the elapsed time and success/error outcome.
//!
//! Persistence is handled by [`persistence`]: aggregates are snapshotted to a
//! per-project JSON file so counters survive server restarts.

pub mod collector;
pub mod persistence;

pub use collector::{
    QueryQualityBucket, RingHistogram, RuntimeSnapshot, StatsCollector, StatsScope,
};
