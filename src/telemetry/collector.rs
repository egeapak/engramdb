//! Process-wide stats collector with per-project breakdown.
//!
//! All recording paths update an in-memory aggregate AND emit a
//! single-row [`EventRow`] onto an unbounded `mpsc` channel. The
//! [`crate::telemetry::persistence`] flush task drains the channel and
//! appends rows to each project's `stats_events` LanceDB table.
//!
//! Session IDs are passed through every recording call so we can:
//! - count unique sessions per project,
//! - compute the followup rate (queries within `followup_window_secs` of
//!   a previous query *in the same session*).
//!
//! When the persistence receiver isn't taken (CLI / tests), events are
//! still buffered in the channel; they're harmless and drop with the
//! collector.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

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

/// Type tag on a persisted event row.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EventType {
    ToolCall,
    Stage,
    QueryOutcome,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ToolCall => "tool_call",
            Self::Stage => "stage",
            Self::QueryOutcome => "query_outcome",
        }
    }

    pub fn parse_label(s: &str) -> Option<Self> {
        match s {
            "tool_call" => Some(Self::ToolCall),
            "stage" => Some(Self::Stage),
            "query_outcome" => Some(Self::QueryOutcome),
            _ => None,
        }
    }
}

/// One row in the persisted event log. Also the message type sent over
/// the in-process mpsc channel.
#[derive(Debug, Clone)]
pub struct EventRow {
    pub ts: DateTime<Utc>,
    pub event_type: EventType,
    pub tool: Option<String>,
    pub stage: Option<String>,
    pub duration_ms: Option<f64>,
    pub success: Option<bool>,
    pub hit: Option<bool>,
    pub retrieval_quality: Option<String>,
    pub session_id: Option<String>,
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

/// Query-outcome counters (hits, zero-results, by-quality bucket, followups).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryCounters {
    pub total: u64,
    pub hits: u64,
    pub zero_results: u64,
    pub by_quality_full: u64,
    pub by_quality_keyword_only: u64,
    pub by_quality_scope_only: u64,
    pub by_quality_no_query_signals: u64,
    /// Queries that arrived within `followup_window_secs` of a prior query
    /// in the same session.
    pub followups: u64,
}

impl QueryCounters {
    fn record(&mut self, hit: bool, quality: QueryQualityBucket, is_followup: bool) {
        self.total += 1;
        if hit {
            self.hits += 1;
        } else {
            self.zero_results += 1;
        }
        if is_followup {
            self.followups += 1;
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
    /// Set of distinct session IDs seen for this project.
    pub sessions: BTreeSet<String>,
    /// Most recent query timestamp per session, used to compute the
    /// followup rate. Capped at a few entries per session — old sessions
    /// linger only as keys with a `last_query_at`.
    pub last_query_at: BTreeMap<String, DateTime<Utc>>,
}

/// Process-wide collector. Cheap to clone (everything behind `Arc`).
pub struct StatsCollector {
    since: DateTime<Utc>,
    config: StatsConfig,
    inner: Mutex<Inner>,
    /// Persistence event sender. Always present; the matching receiver is
    /// stored in `inner.rx_slot` until [`take_receiver`] hands it to the
    /// flush task.
    tx: mpsc::UnboundedSender<(String /* project_id */, EventRow)>,
    /// Count of `stats_events` write failures observed by the persistence
    /// layer. Surfaced in the runtime snapshot so operators have a signal
    /// that telemetry is silently dropping rows.
    persistence_failures: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for StatsCollector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatsCollector")
            .field("since", &self.since)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

struct Inner {
    by_project: HashMap<String, ProjectStats>,
    rx_slot: Option<mpsc::UnboundedReceiver<(String, EventRow)>>,
}

impl StatsCollector {
    pub fn new(config: StatsConfig) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            since: Utc::now(),
            config,
            inner: Mutex::new(Inner {
                by_project: HashMap::new(),
                rx_slot: Some(rx),
            }),
            tx,
            persistence_failures: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Increment the persistence-failure counter. Called by the persistence
    /// layer whenever an `append_events` write fails.
    pub fn record_persistence_failure(&self) {
        self.persistence_failures
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn persistence_failures(&self) -> u64 {
        self.persistence_failures
            .load(std::sync::atomic::Ordering::Relaxed)
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

    /// Take the persistence receiver. Subsequent calls return `None`.
    /// Called once at server startup by the flush-task spawner.
    pub fn take_receiver(&self) -> Option<mpsc::UnboundedReceiver<(String, EventRow)>> {
        // Recover from a poisoned mutex rather than silently returning
        // `None` — that would prevent the flush task from ever spawning,
        // dropping all persistence without any signal.
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.rx_slot.take()
    }

    fn capacity(&self) -> usize {
        self.config.histogram_capacity
    }

    fn followup_window(&self) -> chrono::Duration {
        chrono::Duration::seconds(self.config.followup_window_secs as i64)
    }

    fn with_project<F, R>(&self, project_id: &str, f: F) -> R
    where
        F: FnOnce(&mut ProjectStats) -> R,
    {
        let mut inner = self
            .inner
            .lock()
            // Telemetry must never break a tool call — recover poisoned
            // state instead of panicking. Worst-case the inner state is
            // partially mutated; subsequent record calls will continue
            // updating it consistently.
            .unwrap_or_else(|e| e.into_inner());
        let entry = inner.by_project.entry(project_id.to_string()).or_default();
        f(entry)
    }

    fn send_event(&self, project_id: &str, row: EventRow) {
        // Errors here mean the receiver was dropped without the channel
        // being closed by us — there's nothing we can do, and telemetry
        // must never break a tool call.
        let _ = self.tx.send((project_id.to_string(), row));
    }

    fn note_session(p: &mut ProjectStats, session_id: Option<&str>) {
        if let Some(sid) = session_id {
            if !sid.is_empty() {
                p.sessions.insert(sid.to_string());
            }
        }
    }

    /// Evict the oldest entries from `sessions` and `last_query_at` when
    /// they exceed the configured cap. Called on the recording paths so
    /// growth is amortized.
    fn evict_if_needed(p: &mut ProjectStats, max: usize) {
        if max == 0 {
            return;
        }
        // last_query_at is the source of truth for recency.
        if p.last_query_at.len() > max {
            // Sort by timestamp ascending and drop the oldest until at cap.
            let mut by_age: Vec<(DateTime<Utc>, String)> = p
                .last_query_at
                .iter()
                .map(|(k, v)| (*v, k.clone()))
                .collect();
            by_age.sort_by_key(|(ts, _)| *ts);
            let drop_count = p.last_query_at.len() - max;
            for (_, sid) in by_age.into_iter().take(drop_count) {
                p.last_query_at.remove(&sid);
                p.sessions.remove(&sid);
            }
        }
        // `sessions` may also exceed `max` from anonymous-only paths; cap
        // it independently. We can only evict arbitrary entries since we
        // don't track recency for sessions without queries.
        if p.sessions.len() > max {
            let drop_count = p.sessions.len() - max;
            let to_drop: Vec<String> = p.sessions.iter().take(drop_count).cloned().collect();
            for sid in to_drop {
                p.sessions.remove(&sid);
            }
        }
    }

    /// Record a completed tool call (called from `StatsScope::drop`).
    pub fn record_tool_call(
        &self,
        project_id: &str,
        tool: &'static str,
        elapsed_ms: f64,
        success: bool,
        session_id: Option<&str>,
    ) {
        if !self.config.enabled {
            return;
        }
        let cap = self.capacity();
        let max_sessions = self.config.max_sessions_per_project;
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
            Self::note_session(p, session_id);
            Self::evict_if_needed(p, max_sessions);
        });
        self.send_event(
            project_id,
            EventRow {
                ts: Utc::now(),
                event_type: EventType::ToolCall,
                tool: Some(tool.to_string()),
                stage: None,
                duration_ms: Some(elapsed_ms),
                success: Some(success),
                hit: None,
                retrieval_quality: None,
                session_id: session_id.map(str::to_owned),
            },
        );
    }

    /// Record a stage timing (embed, vector_search, rerank, etc.).
    pub fn record_stage(
        &self,
        project_id: &str,
        stage: &'static str,
        elapsed_ms: f64,
        session_id: Option<&str>,
    ) {
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
        self.send_event(
            project_id,
            EventRow {
                ts: Utc::now(),
                event_type: EventType::Stage,
                tool: None,
                stage: Some(stage.to_string()),
                duration_ms: Some(elapsed_ms),
                success: None,
                hit: None,
                retrieval_quality: None,
                session_id: session_id.map(str::to_owned),
            },
        );
    }

    /// Record a query outcome — hit-rate / zero-result / quality bucket
    /// and (when `session_id` is present) followup-rate computation.
    pub fn record_query_outcome(
        &self,
        project_id: &str,
        hit: bool,
        quality_label: &str,
        session_id: Option<&str>,
    ) {
        if !self.config.enabled {
            return;
        }
        // The producer is in-tree (`RetrievalEngine::query`) and emits one
        // of four `&'static str` values. An unknown label is a programmer
        // error; debug_assert catches it in tests, prod silently falls
        // through to `no_query_signals` so we don't lose the event.
        debug_assert!(
            QueryQualityBucket::from_label(quality_label).is_some(),
            "unknown retrieval_quality label: {quality_label:?}"
        );
        let bucket = QueryQualityBucket::from_label(quality_label)
            .unwrap_or(QueryQualityBucket::NoQuerySignals);
        let now = Utc::now();
        let window = self.followup_window();
        let max_sessions = self.config.max_sessions_per_project;
        self.with_project(project_id, |p| {
            let is_followup = match session_id {
                Some(sid) if !sid.is_empty() => match p.last_query_at.get(sid) {
                    Some(prev) => (now - *prev) <= window,
                    None => false,
                },
                _ => false,
            };
            p.queries.record(hit, bucket, is_followup);
            Self::note_session(p, session_id);
            if let Some(sid) = session_id {
                if !sid.is_empty() {
                    p.last_query_at.insert(sid.to_string(), now);
                }
            }
            Self::evict_if_needed(p, max_sessions);
        });
        self.send_event(
            project_id,
            EventRow {
                ts: now,
                event_type: EventType::QueryOutcome,
                tool: None,
                stage: None,
                duration_ms: None,
                success: None,
                hit: Some(hit),
                retrieval_quality: Some(bucket.as_str().to_string()),
                session_id: session_id.map(str::to_owned),
            },
        );
    }

    /// Replay a slice of events into the in-memory state, replacing any
    /// existing state for the project. Idempotent: calling twice with the
    /// same input produces the same final counters. Defensive against
    /// out-of-order input — the slice is sorted chronologically before
    /// replay so a buggy persistence layer can't poison `last_query_at`
    /// with future timestamps.
    pub fn replay_events(&self, project_id: &str, events: &[EventRow]) {
        let cap = self.capacity();
        let window = self.followup_window();
        // Defensive copy + sort. Cost: O(N log N) over typically a few hundred
        // to ~50k events — negligible at startup.
        let mut sorted: Vec<&EventRow> = events.iter().collect();
        sorted.sort_by(|a, b| a.ts.cmp(&b.ts));
        self.with_project(project_id, |p| {
            // Reset to a clean slate so repeated `replay_events` calls don't
            // double-count.
            *p = ProjectStats::default();
            for ev in sorted.iter().copied() {
                Self::note_session(p, ev.session_id.as_deref());
                match ev.event_type {
                    EventType::ToolCall => {
                        p.total_calls += 1;
                        let tool = ev.tool.clone().unwrap_or_default();
                        let counters = p
                            .by_tool
                            .entry(tool)
                            .or_insert_with(|| ToolCounters::new(cap));
                        counters.calls += 1;
                        if ev.success == Some(false) {
                            counters.errors += 1;
                        }
                        if let Some(d) = ev.duration_ms {
                            counters.latency.record(d);
                        }
                    }
                    EventType::Stage => {
                        let stage = ev.stage.clone().unwrap_or_default();
                        if let Some(d) = ev.duration_ms {
                            p.stages
                                .entry(stage)
                                .or_insert_with(|| RingHistogram::new(cap))
                                .record(d);
                        }
                    }
                    EventType::QueryOutcome => {
                        let bucket = ev
                            .retrieval_quality
                            .as_deref()
                            .and_then(QueryQualityBucket::from_label)
                            .unwrap_or(QueryQualityBucket::NoQuerySignals);
                        let hit = ev.hit.unwrap_or(false);
                        let is_followup = match ev.session_id.as_deref() {
                            Some(sid) if !sid.is_empty() => match p.last_query_at.get(sid) {
                                Some(prev) => (ev.ts - *prev) <= window,
                                None => false,
                            },
                            _ => false,
                        };
                        p.queries.record(hit, bucket, is_followup);
                        if let Some(sid) = ev.session_id.as_deref() {
                            if !sid.is_empty() {
                                p.last_query_at.insert(sid.to_string(), ev.ts);
                            }
                        }
                    }
                }
            }
        });
    }

    /// Returns a serializable snapshot. When `all_projects` is true, the
    /// snapshot includes a `by_project` map keyed by project ID. Otherwise
    /// the focus project's counters are inlined at the top level.
    pub fn snapshot(&self, focus_project: &str, all_projects: bool) -> RuntimeSnapshot {
        let inner = self
            .inner
            .lock()
            // Telemetry must never break a tool call — recover poisoned
            // state instead of panicking. Worst-case the inner state is
            // partially mutated; subsequent record calls will continue
            // updating it consistently.
            .unwrap_or_else(|e| e.into_inner());
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
            persistence_failures: self.persistence_failures(),
            view: focus.map(|s| project_to_view(&s)).unwrap_or_default(),
            by_project,
        }
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
        unique_sessions: stats.sessions.len() as u64,
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
    pub unique_sessions: u64,
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
    pub followups: u64,
    pub followup_rate: f64,
    pub by_quality: BTreeMap<&'static str, u64>,
}

impl From<&QueryCounters> for QueriesView {
    fn from(q: &QueryCounters) -> Self {
        let hit_rate = if q.total == 0 {
            0.0
        } else {
            (q.hits as f64) / (q.total as f64)
        };
        let followup_rate = if q.total == 0 {
            0.0
        } else {
            (q.followups as f64) / (q.total as f64)
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
            followups: q.followups,
            followup_rate: round3(followup_rate),
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
    /// Number of `stats_events` LanceDB writes that have failed since the
    /// collector started. Non-zero values are operator signal — telemetry
    /// is silently dropping rows. Surfaced unconditionally so dashboards
    /// can alert on it.
    pub persistence_failures: u64,
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
    /// Session attribution. Empty string means "anonymous" — the session
    /// dimension is omitted on this event.
    session_id: String,
    started: Instant,
    success: AtomicBool,
}

impl StatsScope {
    pub fn new(
        collector: Arc<StatsCollector>,
        tool: &'static str,
        project_id: String,
        session_id: String,
    ) -> Self {
        Self {
            collector,
            tool,
            project_id,
            session_id,
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
        let sid = if self.session_id.is_empty() {
            None
        } else {
            Some(self.session_id.as_str())
        };
        self.collector
            .record_tool_call(&self.project_id, self.tool, elapsed_ms, success, sid);
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
            followup_window_secs: 60,
            max_sessions_per_project: 10_000,
        }
    }

    #[test]
    fn ring_histogram_percentiles_exact_over_capacity() {
        let mut h = RingHistogram::new(256);
        for i in 1..=256u32 {
            h.record(i as f64);
        }
        assert_eq!(h.percentile(0.5), 128.0);
        assert_eq!(h.percentile(0.95), 244.0);
        assert!((h.avg() - 128.5).abs() < 1e-9);

        h.record(257.0);
        assert_eq!(h.percentile(0.5), 129.0);
        assert_eq!(h.count, 257);
    }

    #[test]
    fn ring_histogram_percentiles_partial_fill() {
        let mut h = RingHistogram::new(256);
        for i in 1..=10u32 {
            h.record(i as f64);
        }
        assert_eq!(h.percentile(0.5), 5.0);
        assert_eq!(h.percentile(0.95), 10.0);
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
        c.record_tool_call("proj-A", "query", 12.0, true, Some("s1"));
        c.record_tool_call("proj-A", "query", 18.0, true, Some("s1"));
        let snap = c.snapshot("proj-A", false);
        assert_eq!(snap.view.usage.total_calls, 2);
        assert_eq!(snap.view.usage.by_tool.get("query").copied(), Some(2));
        assert_eq!(snap.view.usage.unique_sessions, 1);
        assert!(snap.view.usage.errors_by_tool.is_empty());
        let t = snap.view.timings_ms.tool.get("query").unwrap();
        assert_eq!(t.count, 2);
        assert!((t.avg - 15.0).abs() < 1e-6);
    }

    #[test]
    fn collector_counts_unique_sessions() {
        let c = StatsCollector::new(cfg());
        c.record_tool_call("p", "query", 1.0, true, Some("s1"));
        c.record_tool_call("p", "query", 1.0, true, Some("s2"));
        c.record_tool_call("p", "query", 1.0, true, Some("s2"));
        c.record_tool_call("p", "query", 1.0, true, None);
        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.usage.unique_sessions, 2);
    }

    #[test]
    fn collector_records_errors() {
        let c = StatsCollector::new(cfg());
        c.record_tool_call("p", "query", 5.0, false, None);
        c.record_tool_call("p", "query", 7.0, true, None);
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
            c.record_query_outcome("p", true, "full", Some("s"));
        }
        for _ in 0..3 {
            c.record_query_outcome("p", false, "no_query_signals", Some("s"));
        }
        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.queries.total, 10);
        assert_eq!(snap.view.queries.hits, 7);
        assert_eq!(snap.view.queries.zero_results, 3);
        assert!((snap.view.queries.hit_rate - 0.7).abs() < 1e-6);
    }

    #[test]
    fn followup_within_window_counted() {
        let c = StatsCollector::new(cfg());
        // First query — never a followup.
        c.record_query_outcome("p", true, "full", Some("S"));
        // Second query in the same session, immediately after — followup.
        c.record_query_outcome("p", true, "full", Some("S"));
        c.record_query_outcome("p", true, "full", Some("S"));
        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.queries.followups, 2);
        assert!(snap.view.queries.followup_rate > 0.6);
    }

    #[test]
    fn followup_different_session_not_counted() {
        let c = StatsCollector::new(cfg());
        c.record_query_outcome("p", true, "full", Some("S1"));
        c.record_query_outcome("p", true, "full", Some("S2"));
        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.queries.followups, 0);
    }

    #[test]
    fn followup_anonymous_session_not_counted() {
        let c = StatsCollector::new(cfg());
        c.record_query_outcome("p", true, "full", None);
        c.record_query_outcome("p", true, "full", None);
        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.queries.followups, 0);
    }

    #[test]
    fn collector_records_all_quality_buckets() {
        let c = StatsCollector::new(cfg());
        c.record_query_outcome("p", true, "full", None);
        c.record_query_outcome("p", true, "keyword_only", None);
        c.record_query_outcome("p", false, "scope_only", None);
        c.record_query_outcome("p", false, "no_query_signals", None);
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
        c.record_tool_call("a", "query", 10.0, true, None);
        c.record_tool_call("b", "create", 20.0, true, None);
        c.record_query_outcome("a", true, "full", None);

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
            let scope = StatsScope::new(c.clone(), "query", "p".to_string(), "s".to_string());
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
            let scope = StatsScope::new(c.clone(), "query", "p".to_string(), String::new());
            scope.mark_success();
        }
        let snap = c.snapshot("p", false);
        assert!(snap.view.usage.errors_by_tool.is_empty());
        assert_eq!(snap.view.usage.by_tool.get("query").copied(), Some(1));
    }

    #[test]
    fn stage_timings_recorded_per_project() {
        let c = StatsCollector::new(cfg());
        c.record_stage("p", "embed", 5.0, None);
        c.record_stage("p", "embed", 15.0, None);
        c.record_stage("p", "vector_search", 8.0, None);
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
        collector.record_tool_call("p", "query", 10.0, true, None);
        collector.record_query_outcome("p", true, "full", None);
        collector.record_stage("p", "embed", 1.0, None);
        let snap = collector.snapshot("p", false);
        assert_eq!(snap.view.usage.total_calls, 0);
        assert_eq!(snap.view.queries.total, 0);
        assert!(snap.view.timings_ms.stages.is_empty());
    }

    /// Unknown quality labels are a programmer error from in-tree
    /// producers — the debug build panics. Release builds fall through
    /// silently to `no_query_signals` so events aren't lost.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "unknown retrieval_quality label")]
    fn unknown_quality_label_panics_in_debug() {
        let c = StatsCollector::new(cfg());
        c.record_query_outcome("p", true, "weird-label", None);
    }

    #[test]
    fn replay_events_rebuilds_state() {
        let c = StatsCollector::new(cfg());
        let now = Utc::now();
        let events = vec![
            EventRow {
                ts: now,
                event_type: EventType::ToolCall,
                tool: Some("query".to_string()),
                stage: None,
                duration_ms: Some(10.0),
                success: Some(true),
                hit: None,
                retrieval_quality: None,
                session_id: Some("s".to_string()),
            },
            EventRow {
                ts: now + chrono::Duration::milliseconds(50),
                event_type: EventType::QueryOutcome,
                tool: None,
                stage: None,
                duration_ms: None,
                success: None,
                hit: Some(true),
                retrieval_quality: Some("full".to_string()),
                session_id: Some("s".to_string()),
            },
            EventRow {
                ts: now + chrono::Duration::milliseconds(100),
                event_type: EventType::Stage,
                tool: None,
                stage: Some("embed".to_string()),
                duration_ms: Some(5.0),
                success: None,
                hit: None,
                retrieval_quality: None,
                session_id: Some("s".to_string()),
            },
        ];
        c.replay_events("p", &events);

        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.usage.total_calls, 1);
        assert_eq!(snap.view.usage.unique_sessions, 1);
        assert_eq!(snap.view.queries.total, 1);
        assert_eq!(snap.view.queries.hits, 1);
        assert_eq!(snap.view.timings_ms.stages.get("embed").unwrap().count, 1);
        assert_eq!(snap.view.timings_ms.tool.get("query").unwrap().count, 1);
    }

    /// Regression: `replay_events` must be idempotent. Calling it twice
    /// with the same input produces the same counters; previously each
    /// invocation incremented on top of the existing state.
    #[test]
    fn replay_events_is_idempotent() {
        let c = StatsCollector::new(cfg());
        let now = Utc::now();
        let events = vec![
            EventRow {
                ts: now,
                event_type: EventType::ToolCall,
                tool: Some("query".to_string()),
                stage: None,
                duration_ms: Some(10.0),
                success: Some(true),
                hit: None,
                retrieval_quality: None,
                session_id: Some("s".to_string()),
            },
            EventRow {
                ts: now + chrono::Duration::milliseconds(10),
                event_type: EventType::QueryOutcome,
                tool: None,
                stage: None,
                duration_ms: None,
                success: None,
                hit: Some(true),
                retrieval_quality: Some("full".to_string()),
                session_id: Some("s".to_string()),
            },
        ];
        c.replay_events("p", &events);
        c.replay_events("p", &events);
        c.replay_events("p", &events);

        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.usage.total_calls, 1, "calls don't double");
        assert_eq!(snap.view.queries.total, 1, "queries don't double");
        assert_eq!(snap.view.queries.hits, 1);
    }

    /// `take_receiver` returns the receiver exactly once. Subsequent
    /// callers see `None`, so a misconfigured server can't accidentally
    /// spawn two flush tasks competing on the same channel.
    #[test]
    fn take_receiver_returns_none_on_second_call() {
        let c = StatsCollector::new(cfg());
        assert!(c.take_receiver().is_some(), "first call returns Some");
        assert!(c.take_receiver().is_none(), "second call returns None");
        assert!(c.take_receiver().is_none(), "third call still None");
    }

    /// Disabled collector must not push events to the channel — the
    /// receiver should observe nothing even after recording calls.
    #[tokio::test]
    async fn disabled_collector_emits_no_events() {
        let mut c = cfg();
        c.enabled = false;
        let collector = StatsCollector::new(c);
        let mut rx = collector.take_receiver().expect("first take");

        collector.record_tool_call("p", "query", 10.0, true, Some("s"));
        collector.record_query_outcome("p", true, "full", Some("s"));
        collector.record_stage("p", "embed", 1.0, Some("s"));

        // Try to receive — there should be nothing waiting. We assert
        // try_recv returns Empty; the channel must not be closed because
        // the collector is still alive.
        match rx.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            other => panic!("expected Empty, got {other:?}"),
        }
    }

    /// R12 regression: when more than `max_sessions_per_project` distinct
    /// sessions are seen, the oldest are evicted from both `sessions` and
    /// `last_query_at` so memory stays bounded on long-running daemons.
    #[test]
    fn session_tracking_is_bounded() {
        let mut c = cfg();
        c.max_sessions_per_project = 4;
        let collector = StatsCollector::new(c);
        // Record 8 distinct sessions; only the 4 most-recent should remain.
        for i in 0..8 {
            collector.record_query_outcome("p", true, "full", Some(&format!("s{i}")));
        }
        let snap = collector.snapshot("p", false);
        assert_eq!(
            snap.view.usage.unique_sessions, 4,
            "session count capped at max_sessions_per_project"
        );
        // All 8 queries are still counted — only session metadata is bounded.
        assert_eq!(snap.view.queries.total, 8);
    }

    /// Regression: out-of-order events must not poison `last_query_at`
    /// with future timestamps (which would otherwise mark every replayed
    /// query as a followup of itself).
    #[test]
    fn replay_events_handles_out_of_order_input() {
        let c = StatsCollector::new(cfg());
        let now = Utc::now();
        // Three queries from one session, given in reverse order.
        let events = vec![
            EventRow {
                ts: now + chrono::Duration::seconds(20),
                event_type: EventType::QueryOutcome,
                tool: None,
                stage: None,
                duration_ms: None,
                success: None,
                hit: Some(true),
                retrieval_quality: Some("full".to_string()),
                session_id: Some("S".to_string()),
            },
            EventRow {
                ts: now + chrono::Duration::seconds(10),
                event_type: EventType::QueryOutcome,
                tool: None,
                stage: None,
                duration_ms: None,
                success: None,
                hit: Some(true),
                retrieval_quality: Some("full".to_string()),
                session_id: Some("S".to_string()),
            },
            EventRow {
                ts: now,
                event_type: EventType::QueryOutcome,
                tool: None,
                stage: None,
                duration_ms: None,
                success: None,
                hit: Some(true),
                retrieval_quality: Some("full".to_string()),
                session_id: Some("S".to_string()),
            },
        ];
        c.replay_events("p", &events);
        let snap = c.snapshot("p", false);
        assert_eq!(snap.view.queries.total, 3);
        // Two of the three are within-window followups (sorted: 0s, 10s, 20s
        // — the 10s and 20s ones are followups of their predecessors).
        assert_eq!(snap.view.queries.followups, 2);
    }
}
