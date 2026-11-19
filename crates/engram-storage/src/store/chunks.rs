//! The chunks (vectors) table and the content and embed digests.

use super::*;

impl MemoryStore {
    /// Drop and recreate the chunks (vectors) table.
    ///
    /// Destroys all embedding vectors and recreates the table with the
    /// currently configured dimensions. Only call this when a full re-embed
    /// is about to follow (i.e. an embedding provider is confirmed
    /// available) — see [`Self::reindex`] for the vector-preserving path.
    pub async fn clear_chunks(&self) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .clear_chunks()
            .await
            .map_err(StorageError::index("clear_chunks"))
    }

    /// [`Self::clear_chunks`], keeping the vectors (and `has_embedding`) of
    /// the memories in `keep` — the unparseable files of a forced re-embed
    /// under an unchanged model. See `LanceIndex::clear_chunks_keeping`.
    pub async fn clear_chunks_keeping(&self, keep: &[String]) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .clear_chunks_keeping(keep)
            .await
            .map_err(StorageError::index("clear_chunks_keeping"))
    }

    /// The vector width of the on-disk chunks table, or `None` when the
    /// table doesn't exist yet. See `LanceIndex::chunks_table_dimensions`.
    pub async fn chunks_table_dimensions(&self) -> Result<Option<usize>> {
        self.lance_index
            .chunks_table_dimensions()
            .await
            .map_err(StorageError::index("chunks schema read"))
    }

    /// Upsert embedding chunks for a memory, recording no provenance for them.
    ///
    /// The un-digested entry point: the vectors are stored with a null
    /// `embed_sha256`, which every currency check reads as *unknown* and
    /// therefore "re-embed when asked". That is the correct answer for its two
    /// real callers — worktree consolidation, which **relocates** vectors it did
    /// not compute (and cannot compute a digest for: the source store has its
    /// own config, so its model, dimensions, composition and chunk size may all
    /// differ), and tests. The digest-carrying paths are
    /// [`Self::upsert_chunks_if`] and [`Self::upsert_chunks_batch_if`],
    /// which the retrieval engine drives.
    pub async fn upsert_chunks(&self, memory_id: &str, chunks: Vec<Vec<f32>>) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .upsert_chunks(memory_id, chunks, None)
            .await
            .map_err(StorageError::index("upsert_chunks"))
    }

    /// Upsert embedding chunks only if the memory, re-read under the write
    /// lock, still passes `still_current`. Returns `true` when the chunks were
    /// written.
    ///
    /// Detached ingest tasks embed a `Memory` snapshot with no ordering
    /// guarantee: two rapid updates spawn two tasks whose `upsert_chunks`
    /// commits the flock serializes but does not order, so the task carrying
    /// the OLDER content can commit last (vectors then describe v1 while the
    /// file holds v2 until the next update/reindex); and a create-then-delete
    /// leaves a late embed re-inserting chunks for a deleted memory (orphan
    /// rows occupying top-k slots until doctor/reindex prunes them). This
    /// re-reads the memory UNDER the write lock and skips the write when the
    /// memory is gone or `still_current` refuses it.
    ///
    /// The predicate belongs to the caller because what the vectors depend on
    /// (the embedding composition) lives above this crate. The engine passes
    /// "the memory still embeds to the same texts", so a metadata-only write
    /// (a link, a challenge, `harvest mark --memory`) racing the embed no
    /// longer drops its vectors — comparing `updated_at` did.
    ///
    /// `embed_sha256` describes the text `chunks` were computed from and is
    /// written in the same `RecordBatch` as the vectors, so it can never end up
    /// describing a different snapshot than the vectors beside it.
    pub async fn upsert_chunks_if<F>(
        &self,
        memory_id: &str,
        chunks: Vec<Vec<f32>>,
        embed_sha256: Option<String>,
        still_current: F,
    ) -> Result<bool>
    where
        F: FnOnce(&Memory) -> bool + Send,
    {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        match self.get(memory_id).await {
            Ok(current) if still_current(&current) => {}
            Ok(_) => {
                tracing::info!(
                    "skipping stale chunk upsert for {memory_id}: its text changed since the snapshot"
                );
                return Ok(false);
            }
            Err(StorageError::NotFound(_)) => {
                tracing::info!("skipping chunk upsert for deleted memory {memory_id}");
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
        self.lance_index
            .upsert_chunks(memory_id, chunks, embed_sha256.as_deref())
            .await
            .map_err(StorageError::index("upsert_chunks"))?;
        Ok(true)
    }

    /// [`Self::upsert_chunks_if`] for many memories at once.
    ///
    /// Takes the per-project write lock **once**, re-reads every memory in a
    /// single batched directory scan, drops the entries `still_current`
    /// refuses (called with the re-read memory and the entry's digest) or that
    /// vanished, and writes what is left in one merge_insert. Returns the ids
    /// actually written.
    pub async fn upsert_chunks_batch_if<F>(
        &self,
        entries: Vec<crate::lance_index::ChunkWrite>,
        still_current: F,
    ) -> Result<Vec<String>>
    where
        F: Fn(&Memory, Option<&str>) -> bool + Send,
    {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let ids: Vec<&str> = entries.iter().map(|(id, _, _)| id.as_str()).collect();
        let current: HashMap<String, Memory> = self.get_batch(&ids).await?.into_iter().collect();

        let mut fresh: Vec<crate::lance_index::ChunkWrite> = Vec::with_capacity(entries.len());
        for (id, chunks, digest) in entries {
            match current.get(&id) {
                Some(mem) if still_current(mem, digest.as_deref()) => {
                    fresh.push((id, chunks, digest))
                }
                Some(_) => tracing::info!(
                    "skipping stale chunk upsert for {id}: its text changed since the snapshot"
                ),
                None => tracing::info!("skipping chunk upsert for deleted memory {id}"),
            }
        }
        if fresh.is_empty() {
            return Ok(Vec::new());
        }

        self.lance_index
            .upsert_chunks_batch(&fresh)
            .await
            .map_err(StorageError::index("upsert_chunks_batch"))?;
        Ok(fresh.into_iter().map(|(id, _, _)| id).collect())
    }

    /// The `updated_at`-guarded batch write: like
    /// [`Self::upsert_chunks_batch_if`], but an entry is written only while the
    /// memory's `updated_at` still equals the entry's snapshot.
    ///
    /// Kept for worktree consolidation, which **relocates** vectors it did not
    /// compute and so has no digest or composition to compare; any metadata
    /// write in between makes it skip, which there only costs a re-embed. The
    /// engine's own writes use [`Self::upsert_chunks_batch_if`].
    ///
    /// The per-memory version costs a lock, a full directory scan and two
    /// LanceDB commits *each*, so re-embedding a whole store through it is
    /// quadratic. The freshness guarantee is unchanged: every surviving entry
    /// was verified inside the same lock that performs the write.
    ///
    /// Returns the ids actually written.
    pub async fn upsert_chunks_batch(
        &self,
        entries: Vec<crate::lance_index::GuardedChunkWrite>,
    ) -> Result<Vec<String>> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let ids: Vec<&str> = entries.iter().map(|(id, _, _, _)| id.as_str()).collect();
        let current: HashMap<String, Memory> = self.get_batch(&ids).await?.into_iter().collect();

        let mut fresh: Vec<crate::lance_index::ChunkWrite> = Vec::with_capacity(entries.len());
        for (id, snapshot_updated_at, chunks, digest) in entries {
            match current.get(&id) {
                Some(mem) if mem.updated_at == snapshot_updated_at => {
                    fresh.push((id, chunks, digest))
                }
                Some(_) => tracing::debug!(
                    "skipping stale chunk upsert for {id}: memory changed since snapshot"
                ),
                None => tracing::debug!("skipping chunk upsert for deleted memory {id}"),
            }
        }
        if fresh.is_empty() {
            return Ok(Vec::new());
        }

        self.lance_index
            .upsert_chunks_batch(&fresh)
            .await
            .map_err(StorageError::index("upsert_chunks_batch"))?;
        Ok(fresh.into_iter().map(|(id, _, _)| id).collect())
    }

    /// Whether the chunks table holds any row for `id`.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn has_chunks_for(&self, id: &str) -> Result<bool> {
        self.lance_index
            .has_chunks(id)
            .await
            .map_err(StorageError::index("has_chunks"))
    }

    /// Null the `keyword_stems` column of these rows (a pre-0.5.0 row shape),
    /// so a fixture can reach the query path's file fallback.
    /// The memories table's version (one per commit), for tests that count
    /// a write path's commits from outside this crate.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn memories_version_for_test(&self) -> Result<u64> {
        self.lance_index
            .memories_version()
            .await
            .map_err(StorageError::index("memories_version"))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn null_keyword_stems_for_test(&self, ids: &[String]) -> Result<()> {
        self.lance_index
            .null_keyword_stems_for_test(ids)
            .await
            .map_err(StorageError::index("null_keyword_stems"))
    }

    /// Write an index row directly, bypassing the file write.
    ///
    /// Test-only: production always derives the row from the file it just
    /// wrote, which is what keeps the content digest honest. Exposed for
    /// fixtures that need to reproduce a row shape no current write path
    /// produces (e.g. a pre-0.8.0 row with no digest).
    #[cfg(any(test, feature = "test-support"))]
    pub async fn upsert_index_entry_for_test(
        &self,
        entry: &crate::lance_index::IndexEntry,
    ) -> Result<()> {
        self.lance_index
            .upsert(entry)
            .await
            .map_err(StorageError::index("upsert"))
    }

    /// Every indexed row's content digest (schema v0.8.0).
    ///
    /// See [`crate::lance_index::LanceIndex::list_digests`].
    pub async fn index_digests(&self) -> Result<Vec<crate::lance_index::IndexDigest>> {
        self.lance_index
            .list_digests()
            .await
            .map_err(StorageError::index("list_digests"))
    }

    /// Every memory's on-disk digest, computed in **two directory scans total**
    /// rather than two per memory.
    ///
    /// This is the batched form of [`Self::read_memory_bytes`] and exists
    /// because the unbatched one is a trap at scale: it resolves each id with
    /// `find_memory_files`, which is a full `read_dir` of both the shared and
    /// personal directories. Calling it in a loop over every row — which is
    /// exactly what a whole-store drift check does — makes the check quadratic
    /// in dirent work. `stats` runs one on every invocation, so the cost is
    /// paid by a command that used to do no file I/O at all.
    ///
    /// Returns `(digests, unreadable)`. A file that exists but cannot be read
    /// lands in `unreadable`, never silently in the clean set: the whole point
    /// of the distinction in [`Self::read_memory_bytes`].
    pub async fn file_digests(&self) -> Result<(HashMap<String, FileDigest>, Vec<String>)> {
        let shared = scan_dir_to_stats(&paths::memories_dir(&self.project_dir)).await;
        let personal = scan_dir_to_stats(&paths::personal_memories_dir(&self.project_id)?).await;

        let mut digests = HashMap::new();
        let mut unreadable = Vec::new();
        for (id, stat) in merge_cross_dir(shared, personal).await.by_id {
            match async_fs::read(&stat.path).await {
                Ok(bytes) => {
                    digests.insert(id, FileDigest::of(&bytes));
                }
                Err(e) => {
                    tracing::warn!("cannot read memory file {}: {e}", stat.path.display());
                    unreadable.push(id);
                }
            }
        }
        unreadable.sort();
        Ok((digests, unreadable))
    }

    /// The raw bytes of a memory's `.md` file: `Ok(None)` when no file exists,
    /// `Err` when one exists but cannot be read.
    ///
    /// The distinction is the point. A currency check that folded an unreadable
    /// file into "absent" would report it as clean — an assertion of health it
    /// cannot support — while a missing file is a different finding that
    /// `doctor` already reports as a stale index entry.
    ///
    /// **Do not call this in a loop over the store.** Each call is a full
    /// `read_dir` of two directories; use [`Self::file_digests`] for a
    /// whole-store pass.
    pub async fn read_memory_bytes(&self, id: &str) -> Result<Option<Vec<u8>>> {
        for visibility in [Visibility::Shared, Visibility::Personal] {
            let dir = self.get_memories_dir(&visibility)?;
            let mut paths = find_memory_files(&dir, id).await?;
            // Newest wins, matching `locate_memory_file`'s duplicate resolution: the
            // row describes whichever file a reader would resolve to.
            if paths.len() > 1 {
                let mut with_mtime: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
                for p in paths {
                    with_mtime.push((file_mtime(&p).await, p));
                }
                with_mtime.sort_by_key(|(mtime, _)| std::cmp::Reverse(*mtime));
                paths = with_mtime.into_iter().map(|(_, p)| p).collect();
            }
            if let Some(path) = paths.first() {
                return async_fs::read(path).await.map(Some).map_err(Into::into);
            }
        }
        Ok(None)
    }

    /// Every embedded memory's `embed_sha256` (schema v0.8.0), keyed by memory
    /// id. See [`crate::lance_index::LanceIndex::list_embed_digests`].
    pub async fn embed_digests(&self) -> Result<HashMap<String, Option<String>>> {
        self.lance_index
            .list_embed_digests()
            .await
            .map_err(StorageError::index("list_embed_digests"))
    }

    /// Delete all embedding chunks for a memory.
    pub async fn delete_chunks(&self, memory_id: &str) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.lance_index
            .delete_chunks(memory_id)
            .await
            .map_err(StorageError::index("delete_chunks"))
    }

    /// List all distinct memory_ids present in the chunks table.
    pub async fn list_chunk_memory_ids(&self) -> Result<Vec<String>> {
        self.lance_index
            .list_chunk_memory_ids()
            .await
            .map_err(StorageError::index("list_chunk_memory_ids"))
    }

    /// Read every embedding chunk for `memory_id`, ordered by chunk index.
    ///
    /// Empty when the memory was never embedded. Used to relocate vectors
    /// during worktree consolidation so migrated memories stay searchable.
    pub async fn export_chunks(&self, memory_id: &str) -> Result<Vec<Vec<f32>>> {
        self.lance_index
            .chunks_for_memory(memory_id)
            .await
            .map_err(StorageError::index("chunks_for_memory"))
    }

    /// [`Self::export_chunks`] for many memories in one scan, keyed by id.
    ///
    /// Mirrors [`Self::get_batch`], and for the same reason: the per-item form
    /// re-opens the dataset and re-plans the query on every call, so a loop
    /// over it is O(n²). Ids with no stored vectors are absent from the map.
    pub async fn export_chunks_batch<S: AsRef<str>>(
        &self,
        ids: &[S],
    ) -> Result<HashMap<String, Vec<Vec<f32>>>> {
        let refs: Vec<&str> = ids.iter().map(AsRef::as_ref).collect();
        self.lance_index
            .chunks_for_memories(&refs)
            .await
            .map_err(StorageError::index("chunks_for_memories"))
    }

    /// Perform vector similarity search.
    ///
    /// `restrict_to` optionally pushes a candidate `memory_id` set down into
    /// the LanceDB predicate so the top-k window isn't saturated by memories
    /// the caller has already filtered out. `Some(&[])` returns no matches;
    /// `None` searches the whole store.
    pub async fn vector_search(
        &self,
        query: Vec<f32>,
        limit: usize,
        restrict_to: Option<&[String]>,
    ) -> Result<Vec<VectorMatch>> {
        self.lance_index
            .vector_search(query, limit, restrict_to)
            .await
            .map_err(StorageError::index("vector_search"))
    }
}
