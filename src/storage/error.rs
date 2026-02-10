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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn test_error_display_not_found() {
        let error = StorageError::NotFound("x".to_string());
        assert_eq!(error.to_string(), "Memory not found: x");
    }

    #[test]
    fn test_error_display_not_initialized() {
        let error = StorageError::NotInitialized;
        assert_eq!(error.to_string(), "Project not initialized");
    }

    #[test]
    fn test_error_from_io() {
        let io_error = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let storage_error: StorageError = io_error.into();
        assert!(matches!(storage_error, StorageError::Io(_)));
    }

    #[test]
    fn test_error_from_json() {
        let bad_json = "{ invalid json }";
        let result: Result<serde_json::Value> =
            serde_json::from_str(bad_json).map_err(|e| e.into());

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), StorageError::Json(_)));
    }
}
