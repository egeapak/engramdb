use serde::{Deserialize, Serialize};

/// Weights for scoring components
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringWeights {
    /// Semantic similarity weight (optional - not available in degraded mode)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic: Option<f64>,

    /// Relevance weight
    pub relevance: f64,

    /// Scope proximity weight
    pub scope: f64,

    /// Trust/provenance weight
    pub trust: f64,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            semantic: Some(0.5),
            relevance: 0.3,
            scope: 0.15,
            trust: 0.05,
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
                relevance: 0.5,
                scope: 0.4,
                trust: 0.1,
            },
            degraded: ScoringWeights {
                semantic: None,
                relevance: 0.6,
                scope: 0.3,
                trust: 0.1,
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
            same_directory: 0.7,
            same_module: 0.4,
            project_root: 0.1,
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
            exact: 1.0,
            parent: 0.5,
            sibling: 0.3,
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
            agent: 0.9,
            inferred: 0.6,
            imported: 0.7,
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
            gc: 0.1,
            compress: 0.2,
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
            relevance_threshold: 0.5,
            max_results: 20,
            include_expired: false,
            scoring: ScoringConfig::default(),
        }
    }
}

/// Top-level EngramDB configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngramConfig {
    /// Retrieval settings
    #[serde(default)]
    pub retrieval: RetrievalConfig,

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

impl Default for EngramConfig {
    fn default() -> Self {
        Self {
            retrieval: RetrievalConfig::default(),
            scope_proximity: ScopeProximityConfig::default(),
            logical_bonus: LogicalBonusConfig::default(),
            trust_weights: TrustWeights::default(),
            thresholds: ThresholdsConfig::default(),
        }
    }
}

impl EngramConfig {
    /// Load configuration from a TOML file
    pub fn from_toml_file(path: impl AsRef<std::path::Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Save configuration to a TOML file
    pub fn to_toml_file(&self, path: impl AsRef<std::path::Path>) -> Result<(), Box<dyn std::error::Error>> {
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
        assert_eq!(config.retrieval.max_results, 20);
        assert_eq!(config.retrieval.relevance_threshold, 0.5);
        assert!(!config.retrieval.include_expired);

        // Trust weights
        assert_eq!(config.trust_weights.human, 1.0);
        assert_eq!(config.trust_weights.agent, 0.9);
        assert_eq!(config.trust_weights.inferred, 0.6);
        assert_eq!(config.trust_weights.imported, 0.7);

        // Scope proximity
        assert_eq!(config.scope_proximity.exact_file, 1.0);
        assert_eq!(config.scope_proximity.same_directory, 0.7);
        assert_eq!(config.scope_proximity.same_module, 0.4);
        assert_eq!(config.scope_proximity.project_root, 0.1);

        // Logical bonus
        assert_eq!(config.logical_bonus.exact, 1.0);
        assert_eq!(config.logical_bonus.parent, 0.5);
        assert_eq!(config.logical_bonus.sibling, 0.3);

        // Thresholds
        assert_eq!(config.thresholds.needs_review, 0.3);
        assert_eq!(config.thresholds.gc, 0.1);
        assert_eq!(config.thresholds.compress, 0.2);

        // Scoring weights - with_query
        assert_eq!(config.retrieval.scoring.with_query.semantic, Some(0.5));
        assert_eq!(config.retrieval.scoring.with_query.relevance, 0.3);
        assert_eq!(config.retrieval.scoring.with_query.scope, 0.15);
        assert_eq!(config.retrieval.scoring.with_query.trust, 0.05);

        // Scoring weights - scope_only
        assert_eq!(config.retrieval.scoring.scope_only.semantic, None);
        assert_eq!(config.retrieval.scoring.scope_only.relevance, 0.5);
        assert_eq!(config.retrieval.scoring.scope_only.scope, 0.4);
        assert_eq!(config.retrieval.scoring.scope_only.trust, 0.1);

        // Scoring weights - degraded
        assert_eq!(config.retrieval.scoring.degraded.semantic, None);
        assert_eq!(config.retrieval.scoring.degraded.relevance, 0.6);
        assert_eq!(config.retrieval.scoring.degraded.scope, 0.3);
        assert_eq!(config.retrieval.scoring.degraded.trust, 0.1);
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
        assert_eq!(loaded.scope_proximity.exact_file, original.scope_proximity.exact_file);
        assert_eq!(loaded.logical_bonus.exact, original.logical_bonus.exact);
        assert_eq!(loaded.thresholds.needs_review, original.thresholds.needs_review);
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
semantic = 0.5
relevance = 0.3
scope = 0.15
trust = 0.05

[retrieval.scoring.scope_only]
relevance = 0.5
scope = 0.4
trust = 0.1

[retrieval.scoring.degraded]
relevance = 0.6
scope = 0.3
trust = 0.1
"#;

        let config: EngramConfig = toml::from_str(partial_toml).unwrap();

        // Verify retrieval section was parsed
        assert_eq!(config.retrieval.max_results, 50);
        assert_eq!(config.retrieval.relevance_threshold, 0.7);
        assert!(config.retrieval.include_expired);

        // Verify other sections use defaults (thanks to #[serde(default)])
        assert_eq!(config.trust_weights.human, 1.0);
        assert_eq!(config.scope_proximity.exact_file, 1.0);
        assert_eq!(config.logical_bonus.exact, 1.0);
        assert_eq!(config.thresholds.needs_review, 0.3);
    }
}
