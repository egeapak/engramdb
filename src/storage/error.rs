//! Storage error types

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Memory not found: {0}")]
    NotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("Invalid memory file format: {0}")]
    InvalidFormat(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Project not initialized")]
    NotInitialized,
}

pub type Result<T> = std::result::Result<T, StorageError>;
