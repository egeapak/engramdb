//! The append-only JSONL log shared by hook call stats ([`super::hooks`]) and
//! per-memory usage events ([`super::usage`]).
//!
//! Both have the same shape: a short-lived process must record something
//! without paying for a LanceDB write, so it appends one line under
//! `.engramdb/state/` through [`crate::state_file`] (one `open` + `write`, no
//! lock), and a long-running process later moves the lines into a LanceDB
//! table. Everything about that life cycle that is not the line format lives
//! here, so the two logs cannot drift apart in their loss behavior:
//!
//! - **Rotation.** When the live file passes the caller's limit, the writer
//!   that noticed renames it to the rotated generation, replacing the previous
//!   one. Without a drainer, history is at most two generations.
//! - **Rotation race.** Two writers that both see the live file over the limit
//!   both rename. If a third process appended a fresh live file between the
//!   two renames, the second rename replaces the full generation with that
//!   near-empty file and one generation is lost. The window is the gap between
//!   two `rename(2)` calls.
//! - **Drain grace.** A writer that opened the live file just before a drain
//!   renamed it writes into the renamed file. The drain waits a grace period
//!   after renaming before it reads, which covers an `open` + `write`; a
//!   writer stalled for longer than that between the two calls loses its line.
//! - **No double counting.** Readers never read a file a drain has claimed.
//!   The other side of that: lines in a claimed file are missing from reads
//!   until a drain inserts them — for the length of a drain, or, after a drain
//!   that crashed before its delete, until the next drain.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;

use crate::error::Result;
use crate::state_file::{append_state_file, read_state_file};

/// The file names one log uses under `.engramdb/state/`, plus the name of its
/// drain lock in the store's global data dir.
#[derive(Debug, Clone, Copy)]
pub(crate) struct JsonlLog {
    pub live: &'static str,
    pub rotated: &'static str,
    pub draining_prefix: &'static str,
    pub lock_file: &'static str,
}

/// Whether `project_root` holds an initialized store — the same test the
/// SessionEnd hook uses (`manifest.toml` or `memories/`). The plugin
/// registers hooks machine-wide, and every directory Claude Code starts in
/// would otherwise collect a state folder.
pub(crate) fn is_initialized(project_root: &Path) -> bool {
    let engramdb = crate::paths::project_dir(project_root);
    engramdb.join("manifest.toml").is_file() || engramdb.join("memories").is_dir()
}

pub(crate) fn state_dir(project_root: &Path) -> PathBuf {
    crate::paths::project_dir(project_root).join("state")
}

impl JsonlLog {
    pub(crate) fn live_path(&self, project_root: &Path) -> PathBuf {
        state_dir(project_root).join(self.live)
    }

    /// Append `lines` (each already `\n`-terminated) in one write, then rotate
    /// when the live file is past `rotate_bytes`. The caller has already
    /// checked [`is_initialized`].
    pub(crate) fn append(&self, project_root: &Path, lines: &str, rotate_bytes: u64) -> Result<()> {
        let live = self.live_path(project_root);
        append_state_file(&live, lines)?;
        // `symlink_metadata` reads no content; the append above already
        // refused a symlink at this path. `rename` never dereferences its
        // source.
        if let Ok(meta) = std::fs::symlink_metadata(&live) {
            if meta.len() > rotate_bytes {
                let rotated = state_dir(project_root).join(self.rotated);
                // ENOENT here means another writer rotated first (the
                // rotation race in the module docs).
                let _ = std::fs::rename(&live, &rotated);
            }
        }
        Ok(())
    }

    /// The text of both unclaimed generations, oldest first. Missing files
    /// are skipped. Files a drain has claimed are never included.
    pub(crate) fn read_unclaimed(&self, project_root: &Path) -> Result<Vec<String>> {
        let dir = state_dir(project_root);
        let mut out = Vec::new();
        for name in [self.rotated, self.live] {
            if let Some(text) = read_state_file(&dir.join(name))? {
                out.push(text);
            }
        }
        Ok(out)
    }

    /// Move the log into a table.
    ///
    /// 1. Claim: rename the rotated and live files to
    ///    `<draining_prefix><pid>.<n>.<i>.jsonl`. New writers then start a
    ///    fresh live file. Files left by a drain that crashed are picked up.
    /// 2. Wait `grace`, then read every claimed file.
    /// 3. Hand the texts to `ingest`, which inserts them idempotently.
    /// 4. Delete the claimed files — only after `ingest` succeeded. A crash or
    ///    an insert error leaves them for the next drain.
    ///
    /// Serialized per store by a lock file beside the store's `lancedb/`
    /// directory (never under `.engramdb/state/`, whose files only
    /// `state_file` may open). Does nothing, and creates nothing, when that
    /// directory's parent does not exist: the tables refuse to create
    /// scaffolding for a store without one, and a lock created first would
    /// defeat that check.
    ///
    /// Returns the number of claimed files deleted, or `None` when nothing
    /// was claimed (and `ingest` was not called).
    pub(crate) async fn drain<F, Fut, T>(
        &self,
        project_root: &Path,
        store_id: &str,
        grace: Duration,
        ingest: F,
    ) -> anyhow::Result<Option<(T, u64)>>
    where
        F: FnOnce(Vec<String>) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        if !is_initialized(project_root) {
            return Ok(None);
        }
        let Some(store_data) = super::persistence::lancedb_root(store_id)?
            .parent()
            .map(Path::to_path_buf)
        else {
            return Ok(None);
        };
        if !store_data.is_dir() {
            return Ok(None);
        }
        let _guard = crate::write_lock::acquire_lock_file(store_data.join(self.lock_file)).await?;

        let dir = state_dir(project_root);
        let stamp = format!(
            "{}.{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let mut claimed_new = false;
        for (i, name) in [self.rotated, self.live].iter().enumerate() {
            let target = dir.join(format!("{}{stamp}.{i}.jsonl", self.draining_prefix));
            match std::fs::rename(dir.join(name), &target) {
                Ok(()) => claimed_new = true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let mut claimed: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with(self.draining_prefix))
                })
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        if claimed.is_empty() {
            return Ok(None);
        }
        claimed.sort();
        if claimed_new {
            tokio::time::sleep(grace).await;
        }

        let mut texts = Vec::with_capacity(claimed.len());
        for path in &claimed {
            if let Some(text) = read_state_file(path)? {
                texts.push(text);
            }
        }
        let out = ingest(texts).await?;
        let mut files = 0;
        for path in &claimed {
            match std::fs::remove_file(path) {
                Ok(()) => files += 1,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!("could not delete drained {}: {e}", path.display()),
            }
        }
        Ok(Some((out, files)))
    }
}
