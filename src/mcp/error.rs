//! Structured error codes for MCP tool responses.

use serde_json::json;

/// Error codes per the EngramDB spec (section 11.6).
pub enum ErrorCode {
    MemoryNotFound,
    ValidationError,
    StoreNotInitialized,
    IndexCorrupt,
    EmbeddingUnavailable,
    CompressFailed,
    ConcurrentWrite,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MemoryNotFound => "MEMORY_NOT_FOUND",
            Self::ValidationError => "VALIDATION_ERROR",
            Self::StoreNotInitialized => "STORE_NOT_INITIALIZED",
            Self::IndexCorrupt => "INDEX_CORRUPT",
            Self::EmbeddingUnavailable => "EMBEDDING_UNAVAILABLE",
            Self::CompressFailed => "COMPRESS_FAILED",
            Self::ConcurrentWrite => "CONCURRENT_WRITE",
        }
    }
}

/// Format a structured error response for MCP tool results.
pub fn error_response(code: ErrorCode, message: &str) -> String {
    json!({
        "error": {
            "code": code.as_str(),
            "message": message
        }
    })
    .to_string()
}
