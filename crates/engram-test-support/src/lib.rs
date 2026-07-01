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

#[ctor::ctor(unsafe)]
fn init() {
    std::env::set_var("ENGRAMDB_DATA_DIR", TEST_DATA_DIR.path());
    std::env::set_var("ENGRAMDB_CONFIG_DIR", TEST_CONFIG_DIR.path());
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
}
