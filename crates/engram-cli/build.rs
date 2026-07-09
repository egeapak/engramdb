//! Build-time guidance for Intel Mac.
//!
//! Intel Mac (`x86_64-apple-darwin`) has no prebuilt ONNX Runtime 1.24 (the
//! version `fastembed`'s pinned `ort` requires), so a default build there links
//! an ONNX Runtime the crate cannot use — it crashes at first model load. The
//! pure-Rust `tract` backend (`--no-default-features --features tract`) is the
//! supported build for that target. Warn (don't fail) so a user who has hand-
//! built a working ORT 1.24 can still proceed.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    let onnx = std::env::var("CARGO_FEATURE_ONNXRUNTIME").is_ok();
    let tract = std::env::var("CARGO_FEATURE_TRACT").is_ok();
    if target == "x86_64-apple-darwin" && onnx && !tract {
        println!(
            "cargo:warning=Building EngramDB for Intel Mac (x86_64-apple-darwin) with the \
             ONNX Runtime backend. No prebuilt ONNX Runtime 1.24 exists for this target, so \
             the binary will fail at first model load. Rebuild with \
             `--no-default-features --features tract` for the pure-Rust backend."
        );
    }
}
