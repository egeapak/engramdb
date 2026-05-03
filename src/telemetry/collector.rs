//! Process-wide stats collector with per-project breakdown.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::types::config::StatsConfig;

/// One of the four `retrieval_quality` labels emitted by `RetrievalEngine::query`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum QueryQualityBucket {
    Full,
    KeywordOnly,
    ScopeOnly,
    NoQuerySignals,
}

impl QueryQualityBucket {
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "full" => Some(Self::Full),
            "keyword_only" => Some(Self::KeywordOnly),
            "scope_only" => Some(Self::ScopeOnly),
            "no_query_signals" => Some(Self::NoQuerySignals),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::KeywordOnly => "keyword_only",
            Self::ScopeOnly => "scope_only",
            Self::NoQuerySignals => "no_query_signals",
        }
    }
}

/// Fixed-capacity ring buffer of millisecond samples.
///
/// Percentiles are computed by sorting the live window — `O(N log N)` over
/// at most `capacity` (default 256) samples. Counters (`count`, `sum`) are
/// running across the lifetime of the histogram, while the percentile window
/// is recency-weighted: the oldest sample is evicted on overflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RingHistogram {
    capacity: usize,
    buf: Vec<f64>,
    head: usize,
    /// Current number of valid samples in the ring (≤ capacity).
    len: usize,
    /// Lifetime sample count (does not roll back when ring evicts).
    pub count: u64,
    /// Lifetime sum of samples in milliseconds (for the lifetime average).
    pub sum_ms: f64,
}

impl RingHistogram {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            buf: Vec::new(),
            head: 0,
            len: 0,
            count: 0,
            sum_ms: 0.0,
        }
    }

    pub fn record(&mut self, ms: f64) {
        self.count += 1;
        self.sum_ms += ms;
        if self.buf.len() < self.capacity {
            self.buf.push(ms);
            self.len = self.buf.len();
        } else {
            self.buf[self.head] = ms;
            self.head = (self.head + 1) % self.capacity;
            self.len = self.capacity;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn avg(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum_ms / self.count as f64
        }
    }

    /// `p` is in [0.0, 1.0]. Returns 0.0 for an empty ring.
    pub fn percentile(&self, p: f64) -> f64 {
        if self.len == 0 {
            return 0.0;
        }
        let mut sorted: Vec<f64> = self.buf[..self.len].to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p = p.clamp(0.0, 1.0);
        // nearest-rank percentile: ceil(p * N), 1-indexed → 0-indexed
        let rank = ((p * sorted.len() as f64).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1);
        sorted[rank]
    }

    fn snapshot(&self) -> Option<TimingStats> {
        if self.count == 0 {
            return None;
        }
        Some(TimingStats {
            count: self.count,
            avg: round1(self.avg()),
            p50: round1(self.percentile(0.5)),
            p95: round1(self.percentile(0.95)),
        })
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Per-tool counters — one per (project, tool) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCounters {
    pub calls: u64,
    pub errors: u64,
    pub latency: RingHistogram,
}

impl ToolCounters {
    fn new(capacity: usize) -> Self {
        Self {
            calls: 0,
            errors: 0,
            latency: RingHistogram::new(capacity),
        }
    }
}

/// Query-outcome counters (hits, zero-results, by-quality bucket).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryCounters {
    pub total: u64,
    pub hits: u64,
    pub zero_results: u64,
    pub by_quality_full: u64,
    pub by_quality_keyword_only: u64,
    pub by_quality_scope_only: u64,
    pub by_quality_no_query_signals: u64,
}

impl QueryCounters {
    fn record(&mut self, hit: bool, quality: QueryQualityBucket) {
        self.total += 1;
        if hit {
            self.hits += 1;
        } else {
            self.zero_results += 1;
        }
        match quality {
            QueryQualityBucket::Full => self.by_quality_full += 1,
            QueryQualityBucket::KeywordOnly => self.by_quality_keyword_only += 1,
            QueryQualityBucket::ScopeOnly => self.by_quality_scope_only += 1,
            QueryQualityBucket::NoQuerySignals => self.by_quality_no_query_signals += 1,
        }
    }
}

/// All counters and histograms for one project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectStats {
    pub total_calls: u64,
    pub by_tool: HashMap<String, ToolCounters>,
    pub queries: QueryCounters,
    /// Stage timings, keyed by stage name. Known stages:
    /// `query.total`, `embed`, `vector_search`, `score`, `rerank`,
    /// `create.chunk_text`, `create.embed_batch`, `create.upsert_chunks`.
    pub stages: HashMap<String, RingHistogram>,
}

/// Process-wide collector. Cheap to clone (everything behind `Arc`).
#[derive(Debug)]
pub struct StatsCollector {
    since: DateTime<Utc>,
    config: StatsConfig,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    by_project: HashMap<String, ProjectStats>,
}

impl StatsCollector {
    pub fn new(config: StatsConfig) -> Arc<Self> {
        Arc::new(Self {
            since: Utc::now(),
            config,
            inner: Mutex::new(Inner {
                by_project: HashMap::new(),
            }),
        })
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn since(&self) -> DateTime<Utc> {
        self.since
    }

    pub fn config(&self) -> &StatsConfig {
        &self.config
    }

    /// Hydrate from a previously persisted snapshot. Replaces in-memory state.
    pub fn restore_from(&self, since: DateTime<Utc>, projects: HashMap<String, ProjectStats>) {
        // We can't change `since` after construction (it's behind &self,
        // not &mut self), but the persistence layer wants to advertise the
        // *original* start time. We expose `since` via `since()` for live
        // reporting; the persisted `since` is reported separately by
        // `RuntimeSnapshot::since` (set from the persisted file at load).
        let _ = since;
        if let Ok(mut inner) = self.inner.lock() {
            inner.by_project = projects;
        }
    }

    fn capacity(&self) -> usize {
        self.config.histogram_capacity
    }

    fn with_project<F, R>(&self, project_id: &str, f: F) -> R
    where
        F: FnOnce(&mut ProjectStats) -> R,
    {
        let mut inner = self
            .inner
            .lock()
            .expect("StatsCollector mutex poisoned — a previous panic left it in a bad state");
        let entry = inner.by_project.entry(project_id.to_string()).or_default();
        f(entry)
    }

    /// Record a completed tool call (called from `StatsScope::drop`).
    pub fn record_tool_call(
        &self,
        project_id: &str,
        tool: &'static str,
        elapsed_ms: f64,
        success: bool,
    ) {
        if !self.config.enabled {
            return;
        }
        let cap = self.capacity();
        self.with_project(project_id, |p| {
            p.total_calls += 1;
            let counters = p
                .by_tool
                .entry(tool.to_string())
                .or_insert_with(|| ToolCounters::new(cap));
            counters.calls += 1;
            if !success {
                counters.errors += 1;
            }
            counters.latency.record(elapsed_ms);
        });
    }

    /// Record a stage timing (embed, vector_search, rerank, etc.).
    pub fn record_stage(&self, project_id: &str, stage: &'static str, elapsed_ms: f64) {
        if !self.config.enabled {
            return;
        }
        let cap = self.capacity();
        self.with_project(project_id, |p| {
            p.stages
                .entry(stage.to_string())
                .or_insert_with(|| RingHistogram::new(cap))
                .record(elapsed_ms);
        });
    }

    /// Record a query outcome — hit-rate / zero-result / quality bucket.
    pub fn record_query_outcome(&self, project_id: &str, hit: bool, quality_label: &str) {
        if !self.config.enabled {
            return;
        }
        let bucket = QueryQualityBucket::from_label(quality_label).unwrap_or(
            // Unknown labels fall through to "no_query_signals" so we don't lose them.
            QueryQualityBucket::NoQuerySignals,
        );
        self.with_project(project_id, |p| p.queries.record(hit, bucket));
    }

    /// Returns a serializable snapshot. When `all_projects` is true, the
    /// snapshot includes a `by_project` map keyed by project ID. Otherwise
    /// the focus project's counters are inlined at the top level.
    pub fn snapshot(&self, focus_project: &str, all_projects: bool) -> RuntimeSnapshot {
        let inner = self
            .inner
            .lock()
            .expect("StatsCollector mutex poisoned — a previous panic left it in a bad state");
        let focus = inner.by_project.get(focus_project).cloned();
        let by_project = if all_projects {
            let mut map = BTreeMap::new();
            for (pid, stats) in inner.by_project.iter() {
                map.insert(pid.clone(), project_to_view(stats));
            }
            Some(map)
        } else {
            None
        };
        RuntimeSnapshot {
            since: self.since,
            project_id: focus_project.to_string(),
            view: focus.map(|s| project_to_view(&s)).unwrap_or_default(),
            by_project,
        }
    }

    /// Return a clone of the entire by-project map for persistence.
    pub fn snapshot_for_persistence(&self) -> HashMap<String, ProjectStats> {
        let inner = self
            .inner
            .lock()
            .expect("StatsCollector mutex poisoned — a previous panic left it in a bad state");
        inner.by_project.clone()
    }
}

fn project_to_view(stats: &ProjectStats) -> ProjectView {
    let by_tool: BTreeMap<String, u64> = stats
        .by_tool
        .iter()
        .map(|(k, v)| (k.clone(), v.calls))
        .collect();
    let errors_by_tool: BTreeMap<String, u64> = stats
        .by_tool
        .iter()
        .filter(|(_, v)| v.errors > 0)
        .map(|(k, v)| (k.clone(), v.errors))
        .collect();
    let usage = UsageView {
        total_calls: stats.total_calls,
        by_tool,
        errors_by_tool,
    };

    let queries = QueriesView::from(&stats.queries);

    let tool_timings: BTreeMap<String, TimingStats> = stats
        .by_tool
        .iter()
        .filter_map(|(k, v)| v.latency.snapshot().map(|t| (k.clone(), t)))
        .collect();
    let stage_timings: BTreeMap<String, TimingStats> = stats
        .stages
        .iter()
        .filter_map(|(k, v)| v.snapshot().map(|t| (k.clone(), t)))
        .collect();
    let timings_ms = TimingsView {
        tool: tool_timings,
        stages: stage_timings,
    };

    ProjectView {
        usage,
        queries,
        timings_ms,
    }
}

/// Public, serializable view of one project's runtime counters.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ProjectView {
    pub usage: UsageView,
    pub queries: QueriesView,
    pub timings_ms: TimingsView,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UsageView {
    pub total_calls: u64,
    pub by_tool: BTreeMap<String, u64>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub errors_by_tool: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct QueriesView {
    pub total: u64,
    pub hits: u64,
    pub zero_results: u64,
    pub hit_rate: f64,
    pub by_quality: BTreeMap<&'static str, u64>,
}

impl From<&QueryCounters> for QueriesView {
    fn from(q: &QueryCounters) -> Self {
        let hit_rate = if q.total == 0 {
            0.0
        } else {
            (q.hits as f64) / (q.total as f64)
        };
        let mut by_quality = BTreeMap::new();
        if q.by_quality_full > 0 {
            by_quality.insert("full", q.by_quality_full);
        }
        if q.by_quality_keyword_only > 0 {
            by_quality.insert("keyword_only", q.by_quality_keyword_only);
        }
        if q.by_quality_scope_only > 0 {
            by_quality.insert("scope_only", q.by_quality_scope_only);
        }
        if q.by_quality_no_query_signals > 0 {
            by_quality.insert("no_query_signals", q.by_quality_no_query_signals);
        }
        Self {
            total: q.total,
            hits: q.hits,
            zero_results: q.zero_results,
            hit_rate: round3(hit_rate),
            by_quality,
        }
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TimingsView {
    pub tool: BTreeMap<String, TimingStats>,
    pub stages: BTreeMap<String, TimingStats>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimingStats {
    pub count: u64,
    pub avg: f64,
    pub p50: f64,
    pub p95: f64,
}

/// Top-level runtime telemetry payload merged into `stats` responses.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeSnapshot {
    pub since: DateTime<Utc>,
    pub project_id: String,
    #[serde(flatten)]
    pub view: ProjectView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_project: Option<BTreeMap<String, ProjectView>>,
}

// ---------------------------------------------------------------------------
// RAII guard
// ---------------------------------------------------------------------------

/// RAII guard that records a tool call on `Drop`.
///
/// Place at the top of every MCP `#[tool]` handler. The guard captures the
/// start time at construction; on drop it pushes `(elapsed, success)` to the
/// collector. `mark_success` must be called explicitly on the happy path —
/// any drop without `mark_success` is recorded as an error (covering all
/// `?`-propagated early returns).
#[must_use = "StatsScope must be held until the end of the handler — drop = record"]
pub struct StatsScope {
    collector: Arc<StatsCollector>,
    tool: &'static str,
    project_id: String,
    started: Instant,
    success: AtomicBool,
}

impl StatsScope {
    pub fn new(collector: Arc<StatsCollector>, tool: &'static str, project_id: String) -> Self {
        Self {
            collector,
            tool,
            project_id,
            started: Instant::now(),
            success: AtomicBool::new(false),
        }
    }

    /// Marks the call as successful. Must be called before returning `Ok(...)`
    /// from the handler.
    pub fn mark_success(&self) {
        self.success.store(true, Ordering::Relaxed);
    }
}

impl Drop for StatsScope {
    fn drop(&mut self) {
        let elapsed_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        let success = self.success.load(Ordering::Relaxed);
        self.collector
            .record_tool_call(&self.project_id, self.tool, elapsed_ms, success);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> StatsConfig {
        StatsConfig {
            enabled: true,
            histogram_capacity: 256,
            retention_days: None,
            flush_interval_secs: 60,
        }
    }

    #[test]
    fn ring_histogram_percentiles_exact_over_capacity() {
        let mut h = RingHistogram::new(256);
        for i in 1..=256u32 {
            h.record(i as f64);
        }
        // nearest-rank: p50 of 1..=256 is at rank ceil(128) = 128 → value 128
        assert_eq!(h.percentile(0.5), 128.0);
        // p95 at rank ceil(243.2) = 244 → value 244
        assert_eq!(h.percentile(0.95), 244.0);
        // running avg over lifetime = (1+256)/2 = 128.5
        assert!((h.avg() - 128.5).abs() < 1e-9);

        // Insert one more, evicting the oldest (1). Now buffer is 2..=257.
        h.record(257.0);
        assert_eq!(h.percentile(0.5), 129.0);
        assert_eq!(h.count, 257); // count is lifetime, doesn't roll back
    }

    #[test]
    fn ring_histogram_percentiles_partial_fill() {
        let mut h = RingHistogram::new(256);
        for i in 1..=10u32 {
            h.record(i as f64);
        }
        assert_eq!(h.percentile(0.5), 5.0); // ceil(5) = 5 → idx 4 → value 5
        assert_eq!(h.percentile(0.95), 10.0); // ceil(9.5) = 10 → idx 9 → value 10
        assert!((h.avg() - 5.5).abs() < 1e-9);
    }

    #[test]
    fn ring_histogram_empty_returns_zero() {
        let h = RingHistogram::new(64);
        assert_eq!(h.percentile(0.5), 0.0);
        assert_eq!(h.avg(), 0.0);
        assert_eq!(h.count, 0);
    }

    #[test]
    fn collector_records_tool_call_success() {
        let c = StatsCollector::new(cfg());
        c.record_tool_call("proj-A", "query", 12.0, true);
        c.record_tool_call("proj-A", "query", 18.0, true);
        let snap = c.snapshot("proj-A", false);
        assert_eq!(snap.view.usage.total_calls, 2);
        assert_eq!(snap.view.usage.by_tool.get("query").copied(), Some(2));
        assert!(snap.view.usage.errors_by_tool.is_empty());
        let t = snap.view.timings_ms.tool.get("query").unwrap();
        assert_eq!(t.count, 2);
        assert!((t.avg - 15.0).abs() < 1e-6);
    }

    #[test]
    fn collector_records_errors() {
        let c = StatsCollector::new(cfg());
        c.record_tool_call("p", "query", 5.0, false);
        c.record_tool_call("p", "query", 7.0, true);
        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.usage.by_tool.get("query").copied(), Some(2));
        assert_eq!(
            snap.view.usage.errors_by_tool.get("query").copied(),
            Some(1)
        );
    }

    #[test]
    fn collector_query_outcomes_compute_hit_rate() {
        let c = StatsCollector::new(cfg());
        for _ in 0..7 {
            c.record_query_outcome("p", true, "full");
        }
        for _ in 0..3 {
            c.record_query_outcome("p", false, "no_query_signals");
        }
        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.queries.total, 10);
        assert_eq!(snap.view.queries.hits, 7);
        assert_eq!(snap.view.queries.zero_results, 3);
        assert!((snap.view.queries.hit_rate - 0.7).abs() < 1e-6);
        assert_eq!(snap.view.queries.by_quality.get("full").copied(), Some(7));
        assert_eq!(
            snap.view
                .queries
                .by_quality
                .get("no_query_signals")
                .copied(),
            Some(3)
        );
    }

    #[test]
    fn collector_records_all_quality_buckets() {
        let c = StatsCollector::new(cfg());
        c.record_query_outcome("p", true, "full");
        c.record_query_outcome("p", true, "keyword_only");
        c.record_query_outcome("p", false, "scope_only");
        c.record_query_outcome("p", false, "no_query_signals");
        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.queries.by_quality.len(), 4);
        for label in ["full", "keyword_only", "scope_only", "no_query_signals"] {
            assert_eq!(
                snap.view.queries.by_quality.get(label).copied(),
                Some(1),
                "bucket {label} missing"
            );
        }
    }

    #[test]
    fn per_project_isolation() {
        let c = StatsCollector::new(cfg());
        c.record_tool_call("a", "query", 10.0, true);
        c.record_tool_call("b", "create", 20.0, true);
        c.record_query_outcome("a", true, "full");

        let snap_a = c.snapshot("a", false);
        assert_eq!(snap_a.view.usage.total_calls, 1);
        assert!(snap_a.view.usage.by_tool.contains_key("query"));
        assert!(!snap_a.view.usage.by_tool.contains_key("create"));
        assert_eq!(snap_a.view.queries.total, 1);

        let snap_b = c.snapshot("b", false);
        assert_eq!(snap_b.view.usage.total_calls, 1);
        assert!(snap_b.view.usage.by_tool.contains_key("create"));
        assert_eq!(snap_b.view.queries.total, 0);

        let snap_all = c.snapshot("a", true);
        let bp = snap_all.by_project.expect("by_project should be present");
        assert_eq!(bp.len(), 2);
        assert!(bp.contains_key("a") && bp.contains_key("b"));
    }

    #[test]
    fn stats_scope_records_error_on_drop_without_mark_success() {
        let c = StatsCollector::new(cfg());
        {
            let scope = StatsScope::new(c.clone(), "query", "p".to_string());
            // Drop without mark_success → error path
            drop(scope);
        }
        let snap = c.snapshot("p", false);
        assert_eq!(
            snap.view.usage.errors_by_tool.get("query").copied(),
            Some(1)
        );
        assert_eq!(snap.view.usage.by_tool.get("query").copied(), Some(1));
    }

    #[test]
    fn stats_scope_records_success_when_marked() {
        let c = StatsCollector::new(cfg());
        {
            let scope = StatsScope::new(c.clone(), "query", "p".to_string());
            scope.mark_success();
        }
        let snap = c.snapshot("p", false);
        assert!(snap.view.usage.errors_by_tool.is_empty());
        assert_eq!(snap.view.usage.by_tool.get("query").copied(), Some(1));
    }

    #[test]
    fn stage_timings_recorded_per_project() {
        let c = StatsCollector::new(cfg());
        c.record_stage("p", "embed", 5.0);
        c.record_stage("p", "embed", 15.0);
        c.record_stage("p", "vector_search", 8.0);
        let snap = c.snapshot("p", false);
        let t = snap.view.timings_ms.stages.get("embed").unwrap();
        assert_eq!(t.count, 2);
        assert!((t.avg - 10.0).abs() < 1e-6);
        assert!(snap.view.timings_ms.stages.contains_key("vector_search"));
    }

    #[test]
    fn disabled_collector_is_noop() {
        let mut c = cfg();
        c.enabled = false;
        let collector = StatsCollector::new(c);
        collector.record_tool_call("p", "query", 10.0, true);
        collector.record_query_outcome("p", true, "full");
        collector.record_stage("p", "embed", 1.0);
        let snap = collector.snapshot("p", false);
        assert_eq!(snap.view.usage.total_calls, 0);
        assert_eq!(snap.view.queries.total, 0);
        assert!(snap.view.timings_ms.stages.is_empty());
    }

    #[test]
    fn unknown_quality_label_falls_back() {
        let c = StatsCollector::new(cfg());
        c.record_query_outcome("p", true, "weird-label");
        let snap = c.snapshot("p", false);
        // Should still count toward total + hit; bucket falls into no_query_signals
        assert_eq!(snap.view.queries.total, 1);
        assert_eq!(snap.view.queries.hits, 1);
        assert_eq!(
            snap.view
                .queries
                .by_quality
                .get("no_query_signals")
                .copied(),
            Some(1)
        );
    }
}
