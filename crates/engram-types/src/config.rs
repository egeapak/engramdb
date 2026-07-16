//! Configuration types for EngramDB retrieval, scoring, and thresholds.
//!
//! This module defines all configuration structures used throughout EngramDB:
//! - [`EngramConfig`] - top-level configuration with all subsections
//! - [`RetrievalConfig`] - retrieval settings (thresholds, max results)
//! - [`ScoringConfig`] - scoring mode weights (with_query, scope_only, degraded)
//! - [`ScoringWeights`] - component weights for semantic, relevance, scope
//! - [`ScopeProximityConfig`] - physical scope bonuses (exact file, same dir, etc.)
//! - [`LogicalBonusConfig`] - logical scope bonuses (exact, parent, sibling)
//! - [`TrustWeights`] - trust scores by provenance source
//! - [`ThresholdsConfig`] - thresholds for needs_review, gc, compress
//!
//! All structs provide sensible defaults and can be loaded from TOML files.

use serde::{Deserialize, Serialize};

pub use super::title_strategy::TitleStrategy;

/// Default HuggingFace repo for the NLI contradiction-detection model.
///
/// Single source of truth for the `[nli].model` default. The ONNX NLI loader's
/// `DEFAULT_NLI_MODEL.repo` (in `crate::nli`) must equal this value; a unit test
/// in that module asserts the two never drift. It lives here, in the `types`
/// foundation, so `NliConfig::default` can reference it without depending
/// "upward" on the ONNX-backed `nli` module (int8-quantized mirror: ~2× faster,
/// ~3.7× less RAM, identical id2label).
pub const DEFAULT_NLI_MODEL_REPO: &str = "Xenova/nli-deberta-v3-xsmall";

/// Weights for scoring components.
///
/// Trust and scope are applied as multipliers on the entire score,
/// not as weighted components here. Scope uses depth decay
/// (`depth_decay_base` / `depth_decay_floor`) from [`ScoringConfig`].
///
/// Active weights should sum to 1.0 (± 0.001). Use [`ScoringWeights::validate`]
/// to check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringWeights {
    /// Keyword match weight (optional — only present in `with_keyword` mode)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub keyword: Option<f64>,

    /// Semantic similarity weight (optional - not available in degraded mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<f64>,

    /// Relevance weight (criticality * decay)
    pub relevance: f64,
}

impl ScoringWeights {
    /// Validate that active weights sum to 1.0 ± 0.001.
    pub fn validate(&self, mode_name: &str) -> Result<(), anyhow::Error> {
        let sum = self.keyword.unwrap_or(0.0) + self.semantic.unwrap_or(0.0) + self.relevance;
        if (sum - 1.0).abs() > 0.001 {
            anyhow::bail!(
                "scoring.{} weights sum to {} (expected 1.0 ± 0.001)",
                mode_name,
                sum
            );
        }
        Ok(())
    }
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            keyword: None,
            semantic: Some(0.55),
            relevance: 0.45,
        }
    }
}

/// Scoring configuration for different retrieval modes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringConfig {
    /// Weights when both query and scope are provided
    pub with_query: ScoringWeights,

    /// Weights when keyword search is active (search path)
    #[serde(default = "ScoringConfig::default_with_keyword")]
    pub with_keyword: ScoringWeights,

    /// Weights for scope-only retrieval
    pub scope_only: ScoringWeights,

    /// Weights for degraded mode (no embeddings)
    pub degraded: ScoringWeights,

    /// Neutral base for the scope multiplier under logical-only context (default 0.5).
    ///
    /// When a query provides a `path`, scope scoring uses depth decay
    /// (`depth_decay_base` / `depth_decay_floor`) and this field is unused.
    /// When a query provides only `logical` context, the multiplier is
    /// `scope_multiplier_floor + logical_bonus` (capped at 1.0) for memories
    /// with a related logical scope, the bare floor for memories with no
    /// logical scopes, and 0.0 for memories whose logical scopes are
    /// unrelated. Without this floor the logical bonus (max 0.3) alone would
    /// drag every logical-only result below `retrieval.relevance_threshold`.
    #[serde(default = "ScoringConfig::default_scope_multiplier_floor")]
    pub scope_multiplier_floor: f64,

    /// Floor for the trust multiplier (default 0.5).
    ///
    /// Trust is applied as a post-multiplier: `floor + (1 - floor) * trust_weight`.
    /// This prevents low-trust memories from being suppressed too aggressively
    /// when combined with other multipliers (scope, challenge).
    #[serde(default = "ScoringConfig::default_trust_multiplier_floor")]
    pub trust_multiplier_floor: f64,

    /// Flat penalty subtracted from score when memory is challenged (default 0.10).
    ///
    /// Applied as `score -= penalty` instead of multiplicative, so the impact
    /// is uniform regardless of trust/scope combination.
    #[serde(default = "ScoringConfig::default_challenge_penalty")]
    pub challenge_penalty: f64,

    /// Base for exponential depth decay of physical scope scores (default 0.82).
    ///
    /// Physical scope scores use `max(depth_decay_floor, depth_decay_base^depth)`
    /// where depth is the number of directory levels between the memory scope
    /// and the queried file path.
    #[serde(default = "ScoringConfig::default_depth_decay_base")]
    pub depth_decay_base: f64,

    /// Floor for depth decay — minimum scope score regardless of depth (default 0.3).
    #[serde(default = "ScoringConfig::default_depth_decay_floor")]
    pub depth_decay_floor: f64,
}

impl ScoringConfig {
    fn default_with_keyword() -> ScoringWeights {
        ScoringWeights {
            keyword: Some(0.45),
            semantic: Some(0.30),
            relevance: 0.25,
        }
    }

    fn default_scope_multiplier_floor() -> f64 {
        0.5
    }

    fn default_trust_multiplier_floor() -> f64 {
        0.5
    }

    fn default_challenge_penalty() -> f64 {
        0.10
    }

    fn default_depth_decay_base() -> f64 {
        0.82
    }

    fn default_depth_decay_floor() -> f64 {
        0.3
    }
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            with_query: ScoringWeights::default(),
            with_keyword: Self::default_with_keyword(),
            scope_only: ScoringWeights {
                keyword: None,
                semantic: None,
                relevance: 1.0,
            },
            degraded: ScoringWeights {
                keyword: None,
                semantic: None,
                relevance: 1.0,
            },
            scope_multiplier_floor: 0.5,
            trust_multiplier_floor: 0.5,
            challenge_penalty: 0.10,
            depth_decay_base: 0.82,
            depth_decay_floor: 0.3,
        }
    }
}

/// Physical scope proximity bonuses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeProximityConfig {
    /// Exact file match bonus
    pub exact_file: f64,

    /// Same directory bonus
    pub same_directory: f64,

    /// Same module/subtree bonus
    pub same_module: f64,

    /// Project root (default) bonus
    pub project_root: f64,
}

impl Default for ScopeProximityConfig {
    fn default() -> Self {
        Self {
            exact_file: 1.0,
            same_directory: 0.85,
            same_module: 0.6,
            project_root: 0.4,
        }
    }
}

/// Logical scope bonuses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogicalBonusConfig {
    /// Exact logical scope match bonus
    pub exact: f64,

    /// Parent scope match bonus
    pub parent: f64,

    /// Sibling scope match bonus
    pub sibling: f64,
}

impl Default for LogicalBonusConfig {
    fn default() -> Self {
        Self {
            exact: 0.3,
            parent: 0.2,
            sibling: 0.15,
        }
    }
}

/// Trust weights by provenance source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustWeights {
    /// Human-created memories
    pub human: f64,

    /// Agent-created memories
    pub agent: f64,

    /// Inferred memories
    pub inferred: f64,

    /// Imported memories
    pub imported: f64,
}

impl TrustWeights {
    /// Validate that each trust weight is in [0.0, 1.0].
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        for (name, value) in [
            ("human", self.human),
            ("agent", self.agent),
            ("inferred", self.inferred),
            ("imported", self.imported),
        ] {
            if !(0.0..=1.0).contains(&value) {
                anyhow::bail!("trust_weights.{} ({}) must be in [0.0, 1.0]", name, value);
            }
        }
        Ok(())
    }
}

impl Default for TrustWeights {
    fn default() -> Self {
        Self {
            human: 1.0,
            agent: 0.85,
            inferred: 0.6,
            imported: 0.7,
        }
    }
}

/// Search-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Weight applied to semantic (cosine) similarity in search scoring.
    /// Higher values make semantic matches dominate over keyword matches.
    pub semantic_weight: f64,

    /// Minimum score threshold for search results (results below this are filtered out).
    pub threshold: f64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            semantic_weight: 3.0,
            threshold: 0.2,
        }
    }
}

/// Thresholds for various operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdsConfig {
    /// Score threshold for needs_review status
    pub needs_review: f64,

    /// Score threshold for garbage collection
    pub gc: f64,

    /// Score threshold for compression
    pub compress: f64,
}

impl Default for ThresholdsConfig {
    fn default() -> Self {
        Self {
            needs_review: 0.3,
            gc: 0.05,
            compress: 0.4,
        }
    }
}

/// Recency-based review-suggestion settings.
///
/// EngramDB nudges agents and users to revisit memories that have gone stale —
/// active memories that have not been touched (updated or verified) in a while.
/// This is a *review* trigger, not a TTL: nothing is deleted or hidden. The
/// suggestion is surfaced by the MCP session-end prompt and the `review` tool,
/// and stale memories are ranked by criticality so the ones most worth
/// re-verifying float to the top (recency by itself is indifferent to utility).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewConfig {
    /// Active memories whose last update is older than this many days are
    /// surfaced as review suggestions. `None` disables the recency trigger.
    #[serde(default = "default_review_recency_days")]
    pub recency_days: Option<u64>,
}

/// Default recency window before an untouched active memory is suggested for
/// review. 90 days matches the default telemetry retention window and is long
/// enough that routinely-referenced memories (which bump `updated_at` on every
/// edit/resolve) never trip it.
fn default_review_recency_days() -> Option<u64> {
    Some(90)
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            recency_days: default_review_recency_days(),
        }
    }
}

impl ReviewConfig {
    /// Validate the recency window bounds (mirrors `stats.retention_days`).
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        if let Some(days) = self.recency_days {
            if days == 0 {
                anyhow::bail!(
                    "review.recency_days must be >= 1 (0 is ambiguous). Set a positive \
                     number of days, or omit the field to use the default (90)."
                );
            }
            if days > 3650 {
                anyhow::bail!("review.recency_days ({}) must be <= 3650", days);
            }
        }
        Ok(())
    }
}

/// Retrieval configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfig {
    /// Minimum relevance score threshold
    pub relevance_threshold: f64,

    /// Maximum number of results to return
    pub max_results: usize,

    /// Whether to include expired memories
    pub include_expired: bool,

    /// Scoring configuration
    pub scoring: ScoringConfig,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            relevance_threshold: 0.45,
            max_results: 10,
            include_expired: false,
            scoring: ScoringConfig::default(),
        }
    }
}

/// Embedding transport backend preference.
///
/// Controls whether the local ONNX runtime (fastembed) or an Ollama server is
/// used to run the embedding model.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingBackend {
    /// Try ONNX first, fall back to Ollama (default).
    #[default]
    Auto,
    /// Only use local ONNX runtime via fastembed.
    Onnx,
    /// Only use an Ollama server.
    Ollama,
    /// Only use the pure-Rust `tract` engine (fp32 MiniLM). No native ONNX
    /// Runtime — the fallback backend for platforms with no prebuilt `ort`
    /// (notably Intel Mac, `x86_64-apple-darwin`). ~3× slower than ONNX;
    /// selected automatically on those targets, or explicitly here.
    Tract,
}

impl std::fmt::Display for EmbeddingBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Onnx => write!(f, "onnx"),
            Self::Ollama => write!(f, "ollama"),
            Self::Tract => write!(f, "tract"),
        }
    }
}

impl std::str::FromStr for EmbeddingBackend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "onnx" => Ok(Self::Onnx),
            "ollama" => Ok(Self::Ollama),
            "tract" => Ok(Self::Tract),
            other => Err(format!(
                "unknown embedding backend '{}': expected auto, onnx, ollama, or tract",
                other
            )),
        }
    }
}

/// Policy when the store's stored embedding model differs from the one in
/// use (e.g. after an upgrade that changes the default embedding model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReindexOnModelChange {
    /// Don't detect or act (legacy silent behavior; accept mixed vectors).
    Off,
    /// Surface a warning on MCP connect / daemon startup / `doctor`;
    /// keep serving (mildly degraded) — the agent prompts the user to
    /// `engramdb reindex --embeddings-only`.
    #[default]
    Warn,
    /// Surface, and automatically reindex at daemon startup before serving.
    Auto,
    /// Hard-error embedding-dependent operations until the store is
    /// reindexed (strict; guarantees no degraded search).
    Error,
}

/// Embeddings provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingsConfig {
    /// Transport backend: "auto" (default), "onnx", or "ollama".
    #[serde(default)]
    pub backend: EmbeddingBackend,
    /// Embedding model name.
    /// Supported: "all-minilm" (default, 384d), "nomic-embed-text" (768d), "mxbai-embed-large" (1024d).
    /// "onnx" is a backward-compat alias for "all-minilm".
    pub provider: String,
    /// Embedding vector dimensionality (384 for MiniLM, 768 for nomic, etc.)
    pub dimensions: usize,
    /// Maximum input tokens before truncation (256 for MiniLM)
    pub max_tokens: usize,
    /// What to do when the store's embeddings were produced by a
    /// different model than the one now in use (default: warn).
    #[serde(default)]
    pub reindex_on_model_change: ReindexOnModelChange,

    /// Number of independent embedding model sessions to load and
    /// round-robin across.
    ///
    /// One session serializes all inference behind its mutex, so under
    /// concurrent load (the shared daemon serving N agent sessions)
    /// throughput is mutex-bound. `None` (the default) auto-sizes via
    /// [`EmbeddingsConfig::resolved_pool_size`] to `cores/2` for long-lived
    /// multi-tenant contexts (daemon / MCP server); one-shot CLI runs pass
    /// `1` explicitly regardless. `Some(1)` forces a single session;
    /// `Some(n)` pins the pool to `n`. Embedding uses fastembed's own
    /// internal threadpool, so the `pool_size × intra_threads ≤ cores`
    /// constraint that bounds the NLI/T5 sessions does not apply here.
    #[serde(default)]
    pub pool_size: Option<usize>,
}

impl EmbeddingsConfig {
    /// Resolve the configured embedding pool size for a machine with
    /// `cores` logical CPUs: the configured value (only floored at 1 so it
    /// can never disable embeddings), else auto `cores/2` (also ≥ 1).
    ///
    /// `cores` is passed in (not read from the machine here) so the policy
    /// is deterministically unit-testable. Callers in one-shot contexts
    /// (the CLI) bypass this and pass `1` directly — auto-sizing is for the
    /// long-lived multi-tenant daemon / MCP server, where extra sessions
    /// pay back across many concurrent callers.
    pub fn resolved_pool_size(&self, cores: usize) -> usize {
        self.pool_size.unwrap_or((cores / 2).max(1)).max(1)
    }
}

/// Logical CPU count for auto-sizing pools, or `1` if it can't be queried.
/// The single place the machine is consulted, kept out of the pure
/// `resolved_pool_size` policy functions so those stay testable.
pub fn available_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            backend: EmbeddingBackend::default(),
            provider: "onnx".to_string(),
            dimensions: 384,
            max_tokens: 256,
            reindex_on_model_change: ReindexOnModelChange::default(),
            pool_size: None,
        }
    }
}

/// NLI contradiction detection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NliConfig {
    /// Whether NLI contradiction detection is enabled
    pub enabled: bool,

    /// HuggingFace model repository ID
    pub model: String,

    /// Minimum contradiction probability to trigger a challenge (0.0-1.0)
    pub contradiction_threshold: f64,

    /// Maximum number of similar memories to compare against
    pub max_comparisons: usize,

    /// Minimum cosine similarity to consider a memory as a candidate for NLI check
    pub similarity_threshold: f64,
}

impl NliConfig {
    /// Validate that NLI configuration values are within acceptable ranges.
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        if !(0.0..=1.0).contains(&self.contradiction_threshold) {
            anyhow::bail!(
                "nli.contradiction_threshold ({}) must be in [0.0, 1.0]",
                self.contradiction_threshold
            );
        }
        if !(0.0..=1.0).contains(&self.similarity_threshold) {
            anyhow::bail!(
                "nli.similarity_threshold ({}) must be in [0.0, 1.0]",
                self.similarity_threshold
            );
        }
        if self.max_comparisons == 0 {
            anyhow::bail!("nli.max_comparisons must be > 0");
        }
        Ok(())
    }
}

impl Default for NliConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            // Single source of truth: derive from `DEFAULT_NLI_MODEL_REPO`
            // (asserted equal to `nli::DEFAULT_NLI_MODEL.repo` by a test in the
            // nli module) rather than a hand-copied literal, so the default can
            // never drift from the model the NLI loader actually selects
            // (int8-quantized mirror: ~2× faster, ~3.7× less RAM, identical
            // id2label). Custom repos keep the fp32 defaults.
            model: DEFAULT_NLI_MODEL_REPO.to_string(),
            contradiction_threshold: 0.7,
            max_comparisons: 10,
            similarity_threshold: 0.3,
        }
    }
}

/// Configuration for cross-encoder reranking.
///
/// When enabled, a cross-encoder model jointly scores query+document pairs
/// to refine the initial bi-encoder retrieval ranking. This is slower but
/// more accurate for nuanced relevance judgments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankConfig {
    /// Whether reranking is enabled (default: false)
    pub enabled: bool,

    /// Reranker model name (default: "bge-reranker-base").
    /// Supported: "bge-reranker-base", "bge-reranker-v2-m3",
    /// "jina-reranker-v1-turbo-en", "jina-reranker-v2-base-multilingual".
    pub model: String,

    /// Number of top candidates to rerank (default: 50).
    /// Only the top N results from initial retrieval are passed to the
    /// cross-encoder. Higher values improve quality but are slower.
    pub top_n: usize,

    /// Blend weight for rerank score vs original score (default: 0.5).
    /// 0.0 = ignore rerank (original scores only),
    /// 1.0 = use only rerank score.
    /// Formula: blended = (1 - weight) * original + weight * rerank
    pub weight: f64,
}

impl RerankConfig {
    /// Validate that rerank configuration values are within acceptable ranges.
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        if !(0.0..=1.0).contains(&self.weight) {
            anyhow::bail!("rerank.weight ({}) must be in [0.0, 1.0]", self.weight);
        }
        if self.top_n == 0 {
            anyhow::bail!("rerank.top_n must be > 0");
        }
        Ok(())
    }
}

impl Default for RerankConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "bge-reranker-base".to_string(),
            top_n: 50,
            weight: 0.5,
        }
    }
}

/// Automatic title-generation configuration.
///
/// `strategy` selects how a memory's title is derived when the caller
/// doesn't supply one explicitly:
/// - `keyword` (default): RAKE keyword extraction — in-process, no model,
///   negligible cost; never cached/pooled.
/// - `t5`: abstractive T5-small summarization. The model session is
///   expensive (encoder + decoder ONNX init), so when this is configured
///   the daemon / MCP server loads it **once** into the provider bundle
///   (and pools it) instead of rebuilding it on every `create`.
/// - `none`: no automatic title.
///
/// This is the deployment default; the MCP `create` tool's per-call
/// `title_strategy` still overrides it.
///
/// Defaults to `t5`: the shared daemon loads (and pools) the
/// encoder+decoder **once machine-wide**, so the historical per-`create`
/// cost that made keyword the default no longer applies. The one-shot CLI
/// is unaffected — `engramdb add` uses [`TitleStrategy::default`]
/// (`keyword`) so a single command never pays a cold T5 load.
fn default_title_strategy() -> TitleStrategy {
    TitleStrategy::T5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleConfig {
    /// Title-generation strategy. Default `t5` (daemon-amortized; see
    /// [`default_title_strategy`]).
    #[serde(default = "default_title_strategy")]
    pub strategy: TitleStrategy,

    /// Number of independent T5 sessions to pool when `strategy = "t5"`.
    /// `None` (default) auto-sizes to 2 — the bench-optimal for the heavy
    /// encoder+decoder pair — clamped to the core count, and only in
    /// long-lived multi-tenant contexts (the one-shot CLI always uses 1).
    /// Unlike embedding, T5 sessions are direct ORT sessions with an
    /// explicit `intra_threads`, so the builder reduces each member's
    /// `intra_threads` to keep `pool_size × intra_threads ≤ cores`.
    #[serde(default)]
    pub pool_size: Option<usize>,
}

impl Default for TitleConfig {
    fn default() -> Self {
        Self {
            strategy: default_title_strategy(),
            pool_size: None,
        }
    }
}

impl TitleConfig {
    /// Resolve the T5 title pool size: configured value, else auto `2`
    /// (bench-optimal), never exceeding the core count. Paired with a
    /// reduced per-session `intra_threads` by the caller so the pool does
    /// not oversubscribe the CPU.
    pub fn resolved_pool_size(&self, cores: usize) -> usize {
        self.pool_size.unwrap_or(2).clamp(1, cores.max(1))
    }
}

/// Top-level EngramDB configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngramConfig {
    /// Retrieval settings
    #[serde(default)]
    pub retrieval: RetrievalConfig,

    /// Search-specific settings
    #[serde(default)]
    pub search: SearchConfig,

    /// Embeddings provider settings
    #[serde(default)]
    pub embeddings: EmbeddingsConfig,

    /// Physical scope proximity bonuses
    #[serde(default)]
    pub scope_proximity: ScopeProximityConfig,

    /// Logical scope bonuses
    #[serde(default)]
    pub logical_bonus: LogicalBonusConfig,

    /// Trust weights by source
    #[serde(default)]
    pub trust_weights: TrustWeights,

    /// Various thresholds
    #[serde(default)]
    pub thresholds: ThresholdsConfig,

    /// Recency-based review-suggestion settings
    #[serde(default)]
    pub review: ReviewConfig,

    /// NLI contradiction detection settings
    #[serde(default)]
    pub nli: NliConfig,

    /// Cross-encoder reranking settings
    #[serde(default)]
    pub rerank: RerankConfig,

    /// Runtime telemetry / statistics collection settings
    #[serde(default)]
    pub stats: StatsConfig,

    /// Shared embedding daemon settings
    #[serde(default)]
    pub daemon: DaemonConfig,

    /// Automatic title-generation settings
    #[serde(default)]
    pub title: TitleConfig,

    /// Automatic main-worktree maintenance settings
    #[serde(default)]
    pub maintenance: MaintenanceConfig,

    /// Security / access-control settings
    #[serde(default)]
    pub security: SecurityConfig,
}

/// Shared embedding-daemon settings.
///
/// stdio MCP is one process per agent session, so without a daemon every
/// concurrent session loads its own copy of the embedding (and optional
/// NLI / reranker) models. When `enabled`, MCP processes delegate all model
/// inference to a single long-lived daemon over a Unix domain socket, so the
/// models load exactly once machine-wide. When disabled — or when the daemon
/// is unreachable — each process falls back to loading the models in-process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonConfig {
    /// Master switch. When `true` (default) MCP delegates embedding / NLI /
    /// rerank to the shared daemon (auto-spawning it if needed). When `false`
    /// the daemon is never contacted and models load in-process.
    #[serde(default = "default_daemon_enabled")]
    pub enabled: bool,

    /// Seconds the daemon stays alive with no active connections before
    /// exiting. A fresh daemon is auto-spawned on demand by the next MCP
    /// process, so a low value just trades a one-time respawn for not keeping
    /// idle model memory resident.
    #[serde(default = "default_daemon_idle_timeout_secs")]
    pub idle_timeout_secs: u64,

    /// Override the Unix socket path. When unset the per-user default is
    /// used. Resolution precedence (highest first): an explicit `--socket`
    /// CLI flag, the `ENGRAMDB_DAEMON_SOCKET` env var, this config value,
    /// then the default per-user runtime path.
    #[serde(default)]
    pub socket_path: Option<String>,

    /// Whether CLI model-needing ops route through the daemon when one is
    /// reachable. Default `true`. When `false` (or when `enabled = false`),
    /// the CLI always loads models in-process. Override ladder:
    /// `--in-process` flag > `ENGRAMDB_IN_PROCESS` env > this config value.
    #[serde(default = "default_daemon_use_for_cli")]
    pub use_for_cli: bool,
}

fn default_daemon_enabled() -> bool {
    true
}

fn default_daemon_idle_timeout_secs() -> u64 {
    900
}

fn default_daemon_use_for_cli() -> bool {
    true
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            enabled: default_daemon_enabled(),
            idle_timeout_secs: default_daemon_idle_timeout_secs(),
            socket_path: None,
            use_for_cli: default_daemon_use_for_cli(),
        }
    }
}

impl DaemonConfig {
    /// Validate that daemon configuration values are within acceptable ranges.
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        if self.idle_timeout_secs < 60 {
            anyhow::bail!(
                "daemon.idle_timeout_secs must be >= 60 (got {}): the MCP heartbeat \
                 pings at most every 30s, so a smaller idle timeout makes the daemon \
                 idle-reap and respawn between pings",
                self.idle_timeout_secs
            );
        }
        Ok(())
    }
}

/// Automatic main-worktree maintenance settings.
///
/// When a memory operation is invoked directly on a project's *main* worktree
/// (not a linked git worktree), both front-ends run a throttled, best-effort
/// housekeeping pass: orphan/stale-project cleanup plus a quick store health
/// check. A timestamp marker under the global data dir enforces the throttle
/// across processes and sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceConfig {
    /// Master switch. When `true` (default) the housekeeping pass runs
    /// (throttled) on main-worktree invocations. When `false` it never runs.
    /// Override ladder: `--no-maintenance` CLI flag >
    /// `ENGRAMDB_DISABLE_AUTO_MAINTENANCE` env > this config value.
    #[serde(default = "default_maintenance_enabled")]
    pub enabled: bool,

    /// Minimum seconds between automatic maintenance passes. Override:
    /// `ENGRAMDB_AUTO_MAINTENANCE_INTERVAL_SECS` env > this config value.
    #[serde(default = "default_maintenance_interval_secs")]
    pub interval_secs: u64,
}

fn default_maintenance_enabled() -> bool {
    true
}

fn default_maintenance_interval_secs() -> u64 {
    6 * 60 * 60
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        Self {
            enabled: default_maintenance_enabled(),
            interval_secs: default_maintenance_interval_secs(),
        }
    }
}

/// Security / access-control settings.
///
/// Guards the confused-deputy surface of the MCP server: nearly every tool
/// accepts an optional `project` override that `resolve_dir` resolves to *any*
/// project registered in the global registry — not just the session's own
/// project. An MCP agent operating in project A, if steered (e.g. by injected
/// memory content) with project B's path/id, could otherwise mutate a
/// different registered project B on the same machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Whether MCP mutating tools may write to a *different* registered
    /// project than the session's own. Default `true` (preserves historical
    /// behavior — cross-project writes allowed). When `false`, MCP mutating
    /// tools (`create`, `update`, `delete`, `challenge`, `resolve`,
    /// `compress_apply`, `gc`, `reindex`) are rejected when their `project`
    /// override resolves to a project id other than the session's own. The
    /// session's own project (`project` omitted) and the shared global store
    /// (`project = "global"`) are always allowed.
    #[serde(default = "default_allow_cross_project_writes")]
    pub allow_cross_project_writes: bool,
}

fn default_allow_cross_project_writes() -> bool {
    true
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            allow_cross_project_writes: default_allow_cross_project_writes(),
        }
    }
}

impl SecurityConfig {
    /// Validate security configuration. There are no numeric bounds, so this
    /// is trivially `Ok`; it exists to mirror the other sections' `validate`
    /// pattern and give `EngramConfig::validate` a uniform call site.
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        Ok(())
    }
}

/// Runtime telemetry / statistics collection settings.
///
/// Controls the in-memory `StatsCollector` that tracks tool usage, query
/// outcomes, and stage timings. Counters are project-scoped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsConfig {
    /// Master switch. When false, the collector is a no-op and the
    /// `stats` tool/command falls back to static store counts only.
    #[serde(default = "default_stats_enabled")]
    pub enabled: bool,

    /// Per-stage / per-tool histogram capacity. Percentiles are computed
    /// over the most recent N samples (recency-weighted, not lifetime).
    #[serde(default = "default_histogram_capacity")]
    pub histogram_capacity: usize,

    /// Retention window for the on-disk LanceDB event log, in days.
    /// Defaults to 90 so the per-project `stats_events` table cannot grow
    /// without bound; the persistence flush task (and `gc`) prune events
    /// older than this. `None` (only reachable programmatically — TOML has
    /// no null) means unlimited retention; to effectively retain forever
    /// from config, set the maximum of 3650 (10 years). A value of 0 is
    /// rejected by validation: it is ambiguous between "prune everything"
    /// and the legacy "retain forever" meaning.
    /// Note: lifetime counters become "since the oldest non-pruned event".
    #[serde(default = "default_retention_days")]
    pub retention_days: Option<u64>,

    /// Flush task interval in seconds. The persistence task drains
    /// buffered events and appends them to the per-project `stats_events`
    /// LanceDB table this often, plus on shutdown.
    #[serde(default = "default_flush_interval_secs")]
    pub flush_interval_secs: u64,

    /// A query is counted as a "followup" if it arrives within this many
    /// seconds of a previous query in the same session. Default 60.
    #[serde(default = "default_followup_window_secs")]
    pub followup_window_secs: u64,

    /// Soft cap on the number of distinct sessions tracked per project.
    /// When exceeded, the oldest sessions (by `last_query_at`) are
    /// evicted. Bounds memory growth on long-running daemons that see
    /// many sessions. Default 10_000.
    #[serde(default = "default_max_sessions_per_project")]
    pub max_sessions_per_project: usize,
}

fn default_stats_enabled() -> bool {
    true
}
fn default_histogram_capacity() -> usize {
    256
}
fn default_flush_interval_secs() -> u64 {
    60
}
/// 90 days of telemetry by default — finite so the event log cannot grow
/// monotonically on long-lived projects whose users never touch `[stats]`.
fn default_retention_days() -> Option<u64> {
    Some(90)
}
fn default_followup_window_secs() -> u64 {
    60
}
fn default_max_sessions_per_project() -> usize {
    10_000
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            enabled: default_stats_enabled(),
            histogram_capacity: default_histogram_capacity(),
            retention_days: default_retention_days(),
            flush_interval_secs: default_flush_interval_secs(),
            followup_window_secs: default_followup_window_secs(),
            max_sessions_per_project: default_max_sessions_per_project(),
        }
    }
}

impl StatsConfig {
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        if self.histogram_capacity == 0 {
            anyhow::bail!("stats.histogram_capacity must be > 0");
        }
        if self.flush_interval_secs == 0 {
            anyhow::bail!("stats.flush_interval_secs must be >= 1");
        }
        if self.followup_window_secs == 0 {
            anyhow::bail!("stats.followup_window_secs must be >= 1");
        }
        if self.max_sessions_per_project == 0 {
            anyhow::bail!("stats.max_sessions_per_project must be >= 1");
        }
        if let Some(days) = self.retention_days {
            if days == 0 {
                anyhow::bail!(
                    "stats.retention_days must be >= 1 (0 is ambiguous). \
                     Set a positive number of days to prune older events, \
                     or 3650 (the maximum, 10 years) to effectively retain forever."
                );
            }
            if days > 3650 {
                anyhow::bail!("stats.retention_days ({}) must be <= 3650", days);
            }
        }
        Ok(())
    }
}

impl EngramConfig {
    /// Validate all configuration subsections.
    pub fn validate(&self) -> Result<(), anyhow::Error> {
        self.retrieval.scoring.with_query.validate("with_query")?;
        self.retrieval
            .scoring
            .with_keyword
            .validate("with_keyword")?;
        self.retrieval.scoring.scope_only.validate("scope_only")?;
        self.retrieval.scoring.degraded.validate("degraded")?;
        self.trust_weights.validate()?;
        self.nli.validate()?;
        self.rerank.validate()?;
        self.stats.validate()?;
        self.daemon.validate()?;
        self.security.validate()?;
        self.review.validate()?;

        if !(0.0..=1.0).contains(&self.retrieval.scoring.scope_multiplier_floor) {
            anyhow::bail!("scoring.scope_multiplier_floor must be in [0.0, 1.0]");
        }
        if !(0.0..=1.0).contains(&self.retrieval.scoring.trust_multiplier_floor) {
            anyhow::bail!("scoring.trust_multiplier_floor must be in [0.0, 1.0]");
        }
        if !(0.0..=1.0).contains(&self.retrieval.scoring.challenge_penalty) {
            anyhow::bail!("scoring.challenge_penalty must be in [0.0, 1.0]");
        }
        if !(0.0..=1.0).contains(&self.retrieval.scoring.depth_decay_base) {
            anyhow::bail!("scoring.depth_decay_base must be in [0.0, 1.0]");
        }
        if !(0.0..=1.0).contains(&self.retrieval.scoring.depth_decay_floor) {
            anyhow::bail!("scoring.depth_decay_floor must be in [0.0, 1.0]");
        }

        if self.embeddings.dimensions == 0 || self.embeddings.dimensions > 4096 {
            anyhow::bail!(
                "embeddings.dimensions ({}) must be in (0, 4096]",
                self.embeddings.dimensions
            );
        }

        if self.retrieval.max_results == 0 {
            anyhow::bail!("retrieval.max_results must be > 0");
        }

        if !(0.0..=1.0).contains(&self.thresholds.gc) {
            anyhow::bail!(
                "thresholds.gc ({}) must be in [0.0, 1.0]",
                self.thresholds.gc
            );
        }
        if !(0.0..=1.0).contains(&self.thresholds.compress) {
            anyhow::bail!(
                "thresholds.compress ({}) must be in [0.0, 1.0]",
                self.thresholds.compress
            );
        }

        // A negative `search.threshold` is always a configuration error: scores
        // are normalized to [0, 1], so a negative gate is meaningless and — worse
        // — the consumer's old `min(1.0)` left it negative, which silently
        // *disabled* the relevance filter (the gate is `if threshold > 0.0`).
        // Reject it here, and the consumer (`RetrievalEngine`) additionally
        // clamps defensively for configs built programmatically without
        // `validate()`. NaN is likewise rejected (it fails every comparison).
        if self.search.threshold.is_nan() || self.search.threshold < 0.0 {
            anyhow::bail!(
                "search.threshold ({}) must be >= 0.0",
                self.search.threshold
            );
        }

        // Config migration: if search threshold is > 1.0, it was set for the
        // old unbounded scoring scale. Warn and treat as if it were 1.0. This is
        // tolerated (not an error) for backward-compatibility with old configs;
        // the consumer clamps with `min(1.0)`, so it behaves exactly as 1.0.
        if self.search.threshold > 1.0 {
            tracing::warn!(
                "search.threshold ({}) exceeds 1.0 — scores are now normalized to [0, 1]. \
                 Clamping to 1.0. Please update your config.",
                self.search.threshold
            );
        }

        Ok(())
    }

    /// Load configuration from a TOML file
    pub fn from_toml_file(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    /// Save configuration to a TOML file
    pub fn to_toml_file(
        &self,
        path: impl AsRef<std::path::Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let toml_string = toml::to_string_pretty(self)?;
        std::fs::write(path, toml_string)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `EmbeddingBackend` variant round-trips through `Display`/`FromStr`
    /// (case-insensitively), and an unknown string errors. Locks the `tract`
    /// variant added for the Intel-Mac pure-Rust backend.
    #[test]
    fn embedding_backend_roundtrips() {
        use std::str::FromStr;
        for (variant, name) in [
            (EmbeddingBackend::Auto, "auto"),
            (EmbeddingBackend::Onnx, "onnx"),
            (EmbeddingBackend::Ollama, "ollama"),
            (EmbeddingBackend::Tract, "tract"),
        ] {
            assert_eq!(variant.to_string(), name);
            assert_eq!(EmbeddingBackend::from_str(name).unwrap(), variant);
            assert_eq!(
                EmbeddingBackend::from_str(&name.to_uppercase()).unwrap(),
                variant
            );
        }
        assert!(EmbeddingBackend::from_str("nonsense").is_err());
    }

    /// Guards the single-source-of-truth: `NliConfig::default().model` is
    /// derived from [`DEFAULT_NLI_MODEL_REPO`], never a hand-copied literal.
    /// The complementary half — that `nli::DEFAULT_NLI_MODEL.repo` equals
    /// `DEFAULT_NLI_MODEL_REPO` — is asserted by a test in the `nli` module,
    /// so neither the config default nor the loader spec can silently drift.
    #[test]
    fn nli_default_model_tracks_default_nli_model_repo() {
        assert_eq!(NliConfig::default().model.as_str(), DEFAULT_NLI_MODEL_REPO);
    }

    #[test]
    fn reindex_on_model_change_serde_and_backward_compat() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct W {
            v: ReindexOnModelChange,
        }
        // Every variant round-trips by its lowercase name.
        for (variant, name) in [
            (ReindexOnModelChange::Off, "off"),
            (ReindexOnModelChange::Warn, "warn"),
            (ReindexOnModelChange::Auto, "auto"),
            (ReindexOnModelChange::Error, "error"),
        ] {
            let toml_str = toml::to_string(&W { v: variant }).unwrap();
            assert!(
                toml_str.contains(&format!("v = \"{name}\"")),
                "{variant:?} must serialize as {name:?}; got {toml_str:?}"
            );
            let back: W = toml::from_str(&format!("v = \"{name}\"\n")).unwrap();
            assert_eq!(back.v, variant);
        }

        // Backward-compat: an `[embeddings]` table written before this field
        // existed must still parse, defaulting to `Warn` — otherwise every
        // pre-lifecycle deployment fails to start after upgrade.
        let legacy: EmbeddingsConfig =
            toml::from_str("provider = \"onnx\"\ndimensions = 384\nmax_tokens = 256\n").unwrap();
        assert_eq!(legacy.reindex_on_model_change, ReindexOnModelChange::Warn);

        // And the in-code defaults agree.
        assert_eq!(
            EmbeddingsConfig::default().reindex_on_model_change,
            ReindexOnModelChange::Warn
        );
        assert_eq!(
            EngramConfig::default().embeddings.reindex_on_model_change,
            ReindexOnModelChange::Warn
        );
    }

    #[test]
    fn test_config_defaults() {
        let config = EngramConfig::default();

        // Retrieval config
        assert_eq!(config.retrieval.max_results, 10);
        assert_eq!(config.retrieval.relevance_threshold, 0.45);
        assert!(!config.retrieval.include_expired);

        // Trust weights
        assert_eq!(config.trust_weights.human, 1.0);
        assert_eq!(config.trust_weights.agent, 0.85);
        assert_eq!(config.trust_weights.inferred, 0.6);
        assert_eq!(config.trust_weights.imported, 0.7);

        // Scope proximity
        assert_eq!(config.scope_proximity.exact_file, 1.0);
        assert_eq!(config.scope_proximity.same_directory, 0.85);
        assert_eq!(config.scope_proximity.same_module, 0.6);
        assert_eq!(config.scope_proximity.project_root, 0.4);

        // Logical bonus
        assert_eq!(config.logical_bonus.exact, 0.3);
        assert_eq!(config.logical_bonus.parent, 0.2);
        assert_eq!(config.logical_bonus.sibling, 0.15);

        // Thresholds
        assert_eq!(config.thresholds.needs_review, 0.3);
        assert_eq!(config.thresholds.gc, 0.05);
        assert_eq!(config.thresholds.compress, 0.4);

        // Review recency trigger
        assert_eq!(config.review.recency_days, Some(90));

        // Scoring weights - with_query
        assert_eq!(config.retrieval.scoring.with_query.semantic, Some(0.55));
        assert_eq!(config.retrieval.scoring.with_query.relevance, 0.45);

        // Scoring weights - scope_only
        assert_eq!(config.retrieval.scoring.scope_only.semantic, None);
        assert_eq!(config.retrieval.scoring.scope_only.relevance, 1.0);

        // Scoring weights - degraded
        assert_eq!(config.retrieval.scoring.degraded.semantic, None);
        assert_eq!(config.retrieval.scoring.degraded.relevance, 1.0);

        // Scope multiplier floor
        assert_eq!(config.retrieval.scoring.scope_multiplier_floor, 0.5);

        // Trust multiplier floor
        assert_eq!(config.retrieval.scoring.trust_multiplier_floor, 0.5);

        // Challenge penalty
        assert_eq!(config.retrieval.scoring.challenge_penalty, 0.10);

        // Depth decay
        assert_eq!(config.retrieval.scoring.depth_decay_base, 0.82);
        assert_eq!(config.retrieval.scoring.depth_decay_floor, 0.3);

        // Search config
        assert_eq!(config.search.semantic_weight, 3.0);
        assert_eq!(config.search.threshold, 0.2);

        // Scoring weights - with_keyword
        assert_eq!(config.retrieval.scoring.with_keyword.keyword, Some(0.45));
        assert_eq!(config.retrieval.scoring.with_keyword.semantic, Some(0.30));
        assert_eq!(config.retrieval.scoring.with_keyword.relevance, 0.25);

        // Embeddings config
        assert_eq!(config.embeddings.provider, "onnx");
        assert_eq!(config.embeddings.dimensions, 384);
        assert_eq!(config.embeddings.max_tokens, 256);

        // NLI config
        assert!(!config.nli.enabled);
        assert_eq!(config.nli.model, "Xenova/nli-deberta-v3-xsmall");
        assert_eq!(config.nli.contradiction_threshold, 0.7);
        assert_eq!(config.nli.max_comparisons, 10);
        assert_eq!(config.nli.similarity_threshold, 0.3);
    }

    #[test]
    fn test_config_toml_roundtrip() {
        let original = EngramConfig::default();

        // Create a temporary file
        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join("test_config_roundtrip.toml");

        // Save to file
        original.to_toml_file(&temp_path).unwrap();

        // Load from file
        let loaded = EngramConfig::from_toml_file(&temp_path).unwrap();

        // Clean up
        std::fs::remove_file(&temp_path).ok();

        // Verify all values match
        assert_eq!(loaded.retrieval.max_results, original.retrieval.max_results);
        assert_eq!(loaded.trust_weights.human, original.trust_weights.human);
        assert_eq!(
            loaded.scope_proximity.exact_file,
            original.scope_proximity.exact_file
        );
        assert_eq!(loaded.logical_bonus.exact, original.logical_bonus.exact);
        assert_eq!(
            loaded.thresholds.needs_review,
            original.thresholds.needs_review
        );
        assert_eq!(
            loaded.retrieval.scoring.scope_multiplier_floor,
            original.retrieval.scoring.scope_multiplier_floor
        );
        assert_eq!(
            loaded.retrieval.scoring.trust_multiplier_floor,
            original.retrieval.scoring.trust_multiplier_floor
        );
        assert_eq!(
            loaded.retrieval.scoring.challenge_penalty,
            original.retrieval.scoring.challenge_penalty
        );
    }

    #[test]
    fn test_config_partial_toml() {
        // Create TOML with only [retrieval] section (minimal fields)
        // Note: scoring field has default in the struct, so it should use defaults if not provided
        let partial_toml = r#"
[retrieval]
max_results = 50
relevance_threshold = 0.7
include_expired = true

[retrieval.scoring.with_query]
semantic = 0.55
relevance = 0.45

[retrieval.scoring.with_keyword]
keyword = 0.45
semantic = 0.30
relevance = 0.25

[retrieval.scoring.scope_only]
relevance = 1.0

[retrieval.scoring.degraded]
relevance = 1.0
"#;

        let config: EngramConfig = toml::from_str(partial_toml).unwrap();

        // Verify retrieval section was parsed
        assert_eq!(config.retrieval.max_results, 50);
        assert_eq!(config.retrieval.relevance_threshold, 0.7);
        assert!(config.retrieval.include_expired);

        // Verify other sections use defaults (thanks to #[serde(default)])
        assert_eq!(config.trust_weights.human, 1.0);
        assert_eq!(config.scope_proximity.exact_file, 1.0);
        assert_eq!(config.logical_bonus.exact, 0.3);
        assert_eq!(config.thresholds.needs_review, 0.3);
    }

    #[test]
    fn test_maintenance_config_defaults() {
        // Default: enabled with a 6-hour throttle window.
        let config = EngramConfig::default();
        assert!(config.maintenance.enabled);
        assert_eq!(config.maintenance.interval_secs, 6 * 60 * 60);

        // Empty TOML falls back to the same defaults via #[serde(default)].
        let from_empty: EngramConfig = toml::from_str("").unwrap();
        assert!(from_empty.maintenance.enabled);
        assert_eq!(from_empty.maintenance.interval_secs, 6 * 60 * 60);
    }

    #[test]
    fn test_maintenance_config_custom_toml() {
        let toml = r#"
[maintenance]
enabled = false
interval_secs = 60
"#;
        let config: EngramConfig = toml::from_str(toml).unwrap();
        assert!(!config.maintenance.enabled);
        assert_eq!(config.maintenance.interval_secs, 60);
    }

    #[test]
    fn test_security_config_defaults() {
        // Default: cross-project writes allowed (preserves historical behavior).
        let config = EngramConfig::default();
        assert!(config.security.allow_cross_project_writes);
        assert!(SecurityConfig::default().allow_cross_project_writes);

        // A config.toml with NO [security] section parses to the default true.
        let from_empty: EngramConfig = toml::from_str("").unwrap();
        assert!(from_empty.security.allow_cross_project_writes);
    }

    #[test]
    fn test_security_config_partial_toml_defaults_field() {
        // A partial [security] section (present but empty) still defaults the
        // field to true via the field-level `#[serde(default = "...")]`.
        let config: EngramConfig = toml::from_str("[security]\n").unwrap();
        assert!(config.security.allow_cross_project_writes);
    }

    #[test]
    fn test_security_config_custom_toml() {
        let toml = r#"
[security]
allow_cross_project_writes = false
"#;
        let config: EngramConfig = toml::from_str(toml).unwrap();
        assert!(!config.security.allow_cross_project_writes);
        // Validation is trivially Ok regardless of the flag.
        config.validate().unwrap();
    }

    #[test]
    fn test_security_config_toml_roundtrip() {
        let mut config = EngramConfig::default();
        config.security.allow_cross_project_writes = false;

        let temp_path = std::env::temp_dir().join("test_security_config_roundtrip.toml");
        config.to_toml_file(&temp_path).unwrap();
        let loaded = EngramConfig::from_toml_file(&temp_path).unwrap();
        std::fs::remove_file(&temp_path).ok();

        assert!(!loaded.security.allow_cross_project_writes);
    }

    #[test]
    fn test_config_scoring_weights_sum_to_one() {
        let config = EngramConfig::default();

        // with_query: semantic + relevance sum to 1.0
        // (trust and scope are multipliers, not weighted components)
        let wq = &config.retrieval.scoring.with_query;
        let wq_sum = wq.semantic.unwrap_or(0.0) + wq.relevance;
        assert!(
            (wq_sum - 1.0).abs() < f64::EPSILON,
            "with_query weights sum to {}, expected 1.0",
            wq_sum
        );

        // scope_only: relevance sums to 1.0
        let so = &config.retrieval.scoring.scope_only;
        let so_sum = so.semantic.unwrap_or(0.0) + so.relevance;
        assert!(
            (so_sum - 1.0).abs() < f64::EPSILON,
            "scope_only weights sum to {}, expected 1.0",
            so_sum
        );

        // degraded: relevance sums to 1.0
        let dg = &config.retrieval.scoring.degraded;
        let dg_sum = dg.semantic.unwrap_or(0.0) + dg.relevance;
        assert!(
            (dg_sum - 1.0).abs() < f64::EPSILON,
            "degraded weights sum to {}, expected 1.0",
            dg_sum
        );
    }

    #[test]
    fn test_search_config_custom_toml() {
        let toml = r#"
[search]
semantic_weight = 5.0
threshold = 0.25
"#;
        let config: EngramConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.search.semantic_weight, 5.0);
        assert_eq!(config.search.threshold, 0.25);
    }

    #[test]
    fn test_search_config_defaults_when_omitted() {
        // Empty TOML: all sections use #[serde(default)] on EngramConfig
        let config: EngramConfig = toml::from_str("").unwrap();
        assert_eq!(config.search.semantic_weight, 3.0);
        assert_eq!(config.search.threshold, 0.2);
    }

    #[test]
    fn test_search_config_roundtrip() {
        let mut config = EngramConfig::default();
        config.search.semantic_weight = 7.5;
        config.search.threshold = 0.1;

        let temp_path = std::env::temp_dir().join("test_search_config_roundtrip.toml");
        config.to_toml_file(&temp_path).unwrap();
        let loaded = EngramConfig::from_toml_file(&temp_path).unwrap();
        std::fs::remove_file(&temp_path).ok();

        assert_eq!(loaded.search.semantic_weight, 7.5);
        assert_eq!(loaded.search.threshold, 0.1);
    }

    #[test]
    fn test_nli_config_defaults() {
        let config = NliConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.model, "Xenova/nli-deberta-v3-xsmall");
        assert_eq!(config.contradiction_threshold, 0.7);
        assert_eq!(config.max_comparisons, 10);
        assert_eq!(config.similarity_threshold, 0.3);
    }

    #[test]
    fn test_nli_config_toml_roundtrip() {
        let mut config = EngramConfig::default();
        config.nli.enabled = true;
        config.nli.contradiction_threshold = 0.85;
        config.nli.max_comparisons = 20;
        config.nli.similarity_threshold = 0.5;

        let temp_path = std::env::temp_dir().join("test_nli_config_roundtrip.toml");
        config.to_toml_file(&temp_path).unwrap();
        let loaded = EngramConfig::from_toml_file(&temp_path).unwrap();
        std::fs::remove_file(&temp_path).ok();

        assert!(loaded.nli.enabled);
        assert_eq!(loaded.nli.contradiction_threshold, 0.85);
        assert_eq!(loaded.nli.max_comparisons, 20);
        assert_eq!(loaded.nli.similarity_threshold, 0.5);
        assert_eq!(loaded.nli.model, "Xenova/nli-deberta-v3-xsmall");
    }

    #[test]
    fn test_nli_config_omitted_uses_defaults() {
        let config: EngramConfig = toml::from_str("").unwrap();
        assert!(!config.nli.enabled);
        assert_eq!(config.nli.contradiction_threshold, 0.7);
        assert_eq!(config.nli.max_comparisons, 10);
    }

    #[test]
    fn test_nli_config_custom_toml() {
        let toml = r#"
[nli]
enabled = true
model = "custom-model/nli-v1"
contradiction_threshold = 0.9
max_comparisons = 5
similarity_threshold = 0.4
"#;
        let config: EngramConfig = toml::from_str(toml).unwrap();
        assert!(config.nli.enabled);
        assert_eq!(config.nli.model, "custom-model/nli-v1");
        assert_eq!(config.nli.contradiction_threshold, 0.9);
        assert_eq!(config.nli.max_comparisons, 5);
        assert_eq!(config.nli.similarity_threshold, 0.4);
    }

    #[test]
    fn test_rerank_config_defaults() {
        let config = RerankConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.model, "bge-reranker-base");
        assert_eq!(config.top_n, 50);
        assert_eq!(config.weight, 0.5);
    }

    #[test]
    fn test_rerank_config_disabled_by_default() {
        let config = EngramConfig::default();
        assert!(!config.rerank.enabled);
        assert_eq!(config.rerank.model, "bge-reranker-base");
        assert_eq!(config.rerank.top_n, 50);
        assert_eq!(config.rerank.weight, 0.5);
    }

    #[test]
    fn test_rerank_config_custom_toml() {
        let toml = r#"
[rerank]
enabled = true
model = "bge-reranker-v2-m3"
top_n = 20
weight = 0.7
"#;
        let config: EngramConfig = toml::from_str(toml).unwrap();
        assert!(config.rerank.enabled);
        assert_eq!(config.rerank.model, "bge-reranker-v2-m3");
        assert_eq!(config.rerank.top_n, 20);
        assert_eq!(config.rerank.weight, 0.7);
    }

    #[test]
    fn test_rerank_config_defaults_when_omitted() {
        let config: EngramConfig = toml::from_str("").unwrap();
        assert!(!config.rerank.enabled);
        assert_eq!(config.rerank.model, "bge-reranker-base");
        assert_eq!(config.rerank.top_n, 50);
        assert_eq!(config.rerank.weight, 0.5);
    }

    #[test]
    fn test_rerank_config_toml_roundtrip() {
        let mut config = EngramConfig::default();
        config.rerank.enabled = true;
        config.rerank.model = "jina-reranker-v1-turbo-en".to_string();
        config.rerank.top_n = 30;
        config.rerank.weight = 0.8;

        let temp_path = std::env::temp_dir().join("test_rerank_config_roundtrip.toml");
        config.to_toml_file(&temp_path).unwrap();
        let loaded = EngramConfig::from_toml_file(&temp_path).unwrap();
        std::fs::remove_file(&temp_path).ok();

        assert!(loaded.rerank.enabled);
        assert_eq!(loaded.rerank.model, "jina-reranker-v1-turbo-en");
        assert_eq!(loaded.rerank.top_n, 30);
        assert_eq!(loaded.rerank.weight, 0.8);
    }

    #[test]
    fn test_nli_config_validate_rejects_invalid() {
        // contradiction_threshold out of range
        let nli = NliConfig {
            contradiction_threshold: 1.5,
            ..Default::default()
        };
        assert!(nli.validate().is_err());

        let nli = NliConfig {
            contradiction_threshold: -0.1,
            ..Default::default()
        };
        assert!(nli.validate().is_err());

        // similarity_threshold out of range
        let nli = NliConfig {
            similarity_threshold: 2.0,
            ..Default::default()
        };
        assert!(nli.validate().is_err());

        let nli = NliConfig {
            similarity_threshold: -1.0,
            ..Default::default()
        };
        assert!(nli.validate().is_err());

        // max_comparisons zero
        let nli = NliConfig {
            max_comparisons: 0,
            ..Default::default()
        };
        assert!(nli.validate().is_err());

        // valid config passes
        assert!(NliConfig::default().validate().is_ok());
    }

    #[test]
    fn test_trust_weights_validate() {
        // Valid
        assert!(TrustWeights::default().validate().is_ok());

        // Invalid: > 1.0
        let tw = TrustWeights {
            human: 1.5,
            ..Default::default()
        };
        assert!(tw.validate().is_err());

        // Invalid: < 0.0
        let tw = TrustWeights {
            inferred: -0.1,
            ..Default::default()
        };
        assert!(tw.validate().is_err());
    }

    #[test]
    fn test_scoring_weights_validate() {
        // Valid defaults
        assert!(ScoringWeights::default().validate("test").is_ok());

        // Invalid: doesn't sum to 1.0
        let sw = ScoringWeights {
            keyword: None,
            semantic: Some(0.5),
            relevance: 0.6,
        };
        assert!(sw.validate("test").is_err());

        // Valid with keyword
        let sw = ScoringWeights {
            keyword: Some(0.45),
            semantic: Some(0.30),
            relevance: 0.25,
        };
        assert!(sw.validate("test").is_ok());
    }

    #[test]
    fn test_rerank_config_validate_rejects_invalid() {
        // weight out of range
        let rerank = RerankConfig {
            weight: 1.5,
            ..Default::default()
        };
        assert!(rerank.validate().is_err());

        let rerank = RerankConfig {
            weight: -0.1,
            ..Default::default()
        };
        assert!(rerank.validate().is_err());

        // top_n zero
        let rerank = RerankConfig {
            top_n: 0,
            ..Default::default()
        };
        assert!(rerank.validate().is_err());

        // valid config passes
        assert!(RerankConfig::default().validate().is_ok());
    }

    #[test]
    fn test_scoring_config_validate_rejects_invalid_floors() {
        // scope_multiplier_floor > 1.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.scope_multiplier_floor = 1.5;
        assert!(config.validate().is_err());

        // scope_multiplier_floor < 0.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.scope_multiplier_floor = -0.1;
        assert!(config.validate().is_err());

        // trust_multiplier_floor > 1.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.trust_multiplier_floor = 2.0;
        assert!(config.validate().is_err());

        // trust_multiplier_floor < 0.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.trust_multiplier_floor = -0.5;
        assert!(config.validate().is_err());

        // challenge_penalty > 1.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.challenge_penalty = 1.1;
        assert!(config.validate().is_err());

        // challenge_penalty < 0.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.challenge_penalty = -0.01;
        assert!(config.validate().is_err());

        // valid defaults pass
        assert!(EngramConfig::default().validate().is_ok());
    }

    #[test]
    fn test_depth_decay_config_validate_rejects_invalid() {
        // depth_decay_base > 1.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.depth_decay_base = 1.5;
        assert!(config.validate().is_err());

        // depth_decay_base < 0.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.depth_decay_base = -0.1;
        assert!(config.validate().is_err());

        // depth_decay_floor > 1.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.depth_decay_floor = 2.0;
        assert!(config.validate().is_err());

        // depth_decay_floor < 0.0
        let mut config = EngramConfig::default();
        config.retrieval.scoring.depth_decay_floor = -0.5;
        assert!(config.validate().is_err());

        // valid values pass
        let mut config = EngramConfig::default();
        config.retrieval.scoring.depth_decay_base = 0.9;
        config.retrieval.scoring.depth_decay_floor = 0.1;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_depth_decay_config_toml_roundtrip() {
        let mut config = EngramConfig::default();
        config.retrieval.scoring.depth_decay_base = 0.75;
        config.retrieval.scoring.depth_decay_floor = 0.2;

        let temp_path = std::env::temp_dir().join("test_depth_decay_roundtrip.toml");
        config.to_toml_file(&temp_path).unwrap();
        let loaded = EngramConfig::from_toml_file(&temp_path).unwrap();
        std::fs::remove_file(&temp_path).ok();

        assert_eq!(loaded.retrieval.scoring.depth_decay_base, 0.75);
        assert_eq!(loaded.retrieval.scoring.depth_decay_floor, 0.2);
    }

    #[test]
    fn test_depth_decay_config_defaults_when_omitted() {
        let config: EngramConfig = toml::from_str("").unwrap();
        assert_eq!(config.retrieval.scoring.depth_decay_base, 0.82);
        assert_eq!(config.retrieval.scoring.depth_decay_floor, 0.3);
    }

    #[test]
    fn test_daemon_config_defaults() {
        let d = DaemonConfig::default();
        assert!(d.enabled);
        assert_eq!(d.idle_timeout_secs, 900);
        assert_eq!(d.socket_path, None);
        // Absent `[daemon]` section ⇒ defaults (enabled by default).
        let cfg: EngramConfig = toml::from_str("").unwrap();
        assert!(cfg.daemon.enabled);
        assert_eq!(cfg.daemon.idle_timeout_secs, 900);
        assert_eq!(cfg.daemon.socket_path, None);
    }

    #[test]
    fn test_daemon_config_validate() {
        let mut d = DaemonConfig::default();
        assert!(d.validate().is_ok());
        d.idle_timeout_secs = 0;
        assert!(d.validate().is_err());
        // The floor is 60s: the MCP heartbeat interval is clamped to a 30s
        // minimum, so any idle timeout below 60 guarantees the daemon reaps
        // and respawns between pings (respawn churn). 59 → Err, 60 → Ok.
        d.idle_timeout_secs = 59;
        assert!(d.validate().is_err());
        d.idle_timeout_secs = 60;
        assert!(d.validate().is_ok());
        // Surfaced through the top-level config validate too.
        let mut cfg = EngramConfig::default();
        cfg.daemon.idle_timeout_secs = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_review_config_validate() {
        let mut r = ReviewConfig::default();
        assert_eq!(r.recency_days, Some(90));
        assert!(r.validate().is_ok());

        // Disabled trigger is valid.
        r.recency_days = None;
        assert!(r.validate().is_ok());

        // Zero is ambiguous → rejected.
        r.recency_days = Some(0);
        assert!(r.validate().is_err());

        // Upper bound mirrors stats.retention_days (10 years).
        r.recency_days = Some(3650);
        assert!(r.validate().is_ok());
        r.recency_days = Some(3651);
        assert!(r.validate().is_err());

        // Surfaced through the top-level config validate too.
        let mut cfg = EngramConfig::default();
        cfg.review.recency_days = Some(0);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn test_review_config_serde_default() {
        // Absent `[review]` ⇒ default 90-day recency trigger.
        let cfg: EngramConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.review.recency_days, Some(90));
        // A `[review]` table present but without `recency_days` ⇒ default too.
        let cfg: EngramConfig = toml::from_str("[review]\n").unwrap();
        assert_eq!(cfg.review.recency_days, Some(90));
        // Explicit override parses.
        let cfg: EngramConfig = toml::from_str("[review]\nrecency_days = 30\n").unwrap();
        assert_eq!(cfg.review.recency_days, Some(30));
    }

    #[test]
    fn title_config_serde_default_and_roundtrip() {
        use super::TitleStrategy;

        // Absent `[title]` ⇒ t5: the daemon loads/pools it once, so it is
        // the deployment default. (The one-shot CLI is unaffected — it uses
        // `TitleStrategy::default()`, which is still `Keyword`.)
        let cfg: EngramConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.title.strategy, TitleStrategy::T5);
        assert_eq!(TitleConfig::default().strategy, TitleStrategy::T5);
        // A `[title]` table present but without `strategy` also ⇒ t5
        // (field serde-default matches the struct default — no drift).
        let cfg: EngramConfig = toml::from_str("[title]\n").unwrap();
        assert_eq!(cfg.title.strategy, TitleStrategy::T5);
        // The CLI literal default is deliberately still keyword.
        assert_eq!(TitleStrategy::default(), TitleStrategy::Keyword);

        // Each strategy parses by its lowercase name.
        for (name, want) in [
            ("keyword", TitleStrategy::Keyword),
            ("t5", TitleStrategy::T5),
            ("none", TitleStrategy::None),
        ] {
            let cfg: EngramConfig =
                toml::from_str(&format!("[title]\nstrategy = \"{name}\"\n")).unwrap();
            assert_eq!(cfg.title.strategy, want, "strategy = {name:?}");
        }

        // Round-trips through the file writer.
        let mut cfg = EngramConfig::default();
        cfg.title.strategy = TitleStrategy::T5;
        let temp_path = std::env::temp_dir().join("test_title_config_roundtrip.toml");
        cfg.to_toml_file(&temp_path).unwrap();
        let loaded = EngramConfig::from_toml_file(&temp_path).unwrap();
        std::fs::remove_file(&temp_path).ok();
        assert_eq!(loaded.title.strategy, TitleStrategy::T5);
    }

    #[test]
    fn title_pool_size_resolution() {
        // Unset ⇒ auto 2, but never more than the core count.
        let t = TitleConfig::default();
        assert_eq!(t.pool_size, None);
        assert_eq!(t.resolved_pool_size(8), 2);
        assert_eq!(t.resolved_pool_size(1), 1); // clamped to cores
        assert_eq!(t.resolved_pool_size(0), 1); // cores.max(1) guard

        // Explicit value parses and is clamped to [1, cores].
        let cfg: EngramConfig =
            toml::from_str("[title]\nstrategy = \"t5\"\npool_size = 3\n").unwrap();
        assert_eq!(cfg.title.pool_size, Some(3));
        assert_eq!(cfg.title.resolved_pool_size(8), 3);
        assert_eq!(cfg.title.resolved_pool_size(2), 2); // clamp down to cores
    }

    #[test]
    fn embeddings_pool_size_serde_and_resolved() {
        // Unset ⇒ None; resolver auto-sizes to cores/2, floored at 1.
        // `cores` is passed explicitly so the assertion is identical on any
        // machine / CI runner (not derived from the host CPU count).
        let cfg: EngramConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.embeddings.pool_size, None);
        assert_eq!(cfg.embeddings.resolved_pool_size(8), 4);
        assert_eq!(cfg.embeddings.resolved_pool_size(1), 1); // cores/2 floored at 1
        assert_eq!(cfg.embeddings.resolved_pool_size(0), 1); // (cores/2).max(1) guard

        // Explicit value is honored verbatim regardless of cores (only a
        // floor of 1 so it can't disable embeddings).
        let cfg: EngramConfig =
            toml::from_str("[embeddings]\nprovider = \"onnx\"\ndimensions = 384\nmax_tokens = 256\npool_size = 3\n")
                .unwrap();
        assert_eq!(cfg.embeddings.pool_size, Some(3));
        assert_eq!(cfg.embeddings.resolved_pool_size(2), 3);
        assert_eq!(cfg.embeddings.resolved_pool_size(64), 3);

        // Legacy `[embeddings]` without the field still parses (back-compat).
        let legacy: EmbeddingsConfig =
            toml::from_str("provider = \"onnx\"\ndimensions = 384\nmax_tokens = 256\n").unwrap();
        assert_eq!(legacy.pool_size, None);
    }

    #[test]
    fn daemon_use_for_cli_defaults_true_and_is_overridable() {
        let cfg: EngramConfig = toml::from_str("").unwrap();
        assert!(cfg.daemon.use_for_cli);
        let cfg: EngramConfig = toml::from_str("[daemon]\nuse_for_cli = false\n").unwrap();
        assert!(!cfg.daemon.use_for_cli);
    }

    #[test]
    fn test_daemon_config_partial_toml() {
        // Only one field set; the rest fall back to defaults.
        let cfg: EngramConfig = toml::from_str("[daemon]\nenabled = false\n").unwrap();
        assert!(!cfg.daemon.enabled);
        assert_eq!(cfg.daemon.idle_timeout_secs, 900);
        assert_eq!(cfg.daemon.socket_path, None);

        let cfg: EngramConfig =
            toml::from_str("[daemon]\nidle_timeout_secs = 30\nsocket_path = \"/run/x.sock\"\n")
                .unwrap();
        assert!(cfg.daemon.enabled);
        assert_eq!(cfg.daemon.idle_timeout_secs, 30);
        assert_eq!(cfg.daemon.socket_path.as_deref(), Some("/run/x.sock"));
    }

    /// Unbounded-growth guard: telemetry retention must default to a finite
    /// window (90 days) for both `Default` and a config file that never
    /// mentions `[stats]` — otherwise the per-project `stats_events` table
    /// grows forever for everyone who never set the field.
    #[test]
    fn stats_retention_defaults_to_90_days() {
        assert_eq!(StatsConfig::default().retention_days, Some(90));

        let cfg: EngramConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.stats.retention_days, Some(90));

        // A `[stats]` section that sets other fields but omits retention
        // still gets the finite default.
        let cfg: EngramConfig = toml::from_str("[stats]\nenabled = true\n").unwrap();
        assert_eq!(cfg.stats.retention_days, Some(90));

        // An explicit value is honored verbatim.
        let cfg: EngramConfig = toml::from_str("[stats]\nretention_days = 7\n").unwrap();
        assert_eq!(cfg.stats.retention_days, Some(7));
    }

    /// `retention_days = 0` used to compute `cutoff = now` and delete every
    /// event while the docs said 0 meant "retain forever". It is now
    /// rejected outright so users must say what they mean.
    #[test]
    fn stats_retention_zero_is_rejected() {
        let cfg = StatsConfig {
            retention_days: Some(0),
            ..StatsConfig::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("retention_days must be >= 1"),
            "unexpected message: {err}"
        );

        // Boundary values stay valid.
        for days in [Some(1), Some(90), Some(3650), None] {
            let cfg = StatsConfig {
                retention_days: days,
                ..StatsConfig::default()
            };
            assert!(cfg.validate().is_ok(), "{days:?} should validate");
        }
        let cfg = StatsConfig {
            retention_days: Some(3651),
            ..StatsConfig::default()
        };
        assert!(cfg.validate().is_err(), "above the 3650 cap must fail");
    }

    // ---- Finding #4 / #20: search.threshold validation -------------------
    //
    // Scenario: an operator hand-edits `[search] threshold` in config.toml.
    // - A negative value is meaningless once scores are normalized to [0, 1]
    //   and, before the fix, slipped through `validate()` and then through the
    //   consumer's `min(1.0)` still negative, which silently DISABLED the
    //   relevance gate (`if threshold > 0.0` is false for negatives) — i.e.
    //   every result, however irrelevant, was returned.
    // - A value > 1.0 is a legacy artifact (old unbounded scale) and must stay
    //   *tolerated* for backward-compat; the consumer clamps it to 1.0.

    fn config_with_search_threshold(t: f64) -> EngramConfig {
        let mut cfg = EngramConfig::default();
        cfg.search.threshold = t;
        cfg
    }

    #[test]
    fn search_threshold_negative_is_rejected() {
        // NEGATIVE test: this must error. Before the fix it returned Ok(()).
        let err = config_with_search_threshold(-0.1)
            .validate()
            .expect_err("negative search.threshold must be rejected");
        assert!(
            err.to_string().contains("search.threshold"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn search_threshold_nan_is_rejected() {
        // NEGATIVE test: NaN fails every comparison and would disable the gate.
        assert!(config_with_search_threshold(f64::NAN).validate().is_err());
    }

    #[test]
    fn search_threshold_valid_range_is_accepted() {
        // POSITIVE test: the normal [0, 1] domain stays valid.
        for t in [0.0, 0.2, 0.5, 1.0] {
            assert!(
                config_with_search_threshold(t).validate().is_ok(),
                "{t} should validate"
            );
        }
    }

    #[test]
    fn search_threshold_above_one_is_tolerated_for_backcompat() {
        // POSITIVE test: a legacy >1.0 value must NOT be a hard error — it is
        // migrated/clamped at the use site. This pins the back-compat contract.
        assert!(
            config_with_search_threshold(5.0).validate().is_ok(),
            "legacy >1.0 threshold must remain tolerated"
        );
    }
}
