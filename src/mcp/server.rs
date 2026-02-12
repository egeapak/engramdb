//! EngramDB MCP server implementation.
//!
//! Defines the server struct, all MCP tools (14), resources (2), and prompts (2).
//! Tools delegate to the `ops` layer; the server opens a fresh `MemoryStore`
//! per request so it always sees the latest on-disk state.

use std::path::PathBuf;

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
use crate::storage::{FileRegistry, MemoryStore};
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

    #[schemars(description = "File paths this memory applies to (default: [\"/\"])")]
    physical: Option<Vec<String>>,

    #[schemars(description = "Logical scopes in dot notation")]
    logical: Option<Vec<String>>,

    #[schemars(description = "Freeform tags")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Importance 0.0-1.0 (default 0.5)")]
    criticality: Option<f64>,

    #[schemars(description = "Confidence 0.0-1.0 (default 0.8)")]
    confidence: Option<f64>,

    #[schemars(description = "Visibility: shared or personal (default shared)")]
    visibility: Option<String>,

    #[schemars(description = "IDs of memories this supersedes")]
    supersedes: Option<Vec<String>>,

    #[schemars(description = "Decay strategy: none, linear, exponential, or step")]
    decay_strategy: Option<String>,

    #[schemars(description = "Half-life in seconds for decay")]
    decay_half_life: Option<u64>,

    #[schemars(description = "TTL in seconds for decay")]
    decay_ttl: Option<u64>,

    #[schemars(description = "Minimum decay factor (0.0-1.0)")]
    decay_floor: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RetrieveInput {
    #[schemars(description = "Current file path relative to project root")]
    path: Option<String>,

    #[schemars(description = "Current logical scopes")]
    logical: Option<Vec<String>>,

    #[schemars(description = "Optional text query for semantic search")]
    query: Option<String>,

    #[schemars(description = "Filter by memory types")]
    types: Option<Vec<String>>,

    #[schemars(description = "Filter by tags (OR logic)")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Minimum criticality threshold")]
    min_criticality: Option<f64>,

    #[schemars(description = "Maximum results (default 10)")]
    max_results: Option<usize>,

    #[schemars(description = "Detail level: summary, content, or full (default content)")]
    detail_level: Option<String>,

    #[schemars(description = "Include fully decayed/expired memories")]
    include_expired: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchInput {
    #[schemars(description = "Search query matched against summary, content, and tags")]
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

    #[schemars(description = "Maximum results (default 10)")]
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
    #[schemars(description = "New memory type")]
    type_: Option<String>,

    #[schemars(description = "New content")]
    content: Option<String>,

    #[schemars(description = "New summary")]
    summary: Option<String>,

    #[schemars(description = "New details")]
    details: Option<String>,

    #[schemars(description = "New physical scopes")]
    physical: Option<Vec<String>>,

    #[schemars(description = "New logical scopes")]
    logical: Option<Vec<String>>,

    #[schemars(description = "New tags")]
    tags: Option<Vec<String>>,

    #[schemars(description = "Tags to add (merged with existing)")]
    tags_add: Option<Vec<String>>,

    #[schemars(description = "Tags to remove")]
    tags_remove: Option<Vec<String>>,

    #[schemars(description = "New criticality")]
    criticality: Option<f64>,

    #[schemars(description = "New confidence")]
    confidence: Option<f64>,

    #[schemars(description = "New visibility")]
    visibility: Option<String>,

    #[schemars(description = "IDs of memories this supersedes")]
    supersedes: Option<Vec<String>>,

    #[schemars(description = "Decay strategy: none, linear, exponential, or step")]
    decay_strategy: Option<String>,

    #[schemars(description = "Half-life in seconds for decay")]
    decay_half_life: Option<u64>,

    #[schemars(description = "TTL in seconds for decay")]
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

    #[schemars(description = "Evidence that contradicts this memory")]
    evidence: String,

    #[schemars(description = "File where contradicting evidence was found")]
    source_file: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReviewInput {
    #[schemars(description = "Filter to a logical or physical scope")]
    scope: Option<String>,

    #[schemars(description = "Maximum results (default 10)")]
    max_results: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ResolveInput {
    #[schemars(description = "Memory ID")]
    id: String,

    #[schemars(description = "Action: keep, update, or delete")]
    action: String,

    #[schemars(description = "New content (required when action is 'update')")]
    updated_content: Option<String>,

    #[schemars(description = "New summary (optional when action is 'update')")]
    updated_summary: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CompressCandidatesInput {
    #[schemars(description = "Logical or physical scope to filter candidates")]
    scope: Option<String>,

    #[schemars(
        description = "Criticality threshold — memories at or below this are candidates (default 0.4)"
    )]
    threshold: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CompressApplyInput {
    #[schemars(description = "IDs of memories to compress into a single summary")]
    source_ids: Vec<String>,

    #[schemars(description = "One-line summary of the compressed memory")]
    summary: String,

    #[schemars(description = "Full content of the compressed memory")]
    content: String,

    #[schemars(description = "Logical scopes for the new memory")]
    scope: Option<Vec<String>>,

    #[schemars(description = "Tags for the new memory")]
    tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GcInput {
    #[schemars(description = "If true, list only (default true). Set false to delete.")]
    dry_run: Option<bool>,

    #[schemars(description = "Override the GC score threshold")]
    threshold: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReindexInput {
    #[schemars(description = "Only re-embed, don't rebuild index")]
    embeddings_only: Option<bool>,
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
#[derive(Debug, Clone)]
pub struct EngramDbServer {
    dir: PathBuf,
    embedding_backend: Option<EmbeddingBackend>,
    #[allow(dead_code)]
    tool_router: rmcp::handler::server::tool::ToolRouter<Self>,
}

impl EngramDbServer {
    pub fn new(dir: PathBuf, embedding_backend: Option<EmbeddingBackend>) -> Self {
        Self {
            dir,
            embedding_backend,
            tool_router: Self::tool_router(),
        }
    }

    /// Get the global file-backed registry.
    fn get_registry(&self) -> Result<FileRegistry, String> {
        FileRegistry::global()
            .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()))
    }

    /// Open a MemoryStore, auto-initializing if needed.
    async fn open_store(&self) -> Result<MemoryStore, String> {
        let registry = self.get_registry()?;
        let engramdb_dir = self.dir.join(".engramdb");
        if !engramdb_dir.exists() {
            MemoryStore::init(&self.dir, &registry)
                .await
                .map_err(|e| error_response(ErrorCode::StoreNotInitialized, &e.to_string()))?;
        }
        MemoryStore::open(&self.dir, &registry)
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
        description = "Store a new memory about the project. Use after discovering important patterns, making architectural decisions, encountering hazards, or learning conventions."
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

        let result = ops::create_memory(
            &store,
            ops::CreateParams {
                type_,
                content: input.content,
                summary: input.summary,
                physical: input.physical.unwrap_or_default(),
                logical: input.logical.unwrap_or_default(),
                tags: input.tags.unwrap_or_default(),
                criticality: input.criticality.unwrap_or(0.5),
                confidence: input.confidence.unwrap_or(0.8),
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
        description = "Get memories relevant to your current working context. Call before modifying files to surface decisions, hazards, and conventions."
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

        let query = RetrievalQuery {
            path: input.path,
            logical: input.logical.unwrap_or_default(),
            query: input.query,
            types: type_filter,
            tags: input.tags,
            min_criticality: input.min_criticality,
            max_results: Some(input.max_results.unwrap_or(10)),
            include_expired: Some(input.include_expired.unwrap_or(false)),
            ..RetrievalQuery::default()
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

    #[tool(
        description = "Search across all memories by text content. Use when you need specific knowledge regardless of file context."
    )]
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

        let results = ops::search_memories(&engine, &input.query, &filters)
            .await
            .map_err(|e| e.to_string())?;

        let max = input.max_results.unwrap_or(10);
        let memories: Vec<ScoredMemoryOutput> = results
            .iter()
            .take(max)
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

    #[tool(
        description = "Get the full content of a specific memory, including the 'details' field."
    )]
    async fn memory_get(&self, Parameters(input): Parameters<GetInput>) -> Result<String, String> {
        let store = self.open_store().await?;
        let memory = ops::get_memory(&store, &input.id)
            .await
            .map_err(|e| error_response(ErrorCode::MemoryNotFound, &e.to_string()))?;

        serde_json::to_string(&memory_to_output(&memory, true)).map_err(|e| e.to_string())
    }

    #[tool(
        description = "Update an existing memory. Any field can be updated except 'id' and 'created_at'."
    )]
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
                status: None,
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

    #[tool(
        description = "Permanently delete a memory. Prefer memory_update with supersedes for corrections."
    )]
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

    #[tool(
        description = "Flag a memory as potentially incorrect. Reduces retrieval score by 30% and marks for human review."
    )]
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

    #[tool(
        description = "List memories that need human attention - either stale (needs_review) or challenged."
    )]
    async fn memory_review(
        &self,
        Parameters(input): Parameters<ReviewInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;
        let memories = ops::review_memories(&store, input.scope.as_deref(), input.max_results)
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

    #[tool(
        description = "Resolve a challenged or needs_review memory. Use 'keep' to confirm, 'update' to correct, or 'delete' to remove."
    )]
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
        description = "List memories eligible for compression. Returns candidates with criticality at or below the threshold. Review candidates before calling memory_compress_apply."
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
        description = "Compress multiple memories into a single summary memory. You provide the summary and content; the system creates the new memory and marks source memories as superseded. Always call memory_compress_candidates first."
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

    #[tool(description = "Get an overview of the memory store - counts by type, scope, status.")]
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

    #[tool(
        description = "Garbage collect memories that have decayed below the GC threshold. Always dry_run first."
    )]
    async fn memory_gc(&self, Parameters(input): Parameters<GcInput>) -> Result<String, String> {
        let store = self.open_store().await?;
        let config = self.load_config().await;
        let dry_run = input.dry_run.unwrap_or(true);

        let result = ops::gc_memories(&store, &config, dry_run, input.threshold)
            .await
            .map_err(|e| e.to_string())?;

        serde_json::to_string(&serde_json::json!({
            "removed": result.removed,
            "count": result.count,
            "dry_run": dry_run
        }))
        .map_err(|e| e.to_string())
    }

    #[tool(description = "Rebuild the search index and regenerate embedding vectors.")]
    async fn memory_reindex(
        &self,
        Parameters(input): Parameters<ReindexInput>,
    ) -> Result<String, String> {
        let store = self.open_store().await?;
        let embeddings_only = input.embeddings_only.unwrap_or(false);

        let engine = if !embeddings_only {
            None
        } else {
            self.build_engine().await.ok()
        };

        // For full reindex, also try to build engine for embeddings
        let engine_ref = if engine.is_some() {
            engine.as_ref()
        } else if !embeddings_only {
            // Build engine for embedding during full reindex
            let e = self.build_engine().await.ok();
            // We can't return a reference to a local, so we skip embeddings here
            // and do index-only
            drop(e);
            None
        } else {
            None
        };

        let result = ops::reindex(&store, engine_ref, embeddings_only)
            .await
            .map_err(|e| e.to_string())?;

        serde_json::to_string(&serde_json::json!({
            "indexed": result.indexed,
            "embedded": result.embedded,
            "errors": result.errors
        }))
        .map_err(|e| e.to_string())
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
