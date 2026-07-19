//! EngramDB MCP server implementation.
//!
//! Defines the server struct, all MCP tools (23), resources (2), and prompts (2).
//! Tools delegate to the `ops` layer; the server opens a fresh `MemoryStore`
//! per request so it always sees the latest on-disk state.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_handler, tool_router};
use rmcp::{ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};

use crate::error::{error_response, ErrorCode};
use engramdb::ops;
use engramdb::retrieval::engine::{RetrievalEngine, RetrievalMode, RetrievalQuery};
use engramdb::storage::config::load_config_or_default;
use engramdb::storage::{FileRegistry, MemoryStore, RegistryBackend};
use engramdb::title::TitleStrategy;
use engramdb::types::{EmbeddingBackend, Provenance, Status, Visibility};

/// Synthetic telemetry partition for registry-level operations
/// (`projects_list/link/unlink`) that aren't scoped to any single project.
/// Persistence skips this bucket — it lives in-memory only.
pub(crate) const SYSTEM_PROJECT_ID: &str = "__system__";

// ---------------------------------------------------------------------------
// Input parameter structs for tool aggregation
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateInput {
    #[serde(rename = "type")]
    #[schemars(
        description = "Memory type: decision, convention, hazard, context, intent, relationship, debug, or preference"
    )]
    type_: String,

    #[schemars(
        description = "Core knowledge to store (max ~500 tokens). For decisions, state what was chosen, over what alternatives, and why."
    )]
    content: String,

    #[schemars(description = "One-line summary, max 100 chars (required)")]
    summary: String,

    #[schemars(description = "Extended details (lazy-loaded)")]
    details: Option<String>,

    #[schemars(description = "File paths this applies to (default [\"/\"])")]
    physical: Option<Vec<String>>,

    #[schemars(description = "Logical scopes in dot notation")]
    logical: Option<Vec<String>>,

    #[schemars(description = "Freeform tags")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Importance 0.0-1.0 (default 0.5)")]
    criticality: Option<f64>,

    #[schemars(description = "Confidence 0.0-1.0 (default 0.8)")]
    confidence: Option<f64>,

    #[schemars(description = "Visibility: shared|personal (default shared)")]
    visibility: Option<String>,

    #[schemars(description = "IDs of memories this supersedes")]
    supersedes: Option<Vec<String>>,

    #[schemars(
        description = "Epistemic class: fact (structural, verifiable against the repo), observation (measured empirically, may go stale), decision (chosen over alternatives, valid while its premise holds). Defaults from type; set only when it differs."
    )]
    epistemic: Option<String>,

    #[schemars(
        description = "Premise this memory depends on, e.g. 'while we pin ort rc.12'. State it if the memory becomes wrong when something specific changes."
    )]
    premise: Option<String>,

    #[schemars(
        description = "Paths/globs whose change invalidates this memory (distinct from physical, which is where it applies)."
    )]
    invalidated_by: Option<Vec<String>>,

    #[schemars(
        description = "Task or feature this was decided for (short human-readable name, not a session id)."
    )]
    origin_task: Option<String>,

    #[schemars(description = "'project' (default) or 'task' (binding only within origin_task).")]
    generality: Option<String>,

    #[schemars(
        description = "Valid-time start (RFC3339): when the claim became true in the world. Only to backdate; defaults to creation time."
    )]
    valid_from: Option<String>,

    #[schemars(description = "Decay: none|linear|exponential|step")]
    decay_strategy: Option<String>,

    #[schemars(description = "Half-life in seconds")]
    decay_half_life: Option<u64>,

    #[schemars(description = "TTL in seconds")]
    decay_ttl: Option<u64>,

    #[schemars(description = "Minimum decay factor (0.0-1.0)")]
    decay_floor: Option<f64>,

    #[schemars(description = "Optional human-readable title for the memory file")]
    title: Option<String>,

    #[schemars(
        description = "Title generation strategy: keyword|t5|none. Defaults to the project's [title].strategy config (t5 unless overridden)."
    )]
    title_strategy: Option<String>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct QueryInput {
    #[schemars(
        description = "Mode: \"rank\" for context-aware ranked results (browse); \"filter\" to require a positive signal (keyword, scope, tag, or semantic match). Required."
    )]
    mode: String,

    #[schemars(description = "Search query text (tokenized against summary, content, tags)")]
    query: Option<String>,

    #[schemars(
        description = "Physical scope — current file path for proximity scoring. Repo-relative or absolute (absolute paths under the project root are relativized automatically)."
    )]
    path: Option<String>,

    #[schemars(
        description = "Logical scopes in dot notation — contributes to hierarchy-proximity scoring (not a filter)"
    )]
    logical: Option<Vec<String>>,

    #[schemars(description = "Filter by memory types")]
    types: Option<Vec<String>>,

    #[schemars(description = "Filter by tags (OR logic)")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Minimum criticality threshold")]
    min_criticality: Option<f64>,

    #[schemars(description = "Maximum results (default 10)")]
    max_results: Option<usize>,

    #[schemars(
        description = "Detail: summary|content|full (default content) — available in both modes"
    )]
    detail_level: Option<String>,

    #[schemars(description = "Include expired/decayed memories")]
    include_expired: Option<bool>,

    #[schemars(
        description = "Filter by epistemic class: fact, observation, decision (OR logic, like types)."
    )]
    epistemic: Option<Vec<String>>,

    #[schemars(
        description = "Your current situation, to reweight classes: session_start, file_edit, debugging, design_choice. Declare debugging when investigating a failure (observations rank highest) and design_choice when weighing alternatives (prior decisions rank highest)."
    )]
    situation: Option<String>,

    #[schemars(
        description = "Include memories whose validity window was closed (invalidated). Default false."
    )]
    include_invalidated: Option<bool>,

    #[schemars(description = "Also include global memories in results (default false)")]
    include_global: Option<bool>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetInput {
    #[schemars(description = "Memory ID")]
    id: String,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UpdateInput {
    #[schemars(description = "Memory ID")]
    id: String,

    #[serde(rename = "type")]
    #[schemars(description = "Memory type")]
    type_: Option<String>,

    #[schemars(description = "Content")]
    content: Option<String>,

    #[schemars(description = "Summary")]
    summary: Option<String>,

    #[schemars(description = "Details")]
    details: Option<String>,

    #[schemars(description = "Physical scopes")]
    physical: Option<Vec<String>>,

    #[schemars(description = "Logical scopes")]
    logical: Option<Vec<String>>,

    #[schemars(description = "Replace all tags")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Tags to add (merged with existing)")]
    tags_add: Option<Vec<String>>,

    #[schemars(description = "Tags to remove")]
    tags_remove: Option<Vec<String>>,

    #[schemars(description = "Criticality")]
    criticality: Option<f64>,

    #[schemars(description = "Confidence")]
    confidence: Option<f64>,

    #[schemars(description = "Visibility")]
    visibility: Option<String>,

    #[schemars(description = "Human-readable title for the memory file")]
    title: Option<String>,

    #[schemars(description = "Status: active|needsreview|challenged")]
    status: Option<String>,

    #[schemars(description = "IDs of memories this supersedes")]
    supersedes: Option<Vec<String>>,

    #[schemars(
        description = "Epistemic class: fact (structural, verifiable against the repo), observation (measured empirically, may go stale), decision (chosen over alternatives, valid while its premise holds). Defaults from type; set only when it differs."
    )]
    epistemic: Option<String>,

    #[schemars(
        description = "Premise this memory depends on, e.g. 'while we pin ort rc.12'. State it if the memory becomes wrong when something specific changes."
    )]
    premise: Option<String>,

    #[schemars(
        description = "Paths/globs whose change invalidates this memory (distinct from physical, which is where it applies)."
    )]
    invalidated_by: Option<Vec<String>>,

    #[schemars(
        description = "Task or feature this was decided for (short human-readable name, not a session id)."
    )]
    origin_task: Option<String>,

    #[schemars(description = "'project' (default) or 'task' (binding only within origin_task).")]
    generality: Option<String>,

    #[schemars(
        description = "Valid-time start (RFC3339): when the claim became true in the world. Only to backdate; defaults to creation time."
    )]
    valid_from: Option<String>,

    #[schemars(
        description = "Clear the whole validity condition (premise/invalidated_by/origin_task/generality)."
    )]
    clear_validity: Option<bool>,

    #[schemars(
        description = "Reopen a closed validity window: clears invalidated_at and superseded_by. Invalidation is reversible, unlike deletion."
    )]
    clear_invalidated: Option<bool>,

    #[schemars(description = "Decay: none|linear|exponential|step")]
    decay_strategy: Option<String>,

    #[schemars(description = "Half-life in seconds")]
    decay_half_life: Option<u64>,

    #[schemars(description = "TTL in seconds")]
    decay_ttl: Option<u64>,

    #[schemars(description = "Minimum decay factor (0.0-1.0)")]
    decay_floor: Option<f64>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DeleteInput {
    #[schemars(description = "Memory ID")]
    id: String,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ChallengeInput {
    #[schemars(description = "Memory ID")]
    id: String,

    #[schemars(description = "Evidence contradicting this memory")]
    evidence: String,

    #[schemars(description = "File where evidence was found")]
    source_file: Option<String>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReviewInput {
    #[schemars(description = "Filter to a scope")]
    scope: Option<String>,

    #[schemars(description = "Max results (default 10)")]
    max_results: Option<usize>,

    #[serde(rename = "type")]
    #[schemars(description = "Filter by memory type")]
    type_: Option<String>,

    #[schemars(description = "Only show challenged memories")]
    challenged_only: Option<bool>,

    #[schemars(description = "Only show needs-review memories")]
    stale_only: Option<bool>,

    #[schemars(
        description = "Recency trigger: also surface active memories not updated in more than N days. Omit to use the project's [review].recency_days (default 90); set 0 or a very large value to effectively disable."
    )]
    stale_after_days: Option<u64>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ResolveInput {
    #[schemars(description = "Memory ID")]
    id: String,

    #[schemars(
        description = "Action: keep, update, delete, or invalidate. Prefer invalidate over delete when a memory WAS true but no longer is — history is kept and queryable via include_invalidated."
    )]
    action: String,

    #[schemars(description = "New content (required for update)")]
    updated_content: Option<String>,

    #[schemars(description = "New summary (optional for update)")]
    updated_summary: Option<String>,

    #[schemars(
        description = "For invalidate: id of the memory that superseded this one (optional)."
    )]
    superseded_by: Option<String>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VerifyInput {
    #[schemars(description = "Memory ID")]
    id: String,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TaskCurrentInput {
    #[schemars(
        description = "Task/feature name to declare for this session (short human-readable name). Omit to read the current declaration."
    )]
    task: Option<String>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TaskCompleteInput {
    #[schemars(description = "Task/feature name to mark finished.")]
    task: String,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CompressCandidatesInput {
    #[schemars(description = "Scope to filter candidates")]
    scope: Option<String>,

    #[schemars(description = "Criticality threshold (default 0.4)")]
    threshold: Option<f64>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CompressApplyInput {
    #[schemars(description = "IDs of memories to compress")]
    source_ids: Vec<String>,

    #[schemars(description = "Summary for compressed memory")]
    summary: String,

    #[schemars(description = "Content for compressed memory")]
    content: String,

    #[schemars(description = "Logical scopes")]
    scope: Option<Vec<String>>,

    #[schemars(description = "Tags")]
    tags: Option<Vec<String>>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GcInput {
    #[schemars(description = "List only, no delete (default true)")]
    dry_run: Option<bool>,

    #[schemars(description = "Override GC score threshold")]
    threshold: Option<f64>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReindexInput {
    #[schemars(description = "Only re-embed, skip index rebuild")]
    embeddings_only: Option<bool>,

    #[schemars(description = "Only rebuild index, skip embedding")]
    index_only: Option<bool>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListInput {
    #[schemars(description = "Filter by memory types")]
    types: Option<Vec<String>>,

    #[schemars(description = "Filter by epistemic class: fact, observation, decision (OR logic)")]
    epistemic: Option<Vec<String>>,

    #[schemars(description = "Filter by tags (OR logic)")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Filter: active|needsreview|challenged")]
    status: Option<String>,

    #[schemars(description = "Filter by scope (physical or logical)")]
    scope: Option<String>,

    #[schemars(description = "Sort: criticality|created|updated|type (default criticality)")]
    sort_field: Option<String>,

    #[schemars(
        description = "Include memories whose validity window was closed (invalidated). Default false."
    )]
    include_invalidated: Option<bool>,

    #[schemars(description = "Reverse sort order")]
    reverse: Option<bool>,

    #[schemars(description = "Maximum results")]
    limit: Option<usize>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct StatsInput {
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
    #[schemars(
        description = "If true, include a `by_project` map breaking down runtime telemetry per project ID. Default false."
    )]
    #[serde(default)]
    all_projects: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ConfigInput {
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
    #[schemars(
        description = "Number of top unique tags to include (most-used first). Default 20."
    )]
    #[serde(default)]
    top_tags: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DoctorInput {
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
    #[schemars(
        description = "Flip memories with epistemic findings (changed invalidation paths, invalid derived-from sources) to needs_review. Default false: report only."
    )]
    fix: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectsInfoInput {
    #[schemars(
        description = "Target project: absolute path or 16-char project ID. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectsLinkInput {
    #[schemars(description = "Project ID of the child to link (16-char hex)")]
    child: String,

    #[schemars(description = "Project ID of the parent (16-char hex)")]
    parent: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectsUnlinkInput {
    #[schemars(description = "Project ID of the child to promote back to a root (16-char hex)")]
    project_id: String,
}

// ---------------------------------------------------------------------------
// Serialisable output helpers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CreateOutput {
    id: String,
    created: bool,
    summary: String,
}

#[derive(Serialize)]
struct MemoryOutput {
    id: String,
    #[serde(rename = "type")]
    type_: String,
    summary: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
    physical: Vec<String>,
    logical: Vec<String>,
    tags: Vec<String>,
    criticality: f64,
    confidence: f64,
    status: String,
    /// `shared` memories travel with the repo (`.engramdb/memories/` arrives
    /// via `git clone`), so their content is repo-authored; `personal` ones
    /// are the local user's. Surfaced so the consuming agent can weigh trust.
    visibility: String,
    /// Who recorded the memory: human|agent|inferred|imported.
    provenance: String,
    /// Epistemic class: fact|observation|decision (always present, §5.4).
    epistemic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_while: Option<engramdb::types::Validity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    valid_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invalidated_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    superseded_by: Option<String>,
    /// When the memory was last re-confirmed via `verify` (facts decay from
    /// this anchor, so agents can see how stale a verification is).
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn memory_to_output(m: &engramdb::types::Memory, include_details: bool) -> MemoryOutput {
    MemoryOutput {
        id: m.id.clone(),
        type_: format!("{:?}", m.type_).to_lowercase(),
        summary: m.summary.clone(),
        content: m.content.clone(),
        details: if include_details {
            m.details.clone()
        } else {
            None
        },
        physical: m.physical.clone(),
        logical: m.logical.clone(),
        tags: m.tags.clone(),
        criticality: m.criticality,
        confidence: m.confidence,
        status: format!("{:?}", m.status).to_lowercase(),
        visibility: format!("{:?}", m.visibility).to_lowercase(),
        provenance: format!("{:?}", m.provenance.source).to_lowercase(),
        epistemic: m.epistemic.as_str().to_string(),
        valid_while: m.valid_while.clone(),
        valid_from: m.valid_from,
        invalidated_at: m.invalidated_at,
        superseded_by: m.superseded_by.clone(),
        verified_at: m.verified_at,
    }
}

/// True when an ops error bottoms out in a missing-id storage error.
///
/// Only a genuine `StorageError::NotFound` may map to
/// `ErrorCode::MemoryNotFound`; validation and I/O failures must not
/// masquerade as "not found" (anyhow's downcast sees through `.context()`
/// layers, so the typed check works across the ops chain).
fn is_not_found(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<engramdb::storage::StorageError>(),
        Some(engramdb::storage::StorageError::NotFound(_))
    )
}

#[derive(Serialize)]
struct ScoreBreakdownOutput {
    final_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keyword: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rerank: Option<f64>,
    relevance: f64,
    scope: f64,
    scope_multiplier: f64,
    trust: f64,
    trust_multiplier: f64,
    situation_multiplier: f64,
    decay: f64,
    criticality: f64,
}

#[derive(Serialize)]
struct ScoredMemoryOutput {
    #[serde(flatten)]
    memory: MemoryOutput,
    score: f64,
    score_breakdown: ScoreBreakdownOutput,
}

// ---------------------------------------------------------------------------
// Server struct
// ---------------------------------------------------------------------------

/// The EngramDB MCP server.
#[derive(Clone)]
pub struct EngramDbServer {
    /// Original directory the server was launched in.  In a linked git
    /// worktree this is the worktree path (not the main project root).
    dir: PathBuf,
    /// Directory used for all storage operations.  Equal to `dir` for normal
    /// projects; for linked worktrees this is the resolved main worktree path
    /// so memory operations route to the main project.
    effective_dir: PathBuf,
    embedding_backend: Option<EmbeddingBackend>,
    registry: Arc<dyn RegistryBackend>,
    /// Process-wide runtime stats collector. Cloned (Arc) into every tool
    /// handler via `StatsScope` and into every retrieval engine via
    /// `with_stats`. For SSE servers this Arc is shared across all per-
    /// connection instances; see `run_sse`.
    stats: Arc<engramdb::telemetry::StatsCollector>,
    /// Stable session identifier used as a telemetry attribute. Sourced
    /// from `CLAUDE_SESSION_ID` (or `MCP_SESSION_ID`) env var if set,
    /// otherwise a fresh per-process UUID. For stdio servers (the
    /// transport Claude Code uses today) this is one-session-per-process,
    /// which matches Claude Code's lifecycle. For HTTP, each per-
    /// connection server in the factory gets a fresh ID.
    session_id: String,
    /// Cache of resolved project_ids, keyed by the literal `project` input
    /// string (or `""` for `None`). Populated lazily on the first
    /// telemetry recording per project. Avoids repeating the synchronous
    /// path canonicalization + `.git/config` read on every tool handler
    /// entry. Project IDs are deterministic, so the cache never needs
    /// invalidation.
    pid_cache: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// Process-wide in-process provider cache used when the daemon is
    /// disabled or unreachable. Keyed by the provider-relevant config
    /// signature so each model loads at most once per process (PR #35).
    provider_cache: ops::ProviderCache,
    /// Process-wide re-resolvable handle to the shared embedding daemon. Unlike
    /// a one-shot cache, it re-validates the daemon on each resolve and
    /// re-spawns a dead one (rate-limited), so a session self-heals when the
    /// daemon idle-exits or is replaced. The heartbeat task keeps it warm.
    daemon: Arc<engramdb::daemon::DaemonCell>,
    /// Embedding-model-change warning computed once at daemon startup for
    /// the primary project, appended to `get_info` instructions so the
    /// connecting agent surfaces it to the user. `None` = no mismatch /
    /// not yet evaluated.
    embedding_warning: Option<String>,
    #[allow(dead_code)]
    tool_router: rmcp::handler::server::tool::ToolRouter<Self>,
}

impl std::fmt::Debug for EngramDbServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngramDbServer")
            .field("dir", &self.dir)
            .field("effective_dir", &self.effective_dir)
            .field("embedding_backend", &self.embedding_backend)
            .finish()
    }
}

impl EngramDbServer {
    pub fn new(dir: PathBuf, embedding_backend: Option<EmbeddingBackend>) -> anyhow::Result<Self> {
        let stats = engramdb::telemetry::StatsCollector::new(
            engramdb::types::EngramConfig::default().stats,
        );
        Self::new_with_stats(dir, embedding_backend, stats)
    }

    /// Construct a server using a pre-built stats collector. SSE startup uses
    /// this so every per-connection server instance pushes into the same
    /// collector.
    pub fn new_with_stats(
        dir: PathBuf,
        embedding_backend: Option<EmbeddingBackend>,
        stats: Arc<engramdb::telemetry::StatsCollector>,
    ) -> anyhow::Result<Self> {
        let registry: Arc<dyn RegistryBackend> = Arc::new(
            FileRegistry::global()
                .map_err(|e| anyhow::anyhow!("Failed to initialize registry: {}", e))?,
        );
        let effective_dir = engramdb::storage::project_id::detect_worktree_main(&dir)
            .unwrap_or_else(|| dir.clone());
        let session_id = resolve_session_id();
        Ok(Self {
            dir,
            effective_dir,
            embedding_backend,
            registry,
            stats,
            session_id,
            pid_cache: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            provider_cache: ops::ProviderCache::new(),
            daemon: Arc::new(engramdb::daemon::DaemonCell::new()),
            embedding_warning: None,
            tool_router: Self::tool_router(),
        })
    }

    /// Replace this server's model caches with process-shared ones.
    ///
    /// The SSE transport builds a fresh `EngramDbServer` per connection. Each
    /// default-constructed server has its own empty [`ops::ProviderCache`] and
    /// daemon cell, so without sharing every HTTP connection would reload
    /// the embedding model (or re-probe/auto-spawn the daemon) on its first
    /// tool call — defeating the load-once-per-process intent. Injecting one
    /// process-wide cache + daemon cell restores it on SSE the same way
    /// stdio gets it for free (single server instance).
    fn with_shared_model_caches(
        mut self,
        provider_cache: ops::ProviderCache,
        daemon: Arc<engramdb::daemon::DaemonCell>,
    ) -> Self {
        self.provider_cache = provider_cache;
        self.daemon = daemon;
        self
    }

    #[cfg(test)]
    pub fn new_with_registry(
        dir: PathBuf,
        embedding_backend: Option<EmbeddingBackend>,
        registry: Arc<dyn RegistryBackend>,
    ) -> Self {
        let stats = engramdb::telemetry::StatsCollector::new(
            engramdb::types::EngramConfig::default().stats,
        );
        let effective_dir = engramdb::storage::project_id::detect_worktree_main(&dir)
            .unwrap_or_else(|| dir.clone());
        let session_id = resolve_session_id();
        Self {
            dir,
            effective_dir,
            embedding_backend,
            registry,
            stats,
            session_id,
            pid_cache: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            provider_cache: ops::ProviderCache::new(),
            daemon: Arc::new(engramdb::daemon::DaemonCell::new()),
            embedding_warning: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Returns a clone of the runtime stats collector. Useful in tests and
    /// for the CLI's process-scoped collector access.
    pub fn stats(&self) -> Arc<engramdb::telemetry::StatsCollector> {
        self.stats.clone()
    }

    /// Returns the session ID this server reports to telemetry.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// If the server was launched in a linked git worktree, make sure the
    /// main project's store exists and register the worktree as a sub-project
    /// of the main project.  No-op otherwise.
    ///
    /// Idempotent: safe to call on every server startup / per-connection
    /// factory invocation.
    pub async fn ensure_hierarchy(&self) -> anyhow::Result<()> {
        if self.dir == self.effective_dir {
            return Ok(());
        }

        // Auto-init the main project's store if it doesn't exist yet.
        let main_engramdb = self.effective_dir.join(".engramdb");
        if !main_engramdb.exists() {
            MemoryStore::init(&self.effective_dir, self.registry.as_ref())
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "Failed to init main project at {}: {}",
                        self.effective_dir.display(),
                        e
                    )
                })?;
        }

        // Consolidate any memories that were written under the worktree's
        // own stray store (e.g. by a CLI invocation before this fix) into
        // the main project so nothing is left stranded.
        engramdb::storage::worktree::consolidate_worktree_into_main(&self.dir, &self.effective_dir)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to consolidate worktree memories: {}", e))?;

        let child_id = engramdb::storage::project_id::compute_project_id(&self.dir);
        let parent_id = engramdb::storage::project_id::compute_project_id(&self.effective_dir);

        // Register (or refresh) the worktree as a sub-project of the main project.
        self.registry
            .update_with_parent(&self.dir, &child_id, Some(&parent_id))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to register worktree in registry: {}", e))?;

        tracing::info!(
            "Resolved git worktree: routing memory operations from {} to main project at {}",
            self.dir.display(),
            self.effective_dir.display()
        );
        Ok(())
    }

    /// Run best-effort, throttled housekeeping for the main project.
    ///
    /// Only fires when the server is operating directly on the main worktree
    /// (`dir == effective_dir`); a session started inside a linked worktree
    /// just links/consolidates via [`Self::ensure_hierarchy`] and skips this.
    /// Cleans up orphan/stale projects and quick-checks the main store's
    /// health. Never errors — failures are logged and swallowed — so it can be
    /// called unconditionally on startup.
    pub async fn maintain_main_project(&self) {
        if self.dir != self.effective_dir {
            return;
        }
        // Honor the project's `[maintenance]` config (best-effort: defaults if
        // absent). The MCP server has no `--no-maintenance` flag, so cli_skip
        // is always false; the env var and config still apply.
        let config_path = self.effective_dir.join(".engramdb").join("config.toml");
        let config = engramdb::storage::config::load_config_or_default(&config_path).await;
        // §11.4 consolidation needs providers, so pass an engine — but only
        // build one when the throttle says the pass will actually run, so a
        // routine (throttled) startup never loads models for nothing. Engine
        // build failure degrades to the engine-less pass (graceful-skip).
        let engine = if engramdb::ops::maintenance_would_run(&config.maintenance, false).await {
            self.build_engine().await.ok()
        } else {
            None
        };
        engramdb::ops::auto_maintain_with_engine(
            &self.effective_dir,
            self.registry.as_ref(),
            &config.maintenance,
            false,
            engine.as_ref(),
        )
        .await;
    }

    /// Returns `true` if the given project override refers to the global store.
    fn is_global(project: Option<&str>) -> bool {
        matches!(project, Some("global"))
    }

    /// Resolve the target project directory from an optional project override.
    ///
    /// - `None`: returns the effective (hierarchy-resolved) dir for the
    ///   default project. In a linked git worktree this is the main
    ///   worktree's path.
    /// - `"global"`: returns the global store directory.
    /// - 16-char hex: looked up by project ID in the registry, then follows
    ///   `parent_project_id` links so worktree IDs transparently resolve to
    ///   their root project.
    /// - absolute path: canonicalized.  If the path is a linked worktree,
    ///   swaps to the main worktree's path.  The resulting project ID must
    ///   be present in the registry.
    async fn resolve_dir(&self, project: Option<&str>) -> Result<PathBuf, String> {
        let input = match project {
            None => return Ok(self.effective_dir.clone()),
            Some("global") => {
                return engramdb::storage::paths::global_store_dir()
                    .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()));
            }
            Some(s) => s,
        };

        let is_project_id = input.len() == 16 && input.chars().all(|c| c.is_ascii_hexdigit());

        if is_project_id {
            let registry = self
                .registry
                .load()
                .await
                .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
            // Resolve parent chain so that passing a worktree's ID routes to
            // the main project's storage.
            let root_id = engramdb::storage::resolve_root_project_id(&registry, input);
            match registry.projects.iter().find(|e| e.project_id == root_id) {
                Some(e) => Ok(PathBuf::from(&e.project_path)),
                None => Err(error_response(
                    ErrorCode::ProjectNotFound,
                    &format!(
                        "Project ID '{}' not found in registry. Run `engramdb init` in the target project first.",
                        input
                    ),
                )),
            }
        } else {
            let path = PathBuf::from(input);
            if !path.is_absolute() {
                return Err(error_response(
                    ErrorCode::ValidationError,
                    "Project path must be absolute, not relative.",
                ));
            }
            let canonical = path.canonicalize().map_err(|e| {
                error_response(
                    ErrorCode::ProjectNotFound,
                    &format!("Cannot access directory '{}': {}.", input, e),
                )
            })?;
            // If the caller pointed at a linked worktree, swap to the main
            // worktree's project root so storage ops hit the right place.
            let effective = engramdb::storage::project_id::detect_worktree_main(&canonical)
                .unwrap_or(canonical);
            let project_id = engramdb::storage::project_id::compute_project_id(&effective);
            let registry = self
                .registry
                .load()
                .await
                .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
            if !registry.projects.iter().any(|e| e.project_id == project_id) {
                return Err(error_response(
                    ErrorCode::ProjectNotFound,
                    &format!(
                        "Project at '{}' (id: {}) not found in registry. Run `engramdb init` there first.",
                        input, project_id
                    ),
                ));
            }
            Ok(effective)
        }
    }

    /// Open a MemoryStore for the given project override, auto-initializing only for the default project.
    async fn open_store_for(&self, project: Option<&str>) -> Result<MemoryStore, String> {
        if Self::is_global(project) {
            return MemoryStore::open_global()
                .await
                .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()));
        }

        let dir = self.resolve_dir(project).await?;
        let engramdb_dir = dir.join(".engramdb");
        if !engramdb_dir.exists() {
            if project.is_some() {
                return Err(error_response(
                    ErrorCode::StoreNotInitialized,
                    &format!(
                        "Store not initialized at '{}'. Run `engramdb init` there first.",
                        dir.display()
                    ),
                ));
            }
            // Default project: auto-init.  For linked worktrees this targets
            // the main project's root (via effective_dir resolution above)
            // and also registers the worktree as a sub-project.
            MemoryStore::init(&dir, self.registry.as_ref())
                .await
                .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()))?;
            if self.dir != self.effective_dir && dir == self.effective_dir {
                let child_id = engramdb::storage::project_id::compute_project_id(&self.dir);
                let parent_id =
                    engramdb::storage::project_id::compute_project_id(&self.effective_dir);
                self.registry
                    .update_with_parent(&self.dir, &child_id, Some(&parent_id))
                    .await
                    .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
            }
        }
        MemoryStore::open(&dir)
            .await
            .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()))
    }

    /// Open a MemoryStore for the default project, auto-initializing if needed.
    async fn open_store(&self) -> Result<MemoryStore, String> {
        self.open_store_for(None).await
    }

    async fn load_config_for(
        &self,
        project: Option<&str>,
    ) -> Result<engramdb::types::EngramConfig, String> {
        let dir = self.resolve_dir(project).await?;
        let config_path = dir.join(".engramdb").join("config.toml");
        Ok(load_config_or_default(&config_path).await)
    }

    /// Confused-deputy guard for MCP mutating tools.
    ///
    /// Nearly every tool accepts an optional `project` override that
    /// [`Self::resolve_dir`] resolves to *any* project in the global registry,
    /// not just the session's own. A steered agent could therefore mutate a
    /// different registered project on the same machine. This gate — consulted
    /// at the top of every mutating handler — enforces the session's
    /// `[security].allow_cross_project_writes` policy.
    ///
    /// Always `Ok` when:
    /// - `project` is `None` (the session's own project), or
    /// - `project` is `"global"` (the shared global store is the intended
    ///   cross-project store), or
    /// - the override resolves to the session's *own* root project id (a
    ///   worktree of the session's own project is not cross-project), or
    /// - the session's config allows cross-project writes (the default).
    ///
    /// Otherwise (a different registered project, gate off) returns a
    /// structured error and the write is refused.
    async fn check_cross_project_write(&self, project: Option<&str>) -> Result<(), String> {
        // The session's own project and the shared global store are always
        // permitted, without touching the registry or config.
        match project {
            None => return Ok(()),
            Some("global") => return Ok(()),
            Some(_) => {}
        }

        // Resolve the override to its project root exactly as `resolve_dir`
        // does, then compute the root id so a worktree of the target maps to
        // its main project (and a worktree of *our own* project maps to us).
        let target_dir = self.resolve_dir(project).await?;
        let target_id = engramdb::storage::project_id::compute_project_id(&target_dir);
        let own_id = engramdb::storage::project_id::compute_project_id(&self.effective_dir);
        if target_id == own_id {
            return Ok(());
        }

        // Different project: consult the SESSION's own project config.
        let config = self.load_config_for(None).await?;
        if config.security.allow_cross_project_writes {
            return Ok(());
        }

        Err(error_response(
            ErrorCode::ValidationError,
            &format!(
                "Cross-project writes are disabled by [security].allow_cross_project_writes = false. \
                 This session's project (id: {own_id}) may not write to a different registered \
                 project (id: {target_id}). Omit the `project` parameter to write to your own \
                 project, or use project=\"global\" for the shared store.",
            ),
        ))
    }

    /// Build a RetrievalEngine for the given project override.
    ///
    /// Model-backed providers come from the shared embedding daemon when
    /// `daemon.enabled` and a daemon is reachable (so this process loads no
    /// models), otherwise from the process-wide in-process cache so the model
    /// still loads at most once per process. Only the cheap per-store wiring
    /// happens here.
    async fn build_engine_for(&self, project: Option<&str>) -> Result<RetrievalEngine, String> {
        let dir = self.resolve_dir(project).await?;
        let store = self.open_store_for(project).await?;
        let config_path = dir.join(".engramdb").join("config.toml");
        let pid = self.project_id_for_dir(&dir, project);
        let config = load_config_or_default(&config_path).await;
        let mode = config.embeddings.reindex_on_model_change;
        let providers = Self::resolve_providers(
            &self.provider_cache,
            &self.daemon,
            self.embedding_backend,
            &config,
            &dir,
        )
        .await;
        // Strict mode: refuse embedding-dependent work on a model mismatch
        // so the agent gets an actionable error instead of degraded search.
        if mode == engramdb::types::ReindexOnModelChange::Error {
            let current =
                providers
                    .embedding
                    .as_ref()
                    .map(|p| engramdb::storage::EmbeddingFingerprint {
                        model: p.model_id(),
                        dimensions: p.dimensions(),
                    });
            let report = ops::embedding_model_report(&store, current).await;
            if !report.status.is_consistent() {
                return Err(report.warning.unwrap_or_else(|| {
                    "EngramDB: embedding model mismatch; run \
                     `engramdb reindex --embeddings-only`"
                        .to_string()
                }));
            }
        }
        Ok(self.finish_engine(store, config, providers, pid))
    }

    /// Build a retrieval engine **bypassing** the `error`-mode model-mismatch
    /// gate in [`Self::build_engine_for`].
    ///
    /// Remediation paths (`reindex`, `auto`-reindex at startup) must run
    /// precisely when the store is flagged mismatched — going through the
    /// enforcing chokepoint would make `error` mode refuse the one operation
    /// that fixes the mismatch. A genuine engine-build failure (store not
    /// initialized, model load error) is still returned as `Err` so callers
    /// surface it instead of silently degrading to an index-only reindex.
    async fn assemble_engine_for(&self, project: Option<&str>) -> Result<RetrievalEngine, String> {
        let dir = self.resolve_dir(project).await?;
        let store = self.open_store_for(project).await?;
        let config_path = dir.join(".engramdb").join("config.toml");
        let pid = self.project_id_for_dir(&dir, project);
        let config = load_config_or_default(&config_path).await;
        let providers = Self::resolve_providers(
            &self.provider_cache,
            &self.daemon,
            self.embedding_backend,
            &config,
            &dir,
        )
        .await;
        Ok(self.finish_engine(store, config, providers, pid))
    }

    /// Shared tail of [`Self::build_engine_for`] (enforcing) and
    /// [`Self::assemble_engine_for`] (non-enforcing remediation path): wire
    /// the per-store engine from already-resolved pieces. Kept as one place
    /// so the two construction paths can never drift apart.
    fn finish_engine(
        &self,
        store: MemoryStore,
        config: engramdb::types::EngramConfig,
        providers: ops::EngineProviders,
        pid: String,
    ) -> RetrievalEngine {
        ops::assemble_engine(store, config, providers)
            .with_stats(self.stats.clone())
            .with_project_id(pid)
            .with_session_id(Some(self.session_id.clone()))
    }

    /// Evaluate the primary project's embedding-model identity for the
    /// startup warning. Best-effort: any failure or `off` mode → `None`
    /// (never blocks startup). Returns `(mode, report)`.
    async fn embedding_startup_report(
        &self,
    ) -> Option<(
        engramdb::types::ReindexOnModelChange,
        ops::EmbeddingModelReport,
    )> {
        // Best-effort, but never *silently* skipped: a transient I/O failure
        // resolving the project or opening the store would otherwise drop the
        // model-change warning with no trace at all. Log and bail instead.
        let dir = match self.resolve_dir(None).await {
            Ok(dir) => dir,
            Err(e) => {
                tracing::warn!(
                    "EngramDB: skipping embedding model-change check \
                     (cannot resolve project dir): {e}"
                );
                return None;
            }
        };
        let store = match self.open_store_for(None).await {
            Ok(store) => store,
            Err(e) => {
                tracing::warn!(
                    "EngramDB: skipping embedding model-change check \
                     (cannot open store): {e}"
                );
                return None;
            }
        };
        let config_path = dir.join(".engramdb").join("config.toml");
        let config = load_config_or_default(&config_path).await;
        let mode = config.embeddings.reindex_on_model_change;
        if mode == engramdb::types::ReindexOnModelChange::Off {
            return None;
        }
        let providers = Self::resolve_providers(
            &self.provider_cache,
            &self.daemon,
            self.embedding_backend,
            &config,
            &dir,
        )
        .await;
        let current =
            providers
                .embedding
                .as_ref()
                .map(|p| engramdb::storage::EmbeddingFingerprint {
                    model: p.model_id(),
                    dimensions: p.dimensions(),
                });
        Some((mode, ops::embedding_model_report(&store, current).await))
    }

    /// Re-embed the primary project's memories and stamp the model
    /// fingerprint (used by `reindex_on_model_change = auto` at startup).
    async fn auto_reindex_default(&self) -> anyhow::Result<()> {
        let store = self
            .open_store_for(None)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        let engine = self
            .assemble_engine_for(None)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;
        ops::reindex(&store, Some(&engine), true).await?;
        Ok(())
    }

    /// Whether the shared-daemon path should be taken. Always disabled under
    /// the crate's own `cargo test --lib` (see [`Self::resolve_providers`]).
    ///
    /// `ENGRAMDB_IN_PROCESS` (any truthy value) is a hard override that forces
    /// in-process model loading, mirroring the CLI's `--in-process` flag. The
    /// MCP server has no equivalent flag, so this env var is its only knob —
    /// gating it here disables both provider routing and the daemon heartbeat.
    #[cfg(not(test))]
    fn daemon_path_enabled(config: &engramdb::types::EngramConfig) -> bool {
        config.daemon.enabled && !engramdb::types::in_process_override()
    }

    #[cfg(test)]
    fn daemon_path_enabled(_config: &engramdb::types::EngramConfig) -> bool {
        false
    }

    /// Resolve the model-backed providers for `config` at `dir`.
    ///
    /// Thin policy wrapper over the shared [`engramdb::daemon::resolve_providers_with`]
    /// resolver (the same code path the CLI uses): compute the daemon policy
    /// from config + env + `cfg(test)`, and fall back to the pooled
    /// in-process [`ops::ProviderCache`], which still loads each model at
    /// most once per process (PR #35). Associated rather than a `&self`
    /// method so the warmup task can call it with cloned handles.
    ///
    /// The daemon branch is compiled out under `cfg(test)` (via
    /// [`Self::daemon_path_enabled`]): the crate's own `cargo test --lib`
    /// would otherwise auto-spawn the *test* binary as a daemon (it isn't
    /// the CLI), stalling every server test on the connect/retry budget. The
    /// daemon path has dedicated coverage in [`engramdb::daemon::tests`];
    /// integration tests build the lib without `cfg(test)` so they exercise
    /// it normally.
    async fn resolve_providers(
        provider_cache: &ops::ProviderCache,
        daemon: &Arc<engramdb::daemon::DaemonCell>,
        backend_override: Option<EmbeddingBackend>,
        config: &engramdb::types::EngramConfig,
        dir: &Path,
    ) -> ops::EngineProviders {
        let policy = if Self::daemon_path_enabled(config) {
            engramdb::daemon::DaemonPolicy::ConnectOrSpawn
        } else {
            engramdb::daemon::DaemonPolicy::InProcess
        };
        engramdb::daemon::resolve_providers_with(
            daemon,
            config,
            backend_override,
            dir,
            policy,
            engramdb::daemon::InProcessFallback::Pool(provider_cache),
        )
        .await
    }

    /// Preload the embedding model *before* the first tool call so it isn't
    /// paid on it. With the daemon enabled this spawns/connects the daemon and
    /// warms its model; otherwise it warms the in-process cache.
    ///
    /// Non-blocking: spawns a task and returns immediately. Best-effort —
    /// failures are logged, never fatal. If a tool call races this warmup the
    /// daemon's lock / the cache's async mutex make the model load exactly
    /// once.
    pub fn spawn_provider_warmup(&self) {
        let provider_cache = self.provider_cache.clone();
        let daemon = Arc::clone(&self.daemon);
        let backend = self.embedding_backend;
        let dir = self.effective_dir.clone();
        let config_path = self.effective_dir.join(".engramdb").join("config.toml");
        tokio::spawn(async move {
            let config = load_config_or_default(&config_path).await;
            let _ = Self::resolve_providers(&provider_cache, &daemon, backend, &config, &dir).await;
            tracing::debug!("engine provider warmup complete");
        });
    }

    /// Keep the shared daemon alive while this session runs, and self-heal it
    /// if it dies or is replaced.
    ///
    /// A background task resolves the daemon via the re-resolvable
    /// [`engramdb::daemon::DaemonCell`] every `idle_timeout/3` (min 30s). Each resolve sends
    /// a `Ping` — refreshing the daemon's idle clock so it does not reap while
    /// any session is connected (session-aware idle) — and re-spawns a dead
    /// daemon, updating the shared cell so subsequent tool calls pick up the new
    /// one. Best-effort and non-blocking; a `None` resolve just means the next
    /// tick retries. No-op under `cfg(test)` (the daemon path is disabled there,
    /// mirroring [`Self::resolve_providers`]).
    pub fn spawn_daemon_heartbeat(&self) {
        let daemon = Arc::clone(&self.daemon);
        let config_path = self.effective_dir.join(".engramdb").join("config.toml");
        tokio::spawn(async move {
            loop {
                let config = load_config_or_default(&config_path).await;
                if !Self::daemon_path_enabled(&config) {
                    // Daemon disabled (or test build): re-check periodically in
                    // case the config is edited to enable it.
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    continue;
                }
                let idle = config.daemon.idle_timeout_secs;
                let socket = engramdb::daemon::resolve_socket(None, &config.daemon);
                let _ = daemon
                    .get(
                        &socket,
                        idle,
                        engramdb::daemon::DaemonPolicy::ConnectOrSpawn,
                    )
                    .await;
                let interval = std::time::Duration::from_secs((idle / 3).max(30));
                tokio::time::sleep(interval).await;
            }
        });
    }

    /// Compute the project ID used as the telemetry partition key for the
    /// resolved project directory. Mirrors the convention used by
    /// `engramdb::storage::paths::lancedb_dir`: the global store uses
    /// `GLOBAL_PROJECT_ID`; everything else hashes its directory.
    pub(crate) fn project_id_for_dir(&self, dir: &Path, project: Option<&str>) -> String {
        if Self::is_global(project) {
            return engramdb::storage::paths::GLOBAL_PROJECT_ID.to_string();
        }
        engramdb::storage::project_id::compute_project_id(dir)
    }

    /// Synchronous best-effort project ID resolution used as the telemetry
    /// partition key. Unlike `resolve_dir`, this never touches the registry —
    /// it returns the `effective_dir` ID for the default project, the
    /// well-known constant for the global store, the supplied 16-hex string
    /// when it looks like a project ID, and a hash of the canonicalized path
    /// for path-based overrides. Bogus inputs fall back to the launching
    /// project's ID so stats are never lost.
    ///
    /// Results are cached on `self.pid_cache` so the synchronous filesystem
    /// work (canonicalize + `.git/config` read inside `compute_project_id`)
    /// runs at most once per unique `project` input over the server's
    /// lifetime. Project IDs are deterministic, so the cache never
    /// invalidates.
    pub(crate) fn pid_for_input(&self, project: Option<&str>) -> String {
        let key = project.unwrap_or("").to_string();
        {
            let cache = self.pid_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(v) = cache.get(&key) {
                return v.clone();
            }
        }
        let pid = self.resolve_pid_uncached(project);
        let mut cache = self.pid_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.entry(key).or_insert(pid).clone()
    }

    fn resolve_pid_uncached(&self, project: Option<&str>) -> String {
        match project {
            None => engramdb::storage::project_id::compute_project_id(&self.effective_dir),
            Some("global") => engramdb::storage::paths::GLOBAL_PROJECT_ID.to_string(),
            Some(s) if s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit()) => s.to_string(),
            Some(p) => {
                let path = std::path::PathBuf::from(p);
                if path.is_absolute() {
                    let canonical = path.canonicalize().unwrap_or(path);
                    let effective = engramdb::storage::project_id::detect_worktree_main(&canonical)
                        .unwrap_or(canonical);
                    engramdb::storage::project_id::compute_project_id(&effective)
                } else {
                    // Relative-path inputs are ill-formed (the storage layer
                    // will reject them in `resolve_dir`); fall back to the
                    // launching project so the eventually-failing call still
                    // gets bucketed somewhere recognizable.
                    tracing::warn!(
                        "stats: pid_for_input given relative path {:?}; bucketing under launching project",
                        p
                    );
                    engramdb::storage::project_id::compute_project_id(&self.effective_dir)
                }
            }
        }
    }

    /// Build a `StatsScope` for a tool handler. Drop-on-leave records latency
    /// and success/error against the (project_id, session_id) partition.
    pub(crate) fn scope(
        &self,
        tool: &'static str,
        project: Option<&str>,
    ) -> engramdb::telemetry::StatsScope {
        engramdb::telemetry::StatsScope::new(
            self.stats.clone(),
            tool,
            self.pid_for_input(project),
            self.session_id.clone(),
        )
    }

    /// Build a `StatsScope` bucketed under the synthetic `_system` project.
    /// Use for registry-level operations (`projects_list/link/unlink`)
    /// that aren't tied to any single project — without this they'd skew
    /// per-project usage counts on the launching project.
    ///
    /// The `_system` partition is in-memory only; persistence skips it via
    /// the parent-dir check in `connect_for`.
    pub(crate) fn scope_system(&self, tool: &'static str) -> engramdb::telemetry::StatsScope {
        engramdb::telemetry::StatsScope::new(
            self.stats.clone(),
            tool,
            SYSTEM_PROJECT_ID.to_string(),
            self.session_id.clone(),
        )
    }

    /// Build a RetrievalEngine with optional embeddings support for the default project.
    async fn build_engine(&self) -> Result<RetrievalEngine, String> {
        self.build_engine_for(None).await
    }
}

/// Resolve the session ID for a freshly-constructed server.
///
/// Priority:
/// 1. `CLAUDE_SESSION_ID` env var (future-proofing for [issue
///    anthropics/claude-code#25642](https://github.com/anthropics/claude-code/issues/25642)).
/// 2. `MCP_SESSION_ID` env var (compatible with hypothetical wrappers
///    that propagate the MCP session header into the subprocess env).
/// 3. A fresh UUID v7 generated at server construction time.
///
/// Empty/whitespace env-var values are treated as unset.
///
/// ## HTTP transport caveat
///
/// rmcp 0.15's streamable-HTTP server assigns a real `Mcp-Session-Id`
/// header per session, surfaced in `RequestContext.extensions` per
/// request. We do **not** extract it today — the session ID is set once
/// at server construction. This works correctly under stateful HTTP
/// (rmcp's `LocalSessionManager` invokes the factory once per session),
/// where our UUID coincidentally maps 1:1 with rmcp's. Under stateless
/// HTTP it over-segments: every request gets a fresh server →
/// `unique_sessions` explodes. If/when stateless HTTP becomes a
/// supported mode here, threading `RequestContext` through every
/// handler is the structural fix; until then `MCP_SESSION_ID` lets
/// wrappers pin a stable id externally.
fn resolve_session_id() -> String {
    for var in ["CLAUDE_SESSION_ID", "MCP_SESSION_ID"] {
        if let Ok(v) = std::env::var(var) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    uuid::Uuid::now_v7().to_string()
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl EngramDbServer {
    #[tool(
        name = "create",
        description = "Store a new memory about the project (or globally with project=\"global\"). Use after discovering patterns, decisions, or hazards. Set `epistemic` (fact/observation/decision) when it differs from the type default; state `premise` and `invalidated_by` for decisions and observations."
    )]
    async fn memory_create(
        &self,
        Parameters(input): Parameters<CreateInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("create", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        // The engine already owns an open store — reuse it rather than
        // paying a second `MemoryStore::open` (config load + LanceDB
        // connection) per request on the agent hot path.
        let engine = self.build_engine_for(input.project.as_deref()).await?;
        let store = engine.store();
        // The configured strategy is the deployment default; an explicit
        // per-call `title_strategy` still overrides it. This is what makes
        // `[title] strategy = "t5"` actually take effect for agent creates
        // (and thus exercise the cached/pooled T5 generator).
        let config = self.load_config_for(input.project.as_deref()).await?;
        let type_ = ops::parse_memory_type(&input.type_)
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let visibility = input
            .visibility
            .as_deref()
            .map(ops::parse_visibility)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?
            .unwrap_or(Visibility::Shared);

        // Validate score fields
        let criticality = input.criticality.unwrap_or(0.5);
        ops::validate_score(criticality, "criticality")
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        let confidence = input.confidence.unwrap_or(0.8);
        ops::validate_score(confidence, "confidence")
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        if let Some(floor) = input.decay_floor {
            ops::validate_score(floor, "decay_floor")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }

        let epistemic = input
            .epistemic
            .as_deref()
            .map(ops::parse_epistemic)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        let generality = input
            .generality
            .as_deref()
            .map(ops::parse_generality)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        let valid_from = input
            .valid_from
            .as_deref()
            .map(|s| {
                s.parse::<chrono::DateTime<chrono::Utc>>()
                    .map_err(|e| format!("invalid valid_from timestamp: {e}"))
            })
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, e.as_str()))?;

        let result = ops::create_memory(
            store,
            ops::CreateParams {
                type_,
                content: input.content,
                summary: input.summary,
                physical: input.physical.unwrap_or_default(),
                logical: input.logical.unwrap_or_default(),
                tags: input.tags.unwrap_or_default(),
                criticality,
                confidence,
                details: input.details,
                visibility,
                provenance: Provenance::agent("mcp"),
                supersedes: input.supersedes.unwrap_or_default(),
                epistemic,
                premise: input.premise,
                invalidated_by: input.invalidated_by.unwrap_or_default(),
                origin_task: input.origin_task,
                generality,
                valid_from,
                decay_strategy: input.decay_strategy,
                decay_half_life: input.decay_half_life,
                decay_ttl: input.decay_ttl,
                decay_floor: input.decay_floor,
                title: input.title,
                title_strategy: input
                    .title_strategy
                    .map(|s| TitleStrategy::parse(&s))
                    .transpose()
                    .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?
                    .unwrap_or(config.title.strategy),
                // Run embedding + contradiction detection in the background so the
                // agent isn't blocked on embedding-model inference.
                embed_async: true,
            },
            Some(&engine),
        )
        .await
        .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let r = serde_json::to_string(&CreateOutput {
            id: result.id,
            created: true,
            summary: result.summary,
        })
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "query",
        description = "Query all memories. Use `mode: \"rank\"` to browse memories ranked by relevance to a context (current file path, topic, logical scope) — good before modifying files or when orienting. Use `mode: \"filter\"` to find memories containing specific terms, scopes, or tag matches — good when you have a concrete lookup. Filter mode requires at least one of `query`, `logical`, `path`, or `tags`. Pass `situation` (session_start/file_edit/debugging/design_choice) to reweight results for what you're doing; `include_invalidated: true` to see superseded history."
    )]
    async fn memory_query(
        &self,
        Parameters(input): Parameters<QueryInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("query", input.project.as_deref());
        let mode = match input.mode.as_str() {
            "rank" => RetrievalMode::Rank,
            "filter" => RetrievalMode::Filter,
            other => {
                return Err(error_response(
                    ErrorCode::ValidationError,
                    &format!("mode must be \"rank\" or \"filter\", got {:?}", other),
                ));
            }
        };

        let engine = self.build_engine_for(input.project.as_deref()).await?;

        let type_filter = ops::parse_type_filter(input.types.as_deref())
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let detail_level = ops::parse_detail_level_or_default(input.detail_level.as_deref())
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        // Derive output detail inclusion from the *parsed* enum so it honours
        // the same case-insensitive parsing as the engine (finding #9). A raw
        // `== "full"` compare here dropped details for e.g. "Full"/"FULL".
        let include_details =
            matches!(detail_level, engramdb::retrieval::engine::DetailLevel::Full);

        if let Some(mc) = input.min_criticality {
            ops::validate_score(mc, "min_criticality")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }

        let epistemic_filter = ops::parse_epistemic_filter(input.epistemic.as_deref())
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        let situation = input
            .situation
            .as_deref()
            .map(ops::parse_situation)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let query = RetrievalQuery {
            mode,
            path: input.path,
            logical: input.logical.unwrap_or_default(),
            query: input.query,
            types: type_filter,
            tags: input.tags,
            min_criticality: input.min_criticality,
            max_results: Some(input.max_results.unwrap_or(10)),
            include_expired: Some(input.include_expired.unwrap_or(false)),
            detail_level,
            epistemic: epistemic_filter,
            include_invalidated: input.include_invalidated,
            situation,
        };

        // Merge global memories if requested and not already targeting the
        // global store (shared band — see ops::query_memories_with_global).
        let include_global =
            input.include_global.unwrap_or(false) && !Self::is_global(input.project.as_deref());
        let result = ops::query_memories_with_global(&engine, &query, include_global, || async {
            self.build_engine_for(Some("global")).await.ok()
        })
        .await
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let memories: Vec<ScoredMemoryOutput> = result
            .memories
            .iter()
            .map(|sm| ScoredMemoryOutput {
                memory: memory_to_output(&sm.memory, include_details),
                score: sm.score,
                score_breakdown: ScoreBreakdownOutput {
                    final_score: sm.score_breakdown.final_score,
                    semantic: sm.score_breakdown.semantic,
                    keyword: sm.score_breakdown.keyword,
                    rerank: sm.score_breakdown.rerank,
                    relevance: sm.score_breakdown.relevance,
                    scope: sm.score_breakdown.scope,
                    scope_multiplier: sm.score_breakdown.scope_multiplier,
                    trust: sm.score_breakdown.trust,
                    trust_multiplier: sm.score_breakdown.trust_multiplier,
                    situation_multiplier: sm.score_breakdown.situation_multiplier,
                    decay: sm.score_breakdown.decay,
                    criticality: sm.score_breakdown.criticality,
                },
            })
            .collect();

        let r = serde_json::to_string(&serde_json::json!({
            "memories": memories,
            "total": result.total,
            "retrieval_quality": result.retrieval_quality,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "get",
        description = "Get full content of a specific memory, including details."
    )]
    async fn memory_get(&self, Parameters(input): Parameters<GetInput>) -> Result<String, String> {
        let _scope = self.scope("get", input.project.as_deref());
        let store = self.open_store_for(input.project.as_deref()).await?;
        let memory = ops::get_memory(&store, &input.id).await.map_err(|e| {
            let code = if is_not_found(&e) {
                ErrorCode::MemoryNotFound
            } else {
                // Store I/O failures are not "not found" — the memory may
                // well exist.
                ErrorCode::InternalError
            };
            error_response(code, &e.to_string())
        })?;

        let r = serde_json::to_string(&memory_to_output(&memory, true))
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "update",
        description = "Update an existing memory. Cannot change id or created_at."
    )]
    async fn memory_update(
        &self,
        Parameters(input): Parameters<UpdateInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("update", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        // The engine already owns an open store — reuse it rather than
        // paying a second `MemoryStore::open` per request.
        let engine = self.build_engine_for(input.project.as_deref()).await?;
        let store = engine.store();

        let type_ = input
            .type_
            .as_deref()
            .map(ops::parse_memory_type)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let visibility = input
            .visibility
            .as_deref()
            .map(ops::parse_visibility)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let status = input
            .status
            .as_deref()
            .map(ops::parse_status)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        // Validate score fields
        if let Some(c) = input.criticality {
            ops::validate_score(c, "criticality")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }
        if let Some(c) = input.confidence {
            ops::validate_score(c, "confidence")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }
        if let Some(floor) = input.decay_floor {
            ops::validate_score(floor, "decay_floor")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }

        let epistemic = input
            .epistemic
            .as_deref()
            .map(ops::parse_epistemic)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        let generality = input
            .generality
            .as_deref()
            .map(ops::parse_generality)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        let valid_from = input
            .valid_from
            .as_deref()
            .map(|s| {
                s.parse::<chrono::DateTime<chrono::Utc>>()
                    .map_err(|e| format!("invalid valid_from timestamp: {e}"))
            })
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, e.as_str()))?;

        ops::update_memory(
            store,
            &input.id,
            ops::UpdateParams {
                type_,
                content: input.content,
                summary: input.summary,
                details: input.details,
                physical: input.physical,
                logical: input.logical,
                tags: input.tags,
                tags_add: input.tags_add,
                tags_remove: input.tags_remove,
                criticality: input.criticality,
                confidence: input.confidence,
                visibility,
                title: input.title,
                status,
                supersedes: input.supersedes,
                epistemic,
                premise: input.premise,
                invalidated_by: input.invalidated_by,
                origin_task: input.origin_task,
                generality,
                valid_from,
                clear_validity: input.clear_validity.unwrap_or(false),
                clear_invalidated: input.clear_invalidated.unwrap_or(false),
                decay_strategy: input.decay_strategy,
                decay_half_life: input.decay_half_life,
                decay_ttl: input.decay_ttl,
                decay_floor: input.decay_floor,
                // Run re-embedding + contradiction detection in the background
                // so the agent isn't blocked on embedding-model inference.
                embed_async: true,
            },
            Some(&engine),
        )
        .await
        .map_err(|e| {
            let code = if is_not_found(&e) {
                ErrorCode::MemoryNotFound
            } else if e
                .downcast_ref::<engramdb::storage::StorageError>()
                .is_some()
            {
                // A storage failure other than NotFound is an I/O problem.
                ErrorCode::InternalError
            } else {
                // update_memory's non-storage failures are input validation
                // (score/summary/decay-strategy checks before the write).
                ErrorCode::ValidationError
            };
            error_response(code, &e.to_string())
        })?;

        let r = serde_json::to_string(&serde_json::json!({
            "id": input.id,
            "updated": true
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "delete",
        description = "Permanently delete a memory. Prefer supersedes for corrections."
    )]
    async fn memory_delete(
        &self,
        Parameters(input): Parameters<DeleteInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("delete", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        let store = self.open_store_for(input.project.as_deref()).await?;
        ops::delete_memory(&store, &input.id)
            .await
            .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        let r = serde_json::to_string(&serde_json::json!({
            "id": input.id,
            "deleted": true
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "challenge",
        description = "Flag a memory as potentially incorrect and mark for review."
    )]
    async fn memory_challenge(
        &self,
        Parameters(input): Parameters<ChallengeInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("challenge", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        let store = self.open_store_for(input.project.as_deref()).await?;
        let result = ops::challenge_memory(
            &store,
            &input.id,
            &input.evidence,
            input.source_file.as_deref(),
        )
        .await
        .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        let r = serde_json::to_string(&serde_json::json!({
            "challenged": result.challenged,
            "memory": memory_to_output(&result.memory, true)
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "review",
        description = "List memories needing review: flagged (challenged/needs-review) plus, by the recency trigger, active memories not updated in more than [review].recency_days (default 90). Highest-criticality first."
    )]
    async fn memory_review(
        &self,
        Parameters(input): Parameters<ReviewInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("review", input.project.as_deref());
        let store = self.open_store_for(input.project.as_deref()).await?;

        let type_filter = input
            .type_
            .as_deref()
            .map(ops::parse_memory_type)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        // When the caller omits `stale_after_days`, fall back to the project's
        // configured recency window so the recency trigger is on by default.
        let stale_after_days = match input.stale_after_days {
            Some(days) => Some(days),
            None => {
                self.load_config_for(input.project.as_deref())
                    .await?
                    .review
                    .recency_days
            }
        };

        let params = ops::ReviewParams {
            scope: input.scope,
            max_results: input.max_results,
            type_filter,
            challenged_only: input.challenged_only.unwrap_or(false),
            stale_only: input.stale_only.unwrap_or(false),
            stale_after_days,
        };

        let memories = ops::review_memories(&store, &params)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let outputs: Vec<MemoryOutput> = memories
            .iter()
            .map(|m| memory_to_output(m, false))
            .collect();

        let r = serde_json::to_string(&serde_json::json!({
            "memories": outputs,
            "total": memories.len(),
            "recency_days": stale_after_days,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "resolve",
        description = "Resolve a challenged or needs_review memory: keep, update, delete, or invalidate. Prefer invalidate over delete when a memory WAS true but no longer is — history is kept and queryable via include_invalidated."
    )]
    async fn memory_resolve(
        &self,
        Parameters(input): Parameters<ResolveInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("resolve", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        let store = self.open_store_for(input.project.as_deref()).await?;

        let action = match input.action.as_str() {
            "keep" => ops::ResolveAction::Keep,
            "update" => ops::ResolveAction::Update,
            "delete" => ops::ResolveAction::Delete,
            "invalidate" => ops::ResolveAction::Invalidate,
            other => {
                return Err(error_response(
                    ErrorCode::ValidationError,
                    &format!(
                        "Invalid action '{}'. Must be keep, update, delete, or invalidate.",
                        other
                    ),
                ));
            }
        };

        let result = ops::resolve_memory(
            &store,
            ops::ResolveParams {
                id: input.id.clone(),
                action,
                updated_content: input.updated_content,
                updated_summary: input.updated_summary,
                superseded_by: input.superseded_by,
            },
        )
        .await
        .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        let r = serde_json::to_string(&serde_json::json!({
            "id": input.id,
            "action": result.action,
            "resolved": result.resolved
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "verify",
        description = "Confirm a memory is still accurate after checking it against the code. Stamps verified_at (facts rank fresher) and clears doctor-flagged needs_review."
    )]
    async fn memory_verify(
        &self,
        Parameters(input): Parameters<VerifyInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("verify", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        let store = self.open_store_for(input.project.as_deref()).await?;

        let result = ops::verify_memory(&store, &input.id)
            .await
            .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        let r = serde_json::to_string(&serde_json::json!({
            "id": result.id,
            "verified": true,
            "review_cleared": result.review_cleared,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "task_current",
        description = "Declare the task/feature this session is working on. Task-scoped memories from other tasks stay suppressed; yours surface. Call without a task to read the current declaration."
    )]
    async fn memory_task_current(
        &self,
        Parameters(input): Parameters<TaskCurrentInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("task_current", input.project.as_deref());
        let dir = self.resolve_dir(input.project.as_deref()).await?;

        let result = ops::task_current(&dir, &self.session_id, input.task.as_deref())
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let r = serde_json::to_string(&serde_json::json!({
            "session_id": result.session_id,
            "task": result.task,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "task_complete",
        description = "Mark a task/feature finished. Its task-scoped decisions start decaying unless promoted; project-wide memories from the task are listed for review."
    )]
    async fn memory_task_complete(
        &self,
        Parameters(input): Parameters<TaskCompleteInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("task_complete", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        let store = self.open_store_for(input.project.as_deref()).await?;

        let config = self.load_config_for(input.project.as_deref()).await?;
        let result = ops::task_complete(&store, &input.task, &config.epistemic)
            .await
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let notices: Vec<serde_json::Value> = result
            .project_wide_notices
            .iter()
            .map(|(id, summary)| {
                serde_json::json!({
                    "id": id,
                    "summary": summary,
                    "notice": "project-wide memory from a completed task — verify or demote",
                })
            })
            .collect();
        let r = serde_json::to_string(&serde_json::json!({
            "task": input.task,
            "demoted": result.demoted,
            "kept_custom_decay": result.kept_custom_decay,
            "project_wide_review": notices,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "compress_candidates",
        description = "List low-criticality memories eligible for compression. Review before compress_apply."
    )]
    async fn memory_compress_candidates(
        &self,
        Parameters(input): Parameters<CompressCandidatesInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("compress_candidates", input.project.as_deref());
        if let Some(t) = input.threshold {
            ops::validate_score(t, "threshold")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }
        let store = self.open_store_for(input.project.as_deref()).await?;
        let result = ops::compress_candidates(&store, input.scope.as_deref(), input.threshold)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let r = serde_json::to_string(&serde_json::json!({
            "candidates": result.candidates,
            "total": result.total,
            "threshold": result.threshold,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "compress_apply",
        description = "Compress multiple memories into one summary. Call compress_candidates first."
    )]
    async fn memory_compress_apply(
        &self,
        Parameters(input): Parameters<CompressApplyInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("compress_apply", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        // Build the full engine (not just a store): the replacement summary
        // must be embedded — its sources had vectors and are deleted below,
        // so an un-embedded summary would be invisible to semantic search.
        // `embed_async = true` matches the `create` tool's behavior.
        let engine = self.build_engine_for(input.project.as_deref()).await?;
        let result = ops::compress_apply(
            engine.store(),
            ops::CompressApplyParams {
                source_ids: input.source_ids,
                summary: input.summary,
                content: input.content,
                scope: input.scope,
                tags: input.tags,
                embed_async: true,
            },
            Some(&engine),
        )
        .await
        .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let mut response = serde_json::json!({
            "new_id": result.new_id,
            "superseded_count": result.superseded_count,
            "applied": true,
        });
        if !result.skipped_sources.is_empty() {
            // Sources already gone at delete time (deleted concurrently
            // after validation) — the compressed memory is still valid.
            response["skipped_sources"] = serde_json::json!(result.skipped_sources);
        }
        let r = serde_json::to_string(&response)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "stats",
        description = "Overview of memory store — counts by type, scope, status."
    )]
    async fn memory_stats(
        &self,
        Parameters(input): Parameters<StatsInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("stats", input.project.as_deref());
        let store = self.open_store_for(input.project.as_deref()).await?;
        let stats = ops::compute_stats(&store)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let by_type: serde_json::Map<String, serde_json::Value> = stats
            .by_type
            .iter()
            .map(|(t, c)| {
                (
                    format!("{:?}", t).to_lowercase(),
                    serde_json::Value::Number((*c).into()),
                )
            })
            .collect();

        let by_status: serde_json::Map<String, serde_json::Value> = stats
            .by_status
            .iter()
            .map(|(s, c)| {
                (
                    format!("{:?}", s).to_lowercase(),
                    serde_json::Value::Number((*c).into()),
                )
            })
            .collect();

        let by_scope: serde_json::Map<String, serde_json::Value> = stats
            .by_scope
            .iter()
            .map(|(s, c)| (s.clone(), serde_json::Value::Number((*c).into())))
            .collect();

        // Runtime telemetry overlay: per-project counters, hit-rate, response
        // timings, per-tool usage. Merged at the top level next to the
        // existing static fields — schema is purely additive.
        let pid = self.pid_for_input(input.project.as_deref());
        let runtime = self
            .stats
            .snapshot(&pid, input.all_projects.unwrap_or(false));
        let runtime_value = serde_json::to_value(&runtime).unwrap_or(serde_json::Value::Null);

        let mut payload = serde_json::json!({
            "total": stats.total,
            "by_type": by_type,
            "by_status": by_status,
            "by_scope": by_scope,
            "expired": stats.expired,
            "oldest": stats.oldest,
            "newest": stats.newest,
            "avg_criticality": stats.avg_criticality,
        });
        if let serde_json::Value::Object(rt_obj) = runtime_value {
            if let serde_json::Value::Object(ref mut p_obj) = payload {
                for (k, v) in rt_obj {
                    p_obj.insert(k, v);
                }
            }
        }

        let r = serde_json::to_string(&payload)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "config",
        description = "Effective config values and store vocabulary to help use the tools well: summary/content limits to respect on `create`, retrieval/search thresholds and result caps, which optional features (rerank, contradiction detection) are on, and the top unique tags already in memory."
    )]
    async fn memory_config(
        &self,
        Parameters(input): Parameters<ConfigInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("config", input.project.as_deref());
        let config = self.load_config_for(input.project.as_deref()).await?;
        let store = self.open_store_for(input.project.as_deref()).await?;

        let view = ops::AgentConfigView::from_config(&config);
        let limit = input.top_tags.unwrap_or(ops::DEFAULT_TOP_TAGS);
        let tags = ops::top_tags(&store, limit)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let mut payload = serde_json::to_value(&view)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        if let serde_json::Value::Object(ref mut obj) = payload {
            obj.insert(
                "top_tags".to_string(),
                serde_json::to_value(&tags)
                    .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?,
            );
        }

        let r = serde_json::to_string(&payload)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "gc",
        description = "Garbage collect decayed memories. Always dry_run first."
    )]
    async fn memory_gc(&self, Parameters(input): Parameters<GcInput>) -> Result<String, String> {
        let _scope = self.scope("gc", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        if let Some(t) = input.threshold {
            ops::validate_score(t, "threshold")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }
        let store = self.open_store_for(input.project.as_deref()).await?;
        let config = self.load_config_for(input.project.as_deref()).await?;
        let dry_run = input.dry_run.unwrap_or(true);

        let result = ops::gc_memories(&store, &config, dry_run, input.threshold)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let mut response = serde_json::json!({
            "removed": result.removed,
            "count": result.count,
            "dry_run": dry_run
        });
        if !result.skipped.is_empty() {
            // Candidates skipped at delete time: concurrently deleted, or
            // modified/re-scored above the threshold under the write lock.
            response["skipped"] = serde_json::json!(result.skipped);
        }
        if !result.stale_entries.is_empty() {
            response["stale_entries"] = serde_json::json!(result.stale_entries);
            response["warning"] =
                serde_json::json!("Stale index entries found. Run reindex to fix.");
        }
        if let Some(m) = &result.maintenance {
            // Post-deletion index maintenance (compaction + version pruning).
            response["index_bytes_reclaimed"] = serde_json::json!(m.bytes_removed);
        }
        let r = serde_json::to_string(&response)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "reindex",
        description = "Rebuild the search index and embedding vectors."
    )]
    async fn memory_reindex(
        &self,
        Parameters(input): Parameters<ReindexInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("reindex", input.project.as_deref());
        self.check_cross_project_write(input.project.as_deref())
            .await?;
        let embeddings_only = input.embeddings_only.unwrap_or(false);
        let index_only = input.index_only.unwrap_or(false);

        // Build engine outside conditional so it stays alive for the
        // reference. `reindex` is the remediation path for an embedding
        // model mismatch, so it must NOT go through the `error`-mode gate
        // in `build_engine_for` (that would refuse the very operation that
        // fixes the mismatch). A genuine build failure is surfaced rather
        // than silently downgrading to an index-only reindex reported as
        // success. When the engine is built, reuse its store instead of
        // opening a second one.
        let (engine, opened_store) = if !index_only {
            let engine = self
                .assemble_engine_for(input.project.as_deref())
                .await
                .map_err(|e| error_response(ErrorCode::InternalError, &e))?;
            (Some(engine), None)
        } else {
            let store = self.open_store_for(input.project.as_deref()).await?;
            (None, Some(store))
        };
        let store = engine
            .as_ref()
            .map(|e| e.store())
            .or(opened_store.as_ref())
            .expect("either the engine or the direct open supplies a store");

        let result = ops::reindex(store, engine.as_ref(), embeddings_only)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let r = serde_json::to_string(&serde_json::json!({
            "indexed": result.indexed,
            "embedded": result.embedded,
            "errors": result.errors,
            "warnings": result.warnings
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "list",
        description = "List memories with optional filtering, sorting, and limiting."
    )]
    async fn memory_list(
        &self,
        Parameters(input): Parameters<ListInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("list", input.project.as_deref());
        let store = self.open_store_for(input.project.as_deref()).await?;

        let sort_field =
            ops::parse_sort_field(input.sort_field.as_deref().unwrap_or("criticality"))
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let params = ops::ListParams {
            types: input.types,
            epistemic: input.epistemic,
            tags: input.tags,
            status: input.status,
            scope: input.scope,
            sort_field,
            reverse: input.reverse.unwrap_or(false),
            limit: input.limit,
            include_invalidated: input.include_invalidated.unwrap_or(false),
        };

        let entries = ops::list_memories(&store, &params)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let output: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                let mut obj = serde_json::json!({
                    "id": e.id,
                    "type": format!("{:?}", e.type_).to_lowercase(),
                    // §5.4: epistemic is always present in MCP list output.
                    "epistemic": e.epistemic.as_str(),
                    "summary": e.summary,
                    "tags": e.tags,
                    "logical": e.logical,
                    "physical": e.physical,
                    "status": format!("{:?}", e.status).to_lowercase(),
                    "criticality": e.criticality,
                    "created_at": e.created_at.to_rfc3339(),
                    "updated_at": e.updated_at.to_rfc3339(),
                });
                // §5.4: emitted when present. `invalidated_at` is normally
                // only reachable via include_invalidated (or future-dated);
                // `valid_from` appears on any backdated live memory.
                if let Some(t) = e.invalidated_at {
                    obj["invalidated_at"] = serde_json::json!(t.to_rfc3339());
                }
                if let Some(t) = e.valid_from {
                    obj["valid_from"] = serde_json::json!(t.to_rfc3339());
                }
                obj
            })
            .collect();

        let r = serde_json::to_string(&serde_json::json!({
            "memories": output,
            "total": output.len()
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "doctor",
        description = "Check store health (index vs disk consistency) plus epistemic checks: changed invalidation paths, stale observations, invalid derived-from sources. Pass fix: true to flip affected memories to needs_review. For full environment diagnostics, use the CLI: `engramdb doctor`."
    )]
    async fn memory_doctor(
        &self,
        Parameters(input): Parameters<DoctorInput>,
    ) -> Result<String, String> {
        let fix = input.fix.unwrap_or(false);
        if fix {
            self.check_cross_project_write(input.project.as_deref())
                .await?;
        }
        let _scope = self.scope("doctor", input.project.as_deref());
        let store = self.open_store_for(input.project.as_deref()).await?;
        let result = ops::doctor(&store)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        // §10 epistemic checks run alongside the index-consistency check.
        let config = self.load_config_for(input.project.as_deref()).await?;
        let epistemic = ops::doctor_epistemic(&store, &config, fix)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let mut response = serde_json::json!({
            "healthy": result.healthy,
            "indexed": result.indexed,
            "on_disk": result.on_disk,
        });
        if !result.stale_entries.is_empty() {
            response["stale_entries"] = serde_json::json!(result.stale_entries);
        }
        if !result.orphaned_files.is_empty() {
            response["orphaned_files"] = serde_json::json!(result.orphaned_files);
        }
        if !result.healthy {
            response["fix"] = serde_json::json!("Run reindex to repair.");
        }
        if !epistemic.findings.is_empty() {
            response["epistemic_findings"] = serde_json::json!(epistemic.findings);
        }
        // Report-only enrichment nudge: classes without actionable metadata
        // (legacy pre-epistemic memories all start this way).
        if epistemic.gaps.any() {
            response["enrichment_gaps"] = serde_json::json!(epistemic.gaps);
        }
        let r = serde_json::to_string(&response)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "projects_list",
        description = "List all registered EngramDB projects, including hierarchy (parent_project_id). Useful for finding the 16-char IDs you pass as the `project` parameter to other tools."
    )]
    async fn projects_list(&self) -> Result<String, String> {
        let _scope = self.scope_system("projects_list");
        let entries = ops::projects::list_projects(self.registry.as_ref())
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let json: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|e| {
                let mut obj = serde_json::json!({
                    "project_id": e.project_id,
                    "project_path": e.project_path,
                    "exists": e.exists,
                });
                if let Some(parent) = e.parent_project_id {
                    obj["parent_project_id"] = serde_json::Value::String(parent);
                }
                obj
            })
            .collect();

        let r = serde_json::to_string(&json)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "projects_info",
        description = "Info about a specific project: id, name, path, memory count, logical scopes, created_at, parent_project_id. Omit `project` for the current project."
    )]
    async fn projects_info(
        &self,
        Parameters(input): Parameters<ProjectsInfoInput>,
    ) -> Result<String, String> {
        let _scope = self.scope("projects_info", input.project.as_deref());
        let dir = self.resolve_dir(input.project.as_deref()).await?;
        let info = ops::projects::get_project_info(&dir)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let mut obj = serde_json::json!({
            "project_id": info.project_id,
            "project_name": info.project_name,
            "project_path": info.project_path,
            "memory_count": info.memory_count,
            "logical_scopes": info.logical_scopes,
            "created_at": info.created_at,
        });
        if let Some(parent) = info.parent_project_id {
            obj["parent_project_id"] = serde_json::Value::String(parent);
        }
        let r = serde_json::to_string(&obj)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "projects_link",
        description = "Link a registered project as a sub-project of another. Rejects self-links and cycles. Use projects_list to discover project IDs."
    )]
    async fn projects_link(
        &self,
        Parameters(input): Parameters<ProjectsLinkInput>,
    ) -> Result<String, String> {
        let _scope = self.scope_system("projects_link");
        // (`projects_link` is registry-level — no per-project bucket)
        ops::projects::link_project(self.registry.as_ref(), &input.child, &input.parent)
            .await
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        let r = serde_json::to_string(&serde_json::json!({
            "linked": true,
            "child": input.child,
            "parent": input.parent,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }

    #[tool(
        name = "projects_unlink",
        description = "Remove the parent link on a project, promoting it back to a root project. No-op if the project has no parent."
    )]
    async fn projects_unlink(
        &self,
        Parameters(input): Parameters<ProjectsUnlinkInput>,
    ) -> Result<String, String> {
        let _scope = self.scope_system("projects_unlink");
        ops::projects::unlink_project(self.registry.as_ref(), &input.project_id)
            .await
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        let r = serde_json::to_string(&serde_json::json!({
            "unlinked": true,
            "project_id": input.project_id,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;
        _scope.mark_success();
        Ok(r)
    }
}

impl EngramDbServer {
    /// Names of every MCP tool this server exposes, in router order.
    ///
    /// Public so front-ends that maintain tool allowlists (the CLI's `setup`
    /// command writes `permissions.allow` entries per tool) can pin their
    /// lists against the actual tool surface instead of drifting silently.
    pub fn tool_names() -> Vec<String> {
        Self::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// ServerHandler implementation
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for EngramDbServer {
    fn get_info(&self) -> ServerInfo {
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        // rmcp 1.x marks these result/info structs `#[non_exhaustive]`, so build
        // them via the provided constructors/builders instead of struct literals.
        let server_info = Implementation::new("engramdb", env!("CARGO_PKG_VERSION"));
        let instructions = {
            let mut s = "Project-scoped persistent memory store for coding agents. \
                 Stores decisions, hazards, conventions, and context about the codebase. \
                 IMPORTANT: Query memories (query) before answering project questions, \
                 investigating workflows, or researching how things work — not only before \
                 modifying files. Use mode=\"filter\" with a query/logical/path/tags signal \
                 for specific lookups, mode=\"rank\" for context-aware browsing. \
                 Store new knowledge after significant discoveries — for decisions, \
                 state the premise (premise) and what would invalidate them \
                 (invalidated_by); set epistemic (fact/observation/decision) when it \
                 differs from the type default, and origin_task + generality=\"task\" \
                 for task-specific choices. Declare situation on query when debugging \
                 or weighing a design choice. \
                 All tools accept an optional `project` parameter (absolute path, 16-char \
                 project ID, or \"global\") to operate on a different project's memories. \
                 Use project=\"global\" for cross-project memories like personal preferences, \
                 coding conventions, or knowledge that applies everywhere. \
                 Use include_global=true on query to merge global memories into results. \
                 Omit `project` to use the current project. \
                 When you finish the task you were assigned, reflect: if anything durable \
                 about the project, the environment/tooling, or the user's preferences came \
                 up (not task minutiae), query existing memories, then create the new ones \
                 and challenge contradictions. Suggested, not required."
                .to_string();
            if let Some(w) = &self.embedding_warning {
                s.push_str("\n\nIMPORTANT — ACTION NEEDED: ");
                s.push_str(w);
                s.push_str(" Tell the user.");
            }
            s
        };
        InitializeResult::new(capabilities)
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_server_info(server_info)
            .with_instructions(instructions)
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_
    {
        std::future::ready(Ok(ListResourcesResult {
            meta: None,
            next_cursor: None,
            resources: vec![RawResource {
                uri: "memory://index".to_string(),
                name: "EngramDB Store Index".to_string(),
                title: None,
                description: Some(
                    "Lightweight index of all memories with summaries, scopes, tags, and scores."
                        .to_string(),
                ),
                mime_type: Some("application/json".to_string()),
                size: None,
                icons: None,
                meta: None,
            }
            .no_annotation()],
        }))
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourceTemplatesResult, rmcp::ErrorData>>
           + Send
           + '_ {
        std::future::ready(Ok(ListResourceTemplatesResult {
            meta: None,
            next_cursor: None,
            resource_templates: vec![RawResourceTemplate {
                uri_template: "memory://context/{path}".to_string(),
                name: "Contextual Memories".to_string(),
                title: None,
                description: Some(
                    "Memories relevant to the given file path, scored and sorted.".to_string(),
                ),
                mime_type: Some("application/json".to_string()),
                icons: None,
            }
            .no_annotation()],
        }))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, rmcp::ErrorData>> + Send + '_
    {
        let uri = request.uri;
        async move {
            if uri == "memory://index" {
                let store = self
                    .open_store()
                    .await
                    .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
                let entries = store
                    .list_filterable()
                    .await
                    .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

                let index: Vec<serde_json::Value> = entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "id": e.id,
                            "type": format!("{:?}", e.type_).to_lowercase(),
                            "summary": e.summary,
                            "tags": e.tags,
                            "logical": e.logical,
                            "status": format!("{:?}", e.status).to_lowercase(),
                            "criticality": e.criticality,
                        })
                    })
                    .collect();

                let json = serde_json::to_string(&index)
                    .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

                Ok(ReadResourceResult::new(vec![ResourceContents::text(
                    json,
                    "memory://index",
                )]))
            } else if let Some(path) = uri.strip_prefix("memory://context/") {
                let engine = self
                    .build_engine()
                    .await
                    .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

                let query = RetrievalQuery {
                    mode: RetrievalMode::Rank,
                    path: Some(path.to_string()),
                    max_results: Some(10),
                    ..RetrievalQuery::default()
                };
                let result = ops::query_memories(&engine, &query)
                    .await
                    .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

                let memories: Vec<serde_json::Value> = result
                    .memories
                    .iter()
                    .map(|sm| {
                        serde_json::json!({
                            "id": sm.memory.id,
                            "type": format!("{:?}", sm.memory.type_).to_lowercase(),
                            "summary": sm.memory.summary,
                            "content": sm.memory.content,
                            "score": sm.score,
                            "status": format!("{:?}", sm.memory.status).to_lowercase(),
                        })
                    })
                    .collect();

                let json = serde_json::to_string(&memories)
                    .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;

                Ok(ReadResourceResult::new(vec![ResourceContents::text(
                    json, &uri,
                )]))
            } else {
                Err(rmcp::ErrorData::invalid_params(
                    format!("Unknown resource URI: {}", uri),
                    None,
                ))
            }
        }
    }

    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListPromptsResult, rmcp::ErrorData>> + Send + '_
    {
        std::future::ready(Ok(ListPromptsResult {
            meta: None,
            next_cursor: None,
            prompts: vec![
                Prompt::new(
                    "memory-session-start",
                    Some("Orientation prompt for the start of a coding session."),
                    Some(vec![PromptArgument::new("path")
                        .with_description("The file or directory the agent will be working on.")
                        .with_required(false)]),
                ),
                Prompt::new(
                    "memory-session-end",
                    Some::<&str>("End-of-session prompt to review and persist learnings."),
                    None,
                ),
            ],
        }))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<GetPromptResult, rmcp::ErrorData> {
        match request.name.as_str() {
            "memory-session-start" => {
                let path = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("path"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let mut memory_text = String::new();

                if let Ok(engine) = self.build_engine().await {
                    let query = RetrievalQuery {
                        mode: RetrievalMode::Rank,
                        path,
                        max_results: Some(10),
                        ..RetrievalQuery::default()
                    };
                    if let Ok(result) = ops::query_memories(&engine, &query).await {
                        for sm in &result.memories {
                            let status_marker = match sm.memory.status {
                                Status::Challenged => " ⚠️",
                                Status::NeedsReview => " 🕐",
                                _ => "",
                            };
                            memory_text.push_str(&format!(
                                "- [{:?}] {}{}\n",
                                sm.memory.type_, sm.memory.summary, status_marker
                            ));
                        }
                    }
                }

                if memory_text.is_empty() {
                    memory_text = "No relevant memories found.\n".to_string();
                }

                let prompt = format!(
                    "You are working on a project with a persistent memory store (EngramDB).\n\
                         Before making changes, review these relevant memories:\n\n\
                         {}\n\
                         Memories marked ⚠️ may be inaccurate.\n\
                         Memories marked 🕐 are flagged for review.\n\n\
                         When you discover important patterns, decisions, or hazards during \
                         this session, store them using the create tool. If a decision holds \
                         only for THIS task, set origin_task and generality: task, and state \
                         its premise ('because C').\n\
                         If you encounter evidence that contradicts an existing memory, \
                         use challenge and ask the user how to resolve it.",
                    memory_text
                );

                let mut result = GetPromptResult::new(vec![PromptMessage::new_text(
                    PromptMessageRole::User,
                    prompt,
                )]);
                result.description = Some("Session start briefing".to_string());
                Ok(result)
            }
            "memory-session-end" => {
                let mut stats_text = String::new();
                let mut recency_hint = String::new();
                if let Ok(store) = self.open_store().await {
                    if let Ok(stats) = ops::compute_stats(&store).await {
                        let review_count = stats
                            .by_status
                            .iter()
                            .filter(|(s, _)| matches!(s, Status::NeedsReview | Status::Challenged))
                            .map(|(_, c)| c)
                            .sum::<usize>();
                        stats_text = format!(
                            "Current store has {} memories ({} need review).",
                            stats.total, review_count
                        );
                    }

                    // Recency trigger: nudge the agent to revisit memories that
                    // have gone stale (active, untouched past the configured
                    // window) so long-lived stores don't quietly rot.
                    let recency_days = self
                        .load_config_for(None)
                        .await
                        .ok()
                        .and_then(|c| c.review.recency_days);
                    if let Some(days) = recency_days {
                        if let Ok(stale) = ops::count_recency_stale(&store, Some(days)).await {
                            if stale > 0 {
                                recency_hint =
                                    format!(
                                    "\n{} active {} not been touched in over {} days and may be \
                                     stale — run review (or review stale_after_days: {}) to \
                                     confirm or retire {}.",
                                    stale,
                                    if stale == 1 { "memory has" } else { "memories have" },
                                    days,
                                    days,
                                    if stale == 1 { "it" } else { "them" },
                                );
                            }
                        }
                    }

                    // Enrichment nudge: classes without actionable metadata.
                    // Legacy (pre-epistemic) memories all start this way —
                    // their class is type-derived, but premises and watch
                    // globs only accrue as memories are touched. Report-only.
                    if let Ok(gaps) = ops::enrichment_gaps(&store).await {
                        if gaps.any() {
                            recency_hint.push_str(&format!(
                                "\n{} decision(s) lack a recorded premise and {} observation(s) \
                                 lack invalidation watch paths (typical for pre-epistemic \
                                 memories) — when you touch one, enrich it via update \
                                 (premise / invalidated_by).",
                                gaps.decisions_without_premise, gaps.observations_without_watch,
                            ));
                        }
                    }
                }

                let prompt = format!(
                    "Before ending this session, consider:\n\
                         1. Did you make any architectural decisions? -> create type: decision\n\
                         2. Did you discover any hazards or footguns? -> create type: hazard\n\
                         3. Did you decide something for THIS task only? -> set origin_task and generality: task. State the premise ('because C') for decisions.\n\
                         4. Did you encounter non-obvious behavior? -> create type: debug\n\
                         5. Did anything contradict existing memories? -> challenge\n\n\
                         {}\n\
                         Run review if you'd like to address flagged memories with the user.{}",
                    stats_text, recency_hint
                );

                let mut result = GetPromptResult::new(vec![PromptMessage::new_text(
                    PromptMessageRole::User,
                    prompt,
                )]);
                result.description = Some("Session end review".to_string());
                Ok(result)
            }
            _ => Err(rmcp::ErrorData::invalid_params(
                format!("Unknown prompt: {}", request.name),
                None,
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Server startup helpers
// ---------------------------------------------------------------------------

/// Start the MCP server with stdio transport.
pub async fn run_stdio(
    dir: PathBuf,
    embedding_backend: Option<EmbeddingBackend>,
) -> anyhow::Result<()> {
    let stats_cfg = engramdb::types::EngramConfig::default().stats;
    let stats = engramdb::telemetry::StatsCollector::new(stats_cfg.clone());

    // Hydrate counters by replaying recent events from each project's
    // LanceDB `stats_events` table before serving requests.
    if let Err(e) = engramdb::telemetry::persistence::hydrate_collector(&stats).await {
        tracing::warn!("stats hydrate failed: {e}");
    }

    // Drain the collector's persistence channel into the per-project
    // LanceDB tables in the background. Capture the JoinHandle so we can
    // await it on shutdown — without this, tokio runtime teardown
    // cancels mid-`append_events` and tail events are lost.
    let flush_handle = stats.take_receiver().map(|rx| {
        engramdb::telemetry::persistence::spawn_flush_task(
            rx,
            stats_cfg.flush_interval_secs,
            stats_cfg.retention_days,
            Arc::downgrade(&stats),
        )
    });

    let mut server = EngramDbServer::new_with_stats(dir, embedding_backend, stats.clone())?;
    // Detect git worktrees and register/init the main project if needed.
    server.ensure_hierarchy().await?;
    // On the main worktree, run throttled housekeeping (orphan cleanup + a
    // quick store health check). Best-effort: never blocks serving.
    server.maintain_main_project().await;
    // Embedding-model-change check: warn (default), auto-reindex, or — in
    // `error` mode — leave the warning so embedding tools hard-fail.
    if let Some((mode, report)) = server.embedding_startup_report().await {
        if !report.status.is_consistent() {
            if let Some(w) = &report.warning {
                tracing::warn!("{w}");
            }
            server.embedding_warning = report.warning.clone();
            if mode == engramdb::types::ReindexOnModelChange::Auto {
                tracing::warn!(
                    "EngramDB: reindex_on_model_change=auto — re-embedding before serving…"
                );
                match server.auto_reindex_default().await {
                    Ok(()) => {
                        tracing::warn!("EngramDB: auto-reindex complete.");
                        server.embedding_warning = None;
                    }
                    Err(e) => tracing::warn!("EngramDB: auto-reindex failed: {e}"),
                }
            }
        }
    }
    // Load the embedding model in the background now so the first tool call
    // doesn't pay the ~240ms ONNX session init synchronously.
    server.spawn_provider_warmup();
    // Keep the shared daemon resident while this session runs and self-heal it
    // if it dies/restarts.
    server.spawn_daemon_heartbeat();
    // `serve` consumes the server; the running service owns the remaining
    // `Arc<StatsCollector>` baked into it. Wait on the service, then drop
    // both it and the local `stats` so the channel closes and the flush
    // task can finalize before we await it.
    let service = server.serve(rmcp::transport::io::stdio()).await?;
    // `waiting` takes ownership and consumes `service` on completion, so by
    // the time it returns the rmcp service has already dropped its
    // `Arc<StatsCollector>` clone — leaving the local `stats` as the last
    // Arc holder. Dropping it now closes the channel.
    service.waiting().await?;
    drop(stats);
    if let Some(h) = flush_handle {
        // Safety net: if some Arc clone unexpectedly outlives `stats` the
        // channel never closes and `h.await` would hang forever. Bound
        // the wait so a clean exit always happens; legitimate flushes
        // complete in milliseconds.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
    }
    Ok(())
}

/// Start the MCP server with streamable HTTP transport.
pub async fn run_sse(
    dir: PathBuf,
    port: u16,
    embedding_backend: Option<EmbeddingBackend>,
) -> anyhow::Result<()> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    };
    use std::sync::Arc;

    // One process-global stats collector shared across every per-connection
    // server instance. Hydrate once and spawn a single flush task.
    let stats_cfg = engramdb::types::EngramConfig::default().stats;
    let stats = engramdb::telemetry::StatsCollector::new(stats_cfg.clone());
    if let Err(e) = engramdb::telemetry::persistence::hydrate_collector(&stats).await {
        tracing::warn!("stats hydrate failed: {e}");
    }
    let flush_handle = stats.take_receiver().map(|rx| {
        engramdb::telemetry::persistence::spawn_flush_task(
            rx,
            stats_cfg.flush_interval_secs,
            stats_cfg.retention_days,
            Arc::downgrade(&stats),
        )
    });

    // One process-wide model cache + daemon handle shared across every
    // per-connection server, so the embedding model loads once (or the
    // daemon is probed/spawned once) for the whole process rather than per
    // HTTP connection.
    let provider_cache = ops::ProviderCache::new();
    let daemon: Arc<engramdb::daemon::DaemonCell> = Arc::new(engramdb::daemon::DaemonCell::new());

    // Resolve hierarchy eagerly once: subsequent per-connection server
    // instances share the registry, so registration only happens once.
    //
    // The embedding-model-change policy is evaluated here as well. The
    // per-connection servers built by the factory closure below are
    // short-lived and never run startup logic, so without this every SSE
    // connection would silently skip the check (`embedding_warning: None`)
    // and `auto` mode would never re-embed. Mirror `run_stdio`: compute the
    // warning (and auto-reindex if requested) exactly once, then seed every
    // per-connection server with the resulting warning. The warmup server
    // shares the process-wide model cache / daemon so the check (and any
    // auto-reindex) reuse the same providers the connections will.
    let embedding_warning = {
        let mut warmup =
            EngramDbServer::new_with_stats(dir.clone(), embedding_backend, stats.clone())?
                .with_shared_model_caches(provider_cache.clone(), daemon.clone());
        warmup.ensure_hierarchy().await?;
        // Same throttled main-worktree housekeeping as the stdio path; runs
        // once here since per-connection servers share this registry.
        warmup.maintain_main_project().await;
        if let Some((mode, report)) = warmup.embedding_startup_report().await {
            if !report.status.is_consistent() {
                if let Some(w) = &report.warning {
                    tracing::warn!("{w}");
                }
                warmup.embedding_warning = report.warning.clone();
                if mode == engramdb::types::ReindexOnModelChange::Auto {
                    tracing::warn!(
                        "EngramDB: reindex_on_model_change=auto — re-embedding before serving…"
                    );
                    match warmup.auto_reindex_default().await {
                        Ok(()) => {
                            tracing::warn!("EngramDB: auto-reindex complete.");
                            warmup.embedding_warning = None;
                        }
                        Err(e) => tracing::warn!("EngramDB: auto-reindex failed: {e}"),
                    }
                }
            }
        }
        // Warm the shared model cache / daemon once (after any auto-reindex)
        // so the first connection's first tool call doesn't pay the load.
        warmup.spawn_provider_warmup();
        warmup.spawn_daemon_heartbeat();
        warmup.embedding_warning
    };

    let config = StreamableHttpServerConfig::default();
    let ct = config.cancellation_token.clone();
    let service = StreamableHttpService::new(
        {
            let stats = stats.clone();
            let embedding_warning = embedding_warning.clone();
            let provider_cache = provider_cache.clone();
            let daemon = daemon.clone();
            move || {
                let mut server =
                    EngramDbServer::new_with_stats(dir.clone(), embedding_backend, stats.clone())
                        .map(|s| s.with_shared_model_caches(provider_cache.clone(), daemon.clone()))
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                server.embedding_warning = embedding_warning.clone();
                Ok(server)
            }
        },
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("EngramDB MCP server listening on {}", addr);

    axum::serve(listener, router)
        .with_graceful_shutdown(async move { ct.cancelled().await })
        .await?;

    // Drop the collector — closing the channel triggers the flush task to
    // drain any buffered events and exit cleanly. Then await the task so
    // tokio runtime teardown doesn't cancel it mid-`append_events`.
    //
    // Safety net: if any per-connection `EngramDbServer` Arc clone outlives
    // `axum::serve` returning (e.g. via `LocalSessionManager` retention),
    // the channel never closes and the await would hang. Bound the wait.
    drop(stats);
    if let Some(h) = flush_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
