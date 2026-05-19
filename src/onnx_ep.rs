//! Optional ONNX Runtime execution-provider wiring (Core ML, XNNPACK).
//!
//! ONNX Runtime has no literal "MPS" provider. On Apple platforms the
//! Metal / Neural Engine path is the **Core ML** EP. **XNNPACK** is a
//! portable, per-node CPU kernel EP (NEON/SVE on ARM) that falls back to
//! ONNX Runtime's MLAS kernels op-by-op for anything it doesn't implement.
//! This module centralizes the feature-gated decision of which providers to
//! register so every ONNX session — the ones we build directly via `ort`
//! (NLI, T5) and the ones `fastembed` builds internally (embeddings,
//! reranker) — picks up the same policy.
//!
//! Both accelerators are **off by default**. With the feature off,
//! [`providers_for`] returns an empty list and ONNX Runtime uses its
//! built-in CPU (MLAS) provider — byte-for-byte the previous behavior, so
//! the default build is unaffected.
//!
//! - `coreml` (macOS only): Core ML EP, compute units = all (ANE+GPU+CPU);
//!   ORT auto-falls back to CPU for unsupported subgraphs.
//! - `xnnpack` (aarch64/x86_64): XNNPACK EP; per-node MLAS fallback. The
//!   provider is compiled into the prebuilt `ort` binary we already
//!   download, so only the Cargo feature is needed (no source build).
//!
//! Production code uses the build-selected default ([`default_backend`]).
//! The explicit [`Backend`] variants exist so the benchmark suite can A/B
//! the same workload across backends within one process.

use ort::ep::ExecutionProviderDispatch;
use ort::session::builder::SessionBuilder;

/// Which ONNX Runtime execution backend to register for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// ONNX Runtime's built-in CPU (MLAS) provider — no extra EP registered.
    Cpu,
    /// Apple Core ML (Neural Engine + GPU via Metal + CPU). Effective only
    /// when compiled `--features coreml` on macOS; otherwise == [`Self::Cpu`].
    CoreMl,
    /// XNNPACK portable CPU kernels with per-node MLAS fallback. Effective
    /// only when compiled `--features xnnpack`; otherwise == [`Self::Cpu`].
    Xnnpack,
}

/// Whether the Core ML provider is compiled in and usable on this target.
pub fn coreml_available() -> bool {
    cfg!(all(feature = "coreml", target_os = "macos"))
}

/// Whether the XNNPACK provider is compiled in for this target. XNNPACK is
/// supported by ONNX Runtime on aarch64 and x86_64, which covers every
/// EngramDB target; the gate is purely the Cargo feature.
pub fn xnnpack_available() -> bool {
    cfg!(feature = "xnnpack")
}

/// The backend selected by build configuration. Core ML when available,
/// else CPU. XNNPACK is never the implicit default — it is opt-in via the
/// benchmark harness until its A/B data justifies promoting it.
pub fn default_backend() -> Backend {
    if coreml_available() {
        Backend::CoreMl
    } else {
        Backend::Cpu
    }
}

/// Intra-op thread count for the directly-built `ort` sessions (NLI, T5).
///
/// Defaults to 1 — the historical hardcoded value, so production behavior
/// is unchanged — and is overridable via `ENGRAMDB_ONNX_INTRA_THREADS` to
/// benchmark the single-call-latency vs concurrency tradeoff. The embedding
/// path (fastembed) manages its own thread pool and is unaffected.
pub fn intra_threads() -> usize {
    std::env::var("ENGRAMDB_ONNX_INTRA_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n != 0)
        .unwrap_or(1)
}

#[cfg(all(feature = "coreml", target_os = "macos"))]
fn coreml_eps() -> Vec<ExecutionProviderDispatch> {
    use ort::ep::{coreml::ComputeUnits, CoreML};
    vec![CoreML::default()
        .with_compute_units(ComputeUnits::All)
        .build()]
}

#[cfg(not(all(feature = "coreml", target_os = "macos")))]
fn coreml_eps() -> Vec<ExecutionProviderDispatch> {
    Vec::new()
}

#[cfg(feature = "xnnpack")]
fn xnnpack_eps() -> Vec<ExecutionProviderDispatch> {
    use ort::ep::XNNPACK;
    vec![XNNPACK::default().build()]
}

#[cfg(not(feature = "xnnpack"))]
fn xnnpack_eps() -> Vec<ExecutionProviderDispatch> {
    Vec::new()
}

/// Execution providers for an explicit backend, in priority order.
///
/// Empty for [`Backend::Cpu`], and also empty for an accelerator backend on
/// a target/build where it is unavailable, so the caller transparently
/// runs on the built-in CPU provider.
pub fn providers_for(backend: Backend) -> Vec<ExecutionProviderDispatch> {
    match backend {
        Backend::Cpu => Vec::new(),
        Backend::CoreMl => coreml_eps(),
        Backend::Xnnpack => xnnpack_eps(),
    }
}

/// Execution providers for the build-selected default backend.
pub fn execution_providers() -> Vec<ExecutionProviderDispatch> {
    providers_for(default_backend())
}

/// Apply an explicit backend's execution providers to an `ort` session
/// builder.
///
/// Returns the builder unchanged when the backend registers no providers,
/// preserving the plain CPU-only path with zero behavior change.
pub fn apply_backend(builder: SessionBuilder, backend: Backend) -> ort::Result<SessionBuilder> {
    let eps = providers_for(backend);
    if eps.is_empty() {
        Ok(builder)
    } else {
        builder.with_execution_providers(eps)
    }
}

/// Apply the build-selected default backend's execution providers to an
/// `ort` session builder.
pub fn apply_execution_providers(builder: SessionBuilder) -> ort::Result<SessionBuilder> {
    apply_backend(builder, default_backend())
}
