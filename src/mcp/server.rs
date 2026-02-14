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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetInput {
    #[schemars(description = "Memory ID")]
    id: String,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DeleteInput {
    #[schemars(description = "Memory ID")]
    id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ChallengeInput {
    #[schemars(description = "Memory ID")]
    id: String,

    #[schemars(description = "Evidence contradicting this memory")]
    evidence: String,

    #[schemars(description = "File where evidence was found")]
    source_file: Option<String>,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CompressCandidatesInput {
    #[schemars(description = "Scope to filter candidates")]
    scope: Option<String>,

    #[schemars(description = "Criticality threshold (default 0.4)")]
    threshold: Option<f64>,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GcInput {
    #[schemars(description = "List only, no delete (default true)")]
    dry_run: Option<bool>,

    #[schemars(description = "Override GC score threshold")]
    threshold: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReindexInput {
    #[schemars(description = "Only re-embed, skip index rebuild")]
    embeddings_only: Option<bool>,

    #[schemars(description = "Only rebuild index, skip embedding")]
    index_only: Option<bool>,
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
    relevance: f64,
    scope: f64,
    trust: f64,
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
    dir: PathBuf,
    embedding_backend: Option<EmbeddingBackend>,
    registry: Arc<dyn RegistryBackend>,
    #[allow(dead_code)]
    tool_router: rmcp::handler::server::tool::ToolRouter<Self>,
}

impl std::fmt::Debug for EngramDbServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngramDbServer")
            .field("dir", &self.dir)
            .field("embedding_backend", &self.embedding_backend)
            .finish()
    }
}

impl EngramDbServer {
    pub fn new(dir: PathBuf, embedding_backend: Option<EmbeddingBackend>) -> Self {
        let registry: Arc<dyn RegistryBackend> =
            Arc::new(FileRegistry::global().expect("Failed to initialize registry"));
        Self {
            dir,
            embedding_backend,
            registry,
            tool_router: Self::tool_router(),
        }
    }

    #[cfg(test)]
    pub fn new_with_registry(
        dir: PathBuf,
        embedding_backend: Option<EmbeddingBackend>,
        registry: Arc<dyn RegistryBackend>,
    ) -> Self {
        Self {
            dir,
            embedding_backend,
            registry,
            tool_router: Self::tool_router(),
        }
    }

    /// Open a MemoryStore, auto-initializing if needed.
    async fn open_store(&self) -> Result<MemoryStore, String> {
        let engramdb_dir = self.dir.join(".engramdb");
        if !engramdb_dir.exists() {
            MemoryStore::init(&self.dir, self.registry.as_ref())
                .await
                .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()))?;
        }
        MemoryStore::open(&self.dir, self.registry.as_ref())
            .await
            .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()))
    }

    async fn load_config(&self) -> crate::types::EngramConfig {
        let config_path = self.dir.join(".engramdb").join("config.toml");
        load_config(&config_path).await.unwrap_or_default()
    }

    /// Build a RetrievalEngine with optional embeddings support.
    async fn build_engine(&self) -> Result<RetrievalEngine, String> {
        let store = self.open_store().await?;
        let config_path = self.dir.join(".engramdb").join("config.toml");
        Ok(ops::build_engine(store, &config_path, self.embedding_backend).await)
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

#[tool_router]
impl EngramDbServer {
    #[tool(
        description = "Store a new memory about the project. Use after discovering patterns, decisions, or hazards."
    )]
    async fn memory_create(
        &self,
        Parameters(input): Parameters<CreateInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;
        let engine = self.build_engine().await?;
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
        .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Get memories relevant to your current working context. Call before modifying files."
    )]
    async fn memory_retrieve(
        &self,
        Parameters(input): Parameters<RetrieveInput>,
    ) -> Result<String, String> {
        let engine = self.build_engine().await?;

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

        let result = ops::retrieve_memories(&engine, &query)
            .await
            .map_err(|e| e.to_string())?;

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
                    relevance: sm.score_breakdown.relevance,
                    scope: sm.score_breakdown.scope,
                    trust: sm.score_breakdown.trust,
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
        .map_err(|e| e.to_string())
    }

    #[tool(description = "Search all memories by text, regardless of file context.")]
    async fn memory_search(
        &self,
        Parameters(input): Parameters<SearchInput>,
    ) -> Result<String, String> {
        let engine = self.build_engine().await?;

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

        let filters = SearchFilters {
            types: type_filter,
            tags: input.tags,
            physical: input.physical,
            logical: input.logical,
            min_criticality: input.min_criticality,
        };

        let max = input.max_results.unwrap_or(10);
        let results = ops::search_memories(&engine, &input.query, &filters, Some(max))
            .await
            .map_err(|e| e.to_string())?;

        let memories: Vec<ScoredMemoryOutput> = results
            .iter()
            .map(|sm| ScoredMemoryOutput {
                memory: memory_to_output(&sm.memory, false),
                score: sm.score,
                score_breakdown: ScoreBreakdownOutput {
                    final_score: sm.score_breakdown.final_score,
                    semantic: sm.score_breakdown.semantic,
                    keyword: sm.score_breakdown.keyword,
                    relevance: sm.score_breakdown.relevance,
                    scope: sm.score_breakdown.scope,
                    trust: sm.score_breakdown.trust,
                    decay: sm.score_breakdown.decay,
                    criticality: sm.score_breakdown.criticality,
                },
            })
            .collect();

        serde_json::to_string(&serde_json::json!({
            "memories": memories,
            "total": results.len(),
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(description = "Get full content of a specific memory, including details.")]
    async fn memory_get(&self, Parameters(input): Parameters<GetInput>) -> Result<String, String> {
        let store = self.open_store().await?;
        let memory = ops::get_memory(&store, &input.id)
            .await
            .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        serde_json::to_string(&memory_to_output(&memory, true)).map_err(|e| e.to_string())
    }

    #[tool(description = "Update an existing memory. Cannot change id or created_at.")]
    async fn memory_update(
        &self,
        Parameters(input): Parameters<UpdateInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;
        let engine = self.build_engine().await?;

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
        .map_err(|e| e.to_string())
    }

    #[tool(description = "Permanently delete a memory. Prefer supersedes for corrections.")]
    async fn memory_delete(
        &self,
        Parameters(input): Parameters<DeleteInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;
        ops::delete_memory(&store, &input.id)
            .await
            .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        serde_json::to_string(&serde_json::json!({
            "id": input.id,
            "deleted": true
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(description = "Flag a memory as potentially incorrect and mark for review.")]
    async fn memory_challenge(
        &self,
        Parameters(input): Parameters<ChallengeInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;
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
        .map_err(|e| e.to_string())
    }

    #[tool(description = "List memories needing review (stale or challenged).")]
    async fn memory_review(
        &self,
        Parameters(input): Parameters<ReviewInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;

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
            .map_err(|e| e.to_string())?;

        let outputs: Vec<MemoryOutput> = memories
            .iter()
            .map(|m| memory_to_output(m, false))
            .collect();

        serde_json::to_string(&serde_json::json!({
            "memories": outputs,
            "total": memories.len()
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(description = "Resolve a challenged or needs_review memory: keep, update, or delete.")]
    async fn memory_resolve(
        &self,
        Parameters(input): Parameters<ResolveInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;

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
        .map_err(|e| e.to_string())
    }

    #[tool(
        description = "List low-criticality memories eligible for compression. Review before compress_apply."
    )]
    async fn memory_compress_candidates(
        &self,
        Parameters(input): Parameters<CompressCandidatesInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;
        let result = ops::compress_candidates(&store, input.scope.as_deref(), input.threshold)
            .await
            .map_err(|e| e.to_string())?;

        serde_json::to_string(&serde_json::json!({
            "candidates": result.candidates,
            "total": result.total,
            "threshold": result.threshold,
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Compress multiple memories into one summary. Call compress_candidates first."
    )]
    async fn memory_compress_apply(
        &self,
        Parameters(input): Parameters<CompressApplyInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;
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
        .map_err(|e| e.to_string())
    }

    #[tool(description = "Overview of memory store — counts by type, scope, status.")]
    async fn memory_stats(&self) -> Result<String, String> {
        let store = self.open_store().await?;
        let stats = ops::compute_stats(&store)
            .await
            .map_err(|e| e.to_string())?;

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
        .map_err(|e| e.to_string())
    }

    #[tool(description = "Garbage collect decayed memories. Always dry_run first.")]
    async fn memory_gc(&self, Parameters(input): Parameters<GcInput>) -> Result<String, String> {
        let store = self.open_store().await?;
        let config = self.load_config().await;
        let dry_run = input.dry_run.unwrap_or(true);

        let result = ops::gc_memories(&store, &config, dry_run, input.threshold)
            .await
            .map_err(|e| e.to_string())?;

        let mut response = serde_json::json!({
            "removed": result.removed,
            "count": result.count,
            "dry_run": dry_run
        });
        if !result.stale_entries.is_empty() {
            response["stale_entries"] = serde_json::json!(result.stale_entries);
            response["warning"] =
                serde_json::json!("Stale index entries found. Run memory_reindex to fix.");
        }
        serde_json::to_string(&response).map_err(|e| e.to_string())
    }

    #[tool(description = "Rebuild the search index and embedding vectors.")]
    async fn memory_reindex(
        &self,
        Parameters(input): Parameters<ReindexInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;
        let embeddings_only = input.embeddings_only.unwrap_or(false);
        let index_only = input.index_only.unwrap_or(false);

        // Build engine outside conditional so it stays alive for the reference
        let engine = if !index_only {
            self.build_engine().await.ok()
        } else {
            None
        };

        let result = ops::reindex(&store, engine.as_ref(), embeddings_only)
            .await
            .map_err(|e| e.to_string())?;

        serde_json::to_string(&serde_json::json!({
            "indexed": result.indexed,
            "embedded": result.embedded,
            "errors": result.errors
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(description = "List memories with optional filtering, sorting, and limiting.")]
    async fn memory_list(
        &self,
        Parameters(input): Parameters<ListInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;

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
            .map_err(|e| e.to_string())?;

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
                    "confidence": e.confidence,
                    "created_at": e.created_at.to_rfc3339(),
                    "updated_at": e.updated_at.to_rfc3339(),
                })
            })
            .collect();

        serde_json::to_string(&serde_json::json!({
            "memories": output,
            "total": output.len()
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Check store health (index vs disk consistency). Fast, project-scoped check. For full environment diagnostics, use the CLI: `engramdb doctor`."
    )]
    async fn memory_doctor(&self) -> Result<String, String> {
        let store = self.open_store().await?;
        let result = ops::doctor(&store).await.map_err(|e| e.to_string())?;

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
            response["fix"] = serde_json::json!("Run memory_reindex to repair.");
        }
        serde_json::to_string(&response).map_err(|e| e.to_string())
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
                 Retrieve before modifying files. Store after significant discoveries."
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
                    .list()
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
                         this session, store them using the memory_create tool.\n\
                         If you encounter evidence that contradicts an existing memory, \
                         use memory_challenge and ask the user how to resolve it.",
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
                         1. Did you make any architectural decisions? -> memory_create type: decision\n\
                         2. Did you discover any hazards or footguns? -> memory_create type: hazard\n\
                         3. Did you encounter non-obvious behavior? -> memory_create type: debug\n\
                         4. Did anything contradict existing memories? -> memory_challenge\n\n\
                         {}\n\
                         Run memory_review if you'd like to address flagged memories with the user.",
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
    let server = EngramDbServer::new(dir, embedding_backend);
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

    let config = StreamableHttpServerConfig::default();
    let ct = config.cancellation_token.clone();
    let service = StreamableHttpService::new(
        move || Ok(EngramDbServer::new(dir.clone(), embedding_backend)),
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
    use serde_json::json;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, EngramDbServer) {
        let temp_dir = TempDir::new().unwrap();
        let registry: Arc<dyn RegistryBackend> = Arc::new(InMemoryRegistry::new());
        let server =
            EngramDbServer::new_with_registry(temp_dir.path().to_path_buf(), None, registry);
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
        let result = server.memory_get(Parameters(GetInput { id })).await;
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
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["updated"], true);

        let get_result = server.memory_get(Parameters(GetInput { id })).await;
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
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["updated"], true);

        let get_result = server.memory_get(Parameters(GetInput { id })).await;
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
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["updated"], true);

        let get_result = server.memory_get(Parameters(GetInput { id })).await;
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
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
            }))
            .await;
        parse_ok(&result);

        let get_result = server.memory_get(Parameters(GetInput { id })).await;
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
                status: None,
                supersedes: None,
                decay_strategy: None,
                decay_half_life: None,
                decay_ttl: None,
                decay_floor: None,
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
                id: id.clone(),
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
                status: None,
                supersedes: None,
                decay_ttl: None,
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
            .memory_delete(Parameters(DeleteInput { id: id.clone() }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["deleted"], true);

        let get_result = server.memory_get(Parameters(GetInput { id })).await;
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
            ..create_input("convention", "Login convention", "Always use OAuth2")
        };
        server.memory_create(Parameters(input)).await.unwrap();

        let result = server
            .memory_retrieve(Parameters(RetrieveInput {
                path: None,
                logical: Some(vec!["auth.login".to_string()]),
                query: None,
                types: None,
                tags: None,
                min_criticality: None,
                max_results: None,
                detail_level: None,
                include_expired: None,
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
            .memory_delete(Parameters(DeleteInput { id }))
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
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["challenged"], true);

        let get_result = server.memory_get(Parameters(GetInput { id })).await;
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
            }))
            .await
            .unwrap();

        let result = server
            .memory_resolve(Parameters(ResolveInput {
                id: id.clone(),
                action: "keep".to_string(),
                updated_content: None,
                updated_summary: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["resolved"], true);
        assert_eq!(val["action"], "keep");

        let get_result = server.memory_get(Parameters(GetInput { id })).await;
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
            }))
            .await
            .unwrap();

        let result = server
            .memory_resolve(Parameters(ResolveInput {
                id: id.clone(),
                action: "delete".to_string(),
                updated_content: None,
                updated_summary: None,
            }))
            .await;
        let val = parse_ok(&result);
        assert_eq!(val["resolved"], true);
        assert_eq!(val["action"], "delete");

        let get_result = server.memory_get(Parameters(GetInput { id })).await;
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
            .memory_delete(Parameters(DeleteInput { id }))
            .await
            .unwrap();

        let result = server.memory_stats().await;
        let val = parse_ok(&result);
        assert_eq!(val["total"], 0);
    }

    #[tokio::test]
    async fn stats_populated() {
        let (_dir, server) = setup().await;
        let _ = create_and_get_id(&server, "decision", "Dec1", "Content").await;
        let _ = create_and_get_id(&server, "decision", "Dec2", "Content").await;
        let _ = create_and_get_id(&server, "hazard", "Haz1", "Content").await;

        let result = server.memory_stats().await;
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
            }))
            .await;
        let val = parse_ok(&result);
        assert!(val["candidates"].is_array());
        assert!(val["total"].is_number());
    }
}
