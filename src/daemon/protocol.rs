//! Wire protocol for the embedding daemon.
//!
//! One request and one response per line, each a JSON object (`serde_json`
//! emits no embedded newlines, so a line is a frame). The connection stays
//! open for multiple request/response round-trips.

use crate::types::EmbeddingBackend;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Bumped on any incompatible wire change. A client that gets a mismatched
/// `Pong.version` treats the daemon as unusable and falls back in-process
/// rather than risk decoding garbage from a stale daemon binary.
pub const PROTOCOL_VERSION: &str = "1";

/// A request frame: which store's config selects the model, the resolved
/// embedding backend (sent so the daemon's provider key matches the client's
/// regardless of the daemon process environment), and the operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonRequest {
    /// Resolved store directory. The daemon loads
    /// `<dir>/.engramdb/config.toml` to pick the right model bundle. Ignored
    /// (and may be empty) for [`DaemonOp::Ping`].
    pub dir: String,
    /// Resolved embedding backend, or `None` to let the daemon resolve from
    /// config + its own environment.
    pub backend: Option<EmbeddingBackend>,
    /// The operation to perform.
    pub op: DaemonOp,
}

/// The model operation a request is asking the daemon to perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DaemonOp {
    /// Liveness + protocol-version handshake. Loads no models.
    Ping,
    /// Report the embedding model's dimensionality and token limit. Used by
    /// the remote embedding provider, which needs these synchronously for
    /// chunking and vector-store schema agreement.
    Meta,
    /// Embed each text; response preserves order.
    Embed { texts: Vec<String> },
    /// Classify each `(premise, hypothesis)` pair for contradiction detection.
    Classify { pairs: Vec<(String, String)> },
    /// Cross-encoder score `documents` against `query`.
    Rerank {
        query: String,
        documents: Vec<String>,
    },
}

/// A response frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DaemonResponse {
    /// Reply to [`DaemonOp::Ping`].
    Pong { version: String },
    /// Reply to [`DaemonOp::Meta`].
    Meta {
        dimensions: usize,
        max_tokens: usize,
    },
    /// Reply to [`DaemonOp::Embed`], one vector per input text in order.
    Embedded { vectors: Vec<Vec<f32>> },
    /// Reply to [`DaemonOp::Classify`], one result per input pair in order.
    Classified { results: Vec<NliWire> },
    /// Reply to [`DaemonOp::Rerank`]: `(original_index, raw_score)` pairs.
    Reranked { scores: Vec<(usize, f32)> },
    /// The daemon could not satisfy the request (e.g. model unavailable). The
    /// caller falls back to in-process handling.
    Error { message: String },
}

/// NLI class probabilities. The dominant label is recomputed client-side via
/// `NliResult::from_probs`, so it isn't transmitted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NliWire {
    pub entailment: f32,
    pub neutral: f32,
    pub contradiction: f32,
}

/// Serialize `msg` as a single newline-terminated JSON frame and flush it.
pub async fn write_msg<W, T>(w: &mut W, msg: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(msg)?;
    bytes.push(b'\n');
    w.write_all(&bytes).await?;
    w.flush().await
}

/// Read one newline-delimited JSON frame. Returns `Ok(None)` on a clean EOF
/// (peer closed without sending a partial frame).
pub async fn read_msg<R, T>(r: &mut R) -> std::io::Result<Option<T>>
where
    R: AsyncBufReadExt + Unpin,
    T: DeserializeOwned,
{
    let mut line = String::new();
    let n = r.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    let trimmed = line.trim_end();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let msg = serde_json::from_str(trimmed)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(msg))
}
