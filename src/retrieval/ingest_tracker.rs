//! Process-wide count of detached ingest tasks (embed + NLI + proposals).
//!
//! `RetrievalEngine::spawn_ingest` runs the embedding of a just-written memory
//! in a detached task so an MCP `create`/`update` returns at once. A process
//! that exits right after such a call (a `claude -p` run closing stdin, a
//! session ending) used to drop those tasks with the runtime, leaving the
//! memory without vectors and nothing to say so. The MCP shutdown sequence now
//! waits for them, bounded, through [`drain_ingests`].
//!
//! Process-wide rather than per engine because every front end builds a fresh
//! engine per call (the same reason `UsageCache` is a static). The count goes
//! down when an [`IngestGuard`] drops, so a task that panics or is cancelled is
//! still counted out.
//!
//! Declared loss: the drain is a bound, not a guarantee. A task still running
//! at the bound (a model download, a hung provider) or a process killed with
//! `SIGKILL` loses its vectors; `doctor`'s "Memory vectors" check reports the
//! memories left without any, and `engramdb reindex --embeddings-only` embeds
//! exactly those.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static PENDING: AtomicUsize = AtomicUsize::new(0);
static CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Held by one in-flight ingest task; counts it out when dropped.
#[must_use = "the ingest is counted only while the guard lives"]
pub struct IngestGuard(());

impl Drop for IngestGuard {
    fn drop(&mut self) {
        PENDING.fetch_sub(1, Ordering::SeqCst);
        CHANGED.notify_waiters();
    }
}

/// Count one ingest task in. Take it *before* `tokio::spawn` and move it into
/// the task, so a drain that starts right after the spawn already sees it.
pub fn track() -> IngestGuard {
    PENDING.fetch_add(1, Ordering::SeqCst);
    IngestGuard(())
}

/// How many ingest tasks are running now.
pub fn pending_ingests() -> usize {
    PENDING.load(Ordering::SeqCst)
}

/// Wait until no ingest task is running, at most `bound`. Returns how many
/// were still running at the bound (0 when all finished). Returns at once when
/// none runs.
pub async fn drain_ingests(bound: Duration) -> usize {
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        // Register interest before reading the count, so a guard dropped in
        // between is not missed.
        let changed = CHANGED.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        let left = pending_ingests();
        if left == 0 {
            return 0;
        }
        if tokio::time::timeout_at(deadline, changed).await.is_err() {
            return pending_ingests();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two tests in one: the counter is process-wide, and nextest gives each
    /// test its own process, but `cargo test` would not — keeping both halves
    /// in one test keeps the count exact either way.
    #[tokio::test]
    async fn drain_waits_for_a_running_ingest_and_times_out_on_a_stuck_one() {
        assert_eq!(drain_ingests(Duration::from_secs(1)).await, 0);

        // A running ingest that finishes inside the bound is waited for.
        let guard = track();
        let finisher = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(guard);
        });
        let started = tokio::time::Instant::now();
        assert_eq!(drain_ingests(Duration::from_secs(30)).await, 0);
        assert!(started.elapsed() >= Duration::from_millis(200));
        finisher.await.unwrap();
        assert_eq!(pending_ingests(), 0);

        // A stuck one is reported at the bound.
        let stuck = track();
        let started = tokio::time::Instant::now();
        assert_eq!(drain_ingests(Duration::from_millis(300)).await, 1);
        assert!(started.elapsed() >= Duration::from_millis(300));
        drop(stuck);
        assert_eq!(pending_ingests(), 0);
    }
}
