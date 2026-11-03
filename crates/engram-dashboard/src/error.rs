//! The API's one error shape: `{"error": {"code", "message"}}` with a
//! matching status.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }

    /// A store that is registered but cannot be read right now (its path is
    /// gone, it was never indexed here, it is a worktree with no store).
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "unavailable", message)
    }

    pub fn internal(e: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
    }

    /// Map a storage error from a store read: a missing memory is a 404, an
    /// ambiguous prefix or bad input a 400, anything else a 500.
    pub fn from_storage(e: engramdb::storage::StorageError) -> Self {
        use engramdb::storage::StorageError;
        match e {
            StorageError::NotFound(id) => {
                Self::not_found("unknown_memory", format!("no memory {id} in this store"))
            }
            StorageError::Validation(msg) => Self::new(StatusCode::BAD_REQUEST, "invalid", msg),
            other => Self::internal(other),
        }
    }

    /// Map an `anyhow` error from an ops call, keeping a storage `NotFound`
    /// a 404.
    pub fn from_anyhow(e: anyhow::Error) -> Self {
        match e.downcast::<engramdb::storage::StorageError>() {
            Ok(se) => Self::from_storage(se),
            Err(e) => Self::internal(format!("{e:#}")),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": { "code": self.code, "message": self.message }
        });
        crate::api::json_response(self.status, &body)
    }
}
