//! Shared test-isolation harness for the EngramDB workspace.
//!
//! Each workspace crate produces its **own** test binary, so the per-process
//! `#[ctor]` that redirects EngramDB's global data/config directories to
//! throwaway temp dirs must live somewhere every test binary links. This crate
//! is that home: add it as a `dev-dependency` and call [`arm`] once from the
//! crate's test module so the linker cannot dead-strip the constructor.
//!
//! # Why a `#[ctor]`
//!
//! `nextest` runs each test in its own process. The constructor runs once per
//! process, before any test, and points `ENGRAMDB_DATA_DIR` /
//! `ENGRAMDB_CONFIG_DIR` at per-process temp dirs so tests never touch the
//! real `~/Library/Application Support/engramdb/` directory, the user's config,
//! or the registry.
//!
//! The `TempDir` handles are held in statics so they persist for the life of
//! the process (and are cleaned up on exit).

use std::sync::LazyLock;

static TEST_DATA_DIR: LazyLock<tempfile::TempDir> =
    LazyLock::new(|| tempfile::TempDir::new().expect("failed to create test data dir"));
static TEST_CONFIG_DIR: LazyLock<tempfile::TempDir> =
    LazyLock::new(|| tempfile::TempDir::new().expect("failed to create test config dir"));
/// The dashboard's layout cache (`ENGRAMDB_DASHBOARD_CACHE_DIR`), which
/// otherwise lives under the user's real cache dir.
static TEST_DASHBOARD_CACHE_DIR: LazyLock<tempfile::TempDir> =
    LazyLock::new(|| tempfile::TempDir::new().expect("failed to create test dashboard cache dir"));

#[ctor::ctor(unsafe)]
fn init() {
    std::env::set_var("ENGRAMDB_DATA_DIR", TEST_DATA_DIR.path());
    std::env::set_var("ENGRAMDB_CONFIG_DIR", TEST_CONFIG_DIR.path());
    std::env::set_var(
        "ENGRAMDB_DASHBOARD_CACHE_DIR",
        TEST_DASHBOARD_CACHE_DIR.path().join("layouts"),
    );
}

/// Force the test-isolation constructor to be linked into the calling test
/// binary.
///
/// A `#[ctor]` in an rlib only runs if the linker pulls the object in; if
/// nothing references the crate, dead-code elimination can drop it and tests
/// would silently run against the real global directories. Call this once
/// (e.g. in a `#[test]` or a module-level reference) from every crate whose
/// tests touch EngramDB's data/config dirs.
#[inline(never)]
pub fn arm() {
    // Touching the statics guarantees the constructor's translation unit is
    // retained and the env vars are initialized even before `init` fires.
    let _ = TEST_DATA_DIR.path();
    let _ = TEST_CONFIG_DIR.path();
    let _ = TEST_DASHBOARD_CACHE_DIR.path();
}

/// Assert that this test binary runs isolated: both isolation variables are
/// set, and the data and config dirs the product resolved (`data_dir` and
/// `config_dir`, passed in by the caller from `paths::global_data_dir()` /
/// `paths::global_config_dir()`, since this leaf crate must not depend on
/// storage) are under the system temp dir.
///
/// Paths are compared canonicalized, never as text: `/tmp` is a symlink on
/// macOS, and the temp dir may be spelled relatively.
#[track_caller]
pub fn assert_isolated(data_dir: &std::path::Path, config_dir: &std::path::Path) {
    for (var, resolved) in [
        ("ENGRAMDB_DATA_DIR", data_dir),
        ("ENGRAMDB_CONFIG_DIR", config_dir),
    ] {
        let set = std::env::var_os(var).unwrap_or_else(|| {
            panic!("{var} is not set: this test binary does not link engram_test_support::arm()")
        });
        let canon = |p: &std::path::Path| {
            std::fs::canonicalize(p).unwrap_or_else(|e| panic!("canonicalize {}: {e}", p.display()))
        };
        let temp = canon(&std::env::temp_dir());
        let set = canon(std::path::Path::new(&set));
        let resolved = canon(resolved);
        assert_eq!(
            resolved, set,
            "{var}: the product resolved a different dir than the armed one"
        );
        assert!(
            resolved.starts_with(&temp),
            "{var} = {} is not under the temp dir {}",
            resolved.display(),
            temp.display()
        );
    }
}

/// Set to `1`/`true`/`yes`/`on` where the models are staged (the CI test jobs,
/// a sandbox with the models and the runtime in place): a model-backed test
/// then fails with the load error instead of passing by skipping.
///
/// Test-only: the product never reads it.
pub const REQUIRE_MODELS_ENV: &str = "ENGRAMDB_TEST_REQUIRE_MODELS";

/// Whether [`REQUIRE_MODELS_ENV`] is set to a truthy value.
pub fn models_required() -> bool {
    std::env::var(REQUIRE_MODELS_ENV)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// The outcome of a model load, as [`model_or_skip`] takes it: either the
/// model, or the reason it is not there.
pub trait ModelLoad<T> {
    fn into_load(self) -> Result<T, String>;
}

impl<T> ModelLoad<T> for Option<T> {
    fn into_load(self) -> Result<T, String> {
        self.ok_or_else(|| "the constructor returned None and kept no cause".to_string())
    }
}

impl<T, E: std::fmt::Display> ModelLoad<T> for Result<T, E> {
    fn into_load(self) -> Result<T, String> {
        self.map_err(|e| format!("{e:#}"))
    }
}

/// `Some(model)`, or `None` after printing
/// `SKIPPED (model unavailable): <what>: <cause>` — and a panic with the same
/// text when models are required ([`models_required`]).
#[track_caller]
pub fn model_or_skip<T>(what: &str, load: impl ModelLoad<T>) -> Option<T> {
    model_or_skip_with(models_required(), what, load)
}

/// [`model_or_skip`] with the requirement passed in (what its tests use, so
/// they never set the environment variable).
#[track_caller]
pub fn model_or_skip_with<T>(required: bool, what: &str, load: impl ModelLoad<T>) -> Option<T> {
    match load.into_load() {
        Ok(model) => Some(model),
        Err(cause) => {
            let msg = format!("SKIPPED (model unavailable): {what}: {cause}");
            if required {
                panic!("{msg} ({REQUIRE_MODELS_ENV} is set, so a missing model is a failure)");
            }
            eprintln!("{msg}");
            None
        }
    }
}

/// The model, or `return` from the test after [`model_or_skip`] reported
/// why. Use it for every test that needs a real model; under
/// `ENGRAMDB_TEST_REQUIRE_MODELS=1` a missing model fails the test.
#[macro_export]
macro_rules! require_model {
    ($what:expr, $load:expr) => {
        match $crate::model_or_skip($what, $load) {
            Some(m) => m,
            None => return,
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_or_skip_returns_the_model() {
        assert_eq!(model_or_skip_with(true, "m", Some(7)), Some(7));
        assert_eq!(
            model_or_skip_with(true, "m", Ok::<_, std::io::Error>(7)),
            Some(7)
        );
    }

    #[test]
    fn model_or_skip_skips_when_not_required() {
        assert_eq!(model_or_skip_with(false, "m", None::<u8>), None);
        assert_eq!(model_or_skip_with(false, "m", Err::<u8, _>("gone")), None);
    }

    #[test]
    #[should_panic(expected = "SKIPPED (model unavailable): m: no such file")]
    fn model_or_skip_panics_when_required() {
        let _ = model_or_skip_with(true, "m", Err::<u8, _>("no such file"));
    }
}
