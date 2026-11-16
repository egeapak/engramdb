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
    /// ambiguous prefix or bad input a 400, anything else — an index
    /// (`StorageError::Index`) or I/O failure — a 500.
    pub fn from_storage(e: engramdb::storage::StorageError) -> Self {
        use engramdb::storage::StorageError;
        match e {
            StorageError::NotFound(id) => {
                Self::not_found("unknown_memory", format!("no memory {id} in this store"))
            }
            e @ StorageError::AmbiguousId { .. } => {
                Self::new(StatusCode::BAD_REQUEST, "ambiguous_id", e.to_string())
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
        let mut r = crate::api::json_response(self.status, &body);
        // A refusal for load (the layout queue is full): the page retries.
        if self.status == StatusCode::SERVICE_UNAVAILABLE {
            r.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engramdb::storage::StorageError;

    /// The variant decides the status, never the message text.
    #[test]
    fn a_typed_index_error_is_a_500_and_an_ambiguous_id_a_400() {
        let index = ApiError::from_storage(StorageError::index("list_summary")(anyhow::anyhow!(
            "Ambiguous ID prefix in a LanceDB message"
        )));
        assert_eq!(
            (index.status, index.code),
            (StatusCode::INTERNAL_SERVER_ERROR, "internal")
        );

        let ambiguous = ApiError::from_storage(StorageError::AmbiguousId {
            prefix: "0192".into(),
            matches: 2,
        });
        assert_eq!(
            (ambiguous.status, ambiguous.code),
            (StatusCode::BAD_REQUEST, "ambiguous_id")
        );
        assert_eq!(
            ambiguous.message,
            "Ambiguous ID prefix '0192': matches 2 memories"
        );

        let invalid = ApiError::from_storage(StorageError::Validation("bad filter".into()));
        assert_eq!(
            (invalid.status, invalid.code),
            (StatusCode::BAD_REQUEST, "invalid")
        );

        // Through an ops `anyhow` chain too.
        let wrapped = ApiError::from_anyhow(
            anyhow::Error::from(StorageError::index("edges read")(anyhow::anyhow!("io")))
                .context("loading the graph"),
        );
        assert_eq!(wrapped.status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
