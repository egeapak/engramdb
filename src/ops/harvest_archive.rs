//! The SessionEnd archive step: copy the ending session's transcript, record
//! it in the ledger, and run the retention sweep.
//!
//! Policy, so it lives in `ops` rather than in the hook handler: the pin rule
//! (a sweep that cannot enumerate pins evicts nothing) is an invariant, and
//! here it is testable without hook JSON. The hook keeps parsing the event and
//! its fail-open contract; this function swallows every failure itself.

use std::path::{Path, PathBuf};

use crate::storage::RegistryBackend;

/// Archive the ending session's transcript, if archiving is enabled.
///
/// `transcript` is the path the SessionEnd event names (`transcript_path`),
/// when it names one; the hook parses its JSON, this does the rest.
///
/// This is the whole reason the archive exists. Claude Code prunes its own
/// transcripts, and once one is gone the conversation cannot be harvested and
/// any memory derived from it has lost its evidence. Session end is the last
/// moment the file is reliably still there — archiving at *harvest* time
/// would protect nothing, since you necessarily still hold the transcript
/// then.
///
/// Entirely best-effort, in keeping with the rest of SessionEnd: every
/// failure is logged and swallowed, because nothing here may block session
/// teardown. The archive is written under the **root** project so a worktree
/// and its main checkout share one directory, matching the ledger.
pub async fn archive_session_end(
    dir: &Path,
    session_id: &str,
    transcript: Option<PathBuf>,
    registry: &dyn RegistryBackend,
) {
    let config_path = crate::storage::paths::project_dir(dir).join("config.toml");
    let config = crate::storage::config::load_config_or_default(&config_path).await;
    if !config.harvest.archive {
        return;
    }

    // Prefer the path the event hands us; fall back to locating it ourselves
    // (older Claude Code builds, or an event missing the field).
    let transcript = transcript.filter(|p| p.is_file()).or_else(|| {
        crate::storage::transcripts::list_sessions_for(std::slice::from_ref(&dir.to_path_buf()))
            .ok()?
            .into_iter()
            .find(|s| s.session_id == session_id)
            .map(|s| s.transcript_path)
    });
    let Some(transcript) = transcript else {
        tracing::debug!("SessionEnd archive: no transcript found for {session_id}");
        return;
    };

    // Skip *writing* an oversized archive, but keep going: returning here
    // would also skip the retention sweep below, so the very user this
    // ceiling exists for — one whose sessions routinely exceed it — would
    // stop having their existing archives aged out at all.
    let limit = config.harvest.archive_max_transcript_bytes;
    let too_big = limit > 0 && {
        let size = std::fs::metadata(&transcript).map(|m| m.len()).unwrap_or(0);
        if size > limit {
            tracing::debug!(
                "SessionEnd archive: skipping {session_id} ({size} bytes exceeds \
                 [harvest] archive_max_transcript_bytes = {limit})"
            );
        }
        size > limit
    };

    // Resolve the **root** project, exactly as every reader does
    // (`harvest::session_scope`). Using the invoking directory's own id agrees
    // only when `cli::run` already rewrote `dir` to the main worktree; for a
    // project linked with `engramdb projects link` it does not, and the
    // archive would land in a directory no `harvest ledger` command ever reads
    // — invisible to export, and never bounded by the prune budget.
    //
    // The ledger has to come from the same resolution, not from `dir`: the
    // prune below deletes the root's archives, so writing the eviction back to
    // a sub-project's own ledger would leave the root advertising files this
    // very call removed.
    let scope = match crate::ops::harvest::session_scope(dir, registry).await {
        Ok(scope) => Some(scope),
        // Fail open, consistent with the rest of SessionEnd: an unreadable
        // registry must not cost the archive entirely.
        Err(e) => {
            tracing::debug!("SessionEnd archive: scope unresolved, using own id: {e}");
            None
        }
    };
    let (project_id, ledger_dir) = match &scope {
        Some(scope) => (scope.root_project_id.clone(), scope.root_dir.clone()),
        None => (
            crate::storage::project_id::compute_project_id(dir),
            dir.to_path_buf(),
        ),
    };
    if !too_big {
        match crate::storage::transcript_archive::archive_transcript(
            &project_id,
            session_id,
            &transcript,
        ) {
            Ok(archive) => {
                if let Err(e) =
                    crate::storage::harvest_state::set_archive(&ledger_dir, session_id, archive)
                {
                    tracing::debug!("SessionEnd archive: ledger update failed (non-fatal): {e}");
                }
            }
            Err(e) => tracing::debug!("SessionEnd archive failed (non-fatal): {e}"),
        }
    }

    // Which copies are cited as a memory's evidence, and so exempt. Unlike
    // everything else on this path a failure here is **not** shrugged off into
    // a default: an empty pin set is indistinguishable from "no memory cites
    // anything", and acting on that guess would let an unattended hook delete
    // the one surviving copy behind a harvested memory. Unknown pins therefore
    // mean no eviction this run; the next `harvest ledger prune` or the next
    // session end reclaims the bytes once the stores are readable again.
    let pinned = match &scope {
        Some(scope) => match crate::ops::evidence_links(scope).await {
            Ok(links) => Some(links.pinned_sessions()),
            Err(e) => {
                tracing::warn!(
                    "SessionEnd archive: skipping the retention sweep — could not determine \
                     which transcript copies memories still cite ({e})"
                );
                None
            }
        },
        None => {
            tracing::warn!(
                "SessionEnd archive: skipping the retention sweep — the project scope did not \
                 resolve, so pinned transcript copies cannot be identified"
            );
            None
        }
    };
    let Some(pinned) = pinned else { return };

    // Always: session end is the only moment this reliably runs, so an
    // oversized transcript must not also cost the retention sweep.
    match crate::storage::transcript_archive::prune_archives(
        &project_id,
        config.harvest.archive_retention_days,
        config.harvest.archive_max_bytes,
        false,
        &pinned,
    ) {
        // Evicted files must stop being advertised by the ledger, or
        // `harvest ledger show` offers an export that cannot succeed.
        Ok(outcome) => {
            // A copy the sweep chose but could not delete stays on disk and
            // in the ledger and still counts toward the budget; the next
            // sweep retries it. Warn so a persistent failure (permissions, a
            // read-only volume) is visible rather than a silent overrun.
            if !outcome.failed.is_empty() {
                let names: Vec<String> = outcome
                    .failed
                    .iter()
                    .map(|(session, error)| format!("{session} ({error})"))
                    .collect();
                tracing::warn!(
                    "SessionEnd archive: could not remove {} transcript copy(ies), kept: {}",
                    outcome.failed.len(),
                    names.join(", ")
                );
            }
            if let Err(e) =
                crate::storage::harvest_state::clear_archive_refs(&ledger_dir, &outcome.removed)
            {
                tracing::debug!(
                    "SessionEnd archive: clearing evicted refs failed (non-fatal): {e}"
                );
            }
        }
        Err(e) => tracing::debug!("SessionEnd archive: prune failed (non-fatal): {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{transcript_archive, InMemoryRegistry, MemoryStore};

    fn archived(project_id: &str) -> Vec<String> {
        let mut ids: Vec<String> = transcript_archive::list_archives(project_id)
            .unwrap()
            .into_iter()
            .map(|a| a.session_id)
            .collect();
        ids.sort();
        ids
    }

    /// A transcript file Claude Code would have written.
    fn transcript(dir: &Path, session_id: &str) -> PathBuf {
        let path = dir.join(format!("{session_id}.jsonl"));
        std::fs::write(
            &path,
            format!("{{\"type\":\"user\",\"sessionId\":\"{session_id}\"}}\n"),
        )
        .unwrap();
        path
    }

    /// The pin rule, driven directly: a linked sub-project whose path is gone
    /// has pins nobody can read, so the sweep evicts nothing at all, not even
    /// an unpinned copy over the budget. The control run (sub-project path
    /// present) shows the same budget does evict.
    #[tokio::test]
    async fn sweep_is_skipped_when_pins_cannot_be_enumerated() {
        let registry = InMemoryRegistry::new();
        let root = tempfile::TempDir::new().unwrap();
        let logs = tempfile::TempDir::new().unwrap();
        let store = MemoryStore::init(root.path(), &registry).await.unwrap();
        let root_id = store.project_id.clone();

        // A first copy under the default budget.
        archive_session_end(
            root.path(),
            "s-old",
            Some(transcript(logs.path(), "s-old")),
            &registry,
        )
        .await;
        assert_eq!(archived(&root_id), vec!["s-old"]);

        // From now on any copy is over budget, and a linked sub-project's
        // path no longer exists (moved, deleted, an unmounted volume).
        std::fs::write(
            root.path().join(".engramdb").join("config.toml"),
            "[harvest]\narchive_max_bytes = 1\n",
        )
        .unwrap();
        let gone = root.path().with_extension("gone-sub-project");
        registry
            .update_with_parent(&gone, "0123456789abcdef", Some(&root_id))
            .await
            .unwrap();

        archive_session_end(
            root.path(),
            "s-new",
            Some(transcript(logs.path(), "s-new")),
            &registry,
        )
        .await;
        assert_eq!(
            archived(&root_id),
            vec!["s-new", "s-old"],
            "a sweep that could not read every store's pins evicted a copy"
        );

        // Control: once the sub-project's path exists (and has no store of
        // its own, a real "no pins here"), the same budget evicts.
        std::fs::create_dir_all(&gone).unwrap();
        archive_session_end(
            root.path(),
            "s-third",
            Some(transcript(logs.path(), "s-third")),
            &registry,
        )
        .await;
        assert!(
            archived(&root_id).len() < 3,
            "the control sweep did not run: {:?}",
            archived(&root_id)
        );
        let _ = std::fs::remove_dir_all(&gone);
    }
}
