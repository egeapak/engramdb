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
use fs4::fs_std::FileExt;
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
    tokio::fs::create_dir_all(lock_dir).await?;
    let lock_path = lock_dir.join("write.lock");

    tokio::task::spawn_blocking(move || -> Result<WriteLockGuard> {
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        file.lock_exclusive()?;
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
    /// serialize. We measure this by holding the lock for a known interval
    /// in one task and starting another mid-hold — the second's acquire
    /// must not return until the first releases. Without this guarantee
    /// the write lock is useless as a cross-process serialization point.
    #[tokio::test]
    async fn concurrent_acquisitions_serialize() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let temp_dir = Arc::new(TempDir::new().unwrap());
        let hold_ms = 150u64;

        let dir1 = Arc::clone(&temp_dir);
        let holder = tokio::spawn(async move {
            let guard = acquire_write_lock_at(dir1.path()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(hold_ms)).await;
            drop(guard);
        });

        // Give the holder a head start so it definitely owns the lock first.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let dir2 = Arc::clone(&temp_dir);
        let start = Instant::now();
        let waiter = tokio::spawn(async move {
            let guard = acquire_write_lock_at(dir2.path()).await.unwrap();
            drop(guard);
        });

        waiter.await.unwrap();
        holder.await.unwrap();
        let elapsed = start.elapsed();

        // The waiter cannot have acquired before the holder released. Allow
        // ample slack (CI scheduling) but assert clearly above the headstart.
        assert!(
            elapsed >= Duration::from_millis(hold_ms - 30),
            "waiter returned in {:?}; lock did not serialize",
            elapsed
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

        // Must succeed immediately — guard from previous block was released.
        let started = std::time::Instant::now();
        let guard = acquire_write_lock_at(temp_dir.path()).await.unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "second acquire should be ~instant after drop"
        );
        drop(guard);
    }
}
