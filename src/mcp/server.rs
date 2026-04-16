//! EngramDB MCP server implementation.
//!
//! Defines the server struct, all MCP tools (16), resources (2), and prompts (2).
//! Tools delegate to the `ops` layer; the server opens a fresh `MemoryStore`
//! per request so it always sees the latest on-disk state.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::schemars::{self, JsonSchema};
use rmcp::{tool, tool_handler, tool_router};
use rmcp::{ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};

use crate::mcp::error::{error_response, ErrorCode};
use crate::ops;
use crate::retrieval::engine::{RetrievalEngine, RetrievalQuery};
use crate::retrieval::filters::SearchFilters;
use crate::storage::config::load_config;
use crate::storage::{FileRegistry, MemoryStore, RegistryBackend};
use crate::title::TitleStrategy;
use crate::types::{EmbeddingBackend, Provenance, Status, Visibility};

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

    #[schemars(description = "Core knowledge to store (max ~500 tokens)")]
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

    #[schemars(description = "Title generation strategy: keyword|t5|none (default keyword)")]
    title_strategy: Option<String>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RetrieveInput {
    #[schemars(description = "File path relative to project root")]
    path: Option<String>,

    #[schemars(description = "Logical scopes")]
    logical: Option<Vec<String>>,

    #[schemars(description = "Text query for semantic search")]
    query: Option<String>,

    #[schemars(description = "Filter by memory types")]
    types: Option<Vec<String>>,

    #[schemars(description = "Filter by tags (OR logic)")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Minimum criticality threshold")]
    min_criticality: Option<f64>,

    #[schemars(description = "Maximum results (default 10)")]
    max_results: Option<usize>,

    #[schemars(description = "Detail: summary|content|full (default content)")]
    detail_level: Option<String>,

    #[schemars(description = "Include expired/decayed memories")]
    include_expired: Option<bool>,

    #[schemars(description = "Also include global memories in results (default false)")]
    include_global: Option<bool>,

    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchInput {
    #[schemars(description = "Search query against summary, content, and tags")]
    query: String,

    #[schemars(description = "Filter by memory types")]
    types: Option<Vec<String>>,

    #[schemars(description = "Filter by tags")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Filter by physical scope")]
    physical: Option<String>,

    #[schemars(description = "Filter by logical scope")]
    logical: Option<String>,

    #[schemars(description = "Minimum criticality")]
    min_criticality: Option<f64>,

    #[schemars(description = "Max results (default 10)")]
    max_results: Option<usize>,

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
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ResolveInput {
    #[schemars(description = "Memory ID")]
    id: String,

    #[schemars(description = "Action: keep, update, or delete")]
    action: String,

    #[schemars(description = "New content (required for update)")]
    updated_content: Option<String>,

    #[schemars(description = "New summary (optional for update)")]
    updated_summary: Option<String>,

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

    #[schemars(description = "Filter by tags (OR logic)")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Filter: active|needsreview|challenged")]
    status: Option<String>,

    #[schemars(description = "Filter by scope (physical or logical)")]
    scope: Option<String>,

    #[schemars(description = "Sort: criticality|created|updated|type (default criticality)")]
    sort_field: Option<String>,

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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DoctorInput {
    #[schemars(
        description = "Target project: absolute path, 16-char project ID, or \"global\" for cross-project memories. Omit for current project."
    )]
    project: Option<String>,
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
}

/// Merge global scored memories into the project results, re-sort by score,
/// deduplicate by ID, and truncate to `max`.
fn merge_scored_memories(
    project: &mut Vec<crate::retrieval::engine::ScoredMemory>,
    global: Vec<crate::retrieval::engine::ScoredMemory>,
    max: usize,
) {
    use std::collections::HashSet;
    let existing_ids: HashSet<String> = project.iter().map(|sm| sm.memory.id.clone()).collect();
    for sm in global {
        if !existing_ids.contains(&sm.memory.id) {
            project.push(sm);
        }
    }
    project.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    project.truncate(max);
}

fn memory_to_output(m: &crate::types::Memory, include_details: bool) -> MemoryOutput {
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
    }
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
        let registry: Arc<dyn RegistryBackend> = Arc::new(
            FileRegistry::global()
                .map_err(|e| anyhow::anyhow!("Failed to initialize registry: {}", e))?,
        );
        let effective_dir =
            crate::storage::project_id::detect_worktree_main(&dir).unwrap_or_else(|| dir.clone());
        Ok(Self {
            dir,
            effective_dir,
            embedding_backend,
            registry,
            tool_router: Self::tool_router(),
        })
    }

    #[cfg(test)]
    pub fn new_with_registry(
        dir: PathBuf,
        embedding_backend: Option<EmbeddingBackend>,
        registry: Arc<dyn RegistryBackend>,
    ) -> Self {
        let effective_dir =
            crate::storage::project_id::detect_worktree_main(&dir).unwrap_or_else(|| dir.clone());
        Self {
            dir,
            effective_dir,
            embedding_backend,
            registry,
            tool_router: Self::tool_router(),
        }
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

        let child_id = crate::storage::project_id::compute_project_id(&self.dir);
        let parent_id = crate::storage::project_id::compute_project_id(&self.effective_dir);

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
                return crate::storage::paths::global_store_dir()
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
            let root_id = crate::storage::resolve_root_project_id(&registry, input);
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
            let effective =
                crate::storage::project_id::detect_worktree_main(&canonical).unwrap_or(canonical);
            let project_id = crate::storage::project_id::compute_project_id(&effective);
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
                let child_id = crate::storage::project_id::compute_project_id(&self.dir);
                let parent_id = crate::storage::project_id::compute_project_id(&self.effective_dir);
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
    ) -> Result<crate::types::EngramConfig, String> {
        let dir = self.resolve_dir(project).await?;
        let config_path = dir.join(".engramdb").join("config.toml");
        Ok(load_config(&config_path).await.unwrap_or_default())
    }

    /// Build a RetrievalEngine for the given project override.
    async fn build_engine_for(&self, project: Option<&str>) -> Result<RetrievalEngine, String> {
        let dir = self.resolve_dir(project).await?;
        let store = self.open_store_for(project).await?;
        let config_path = dir.join(".engramdb").join("config.toml");
        Ok(ops::build_engine(store, &config_path, self.embedding_backend).await)
    }

    /// Build a RetrievalEngine with optional embeddings support for the default project.
    async fn build_engine(&self) -> Result<RetrievalEngine, String> {
        self.build_engine_for(None).await
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl EngramDbServer {
    #[tool(
        name = "create",
        description = "Store a new memory about the project (or globally with project=\"global\"). Use after discovering patterns, decisions, or hazards."
    )]
    async fn memory_create(
        &self,
        Parameters(input): Parameters<CreateInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;
        let engine = self.build_engine_for(input.project.as_deref()).await?;
        let type_ = ops::parse_memory_type(&input.type_)
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let visibility = match input.visibility.as_deref() {
            Some("personal") => Visibility::Personal,
            _ => Visibility::Shared,
        };

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

        let result = ops::create_memory(
            &store,
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
                    .unwrap_or_default(),
            },
            Some(&engine),
        )
        .await
        .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        serde_json::to_string(&CreateOutput {
            id: result.id,
            created: true,
            summary: result.summary,
        })
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "retrieve",
        description = "Get memories relevant to your current working context. Call before modifying files, researching project questions, or investigating how things work."
    )]
    async fn memory_retrieve(
        &self,
        Parameters(input): Parameters<RetrieveInput>,
    ) -> Result<String, String> {
        let engine = self.build_engine_for(input.project.as_deref()).await?;

        let type_filter = if let Some(types) = &input.types {
            let mut parsed = Vec::new();
            for t in types {
                parsed.push(
                    ops::parse_memory_type(t)
                        .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?,
                );
            }
            Some(parsed)
        } else {
            None
        };

        let detail_level = if let Some(ref level_str) = input.detail_level {
            ops::parse_detail_level(level_str)
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?
        } else {
            crate::retrieval::engine::DetailLevel::Content
        };

        if let Some(mc) = input.min_criticality {
            ops::validate_score(mc, "min_criticality")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }

        let query = RetrievalQuery {
            path: input.path,
            logical: input.logical.unwrap_or_default(),
            query: input.query,
            types: type_filter,
            tags: input.tags,
            min_criticality: input.min_criticality,
            max_results: Some(input.max_results.unwrap_or(10)),
            include_expired: Some(input.include_expired.unwrap_or(false)),
            detail_level,
        };

        let mut result = ops::retrieve_memories(&engine, &query)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        // Merge global memories if requested and not already targeting the global store
        if input.include_global.unwrap_or(false) && !Self::is_global(input.project.as_deref()) {
            if let Ok(global_engine) = self.build_engine_for(Some("global")).await {
                if let Ok(global_result) = ops::retrieve_memories(&global_engine, &query).await {
                    let max = query.max_results.unwrap_or(10);
                    merge_scored_memories(&mut result.memories, global_result.memories, max);
                    result.total += global_result.total;
                }
            }
        }

        let include_details = input.detail_level.as_deref() == Some("full");
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
                    decay: sm.score_breakdown.decay,
                    criticality: sm.score_breakdown.criticality,
                },
            })
            .collect();

        serde_json::to_string(&serde_json::json!({
            "memories": memories,
            "total": result.total,
            "query_mode": result.query_mode,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "search",
        description = "Search all memories by text. Use before answering questions about project conventions, workflows, architecture, or tooling."
    )]
    async fn memory_search(
        &self,
        Parameters(input): Parameters<SearchInput>,
    ) -> Result<String, String> {
        let engine = self.build_engine_for(input.project.as_deref()).await?;

        let type_filter = if let Some(types) = &input.types {
            let mut parsed = Vec::new();
            for t in types {
                parsed.push(
                    ops::parse_memory_type(t)
                        .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?,
                );
            }
            Some(parsed)
        } else {
            None
        };

        if let Some(mc) = input.min_criticality {
            ops::validate_score(mc, "min_criticality")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }

        let filters = SearchFilters {
            types: type_filter,
            tags: input.tags,
            physical: input.physical,
            logical: input.logical,
            min_criticality: input.min_criticality,
        };

        let max = input.max_results.unwrap_or(10);
        let mut results = ops::search_memories(&engine, &input.query, &filters, Some(max))
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        // Merge global memories if requested and not already targeting the global store
        if input.include_global.unwrap_or(false) && !Self::is_global(input.project.as_deref()) {
            if let Ok(global_engine) = self.build_engine_for(Some("global")).await {
                if let Ok(global_results) =
                    ops::search_memories(&global_engine, &input.query, &filters, Some(max)).await
                {
                    merge_scored_memories(&mut results, global_results, max);
                }
            }
        }

        let memories: Vec<ScoredMemoryOutput> = results
            .iter()
            .map(|sm| ScoredMemoryOutput {
                memory: memory_to_output(&sm.memory, false),
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
                    decay: sm.score_breakdown.decay,
                    criticality: sm.score_breakdown.criticality,
                },
            })
            .collect();

        serde_json::to_string(&serde_json::json!({
            "memories": memories,
            "total": results.len(),
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "get",
        description = "Get full content of a specific memory, including details."
    )]
    async fn memory_get(&self, Parameters(input): Parameters<GetInput>) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;
        let memory = ops::get_memory(&store, &input.id)
            .await
            .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        serde_json::to_string(&memory_to_output(&memory, true))
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "update",
        description = "Update an existing memory. Cannot change id or created_at."
    )]
    async fn memory_update(
        &self,
        Parameters(input): Parameters<UpdateInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;
        let engine = self.build_engine_for(input.project.as_deref()).await?;

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

        ops::update_memory(
            &store,
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
                decay_strategy: input.decay_strategy,
                decay_half_life: input.decay_half_life,
                decay_ttl: input.decay_ttl,
                decay_floor: input.decay_floor,
            },
            Some(&engine),
        )
        .await
        .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        serde_json::to_string(&serde_json::json!({
            "id": input.id,
            "updated": true
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "delete",
        description = "Permanently delete a memory. Prefer supersedes for corrections."
    )]
    async fn memory_delete(
        &self,
        Parameters(input): Parameters<DeleteInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;
        ops::delete_memory(&store, &input.id)
            .await
            .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        serde_json::to_string(&serde_json::json!({
            "id": input.id,
            "deleted": true
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "challenge",
        description = "Flag a memory as potentially incorrect and mark for review."
    )]
    async fn memory_challenge(
        &self,
        Parameters(input): Parameters<ChallengeInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;
        let result = ops::challenge_memory(
            &store,
            &input.id,
            &input.evidence,
            input.source_file.as_deref(),
        )
        .await
        .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        serde_json::to_string(&serde_json::json!({
            "challenged": result.challenged,
            "memory": memory_to_output(&result.memory, true)
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "review",
        description = "List memories needing review (stale or challenged)."
    )]
    async fn memory_review(
        &self,
        Parameters(input): Parameters<ReviewInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;

        let type_filter = input
            .type_
            .as_deref()
            .map(ops::parse_memory_type)
            .transpose()
            .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let params = ops::ReviewParams {
            scope: input.scope,
            max_results: input.max_results,
            type_filter,
            challenged_only: input.challenged_only.unwrap_or(false),
            stale_only: input.stale_only.unwrap_or(false),
        };

        let memories = ops::review_memories(&store, &params)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let outputs: Vec<MemoryOutput> = memories
            .iter()
            .map(|m| memory_to_output(m, false))
            .collect();

        serde_json::to_string(&serde_json::json!({
            "memories": outputs,
            "total": memories.len()
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "resolve",
        description = "Resolve a challenged or needs_review memory: keep, update, or delete."
    )]
    async fn memory_resolve(
        &self,
        Parameters(input): Parameters<ResolveInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;

        let action = match input.action.as_str() {
            "keep" => ops::ResolveAction::Keep,
            "update" => ops::ResolveAction::Update,
            "delete" => ops::ResolveAction::Delete,
            other => {
                return Err(error_response(
                    ErrorCode::ValidationError,
                    &format!(
                        "Invalid action '{}'. Must be keep, update, or delete.",
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
            },
        )
        .await
        .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        serde_json::to_string(&serde_json::json!({
            "id": input.id,
            "action": result.action,
            "resolved": result.resolved
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "compress_candidates",
        description = "List low-criticality memories eligible for compression. Review before compress_apply."
    )]
    async fn memory_compress_candidates(
        &self,
        Parameters(input): Parameters<CompressCandidatesInput>,
    ) -> Result<String, String> {
        if let Some(t) = input.threshold {
            ops::validate_score(t, "threshold")
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;
        }
        let store = self.open_store_for(input.project.as_deref()).await?;
        let result = ops::compress_candidates(&store, input.scope.as_deref(), input.threshold)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        serde_json::to_string(&serde_json::json!({
            "candidates": result.candidates,
            "total": result.total,
            "threshold": result.threshold,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "compress_apply",
        description = "Compress multiple memories into one summary. Call compress_candidates first."
    )]
    async fn memory_compress_apply(
        &self,
        Parameters(input): Parameters<CompressApplyInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;
        let result = ops::compress_apply(
            &store,
            input.source_ids,
            input.summary,
            input.content,
            input.scope,
            input.tags,
        )
        .await
        .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        serde_json::to_string(&serde_json::json!({
            "new_id": result.new_id,
            "superseded_count": result.superseded_count,
            "applied": true,
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "stats",
        description = "Overview of memory store — counts by type, scope, status."
    )]
    async fn memory_stats(
        &self,
        Parameters(input): Parameters<StatsInput>,
    ) -> Result<String, String> {
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

        serde_json::to_string(&serde_json::json!({
            "total": stats.total,
            "by_type": by_type,
            "by_status": by_status,
            "by_scope": by_scope,
            "expired": stats.expired,
            "oldest": stats.oldest,
            "newest": stats.newest,
            "avg_criticality": stats.avg_criticality
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "gc",
        description = "Garbage collect decayed memories. Always dry_run first."
    )]
    async fn memory_gc(&self, Parameters(input): Parameters<GcInput>) -> Result<String, String> {
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
        if !result.stale_entries.is_empty() {
            response["stale_entries"] = serde_json::json!(result.stale_entries);
            response["warning"] =
                serde_json::json!("Stale index entries found. Run reindex to fix.");
        }
        serde_json::to_string(&response)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "reindex",
        description = "Rebuild the search index and embedding vectors."
    )]
    async fn memory_reindex(
        &self,
        Parameters(input): Parameters<ReindexInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;
        let embeddings_only = input.embeddings_only.unwrap_or(false);
        let index_only = input.index_only.unwrap_or(false);

        // Build engine outside conditional so it stays alive for the reference
        let engine = if !index_only {
            self.build_engine_for(input.project.as_deref()).await.ok()
        } else {
            None
        };

        let result = ops::reindex(&store, engine.as_ref(), embeddings_only)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        serde_json::to_string(&serde_json::json!({
            "indexed": result.indexed,
            "embedded": result.embedded,
            "errors": result.errors
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "list",
        description = "List memories with optional filtering, sorting, and limiting."
    )]
    async fn memory_list(
        &self,
        Parameters(input): Parameters<ListInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;

        let sort_field =
            ops::parse_sort_field(input.sort_field.as_deref().unwrap_or("criticality"))
                .map_err(|e| error_response(ErrorCode::ValidationError, &e.to_string()))?;

        let params = ops::ListParams {
            types: input.types,
            tags: input.tags,
            status: input.status,
            scope: input.scope,
            sort_field,
            reverse: input.reverse.unwrap_or(false),
            limit: input.limit,
        };

        let entries = ops::list_memories(&store, &params)
            .await
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))?;

        let output: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "type": format!("{:?}", e.type_).to_lowercase(),
                    "summary": e.summary,
                    "tags": e.tags,
                    "logical": e.logical,
                    "physical": e.physical,
                    "status": format!("{:?}", e.status).to_lowercase(),
                    "criticality": e.criticality,
                    "created_at": e.created_at.to_rfc3339(),
                    "updated_at": e.updated_at.to_rfc3339(),
                })
            })
            .collect();

        serde_json::to_string(&serde_json::json!({
            "memories": output,
            "total": output.len()
        }))
        .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }

    #[tool(
        name = "doctor",
        description = "Check store health (index vs disk consistency). Fast, project-scoped check. For full environment diagnostics, use the CLI: `engramdb doctor`."
    )]
    async fn memory_doctor(
        &self,
        Parameters(input): Parameters<DoctorInput>,
    ) -> Result<String, String> {
        let store = self.open_store_for(input.project.as_deref()).await?;
        let result = ops::doctor(&store)
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
        serde_json::to_string(&response)
            .map_err(|e| error_response(ErrorCode::InternalError, &e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// ServerHandler implementation
// ---------------------------------------------------------------------------

#[tool_handler]
impl ServerHandler for EngramDbServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
            server_info: Implementation {
                name: "engramdb".to_string(),
                title: None,
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: None,
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Project-scoped persistent memory store for coding agents. \
                 Stores decisions, hazards, conventions, and context about the codebase. \
                 IMPORTANT: Search memories (search) before answering project questions, \
                 investigating workflows, or researching how things work — not only before \
                 modifying files. Store new knowledge after significant discoveries. \
                 All tools accept an optional `project` parameter (absolute path, 16-char \
                 project ID, or \"global\") to operate on a different project's memories. \
                 Use project=\"global\" for cross-project memories like personal preferences, \
                 coding conventions, or knowledge that applies everywhere. \
                 Use include_global=true on retrieve/search to merge global memories into results. \
                 Omit `project` to use the current project."
                    .to_string(),
            ),
        }
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

                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(json, "memory://index")],
                })
            } else if let Some(path) = uri.strip_prefix("memory://context/") {
                let engine = self
                    .build_engine()
                    .await
                    .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;

                let query = RetrievalQuery {
                    path: Some(path.to_string()),
                    max_results: Some(10),
                    ..RetrievalQuery::default()
                };
                let result = ops::retrieve_memories(&engine, &query)
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

                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(json, &uri)],
                })
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
                    Some(vec![PromptArgument {
                        name: "path".to_string(),
                        title: None,
                        description: Some(
                            "The file or directory the agent will be working on.".to_string(),
                        ),
                        required: Some(false),
                    }]),
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
                        path,
                        max_results: Some(10),
                        ..RetrievalQuery::default()
                    };
                    if let Ok(result) = ops::retrieve_memories(&engine, &query).await {
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
                         this session, store them using the create tool.\n\
                         If you encounter evidence that contradicts an existing memory, \
                         use challenge and ask the user how to resolve it.",
                    memory_text
                );

                Ok(GetPromptResult {
                    description: Some("Session start briefing".to_string()),
                    messages: vec![PromptMessage::new_text(PromptMessageRole::User, prompt)],
                })
            }
            "memory-session-end" => {
                let mut stats_text = String::new();
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
                }

                let prompt = format!(
                    "Before ending this session, consider:\n\
                         1. Did you make any architectural decisions? -> create type: decision\n\
                         2. Did you discover any hazards or footguns? -> create type: hazard\n\
                         3. Did you encounter non-obvious behavior? -> create type: debug\n\
                         4. Did anything contradict existing memories? -> challenge\n\n\
                         {}\n\
                         Run review if you'd like to address flagged memories with the user.",
                    stats_text
                );

                Ok(GetPromptResult {
                    description: Some("Session end review".to_string()),
                    messages: vec![PromptMessage::new_text(PromptMessageRole::User, prompt)],
                })
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
    let server = EngramDbServer::new(dir, embedding_backend)?;
    // Detect git worktrees and register/init the main project if needed.
    server.ensure_hierarchy().await?;
    let service = server.serve(rmcp::transport::io::stdio()).await?;
    service.waiting().await?;
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

    // Resolve hierarchy eagerly once: subsequent per-connection server
    // instances share the registry, so registration only happens once.
    {
        let warmup = EngramDbServer::new(dir.clone(), embedding_backend)?;
        warmup.ensure_hierarchy().await?;
    }

    let config = StreamableHttpServerConfig::default();
    let ct = config.cancellation_token.clone();
    let service = StreamableHttpService::new(
        move || {
            EngramDbServer::new(dir.clone(), embedding_backend)
                .map_err(|e| std::io::Error::other(e.to_string()))
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
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::EmbeddingBackend;
    use serde_json::json;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, EngramDbServer) {
        let temp_dir = TempDir::new().unwrap();
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = EngramDbServer::new_with_registry(
            temp_dir.path().to_path_buf(),
            Some(EmbeddingBackend::Onnx),
            registry,
        );
        (temp_dir, server)
    }

    fn parse_ok(result: &Result<String, String>) -> serde_json::Value {
        let json_str = result.as_ref().expect("tool should succeed");
        serde_json::from_str(json_str).expect("should be valid JSON")
    }

    fn parse_err(result: &Result<String, String>) -> serde_json::Value {
        let json_str = result.as_ref().unwrap_err();
        serde_json::from_str(json_str).unwrap_or_else(|_| json!({"error": {"message": json_str}}))
    }

    fn create_input(type_: &str, summary: &str, content: &str) -> CreateInput {
        CreateInput {
            type_: type_.to_string(),
            content: content.to_string(),
            summary: summary.to_string(),
            details: None,
            physical: None,
            logical: None,
            tags: None,
            criticality: None,
            confidence: None,
            visibility: None,
            supersedes: None,
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            title: None,
            title_strategy: None,
            project: None,
        }
    }

    /// Helper: create a memory and return its ID.
    async fn create_and_get_id(
        server: &EngramDbServer,
        type_: &str,
        summary: &str,
        content: &str,
    ) -> String {
        let result = server
            .memory_create(Parameters(create_input(type_, summary, content)))
            .await;
        let val = parse_ok(&result);
        val["id"].as_str().unwrap().to_string()
    }

    // -----------------------------------------------------------------------
    // memory_create
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn create_basic() {
        let (_dir, server) = setup().await;
        let result = server
            .memory_create(Parameters(create_input(
                "decision",
                "Use Rust",
                "We chose Rust for performance",
            )))
            .await;
        let val = parse_ok(&result);
        assert!(val["id"].is_string());
        assert_eq!(val["created"], true);
        assert_eq!(val["summary"], "Use Rust");
    }

    #[tokio::test]
    async fn create_with_all_fields() {
        let (_dir, server) = setup().await;
        let input = CreateInput {
            type_: "hazard".to_string(),
            content: "Race condition in cache".to_string(),
            summary: "Cache race".to_string(),
            details: Some("Detailed explanation".to_string()),
            physical: Some(vec!["src/cache.rs".to_string()]),
            logical: Some(vec!["caching.invalidation".to_string()]),
            tags: Some(vec!["perf".to_string(), "critical".to_string()]),
            criticality: Some(0.9),
            confidence: Some(0.7),
            visibility: Some("personal".to_string()),
            supersedes: Some(vec![]),
            decay_strategy: Some("exponential".to_string()),
            decay_half_life: Some(86400),
            decay_ttl: None,
            decay_floor: Some(0.1),
            title: None,
            title_strategy: None,
            project: None,
        };
        let result = server.memory_create(Parameters(input)).await;
        let val = parse_ok(&result);
        assert!(val["id"].is_string());
        assert_eq!(val["created"], true);
    }

    #[tokio::test]
    async fn create_invalid_type() {
        let (_dir, server) = setup().await;
        let result = server
            .memory_create(Parameters(create_input("nonsense", "Bad", "Content")))
            .await;
        let val = parse_err(&result);
        assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn create_criticality_out_of_range() {
        let (_dir, server) = setup().await;
        let mut input = create_input("decision", "Test", "Content");
        input.criticality = Some(2.0);
        let result = server.memory_create(Parameters(input)).await;
        let val = parse_err(&result);
        assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn create_confidence_out_of_range() {
        let (_dir, server) = setup().await;
        let mut input = create_input("decision", "Test", "Content");
        input.confidence = Some(-0.1);
        let result = server.memory_create(Parameters(input)).await;
        let val = parse_err(&result);
        assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn create_decay_floor_out_of_range() {
        let (_dir, server) = setup().await;
        let mut input = create_input("decision", "Test", "Content");
        input.decay_floor = Some(1.5);
        let result = server.memory_create(Parameters(input)).await;
        let val = parse_err(&result);
        assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
    }

    // -----------------------------------------------------------------------
    // memory_get
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn get_existing() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(
            &server,
            "convention",
            "Use snake_case",
            "All names use snake_case",
        )
        .await;
        let result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["summary"], "Use snake_case");
        assert_eq!(val["content"], "All names use snake_case");
        assert_eq!(val["type"], "convention");
    }

    #[tokio::test]
    async fn get_nonexistent() {
        let (_dir, server) = setup().await;
        // Need a store to exist first
        let _ = create_and_get_id(&server, "decision", "Setup", "Content").await;
        let result = server
            .memory_get(Parameters(GetInput {
                id: "nonexistent-id-1234".to_string(),
                project: None,
            }))
            .await;
        let val = parse_err(&result);
        assert_eq!(val["error"]["code"], "MEMORY_NOT_FOUND");
    }

    #[tokio::test]
    async fn get_by_prefix() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Prefix test", "Content").await;
        let prefix = &id[..8];
        let result = server
            .memory_get(Parameters(GetInput {
                id: prefix.to_string(),
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["summary"], "Prefix test");
    }

    // -----------------------------------------------------------------------
    // memory_update
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn update_summary() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Old summary", "Content").await;
        let result = server
            .memory_update(Parameters(UpdateInput {
                id: id.clone(),
                summary: Some("New summary".to_string()),
                type_: None,
                content: None,
                details: None,
                physical: None,
                logical: None,
                tags: None,
                tags_add: None,
                tags_remove: None,
                criticality: None,
                confidence: None,
                visibility: None,
                title: None,
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["updated"], true);

        let get_result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["summary"], "New summary");
    }

    #[tokio::test]
    async fn update_type() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Summary", "Content").await;
        let result = server
            .memory_update(Parameters(UpdateInput {
                id: id.clone(),
                type_: Some("hazard".to_string()),
                summary: None,
                content: None,
                details: None,
                physical: None,
                logical: None,
                tags: None,
                tags_add: None,
                tags_remove: None,
                criticality: None,
                confidence: None,
                visibility: None,
                title: None,
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["updated"], true);

        let get_result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["type"], "hazard");
    }

    #[tokio::test]
    async fn update_status() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Summary", "Content").await;
        let result = server
            .memory_update(Parameters(UpdateInput {
                id: id.clone(),
                status: Some("challenged".to_string()),
                type_: None,
                summary: None,
                content: None,
                details: None,
                physical: None,
                logical: None,
                tags: None,
                tags_add: None,
                tags_remove: None,
                criticality: None,
                confidence: None,
                visibility: None,
                title: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["updated"], true);

        let get_result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["status"], "challenged");
    }

    #[tokio::test]
    async fn update_tags_add_remove() {
        let (_dir, server) = setup().await;
        let input = CreateInput {
            tags: Some(vec!["alpha".to_string(), "beta".to_string()]),
            ..create_input("decision", "Tagged", "Content")
        };
        let result = server.memory_create(Parameters(input)).await;
        let id = parse_ok(&result)["id"].as_str().unwrap().to_string();

        let result = server
            .memory_update(Parameters(UpdateInput {
                id: id.clone(),
                tags_add: Some(vec!["gamma".to_string()]),
                tags_remove: Some(vec!["alpha".to_string()]),
                type_: None,
                summary: None,
                content: None,
                details: None,
                physical: None,
                logical: None,
                tags: None,
                criticality: None,
                confidence: None,
                visibility: None,
                title: None,
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
                project: None,
            }))
            .await;
        parse_ok(&result);

        let get_result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        let get_val = parse_ok(&get_result);
        let tags: Vec<String> = get_val["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(tags.contains(&"beta".to_string()));
        assert!(tags.contains(&"gamma".to_string()));
        assert!(!tags.contains(&"alpha".to_string()));
    }

    #[tokio::test]
    async fn update_criticality_validation() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Summary", "Content").await;
        let result = server
            .memory_update(Parameters(UpdateInput {
                id,
                criticality: Some(2.0),
                type_: None,
                summary: None,
                content: None,
                details: None,
                physical: None,
                logical: None,
                tags: None,
                tags_add: None,
                tags_remove: None,
                confidence: None,
                visibility: None,
                title: None,
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
                project: None,
            }))
            .await;
        let val = parse_err(&result);
        assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn update_decay_params() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Summary", "Content").await;
        let result = server
            .memory_update(Parameters(UpdateInput {
                id,
                decay_strategy: Some("exponential".to_string()),
                decay_half_life: Some(3600),
                decay_floor: Some(0.2),
                type_: None,
                summary: None,
                content: None,
                details: None,
                physical: None,
                logical: None,
                tags: None,
                tags_add: None,
                tags_remove: None,
                criticality: None,
                confidence: None,
                visibility: None,
                title: None,
                status: None,
                supersedes: None,
                decay_ttl: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["updated"], true);
    }

    // -----------------------------------------------------------------------
    // memory_delete
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn delete_existing() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "To delete", "Content").await;
        let result = server
            .memory_delete(Parameters(DeleteInput {
                id: id.clone(),
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["deleted"], true);

        let get_result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        let err_val = parse_err(&get_result);
        assert_eq!(err_val["error"]["code"], "MEMORY_NOT_FOUND");
    }

    #[tokio::test]
    async fn delete_nonexistent() {
        let (_dir, server) = setup().await;
        // Ensure store exists
        let _ = create_and_get_id(&server, "decision", "Setup", "Content").await;
        let result = server
            .memory_delete(Parameters(DeleteInput {
                id: "nonexistent-id-5678".to_string(),
                project: None,
            }))
            .await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // memory_search
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn search_basic() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(
            &server,
            "decision",
            "Use Rust for speed",
            "Rust is fast and safe",
        )
        .await;
        let _ = create_and_get_id(
            &server,
            "convention",
            "snake_case naming",
            "Use snake_case everywhere",
        )
        .await;

        let result = server
            .memory_search(Parameters(SearchInput {
                query: "Rust fast".to_string(),
                types: None,
                tags: None,
                physical: None,
                logical: None,
                min_criticality: None,
                max_results: None,
                include_global: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert!(!val["memories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_with_type_filter() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "Decision mem", "Decision content").await;
        let _ = create_and_get_id(&server, "hazard", "Hazard mem", "Hazard content").await;

        let result = server
            .memory_search(Parameters(SearchInput {
                query: "content".to_string(),
                types: Some(vec!["hazard".to_string()]),
                tags: None,
                physical: None,
                logical: None,
                min_criticality: None,
                max_results: None,
                include_global: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        let memories = val["memories"].as_array().unwrap();
        for m in memories {
            assert_eq!(m["type"], "hazard");
        }
    }

    #[tokio::test]
    async fn search_max_results() {
        let (_dir, server) = setup().await;
        for i in 0..5 {
            let _ = create_and_get_id(
                &server,
                "decision",
                &format!("Memory {}", i),
                &format!("Content about topic {}", i),
            )
            .await;
        }

        let result = server
            .memory_search(Parameters(SearchInput {
                query: "topic".to_string(),
                types: None,
                tags: None,
                physical: None,
                logical: None,
                min_criticality: None,
                max_results: Some(1),
                include_global: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["memories"].as_array().unwrap().len() <= 1);
    }

    #[tokio::test]
    async fn search_no_match() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "About Rust", "Rust content").await;

        let result = server
            .memory_search(Parameters(SearchInput {
                query: "xyzzy_nonexistent_term_9999".to_string(),
                types: None,
                tags: None,
                physical: None,
                logical: None,
                min_criticality: None,
                max_results: None,
                include_global: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["memories"].as_array().unwrap().len(), 0);
    }

    // -----------------------------------------------------------------------
    // memory_retrieve
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn retrieve_by_path() {
        let (_dir, server) = setup().await;
        let input = CreateInput {
            physical: Some(vec!["src/main.rs".to_string()]),
            criticality: Some(0.9),
            ..create_input("decision", "Main entry", "The main function starts here")
        };
        server.memory_create(Parameters(input)).await.unwrap();

        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: Some("src/main.rs".to_string()),
                logical: None,
                query: None,
                types: None,
                tags: None,
                min_criticality: None,
                max_results: None,
                detail_level: None,
                include_expired: None,
                include_global: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert!(!val["memories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn retrieve_by_logical() {
        let (_dir, server) = setup().await;
        let input = CreateInput {
            logical: Some(vec!["auth.login".to_string()]),
            physical: Some(vec!["src/auth/login.rs".to_string()]),
            criticality: Some(0.9),
            ..create_input("convention", "Login convention", "Always use OAuth2")
        };
        server.memory_create(Parameters(input)).await.unwrap();

        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: Some("src/auth/login.rs".to_string()),
                logical: Some(vec!["auth.login".to_string()]),
                query: None,
                types: None,
                tags: None,
                min_criticality: None,
                max_results: None,
                detail_level: None,
                include_expired: None,
                include_global: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert!(!val["memories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn retrieve_detail_level_summary() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(
            &server,
            "decision",
            "Summary test",
            "Content for detail test",
        )
        .await;

        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: Some("/".to_string()),
                logical: None,
                query: None,
                types: None,
                tags: None,
                min_criticality: None,
                max_results: None,
                detail_level: Some("summary".to_string()),
                include_expired: None,
                include_global: None,
                project: None,
            }))
            .await;
        // Should succeed without error
        parse_ok(&result);
    }

    #[tokio::test]
    async fn retrieve_detail_level_invalid() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "Setup", "Content").await;

        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: None,
                logical: None,
                query: None,
                types: None,
                tags: None,
                min_criticality: None,
                max_results: None,
                detail_level: Some("bogus".to_string()),
                include_expired: None,
                include_global: None,
                project: None,
            }))
            .await;
        let val = parse_err(&result);
        assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
    }

    // -----------------------------------------------------------------------
    // memory_list
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn list_empty() {
        let (_dir, server) = setup().await;
        // Init the store by creating and immediately deleting a memory
        let id = create_and_get_id(&server, "decision", "Temp", "Temp").await;
        server
            .memory_delete(Parameters(DeleteInput { id, project: None }))
            .await
            .unwrap();

        let result = server
            .memory_list(Parameters(ListInput {
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["total"], 0);
        assert_eq!(val["memories"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_all() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "First", "Content 1").await;
        let _ = create_and_get_id(&server, "hazard", "Second", "Content 2").await;
        let _ = create_and_get_id(&server, "convention", "Third", "Content 3").await;

        let result = server
            .memory_list(Parameters(ListInput {
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["total"], 3);
    }

    #[tokio::test]
    async fn list_filter_by_type() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "Dec1", "Content").await;
        let _ = create_and_get_id(&server, "hazard", "Haz1", "Content").await;
        let _ = create_and_get_id(&server, "decision", "Dec2", "Content").await;

        let result = server
            .memory_list(Parameters(ListInput {
                types: Some(vec!["decision".to_string()]),
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["total"], 2);
        for m in val["memories"].as_array().unwrap() {
            assert_eq!(m["type"], "decision");
        }
    }

    #[tokio::test]
    async fn list_sort_and_limit() {
        let (_dir, server) = setup().await;
        for i in 0..5 {
            let mut input = create_input("decision", &format!("Mem {}", i), "Content");
            input.criticality = Some(i as f64 * 0.2);
            server.memory_create(Parameters(input)).await.unwrap();
        }

        let result = server
            .memory_list(Parameters(ListInput {
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: Some("criticality".to_string()),
                reverse: None,
                limit: Some(2),
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["total"], 2);
    }

    #[tokio::test]
    async fn list_invalid_sort() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "Setup", "Content").await;

        let result = server
            .memory_list(Parameters(ListInput {
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: Some("bogus".to_string()),
                reverse: None,
                limit: None,
                project: None,
            }))
            .await;
        let val = parse_err(&result);
        assert_eq!(val["error"]["code"], "VALIDATION_ERROR");
    }

    // -----------------------------------------------------------------------
    // memory_challenge + memory_review + memory_resolve
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn challenge_memory() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Old decision", "Maybe wrong").await;
        let result = server
            .memory_challenge(Parameters(ChallengeInput {
                id: id.clone(),
                evidence: "Found contradicting evidence".to_string(),
                source_file: Some("src/test.rs".to_string()),
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["challenged"], true);

        let get_result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["status"], "challenged");
    }

    #[tokio::test]
    async fn review_shows_challenged() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Reviewed decision", "Content").await;
        server
            .memory_challenge(Parameters(ChallengeInput {
                id,
                evidence: "Evidence".to_string(),
                source_file: None,
                project: None,
            }))
            .await
            .unwrap();

        let result = server
            .memory_review(Parameters(ReviewInput {
                scope: None,
                max_results: None,
                type_: None,
                challenged_only: Some(true),
                stale_only: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["total"].as_u64().unwrap() > 0);
        for m in val["memories"].as_array().unwrap() {
            assert_eq!(m["status"], "challenged");
        }
    }

    #[tokio::test]
    async fn review_with_type_filter() {
        let (_dir, server) = setup().await;
        let id1 = create_and_get_id(&server, "decision", "Dec challenged", "Content").await;
        let id2 = create_and_get_id(&server, "hazard", "Haz challenged", "Content").await;
        for id in [&id1, &id2] {
            server
                .memory_challenge(Parameters(ChallengeInput {
                    id: id.clone(),
                    evidence: "Evidence".to_string(),
                    source_file: None,
                    project: None,
                }))
                .await
                .unwrap();
        }

        let result = server
            .memory_review(Parameters(ReviewInput {
                scope: None,
                max_results: None,
                type_: Some("decision".to_string()),
                challenged_only: Some(true),
                stale_only: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        for m in val["memories"].as_array().unwrap() {
            assert_eq!(m["type"], "decision");
        }
    }

    #[tokio::test]
    async fn resolve_keep() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Keep me", "Content").await;
        server
            .memory_challenge(Parameters(ChallengeInput {
                id: id.clone(),
                evidence: "Maybe wrong".to_string(),
                source_file: None,
                project: None,
            }))
            .await
            .unwrap();

        let result = server
            .memory_resolve(Parameters(ResolveInput {
                id: id.clone(),
                action: "keep".to_string(),
                updated_content: None,
                updated_summary: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["resolved"], true);
        assert_eq!(val["action"], "keep");

        let get_result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["status"], "active");
    }

    #[tokio::test]
    async fn resolve_delete() {
        let (_dir, server) = setup().await;
        let id = create_and_get_id(&server, "decision", "Delete me", "Content").await;
        server
            .memory_challenge(Parameters(ChallengeInput {
                id: id.clone(),
                evidence: "Definitely wrong".to_string(),
                source_file: None,
                project: None,
            }))
            .await
            .unwrap();

        let result = server
            .memory_resolve(Parameters(ResolveInput {
                id: id.clone(),
                action: "delete".to_string(),
                updated_content: None,
                updated_summary: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["resolved"], true);
        assert_eq!(val["action"], "delete");

        let get_result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        assert!(get_result.is_err());
    }

    // -----------------------------------------------------------------------
    // memory_stats
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn stats_empty() {
        let (_dir, server) = setup().await;
        // Init store
        let id = create_and_get_id(&server, "decision", "Temp", "Temp").await;
        server
            .memory_delete(Parameters(DeleteInput { id, project: None }))
            .await
            .unwrap();

        let result = server
            .memory_stats(Parameters(StatsInput { project: None }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["total"], 0);
    }

    #[tokio::test]
    async fn stats_populated() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "Dec1", "Content").await;
        let _ = create_and_get_id(&server, "decision", "Dec2", "Content").await;
        let _ = create_and_get_id(&server, "hazard", "Haz1", "Content").await;

        let result = server
            .memory_stats(Parameters(StatsInput { project: None }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["total"], 3);
        assert_eq!(val["by_type"]["decision"], 2);
        assert_eq!(val["by_type"]["hazard"], 1);
    }

    // -----------------------------------------------------------------------
    // memory_gc
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn gc_dry_run() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "Keep me", "Content").await;

        let result = server
            .memory_gc(Parameters(GcInput {
                dry_run: Some(true),
                threshold: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["dry_run"], true);

        // Memory should still be there
        let list_result = server
            .memory_list(Parameters(ListInput {
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: None,
            }))
            .await;
        let list_val = parse_ok(&list_result);
        assert_eq!(list_val["total"], 1);
    }

    #[tokio::test]
    async fn gc_confirm() {
        let (_dir, server) = setup().await;
        // Create a memory with very low criticality
        let mut input = create_input("debug", "Low priority debug", "Ephemeral content");
        input.criticality = Some(0.01);
        input.confidence = Some(0.01);
        server.memory_create(Parameters(input)).await.unwrap();

        // GC with a high threshold to ensure it catches the low-criticality memory
        let result = server
            .memory_gc(Parameters(GcInput {
                dry_run: Some(false),
                threshold: Some(0.99),
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["dry_run"], false);
    }

    // -----------------------------------------------------------------------
    // memory_reindex
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn reindex_basic() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "To reindex", "Content").await;

        let result = server
            .memory_reindex(Parameters(ReindexInput {
                embeddings_only: None,
                index_only: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["indexed"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn reindex_index_only() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "To reindex", "Content").await;

        let result = server
            .memory_reindex(Parameters(ReindexInput {
                embeddings_only: None,
                index_only: Some(true),
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["indexed"].as_u64().unwrap() >= 1);
    }

    // -----------------------------------------------------------------------
    // memory_compress_candidates
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn compress_candidates_basic() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "Candidate", "Content").await;

        let result = server
            .memory_compress_candidates(Parameters(CompressCandidatesInput {
                scope: None,
                threshold: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["candidates"].is_array());
        assert!(val["total"].is_number());
    }

    // -----------------------------------------------------------------------
    // Cross-project: resolve_dir
    // -----------------------------------------------------------------------

    /// Helper: set up two projects (A = server default, B = cross-project target)
    /// with a shared registry that knows about both.
    async fn setup_cross_project() -> (TempDir, TempDir, EngramDbServer) {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let project_id_b = crate::storage::project_id::compute_project_id(dir_b.path());

        let registry = InMemoryRegistry::new();
        // Register project B so resolve_dir can find it
        registry.update(dir_b.path(), &project_id_b).await.unwrap();

        let registry: Arc<dyn RegistryBackend> = Arc::new(registry);
        let server = EngramDbServer::new_with_registry(
            dir_a.path().to_path_buf(),
            Some(EmbeddingBackend::Onnx),
            registry,
        );

        // Init project B's store so cross-project opens work
        MemoryStore::init(dir_b.path(), &InMemoryRegistry::new())
            .await
            .unwrap();

        (dir_a, dir_b, server)
    }

    #[tokio::test]
    async fn resolve_dir_none_returns_self_dir() {
        let (_dir, server) = setup().await;
        let resolved = server.resolve_dir(None).await.unwrap();
        assert_eq!(resolved, server.dir);
    }

    #[tokio::test]
    async fn resolve_dir_valid_project_id() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_id_b = crate::storage::project_id::compute_project_id(dir_b.path());

        let resolved = server.resolve_dir(Some(&project_id_b)).await.unwrap();
        // The registry stores canonicalized paths
        let expected = dir_b.path().canonicalize().unwrap();
        assert_eq!(resolved, expected);
    }

    #[tokio::test]
    async fn resolve_dir_valid_path() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let path_str = dir_b.path().to_string_lossy().to_string();

        let resolved = server.resolve_dir(Some(&path_str)).await.unwrap();
        let expected = dir_b.path().canonicalize().unwrap();
        assert_eq!(resolved, expected);
    }

    #[tokio::test]
    async fn resolve_dir_unregistered_project_id() {
        let (_dir, server) = setup().await;
        let result = server.resolve_dir(Some("abcdef0123456789")).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("PROJECT_NOT_FOUND"), "got: {}", err);
    }

    #[tokio::test]
    async fn resolve_dir_unregistered_path() {
        let (_dir, server) = setup().await;
        let unregistered = TempDir::new().unwrap();
        let path_str = unregistered.path().to_string_lossy().to_string();

        let result = server.resolve_dir(Some(&path_str)).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("PROJECT_NOT_FOUND"), "got: {}", err);
    }

    #[tokio::test]
    async fn resolve_dir_ambiguous_hex_treated_as_id() {
        let (_dir, server) = setup().await;
        // 16-char hex should be treated as project ID, not path
        let result = server.resolve_dir(Some("0123456789abcdef")).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("PROJECT_NOT_FOUND"), "got: {}", err);
    }

    #[tokio::test]
    async fn resolve_dir_relative_path_rejected() {
        let (_dir, server) = setup().await;
        let result = server.resolve_dir(Some("relative/path")).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("VALIDATION_ERROR"), "got: {}", err);
    }

    #[tokio::test]
    async fn resolve_dir_nonexistent_path() {
        let (_dir, server) = setup().await;
        let result = server
            .resolve_dir(Some("/tmp/nonexistent_engramdb_test_dir_12345"))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("PROJECT_NOT_FOUND"), "got: {}", err);
    }

    // -----------------------------------------------------------------------
    // Cross-project: integration tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn cross_project_create_and_get() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_b = dir_b.path().to_string_lossy().to_string();

        // Create a memory in project B from server anchored at A
        let mut input = create_input("decision", "Cross-project decision", "Stored in B");
        input.project = Some(project_b.clone());
        let result = server.memory_create(Parameters(input)).await;
        let val = parse_ok(&result);
        let id = val["id"].as_str().unwrap().to_string();
        assert!(val["created"].as_bool().unwrap());

        // Get it back via project override
        let get_result = server
            .memory_get(Parameters(GetInput {
                id: id.clone(),
                project: Some(project_b.clone()),
            }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["summary"], "Cross-project decision");
        assert_eq!(get_val["content"], "Stored in B");

        // Verify it's NOT in project A
        let get_from_a = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        assert!(get_from_a.is_err());
    }

    #[tokio::test]
    async fn cross_project_search() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_b = dir_b.path().to_string_lossy().to_string();

        // Create memories in project B
        let mut input = create_input(
            "convention",
            "Use snake_case in B",
            "Convention for project B",
        );
        input.project = Some(project_b.clone());
        server.memory_create(Parameters(input)).await.unwrap();

        // Search from server A targeting project B
        let result = server
            .memory_search(Parameters(SearchInput {
                query: "snake_case".to_string(),
                types: None,
                tags: None,
                physical: None,
                logical: None,
                min_criticality: None,
                max_results: None,
                include_global: None,
                project: Some(project_b),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["total"].as_u64().unwrap() > 0);
        assert_eq!(val["memories"][0]["summary"], "Use snake_case in B");
    }

    #[tokio::test]
    async fn cross_project_delete() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_b = dir_b.path().to_string_lossy().to_string();

        // Create in B
        let mut input = create_input("debug", "To delete from B", "Temp content");
        input.project = Some(project_b.clone());
        let result = server.memory_create(Parameters(input)).await;
        let id = parse_ok(&result)["id"].as_str().unwrap().to_string();

        // Delete from B via server A
        let del_result = server
            .memory_delete(Parameters(DeleteInput {
                id: id.clone(),
                project: Some(project_b.clone()),
            }))
            .await;
        let del_val = parse_ok(&del_result);
        assert!(del_val["deleted"].as_bool().unwrap());

        // Confirm gone from B
        let get_result = server
            .memory_get(Parameters(GetInput {
                id,
                project: Some(project_b),
            }))
            .await;
        assert!(get_result.is_err());
    }

    #[tokio::test]
    async fn cross_project_stats() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_b = dir_b.path().to_string_lossy().to_string();

        // Create a memory in B
        let mut input = create_input("hazard", "Hazard in B", "Watch out");
        input.project = Some(project_b.clone());
        server.memory_create(Parameters(input)).await.unwrap();

        // Stats for B from server A
        let result = server
            .memory_stats(Parameters(StatsInput {
                project: Some(project_b),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["total"], 1);
        assert_eq!(val["by_type"]["hazard"], 1);
    }

    #[tokio::test]
    async fn cross_project_uninitialized_store_errors() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let project_id_b = crate::storage::project_id::compute_project_id(dir_b.path());

        let registry = InMemoryRegistry::new();
        registry.update(dir_b.path(), &project_id_b).await.unwrap();

        let registry: Arc<dyn RegistryBackend> = Arc::new(registry);
        let server = EngramDbServer::new_with_registry(
            dir_a.path().to_path_buf(),
            Some(EmbeddingBackend::Onnx),
            registry,
        );

        // Do NOT init project B — it should fail with StoreNotInitialized
        let project_b = dir_b.path().to_string_lossy().to_string();
        let result = server
            .memory_stats(Parameters(StatsInput {
                project: Some(project_b),
            }))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("STORE_NOT_INITIALIZED"),
            "Expected STORE_NOT_INITIALIZED, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn default_behavior_preserved() {
        let (_dir, server) = setup().await;
        // Create without project override — should work as before
        let id = create_and_get_id(&server, "decision", "Default project", "Content").await;
        let result = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["summary"], "Default project");
    }

    #[tokio::test]
    async fn cross_project_update() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_b = dir_b.path().to_string_lossy().to_string();

        // Create in B
        let mut input = create_input("decision", "Original summary", "Original content");
        input.project = Some(project_b.clone());
        let result = server.memory_create(Parameters(input)).await;
        let id = parse_ok(&result)["id"].as_str().unwrap().to_string();

        // Update in B from server A
        let update_result = server
            .memory_update(Parameters(UpdateInput {
                id: id.clone(),
                summary: Some("Updated summary".to_string()),
                content: Some("Updated content".to_string()),
                type_: None,
                details: None,
                physical: None,
                logical: None,
                tags: None,
                tags_add: None,
                tags_remove: None,
                criticality: None,
                confidence: None,
                visibility: None,
                title: None,
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
                project: Some(project_b.clone()),
            }))
            .await;
        let update_val = parse_ok(&update_result);
        assert!(update_val["updated"].as_bool().unwrap());

        // Verify update landed in B
        let get_result = server
            .memory_get(Parameters(GetInput {
                id,
                project: Some(project_b),
            }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["summary"], "Updated summary");
        assert_eq!(get_val["content"], "Updated content");
    }

    #[tokio::test]
    async fn cross_project_write_isolation() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_b = dir_b.path().to_string_lossy().to_string();

        // Create memories in A
        create_and_get_id(&server, "decision", "A memory 1", "Content A1").await;
        create_and_get_id(&server, "convention", "A memory 2", "Content A2").await;

        // Get A's count
        let stats_a_before = server
            .memory_stats(Parameters(StatsInput { project: None }))
            .await;
        let count_a_before = parse_ok(&stats_a_before)["total"].as_u64().unwrap();
        assert_eq!(count_a_before, 2);

        // Write to B
        let mut input = create_input("hazard", "B hazard", "B content");
        input.project = Some(project_b.clone());
        server.memory_create(Parameters(input)).await.unwrap();

        // Delete from B
        let mut input2 = create_input("debug", "B debug to delete", "B temp");
        input2.project = Some(project_b.clone());
        let result = server.memory_create(Parameters(input2)).await;
        let id_b = parse_ok(&result)["id"].as_str().unwrap().to_string();
        server
            .memory_delete(Parameters(DeleteInput {
                id: id_b,
                project: Some(project_b),
            }))
            .await
            .unwrap();

        // Verify A is completely unaffected
        let stats_a_after = server
            .memory_stats(Parameters(StatsInput { project: None }))
            .await;
        let count_a_after = parse_ok(&stats_a_after)["total"].as_u64().unwrap();
        assert_eq!(count_a_after, count_a_before);
    }

    #[tokio::test]
    async fn cross_project_via_project_id() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_id_b = crate::storage::project_id::compute_project_id(dir_b.path());

        // Create using project ID instead of path
        let mut input = create_input("convention", "Via project ID", "Created by ID");
        input.project = Some(project_id_b.clone());
        let result = server.memory_create(Parameters(input)).await;
        let val = parse_ok(&result);
        let id = val["id"].as_str().unwrap().to_string();
        assert!(val["created"].as_bool().unwrap());

        // Get back via project ID
        let get_result = server
            .memory_get(Parameters(GetInput {
                id: id.clone(),
                project: Some(project_id_b),
            }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["summary"], "Via project ID");

        // Verify not in A
        let get_from_a = server
            .memory_get(Parameters(GetInput { id, project: None }))
            .await;
        assert!(get_from_a.is_err());
    }

    #[tokio::test]
    async fn cross_project_doctor() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_b = dir_b.path().to_string_lossy().to_string();

        // Create a memory in B so the store has data
        let mut input = create_input("context", "Doctor test", "Health check");
        input.project = Some(project_b.clone());
        server.memory_create(Parameters(input)).await.unwrap();

        // Run doctor on B from server A
        let result = server
            .memory_doctor(Parameters(DoctorInput {
                project: Some(project_b),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["healthy"].as_bool().unwrap());
        assert_eq!(val["on_disk"], 1);
    }

    #[tokio::test]
    async fn cross_project_list() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_b = dir_b.path().to_string_lossy().to_string();

        // Create memories in B
        let mut input1 = create_input("decision", "B decision", "Content");
        input1.project = Some(project_b.clone());
        server.memory_create(Parameters(input1)).await.unwrap();

        let mut input2 = create_input("hazard", "B hazard", "Content");
        input2.project = Some(project_b.clone());
        server.memory_create(Parameters(input2)).await.unwrap();

        // List from A targeting B
        let result = server
            .memory_list(Parameters(ListInput {
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: Some(project_b),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["total"], 2);

        // List A — should have nothing
        let result_a = server
            .memory_list(Parameters(ListInput {
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: None,
            }))
            .await;
        let val_a = parse_ok(&result_a);
        assert_eq!(val_a["total"], 0);
    }

    #[tokio::test]
    async fn cross_project_challenge_and_review() {
        let (_dir_a, dir_b, server) = setup_cross_project().await;
        let project_b = dir_b.path().to_string_lossy().to_string();

        // Create in B
        let mut input = create_input("decision", "Questionable decision", "Maybe wrong");
        input.project = Some(project_b.clone());
        let result = server.memory_create(Parameters(input)).await;
        let id = parse_ok(&result)["id"].as_str().unwrap().to_string();

        // Challenge from A targeting B
        let challenge_result = server
            .memory_challenge(Parameters(ChallengeInput {
                id: id.clone(),
                evidence: "New evidence contradicts this".to_string(),
                source_file: None,
                project: Some(project_b.clone()),
            }))
            .await;
        let challenge_val = parse_ok(&challenge_result);
        assert!(challenge_val["challenged"].as_bool().unwrap());

        // Review B from A — should show the challenged memory
        let review_result = server
            .memory_review(Parameters(ReviewInput {
                scope: None,
                max_results: None,
                type_: None,
                challenged_only: Some(true),
                stale_only: None,
                project: Some(project_b),
            }))
            .await;
        let review_val = parse_ok(&review_result);
        assert_eq!(review_val["total"], 1);
        assert_eq!(review_val["memories"][0]["id"], id);
    }

    #[tokio::test]
    async fn cross_project_create_on_uninitialized_errors() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let project_id_b = crate::storage::project_id::compute_project_id(dir_b.path());
        let registry = InMemoryRegistry::new();
        registry.update(dir_b.path(), &project_id_b).await.unwrap();

        let registry: Arc<dyn RegistryBackend> = Arc::new(registry);
        let server = EngramDbServer::new_with_registry(
            dir_a.path().to_path_buf(),
            Some(EmbeddingBackend::Onnx),
            registry,
        );

        // Try to create in uninitialized B — should fail, NOT auto-init
        let project_b = dir_b.path().to_string_lossy().to_string();
        let mut input = create_input("decision", "Should fail", "No auto-init");
        input.project = Some(project_b);
        let result = server.memory_create(Parameters(input)).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("STORE_NOT_INITIALIZED"),
            "Expected STORE_NOT_INITIALIZED, got: {}",
            err
        );

        // Verify B was NOT auto-initialized
        assert!(!dir_b.path().join(".engramdb").exists());
    }

    // =======================================================================
    // Global memory tests — feature parity with project-scoped memories
    // =======================================================================

    /// Setup for global tests.
    /// With nextest, each test runs in its own process with an isolated
    /// ENGRAMDB_DATA_DIR, so no locking or cleanup is needed.
    async fn setup_global() -> (TempDir, EngramDbServer) {
        MemoryStore::init_global().await.unwrap();
        setup().await
    }

    fn global_project() -> Option<String> {
        Some("global".to_string())
    }

    fn create_global_input(type_: &str, summary: &str, content: &str) -> CreateInput {
        CreateInput {
            project: global_project(),
            ..create_input(type_, summary, content)
        }
    }

    async fn create_global_and_get_id(
        server: &EngramDbServer,
        type_: &str,
        summary: &str,
        content: &str,
    ) -> String {
        let result = server
            .memory_create(Parameters(create_global_input(type_, summary, content)))
            .await;
        let val = parse_ok(&result);
        val["id"].as_str().unwrap().to_string()
    }

    // --- Global CRUD ---

    #[tokio::test]
    async fn global_create_basic() {
        let (_dir, server) = setup_global().await;
        let result = server
            .memory_create(Parameters(create_global_input(
                "decision",
                "Global preference",
                "I prefer tabs over spaces",
            )))
            .await;
        let val = parse_ok(&result);
        assert!(val["id"].is_string());
        assert_eq!(val["created"], true);
        assert_eq!(val["summary"], "Global preference");
    }

    #[tokio::test]
    async fn global_create_with_all_fields() {
        let (_dir, server) = setup_global().await;
        let input = CreateInput {
            type_: "convention".to_string(),
            content: "Always use semantic versioning".to_string(),
            summary: "Use semver".to_string(),
            details: Some("Major.Minor.Patch format".to_string()),
            physical: Some(vec!["**/Cargo.toml".to_string()]),
            logical: Some(vec!["versioning".to_string()]),
            tags: Some(vec!["global".to_string(), "convention".to_string()]),
            criticality: Some(0.9),
            confidence: Some(0.95),
            visibility: Some("shared".to_string()),
            supersedes: Some(vec![]),
            decay_strategy: None,
            decay_half_life: None,
            decay_ttl: None,
            decay_floor: None,
            title: Some("Semver Convention".to_string()),
            title_strategy: None,
            project: global_project(),
        };
        let result = server.memory_create(Parameters(input)).await;
        let val = parse_ok(&result);
        assert!(val["id"].is_string());
        assert_eq!(val["created"], true);
    }

    #[tokio::test]
    async fn global_get() {
        let (_dir, server) = setup_global().await;
        let id = create_global_and_get_id(
            &server,
            "preference",
            "Dark mode pref",
            "Always use dark mode in editors",
        )
        .await;

        let result = server
            .memory_get(Parameters(GetInput {
                id: id.clone(),
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["id"], id);
        assert_eq!(val["summary"], "Dark mode pref");
        assert_eq!(val["content"], "Always use dark mode in editors");
    }

    #[tokio::test]
    async fn global_update() {
        let (_dir, server) = setup_global().await;
        let id = create_global_and_get_id(
            &server,
            "convention",
            "Commit convention",
            "Use conventional commits",
        )
        .await;

        let result = server
            .memory_update(Parameters(UpdateInput {
                id: id.clone(),
                type_: None,
                content: Some("Use conventional commits with scope".to_string()),
                summary: Some("Commit convention v2".to_string()),
                details: None,
                physical: None,
                logical: None,
                tags: Some(vec!["git".to_string()]),
                tags_add: None,
                tags_remove: None,
                criticality: Some(0.8),
                confidence: None,
                visibility: None,
                title: None,
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["updated"], true);

        // Verify the update
        let get_result = server
            .memory_get(Parameters(GetInput {
                id,
                project: global_project(),
            }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["summary"], "Commit convention v2");
        assert_eq!(get_val["content"], "Use conventional commits with scope");
        assert!(get_val["tags"].as_array().unwrap().contains(&json!("git")));
    }

    #[tokio::test]
    async fn global_delete() {
        let (_dir, server) = setup_global().await;
        let id = create_global_and_get_id(
            &server,
            "decision",
            "To delete globally",
            "Global content to remove",
        )
        .await;

        let result = server
            .memory_delete(Parameters(DeleteInput {
                id: id.clone(),
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["deleted"], true);

        // Verify it's gone
        let get_result = server
            .memory_get(Parameters(GetInput {
                id,
                project: global_project(),
            }))
            .await;
        let err_val = parse_err(&get_result);
        assert_eq!(err_val["error"]["code"], "MEMORY_NOT_FOUND");
    }

    // --- Global isolation ---

    #[tokio::test]
    async fn global_memories_isolated_from_project() {
        let (_dir, server) = setup_global().await;

        // Create in global store
        let global_id = create_global_and_get_id(
            &server,
            "preference",
            "Global only memory",
            "This lives only in global",
        )
        .await;

        // Create in project store
        let project_id = create_and_get_id(
            &server,
            "decision",
            "Project only memory",
            "Project content",
        )
        .await;

        // Global memory NOT visible in project
        let result = server
            .memory_get(Parameters(GetInput {
                id: global_id.clone(),
                project: None,
            }))
            .await;
        assert!(result.is_err());

        // Project memory NOT visible in global
        let result = server
            .memory_get(Parameters(GetInput {
                id: project_id,
                project: global_project(),
            }))
            .await;
        assert!(result.is_err());

        // Global memory IS visible with project="global"
        let result = server
            .memory_get(Parameters(GetInput {
                id: global_id,
                project: global_project(),
            }))
            .await;
        assert!(result.is_ok());
    }

    // --- Global retrieve & search ---

    #[tokio::test]
    async fn global_retrieve() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(
            &server,
            "convention",
            "Always lint code",
            "Run linters before committing",
        )
        .await;

        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: Some("/".to_string()),
                logical: None,
                query: None,
                types: None,
                tags: None,
                min_criticality: None,
                max_results: None,
                detail_level: None,
                include_expired: None,
                include_global: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(!val["memories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn global_retrieve_with_semantic_query() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(
            &server,
            "convention",
            "Error handling convention",
            "Always use Result types for error handling in Rust",
        )
        .await;

        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: None,
                logical: None,
                query: Some("error handling".to_string()),
                types: None,
                tags: None,
                min_criticality: None,
                max_results: None,
                detail_level: None,
                include_expired: None,
                include_global: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(!val["memories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn global_search() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(
            &server,
            "preference",
            "Editor font preference",
            "Use JetBrains Mono font in all editors",
        )
        .await;

        let result = server
            .memory_search(Parameters(SearchInput {
                query: "JetBrains Mono".to_string(),
                types: None,
                tags: None,
                physical: None,
                logical: None,
                min_criticality: None,
                max_results: None,
                include_global: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(!val["memories"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn global_search_with_type_filter() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(
            &server,
            "convention",
            "Convention in global",
            "Convention content",
        )
        .await;
        create_global_and_get_id(&server, "hazard", "Hazard in global", "Hazard content").await;

        let result = server
            .memory_search(Parameters(SearchInput {
                query: "content".to_string(),
                types: Some(vec!["hazard".to_string()]),
                tags: None,
                physical: None,
                logical: None,
                min_criticality: None,
                max_results: None,
                include_global: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        let memories = val["memories"].as_array().unwrap();
        for m in memories {
            assert_eq!(m["type"], "hazard");
        }
    }

    // --- include_global merges results ---

    #[tokio::test]
    async fn include_global_in_retrieve() {
        let (_dir, server) = setup_global().await;

        // Create a global memory
        create_global_and_get_id(
            &server,
            "convention",
            "Global lint convention",
            "Always run clippy before committing Rust code",
        )
        .await;

        // Create a project memory
        create_and_get_id(
            &server,
            "decision",
            "Project-specific decision",
            "Use tokio runtime",
        )
        .await;

        // Retrieve with include_global=true should include both
        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: Some("/".to_string()),
                logical: None,
                query: None,
                types: None,
                tags: None,
                min_criticality: None,
                max_results: Some(20),
                detail_level: None,
                include_expired: None,
                include_global: Some(true),
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        let memories = val["memories"].as_array().unwrap();
        let summaries: Vec<&str> = memories
            .iter()
            .filter_map(|m| m["summary"].as_str())
            .collect();
        assert!(
            summaries.contains(&"Global lint convention"),
            "Expected global memory in results, got: {:?}",
            summaries
        );
        assert!(
            summaries.contains(&"Project-specific decision"),
            "Expected project memory in results, got: {:?}",
            summaries
        );
    }

    #[tokio::test]
    async fn include_global_in_search() {
        let (_dir, server) = setup_global().await;

        create_global_and_get_id(
            &server,
            "preference",
            "Global search test memory",
            "This memory is global for search merge test",
        )
        .await;

        create_and_get_id(
            &server,
            "decision",
            "Project search test memory",
            "This memory is project-scoped for search merge test",
        )
        .await;

        let result = server
            .memory_search(Parameters(SearchInput {
                query: "search test memory".to_string(),
                types: None,
                tags: None,
                physical: None,
                logical: None,
                min_criticality: None,
                max_results: Some(20),
                include_global: Some(true),
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        let memories = val["memories"].as_array().unwrap();
        let summaries: Vec<&str> = memories
            .iter()
            .filter_map(|m| m["summary"].as_str())
            .collect();
        assert!(
            summaries.contains(&"Global search test memory"),
            "Expected global memory in search results, got: {:?}",
            summaries
        );
        assert!(
            summaries.contains(&"Project search test memory"),
            "Expected project memory in search results, got: {:?}",
            summaries
        );
    }

    #[tokio::test]
    async fn include_global_false_excludes_global() {
        let (_dir, server) = setup_global().await;

        create_global_and_get_id(
            &server,
            "convention",
            "Global only for exclusion test",
            "Should not appear when include_global=false",
        )
        .await;

        create_and_get_id(
            &server,
            "decision",
            "Project only for exclusion test",
            "Should appear",
        )
        .await;

        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: Some("/".to_string()),
                logical: None,
                query: None,
                types: None,
                tags: None,
                min_criticality: None,
                max_results: Some(20),
                detail_level: None,
                include_expired: None,
                include_global: Some(false),
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        let memories = val["memories"].as_array().unwrap();
        let summaries: Vec<&str> = memories
            .iter()
            .filter_map(|m| m["summary"].as_str())
            .collect();
        assert!(
            !summaries.contains(&"Global only for exclusion test"),
            "Global memory should NOT appear when include_global=false, got: {:?}",
            summaries
        );
    }

    #[tokio::test]
    async fn include_global_default_excludes_global() {
        let (_dir, server) = setup_global().await;

        create_global_and_get_id(
            &server,
            "convention",
            "Global default exclusion test",
            "Should not appear when include_global is omitted",
        )
        .await;

        create_and_get_id(
            &server,
            "decision",
            "Project default exclusion test",
            "Should appear",
        )
        .await;

        // include_global defaults to None (false)
        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: Some("/".to_string()),
                logical: None,
                query: None,
                types: None,
                tags: None,
                min_criticality: None,
                max_results: Some(20),
                detail_level: None,
                include_expired: None,
                include_global: None,
                project: None,
            }))
            .await;
        let val = parse_ok(&result);
        let memories = val["memories"].as_array().unwrap();
        let summaries: Vec<&str> = memories
            .iter()
            .filter_map(|m| m["summary"].as_str())
            .collect();
        assert!(
            !summaries.contains(&"Global default exclusion test"),
            "Global memory should NOT appear by default, got: {:?}",
            summaries
        );
    }

    // --- Global challenge / review / resolve ---

    #[tokio::test]
    async fn global_challenge_and_review() {
        let (_dir, server) = setup_global().await;
        let id = create_global_and_get_id(
            &server,
            "convention",
            "Challengeable convention",
            "Use semicolons in JS",
        )
        .await;

        // Challenge
        let result = server
            .memory_challenge(Parameters(ChallengeInput {
                id: id.clone(),
                evidence: "Modern JS style guides recommend no semicolons".to_string(),
                source_file: Some(".eslintrc".to_string()),
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["challenged"], true);

        // Review should list challenged memory
        let result = server
            .memory_review(Parameters(ReviewInput {
                scope: None,
                max_results: None,
                type_: None,
                challenged_only: Some(true),
                stale_only: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        let memories = val["memories"].as_array().unwrap();
        assert!(
            memories.iter().any(|m| m["id"] == id),
            "Challenged global memory should appear in review"
        );
    }

    #[tokio::test]
    async fn global_resolve_keep() {
        let (_dir, server) = setup_global().await;
        let id = create_global_and_get_id(
            &server,
            "convention",
            "Resolve test",
            "Convention to resolve",
        )
        .await;

        // Challenge first
        server
            .memory_challenge(Parameters(ChallengeInput {
                id: id.clone(),
                evidence: "Might be wrong".to_string(),
                source_file: None,
                project: global_project(),
            }))
            .await
            .unwrap();

        // Resolve by keeping
        let result = server
            .memory_resolve(Parameters(ResolveInput {
                id: id.clone(),
                action: "keep".to_string(),
                updated_content: None,
                updated_summary: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["resolved"], true);
        assert_eq!(val["action"], "keep");

        // Verify it's back to active
        let get_result = server
            .memory_get(Parameters(GetInput {
                id,
                project: global_project(),
            }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["status"], "active");
    }

    #[tokio::test]
    async fn global_resolve_update() {
        let (_dir, server) = setup_global().await;
        let id = create_global_and_get_id(
            &server,
            "convention",
            "To resolve with update",
            "Original content",
        )
        .await;

        server
            .memory_challenge(Parameters(ChallengeInput {
                id: id.clone(),
                evidence: "Needs update".to_string(),
                source_file: None,
                project: global_project(),
            }))
            .await
            .unwrap();

        let result = server
            .memory_resolve(Parameters(ResolveInput {
                id: id.clone(),
                action: "update".to_string(),
                updated_content: Some("Updated content after resolve".to_string()),
                updated_summary: Some("Resolved convention".to_string()),
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["resolved"], true);

        let get_val = parse_ok(
            &server
                .memory_get(Parameters(GetInput {
                    id,
                    project: global_project(),
                }))
                .await,
        );
        assert_eq!(get_val["summary"], "Resolved convention");
        assert_eq!(get_val["content"], "Updated content after resolve");
    }

    #[tokio::test]
    async fn global_resolve_delete() {
        let (_dir, server) = setup_global().await;
        let id = create_global_and_get_id(
            &server,
            "decision",
            "To resolve-delete",
            "Will be removed by resolve",
        )
        .await;

        server
            .memory_challenge(Parameters(ChallengeInput {
                id: id.clone(),
                evidence: "Should be deleted".to_string(),
                source_file: None,
                project: global_project(),
            }))
            .await
            .unwrap();

        let result = server
            .memory_resolve(Parameters(ResolveInput {
                id: id.clone(),
                action: "delete".to_string(),
                updated_content: None,
                updated_summary: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["resolved"], true);

        // Should be gone
        let get_result = server
            .memory_get(Parameters(GetInput {
                id,
                project: global_project(),
            }))
            .await;
        assert!(get_result.is_err());
    }

    // --- Global list, stats, doctor, reindex, gc ---

    #[tokio::test]
    async fn global_list() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(
            &server,
            "decision",
            "Listed global decision",
            "Decision content",
        )
        .await;
        create_global_and_get_id(
            &server,
            "convention",
            "Listed global convention",
            "Convention content",
        )
        .await;

        let result = server
            .memory_list(Parameters(ListInput {
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["total"].as_u64().unwrap() >= 2);
    }

    #[tokio::test]
    async fn global_list_with_type_filter() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(&server, "decision", "G decision", "Decision content").await;
        create_global_and_get_id(&server, "hazard", "G hazard", "Hazard content").await;

        let result = server
            .memory_list(Parameters(ListInput {
                types: Some(vec!["hazard".to_string()]),
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        let memories = val["memories"].as_array().unwrap();
        for m in memories {
            assert_eq!(m["type"], "hazard");
        }
    }

    #[tokio::test]
    async fn global_stats() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(&server, "decision", "Stat decision", "Content").await;
        create_global_and_get_id(&server, "hazard", "Stat hazard", "Content").await;

        let result = server
            .memory_stats(Parameters(StatsInput {
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["total"].as_u64().unwrap() >= 2);
        assert!(val["by_type"]["decision"].as_u64().unwrap() >= 1);
        assert!(val["by_type"]["hazard"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn global_doctor() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(&server, "decision", "Doctor test", "Content").await;

        let result = server
            .memory_doctor(Parameters(DoctorInput {
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["healthy"], true);
        assert!(val["indexed"].as_u64().unwrap() >= 1);
        assert!(val["on_disk"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn global_reindex() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(&server, "decision", "Reindex test", "Content to reindex").await;

        let result = server
            .memory_reindex(Parameters(ReindexInput {
                embeddings_only: None,
                index_only: Some(true),
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["indexed"].as_u64().unwrap() >= 1);

        // Memory should still be accessible after reindex
        let list_result = server
            .memory_list(Parameters(ListInput {
                types: None,
                tags: None,
                status: None,
                scope: None,
                sort_field: None,
                reverse: None,
                limit: None,
                project: global_project(),
            }))
            .await;
        let list_val = parse_ok(&list_result);
        assert!(list_val["total"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn global_gc_dry_run() {
        let (_dir, server) = setup_global().await;
        create_global_and_get_id(&server, "decision", "GC test", "Content").await;

        let result = server
            .memory_gc(Parameters(GcInput {
                dry_run: Some(true),
                threshold: None,
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["dry_run"], true);
    }

    // --- Global compress ---

    #[tokio::test]
    async fn global_compress_candidates() {
        let (_dir, server) = setup_global().await;

        // Create low-criticality global memories
        let input1 = CreateInput {
            criticality: Some(0.1),
            ..create_global_input("context", "Low crit 1", "Low criticality global context 1")
        };
        server.memory_create(Parameters(input1)).await.unwrap();

        let input2 = CreateInput {
            criticality: Some(0.1),
            ..create_global_input("context", "Low crit 2", "Low criticality global context 2")
        };
        server.memory_create(Parameters(input2)).await.unwrap();

        let result = server
            .memory_compress_candidates(Parameters(CompressCandidatesInput {
                scope: None,
                threshold: Some(0.3),
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["total"].as_u64().unwrap() >= 2);
    }

    #[tokio::test]
    async fn global_compress_apply() {
        let (_dir, server) = setup_global().await;

        let id1 = create_global_and_get_id(
            &server,
            "context",
            "Compress source 1",
            "Global context to compress A",
        )
        .await;
        let id2 = create_global_and_get_id(
            &server,
            "context",
            "Compress source 2",
            "Global context to compress B",
        )
        .await;

        let result = server
            .memory_compress_apply(Parameters(CompressApplyInput {
                source_ids: vec![id1.clone(), id2.clone()],
                summary: "Compressed global ctx".to_string(),
                content: "Combined global context A and B".to_string(),
                scope: None,
                tags: Some(vec!["compressed".to_string()]),
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["applied"], true);
        assert_eq!(val["superseded_count"], 2);
        assert!(val["new_id"].is_string());

        // New memory should be accessible
        let new_id = val["new_id"].as_str().unwrap();
        let get_result = server
            .memory_get(Parameters(GetInput {
                id: new_id.to_string(),
                project: global_project(),
            }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["summary"], "Compressed global ctx");
    }

    // --- Global with all memory types ---

    #[tokio::test]
    async fn global_all_memory_types() {
        let (_dir, server) = setup_global().await;
        let types = [
            "decision",
            "convention",
            "hazard",
            "context",
            "intent",
            "relationship",
            "debug",
            "preference",
        ];

        for t in &types {
            let result = server
                .memory_create(Parameters(create_global_input(
                    t,
                    &format!("Global {} memory", t),
                    &format!("Content for global {}", t),
                )))
                .await;
            let val = parse_ok(&result);
            assert_eq!(val["created"], true, "Failed to create global {} memory", t);
        }

        let result = server
            .memory_stats(Parameters(StatsInput {
                project: global_project(),
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["total"].as_u64().unwrap() >= types.len() as u64);
    }

    // --- Global with personal visibility ---

    #[tokio::test]
    async fn global_personal_visibility() {
        let (_dir, server) = setup_global().await;
        let input = CreateInput {
            visibility: Some("personal".to_string()),
            ..create_global_input(
                "preference",
                "Personal global pref",
                "My personal preference stored globally",
            )
        };
        let result = server.memory_create(Parameters(input)).await;
        let val = parse_ok(&result);
        let id = val["id"].as_str().unwrap();

        let get_result = server
            .memory_get(Parameters(GetInput {
                id: id.to_string(),
                project: global_project(),
            }))
            .await;
        let get_val = parse_ok(&get_result);
        assert_eq!(get_val["summary"], "Personal global pref");
    }

    // --- Global retrieve detail levels ---

    #[tokio::test]
    async fn global_retrieve_detail_levels() {
        let (_dir, server) = setup_global().await;
        let input = CreateInput {
            details: Some("Extended details for this global memory".to_string()),
            ..create_global_input(
                "decision",
                "Detail level test",
                "Content for detail level test",
            )
        };
        server.memory_create(Parameters(input)).await.unwrap();

        for level in &["summary", "content", "full"] {
            let result = server
                .memory_retrieve(Parameters(RetrieveInput {
                    path: Some("/".to_string()),
                    logical: None,
                    query: None,
                    types: None,
                    tags: None,
                    min_criticality: None,
                    max_results: None,
                    detail_level: Some(level.to_string()),
                    include_expired: None,
                    include_global: None,
                    project: global_project(),
                }))
                .await;
            let val = parse_ok(&result);
            assert!(
                !val["memories"].as_array().unwrap().is_empty(),
                "detail_level={} returned no memories",
                level
            );
        }
    }

    // --- Global auto-init ---

    #[tokio::test]
    async fn global_auto_initializes() {
        // Unlike non-default project stores, global should auto-init
        let (_dir, server) = setup_global().await;

        // This should succeed even if global store wasn't explicitly initialized
        let result = server
            .memory_create(Parameters(create_global_input(
                "decision",
                "Auto-init test",
                "Testing auto-initialization",
            )))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["created"], true);
    }

    // =========================================================================
    // Worktree / project hierarchy tests
    // =========================================================================

    /// Build a fake linked-worktree layout under `root`:
    ///
    ///   <root>/main/.git/                 (main .git dir)
    ///   <root>/main/.git/worktrees/wt/    (per-worktree gitdir)
    ///   <root>/wt/.git                    (file: `gitdir: <abs path>`)
    ///
    /// Returns (canonicalized main path, canonicalized worktree path).
    fn make_fake_worktree_mcp(root: &std::path::Path) -> (PathBuf, PathBuf) {
        let main = root.join("main");
        let wt = root.join("wt");
        let wt_gitdir = main.join(".git").join("worktrees").join("wt");
        std::fs::create_dir_all(main.join(".git")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::create_dir_all(&wt_gitdir).unwrap();
        std::fs::write(wt_gitdir.join("commondir"), "../..").unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", wt_gitdir.display()),
        )
        .unwrap();
        (main.canonicalize().unwrap(), wt.canonicalize().unwrap())
    }

    fn new_server_at(dir: &std::path::Path, registry: Arc<dyn RegistryBackend>) -> EngramDbServer {
        EngramDbServer::new_with_registry(dir.to_path_buf(), Some(EmbeddingBackend::Onnx), registry)
    }

    #[tokio::test]
    async fn effective_dir_in_worktree_resolves_to_main() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree_mcp(tmp.path());
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = new_server_at(&wt, registry);
        assert_eq!(server.dir, wt);
        assert_eq!(server.effective_dir, main);
    }

    #[tokio::test]
    async fn effective_dir_for_non_worktree_equals_dir() {
        let tmp = TempDir::new().unwrap();
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = new_server_at(tmp.path(), registry);
        assert_eq!(server.dir, server.effective_dir);
    }

    #[tokio::test]
    async fn ensure_hierarchy_noop_for_non_worktree() {
        let tmp = TempDir::new().unwrap();
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = new_server_at(tmp.path(), registry.clone());
        server
            .ensure_hierarchy()
            .await
            .expect("ensure_hierarchy should succeed");
        // No registration happens in a non-worktree; store is only init'd on
        // the first actual memory operation.
        let loaded = registry.load().await.unwrap();
        assert!(loaded.projects.is_empty());
        assert!(!tmp.path().join(".engramdb").exists());
    }

    #[tokio::test]
    async fn ensure_hierarchy_auto_inits_main_and_registers_worktree() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree_mcp(tmp.path());
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = new_server_at(&wt, registry.clone());

        server.ensure_hierarchy().await.unwrap();

        // Main project got initialized.
        assert!(main.join(".engramdb").exists());
        // Worktree did NOT get its own .engramdb/.
        assert!(!wt.join(".engramdb").exists());

        // Registry contains both with the child's parent set to the main id.
        let reg = registry.load().await.unwrap();
        let main_id = crate::storage::project_id::compute_project_id(&main);
        let wt_id = crate::storage::project_id::compute_project_id(&wt);
        let main_entry = reg
            .projects
            .iter()
            .find(|e| e.project_id == main_id)
            .expect("main project registered");
        assert_eq!(main_entry.parent_project_id, None);
        let wt_entry = reg
            .projects
            .iter()
            .find(|e| e.project_id == wt_id)
            .expect("worktree registered");
        assert_eq!(
            wt_entry.parent_project_id.as_deref(),
            Some(main_id.as_str())
        );
    }

    #[tokio::test]
    async fn ensure_hierarchy_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree_mcp(tmp.path());
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = new_server_at(&wt, registry.clone());

        server.ensure_hierarchy().await.unwrap();
        server.ensure_hierarchy().await.unwrap();
        server.ensure_hierarchy().await.unwrap();

        // Still exactly two entries after repeated calls.
        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 2);
        let wt_id = crate::storage::project_id::compute_project_id(&wt);
        let main_id = crate::storage::project_id::compute_project_id(&main);
        let wt_entry = reg.projects.iter().find(|e| e.project_id == wt_id).unwrap();
        assert_eq!(
            wt_entry.parent_project_id.as_deref(),
            Some(main_id.as_str())
        );
    }

    #[tokio::test]
    async fn ensure_hierarchy_skips_init_when_main_already_initialized() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree_mcp(tmp.path());
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());

        // Pre-initialize the main project.
        MemoryStore::init(&main, registry.as_ref()).await.unwrap();

        let server = new_server_at(&wt, registry.clone());
        server.ensure_hierarchy().await.unwrap();

        // Main still exists; worktree registered with parent link.
        assert!(main.join(".engramdb").exists());
        let reg = registry.load().await.unwrap();
        assert_eq!(reg.projects.len(), 2);
        let wt_id = crate::storage::project_id::compute_project_id(&wt);
        let main_id = crate::storage::project_id::compute_project_id(&main);
        let wt_entry = reg.projects.iter().find(|e| e.project_id == wt_id).unwrap();
        assert_eq!(
            wt_entry.parent_project_id.as_deref(),
            Some(main_id.as_str())
        );
    }

    #[tokio::test]
    async fn resolve_dir_none_in_worktree_returns_main_path() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree_mcp(tmp.path());
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = new_server_at(&wt, registry.clone());
        server.ensure_hierarchy().await.unwrap();

        let resolved = server.resolve_dir(None).await.unwrap();
        assert_eq!(resolved, main);
    }

    #[tokio::test]
    async fn resolve_dir_with_worktree_path_swaps_to_main() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree_mcp(tmp.path());
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = new_server_at(&wt, registry.clone());
        server.ensure_hierarchy().await.unwrap();

        let wt_str = wt.to_string_lossy().to_string();
        let resolved = server.resolve_dir(Some(&wt_str)).await.unwrap();
        assert_eq!(resolved, main);
    }

    #[tokio::test]
    async fn resolve_dir_with_worktree_id_follows_parent_chain() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree_mcp(tmp.path());
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = new_server_at(&wt, registry.clone());
        server.ensure_hierarchy().await.unwrap();

        let wt_id = crate::storage::project_id::compute_project_id(&wt);
        let resolved = server.resolve_dir(Some(&wt_id)).await.unwrap();
        assert_eq!(resolved, main);
    }

    #[tokio::test]
    async fn memory_create_in_worktree_writes_to_main_store() {
        let tmp = TempDir::new().unwrap();
        let (main, wt) = make_fake_worktree_mcp(tmp.path());
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server = new_server_at(&wt, registry.clone());
        server.ensure_hierarchy().await.unwrap();

        // Create a memory via the MCP handler while running in the worktree.
        let res = server
            .memory_create(Parameters(create_input(
                "decision",
                "From worktree",
                "Created while running in linked worktree",
            )))
            .await;
        let val = parse_ok(&res);
        assert_eq!(val["created"], true);

        // The memory should live under the MAIN project's store, not the worktree.
        let main_store = MemoryStore::open(&main).await.unwrap();
        let summaries = main_store.list_summary().await.unwrap();
        assert_eq!(summaries.len(), 1, "memory should be in main project");
        let mem = main_store.get(&summaries[0].id).await.unwrap();
        assert_eq!(mem.summary, "From worktree");

        // The worktree still has no .engramdb/.
        assert!(!wt.join(".engramdb").exists());
    }
}
