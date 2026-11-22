//! Schema migration and the index rebuild from the memory files.

use super::*;

impl MemoryStore {
    /// Rebuild the LanceDB metadata index from memory files on disk.
    ///
    /// Clears and rebuilds only the memories (metadata) table. Existing
    /// embedding vectors in the chunks table are preserved — chunks are
    /// keyed by `memory_id`, so a metadata rebuild does not invalidate
    /// them. Chunk rows whose `memory_id` no longer corresponds to a file
    /// on disk are deleted, keeping the chunks table consistent. Callers
    /// that intend to re-embed everything (and only those) should follow
    /// up with [`Self::clear_chunks`].
    ///
    /// **Shared-ID guard:** when [`Self::checkout_conflict`] detects that a
    /// different, still-existing checkout owns this project ID (two clones
    /// of the same git remote share one index but have separate
    /// `.engramdb/memories/` trees), the rebuild degrades to a
    /// non-destructive, upsert-only pass: the table is not cleared and
    /// orphan chunks are not pruned, so the other checkout's rows and
    /// vectors survive. Index rows have no per-checkout ownership column,
    /// so a clear here would be unrecoverable data loss for the other clone.
    /// Migrate the on-disk index schema when the manifest records an older
    /// [`manifest::CURRENT_SCHEMA_VERSION`].
    ///
    /// The migration is a plain **reindex**: the memories table is rebuilt from
    /// the authoritative `.md` files (populating new columns like `decay` and
    /// `has_embedding`) while the chunks/vectors table is preserved untouched —
    /// so it takes seconds and never re-embeds. Idempotent: a store already at
    /// the current version is a no-op, and a second open after a successful
    /// migration does nothing. Callers MUST NOT hold the write lock: a stale
    /// store takes it once, re-checks the manifest under it (so concurrent
    /// opens migrate once), and holds it across the column add, the rebuild
    /// and the stamp. Migration failures propagate so a broken store is loud
    /// rather than silently half-migrated.
    ///
    /// **Cancellation.** A migration (or any reindex) whose task is aborted
    /// midway is restart-safe: the files are never modified, the table is
    /// emptied by one overwrite commit and refilled by upserts, and the
    /// manifest is stamped last. An aborted run leaves a table with some rows
    /// missing and the old stamp, so the next open migrates again; an aborted
    /// plain `reindex` leaves rows missing until the next `reindex` (the
    /// staleness check reports the count mismatch meanwhile).
    pub(super) async fn migrate_schema_if_needed(&self) -> Result<()> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let loaded = manifest::load_manifest(&manifest_path).await.ok();
        let stored = loaded.as_ref().map(|m| m.schema_version.clone());
        let stored_normalizer = loaded.as_ref().and_then(|m| m.normalizer.clone());
        // Migrate only when the store is genuinely behind: a version at or ahead
        // of current (a newer binary's store) is left untouched, and a
        // manifest-less store is treated as pre-migration.
        let schema_stale = stored
            .as_deref()
            .is_none_or(|v| !manifest::schema_version_is_current(v));
        // Second, independent axis: the `keyword_stems` column can be present
        // and well-formed while its contents were produced by a different
        // stemmer or stoplist. Those stems no longer match query terms, so the
        // keyword signal silently degrades — regenerate them. Unlike the schema
        // version this is an equality check, not an ordering one: any
        // difference, older or newer, means the stored stems were not produced
        // by this binary's normalizer.
        let normalizer_stale = stored_normalizer.as_deref() != Some(engram_types::NORMALIZER_STAMP);
        if !schema_stale && !normalizer_stale {
            return Ok(());
        }

        // Stale by the unlocked read. Take the write lock ONCE for the whole
        // migration and look again: several processes opening a stale store
        // at the same moment (every session's hooks after an upgrade) all
        // pass the check above, and without the re-check each rebuilt the
        // table in turn — while the lock was released between the column
        // add, the rebuild and the stamp, so another writer could land
        // between any two of them.
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        let loaded = manifest::load_manifest(&manifest_path).await.ok();
        let stored = loaded.as_ref().map(|m| m.schema_version.clone());
        let stored_normalizer = loaded.as_ref().and_then(|m| m.normalizer.clone());
        let schema_stale = stored
            .as_deref()
            .is_none_or(|v| !manifest::schema_version_is_current(v));
        let normalizer_stale = stored_normalizer.as_deref() != Some(engram_types::NORMALIZER_STAMP);
        if !schema_stale && !normalizer_stale {
            return Ok(());
        }
        #[cfg(test)]
        MIGRATION_REBUILDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tracing::info!(
            "Migrating EngramDB store (schema {} -> {}, normalizer {} -> {}): rebuilding index \
             from memory files (vectors preserved, no re-embed)",
            stored.as_deref().unwrap_or("<none>"),
            manifest::CURRENT_SCHEMA_VERSION,
            stored_normalizer.as_deref().unwrap_or("<none>"),
            engram_types::NORMALIZER_STAMP
        );
        // The chunks table has no `.md` files to rebuild from — recreating it
        // would destroy the vectors the rebuild exists to preserve — so any new
        // column there is added in place, as all-nulls (= "provenance
        // unknown" = re-embed when asked). Done HERE rather than at store open
        // because it commits a Lance `Merge`, which conflicts with any
        // concurrent write: unlocked at open, a parallel MCP session writing
        // chunks would fail the loser's `MemoryStore::open` and break an
        // ordinary read command.
        self.lance_index
            .add_missing_chunk_columns()
            .await
            .map_err(StorageError::index("chunks-table column add"))?;

        // `force_schema_reset`: a schema migration must recreate the memories
        // table with the current columns. The default (upsert-only) reindex path
        // taken under a foreign checkout can't add columns and would fail on a
        // schema mismatch, so migration forces the table rebuild even then.
        // `reindex_locked`, not `reindex_with`: the lock is already held and
        // is not reentrant.
        self.reindex_locked(true, false).await?;
        // reindex's `update_manifest_stats` rewrote the manifest but preserved
        // the old `schema_version`; stamp the new one now that the rebuild
        // succeeded (mirrors how the embedding fingerprint is stamped only on
        // success). Propagate a load failure rather than clobbering the store's
        // identity with a default manifest. Still under the same lock: this is
        // a load-modify-save of `manifest.toml` racing every mutating op's
        // `update_manifest_stats`.
        let mut manifest = manifest::load_manifest(&manifest_path).await?;
        manifest.schema_version = manifest::CURRENT_SCHEMA_VERSION.to_string();
        // Stamped together and only on success: the rebuild that refreshed the
        // columns is the same one that regenerated the stems, so recording one
        // without the other would leave the store claiming a state it is not in.
        manifest.normalizer = Some(engram_types::NORMALIZER_STAMP.to_string());
        manifest::save_manifest(&manifest_path, &manifest).await?;
        Ok(())
    }

    pub async fn reindex(&self) -> Result<usize> {
        Ok(self.reindex_with(false, false).await?.indexed)
    }

    /// [`Self::reindex`] with the full counts — including the files that did
    /// not parse, which a surface must report.
    pub async fn reindex_counted(&self) -> Result<ReindexCounts> {
        self.reindex_with(false, false).await
    }

    /// Rebuild only the rows whose file has changed since it was indexed.
    ///
    /// Every file is still enumerated, read, hashed and deduplicated — the skip
    /// removes the *parse*, the stem derivation and the row write, not the I/O,
    /// because deciding whether a file changed requires reading it. That bounds
    /// the possible win; see `docs/contributors/architecture.md`.
    ///
    /// Falls back to a full rebuild, by design, when the manifest says this
    /// store's rows were not derived by this binary — a schema version or
    /// normalizer stamp behind the current one means the *derivation* changed
    /// even where the bytes did not, and an unchanged file is then exactly the
    /// case that needs rebuilding. The returned `skipped` is zero on that path.
    pub async fn reindex_incremental(&self) -> Result<ReindexCounts> {
        let safe = self.incremental_is_safe().await;
        self.reindex_with(false, safe).await
    }

    /// Whether row-level skipping may be trusted for this store.
    ///
    /// Both stamps must match this binary. `migrate_schema_if_needed` normally
    /// guarantees that on open, but the check is repeated here rather than
    /// assumed: it is what makes "the bytes are unchanged" imply "the row is
    /// correct", and that implication is the whole basis of the skip.
    async fn incremental_is_safe(&self) -> bool {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let Ok(manifest) = manifest::load_manifest(&manifest_path).await else {
            // No readable manifest means no derivation stamp to trust.
            return false;
        };
        let schema_current = manifest.schema_version == manifest::CURRENT_SCHEMA_VERSION;
        let normalizer_current =
            manifest.normalizer.as_deref() == Some(engram_types::NORMALIZER_STAMP);
        if !schema_current || !normalizer_current {
            tracing::info!(
                "incremental reindex not applicable (schema {}, normalizer {:?}); \
                 rebuilding every row",
                manifest.schema_version,
                manifest.normalizer
            );
        }
        schema_current && normalizer_current
    }

    /// Rebuild the memories table from the on-disk `.md` files, preserving
    /// vectors.
    ///
    /// `force_schema_reset` controls the foreign-checkout case: normally
    /// (`false`) a store whose project ID is shared with another live checkout
    /// runs a non-destructive, upsert-only rebuild so the other checkout's rows
    /// survive. A schema migration passes `true`: the memories table must be
    /// recreated with the current column set (upsert-only can't add columns and
    /// would fail on a schema mismatch), so the table is rebuilt from this
    /// checkout's files even under a conflict. The other checkout's **vectors**
    /// are still preserved (the chunks table is never dropped and orphan-pruning
    /// stays skipped); its index rows are rebuilt when it runs its own migration.
    ///
    /// `incremental` skips rewriting a row whose file still hashes to the
    /// digest that row records. It implies upsert-only mode — clearing the
    /// table would destroy precisely the rows it is about to decide to keep —
    /// so deletions are reconciled explicitly instead, by removing rows for ids
    /// no longer present on disk.
    ///
    /// **The lock only excludes other EngramDB processes.** A `git checkout` or
    /// a hand edit landing between the hash in phase 3 and the upsert in phase 5
    /// stamps a digest for bytes that are no longer on disk — which is exactly
    /// the class of writer this feature exists to notice. It is self-correcting
    /// (the next `doctor` or staleness check reports the drift) but it is not
    /// excluded, and cannot be with an advisory lock.
    async fn reindex_with(
        &self,
        force_schema_reset: bool,
        incremental: bool,
    ) -> Result<ReindexCounts> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;
        self.reindex_locked(force_schema_reset, incremental).await
    }

    /// [`Self::reindex_with`] for a caller that already holds the project
    /// write lock (the schema migration). Nothing in here may acquire it:
    /// `flock` on a fresh descriptor blocks in the same process too.
    async fn reindex_locked(
        &self,
        force_schema_reset: bool,
        incremental: bool,
    ) -> Result<ReindexCounts> {
        // What each row was built from, read before any file is touched. Empty
        // in full-rebuild mode, which makes every file a miss and every row a
        // rewrite — the same work the unconditional path always did.
        // Two views of the same read, and they must stay separate.
        //
        // `recorded` drives the skip and holds only rows that HAVE a digest —
        // a row without one records nothing about its file and can never
        // match. `existing_ids` drives the deletion reconciliation and holds
        // EVERY row: deriving deletions from `recorded` instead would leave a
        // digest-less row whose file is gone in the index permanently, since
        // it is absent from the map that decides what to remove.
        let (recorded, existing_ids): (HashMap<String, RowIdentity>, Vec<String>) = if incremental {
            let rows = self
                .lance_index
                .list_digests()
                .await
                .map_err(StorageError::index("list_digests"))?;
            let ids = rows.iter().map(|r| r.memory_id.clone()).collect();
            let map = rows
                .into_iter()
                .filter_map(|row| {
                    let sha = row.content_sha256?;
                    let len = row.content_len?;
                    Some((
                        row.memory_id,
                        RowIdentity {
                            sha256: sha,
                            len,
                            has_embedding: row.has_embedding,
                        },
                    ))
                })
                .collect();
            (map, ids)
        } else {
            (HashMap::new(), Vec::new())
        };

        // Parse everything BEFORE touching the table: the files that do not
        // parse decide which existing rows must survive the rebuild.
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;
        let mut shared = if shared_dir.exists() {
            self.scan_dir(&shared_dir).await?
        } else {
            ScannedDir {
                owned: Vec::new(),
                paths: HashMap::new(),
                unparseable: Vec::new(),
                renamed: Vec::new(),
            }
        };
        let mut personal = if personal_dir.exists() {
            self.scan_dir(&personal_dir).await?
        } else {
            ScannedDir {
                owned: Vec::new(),
                paths: HashMap::new(),
                unparseable: Vec::new(),
                renamed: Vec::new(),
            }
        };
        // An id with a file in BOTH directories is one memory: index the
        // winner, keep and report the loser (see `ReindexCounts::shadowed`).
        // Writing both rows let whichever directory was written last win the
        // row, whatever `get` said, and counted the memory twice.
        let mut renamed: Vec<(String, PathBuf)> = shared
            .renamed
            .drain(..)
            .chain(personal.renamed.drain(..))
            .collect();
        renamed.sort();
        let mut shadowed: Vec<(String, PathBuf)> = Vec::new();
        {
            let personal_updated: HashMap<&str, chrono::DateTime<chrono::Utc>> = personal
                .owned
                .iter()
                .map(|(id, m, _)| (id.as_str(), m.updated_at))
                .collect();
            let mut drop_shared: HashSet<String> = HashSet::new();
            let mut drop_personal: HashSet<String> = HashSet::new();
            for (id, memory, _) in &shared.owned {
                let Some(personal_at) = personal_updated.get(id.as_str()) else {
                    continue;
                };
                match cross_dir_rule(Some(memory.updated_at), Some(*personal_at)) {
                    CrossDirWinner::Shared => drop_personal.insert(id.clone()),
                    CrossDirWinner::Personal => drop_shared.insert(id.clone()),
                };
            }
            drop(personal_updated);
            for (side, dropped) in [(&mut shared, drop_shared), (&mut personal, drop_personal)] {
                if dropped.is_empty() {
                    continue;
                }
                side.owned.retain(|(id, _, _)| !dropped.contains(id));
                for id in dropped {
                    let path = side.paths.remove(&id).unwrap_or_default();
                    tracing::warn!(
                        "memory {id} has a file in both the shared and the personal directory; \
                         {} is the older copy and is not indexed (it is left on disk — update \
                         or delete the memory to remove it)",
                        path.display()
                    );
                    shadowed.push((id, path));
                }
            }
            shadowed.sort();
        }
        let parsed_ids: std::collections::HashSet<&str> = shared
            .owned
            .iter()
            .chain(personal.owned.iter())
            .map(|(id, _, _)| id.as_str())
            .collect();
        let mut unparseable: Vec<UnparseableMemoryFile> = shared
            .unparseable
            .drain(..)
            .chain(personal.unparseable.drain(..))
            .collect();
        // An id a parseable file also claims is that file's (it wins, as a
        // duplicate would); only the rest are kept.
        let mut kept_ids: Vec<String> = unparseable
            .iter()
            .filter_map(|u| u.id.clone())
            .filter(|id| !parsed_ids.contains(id.as_str()))
            .collect();
        kept_ids.sort();
        kept_ids.dedup();
        drop(parsed_ids);

        let foreign_checkout = self.checkout_conflict().await;
        let will_clear = !incremental && (foreign_checkout.is_none() || force_schema_reset);
        let existing_kept: Vec<String> = if kept_ids.is_empty() {
            Vec::new()
        } else {
            let all: std::collections::HashSet<String> = self
                .lance_index
                .list_ids()
                .await
                .map_err(StorageError::index("list_ids"))?
                .into_iter()
                .collect();
            kept_ids
                .iter()
                .filter(|id| all.contains(*id))
                .cloned()
                .collect()
        };
        for u in &mut unparseable {
            u.preserved = u.id.as_ref().is_some_and(|id| existing_kept.contains(id));
        }
        let kept_rows = if will_clear {
            self.lance_index
                .snapshot_rows(&existing_kept)
                .await
                .map_err(StorageError::index("snapshot"))?
        } else {
            Vec::new()
        };
        // Their edges cannot be re-derived either; keep the rows they own.
        // A failed read is fatal, before anything is cleared: the rebuild
        // below overwrites the whole table, so carrying on would delete the
        // rows these files own until each is fixed and reindexed again.
        let kept_edges: Vec<crate::edge_index::EdgeRow> = if existing_kept.is_empty() {
            Vec::new()
        } else {
            let owners: std::collections::HashSet<&str> =
                existing_kept.iter().map(String::as_str).collect();
            crate::edge_index::load_all(self.lance_index.connection())
                .await
                .map_err(StorageError::index(
                    "edges read for the unparseable memories (their rows would be lost by \
                     the rebuild; reindex refused)",
                ))?
                .into_iter()
                .filter(|r| owners.contains(r.owner_id.as_str()))
                .collect()
        };
        let kept_ids = existing_kept;
        let mut cleared = false;
        if incremental {
            // No clear: the surviving rows ARE the result. Deletions are
            // reconciled after the dir passes below.
        } else if let Some(other) = &foreign_checkout {
            if force_schema_reset {
                // Order matters in the shared-table case: the OTHER checkout's
                // rows rebuild on its next open only if it hasn't migrated yet
                // (its manifest is per-checkout, so its own migration re-runs
                // this clear+rebuild). A checkout that already migrated won't
                // migrate again, so THIS clear leaves it unindexed until it
                // runs `engramdb reindex` — say so instead of promising a
                // self-heal that only holds for the first migrator.
                tracing::warn!(
                    "Project ID is shared with another checkout at {} — migrating the \
                     shared memories table to the current schema; that checkout's vectors \
                     are preserved, and its index rows rebuild on its next open (or via \
                     `engramdb reindex` there if it already migrated)",
                    other.display()
                );
                self.lance_index
                    .clear_memories()
                    .await
                    .map_err(StorageError::index("clear"))?;
                cleared = true;
            } else {
                tracing::warn!(
                    "Project ID is shared with another checkout at {} — running a \
                     non-destructive (upsert-only) reindex; index rows and vectors \
                     belonging to the other checkout are preserved",
                    other.display()
                );
            }
        } else {
            // Clear only the metadata table — never the vectors. Dropping the
            // chunks table here would silently destroy all embeddings whenever
            // no provider is available to rebuild them afterwards.
            self.lance_index
                .clear_memories()
                .await
                .map_err(StorageError::index("clear"))?;
            cleared = true;
        }

        debug_assert_eq!(
            cleared, will_clear,
            "snapshot taken iff the table is cleared"
        );
        let mut indexed_ids = Vec::new();

        // The set of memories that currently have embedding chunks. Gathered
        // once, up front (the chunks table is untouched by the metadata
        // rebuild), and used both to stamp each rebuilt row's `has_embedding`
        // flag (R3) and to prune orphan chunks below.
        let chunk_ids: std::collections::HashSet<String> = self
            .lance_index
            .list_chunk_memory_ids()
            .await
            .map_err(StorageError::index("list_chunk_memory_ids"))?
            .into_iter()
            .collect();

        let mut skipped = 0;
        let mut edges: Vec<crate::edge_index::EdgeRow> = Vec::new();

        skipped += self
            .write_dir(
                &shared.owned,
                Visibility::Shared,
                &chunk_ids,
                &mut indexed_ids,
                &recorded,
                &mut edges,
            )
            .await?;
        skipped += self
            .write_dir(
                &personal.owned,
                Visibility::Personal,
                &chunk_ids,
                &mut indexed_ids,
                &recorded,
                &mut edges,
            )
            .await?;

        // Carry the unparseable files' rows across the clear, and count them
        // as indexed so neither the deletion reconciliation nor the orphan
        // prune below takes their row or their vectors.
        if !kept_rows.is_empty() {
            let (_, lost) = self
                .lance_index
                .restore_rows(kept_rows)
                .await
                .map_err(StorageError::index("restore of unparseable rows"))?;
            if lost > 0 {
                tracing::warn!(
                    "{lost} unparseable memory row(s) could not be carried across this schema \
                     change; they reappear once their files are fixed and reindexed"
                );
            }
        }
        indexed_ids.extend(kept_ids.iter().cloned());
        edges.extend(kept_edges);

        // Reconcile deletions. The unconditional path gets this free by
        // clearing the table first; the incremental path never clears, so a row
        // whose file is gone would survive forever and keep answering queries
        // with a memory that no longer exists.
        //
        // Skipped under a checkout conflict for the same reason the orphan
        // prune is: the other clone's files are not visible here, so all of its
        // rows would look deleted.
        if incremental && foreign_checkout.is_none() {
            let on_disk: std::collections::HashSet<&str> =
                indexed_ids.iter().map(|s| s.as_str()).collect();
            let removed: Vec<String> = existing_ids
                .iter()
                .filter(|id| !on_disk.contains(id.as_str()))
                .cloned()
                .collect();
            if !removed.is_empty() {
                self.lance_index
                    .delete_batch(&removed)
                    .await
                    .map_err(StorageError::index("delete_batch"))?;
            }
        }

        // Prune chunks for memories that no longer exist on disk, so the
        // preserved chunks table stays consistent with the rebuilt index.
        // Skipped under a checkout conflict: the other clone's memory files
        // are not visible from here, so its chunk rows would all look like
        // orphans and be deleted.
        if foreign_checkout.is_none() {
            let indexed_set: std::collections::HashSet<&str> =
                indexed_ids.iter().map(|s| s.as_str()).collect();
            let orphans: Vec<String> = chunk_ids
                .iter()
                .filter(|id| !indexed_set.contains(id.as_str()))
                .cloned()
                .collect();
            // One IN-list delete commit for all orphans, not one per ID.
            self.lance_index
                .delete_chunks_batch(&orphans)
                .await
                .map_err(StorageError::index("delete_chunks"))?;
        }

        // The derived edges table (schema 0.9.0). Rebuilt whole in one commit
        // unless another checkout shares this index: its files are invisible
        // here, so its memories' rows would all look deleted — replace only
        // the rows this checkout's memories own, like the upsert-only rows
        // above. That same per-owner path is also correct for an incremental
        // pass, whose removed ids were reconciled above; their rows go too.
        let conn = self.lance_index.connection();
        let edge_result = if foreign_checkout.is_none() {
            crate::edge_index::rebuild(conn, &edges).await
        } else {
            crate::edge_index::replace_owned(conn, &indexed_ids, &edges).await
        };
        edge_result.map_err(StorageError::index("edges rebuild"))?;

        // Update manifest stats
        self.update_manifest_stats().await?;

        Ok(ReindexCounts {
            indexed: indexed_ids.len(),
            skipped,
            unparseable,
            shadowed,
            renamed,
        })
    }
}
