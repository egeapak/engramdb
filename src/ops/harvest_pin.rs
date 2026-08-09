//! The evidence link between a memory and the conversation it came from.
//!
//! A harvested memory is an assertion about a conversation nobody can reread
//! by default: Claude Code prunes its own transcripts, so within weeks the only
//! surviving copy is the one the SessionEnd hook took — and that copy is
//! subject to a size and age budget. Recording *which* sessions a memory came
//! from does two things at once:
//!
//! 1. A challenged memory resolves to its source. `harvest show <id>` reads the
//!    copy, so the loop from a disputed claim back to what was actually said
//!    closes without the agent having to remember anything.
//! 2. It **pins** that copy. [`transcript_archive::prune_archives`] never
//!    evicts a session some memory cites, so the evidence outlives the budget
//!    rather than the other way round.
//!
//! # Where the link lives, and why not in a table of its own
//!
//! It is a field on the memory — `source_sessions` in the `.md` file's hidden
//! block, mirrored into the `memories` table as a column (schema `0.7.0`).
//!
//! A join table was the alternative and is the wrong shape here. The schema
//! migration rebuilds the memories table **from the `.md` files**, so a
//! relation held only in LanceDB is destroyed by the very mechanism that adds
//! columns — and `reindex` would silently drop every pin, which is to say every
//! transcript copy would become evictable at the next open. The memory files
//! are also the portable half: they are committed and travel with a clone,
//! while `lancedb/` lives under the global data dir and does not. A relation
//! whose lifetime must equal the memory's belongs in the artifact that *is* the
//! memory.
//!
//! The cardinality agrees: a memory comes from one session, occasionally a few.
//! Both directions stay cheap — memory→sessions is on the row you already
//! loaded, and session→memories is one narrow column scan per eviction pass
//! ([`crate::storage::lance_index::LanceIndex::list_source_session_links`]).
//!
//! # Root scope
//!
//! Like the ledger and the conversation index, everything here is resolved
//! through [`SessionScope`]: the copies are keyed by the root project id, so
//! the pins that protect them must be gathered from every store in that scope
//! or an eviction run from one project would delete the evidence of another.

use crate::ops::harvest::SessionScope;
use crate::storage::{project_id, transcript_archive, transcripts, MemoryStore};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashSet};

/// Every memory→session citation in a scope, indexed by session.
#[derive(Debug, Clone, Default)]
pub struct EvidenceLinks {
    /// Session id → the ids of the memories citing it, sorted and deduped.
    pub by_session: BTreeMap<String, Vec<String>>,
}

impl EvidenceLinks {
    /// The sessions whose stored copy must not be evicted.
    pub fn pinned_sessions(&self) -> HashSet<String> {
        self.by_session.keys().cloned().collect()
    }

    /// How many memories cite something, counted once each.
    pub fn citing_memories(&self) -> usize {
        self.by_session
            .values()
            .flatten()
            .collect::<HashSet<_>>()
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_session.is_empty()
    }
}

/// Gather the evidence links held by every memory store in `scope`.
///
/// A path that is **present** and simply has no store of its own (the usual
/// shape of a registered git worktree, whose storage lives at its parent)
/// contributes nothing and is not an error. Any *other* failure propagates: a
/// store that cannot be read is a store whose pins are unknown, and the caller
/// must be able to tell that apart from a store with no pins — the difference is
/// whether an eviction pass is allowed to run at all.
///
/// The two are not the same condition, and reading `NotInitialized` as the
/// first was the defect. `MemoryStore::open` reports it for any `dir` whose
/// `.engramdb/` does not *stat*, which covers a linked sub-project that was
/// moved, deleted, or sits on an unmounted volume — and
/// [`crate::ops::harvest::session_scope`] deliberately keeps registered paths
/// that no longer exist, so those are ordinary, not exotic. Every such path
/// silently contributed zero pins, and the SessionEnd sweep then ran with an
/// incomplete set: exactly the "unknown pin is indistinguishable from an absent
/// one" case, resolved the wrong way, against the only surviving copy of a
/// harvested conversation. So the *directory* is what separates them — present
/// but storeless is a real answer, absent is not an answer at all.
pub async fn evidence_links(scope: &SessionScope) -> Result<EvidenceLinks> {
    let mut links: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Deduped by project id, never by path spelling: `.`, a relative `--dir`
    // and a symlinked checkout are the same store under three names, and
    // opening one twice would double every memory id behind a session.
    let mut seen: HashSet<String> = HashSet::new();
    for path in &scope.paths {
        // Opened *before* the id is marked seen. A git worktree shares its main
        // checkout's project id but has no `.engramdb/` of its own, so marking
        // first would let whichever of the two sorts earlier claim the id — and
        // if that was the worktree, the store holding every pin would be
        // skipped and the whole scope would look uncited.
        let store = match MemoryStore::open(path).await {
            Ok(store) => store,
            Err(crate::storage::StorageError::NotInitialized) if is_readable_dir(path) => continue,
            Err(crate::storage::StorageError::NotInitialized) => {
                anyhow::bail!(
                    "{} is registered in this project's scope but is not a readable directory, \
                     so whether any memory there cites a conversation is unknown. Re-link or \
                     unregister it with `engramdb projects`, or restore the path.",
                    path.display()
                )
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "could not read the memory store at {} to find which conversations \
                         are still cited",
                        path.display()
                    )
                })
            }
        };
        if !seen.insert(project_id::compute_project_id(path)) {
            continue;
        }
        for link in store.list_source_session_links().await? {
            for session in link.sessions {
                links
                    .entry(session)
                    .or_default()
                    .push(link.memory_id.clone());
            }
        }
    }
    for ids in links.values_mut() {
        ids.sort();
        ids.dedup();
    }
    Ok(EvidenceLinks { by_session: links })
}

/// Can this path be read at all, i.e. is "no store here" a fact rather than a
/// guess?
///
/// `std::fs::metadata`, not `Path::is_dir`: the latter folds every error into
/// `false`, which is the same collapse this function exists to undo.
fn is_readable_dir(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_dir())
}

/// Every session in scope that still has bytes behind it — a live transcript
/// or a stored copy.
pub fn sessions_with_bytes(scope: &SessionScope) -> Result<HashSet<String>> {
    let mut ids: HashSet<String> = transcripts::list_sessions_for(&scope.paths)?
        .into_iter()
        .map(|s| s.session_id)
        .collect();
    ids.extend(
        transcript_archive::list_archives(&scope.root_project_id)?
            .into_iter()
            .map(|a| a.session_id),
    );
    Ok(ids)
}

/// A cited conversation that can no longer be read.
#[derive(Debug, Clone)]
pub struct ExpiredEvidence {
    pub session_id: String,
    /// Memories that cite it.
    pub memory_ids: Vec<String>,
}

/// Which citations name a session with no bytes left.
///
/// This is expiry, not damage. A copy reaches the end of its retention window,
/// or was collected on a machine this clone has never seen, and the memory it
/// supports is untouched and still true — it simply cannot be traced back any
/// more. Callers must report it in those terms.
pub fn expired_evidence(
    links: &EvidenceLinks,
    with_bytes: &HashSet<String>,
) -> Vec<ExpiredEvidence> {
    links
        .by_session
        .iter()
        .filter(|(session, _)| !with_bytes.contains(*session))
        .map(|(session, memory_ids)| ExpiredEvidence {
            session_id: session.clone(),
            memory_ids: memory_ids.clone(),
        })
        .collect()
}

/// What [`link_memories`] did.
#[derive(Debug, Clone, Default)]
pub struct LinkReport {
    /// Memories that gained the link.
    pub linked: Vec<String>,
    /// Memories that already carried it — re-running `harvest mark` to fix a
    /// note must not double-count the citation.
    pub unchanged: Vec<String>,
    /// Ids that named no memory in this store, with the reason.
    pub unresolved: Vec<(String, String)>,
}

impl LinkReport {
    /// How many memories cite the session after this call — i.e. how strongly
    /// its copy is now pinned. Counts the ones that already did: re-running
    /// `harvest mark` must report the same pin, not a weaker one.
    pub fn pinned(&self) -> usize {
        self.linked.len() + self.unchanged.len()
    }
}

/// Record that each of `memory_ids` was extracted from `session_id`.
///
/// Driven from `harvest mark`, which is the one moment both halves are already
/// named. Per-memory best effort: an id that does not resolve is reported
/// rather than failing the call, because the ledger decision has already been
/// written by then and losing it to a typo'd memory id would leave the session
/// re-offered forever.
pub async fn link_memories(
    store: &MemoryStore,
    session_id: &str,
    memory_ids: &[String],
) -> Result<LinkReport> {
    // Same gate the ledger applies. The value ends up in a memory file that is
    // committed and cloned, and is later joined into a transcript path by the
    // pin lookup.
    if !transcripts::is_valid_session_id(session_id) {
        anyhow::bail!(
            "cannot record provenance for session id {session_id:?}: expected a plain \
identifier (letters, digits, '-', '_', '.') that is not a path"
        );
    }
    let mut report = LinkReport::default();
    for id in memory_ids {
        // Only the pre-state knows whether the citation is new, and the closure
        // is the only place that sees it — `update_with` re-reads inside the
        // lock, so comparing before the call would be racing the very thing the
        // lock exists to prevent.
        let mut was_new = false;
        // `update_with` is that atomic read-modify-write: two harvests marking
        // the same memory from different sessions must not erase each other's
        // citation, which an unlocked read-then-update would allow.
        match store
            .update_with(id, |memory| {
                was_new = memory.link_source_session(session_id);
                Ok(())
            })
            .await
        {
            Ok(memory) if was_new => report.linked.push(memory.id),
            Ok(memory) => report.unchanged.push(memory.id),
            Err(e) => report.unresolved.push((id.clone(), format!("{e}"))),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::InMemoryRegistry;
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    async fn store_with(dir: &std::path::Path) -> MemoryStore {
        MemoryStore::init(dir, &InMemoryRegistry::new())
            .await
            .unwrap()
    }

    fn memory(summary: &str) -> Memory {
        Memory::new(
            MemoryType::Decision,
            summary,
            "content",
            Provenance::human(),
        )
    }

    #[tokio::test]
    async fn linking_survives_a_reload_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let store = store_with(tmp.path()).await;
        let id = store.create(&memory("m")).await.unwrap();

        let report = link_memories(&store, "sess-a", std::slice::from_ref(&id))
            .await
            .unwrap();
        assert_eq!(report.pinned(), 1);
        assert!(report.unresolved.is_empty());

        // Re-marking the same pair must not duplicate the citation.
        link_memories(&store, "sess-a", std::slice::from_ref(&id))
            .await
            .unwrap();
        let loaded = store.get(&id).await.unwrap();
        assert_eq!(loaded.source_sessions, vec!["sess-a".to_string()]);

        // A second session adds, rather than replaces.
        link_memories(&store, "sess-b", std::slice::from_ref(&id))
            .await
            .unwrap();
        let loaded = store.get(&id).await.unwrap();
        assert_eq!(
            loaded.source_sessions,
            vec!["sess-a".to_string(), "sess-b".to_string()]
        );
    }

    #[tokio::test]
    async fn an_unknown_memory_id_is_reported_not_fatal() {
        let tmp = TempDir::new().unwrap();
        let store = store_with(tmp.path()).await;
        let good = store.create(&memory("m")).await.unwrap();

        let report = link_memories(
            &store,
            "sess-a",
            &[good.clone(), "no-such-memory".to_string()],
        )
        .await
        .unwrap();
        assert_eq!(report.linked, vec![good]);
        assert_eq!(report.unresolved.len(), 1);
        assert_eq!(report.unresolved[0].0, "no-such-memory");
    }

    #[tokio::test]
    async fn a_path_shaped_session_id_is_refused() {
        let tmp = TempDir::new().unwrap();
        let store = store_with(tmp.path()).await;
        let id = store.create(&memory("m")).await.unwrap();
        assert!(link_memories(&store, "../../etc", &[id]).await.is_err());
    }

    /// A git worktree shares its main checkout's project id (that id keys off
    /// the git remote) but has no `.engramdb/` of its own. Deduping by id
    /// *before* opening let whichever path sorted earlier claim the id, so a
    /// worktree sorting ahead of the main checkout silently shadowed the store
    /// holding every pin — and the whole scope read as uncited, which is a
    /// licence to evict every transcript copy behind a memory.
    #[tokio::test]
    async fn a_storeless_path_sharing_a_project_id_does_not_shadow_the_real_store() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("zz-main");
        let worktree = tmp.path().join("aa-worktree");
        for dir in [&main, &worktree] {
            std::fs::create_dir_all(dir.join(".git")).unwrap();
            std::fs::write(
                dir.join(".git").join("config"),
                "[remote \"origin\"]\n\turl = https://example.invalid/shared.git\n",
            )
            .unwrap();
        }
        assert_eq!(
            project_id::compute_project_id(&main),
            project_id::compute_project_id(&worktree),
            "fixture must reproduce the shared id, or the test proves nothing"
        );

        let store = MemoryStore::init(&main, &InMemoryRegistry::new())
            .await
            .unwrap();
        let id = store.create(&memory("m")).await.unwrap();
        link_memories(&store, "sess-a", std::slice::from_ref(&id))
            .await
            .unwrap();

        // Worktree first — the order `SessionScope::paths` (sorted) produces
        // whenever the worktree's path sorts ahead of the main checkout's.
        let scope = SessionScope {
            root_project_id: project_id::compute_project_id(&main),
            root_dir: main.clone(),
            paths: vec![worktree, main],
        };
        let links = evidence_links(&scope).await.unwrap();
        assert_eq!(links.pinned_sessions().len(), 1);
        assert_eq!(links.by_session["sess-a"], vec![id]);
    }

    /// A sub-project that moved (or was deleted, or lives on a volume that is
    /// not mounted) is still in the registry, and `session_scope` deliberately
    /// keeps registered paths that no longer exist. Reading that as "this
    /// project has no pins" hands the eviction sweep a licence it has not
    /// earned: the copies it then deletes may be the only evidence behind a
    /// memory in the store it could not open.
    #[tokio::test]
    async fn a_scope_path_that_cannot_be_read_is_not_a_project_with_no_pins() {
        let tmp = TempDir::new().unwrap();
        let main = tmp.path().join("main");
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let store = store_with(&main).await;
        let id = store.create(&memory("m")).await.unwrap();
        link_memories(&store, "sess-a", std::slice::from_ref(&id))
            .await
            .unwrap();

        let scope = |paths: Vec<std::path::PathBuf>| SessionScope {
            root_project_id: project_id::compute_project_id(&main),
            root_dir: main.clone(),
            paths,
        };

        // Control: while the sub-project is a real (if storeless) directory,
        // the scan succeeds and reports the main store's pin. Without this the
        // assertion below would pass for a scan that always failed.
        let links = evidence_links(&scope(vec![main.clone(), sub.clone()]))
            .await
            .expect("a present but storeless path is an ordinary member of a scope");
        assert_eq!(links.pinned_sessions().len(), 1);

        std::fs::rename(&sub, tmp.path().join("sub-moved")).unwrap();
        let err = evidence_links(&scope(vec![main.clone(), sub.clone()]))
            .await
            .expect_err("a path that cannot be read was counted as holding no pins");
        assert!(
            err.to_string().contains("not a readable directory"),
            "the refusal must name the path it could not read: {err:#}"
        );
    }

    #[test]
    fn expiry_is_the_absence_of_bytes_not_the_absence_of_a_ledger_entry() {
        let mut by_session = BTreeMap::new();
        by_session.insert("gone".to_string(), vec!["m1".to_string()]);
        by_session.insert("here".to_string(), vec!["m2".to_string()]);
        let links = EvidenceLinks { by_session };

        let with_bytes: HashSet<String> = ["here".to_string()].into_iter().collect();
        let expired = expired_evidence(&links, &with_bytes);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].session_id, "gone");
        assert_eq!(expired[0].memory_ids, vec!["m1".to_string()]);

        // Control: with every copy present nothing is expired.
        let all: HashSet<String> = links.pinned_sessions();
        assert!(expired_evidence(&links, &all).is_empty());
    }

    #[test]
    fn citing_memories_counts_each_memory_once() {
        let mut by_session = BTreeMap::new();
        by_session.insert("a".to_string(), vec!["m1".to_string(), "m2".to_string()]);
        by_session.insert("b".to_string(), vec!["m1".to_string()]);
        let links = EvidenceLinks { by_session };
        assert_eq!(links.citing_memories(), 2);
        assert_eq!(links.pinned_sessions().len(), 2);
    }
}
