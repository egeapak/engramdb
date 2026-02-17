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
}
