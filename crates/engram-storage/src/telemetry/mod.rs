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
//! Persistence is handled by [`persistence`]: events are appended to a
//! per-project LanceDB `stats_events` table so counters survive restarts.
//!
//! Public surface is curated below. The `collector` and `persistence`
//! submodules are `pub(crate)` so internal types like `EventRow` and
//! `ToolCounters` aren't part of the library API.

pub(crate) mod collector;
// `persistence` is `pub` (not `pub(crate)`) because the top-level `engramdb`
// crate's `ops` / `mcp` / `cli` layers drive flush/hydrate directly across the
// crate boundary.
pub mod persistence;

pub use collector::{
    ProjectView, QueriesView, QueryQualityBucket, RingHistogram, RuntimeSnapshot, StatsCollector,
    StatsScope, TimingStats, TimingsView, UsageView,
};
// `EventRow` appears in `persistence::spawn_flush_task`'s receiver type, which
// the top-level crate drives across the boundary, so it must be public too.
// `EventType` is an `EventRow` field type, exported so cross-crate callers
// (and the core crate's gc-maintenance tests) can construct rows.
pub use collector::{EventRow, EventType};
