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

use ort::ep::ExecutionProviderDispatch;
use ort::session::builder::SessionBuilder;

/// Execution providers to register, in priority order.
///
/// Empty unless an accelerator feature is enabled for the current target,
/// in which case ONNX Runtime falls back to its built-in CPU provider.
pub fn execution_providers() -> Vec<ExecutionProviderDispatch> {
    #[cfg(all(feature = "coreml", target_os = "macos"))]
    {
        use ort::ep::{coreml::ComputeUnits, CoreML};
        vec![CoreML::default()
            .with_compute_units(ComputeUnits::All)
            .build()]
    }
    #[cfg(not(all(feature = "coreml", target_os = "macos")))]
    {
        Vec::new()
    }
}

/// Apply the configured execution providers to a directly-built `ort`
/// session builder.
///
/// Returns the builder unchanged when no accelerator is enabled, preserving
/// the existing CPU-only path with zero behavior change in default builds.
pub fn apply_execution_providers(builder: SessionBuilder) -> ort::Result<SessionBuilder> {
    let eps = execution_providers();
    if eps.is_empty() {
        Ok(builder)
    } else {
        builder.with_execution_providers(eps)
    }
}
