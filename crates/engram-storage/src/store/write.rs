//! Writes: create, update and delete of memory files and their index rows.

use super::*;

impl MemoryStore {
    /// Create a new memory.
    pub async fn create(&self, memory: &Memory) -> Result<String> {
        self.create_inner(memory, None).await
    }

    /// [`Self::create`] for a memory whose vectors are already computed: the
    /// file, then the chunks, then the index row with `has_embedding` already
    /// true — one memories-table commit instead of the two that `create`
    /// followed by a chunk upsert costs (the row, then its flag).
    ///
    /// `embed_sha256` describes the text `chunks` were embedded from, as for
    /// [`Self::upsert_chunks_if`]. Empty `chunks` is plain `create`.
    ///
    /// A crash between the chunks and the row leaves a file with vectors and
    /// no row: `doctor` reports the missing row and `reindex` rebuilds it,
    /// deriving `has_embedding` from chunk presence, so the vectors are kept.
    pub async fn create_with_chunks(
        &self,
        memory: &Memory,
        chunks: Vec<Vec<f32>>,
        embed_sha256: Option<String>,
    ) -> Result<String> {
        if chunks.is_empty() {
            return self.create_inner(memory, None).await;
        }
        self.create_inner(memory, Some((chunks, embed_sha256)))
            .await
    }

    async fn create_inner(
        &self,
        memory: &Memory,
        chunks: Option<(Vec<Vec<f32>>, Option<String>)>,
    ) -> Result<String> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let memories_dir = self.get_memories_dir(&memory.visibility)?;
        async_fs::create_dir_all(&memories_dir).await?;

        // Write memory file atomically (filename includes title slug if present)
        let filename = memory_file::memory_filename(memory);
        let file_path = memories_dir.join(&filename);
        let content = memory_file::write_memory_file(memory)?;
        // Stamp the identity of exactly the bytes we are about to write — not a
        // re-serialization, and no re-read. `atomic_write` writes `content`
        // verbatim, so this IS the on-disk identity.
        let digest = crate::digest::FileDigest::of(content.as_bytes());
        atomic_write(&file_path, &content).await?;

        // Sweep up any pre-existing file(s) for this exact ID — in BOTH
        // visibility directories — that are not the file we just wrote. Without
        // this, `create` for an ID that already exists under a *different*
        // visibility (worktree consolidation re-runs, or a personal/shared
        // re-create) would orphan the old file on disk while the index row
        // simply flips, diverging disk from the index (finding #1). New-file-
        // first ordering (the `atomic_write` above) means a reader never sees a
        // spurious NotFound. NotFound on removal is benign (already gone).
        //
        // Gate the sweep on a cheap index probe: every store-managed file has
        // an index row (create/update upsert one synchronously), so a fresh
        // ID — the overwhelmingly common case, since IDs are new UUIDs —
        // skips both full directory scans. This is what kept bulk creates
        // (worktree consolidation, imports) from being O(n²) in dirents. A
        // crash-orphaned file with no index row is doctor's territory either
        // way — the sweep never saw files the index didn't know about
        // re-created under colliding fresh UUIDs in practice.
        //
        // An equality probe, not the prefix `LIKE`: the question is about
        // this one whole id.
        let id_preexisting = self
            .lance_index
            .id_exists(&memory.id)
            .await
            .map_err(StorageError::index("id probe"))?;
        if id_preexisting {
            for dir in [
                self.get_memories_dir(&Visibility::Shared)?,
                self.get_memories_dir(&Visibility::Personal)?,
            ] {
                for old in find_memory_files(&dir, &memory.id).await? {
                    if old != file_path {
                        match async_fs::remove_file(&old).await {
                            Ok(()) => {}
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
            }
        }

        // Upsert metadata to LanceDB (vectors stored separately in chunks table).
        // A re-create of an existing memory (worktree consolidation, or a
        // personal/shared visibility flip — see the sweep above) must not reset
        // `has_embedding`: the chunks table is untouched here, so carry the
        // current chunk-presence state forward. A fresh `IndexEntry`'s default
        // `false` would otherwise drop an already-embedded memory from semantic
        // ranking (R3). A brand-new id has no row and therefore no chunks
        // (chunks are only ever written for an existing memory), so the
        // chunk probe is skipped for it: one index read per fresh create.
        let mut entry = IndexEntry::for_file(memory, &digest);
        entry.has_embedding = match chunks {
            Some((chunks, embed_sha256)) => {
                // Chunks before the row: the row is then born with the flag
                // set, and a crash in between is the recoverable direction
                // (see `create_with_chunks`).
                self.lance_index
                    .write_chunks(&memory.id, chunks, embed_sha256.as_deref())
                    .await
                    .map_err(StorageError::index("upsert_chunks"))?;
                true
            }
            None if id_preexisting => self
                .lance_index
                .has_chunks(&memory.id)
                .await
                .map_err(StorageError::index("has_chunks"))?,
            None => false,
        };
        self.lance_index
            .upsert(&entry)
            .await
            .map_err(StorageError::index("upsert"))?;
        self.sync_edges_locked(std::slice::from_ref(memory)).await?;

        // Update manifest stats. A fresh id adds exactly one row, which the
        // cheap path can verify; a re-create may have changed scopes on an
        // existing row, which only the scan can see.
        if id_preexisting {
            self.update_manifest_stats().await?;
        } else {
            self.update_manifest_stats_after(1, &memory.logical).await?;
        }

        Ok(memory.id.clone())
    }

    /// [`Self::create`] for many memories at once.
    ///
    /// Takes the per-project write lock **once**, writes every file, then
    /// performs a single ID probe, a single chunk-presence probe, a single
    /// index upsert and a single manifest refresh for the whole set.
    ///
    /// The per-memory version cost *four* separate O(store size) operations
    /// each — a prefix `LIKE` scan, the `has_chunks` query, the
    /// `merge_insert` commit, and the full-column scan behind
    /// `update_manifest_stats` — so creating W memories into a store of M was
    /// quadratic. Measured then: one `create` cost 24 ms into an empty store
    /// and 582 ms into a 1000-memory one. A single `create` of a fresh id is
    /// now one equality probe, one commit and a metadata row count, but it is
    /// still one commit (and one lock) per memory, which this avoids.
    ///
    /// Semantics are identical to calling [`Self::create`] in a loop, with two
    /// deliberate differences that both follow from batching:
    ///
    /// - The manifest is refreshed once at the end rather than after each
    ///   memory. Intermediate states were never observable anyway: the whole
    ///   loop ran under one lock in the caller's eyes, and the refresh is a
    ///   derived cache that `update_manifest_stats` recomputes from scratch.
    /// - The ID pre-existence probe reads the *filesystem* (one scan of both
    ///   visibility dirs) rather than the index. `create` gates its sweep on
    ///   an index probe reasoning that every store-managed file has an index
    ///   row; asking the filesystem answers the same question more directly,
    ///   and is what the sweep actually acts on. It is taken **before** any
    ///   file is written, so a memory in this batch cannot see its own new
    ///   file and mistake it for a pre-existing one.
    ///
    /// Ordering within the batch is preserved in the returned ids. Duplicate
    /// IDs within one call are not deduplicated — the later write wins, the
    /// same as consecutive `create` calls.
    pub async fn create_batch(&self, memories: &[Memory]) -> Result<Vec<String>> {
        if memories.is_empty() {
            return Ok(Vec::new());
        }

        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        // Probe BEFORE writing anything: after the writes every ID in the
        // batch would trivially "exist".
        let ids: Vec<&str> = memories.iter().map(|m| m.id.as_str()).collect();
        let preexisting = self.batch_exists(&ids).await?;

        // The index rows are built in a SECOND pass below (after one batched
        // chunk-presence probe), by which point each `content` string is long
        // out of scope — so the digests have to be carried forward rather than
        // recomputed. Recomputing would mean re-reading every file we just
        // wrote.
        let mut digests: HashMap<String, crate::digest::FileDigest> =
            HashMap::with_capacity(memories.len());

        for memory in memories {
            let memories_dir = self.get_memories_dir(&memory.visibility)?;
            async_fs::create_dir_all(&memories_dir).await?;

            let filename = memory_file::memory_filename(memory);
            let file_path = memories_dir.join(&filename);
            let content = memory_file::write_memory_file(memory)?;
            digests.insert(
                memory.id.clone(),
                crate::digest::FileDigest::of(content.as_bytes()),
            );
            atomic_write(&file_path, &content).await?;

            // Same cross-visibility sweep as `create`, on the same condition:
            // an ID that already had a file may have it under the *other*
            // visibility, or under a different title slug. New-file-first
            // ordering (the `atomic_write` above) means a concurrent reader
            // never sees a spurious NotFound. See `create` for the full
            // rationale; skipping this for fresh IDs is what keeps bulk
            // creates off the O(n^2) dirent path.
            if preexisting.contains(&memory.id) {
                for dir in [
                    self.get_memories_dir(&Visibility::Shared)?,
                    self.get_memories_dir(&Visibility::Personal)?,
                ] {
                    for old in find_memory_files(&dir, &memory.id).await? {
                        if old != file_path {
                            match async_fs::remove_file(&old).await {
                                Ok(()) => {}
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                                Err(e) => return Err(e.into()),
                            }
                        }
                    }
                }
            }
        }

        // One chunk-presence probe for the whole batch. As in `create`, a
        // re-create must not reset `has_embedding`: the chunks table is
        // untouched here, so carry the current presence forward or an
        // already-embedded memory silently drops out of semantic ranking
        // (R3). Fresh IDs are absent from this set, giving `false`.
        let with_chunks = self
            .lance_index
            .memory_ids_with_chunks(&ids)
            .await
            .map_err(StorageError::index("memory_ids_with_chunks"))?;

        let entries: Vec<IndexEntry> = memories
            .iter()
            .map(|m| {
                // Every id was inserted in the write loop above, so the lookup
                // cannot miss; `without_digest` is an unreachable fallback that
                // degrades to "unknown" rather than panicking on a future
                // refactor that breaks the pairing.
                let mut entry = match digests.get(&m.id) {
                    Some(digest) => IndexEntry::for_file(m, digest),
                    None => IndexEntry::without_digest(m),
                };
                entry.has_embedding = with_chunks.contains(&m.id);
                entry
            })
            .collect();

        self.lance_index
            .upsert_batch(&entries)
            .await
            .map_err(StorageError::index("upsert_batch"))?;
        self.sync_edges_locked(memories).await?;

        self.update_manifest_stats().await?;

        Ok(memories.iter().map(|m| m.id.clone()).collect())
    }

    /// Update a memory.
    ///
    /// Note: callers that first read the memory to compute the update perform
    /// an unlocked read-modify-write — a concurrent update can be lost. Use
    /// [`MemoryStore::update_with`] to make read-modify-write atomic.
    pub async fn update(&self, id: &str, updates: MemoryUpdate) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        // Get existing memory (inside the lock)
        let mut memory = self.get(id).await?;
        let old_visibility = memory.visibility;

        // Apply updates
        updates.apply_to(&mut memory);

        self.write_updated_locked(id, &memory, old_visibility).await
    }

    /// Atomically read-modify-write a memory.
    ///
    /// Acquires the per-project write lock, re-reads the memory inside the
    /// lock, applies `f` to it, then persists via the same write path as
    /// [`MemoryStore::update`]. This makes the whole read-merge-write sequence
    /// one critical section, so two concurrent `update_with` calls (or an
    /// `update_with` racing an `update`) cannot silently erase each other's
    /// changes.
    ///
    /// The memory's `updated_at` is bumped after `f` succeeds, matching the
    /// `MemoryUpdate::apply_to` behavior of `update`. If `f` returns an error,
    /// nothing is written and the error is surfaced as a validation error.
    ///
    /// Returns the memory exactly as persisted.
    pub async fn update_with<F>(&self, id: &str, f: F) -> Result<Memory>
    where
        F: FnOnce(&mut Memory) -> anyhow::Result<()>,
    {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        // Re-read inside the lock so `f` sees the latest persisted state.
        let mut memory = self.get(id).await?;
        let old_visibility = memory.visibility;

        f(&mut memory).map_err(|e| StorageError::Validation(format!("{:#}", e)))?;
        memory.mark_updated();

        self.write_updated_locked(id, &memory, old_visibility)
            .await?;

        Ok(memory)
    }

    /// [`Self::update_with`] for many memories under **one** lock, one batched
    /// read, one index upsert and one manifest-stats refresh.
    ///
    /// The per-memory version costs a lock, a directory scan, a `has_chunks`
    /// scan, an index commit and a full manifest-stats scan *each* — measured
    /// at 28 ms/memory at n=16 rising to 60 ms/memory at n=128, i.e.
    /// quadratic. Callers that mutate a set of memories (task completion,
    /// closing superseded validity windows) should use this.
    ///
    /// Semantics match `update_with` per entry: each memory is re-read inside
    /// the lock so `f` sees the latest persisted state, `updated_at` is bumped,
    /// and a failing closure fails only its own memory. Ids that do not resolve
    /// are reported rather than aborting the batch.
    ///
    /// **Prefix ids resolve, as they do for `update_with`.** [`Self::get_batch`]
    /// keys off the exact id, so anything shorter than a full id missed the map
    /// and would have been reported as "memory not found" — a silent divergence
    /// from the per-memory version this is documented to match. Resolution is a
    /// second pass over only the ids the batched read did not answer, so a
    /// caller passing full ids (all of them today) pays nothing for it.
    ///
    /// Returns `(updated_ids, per_id_errors)`, where the updated ids are the
    /// **canonical** ones — `update_with` returns the persisted `Memory`, whose
    /// id is canonical, so a caller that reports what it changed must not be
    /// handed back the prefix it happened to pass in.
    pub async fn update_batch_with<F>(
        &self,
        ids: &[String],
        mut f: F,
    ) -> Result<(Vec<String>, Vec<(String, String)>)>
    where
        F: FnMut(&mut Memory) -> anyhow::Result<()>,
    {
        if ids.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut current: HashMap<String, Memory> =
            self.get_batch(&id_refs).await?.into_iter().collect();

        // Second pass for the ids the exact-keyed batch read could not answer.
        // `get` is what `update_with` calls, so a prefix resolves by exactly
        // the same rules (including "an exact full-id match beats prefix
        // ambiguity") rather than by a second, subtly different implementation.
        // Empty in the common case — every caller today passes full ids — so
        // the directory scan it costs is paid only by a batch that would
        // otherwise have reported a resolvable id as missing.
        let unresolved: Vec<&String> = ids.iter().filter(|id| !current.contains_key(*id)).collect();
        let mut aliases: HashMap<String, String> = HashMap::new();
        for id in unresolved {
            let Ok(memory) = self.get(id).await else {
                continue;
            };
            let full = memory.id.clone();
            // Two spellings of one memory in the same batch: alias the prefix
            // onto the row already read rather than keeping both, so `f` still
            // sees each memory exactly once — running the caller's mutation
            // twice, the second time against a copy that predates the first
            // write, is a lost update.
            current.entry(full.clone()).or_insert(memory);
            aliases.insert(id.clone(), full);
        }

        let mut updated: Vec<String> = Vec::new();
        let mut errors: Vec<(String, String)> = Vec::new();
        let mut entries: Vec<IndexEntry> = Vec::new();
        let mut written: Vec<Memory> = Vec::new();
        let mut probe_error: Option<StorageError> = None;

        for given in ids {
            let key = aliases.get(given).unwrap_or(given);
            let Some(mut memory) = current.remove(key) else {
                errors.push((given.clone(), "memory not found".to_string()));
                continue;
            };
            // Errors stay keyed by the id the caller passed — that is the
            // string it can act on — while `updated` reports the canonical one.
            let canonical = memory.id.clone();
            let old_visibility = memory.visibility;
            if let Err(e) = f(&mut memory) {
                errors.push((given.clone(), format!("{:#}", e)));
                continue;
            }
            memory.mark_updated();

            // File writes stay per-memory: each is an atomic rename to its own
            // path, and the rename-on-title-change cleanup is inherently
            // per-file. It is the LanceDB commits and the manifest scan that
            // were quadratic, not this.
            let digest = match self
                .write_updated_file_locked(&canonical, &memory, old_visibility)
                .await
            {
                Ok(digest) => digest,
                Err(e) => {
                    errors.push((given.clone(), format!("{:#}", e)));
                    continue;
                }
            };

            let mut entry = IndexEntry::for_file(&memory, &digest);
            // The file is already written. Returning on a failed probe left
            // this file — and every file written earlier in the batch —
            // without an index row. Record the row as un-embedded (a reindex
            // derives the flag from chunk presence again), finish the batch's
            // index writes, and report the error afterwards.
            entry.has_embedding = match self.probe_has_chunks(&memory.id).await {
                Ok(has) => has,
                Err(e) => {
                    probe_error.get_or_insert(e);
                    false
                }
            };
            entries.push(entry);
            written.push(memory);
            updated.push(canonical);
        }

        if !entries.is_empty() {
            self.lance_index
                .upsert_batch(&entries)
                .await
                .map_err(StorageError::index("upsert_batch"))?;
            self.sync_edges_locked(&written).await?;
            // One refresh for the whole batch instead of one per memory.
            self.update_manifest_stats().await?;
        }
        if let Some(e) = probe_error {
            return Err(e);
        }

        Ok((updated, errors))
    }

    /// Close a memory's validity window (§2.4): set `invalidated_at = now`
    /// and, when the closure was caused by supersession, the ADR-style
    /// reverse link `superseded_by`. The memory is retained on disk and
    /// queryable via `include_invalidated`, but excluded from default
    /// retrieval. A no-op error-free path is deliberately NOT provided for
    /// already-invalidated memories — the caller decides whether to skip
    /// (ops-level supersession logs and skips; see `is_invalidated_at`).
    ///
    /// Built on [`MemoryStore::update_with`], so the read-modify-write is one
    /// critical section under the per-project write lock and `updated_at` is
    /// bumped. Reopening remains possible via a plain `update_with` clearing
    /// the two fields — invalidation is reversible, unlike deletion.
    pub async fn invalidate_with(
        &self,
        id: &str,
        superseded_by: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Memory> {
        self.update_with(id, |memory| {
            memory.invalidated_at = Some(now);
            memory.superseded_by = superseded_by;
            Ok(())
        })
        .await
    }

    /// Shared write path for `update` / `update_with`. Callers MUST hold the
    /// per-project write lock — this helper does not (re-)acquire it, because
    /// one acquisition must span the entire read-modify-write critical
    /// section.
    async fn write_updated_locked(
        &self,
        id: &str,
        memory: &Memory,
        old_visibility: Visibility,
    ) -> Result<()> {
        let digest = self
            .write_updated_file_locked(id, memory, old_visibility)
            .await?;
        self.sync_index_row_locked(memory, &digest).await
    }

    /// The file half of [`Self::write_updated_locked`]: locate the old
    /// file(s), atomically write the new one, remove any stale path.
    ///
    /// Split out so [`Self::update_batch_with`] reuses this exact code rather
    /// than reimplementing the durability ordering below — the ordering is
    /// subtle enough that a second copy would be a latent data-loss bug.
    ///
    /// Returns the identity of the bytes it wrote, so the index half stamps the
    /// row from the same string rather than re-reading (or, worse,
    /// re-serializing) the file.
    async fn write_updated_file_locked(
        &self,
        id: &str,
        memory: &Memory,
        old_visibility: Visibility,
    ) -> Result<crate::digest::FileDigest> {
        // Locate the existing file(s) for this memory before writing. `id`
        // may be a prefix; the filename may be about to change (title or
        // visibility update); and a stale duplicate from a previously
        // crashed update may also still be present.
        let old_dir = self.get_memories_dir(&old_visibility)?;
        let old_paths = find_memory_files(&old_dir, id).await?;

        let memories_dir = self.get_memories_dir(&memory.visibility)?;
        async_fs::create_dir_all(&memories_dir).await?;
        let filename = memory_file::memory_filename(memory);
        let file_path = memories_dir.join(&filename);
        let content = memory_file::write_memory_file(memory)?;
        let digest = crate::digest::FileDigest::of(content.as_bytes());

        // Durability ordering: write the NEW file first, THEN remove any old
        // file at a different path. In the common case (filename unchanged)
        // the atomic_write replaces the file in place and nothing is deleted,
        // so the memory is never absent from disk and lock-free readers can
        // never observe a spurious NotFound. When the filename changed
        // (title/visibility update), a crash between the write and the
        // removal leaves at worst TWO files for one ID — readers resolve
        // that to the newest mtime (see `locate_memory_file`) and the next
        // update/delete/reindex cleans the stale one up. The old
        // delete-then-write order could lose the memory's file entirely.
        atomic_write(&file_path, &content).await?;
        for old in &old_paths {
            if *old != file_path {
                match async_fs::remove_file(old).await {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        // An explicit write to this id also heals a cross-directory
        // duplicate: a copy of the same id in the OTHER visibility directory
        // (a `git checkout` restoring a tracked shared file after the memory
        // went personal, a crashed visibility flip) would otherwise keep
        // competing with the file just written. `create` sweeps both
        // directories the same way.
        for dir in [
            self.get_memories_dir(&Visibility::Shared)?,
            self.get_memories_dir(&Visibility::Personal)?,
        ] {
            if dir == old_dir {
                continue;
            }
            for other in find_memory_files(&dir, &memory.id).await? {
                if other == file_path {
                    continue;
                }
                match async_fs::remove_file(&other).await {
                    Ok(()) => tracing::warn!(
                        "removed {}: a second copy of memory {} in the other visibility \
                         directory, replaced by this update",
                        other.display(),
                        memory.id
                    ),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }

        Ok(digest)
    }

    /// The index half of [`Self::write_updated_locked`].
    async fn sync_index_row_locked(
        &self,
        memory: &Memory,
        digest: &crate::digest::FileDigest,
    ) -> Result<()> {
        // Upsert metadata to LanceDB (chunks are managed separately). An update
        // must not reset `has_embedding`: the memory may already have chunks
        // that this update isn't touching. Carry the current chunk-presence
        // state forward (a content-changing update's re-embed will set it true
        // again via `upsert_chunks` regardless).
        let mut entry = IndexEntry::for_file(memory, digest);
        // Propagate a chunk-presence read error rather than defaulting to
        // `false`: silently clearing the flag would drop a still-embedded memory
        // from `has_embedding`-gated semantic ranking (R3) until the next reindex.
        entry.has_embedding = self
            .lance_index
            .has_chunks(&memory.id)
            .await
            .map_err(StorageError::index("has_chunks"))?;
        self.lance_index
            .upsert(&entry)
            .await
            .map_err(StorageError::index("upsert"))?;
        self.sync_edges_locked(std::slice::from_ref(memory)).await?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(())
    }

    /// Delete a memory.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        // Resolve full ID first via the index
        let full_id = self.resolve_full_id(id).await?;

        self.delete_locked(&full_id).await
    }

    /// Conditionally delete a memory under the per-project write lock.
    ///
    /// Re-reads the memory *inside* the lock and calls `predicate` on the
    /// fresh state, so callers that scored or inspected the memory earlier
    /// (lock-free) can re-validate that decision against the latest persisted
    /// state before destroying data — closing the check-then-delete TOCTOU
    /// window that a plain [`Self::delete`] after an unlocked read leaves
    /// open.
    ///
    /// Returns:
    /// - `Ok(true)` — predicate returned `true` and the memory was deleted.
    /// - `Ok(false)` — the memory was kept (predicate returned `false`), or
    ///   it no longer exists (concurrently deleted, or its data file is
    ///   missing). Missing memories are a skip, never an error.
    pub async fn delete_if<F>(&self, id: &str, predicate: F) -> Result<bool>
    where
        F: FnOnce(&Memory) -> bool,
    {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let full_id = match self.resolve_full_id(id).await {
            Ok(full_id) => full_id,
            Err(StorageError::NotFound(_)) => return Ok(false),
            Err(e) => return Err(e),
        };

        // Fresh read inside the lock — this is the state the predicate
        // judges. A missing data file (stale index entry) is also a skip.
        let memory = match self.get(&full_id).await {
            Ok(m) => m,
            Err(StorageError::NotFound(_)) => return Ok(false),
            Err(e) => return Err(e),
        };

        if !predicate(&memory) {
            return Ok(false);
        }

        self.delete_locked(&full_id).await?;
        Ok(true)
    }

    /// [`Self::delete_if`] for many ids under **one** lock, one index delete,
    /// one chunk delete and one manifest-stats refresh.
    ///
    /// The per-id version costs a lock, an id resolution scan, a directory
    /// scan, two LanceDB commits and a full manifest-stats scan *each* — the
    /// heaviest per-item primitive in the store, and GC calls it once per
    /// candidate. On a sweep that is quadratic.
    ///
    /// The predicate contract is unchanged: every memory is re-read inside
    /// the same lock that performs the deletion and judged individually, so a
    /// concurrently-modified memory is skipped exactly as before. Returns
    /// `(deleted_ids, skipped_ids)`, both in input order.
    pub async fn delete_batch_if<F>(
        &self,
        ids: &[String],
        mut predicate: F,
    ) -> Result<(Vec<String>, Vec<String>)>
    where
        F: FnMut(&Memory) -> bool,
    {
        if ids.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let current: HashMap<String, Memory> =
            self.get_batch(&id_refs).await?.into_iter().collect();

        let mut to_delete: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for id in ids {
            match current.get(id) {
                // A memory whose file vanished (stale index entry) is a skip,
                // matching `delete_if`'s NotFound handling.
                None => skipped.push(id.clone()),
                Some(memory) if predicate(memory) => to_delete.push(id.clone()),
                Some(_) => skipped.push(id.clone()),
            }
        }
        if to_delete.is_empty() {
            return Ok((Vec::new(), skipped));
        }

        // Files first: the index row is the recovery hint if we crash midway
        // (a reindex rebuilds the row from the file, so an orphaned row is
        // repairable while an orphaned file would resurrect on reindex).
        for id in &to_delete {
            self.delete_files_in_both_dirs(id).await?;
        }

        self.lance_index
            .delete_batch(&to_delete)
            .await
            .map_err(StorageError::index("delete_batch"))?;
        self.lance_index
            .delete_chunks_batch(&to_delete)
            .await
            .map_err(StorageError::index("delete_chunks_batch"))?;
        self.delete_edges_locked(&to_delete).await?;

        // One refresh for the whole sweep instead of one per deletion.
        self.update_manifest_stats().await?;

        Ok((to_delete, skipped))
    }

    /// Shared deletion path for `delete` / `delete_if`. Callers MUST hold the
    /// per-project write lock and pass a fully resolved ID — this helper does
    /// not (re-)acquire the lock (`flock(2)` on a second fd in the same
    /// process would deadlock).
    async fn delete_locked(&self, full_id: &str) -> Result<()> {
        // Try to delete file from shared. Only a NotFound falls through to
        // the personal dir — a real I/O error must propagate as-is, not be
        // masked by the personal lookup's NotFound.
        self.delete_files_in_both_dirs(full_id).await?;

        // Delete from LanceDB (metadata + chunks)
        self.lance_index
            .delete(full_id)
            .await
            .map_err(StorageError::index("delete"))?;
        self.lance_index
            .delete_chunks(full_id)
            .await
            .map_err(StorageError::index("delete_chunks"))?;
        self.delete_edges_locked(&[full_id.to_string()]).await?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(())
    }
}
