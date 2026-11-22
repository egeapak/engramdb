//! `check_staleness`: whether the index has fallen behind the files.

use super::*;

impl MemoryStore {
    /// Check whether the LanceDB index has fallen behind the memory files on
    /// disk, at the tier `cfg` asks for.
    ///
    /// Returns `Some(warning_message)` if drift was confirmed, `None` if the
    /// store looks current. See [`crate::staleness`] for what each tier
    /// compares, why the default is `size` rather than exact hashing, and why a
    /// finding must be confirmed by a second pass.
    ///
    /// **A `None` here is not proof of currency** — only the `content` tier is
    /// exact, and even it is budget-bounded. `doctor` and `reindex --dry-run`
    /// are the unbudgeted authority; this runs on every read and is a
    /// best-effort early warning.
    ///
    /// Never returns `Err` for a *tier* failure. The three callers all treat an
    /// error as "no warning", so a tier that cannot complete degrades to the
    /// strongest tier that can rather than silently reporting a clean store.
    /// An error here means the index itself could not be counted.
    pub async fn check_staleness(&self, cfg: &IndexConfig) -> Result<Option<String>> {
        // A shared project id means two checkouts legitimately hold different
        // bytes for the same ids, and the index holds the union. Every tier
        // would fire, permanently and unrepairably: reindex degrades to a
        // non-destructive upsert under conflict by design, so the warning would
        // never clear.
        if self.checkout_conflict().await.is_some() {
            return Ok(None);
        }

        let first = self.staleness_pass(cfg).await?;
        if first.is_clean() {
            return Ok(None);
        }
        // Something was found — which on a lock-free read path is as likely to
        // be a write committing across the pass as it is real drift. Pay for a
        // second pass only here, and report only what both agree on.
        let second = self.staleness_pass(cfg).await?;
        Ok(first.confirmed_by(&second).message(cfg.staleness_check))
    }

    /// One tiered comparison of the files against the index.
    ///
    /// Ordering is load-bearing: the file enumeration happens **first** and the
    /// index projection second. `create` writes the file before the row, so
    /// reading the index first would make every concurrent create look like a
    /// missing row; this ordering makes it look like an extra file instead,
    /// which the second pass then clears. Neither ordering avoids the race —
    /// that is what [`StalenessFindings::confirmed_by`] is for — but this one
    /// keeps the transient state consistent between the two passes.
    async fn staleness_pass(&self, cfg: &IndexConfig) -> Result<StalenessFindings> {
        let shared_dir = paths::memories_dir(&self.project_dir);
        let personal_dir = paths::personal_memories_dir(&self.project_id)?;

        // Tier A always runs: it is the cheapest signal and the only one that
        // needs no per-file `statx`.
        if cfg.staleness_check == StalenessCheck::Counts {
            let md_count = count_distinct_md_files(&shared_dir, &personal_dir).await;
            let lance_count = self.lance_count().await?;
            return Ok(StalenessFindings {
                md_count,
                lance_count,
                ..Default::default()
            });
        }

        let shared = scan_dir_to_stats(&shared_dir).await;
        let personal = scan_dir_to_stats(&personal_dir).await;
        // A file count, not a distinct-id count, WITHIN a directory: a stale
        // duplicate left by a crashed rename is a real inconsistency, and
        // tier A has always reported it. An id present in BOTH directories is
        // counted once and compared through its winning copy: reindex keeps
        // (and reports) such a pair rather than deleting a file, so counting
        // both made this warning permanent.
        let merged = merge_cross_dir(shared, personal).await;
        let md_count = merged.file_count;
        let by_id = merged.by_id;

        let rows = self.index_digests().await?;
        let lance_count = rows.len();

        let mut findings = StalenessFindings {
            md_count,
            lance_count,
            ..Default::default()
        };

        // Tier B — id sets. Catches a delete paired with a create, which leaves
        // the counts equal.
        let mut indexed_ids = std::collections::HashSet::with_capacity(rows.len());
        // Tier C candidates: rows whose length still matches, which only the
        // content tier can rule on.
        let mut len_matched = Vec::new();
        for row in &rows {
            indexed_ids.insert(row.memory_id.as_str());
            let Some(stat) = by_id.get(&row.memory_id) else {
                findings.missing_from_disk.insert(row.memory_id.clone());
                continue;
            };
            match row.content_len {
                // Tier C — length. A row that predates schema 0.8.0 records no
                // length; it is *unknown*, not matching, and must not be
                // reported as changed. Backfilling it is reindex's job.
                None => {}
                Some(len) if len != stat.len => {
                    findings.changed.insert(row.memory_id.clone());
                }
                Some(_) => len_matched.push((row, stat)),
            }
        }
        for id in by_id.keys() {
            if !indexed_ids.contains(id.as_str()) {
                findings.missing_from_index.insert(id.clone());
            }
        }

        if cfg.staleness_check == StalenessCheck::Content {
            self.hash_len_matched(&len_matched, cfg.staleness_max_bytes, &mut findings)
                .await;
        }
        Ok(findings)
    }

    /// Tier D — hash the files whose length still matches, up to `max_bytes`.
    ///
    /// Sorted by id so the budget cuts at a deterministic place: an arbitrary
    /// `read_dir` order would make a budget-limited check report a different
    /// subset on every run, which reads as flapping drift.
    ///
    /// Stops at the budget and records that it did, rather than downgrading to
    /// tier C silently — a partial exact check must never be reported as a
    /// clean exact check.
    async fn hash_len_matched(
        &self,
        candidates: &[(&IndexDigest, &FileStat)],
        max_bytes: u64,
        findings: &mut StalenessFindings,
    ) {
        let mut ordered: Vec<_> = candidates.iter().collect();
        ordered.sort_by(|a, b| a.0.memory_id.cmp(&b.0.memory_id));

        let mut spent: u64 = 0;
        for (row, stat) in ordered {
            let Some(recorded) = row.content_sha256.as_deref() else {
                continue;
            };
            if spent.saturating_add(stat.len) > max_bytes {
                findings.budget_exhausted = true;
                break;
            }
            spent = spent.saturating_add(stat.len);
            match async_fs::read(&stat.path).await {
                Ok(bytes) => {
                    if crate::digest::FileDigest::of(&bytes).sha256 != recorded {
                        findings.changed.insert(row.memory_id.clone());
                    }
                }
                // Unreadable is *undetermined*, not drifted. `doctor` reports
                // it as its own category; on the read path, warning "changed"
                // for a permissions problem would send the user to reindex,
                // which cannot fix it either.
                Err(e) => {
                    tracing::debug!(
                        "staleness: cannot hash memory {}: {e}",
                        row.memory_id.as_str()
                    );
                }
            }
        }
    }

    /// Row count, as an index error rather than a tier failure.
    async fn lance_count(&self) -> Result<usize> {
        self.lance_index
            .count()
            .await
            .map_err(StorageError::index("count"))
    }
}
