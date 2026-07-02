//! Configuration loading with defaults.
//!
//! This module provides the `load_config()` function to load EngramDB
//! configuration from config.toml in the project directory. If the file
//! doesn't exist, returns default configuration values.
//!
//! Configuration is defined in [`engram_types::EngramConfig`] and includes
//! scoring weights, retrieval thresholds, scope bonuses, and trust weights.

use super::error::Result;
use engram_types::EngramConfig;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Load configuration from config.toml, or return defaults if file doesn't exist
///
/// A config that parses but fails [`EngramConfig::validate`] (weights not
/// summing to 1.0, out-of-range timeouts, …) is returned as-is with a loud
/// warning: runtime behavior stays permissive (score math clamps; `doctor`
/// is the diagnostic surface), but the problem is no longer invisible.
pub async fn load_config(config_path: &Path) -> Result<EngramConfig> {
    if !config_path.exists() {
        return Ok(EngramConfig::default());
    }

    let content = tokio::fs::read_to_string(config_path).await?;
    let config: EngramConfig = toml::from_str(&content)?;
    if let Err(e) = config.validate() {
        warn_once(
            config_path,
            &format!(
                "config file {} is invalid ({e}); continuing with the values as written — \
                 run `engramdb doctor` for details",
                config_path.display()
            ),
        );
    }
    Ok(config)
}

/// Load configuration, falling back to defaults on ANY failure — loudly.
///
/// This is the shared replacement for the `load_config(...).unwrap_or_default()`
/// pattern: front-ends that must never fail because of a bad config (the MCP
/// server, provider resolution, store open) still get defaults, but a config
/// file that fails to parse is reported once per path per process instead of
/// being silently ignored wholesale. A partial section (e.g. `[nli]` with
/// required fields missing) fails the whole-file parse, so without the
/// warning a user's entire config — including valid sections — vanished
/// without a trace.
pub async fn load_config_or_default(config_path: &Path) -> EngramConfig {
    match load_config(config_path).await {
        Ok(config) => config,
        Err(e) => {
            warn_once(
                config_path,
                &format!(
                    "ignoring config file {}: {e}; ALL settings in it are falling back to \
                     defaults — fix the file (see `engramdb doctor`) to restore them",
                    config_path.display()
                ),
            );
            EngramConfig::default()
        }
    }
}

/// Emit `tracing::warn!` for a config problem once per path per process, so
/// per-tool-call config loads don't flood the log with the same diagnosis.
fn warn_once(config_path: &Path, message: &str) {
    static WARNED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    let warned = WARNED.get_or_init(|| Mutex::new(HashSet::new()));
    let fresh = warned
        .lock()
        .map(|mut set| set.insert(config_path.to_path_buf()))
        .unwrap_or(true);
    if fresh {
        tracing::warn!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_load_config_returns_defaults_when_missing() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        let config = load_config(&config_path).await.unwrap();
        let default_config = EngramConfig::default();

        assert_eq!(
            config.retrieval.max_results,
            default_config.retrieval.max_results
        );
        assert_eq!(
            config.retrieval.relevance_threshold,
            default_config.retrieval.relevance_threshold
        );
    }

    #[tokio::test]
    async fn test_load_config_from_valid_file() {
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
        tokio::fs::write(&config_path, toml_content).await.unwrap();

        let config = load_config(&config_path).await.unwrap();

        assert_eq!(config.retrieval.max_results, 10);
        assert_eq!(config.retrieval.relevance_threshold, 0.8);
        assert!(config.retrieval.include_expired);
        assert_eq!(config.trust_weights.human, 0.95);
        assert_eq!(config.trust_weights.agent, 0.85);
        assert_eq!(config.trust_weights.inferred, 0.55);
        assert_eq!(config.trust_weights.imported, 0.65);
    }

    #[tokio::test]
    async fn test_load_config_invalid_toml() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        tokio::fs::write(&config_path, "invalid { toml content")
            .await
            .unwrap();

        let result = load_config(&config_path).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            super::super::error::StorageError::Toml(_)
        ));
    }
}
