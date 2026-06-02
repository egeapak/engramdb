//! Test-only support utilities shared across the crate.
//!
//! # Why this exists
//!
//! Tests live in a single process (`cargo test` default harness) and share a
//! single `ENGRAMDB_DATA_DIR` (set in `src/lib.rs::test_isolation`). The
//! global memory store therefore resolves to one on-disk location for every
//! test in the process. When multiple tests call `MemoryStore::init_global`
//! (or any MCP entrypoint that routes to `project: "global"`), they race on:
//!
//! - directory creation (mostly benign),
//! - LanceDB table creation (`create_empty_table` fails if a concurrent task
//!   already created it), and
//! - data mutations (one test's writes leak into another's reads).
//!
//! Under `cargo nextest` each test is its own process so this isn't observed;
//! under `cargo test` it surfaces as flaky "Failed to create LanceDB memories
//! table" errors and cross-test state bleed.
//!
//! # Fix
//!
//! A process-global async mutex serializes every test that touches the global
//! store. On acquisition we also wipe the global data layout so each test
//! starts from a clean slate, matching nextest semantics.
//!
//! The lock is exposed via [`acquire_global_test_lock`]; tests that only need
//! the guard can call it directly, while `setup_global`-style helpers bundle
//! the guard into a handle so callers don't have to thread it through their
//! return tuples.
//!
//! Compiled for this crate's own tests (`#[cfg(test)]`) and, for downstream
//! crates that need the same global-store serialization in *their* tests, when
//! the `test-support` feature is enabled.

use std::sync::{Arc, LazyLock};
use tokio::sync::{Mutex, OwnedMutexGuard};

/// Async mutex shared across every test in the process. Held for the
/// duration of each global-store test.
static GLOBAL_TEST_LOCK: LazyLock<Arc<Mutex<()>>> = LazyLock::new(|| Arc::new(Mutex::new(())));

/// RAII guard returned by [`acquire_global_test_lock`].
///
/// Dropping it releases the lock and allows the next queued test to proceed.
pub struct GlobalTestLock {
    _inner: OwnedMutexGuard<()>,
}

/// Acquire the global-test mutex and wipe the on-disk global store so the
/// caller starts from a clean slate.
///
/// The returned guard must be kept alive for the entire test body; dropping
/// it releases the lock.
///
/// Only `<ENGRAMDB_DATA_DIR>/global/` is wiped. Per-project data under
/// `<ENGRAMDB_DATA_DIR>/projects/<id>/` is left alone because each
/// project-scoped test has a unique project id (derived from its own
/// tempdir path) and may be running concurrently — wiping the shared
/// `projects/` tree would nuke their LanceDB tables mid-flight.
pub async fn acquire_global_test_lock() -> GlobalTestLock {
    let guard = Arc::clone(&GLOBAL_TEST_LOCK).lock_owned().await;

    if let Ok(global) = crate::paths::global_store_dir() {
        let _ = tokio::fs::remove_dir_all(&global).await;
    }

    GlobalTestLock { _inner: guard }
}
