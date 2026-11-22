//! Reads: get by id and the index listings, counts and table maintenance.

use super::*;

impl MemoryStore {
    /// Get a memory by ID (supports prefix matching).
    ///
    /// When the same id has a file in both the shared and the personal
    /// directory, [`cross_dir_winner`] decides which one is the memory — the
    /// same rule every other reader and the index use.
    pub async fn get(&self, id: &str) -> Result<Memory> {
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        let shared = locate_memory_file(&shared_dir, id).await?;
        let personal = match locate_memory_file(&personal_dir, id).await {
            Ok(found) => found,
            // A shared hit used to answer without looking at the personal
            // directory at all; a problem there must not fail it now.
            Err(_) if shared.is_some() => None,
            Err(e) => return Err(e),
        };

        let candidates: Vec<&PathBuf> = match (&shared, &personal) {
            (Some(s), Some(p)) if stem_id(s) == stem_id(p) => match cross_dir_winner(s, p).await {
                CrossDirWinner::Shared => vec![s, p],
                CrossDirWinner::Personal => vec![p, s],
            },
            // A prefix naming different ids in the two directories: shared
            // first, as before.
            (Some(s), Some(p)) => vec![s, p],
            (Some(s), None) => vec![s],
            (None, Some(p)) => vec![p],
            (None, None) => Vec::new(),
        };
        // A file removed between the scan and the read (a concurrent
        // rename-update or delete) falls through to the next candidate.
        for path in candidates {
            match Self::read_found_file(path, id).await {
                Err(StorageError::NotFound(_)) => continue,
                other => return other,
            }
        }
        Err(StorageError::NotFound(id.to_string()))
    }

    /// Every id that has a file in both the shared and the personal
    /// directory, with the copy that wins. Read-only; for `doctor`.
    pub async fn cross_dir_duplicates(&self) -> Result<Vec<CrossDirDuplicate>> {
        let shared = scan_dir_to_stats(&paths::memories_dir(&self.project_dir)).await;
        let personal = scan_dir_to_stats(&paths::personal_memories_dir(&self.project_id)?).await;
        let mut duplicates = merge_cross_dir(shared, personal).await.duplicates;
        duplicates.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(duplicates)
    }

    /// Get multiple memories by their full IDs in a single batch.
    ///
    /// Performs one directory scan of shared and personal memory dirs,
    /// then reads only the requested files.  This is O(dir_size + N)
    /// instead of O(dir_size × N) for N individual [`get`] calls.
    ///
    /// Returns a Vec of `(id, Memory)` pairs.  IDs that cannot be loaded
    /// (missing file, parse error) are silently skipped.
    pub async fn get_batch<S: AsRef<str>>(&self, ids: &[S]) -> Result<Vec<(String, Memory)>> {
        if ids.is_empty() {
            // No directory scans for nothing to read.
            return Ok(Vec::new());
        }
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;

        // Only the requested ids are kept from the scans: a store's
        // directory holds every memory, a batch names a few of them.
        let wanted: HashSet<&str> = ids.iter().map(AsRef::as_ref).collect();
        let shared_map = scan_dir_to_map(&shared_dir, &wanted).await;
        let personal_map = scan_dir_to_map(&personal_dir, &wanted).await;

        // Overlap the per-file reads (bounded) instead of awaiting each in
        // sequence — this backs the retrieval hot path, where serializing
        // dozens of read latencies adds up. `buffered` (not
        // `buffer_unordered`) preserves the caller's id order. The (id, path)
        // pairs are materialized first — an owned Vec, not a lazy iterator —
        // which also sidesteps the higher-ranked lifetime bound the stream
        // adapters would otherwise demand of the map-lookup closure.
        use futures_util::{stream, StreamExt};
        let mut to_read: Vec<(String, PathBuf)> = Vec::with_capacity(ids.len());
        for id in ids {
            let id_str = id.as_ref();
            let path = match (shared_map.get(id_str), personal_map.get(id_str)) {
                // In both directories: the same winner `get` and the index pick.
                (Some(s), Some(p)) => match cross_dir_winner(s, p).await {
                    CrossDirWinner::Shared => s,
                    CrossDirWinner::Personal => p,
                },
                (Some(path), None) | (None, Some(path)) => path,
                (None, None) => continue,
            };
            to_read.push((id_str.to_owned(), path.clone()));
        }
        let read: Vec<(String, PathBuf, String)> = stream::iter(to_read)
            .map(|(id_str, path)| async move {
                // An indexed memory whose file is unreadable/unparseable is a
                // data-integrity problem. Drop it (as before, so one bad file
                // doesn't fail a whole batch) but `warn!` rather than swallow it
                // silently, matching `reindex_dir`'s handling (finding #15).
                match async_fs::read_to_string(&path).await {
                    Ok(content) => Some((id_str, path, content)),
                    Err(e) => {
                        tracing::warn!(
                            "get_batch: skipping {} ({}): failed to read: {}",
                            id_str,
                            path.display(),
                            e
                        );
                        None
                    }
                }
            })
            .buffered(PARALLEL_READ_WIDTH)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flatten()
            .collect();

        Ok(parse_batch(read))
    }

    /// Check which of the given IDs have `.md` files on disk.
    ///
    /// Scans shared and personal directories once each, returning only
    /// those IDs that have a corresponding file.  Much cheaper than
    /// [`get_batch`] because no files are read or parsed.
    pub async fn batch_exists<S: AsRef<str>>(&self, ids: &[S]) -> Result<HashSet<String>> {
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;

        let mut on_disk = HashSet::new();
        collect_stems(&shared_dir, &mut on_disk).await;
        collect_stems(&personal_dir, &mut on_disk).await;

        Ok(ids
            .iter()
            .filter(|id| on_disk.contains(id.as_ref()))
            .map(|id| id.as_ref().to_owned())
            .collect())
    }

    /// Read + parse a memory file found by a directory scan.
    ///
    /// Reads are lock-free, so a concurrent rename-update (title/visibility
    /// change writes the new file then unlinks the old) or delete can remove
    /// the scanned path before we read it. That is the documented `NotFound`
    /// condition — `get` relies on it to fall through to the personal dir —
    /// not an opaque I/O failure.
    async fn read_found_file(path: &Path, id: &str) -> Result<Memory> {
        match async_fs::read_to_string(path).await {
            Ok(content) => memory_file::parse_memory_file_from(&content, path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(StorageError::NotFound(id.to_string()))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Index rows for exactly `ids` (full ids; unknown ids are simply
    /// absent), in `IN` lists of at most 500. Read-only. The rows carry no
    /// `keyword_stems` (`FilterProjection::NoStems`).
    pub async fn list_for_filtering_by_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<IndexForFiltering>> {
        use lancedb::expr::{col, is_in, lit};
        let mut out = Vec::new();
        for chunk in ids.chunks(500) {
            let expr = is_in(col("id"), chunk.iter().map(lit).collect());
            // No caller of this reads the stems.
            out.extend(
                self.lance_index
                    .list_for_filtering_where_expr_projected(
                        expr,
                        crate::lance_index::FilterProjection::NoStems,
                    )
                    .await
                    .map_err(StorageError::index("list_for_filtering_by_ids"))?,
            );
        }
        Ok(out)
    }

    /// List all memories with filterable/displayable columns (12 of 14).
    ///
    /// Omits `provenance_source` and `confidence` which no caller reads.
    pub async fn list_filterable(&self) -> Result<Vec<IndexFilterable>> {
        self.lance_index
            .list_filterable()
            .await
            .map_err(StorageError::index("list_filterable"))
    }

    /// List minimal columns for index-level filtering (6 of 14).
    ///
    /// Returns only the fields needed by `apply_index_filters` plus `id`.
    /// Use this for the retrieval pipeline where full metadata is loaded
    /// later via `get()` for surviving entries only.
    pub async fn list_for_filtering(&self) -> Result<Vec<IndexForFiltering>> {
        self.lance_index
            .list_for_filtering()
            .await
            .map_err(StorageError::index("list_for_filtering"))
    }

    /// Like [`Self::list_for_filtering`], but pushes an optional LanceDB
    /// `WHERE`-clause predicate into the index scan so selective scalar
    /// filters (type, criticality, expiry) narrow the row set before any
    /// disk I/O. See [`LanceIndex::list_for_filtering_where`] for the
    /// predicate-safety contract (trusted, escaped inputs only).
    pub async fn list_for_filtering_where(
        &self,
        predicate: Option<String>,
    ) -> Result<Vec<IndexForFiltering>> {
        self.lance_index
            .list_for_filtering_where(predicate)
            .await
            .map_err(StorageError::index("list_for_filtering_where"))
    }

    /// [`Self::list_for_filtering_where`] with a chosen projection; a caller
    /// that never reads `keyword_stems` passes
    /// [`crate::lance_index::FilterProjection::NoStems`].
    pub async fn list_for_filtering_where_projected(
        &self,
        predicate: Option<String>,
        proj: crate::lance_index::FilterProjection,
    ) -> Result<Vec<IndexForFiltering>> {
        self.lance_index
            .list_for_filtering_where_projected(predicate, proj)
            .await
            .map_err(StorageError::index("list_for_filtering_where"))
    }

    /// The rows that watch at least one path (the PostToolUse hook's read).
    /// See [`LanceIndex::list_watched`].
    pub async fn list_watched(&self) -> Result<Vec<crate::lance_index::WatchedEntry>> {
        self.lance_index
            .list_watched()
            .await
            .map_err(StorageError::index("list_watched"))
    }

    /// [`Self::list_for_filtering_where`] driven by a type-safe expression.
    /// See [`LanceIndex::list_for_filtering_where_expr`].
    pub async fn list_for_filtering_where_expr(
        &self,
        predicate: lancedb::expr::DfExpr,
    ) -> Result<Vec<IndexForFiltering>> {
        self.lance_index
            .list_for_filtering_where_expr(predicate)
            .await
            .map_err(StorageError::index("list_for_filtering_where_expr"))
    }

    /// Build the memories-table scalar indexes. See
    /// [`LanceIndex::create_scalar_indexes`]. Best-effort: a scalar index only
    /// changes predicate speed, never which rows match.
    pub async fn create_scalar_indexes(&self) -> Result<Vec<String>> {
        self.lance_index
            .create_scalar_indexes()
            .await
            .map_err(StorageError::index("create_scalar_indexes"))
    }

    /// Build the FM substring index on `tags`. See
    /// [`LanceIndex::create_tag_search_index`].
    pub async fn create_tag_search_index(&self) -> Result<bool> {
        self.lance_index
            .create_tag_search_index()
            .await
            .map_err(StorageError::index("create_tag_search_index"))
    }

    /// List lightweight metadata summaries for all memories (7 columns).
    pub async fn list_summary(&self) -> Result<Vec<IndexSummary>> {
        self.lance_index
            .list_summary()
            .await
            .map_err(StorageError::index("list_summary"))
    }

    /// Every memory id that has a **file on disk**, plus the bytes-on-disk
    /// length of each.
    ///
    /// Deliberately distinct from [`Self::list_ids`], which reads the *index*.
    /// Anything comparing the two sides — "which files are not indexed?",
    /// "how many memories are actually on disk?" — must read this side from the
    /// filesystem, or it is comparing the index against itself and can only
    /// ever report agreement.
    ///
    /// Duplicate ids resolve newest-wins, matching what a reader would get.
    pub async fn list_file_ids(&self) -> Result<Vec<String>> {
        let shared = scan_dir_to_stats(&paths::memories_dir(&self.project_dir)).await;
        let personal = scan_dir_to_stats(&paths::personal_memories_dir(&self.project_id)?).await;
        let mut ids: Vec<String> = shared
            .by_id
            .into_keys()
            .chain(personal.by_id.into_keys())
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// List all memory IDs **from the index**.
    ///
    /// Not the files — see [`Self::list_file_ids`] when the question is what
    /// exists on disk.
    pub async fn list_ids(&self) -> Result<Vec<String>> {
        self.lance_index
            .list_ids()
            .await
            .map_err(StorageError::index("list_ids"))
    }

    /// List every memory in this store that cites a source session
    /// (schema v0.7.0). See [`LanceIndex::list_source_session_links`].
    pub async fn list_source_session_links(
        &self,
    ) -> Result<Vec<super::lance_index::SourceSessionLink>> {
        self.lance_index
            .list_source_session_links()
            .await
            .map_err(StorageError::index("source-session scan"))
    }

    /// How many uncompacted fragments this store's tables carry.
    ///
    /// Cheap enough for a command hot path (fragment metadata only). Used to
    /// decide whether compaction is due *before* the time-based maintenance
    /// window expires — a write-heavy session degrades scans several-fold
    /// within a single window, see `docs/contributors/query-latency-profile.md`.
    pub async fn fragmentation(&self) -> Result<super::lance_index::Fragmentation> {
        self.lance_index
            .fragmentation()
            .await
            .map_err(StorageError::index("stats"))
    }

    /// Compact fragments and prune old LanceDB dataset versions for the
    /// memories and chunks tables. See [`LanceIndex::optimize`].
    ///
    /// Every create/update/delete commits a new immutable Lance version, so
    /// this is the disk-space reclamation path. Safe to run concurrently
    /// with readers (version pruning keeps the lancedb-default 7 days of
    /// versions). Callers should treat failures as non-fatal.
    pub async fn optimize(&self) -> Result<super::lance_index::IndexOptimizeStats> {
        self.lance_index
            .optimize()
            .await
            .map_err(StorageError::index("optimize"))
    }

    /// Return the count of memories without loading data.
    /// Count memories grouped by type in one single-column index scan.
    ///
    /// See [`LanceIndex::count_by_type`]. Cheaper than deriving the histogram
    /// from [`Self::list_summary`], which decodes six columns this does not
    /// need.
    pub async fn count_by_type(&self) -> Result<HashMap<MemoryType, usize>> {
        self.lance_index
            .count_by_type()
            .await
            .map_err(StorageError::index("count_by_type"))
    }

    pub async fn count(&self) -> Result<usize> {
        self.lance_index
            .count()
            .await
            .map_err(StorageError::index("count"))
    }
}
