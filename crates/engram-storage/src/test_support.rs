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

/// Strip every row's content digest, reproducing a store whose index predates
/// schema 0.8.0.
///
/// Test-only, and deliberately here rather than as a `MemoryStore` method:
/// production has no reason to un-record a digest, and widening the store's API
/// to construct a test fixture would put a footgun in the real surface. It
/// rewrites each row through the normal upsert path, so the result is a genuine
/// row with null digest columns, not a mocked one.
pub async fn clear_content_digests(store: &crate::MemoryStore) -> crate::Result<()> {
    for id in store.list_ids().await? {
        let Ok(memory) = store.get(&id).await else {
            continue;
        };
        let mut entry = crate::lance_index::IndexEntry::without_digest(&memory);
        entry.has_embedding = store.has_chunks_for(&id).await?;
        store.upsert_index_entry_for_test(&entry).await?;
    }
    Ok(())
}

/// Rewrite a store's `memories` table with `drop_cols` removed from the
/// Arrow schema, and stamp the manifest at `version`.
///
/// Shared with downstream crates (the dashboard reads such a store) through
/// the `test-support` feature.
///
/// The other migration tests downgrade only the manifest stamp, which
/// exercises the backfill but never the case that actually breaks: a table
/// whose Arrow schema is genuinely missing the new column. This builds that
/// table by reading the current one, dropping the fields and their arrays,
/// and recreating the table from the reduced batches — so the fixture is the
/// previous version's on-disk shape rather than a stand-in for it, and it
/// stays honest if a later version adds another column.
pub async fn downgrade_dropping(dir: &std::path::Path, drop_cols: &[&str], version: &str) {
    use arrow_array::RecordBatchIterator;
    use futures_util::stream::StreamExt;
    use lancedb::query::ExecutableQuery;
    use std::sync::Arc;

    let project_id = crate::project_id::compute_project_id(dir);
    let lance_path = crate::paths::lancedb_dir(&project_id).unwrap();
    let conn = lancedb::connect(lance_path.to_str().unwrap())
        .execute()
        .await
        .unwrap();
    let table = conn.open_table("memories").execute().await.unwrap();

    let mut stream = table.query().execute().await.unwrap();
    let mut reduced = Vec::new();
    let mut reduced_schema = None;
    while let Some(batch) = stream.next().await {
        let batch = batch.unwrap();
        let schema = batch.schema();
        let keep: Vec<usize> = (0..schema.fields().len())
            .filter(|i| !drop_cols.contains(&schema.field(*i).name().as_str()))
            .collect();
        assert_eq!(
            keep.len() + drop_cols.len(),
            schema.fields().len(),
            "fixture must actually remove every named column"
        );
        let fields: Vec<_> = keep.iter().map(|i| schema.field(*i).clone()).collect();
        let new_schema = Arc::new(arrow_schema::Schema::new(fields));
        let columns: Vec<_> = keep.iter().map(|i| batch.column(*i).clone()).collect();
        reduced.push(arrow_array::RecordBatch::try_new(new_schema.clone(), columns).unwrap());
        reduced_schema = Some(new_schema);
    }
    let schema = reduced_schema.expect("fixture needs at least one memory");

    conn.drop_table("memories", &[]).await.unwrap();
    let batches: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(
        RecordBatchIterator::new(reduced.into_iter().map(Ok), schema),
    );
    conn.create_table("memories", batches)
        .execute()
        .await
        .unwrap();

    let manifest_path = crate::paths::project_dir(dir).join("manifest.toml");
    let mut m = crate::manifest::load_manifest(&manifest_path)
        .await
        .unwrap();
    m.schema_version = version.to_string();
    // Only the schema axis may trigger: leaving the normalizer stale would
    // migrate for the wrong reason and the test would pass with the version
    // gate broken.
    m.normalizer = Some(engram_types::NORMALIZER_STAMP.to_string());
    crate::manifest::save_manifest(&manifest_path, &m)
        .await
        .unwrap();
}
