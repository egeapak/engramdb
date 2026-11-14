//! Storage error types.
//!
//! This module defines [`StorageError`], the error type for all storage
//! operations. It wraps IO, YAML, JSON, and TOML parsing errors, and
//! provides custom error variants for:
//! - NotFound - memory ID not found
//! - InvalidFormat - malformed memory file
//! - Validation - invalid input (the caller's fault)
//! - AmbiguousId - an id prefix that matches more than one memory
//! - Index - a LanceDB (index) operation failed (never the caller's fault)
//! - NotInitialized - EngramDB not initialized in directory
//!
//! The Result type alias simplifies error handling throughout the storage layer.

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Memory not found: {0}")]
    NotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("YAML parse error: {0}")]
    Yaml(#[from] serde_saphyr::DeserializeError),

    #[error("YAML write error: {0}")]
    YamlWrite(#[from] serde_saphyr::SerializeError),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("Invalid memory file format: {0}")]
    InvalidFormat(String),

    /// The caller's input was invalid. Never an index failure: those are
    /// [`StorageError::Index`], so a front end can map this variant to a
    /// "bad request" without reading the message.
    #[error("Validation error: {0}")]
    Validation(String),

    /// An id prefix matched more than one memory.
    #[error("Ambiguous ID prefix '{prefix}': matches {matches} memories")]
    AmbiguousId { prefix: String, matches: usize },

    /// A LanceDB operation failed. `op` names it; the cause is kept as the
    /// source (and printed, with its chain, in the message).
    #[error("index error: {op} failed: {source:#}")]
    Index {
        op: &'static str,
        #[source]
        source: anyhow::Error,
    },

    #[error("Atomic write failed: {0}")]
    Persist(#[from] tempfile::PersistError),

    #[error("Project not initialized")]
    NotInitialized,
}

impl StorageError {
    /// `map_err` adapter for an index operation:
    /// `.map_err(StorageError::index("upsert"))`.
    ///
    /// Deliberately not a `From<anyhow::Error>`: a blanket conversion would
    /// label every unrelated `anyhow` error an index failure.
    pub fn index<E>(op: &'static str) -> impl FnOnce(E) -> StorageError
    where
        E: Into<anyhow::Error>,
    {
        move |e| StorageError::Index {
            op,
            source: e.into(),
        }
    }
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

    #[test]
    fn index_error_keeps_its_source() {
        use std::error::Error as _;
        let cause = anyhow::anyhow!("disk full").context("commit failed");
        let error = StorageError::index("upsert")(cause);
        assert!(matches!(error, StorageError::Index { op: "upsert", .. }));
        assert_eq!(
            error.to_string(),
            "index error: upsert failed: commit failed: disk full"
        );
        assert_eq!(error.source().unwrap().to_string(), "commit failed");
    }

    #[test]
    fn ambiguous_id_message_is_unchanged() {
        let error = StorageError::AmbiguousId {
            prefix: "0192".to_string(),
            matches: 3,
        };
        assert_eq!(
            error.to_string(),
            "Ambiguous ID prefix '0192': matches 3 memories"
        );
    }
}
