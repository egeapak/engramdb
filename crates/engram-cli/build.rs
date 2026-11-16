//! Give the `engramdb` binary's main thread an 8 MiB stack on Windows.
//!
//! `main` runs the whole command on the main thread (`#[tokio::main]`
//! blocks on it there). Linux and macOS give that thread 8 MiB; Windows
//! gives it 1 MiB, and a debug build of `engramdb init` overflowed it
//! (`thread 'main' has overflowed its stack`, found by the Windows CI job).
//! The value matches the Unix default, so every platform runs the same
//! code with the same headroom. Only the binary is affected: the
//! `-bins` form leaves test and library targets alone.
fn main() {
    const STACK_BYTES: u32 = 8 * 1024 * 1024;
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" {
        if env == "msvc" {
            println!("cargo:rustc-link-arg-bins=/STACK:{STACK_BYTES}");
        } else {
            println!("cargo:rustc-link-arg-bins=-Wl,--stack,{STACK_BYTES}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");
}
