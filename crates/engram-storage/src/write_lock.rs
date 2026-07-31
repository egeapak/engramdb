//! Advisory write lock for cross-process serialization.
//!
//! Multiple processes (e.g., separate `engramdb serve --stdio` sessions) may
//! target the same project simultaneously. This module provides an advisory
//! file lock (`flock(2)`) per project to serialize mutating operations.
//!
//! The lock is per-operation (not held on `MemoryStore`). An RAII guard
//! ensures the lock is always released on `?` early returns, panics, or
//! process crashes.

use super::error::{Result, StorageError};
use super::paths;
use std::fs::File;
use std::path::Path;

/// RAII guard that releases the advisory write lock on drop.
pub struct WriteLockGuard {
    _file: File,
}

impl Drop for WriteLockGuard {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

/// Acquire an exclusive advisory lock for the given project.
///
/// Opens (or creates) `<global_data_dir>/projects/<project_id>/write.lock`
/// and calls `flock(LOCK_EX)` inside `spawn_blocking` to avoid blocking
/// the async executor. Returns an RAII guard that releases the lock on drop.
pub async fn acquire_write_lock(project_id: &str) -> Result<WriteLockGuard> {
    let lock_dir = paths::global_data_dir()?.join("projects").join(project_id);
    acquire_write_lock_at(&lock_dir).await
}

/// Acquire an exclusive advisory lock in the given directory.
pub(crate) async fn acquire_write_lock_at(lock_dir: &Path) -> Result<WriteLockGuard> {
    acquire_lock_file(lock_dir.join("write.lock")).await
}

/// Acquire an exclusive advisory lock on an arbitrary lock file.
///
/// Generalization of [`acquire_write_lock_at`] that lets the caller name the
/// lock file itself (e.g. `registry.json.lock` next to the registry file).
/// Parent directories are created as needed; `flock(LOCK_EX)` runs inside
/// `spawn_blocking` so a contended acquire never blocks the async executor.
///
/// `flock` is per open file description: every call opens a fresh fd, so two
/// acquisitions serialize even within one process. The flip side is that a
/// task must never re-acquire a lock it already holds — the second fd would
/// block forever.
pub(crate) async fn acquire_lock_file(lock_path: std::path::PathBuf) -> Result<WriteLockGuard> {
    if let Some(parent) = lock_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::task::spawn_blocking(move || -> Result<WriteLockGuard> {
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        file.lock()?;
        Ok(WriteLockGuard { _file: file })
    })
    .await
    .map_err(|e| StorageError::Validation(format!("Write lock task failed: {}", e)))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_acquire_and_release() {
        let temp_dir = TempDir::new().unwrap();
        let guard = acquire_write_lock_at(temp_dir.path()).await.unwrap();
        drop(guard);
    }

    #[tokio::test]
    async fn test_sequential_reacquisition() {
        let temp_dir = TempDir::new().unwrap();

        let guard1 = acquire_write_lock_at(temp_dir.path()).await.unwrap();
        drop(guard1);

        let guard2 = acquire_write_lock_at(temp_dir.path()).await.unwrap();
        drop(guard2);
    }

    /// The core safety property: two tasks racing for the SAME lock must
    /// serialize. The holder keeps the lock until this test tells it to let
    /// go, so the ordering is driven by signals rather than by sleeps — an
    /// earlier version timed a fixed hold against a fixed head start and
    /// failed on Windows, whose ~15.6ms timer granularity stretched the head
    /// start far enough into the hold to eat the slack.
    #[tokio::test]
    async fn concurrent_acquisitions_serialize() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        use tokio::sync::oneshot;

        let temp_dir = Arc::new(TempDir::new().unwrap());

        let (holder_ready_tx, holder_ready_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel::<()>();

        let dir1 = Arc::clone(&temp_dir);
        let holder = tokio::spawn(async move {
            let guard = acquire_write_lock_at(dir1.path()).await.unwrap();
            holder_ready_tx.send(()).unwrap();
            release_rx.await.unwrap();
            // Stamped *before* the unlock, so a serialized waiter can only
            // ever observe a later instant.
            let released_at = Instant::now();
            drop(guard);
            released_at
        });

        // From here the holder provably owns the lock — no head start needed.
        holder_ready_rx.await.unwrap();

        let (waiter_ready_tx, waiter_ready_rx) = oneshot::channel();
        let dir2 = Arc::clone(&temp_dir);
        let waiter = tokio::spawn(async move {
            waiter_ready_tx.send(()).unwrap();
            let guard = acquire_write_lock_at(dir2.path()).await.unwrap();
            let acquired_at = Instant::now();
            drop(guard);
            acquired_at
        });

        // Let the waiter reach the blocking acquire. Oversleeping here only
        // makes the next assertion stronger, so it carries no timing risk.
        waiter_ready_rx.await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !waiter.is_finished(),
            "waiter acquired the lock while the holder still held it"
        );

        release_tx.send(()).unwrap();
        let acquired_at = waiter.await.unwrap();
        let released_at = holder.await.unwrap();

        assert!(
            acquired_at >= released_at,
            "waiter acquired at {acquired_at:?}, before the holder released at {released_at:?}"
        );
    }

    /// Dropping the guard releases the lock — required for `?`-on-error or
    /// panic-in-critical-section recovery.
    #[tokio::test]
    async fn dropped_guard_releases_lock() {
        let temp_dir = TempDir::new().unwrap();

        {
            let _g = acquire_write_lock_at(temp_dir.path()).await.unwrap();
            // guard dropped at end of this scope
        }

        // Must not block — the guard from the previous block was released.
        // A timeout (rather than a latency bound) is what distinguishes a
        // still-held lock from a merely slow CI runner.
        let guard = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            acquire_write_lock_at(temp_dir.path()),
        )
        .await
        .expect("second acquire blocked: the dropped guard did not release the lock")
        .unwrap();
        drop(guard);
    }
}
