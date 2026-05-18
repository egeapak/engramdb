//! Optional Core ML (Apple GPU / Neural Engine) execution-provider wiring.
//!
//! ONNX Runtime has no literal "MPS" execution provider. On Apple platforms
//! the Metal / Neural Engine acceleration path is the **Core ML** execution
//! provider: Core ML lowers supported subgraphs onto the Apple Neural Engine,
//! the GPU (via Metal Performance Shaders), or the CPU as appropriate. This
//! module centralizes the feature-gated decision of which execution providers
//! to register so every ONNX session in the codebase — both the sessions we
//! build directly through `ort` (NLI, T5) and the ones `fastembed` builds
//! internally (embeddings, reranker) — picks up the same acceleration policy.
//!
//! Behavior matrix:
//! - `coreml` feature **off** (default): no execution providers registered;
//!   ONNX Runtime uses its built-in CPU provider. Byte-for-byte the previous
//!   behavior, so the default Linux/CI build is unaffected.
//! - `coreml` feature **on**, `target_os = "macos"`: the Core ML EP is
//!   prepended (compute units = all: ANE + GPU + CPU). ONNX Runtime
//!   automatically falls back to CPU for any op or subgraph Core ML cannot
//!   run, so this is safe even for models that are not fully Core
//!   ML-compatible.
//! - `coreml` feature **on**, non-macOS: no-op (the provider is unavailable
//!   on the platform), so the build still succeeds but runs on CPU.
//!
//! Production code uses the build-selected default ([`default_backend`] via
//! [`execution_providers`] / [`apply_execution_providers`]). The explicit
//! [`Backend`] variants exist so the benchmark suite can A/B the same
//! workload on CPU vs Core ML within a single process.

use ort::ep::ExecutionProviderDispatch;
use ort::session::builder::SessionBuilder;

/// Which ONNX Runtime execution backend to register for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// ONNX Runtime's built-in CPU provider (no extra EP registered).
    Cpu,
    /// Apple Core ML provider (Neural Engine + GPU via Metal + CPU). Only
    /// effective when compiled `--features coreml` on macOS; on any other
    /// target it degrades to the same behavior as [`Backend::Cpu`].
    CoreMl,
}

/// Whether the Core ML provider is actually compiled in and usable on this
/// target.
///
/// Lets benchmarks/diagnostics avoid misreporting a CPU-vs-CPU comparison as
/// CPU-vs-Core ML when built without `--features coreml` or off macOS.
pub fn coreml_available() -> bool {
    cfg!(all(feature = "coreml", target_os = "macos"))
}

/// The backend selected by build configuration: Core ML when compiled with
/// `--features coreml` on macOS, otherwise CPU.
pub fn default_backend() -> Backend {
    if coreml_available() {
        Backend::CoreMl
    } else {
        Backend::Cpu
    }
}

/// Execution providers for an explicit backend, in priority order.
///
/// Empty for [`Backend::Cpu`] (ONNX Runtime uses its built-in CPU provider),
/// and also empty for [`Backend::CoreMl`] on targets where Core ML is
/// unavailable, so the caller transparently runs on CPU.
pub fn providers_for(backend: Backend) -> Vec<ExecutionProviderDispatch> {
    #[cfg(all(feature = "coreml", target_os = "macos"))]
    {
        match backend {
            Backend::Cpu => Vec::new(),
            Backend::CoreMl => {
                use ort::ep::{coreml::ComputeUnits, CoreML};
                vec![CoreML::default()
                    .with_compute_units(ComputeUnits::All)
                    .build()]
            }
        }
    }
    #[cfg(not(all(feature = "coreml", target_os = "macos")))]
    {
        // Core ML is not compiled in / not on macOS: every backend runs on
        // the built-in CPU provider.
        let _ = backend;
        Vec::new()
    }
}

/// Execution providers for the build-selected default backend.
pub fn execution_providers() -> Vec<ExecutionProviderDispatch> {
    providers_for(default_backend())
}

/// Apply an explicit backend's execution providers to an `ort` session
/// builder.
///
/// Returns the builder unchanged when the backend registers no providers
/// (CPU, or Core ML on an unsupported target), preserving the plain
/// CPU-only path with zero behavior change.
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
