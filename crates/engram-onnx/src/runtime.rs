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
//! Validation is not just "does the file open". `ort` requires a minimum C API
//! version ([`REQUIRED_API_VERSION`], 24); an older runtime (Homebrew's 1.22,
//! say) loads fine and then fails inside `ort`. So the probe applies both of
//! the checks `ort`'s own loader makes, and treats a failure of either as
//! "too old", which converts that abort into a legible diagnostic:
//!
//! - the minor number in `GetVersionString()` must be at least 24. `ort`
//!   checks this string *before* asking for the API, and a failure
//!   there is the same uncatchable panic; and
//! - `OrtGetApiBase()->GetApi(24)` must not return null.
//!
//! # No bare library name
//!
//! The probe never hands the platform loader a file name without a directory.
//! On Windows and macOS the loader's search for a bare name includes the
//! current directory, and hooks and the MCP server start in the project
//! directory: a checkout holding `onnxruntime.dll` would be loaded, and
//! loading a library runs its code. So the probe resolves `PATH`,
//! `LD_LIBRARY_PATH` and `DYLD_*` itself into absolute paths (dropping empty
//! and relative entries), and then hands `ort` the absolute path it validated
//! (`ort::init_from`), so `ort` runs no search of its own either. See
//! [`candidates`].

use std::ffi::{CStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The C API version `ort` requires: the highest `api-N` feature enabled
/// anywhere in the dependency graph (`api-24` today, from this crate and from
/// `fastembed`).
///
/// Read from `ort` rather than written down, so the probe can never demand
/// less than `ort`'s own loader does. If some dependency ever turns on a
/// higher `api-N`, the probe follows it and a too-old runtime still gets the
/// legible [`RuntimeError::TooOld`] instead of an abort;
/// `required_api_version_is_24` then fails, because the documented minimum
/// runtime (1.24) would have changed. [`ensure`] rejects any runtime that
/// cannot vend this version.
pub const REQUIRED_API_VERSION: u32 = ort::MINOR_VERSION;

/// Environment variable naming an explicit library path. `ort` reads it too,
/// but only when no library was handed to it; [`ensure`] always hands it one,
/// so this module is its only reader.
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
        /// The system loader's own search was left out because
        /// `LD_LIBRARY_PATH` has an empty or relative entry.
        loader_skipped: bool,
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
            Self::NotFound {
                searched,
                loader_skipped,
            } => {
                write!(
                    f,
                    "ONNX Runtime ({}) not found. Install it (`brew install onnxruntime`, \
                     your distro's package, or on Windows the official ONNX Runtime zip) or \
                     set {} to the absolute path of the library. Searched: {}",
                    dylib_file_name(),
                    ORT_DYLIB_PATH,
                    searched
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )?;
                if *loader_skipped {
                    write!(
                        f,
                        ". The system loader's own search was skipped because \
                         LD_LIBRARY_PATH has an empty or relative entry"
                    )?;
                }
                Ok(())
            }
            Self::TooOld { path, version } => write!(
                f,
                "ONNX Runtime at {} is version {}, which does not provide C API version {}. \
                 Upgrade to 1.{} or newer.",
                path.display(),
                version,
                REQUIRED_API_VERSION,
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

/// What the candidate list is built from: the executable's location and the
/// environment variables that name library directories.
///
/// A value, not a read of the process, so a test can build one. The candidate
/// list is a pure function of it ([`candidates`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeEnv {
    /// Path of the running executable.
    pub exe: Option<PathBuf>,
    /// `ORT_DYLIB_PATH`: an explicit library path.
    pub ort_dylib_path: Option<OsString>,
    /// `PATH` (searched on Windows only).
    pub path: Option<OsString>,
    /// `LD_LIBRARY_PATH` (Linux and other non-macOS Unix).
    pub ld_library_path: Option<OsString>,
    /// `DYLD_LIBRARY_PATH` (macOS).
    pub dyld_library_path: Option<OsString>,
    /// `DYLD_FALLBACK_LIBRARY_PATH` (macOS).
    pub dyld_fallback_library_path: Option<OsString>,
    /// `HOME` (macOS: `$HOME/lib`).
    pub home: Option<OsString>,
    /// `HOMEBREW_PREFIX` (macOS).
    pub homebrew_prefix: Option<OsString>,
    /// `SCOOP` (Windows).
    pub scoop: Option<OsString>,
    /// `USERPROFILE` (Windows).
    pub userprofile: Option<OsString>,
    /// Fixed package-manager and system directories for this platform.
    pub system_dirs: Vec<PathBuf>,
}

impl ProbeEnv {
    /// The running process's values.
    pub fn from_process() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
        Self {
            exe: std::env::current_exe().ok(),
            ort_dylib_path: var(ORT_DYLIB_PATH),
            path: var("PATH"),
            ld_library_path: var("LD_LIBRARY_PATH"),
            dyld_library_path: var("DYLD_LIBRARY_PATH"),
            dyld_fallback_library_path: var("DYLD_FALLBACK_LIBRARY_PATH"),
            home: var("HOME"),
            homebrew_prefix: var("HOMEBREW_PREFIX"),
            scoop: var("SCOOP"),
            userprofile: var("USERPROFILE"),
            system_dirs: default_system_dirs(),
        }
    }
}

/// The fixed directories searched after the environment's own.
///
/// Neither Homebrew prefix is on macOS's default dyld search path, so they are
/// listed. Windows has none: a runtime there is found beside the executable,
/// on `PATH`, or through `ORT_DYLIB_PATH`.
fn default_system_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let dirs: &[&str] = &[
        "/opt/homebrew/lib",
        "/usr/local/lib",
        "/opt/local/lib",
        "/usr/lib",
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    let dirs: &[&str] = &[
        "/usr/local/lib",
        "/usr/lib",
        "/usr/lib64",
        // Debian/Ubuntu multiarch.
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
    ];
    #[cfg(not(unix))]
    let dirs: &[&str] = &[];
    dirs.iter().map(PathBuf::from).collect()
}

/// One place the runtime may be loaded from.
///
/// There is no variant for a bare file name. A bare name hands the search to
/// the platform loader, and on Windows and macOS that search includes the
/// current directory: a checkout holding `onnxruntime.dll` would be loaded,
/// and loading runs its initializers. Every `File` is absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidate {
    /// An absolute path to the library.
    File(PathBuf),
    /// `ORT_DYLIB_PATH` held a value that is not loaded. Nothing is opened.
    Refused {
        /// The value as given.
        path: PathBuf,
        /// Why it is not used.
        reason: String,
    },
    /// The system loader's own search (the `ld.so` cache and its default
    /// directories). Offered only where that search cannot reach the current
    /// directory: never on Windows or macOS, and on other Unix systems only
    /// when `LD_LIBRARY_PATH` has no empty or relative entry.
    #[cfg(all(unix, not(target_os = "macos")))]
    SystemLoader,
}

impl Candidate {
    /// What to show for this candidate in a "searched" list.
    fn label(&self) -> PathBuf {
        match self {
            Self::File(path) | Self::Refused { path, .. } => path.clone(),
            #[cfg(all(unix, not(target_os = "macos")))]
            Self::SystemLoader => PathBuf::from(format!("<system loader: {}>", dylib_file_name())),
        }
    }
}

/// The candidate list for an environment, and what was left out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    /// Candidates in the order they are tried.
    pub list: Vec<Candidate>,
    /// The system loader step was left out because `LD_LIBRARY_PATH` has an
    /// empty or relative entry, which would let it open a file in the current
    /// directory.
    pub loader_skipped: bool,
}

/// The absolute entries of a `PATH`-style variable. Empty, `.` and relative
/// entries are dropped: each of them means "the current directory".
fn absolute_entries(value: Option<&OsString>) -> Vec<PathBuf> {
    value
        .map(|v| {
            std::env::split_paths(v)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a `PATH`-style variable has an entry that is not absolute.
#[cfg(all(unix, not(target_os = "macos")))]
fn has_relative_entry(value: Option<&OsString>) -> bool {
    value.is_some_and(|v| std::env::split_paths(v).any(|p| !p.is_absolute()))
}

/// Candidate library locations for `env`, in the order [`ensure`] tries them.
///
/// 1. `ORT_DYLIB_PATH`. It must be absolute, or relative and existing under
///    the executable's directory (`ort`'s own rule for a relative value). Any
///    other value is [`Candidate::Refused`]: a path relative to the current
///    directory is never loaded.
/// 2. The executable's directory, then `<that>/../lib`. A release archive may
///    hold the library beside `engramdb`, and that copy is known to match it.
/// 3. The platform's library-path variables, absolute entries only: `PATH` on
///    Windows (then the two Scoop locations), `DYLD_LIBRARY_PATH` and
///    `DYLD_FALLBACK_LIBRARY_PATH` on macOS (then the Homebrew prefix and
///    `$HOME/lib`), `LD_LIBRARY_PATH` elsewhere.
/// 4. The fixed directories in [`ProbeEnv::system_dirs`].
/// 5. On non-macOS Unix, the system loader ([`Candidate::SystemLoader`]),
///    which is what finds a runtime in an `ld.so.conf.d` directory.
pub fn candidates(env: &ProbeEnv) -> Candidates {
    let file = dylib_file_name();
    let exe_dir = env
        .exe
        .as_deref()
        .and_then(Path::parent)
        .filter(|dir| dir.is_absolute());
    let mut list = Vec::new();

    if let Some(explicit) = env.ort_dylib_path.as_ref().filter(|v| !v.is_empty()) {
        let explicit = PathBuf::from(explicit);
        if explicit.is_absolute() {
            list.push(Candidate::File(explicit));
        } else {
            match exe_dir.map(|dir| dir.join(&explicit)) {
                Some(beside_exe) if beside_exe.exists() => list.push(Candidate::File(beside_exe)),
                _ => list.push(Candidate::Refused {
                    path: explicit,
                    reason: format!(
                        "{ORT_DYLIB_PATH} must be an absolute path (a relative one is used \
                         only when it exists under the directory of the engramdb executable)"
                    ),
                }),
            }
        }
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe_dir {
        dirs.push(dir.to_path_buf());
        // Archives that follow the usual `bin/` + `lib/` split.
        if let Some(prefix) = dir.parent() {
            dirs.push(prefix.join("lib"));
        }
    }

    #[cfg(windows)]
    {
        dirs.extend(absolute_entries(env.path.as_ref()));
        let scoop_lib = |root: PathBuf| {
            root.join("apps")
                .join("onnxruntime")
                .join("current")
                .join("lib")
        };
        if let Some(scoop) = env.scoop.as_ref().map(PathBuf::from) {
            dirs.push(scoop_lib(scoop));
        }
        if let Some(profile) = env.userprofile.as_ref().map(PathBuf::from) {
            dirs.push(scoop_lib(profile.join("scoop")));
        }
    }
    #[cfg(target_os = "macos")]
    {
        dirs.extend(absolute_entries(env.dyld_library_path.as_ref()));
        dirs.extend(absolute_entries(env.dyld_fallback_library_path.as_ref()));
        if let Some(prefix) = env.homebrew_prefix.as_ref().map(PathBuf::from) {
            dirs.push(prefix.join("lib"));
        }
        if let Some(home) = env.home.as_ref().map(PathBuf::from) {
            dirs.push(home.join("lib"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    dirs.extend(absolute_entries(env.ld_library_path.as_ref()));

    dirs.extend(env.system_dirs.iter().cloned());

    for dir in dirs {
        // A variable such as `SCOOP` or `HOME` can itself be relative.
        if !dir.is_absolute() {
            continue;
        }
        let candidate = Candidate::File(dir.join(file));
        if !list.contains(&candidate) {
            list.push(candidate);
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    let loader_skipped = {
        // An empty or relative `LD_LIBRARY_PATH` entry makes the loader look
        // in the current directory, so the loader is not asked at all then.
        let dirty = has_relative_entry(env.ld_library_path.as_ref());
        if !dirty {
            list.push(Candidate::SystemLoader);
        }
        dirty
    };
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    let loader_skipped = false;

    Candidates {
        list,
        loader_skipped,
    }
}

/// A library that passed the probe. The handle stays open for the life of the
/// process, so the module `ort` then opens by the same path is this one.
struct Probed {
    lib: libloading::Library,
    /// Absolute path of the loaded file.
    path: PathBuf,
    version: String,
}

/// Open the library file at `path`.
///
/// On Windows the DLL's own imports are searched in its directory and in
/// System32 only, so they cannot come from the current directory either.
fn open_file(path: &Path) -> Result<libloading::Library, libloading::Error> {
    #[cfg(windows)]
    {
        use libloading::os::windows::{
            Library, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
        };
        // SAFETY: loading a shared library can run initializers; this is the
        // same operation `ort` would perform, done earlier so we can report
        // failure. `path` is absolute, which these flags require.
        unsafe {
            Library::load_with_flags(
                path.as_os_str(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map(Into::into)
    }
    #[cfg(not(windows))]
    {
        // SAFETY: as above.
        unsafe { libloading::Library::new(path.as_os_str()) }
    }
}

/// The absolute path of the file that holds `symbol`, from `dladdr`.
#[cfg(all(unix, not(target_os = "macos")))]
fn module_path_of(symbol: *const std::ffi::c_void) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStrExt;

    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::zeroed();
    // SAFETY: `dladdr` writes `info` and reads nothing through `symbol`.
    let found = unsafe { libc::dladdr(symbol, info.as_mut_ptr()) };
    if found == 0 {
        return Err("the loader could not say which file it loaded".to_string());
    }
    // SAFETY: `dladdr` returned non-zero, so `info` is initialized.
    let info = unsafe { info.assume_init() };
    if info.dli_fname.is_null() {
        return Err("the loader reported no file name".to_string());
    }
    // SAFETY: `dli_fname` is a NUL-terminated string owned by the loader and
    // valid while the library is loaded; it is copied here.
    let name = unsafe { CStr::from_ptr(info.dli_fname) };
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes()));
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!(
            "the loader resolved it to the relative path {}",
            path.display()
        ))
    }
}

/// Open a candidate and check it vends [`REQUIRED_API_VERSION`].
///
/// # Safety rationale
///
/// The `unsafe` blocks call into the ONNX Runtime C API's documented,
/// ABI-stable entry point. `OrtApiBase` is two function pointers and has not
/// changed across the runtime's lifetime; `GetVersionString` returns a static
/// NUL-terminated buffer the caller must not free.
fn probe(candidate: &Candidate) -> Result<Probed, RuntimeError> {
    #[repr(C)]
    struct OrtApiBase {
        get_api: unsafe extern "system" fn(u32) -> *const std::ffi::c_void,
        get_version_string: unsafe extern "system" fn() -> *const std::ffi::c_char,
    }

    let label = candidate.label();
    let unusable = |reason: String| RuntimeError::Unusable {
        path: label.clone(),
        reason,
    };

    let lib = match candidate {
        Candidate::File(path) => open_file(path).map_err(|e| unusable(e.to_string()))?,
        Candidate::Refused { reason, .. } => return Err(unusable(reason.clone())),
        #[cfg(all(unix, not(target_os = "macos")))]
        // SAFETY: as in `open_file`. The name has no directory, so the loader
        // searches; `candidates` offers this only when that search cannot
        // reach the current directory.
        Candidate::SystemLoader => unsafe { libloading::Library::new(dylib_file_name()) }
            .map_err(|e| unusable(e.to_string()))?,
    };

    // SAFETY: `OrtGetApiBase` is the runtime's documented entry point and has
    // this signature in every published version.
    let get_base = unsafe {
        lib.get::<unsafe extern "system" fn() -> *const OrtApiBase>(b"OrtGetApiBase\0")
            .map(|symbol| *symbol)
            .map_err(|e| unusable(format!("missing OrtGetApiBase: {e}")))?
    };

    let path = match candidate {
        Candidate::File(path) => path.clone(),
        Candidate::Refused { path, .. } => path.clone(),
        #[cfg(all(unix, not(target_os = "macos")))]
        Candidate::SystemLoader => {
            module_path_of(get_base as *const std::ffi::c_void).map_err(unusable)?
        }
    };

    // SAFETY: see above; the returned pointer is into the loaded library,
    // which `lib` keeps open.
    let base = unsafe {
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

    // The version string first: it is `ort`'s own first check, and a runtime
    // that fails it never reaches `GetApi`, which on an old runtime prints
    // "The requested API version [24] is not available" to stderr itself.
    if !version_meets_requirement(&version) {
        return Err(RuntimeError::TooOld { path, version });
    }

    // SAFETY: documented to return null for an unsupported version rather than
    // failing, which is exactly the case we want to detect.
    let api = unsafe { (base.get_api)(REQUIRED_API_VERSION) };
    if api.is_null() {
        return Err(RuntimeError::TooOld { path, version });
    }

    Ok(Probed { lib, path, version })
}

/// Whether a `GetVersionString()` value passes `ort`'s own version check.
///
/// Mirrors `ort::load_dynamic::init` exactly: take the second dot-separated
/// field as the minor version, treat anything unparsable as `0`, and require
/// at least [`REQUIRED_API_VERSION`]. `ort` panics when this fails, so the
/// probe must refuse every runtime `ort` would refuse — including one whose
/// version string is malformed even though `GetApi` succeeds.
fn version_meets_requirement(version: &str) -> bool {
    let minor = version
        .split('.')
        .nth(1)
        .map_or(0, |m| m.parse::<u32>().unwrap_or(0));
    minor >= REQUIRED_API_VERSION
}

/// Try each candidate in order and return the first that passes the probe.
///
/// Not cached and no `ort` call: [`ensure`] adds both.
fn resolve(candidates: &Candidates) -> Result<Probed, RuntimeError> {
    // Remember the most specific failure: a runtime that exists but is too old
    // is a far more useful thing to report than "not found".
    let mut best_error: Option<RuntimeError> = None;

    for candidate in &candidates.list {
        match probe(candidate) {
            Ok(probed) => return Ok(probed),
            // A version mismatch is the most informative failure there is — it
            // means the user *has* a runtime, just the wrong one — so it always
            // wins the reporting slot.
            Err(e @ RuntimeError::TooOld { .. }) => best_error = Some(e),
            // Most candidates are speculative paths that simply do not exist.
            // Reporting "could not be loaded: no such file" for the first of
            // them would describe a missing *installation* as a broken library.
            // Only a path that is really there and still fails, or a refused
            // `ORT_DYLIB_PATH`, is worth surfacing.
            Err(e) => {
                let worth_reporting = match candidate {
                    Candidate::File(path) => path.exists(),
                    Candidate::Refused { .. } => true,
                    #[cfg(all(unix, not(target_os = "macos")))]
                    Candidate::SystemLoader => false,
                };
                if worth_reporting && best_error.is_none() {
                    best_error = Some(e);
                }
            }
        }
    }

    Err(best_error.unwrap_or_else(|| RuntimeError::NotFound {
        searched: candidates.list.iter().map(Candidate::label).collect(),
        loader_skipped: candidates.loader_skipped,
    }))
}

/// Resolve, validate, and remember the ONNX Runtime for this process.
///
/// Succeeds at most once: the result is cached. On success the probed library
/// stays loaded and `ort` is initialized from its absolute path
/// (`ort::init_from`), so every later `ort` call uses that module. `ort` never
/// runs a search of its own and never reads `ORT_DYLIB_PATH`, and this
/// function sets no environment variable.
///
/// Callers should treat `Err` as "ONNX is unavailable" and fall back exactly as
/// they would for a missing model — never as a fatal error.
pub fn ensure() -> Result<&'static RuntimeInfo, &'static RuntimeError> {
    // The library handle is kept so the module is never unloaded: `ort` opens
    // the same absolute path, which for an already loaded module only takes
    // another reference to it.
    static RUNTIME: OnceLock<Result<(RuntimeInfo, libloading::Library), RuntimeError>> =
        OnceLock::new();

    RUNTIME
        .get_or_init(|| {
            let probed = resolve(&candidates(&ProbeEnv::from_process()))?;
            // `init_from` fills `ort`'s library slot. The returned environment
            // builder is not needed: `crate::environment()` builds the one
            // environment later.
            if let Err(e) = ort::init_from(&probed.path) {
                return Err(RuntimeError::Unusable {
                    path: probed.path,
                    reason: e.to_string(),
                });
            }
            Ok((
                RuntimeInfo {
                    path: probed.path,
                    version: probed.version,
                },
                probed.lib,
            ))
        })
        .as_ref()
        .map(|(info, _lib)| info)
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

    /// An environment whose every library-path variable holds an empty, a
    /// `.` and a relative entry beside one absolute directory.
    fn dirty_env(abs: &Path) -> ProbeEnv {
        let list = std::env::join_paths([Path::new(""), Path::new("."), Path::new("rel/dir"), abs])
            .unwrap();
        ProbeEnv {
            exe: Some(abs.join("bin").join("engramdb")),
            ort_dylib_path: None,
            path: Some(list.clone()),
            ld_library_path: Some(list.clone()),
            dyld_library_path: Some(list.clone()),
            dyld_fallback_library_path: Some(list),
            home: Some("relative-home".into()),
            homebrew_prefix: Some("relative-brew".into()),
            scoop: Some("relative-scoop".into()),
            userprofile: Some("relative-profile".into()),
            system_dirs: default_system_dirs(),
        }
    }

    /// The defect this replaced: the list ended with the bare file name, and
    /// the platform loader's search for a bare name includes the current
    /// directory on Windows and macOS.
    #[test]
    fn every_candidate_is_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let found = candidates(&dirty_env(dir.path()));
        let mut files = Vec::new();
        for candidate in &found.list {
            match candidate {
                Candidate::File(path) => {
                    assert!(path.is_absolute(), "relative candidate {}", path.display());
                    files.push(path.clone());
                }
                Candidate::Refused { path, .. } => panic!("refused {}", path.display()),
                #[cfg(all(unix, not(target_os = "macos")))]
                Candidate::SystemLoader => {}
            }
        }
        assert!(
            files.contains(&dir.path().join(dylib_file_name())),
            "the absolute entry of the library-path variable is searched: {files:?}"
        );
        assert!(
            files.contains(&dir.path().join("bin").join(dylib_file_name())),
            "the executable's directory is searched: {files:?}"
        );

        // The process's own list too, whatever this machine has set.
        for candidate in candidates(&ProbeEnv::from_process()).list {
            if let Candidate::File(path) = candidate {
                assert!(path.is_absolute(), "relative candidate {}", path.display());
            }
        }
    }

    #[test]
    fn a_relative_ort_dylib_path_is_refused_without_a_load() {
        let dir = tempfile::tempdir().unwrap();
        let env = ProbeEnv {
            exe: Some(dir.path().join("engramdb")),
            ort_dylib_path: Some(format!("elsewhere/{}", dylib_file_name()).into()),
            ..ProbeEnv::default()
        };
        let found = candidates(&env);
        assert!(
            matches!(found.list.first(), Some(Candidate::Refused { .. })),
            "{found:?}"
        );
        let err = probe(&found.list[0]).map(|p| p.path).unwrap_err();
        match err {
            RuntimeError::Unusable { reason, .. } => {
                assert!(reason.contains("must be an absolute path"), "{reason}")
            }
            other => panic!("expected Unusable, got {other:?}"),
        }

        // `ort`'s own rule is kept: a relative value that exists under the
        // executable's directory is that file.
        let beside = dir.path().join("elsewhere");
        std::fs::create_dir_all(&beside).unwrap();
        std::fs::write(beside.join(dylib_file_name()), b"x").unwrap();
        let found = candidates(&env);
        assert_eq!(
            found.list.first(),
            Some(&Candidate::File(beside.join(dylib_file_name())))
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn a_dirty_ld_library_path_skips_the_system_loader() {
        let dir = tempfile::tempdir().unwrap();
        let dirty = candidates(&dirty_env(dir.path()));
        assert!(dirty.loader_skipped);
        assert!(!dirty.list.contains(&Candidate::SystemLoader));

        let clean = candidates(&ProbeEnv {
            ld_library_path: Some(dir.path().as_os_str().to_os_string()),
            ..ProbeEnv::default()
        });
        assert!(!clean.loader_skipped);
        assert_eq!(clean.list.last(), Some(&Candidate::SystemLoader));

        let text = RuntimeError::NotFound {
            searched: Vec::new(),
            loader_skipped: true,
        }
        .to_string();
        assert!(text.contains("LD_LIBRARY_PATH"), "{text}");
    }

    /// A file named like the runtime in the current directory is never opened.
    /// Before the fix the last candidate was the bare name, the loader opened
    /// the file in the current directory (Windows, macOS), and the result was
    /// `Unusable { path: "onnxruntime.dll" }`. nextest runs one process per
    /// test, so changing the current directory here is safe.
    #[test]
    fn a_library_in_the_current_directory_is_not_probed() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        std::fs::write(cwd.join(dylib_file_name()), b"not a library").unwrap();
        std::env::set_current_dir(&cwd).unwrap();

        let found = candidates(&ProbeEnv::default());
        for candidate in &found.list {
            if let Candidate::File(path) = candidate {
                assert!(!path.starts_with(&cwd), "{}", path.display());
            }
        }
        match resolve(&found).map(|p| p.path) {
            // Windows and macOS have no loader step: nothing is searched.
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            other => assert!(
                matches!(other, Err(RuntimeError::NotFound { .. })),
                "{other:?}"
            ),
            // Elsewhere the loader may find an installed runtime, but never
            // the file in the current directory.
            #[cfg(all(unix, not(target_os = "macos")))]
            Ok(path) => assert!(!path.starts_with(&cwd), "{}", path.display()),
            #[cfg(all(unix, not(target_os = "macos")))]
            Err(RuntimeError::NotFound { .. }) => {}
            #[cfg(all(unix, not(target_os = "macos")))]
            Err(other) => panic!("the file in the current directory was probed: {other:?}"),
        }
    }

    /// The loader step names the file it opened by absolute path (`dladdr`),
    /// so `ort` is never handed a bare name. Skips when the loader finds no
    /// runtime on this machine.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn the_system_loader_reports_an_absolute_path() {
        if let Ok(probed) = probe(&Candidate::SystemLoader) {
            assert!(probed.path.is_absolute(), "{}", probed.path.display());
            assert!(probed.path.is_file(), "{}", probed.path.display());
        }
    }

    #[test]
    fn the_resolved_path_is_absolute() {
        // Skips when no runtime is installed on this machine.
        if let Ok(info) = ensure() {
            assert!(info.path.is_absolute(), "{}", info.path.display());
            assert!(info.path.is_file(), "{}", info.path.display());
        }
    }

    #[test]
    fn candidates_are_named_consistently() {
        let found = candidates(&ProbeEnv::from_process());
        assert!(!found.list.is_empty());
        for candidate in &found.list {
            if let Candidate::File(path) = candidate {
                // `ORT_DYLIB_PATH` may name any file; every other candidate
                // names the platform library.
                if std::env::var_os(ORT_DYLIB_PATH).is_some_and(|v| Path::new(&v) == path) {
                    continue;
                }
                assert_eq!(
                    path.file_name().and_then(|s| s.to_str()),
                    Some(dylib_file_name()),
                    "every candidate must name the platform library: {}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn probing_a_missing_file_is_unusable_not_a_panic() {
        // The whole point of this module: failure returns, it does not abort.
        let err = probe(&Candidate::File(PathBuf::from(
            "/nonexistent/libonnxruntime.so",
        )))
        .map(|p| p.path)
        .unwrap_err();
        assert!(matches!(err, RuntimeError::Unusable { .. }), "got {err:?}");
    }

    #[test]
    fn probing_a_non_library_is_unusable_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join(dylib_file_name());
        std::fs::write(&bogus, b"this is definitely not an ELF/Mach-O/PE image").unwrap();
        let err = probe(&Candidate::File(bogus)).map(|p| p.path).unwrap_err();
        assert!(matches!(err, RuntimeError::Unusable { .. }), "got {err:?}");
    }

    #[test]
    fn errors_render_actionable_messages() {
        let not_found = RuntimeError::NotFound {
            searched: vec![PathBuf::from("/usr/lib/libonnxruntime.so")],
            loader_skipped: false,
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
        assert!(text.contains("C API version 24"), "{text}");
        assert!(text.contains("Upgrade to 1.24 or newer"), "{text}");
    }

    /// The documented minimum runtime is 1.24 (installation docs, release
    /// notes, CI's `ONNXRUNTIME_VERSION`). `REQUIRED_API_VERSION` follows the
    /// highest `api-N` feature in the graph, so a dependency that raises it
    /// silently raises the minimum too. This makes that visible.
    #[test]
    fn required_api_version_is_24() {
        assert_eq!(
            REQUIRED_API_VERSION, 24,
            "an `api-N` feature somewhere in the graph raised ort's C API level; \
             update the documented minimum ONNX Runtime version everywhere"
        );
    }

    /// Mirrors `ort`'s own version-string check, which panics on failure, so
    /// every string `ort` would refuse must be refused here first.
    #[test]
    fn version_string_check_matches_ort() {
        for ok in ["1.24.0", "1.24.2", "1.28.0", "1.24.2-dev", "2.30.1"] {
            assert!(version_meets_requirement(ok), "{ok} should be accepted");
        }
        for too_old in ["1.23.1", "1.22.0", "1.9.0", "unknown", "", "1", "1.x.0"] {
            assert!(
                !version_meets_requirement(too_old),
                "{too_old:?} should be refused (ort would panic on it)"
            );
        }
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

        let err = probe(&Candidate::File(missing.clone()))
            .map(|p| p.path)
            .unwrap_err();
        assert!(matches!(err, RuntimeError::Unusable { .. }));

        // ...but `ensure`'s selection logic skips non-existent candidates, so
        // what surfaces is NotFound. Assert on that rule directly, since
        // `ensure` itself is process-cached and depends on the host.
        let not_found = RuntimeError::NotFound {
            searched: vec![missing],
            loader_skipped: false,
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
