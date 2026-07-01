//! Structured error codes for MCP tool responses.

use serde_json::json;

/// Error codes per the EngramDB spec (section 11.6).
pub enum ErrorCode {
    MemoryNotFound,
    ValidationError,
    StoreNotInitialized,
    ProjectNotFound,
    IndexCorrupt,
    EmbeddingUnavailable,
    CompressFailed,
    ConcurrentWrite,
    InternalError,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MemoryNotFound => "MEMORY_NOT_FOUND",
            Self::ValidationError => "VALIDATION_ERROR",
            Self::StoreNotInitialized => "STORE_NOT_INITIALIZED",
            Self::ProjectNotFound => "PROJECT_NOT_FOUND",
            Self::IndexCorrupt => "INDEX_CORRUPT",
            Self::EmbeddingUnavailable => "EMBEDDING_UNAVAILABLE",
            Self::CompressFailed => "COMPRESS_FAILED",
            Self::ConcurrentWrite => "CONCURRENT_WRITE",
            Self::InternalError => "INTERNAL_ERROR",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_maps_to_stable_screaming_snake_case() {
        let pairs = [
            (ErrorCode::MemoryNotFound, "MEMORY_NOT_FOUND"),
            (ErrorCode::ValidationError, "VALIDATION_ERROR"),
            (ErrorCode::StoreNotInitialized, "STORE_NOT_INITIALIZED"),
            (ErrorCode::ProjectNotFound, "PROJECT_NOT_FOUND"),
            (ErrorCode::IndexCorrupt, "INDEX_CORRUPT"),
            (ErrorCode::EmbeddingUnavailable, "EMBEDDING_UNAVAILABLE"),
            (ErrorCode::CompressFailed, "COMPRESS_FAILED"),
            (ErrorCode::ConcurrentWrite, "CONCURRENT_WRITE"),
            (ErrorCode::InternalError, "INTERNAL_ERROR"),
        ];
        for (code, expected) in pairs {
            assert_eq!(code.as_str(), expected);
        }
    }

    #[test]
    fn error_response_is_well_formed_json() {
        let raw = error_response(ErrorCode::MemoryNotFound, "no such memory: abc");
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["error"]["code"], "MEMORY_NOT_FOUND");
        assert_eq!(parsed["error"]["message"], "no such memory: abc");
    }

    #[test]
    fn error_response_escapes_special_characters_in_message() {
        // Messages can contain quotes/newlines from underlying errors; the
        // output must stay parseable JSON rather than corrupting the envelope.
        let raw = error_response(ErrorCode::ValidationError, "bad \"value\"\nline2");
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["error"]["message"], "bad \"value\"\nline2");
    }
}
