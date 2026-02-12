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

/// Weights for scoring components.
///
/// Trust is applied as a multiplier on the entire score (from `TrustWeights`),
/// not as a weighted component here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringWeights {
    /// Semantic similarity weight (optional - not available in degraded mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<f64>,

    /// Relevance weight (criticality * decay)
    pub relevance: f64,

    /// Scope proximity weight
    pub scope: f64,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            semantic: Some(0.35),
            relevance: 0.45,
            scope: 0.20,
        }
    }
}

/// Scoring configuration for different retrieval modes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringConfig {
    /// Weights when both query and scope are provided
    pub with_query: ScoringWeights,

    /// Weights for scope-only retrieval
    pub scope_only: ScoringWeights,

    /// Weights for degraded mode (no embeddings)
    pub degraded: ScoringWeights,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            with_query: ScoringWeights::default(),
            scope_only: ScoringWeights {
                semantic: None,
                relevance: 0.50,
                scope: 0.50,
            },
            degraded: ScoringWeights {
                semantic: None,
                relevance: 0.70,
                scope: 0.30,
            },
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
            threshold: 2.0,
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
            relevance_threshold: 0.3,
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
}

impl std::fmt::Display for EmbeddingBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::Onnx => write!(f, "onnx"),
            Self::Ollama => write!(f, "ollama"),
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
            other => Err(format!(
                "unknown embedding backend '{}': expected auto, onnx, or ollama",
                other
            )),
        }
    }
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
}

impl Default for EmbeddingsConfig {
    fn default() -> Self {
        Self {
            backend: EmbeddingBackend::default(),
            provider: "onnx".to_string(),
            dimensions: 384,
            max_tokens: 256,
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

impl Default for NliConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "cross-encoder/nli-deberta-v3-xsmall".to_string(),
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

    /// NLI contradiction detection settings
    #[serde(default)]
    pub nli: NliConfig,

    /// Cross-encoder reranking settings
    #[serde(default)]
    pub rerank: RerankConfig,
}

impl EngramConfig {
    /// Load configuration from a TOML file
    pub fn from_toml_file(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config = toml::from_str(&contents)?;
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

    #[test]
    fn test_config_defaults() {
        let config = EngramConfig::default();

        // Retrieval config
        assert_eq!(config.retrieval.max_results, 10);
        assert_eq!(config.retrieval.relevance_threshold, 0.3);
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

        // Scoring weights - with_query
        assert_eq!(config.retrieval.scoring.with_query.semantic, Some(0.35));
        assert_eq!(config.retrieval.scoring.with_query.relevance, 0.45);
        assert_eq!(config.retrieval.scoring.with_query.scope, 0.20);

        // Scoring weights - scope_only
        assert_eq!(config.retrieval.scoring.scope_only.semantic, None);
        assert_eq!(config.retrieval.scoring.scope_only.relevance, 0.50);
        assert_eq!(config.retrieval.scoring.scope_only.scope, 0.50);

        // Scoring weights - degraded
        assert_eq!(config.retrieval.scoring.degraded.semantic, None);
        assert_eq!(config.retrieval.scoring.degraded.relevance, 0.70);
        assert_eq!(config.retrieval.scoring.degraded.scope, 0.30);

        // Search config
        assert_eq!(config.search.semantic_weight, 3.0);
        assert_eq!(config.search.threshold, 2.0);

        // Embeddings config
        assert_eq!(config.embeddings.provider, "onnx");
        assert_eq!(config.embeddings.dimensions, 384);
        assert_eq!(config.embeddings.max_tokens, 256);

        // NLI config
        assert!(!config.nli.enabled);
        assert_eq!(config.nli.model, "cross-encoder/nli-deberta-v3-xsmall");
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
semantic = 0.35
relevance = 0.45
scope = 0.20

[retrieval.scoring.scope_only]
relevance = 0.50
scope = 0.50

[retrieval.scoring.degraded]
relevance = 0.70
scope = 0.30
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
    fn test_config_scoring_weights_sum_to_one() {
        let config = EngramConfig::default();

        // with_query: semantic + relevance + scope sum to 1.0
        // (trust is now a multiplier, not a weighted component)
        let wq = &config.retrieval.scoring.with_query;
        let wq_sum = wq.semantic.unwrap_or(0.0) + wq.relevance + wq.scope;
        assert!(
            (wq_sum - 1.0).abs() < f64::EPSILON,
            "with_query weights sum to {}, expected 1.0",
            wq_sum
        );

        // scope_only: relevance + scope sum to 1.0
        let so = &config.retrieval.scoring.scope_only;
        let so_sum = so.semantic.unwrap_or(0.0) + so.relevance + so.scope;
        assert!(
            (so_sum - 1.0).abs() < f64::EPSILON,
            "scope_only weights sum to {}, expected 1.0",
            so_sum
        );

        // degraded: relevance + scope sum to 1.0
        let dg = &config.retrieval.scoring.degraded;
        let dg_sum = dg.semantic.unwrap_or(0.0) + dg.relevance + dg.scope;
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
        assert_eq!(config.search.threshold, 2.0);
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
        assert_eq!(config.model, "cross-encoder/nli-deberta-v3-xsmall");
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
        assert_eq!(loaded.nli.model, "cross-encoder/nli-deberta-v3-xsmall");
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
}
