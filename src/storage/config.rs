//! Configuration loading with defaults

use crate::types::EngramConfig;
use super::error::Result;
use std::path::Path;
use std::fs;

/// Load configuration from config.toml, or return defaults if file doesn't exist
pub fn load_config(config_path: &Path) -> Result<EngramConfig> {
    if !config_path.exists() {
        return Ok(EngramConfig::default());
    }

    let content = fs::read_to_string(config_path)?;
    let config: EngramConfig = toml::from_str(&content)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_config_returns_defaults_when_missing() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let config = load_config(&config_path).unwrap();
        let default_config = EngramConfig::default();

        assert_eq!(config.retrieval.max_results, default_config.retrieval.max_results);
        assert_eq!(config.retrieval.relevance_threshold, default_config.retrieval.relevance_threshold);
    }

    #[test]
    fn test_load_config_from_valid_file() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let toml_content = r#"
[retrieval]
max_results = 10
relevance_threshold = 0.8
include_expired = true

[retrieval.scoring.with_query]
semantic = 0.6
relevance = 0.25
scope = 0.1
trust = 0.05

[retrieval.scoring.scope_only]
relevance = 0.6
scope = 0.3
trust = 0.1

[retrieval.scoring.degraded]
relevance = 0.7
scope = 0.2
trust = 0.1

[trust_weights]
human = 0.95
agent = 0.85
inferred = 0.55
imported = 0.65
"#;
        fs::write(&config_path, toml_content).unwrap();

        let config = load_config(&config_path).unwrap();

        assert_eq!(config.retrieval.max_results, 10);
        assert_eq!(config.retrieval.relevance_threshold, 0.8);
        assert!(config.retrieval.include_expired);
        assert_eq!(config.trust_weights.human, 0.95);
        assert_eq!(config.trust_weights.agent, 0.85);
        assert_eq!(config.trust_weights.inferred, 0.55);
        assert_eq!(config.trust_weights.imported, 0.65);
    }

    #[test]
    fn test_load_config_invalid_toml() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        fs::write(&config_path, "invalid { toml content").unwrap();

        let result = load_config(&config_path);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), super::super::error::StorageError::Toml(_)));
    }
}
