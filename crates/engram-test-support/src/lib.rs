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
