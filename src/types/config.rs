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
            threshold: 0.0,
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

/// Embeddings provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingsConfig {
    /// Embedding model name. Transport (ONNX/Ollama) is resolved automatically.
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
            provider: "onnx".to_string(),
            dimensions: 384,
            max_tokens: 256,
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
        assert_eq!(config.search.threshold, 0.0);

        // Embeddings config
        assert_eq!(config.embeddings.provider, "onnx");
        assert_eq!(config.embeddings.dimensions, 384);
        assert_eq!(config.embeddings.max_tokens, 256);
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
        assert_eq!(config.search.threshold, 0.0);
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
}
