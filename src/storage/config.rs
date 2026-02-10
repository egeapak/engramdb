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
