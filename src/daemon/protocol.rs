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
///
/// `4` added `Pong.build` and `DaemonStatus::{build, model_ids}`.
///
/// **Also bump on a model-default change, not just a wire change.** The daemon
/// caches provider bundles for its whole lifetime, so one started before an
/// upgrade keeps serving the *old* embedding model while the upgraded client
/// fingerprints the new one. The wire was unchanged across 0.8.0 → 0.9.0, so
/// nothing evicted the stale daemon: `reindex` re-embedded with the old model
/// and stamped its id, `doctor` demanded the new one, and repeating the
/// suggested command could never converge. `Pong.build` now catches this
/// automatically; bumping here is the explicit lever.
pub const PROTOCOL_VERSION: &str = "4";

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
    /// Generate an abstractive (T5) title for `text`. Delegated so the
    /// dominant create-path model loads once machine-wide (and is pooled)
    /// instead of being rebuilt per `create`.
    Title { text: String },
    /// Report daemon status + cumulative request metrics. Loads no models.
    Status,
    /// Ask the daemon to exit. It acks, finishes flushing the response, then
    /// terminates (a fresh one is auto-spawned by the next MCP run).
    Shutdown,
}

/// A response frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DaemonResponse {
    /// Reply to [`DaemonOp::Ping`].
    Pong {
        /// Wire-protocol version ([`PROTOCOL_VERSION`]).
        version: String,
        /// Daemon binary's crate version. Separate from `version` because a
        /// release can change which models load without changing the wire.
        /// `None` from a pre-protocol-4 daemon.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build: Option<String>,
    },
    /// Reply to [`DaemonOp::Meta`].
    Meta {
        dimensions: usize,
        max_tokens: usize,
        /// Stable id of the embedding model the daemon actually loaded
        /// (`EmbeddingProvider::model_id`), so remote providers report the
        /// daemon's true model identity for model-change detection.
        model_id: String,
    },
    /// Reply to [`DaemonOp::Embed`], one vector per input text in order.
    Embedded { vectors: Vec<Vec<f32>> },
    /// Reply to [`DaemonOp::Classify`], one result per input pair in order.
    Classified { results: Vec<NliWire> },
    /// Reply to [`DaemonOp::Rerank`]: `(original_index, raw_score)` pairs.
    Reranked { scores: Vec<(usize, f32)> },
    /// Reply to [`DaemonOp::Title`]: the generated title.
    Title { title: String },
    /// Reply to [`DaemonOp::Status`].
    Status(DaemonStatus),
    /// Reply to [`DaemonOp::Shutdown`] — sent immediately before the daemon
    /// exits.
    ShuttingDown,
    /// The daemon could not satisfy the request (e.g. model unavailable). The
    /// caller falls back to in-process handling.
    Error { message: String },
}

/// Cumulative per-op request counts, nested inside [`DaemonStatus`].
///
/// Distinct from the internal `MetricsSnapshot`, which is an in-memory counter
/// view with `total` as a computed method: `total` is an explicit field here so
/// consumers read it rather than re-deriving it.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct RequestCounts {
    pub embed: u64,
    pub classify: u64,
    pub rerank: u64,
    pub meta: u64,
    pub status: u64,
    pub title: u64,
    pub total: u64,
}

impl From<crate::daemon::metrics::MetricsSnapshot> for RequestCounts {
    fn from(s: crate::daemon::metrics::MetricsSnapshot) -> Self {
        Self {
            embed: s.embed,
            classify: s.classify,
            rerank: s.rerank,
            meta: s.meta,
            status: s.status,
            title: s.title,
            total: s.total(),
        }
    }
}

/// Daemon status + cumulative request metrics. [`RequestCounts`] is cumulative
/// across daemon restarts (persisted to the global LanceDB store).
///
/// The field layout deliberately matches what the CLI prints, so
/// `engram_cli::output` can `#[serde(flatten)]` this straight into its own
/// shape instead of unpacking and re-nesting it. That is why `version`
/// serializes as `protocol` (there is now also a `build` version, so the bare
/// name is ambiguous) and why the counters are grouped rather than seven flat
/// `requests_*` fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    #[serde(rename = "protocol")]
    pub version: String,
    /// Daemon binary's crate version — tells you it is running an older build
    /// than the CLI querying it, hence possibly stale cached models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub pid: u32,
    /// Seconds since *this* daemon process started.
    pub uptime_secs: u64,
    /// Seconds since the daemon last served a request.
    pub idle_secs: u64,
    /// Distinct model bundles (config signatures) currently resident.
    pub bundles_loaded: usize,
    /// Model ids currently resident, e.g. `["embed=onnx/all-MiniLM-L12-v2-u8"]`.
    /// Answers "which models is this daemon actually serving?".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_ids: Vec<String>,
    /// Number of `Ping` requests received since this daemon process started.
    /// In-memory only — not persisted across restarts.
    pub ping_count: u64,
    /// Seconds since the last `Ping` was received, or `None` if no ping has
    /// arrived yet in this daemon's lifetime. In-memory only.
    pub last_ping_secs_ago: Option<u64>,
    pub requests: RequestCounts,
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

/// Hard cap on a single frame. The socket is a local IPC trust boundary; a
/// buggy or hostile peer that never sends `\n` must not be able to drive the
/// daemon (or a client) to OOM. 64 MiB is far above any legitimate frame —
/// requests carry short texts and responses carry per-memory chunk vectors.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Read one newline-delimited JSON frame, bounded by [`MAX_FRAME_BYTES`].
///
/// Returns `Ok(None)` on a clean EOF or an empty/blank frame (peer closed
/// without sending data). Errors with `InvalidData` if a frame exceeds the
/// cap or isn't valid JSON.
pub async fn read_msg<R, T>(r: &mut R) -> std::io::Result<Option<T>>
where
    R: AsyncBufReadExt + Unpin,
    T: DeserializeOwned,
{
    read_msg_capped(r, MAX_FRAME_BYTES).await
}

/// [`read_msg`] with an explicit byte cap. Split out so the cap behavior is
/// unit-testable without allocating the 64 MiB production limit.
pub(crate) async fn read_msg_capped<R, T>(r: &mut R, max: usize) -> std::io::Result<Option<T>>
where
    R: AsyncBufReadExt + Unpin,
    T: DeserializeOwned,
{
    let too_big = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "daemon frame exceeds maximum size",
        )
    };
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = r.fill_buf().await?;
        if chunk.is_empty() {
            // EOF. A partial (newline-less) frame here is a truncated peer;
            // treat it as a clean close rather than feeding junk to serde.
            return Ok(None);
        }
        if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&chunk[..pos]);
            r.consume(pos + 1);
            break;
        }
        let n = chunk.len();
        buf.extend_from_slice(chunk);
        r.consume(n);
        if buf.len() > max {
            return Err(too_big());
        }
    }
    if buf.len() > max {
        return Err(too_big());
    }
    if buf.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(None);
    }
    let msg = serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(msg))
}
