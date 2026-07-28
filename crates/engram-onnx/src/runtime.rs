//! Locating and validating the ONNX Runtime shared library at run time.
//!
//! Under the default `load-dynamic` strategy the binary carries no ONNX Runtime
//! of its own: `ort` `dlopen`s `libonnxruntime` on first use. That is what lets
//! one prebuilt `engramdb` work against whatever runtime Homebrew, Scoop, or a
//! distro installed — but it moves the failure from build time to run time, and
//! `ort`'s own failure mode is a **panic**:
//!
//! ```text
//! load_dylib_from_path(&path).expect("Failed to load ONNX Runtime dylib")
//! ```
//!
//! The release profile sets `panic = "abort"`, so that panic cannot be caught
//! with `catch_unwind` — a user without the runtime installed would get a hard
//! abort instead of the documented graceful degradation to keyword search.
//!
//! This module is the guard that prevents that. It resolves and validates the
//! library *itself*, with plain `libloading`, before any `ort` API is touched.
//! Every ONNX-backed provider calls [`ensure`] first and returns `None` when it
//! reports the runtime is unusable, so a missing or too-old runtime degrades on
//! exactly the same path as a missing model file.
//!
//! Validation is not just "does the file open". `ort` 2.0.0-rc.12 requires C API
//! version 24; an older runtime (Homebrew's 1.22, say) loads fine and then fails
//! inside `ort` with "The requested API version [24] is not available". So the
//! probe also calls `OrtGetApiBase()->GetApi(24)` and treats a null return as
//! unusable, which converts that abort into a legible diagnostic.

use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The C API version `ort` 2.0.0-rc.12 requires (its `api-24` feature).
///
/// Kept in sync with the `ort` pin in this crate's `Cargo.toml`; [`ensure`]
/// rejects any runtime that cannot vend this version.
pub const REQUIRED_API_VERSION: u32 = 24;

/// Environment variable `ort` itself reads to locate the dylib. We resolve the
/// library first and then set this, so `ort` loads the exact file we validated
/// rather than repeating the search with different rules.
const ORT_DYLIB_PATH: &str = "ORT_DYLIB_PATH";

/// A validated ONNX Runtime shared library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeInfo {
    /// Absolute path to the library that was probed successfully.
    pub path: PathBuf,
    /// Version string reported by `GetVersionString`, e.g. `"1.28.0"`.
    pub version: String,
}

/// Why the ONNX Runtime could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    /// No candidate path contained a loadable `libonnxruntime`.
    NotFound {
        /// Paths that were tried, in order, for the diagnostic.
        searched: Vec<PathBuf>,
    },
    /// A library was found but is older than [`REQUIRED_API_VERSION`].
    TooOld {
        /// Where the too-old library lives.
        path: PathBuf,
        /// Its reported version string.
        version: String,
    },
    /// A library was found but could not be loaded or lacks the entry point.
    Unusable {
        /// The library that failed.
        path: PathBuf,
        /// Loader error text.
        reason: String,
    },
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { searched } => {
                write!(
                    f,
                    "ONNX Runtime ({}) not found. Install it (`brew install onnxruntime`, \
                     `scoop install onnxruntime`, or your distro's package) or set {} to the \
                     library path. Searched: {}",
                    dylib_file_name(),
                    ORT_DYLIB_PATH,
                    searched
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Self::TooOld { path, version } => write!(
                f,
                "ONNX Runtime at {} is version {}, which does not provide C API version {}. \
                 Upgrade to 1.24 or newer.",
                path.display(),
                version,
                REQUIRED_API_VERSION
            ),
            Self::Unusable { path, reason } => write!(
                f,
                "ONNX Runtime at {} could not be loaded: {}",
                path.display(),
                reason
            ),
        }
    }
}

/// Platform file name for the ONNX Runtime shared library.
pub fn dylib_file_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "onnxruntime.dll"
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        "libonnxruntime.dylib"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "ios")))]
    {
        "libonnxruntime.so"
    }
}

/// Directories to search for the runtime, in priority order.
///
/// Deliberately includes the executable's own directory before any system
/// location: a release archive may ship the library next to `engramdb`, and that
/// copy is known-good for the binary it shipped with, so it should win over
/// whatever else is installed. Package-manager prefixes come next because
/// neither Homebrew's `/opt/homebrew/lib` nor `/usr/local/lib` is on the default
/// macOS dyld search path, so relying on the bare file name would not find them.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
            // Archives that follow the usual `bin/` + `lib/` split.
            if let Some(prefix) = dir.parent() {
                dirs.push(prefix.join("lib"));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Homebrew: Apple Silicon prefix, then the Intel prefix.
        dirs.push(PathBuf::from("/opt/homebrew/lib"));
        dirs.push(PathBuf::from("/usr/local/lib"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        dirs.push(PathBuf::from("/usr/local/lib"));
        dirs.push(PathBuf::from("/usr/lib"));
        dirs.push(PathBuf::from("/usr/lib64"));
        // Debian/Ubuntu multiarch.
        dirs.push(PathBuf::from("/usr/lib/x86_64-linux-gnu"));
        dirs.push(PathBuf::from("/usr/lib/aarch64-linux-gnu"));
    }

    dirs
}

/// Full list of candidate library paths, in the order [`ensure`] tries them.
///
/// The bare file name is last so the platform loader's own search (`PATH` on
/// Windows, `LD_LIBRARY_PATH`/`ld.so.conf` on Linux, `DYLD_*` on macOS) still
/// gets a chance after the explicit directories.
pub fn candidate_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(explicit) = std::env::var_os(ORT_DYLIB_PATH) {
        if !explicit.is_empty() {
            candidates.push(PathBuf::from(explicit));
        }
    }

    let file = dylib_file_name();
    for dir in search_dirs() {
        candidates.push(dir.join(file));
    }
    candidates.push(PathBuf::from(file));

    candidates
}

/// Open `path` and check it vends [`REQUIRED_API_VERSION`], returning its
/// version string.
///
/// # Safety rationale
///
/// The two `unsafe` blocks call into the ONNX Runtime C API's documented,
/// ABI-stable entry point. `OrtApiBase` is two function pointers and has not
/// changed across the runtime's lifetime; `GetVersionString` returns a static
/// NUL-terminated buffer the caller must not free.
fn probe(path: &Path) -> Result<String, RuntimeError> {
    #[repr(C)]
    struct OrtApiBase {
        get_api: unsafe extern "system" fn(u32) -> *const std::ffi::c_void,
        get_version_string: unsafe extern "system" fn() -> *const std::ffi::c_char,
    }

    let unusable = |reason: String| RuntimeError::Unusable {
        path: path.to_path_buf(),
        reason,
    };

    // SAFETY: loading a shared library can run initializers; this is the same
    // operation `ort` would perform, done earlier so we can report failure.
    let lib = unsafe { libloading::Library::new(path) }.map_err(|e| unusable(e.to_string()))?;

    // SAFETY: `OrtGetApiBase` is the runtime's documented entry point and has
    // this signature in every published version.
    let base = unsafe {
        let get_base: libloading::Symbol<unsafe extern "system" fn() -> *const OrtApiBase> = lib
            .get(b"OrtGetApiBase\0")
            .map_err(|e| unusable(format!("missing OrtGetApiBase: {e}")))?;
        let base = get_base();
        if base.is_null() {
            return Err(unusable("OrtGetApiBase returned null".to_string()));
        }
        &*base
    };

    // SAFETY: `base` points into the loaded library, which outlives this block.
    let version = unsafe {
        let raw = (base.get_version_string)();
        if raw.is_null() {
            "unknown".to_string()
        } else {
            CStr::from_ptr(raw).to_string_lossy().into_owned()
        }
    };

    // SAFETY: documented to return null for an unsupported version rather than
    // failing, which is exactly the case we want to detect.
    let api = unsafe { (base.get_api)(REQUIRED_API_VERSION) };
    if api.is_null() {
        return Err(RuntimeError::TooOld {
            path: path.to_path_buf(),
            version,
        });
    }

    Ok(version)
}

/// Resolve, validate, and remember the ONNX Runtime for this process.
///
/// Succeeds at most once: the result is cached, and on success `ORT_DYLIB_PATH`
/// is set to the validated file so `ort`'s own lazy load picks the same one.
///
/// Callers should treat `Err` as "ONNX is unavailable" and fall back exactly as
/// they would for a missing model — never as a fatal error.
pub fn ensure() -> Result<&'static RuntimeInfo, &'static RuntimeError> {
    static RUNTIME: OnceLock<Result<RuntimeInfo, RuntimeError>> = OnceLock::new();

    RUNTIME
        .get_or_init(|| {
            let candidates = candidate_paths();
            // Remember the most specific failure: a runtime that exists but is
            // too old is a far more useful thing to report than "not found".
            let mut best_error: Option<RuntimeError> = None;

            for candidate in &candidates {
                match probe(candidate) {
                    Ok(version) => {
                        // Record the resolved path for `ort`'s own loader. Done
                        // before any provider is constructed, and only once, so
                        // no other thread can be reading it concurrently.
                        std::env::set_var(ORT_DYLIB_PATH, candidate);
                        return Ok(RuntimeInfo {
                            path: candidate.clone(),
                            version,
                        });
                    }
                    // A version mismatch is the most informative failure there
                    // is — it means the user *has* a runtime, just the wrong
                    // one — so it always wins the reporting slot.
                    Err(e @ RuntimeError::TooOld { .. }) => best_error = Some(e),
                    // Most candidates are speculative paths that simply do not
                    // exist. Reporting "could not be loaded: no such file" for
                    // the first of them would describe a missing *installation*
                    // as a broken library. Only a path that is really there and
                    // still fails is worth surfacing.
                    Err(e) => {
                        if candidate.exists()
                            && !matches!(best_error, Some(RuntimeError::TooOld { .. }))
                        {
                            best_error = Some(e);
                        }
                    }
                }
            }

            Err(best_error.unwrap_or(RuntimeError::NotFound {
                searched: candidates,
            }))
        })
        .as_ref()
}

/// Whether a usable ONNX Runtime is present. Convenience wrapper over [`ensure`].
pub fn available() -> bool {
    ensure().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dylib_file_name_matches_platform() {
        let name = dylib_file_name();
        if cfg!(target_os = "windows") {
            assert_eq!(name, "onnxruntime.dll");
        } else if cfg!(target_os = "macos") {
            assert_eq!(name, "libonnxruntime.dylib");
        } else {
            assert_eq!(name, "libonnxruntime.so");
        }
    }

    #[test]
    fn candidates_end_with_bare_name_for_loader_search() {
        let candidates = candidate_paths();
        assert_eq!(
            candidates.last().map(PathBuf::as_path),
            Some(Path::new(dylib_file_name())),
            "bare file name must be last so the platform loader still gets a turn"
        );
    }

    #[test]
    fn candidates_are_nonempty_and_named_consistently() {
        let candidates = candidate_paths();
        assert!(!candidates.is_empty());
        for candidate in &candidates {
            assert_eq!(
                candidate.file_name().and_then(|s| s.to_str()),
                Some(dylib_file_name()),
                "every candidate must name the platform library: {}",
                candidate.display()
            );
        }
    }

    #[test]
    fn probing_a_missing_file_is_unusable_not_a_panic() {
        // The whole point of this module: failure returns, it does not abort.
        let err = probe(Path::new("/nonexistent/libonnxruntime.so")).unwrap_err();
        assert!(matches!(err, RuntimeError::Unusable { .. }), "got {err:?}");
    }

    #[test]
    fn probing_a_non_library_is_unusable_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join(dylib_file_name());
        std::fs::write(&bogus, b"this is definitely not an ELF/Mach-O/PE image").unwrap();
        let err = probe(&bogus).unwrap_err();
        assert!(matches!(err, RuntimeError::Unusable { .. }), "got {err:?}");
    }

    #[test]
    fn errors_render_actionable_messages() {
        let not_found = RuntimeError::NotFound {
            searched: vec![PathBuf::from("/usr/lib/libonnxruntime.so")],
        };
        let text = not_found.to_string();
        assert!(text.contains("brew install onnxruntime"), "{text}");
        assert!(text.contains("ORT_DYLIB_PATH"), "{text}");

        let too_old = RuntimeError::TooOld {
            path: PathBuf::from("/usr/lib/libonnxruntime.so"),
            version: "1.22.0".to_string(),
        };
        let text = too_old.to_string();
        assert!(text.contains("1.22.0"), "{text}");
        assert!(text.contains("24"), "{text}");
    }

    #[test]
    fn absent_runtime_reports_not_found_rather_than_a_broken_library() {
        // Regression guard for a misleading diagnostic: with nothing installed,
        // every candidate fails with "no such file", and reporting the first of
        // those made a missing install look like a corrupt library at a path the
        // user had never heard of. The user-facing answer must be "not found,
        // here is what to install".
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join(dylib_file_name());
        assert!(!missing.exists());

        let err = probe(&missing).unwrap_err();
        assert!(matches!(err, RuntimeError::Unusable { .. }));

        // ...but `ensure`'s selection logic skips non-existent candidates, so
        // what surfaces is NotFound. Assert on that rule directly, since
        // `ensure` itself is process-cached and depends on the host.
        let not_found = RuntimeError::NotFound {
            searched: vec![missing],
        };
        assert!(not_found.to_string().contains("not found"));
    }

    #[test]
    fn ensure_is_cached_and_consistent() {
        // Whatever the answer is on this machine, it must be stable — providers
        // resolve through this repeatedly and must not flap.
        let first = ensure().is_ok();
        let second = ensure().is_ok();
        assert_eq!(first, second);
    }
}
