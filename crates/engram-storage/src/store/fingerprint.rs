//! The embedding-model fingerprint in the manifest, and checkout ownership.

use super::*;

impl MemoryStore {
    /// Detect whether a different, still-existing checkout owns this
    /// project ID.
    ///
    /// Two independent clones of the same git remote hash to the same
    /// project ID (deliberately — the ID is stable across moves/re-clones)
    /// and therefore share one LanceDB index, write lock, and personal
    /// memories dir, while each keeps its own `.engramdb/memories/` files.
    /// The global registry records which checkout registered the ID first;
    /// when that path is a different, still-existing directory, destructive
    /// index operations from this checkout would corrupt the other one's
    /// data (see [`Self::reindex`]).
    ///
    /// Returns the registered checkout's canonicalized path, or `None` when
    /// this checkout is the registered owner, the registered path no longer
    /// exists, this is the global store, or the registry is unavailable.
    /// Linked git worktrees of the registered checkout never count as a
    /// conflict.
    pub async fn checkout_conflict(&self) -> Option<PathBuf> {
        if self.is_global() {
            return None;
        }
        let registry = super::registry::FileRegistry::global().ok()?;
        let reg = registry.load().await.ok()?;
        super::registry::conflicting_checkout_path(&reg, &self.project_id, &self.project_dir)
    }

    /// Read the persisted embedding-model fingerprint, if any. `None` on
    /// legacy stores created before model tracking (treated as untracked).
    pub async fn embedding_fingerprint(&self) -> Result<Option<manifest::EmbeddingFingerprint>> {
        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let manifest = manifest::load_manifest(&manifest_path).await?;
        Ok(manifest.embedding)
    }

    /// Whether the store holds ANY embedding vectors (cheap `count_rows`).
    /// See [`LanceIndex::has_any_chunks`].
    pub async fn has_any_chunks(&self) -> Result<bool> {
        self.lance_index
            .has_any_chunks()
            .await
            .map_err(StorageError::index("chunk count"))
    }

    /// Stamp the store with the embedding-model fingerprint its vectors
    /// were produced with. Called after a successful full (re)embed.
    ///
    /// Acquires the per-project write lock: this is a load-modify-save of
    /// `manifest.toml`, racing `update_manifest_stats` (which every mutating
    /// op runs under the lock). Without the lock, a concurrent `create`
    /// could load the pre-stamp manifest and save over the fingerprint,
    /// silently flipping the store back to Untracked right after a
    /// successful reindex.
    ///
    /// Callers must NOT already hold the project write lock: `flock(2)` is
    /// per open-file-description, so re-acquiring on a second fd in the same
    /// process blocks forever. All call sites (e.g. `ops::reindex`) invoke
    /// this after `store.reindex()` has returned and released the lock.
    pub async fn set_embedding_fingerprint(
        &self,
        fingerprint: manifest::EmbeddingFingerprint,
    ) -> Result<()> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let mut manifest = manifest::load_manifest(&manifest_path).await?;
        manifest.embedding = Some(fingerprint);
        manifest::save_manifest(&manifest_path, &manifest).await?;
        Ok(())
    }

    /// Stamp `fingerprint` only if the store has none yet; returns whether it
    /// wrote. The unset check and the write happen under one write lock.
    ///
    /// This is the first-embed stamp's writer (`embed_memory_with`), and the
    /// check has to be repeated here rather than trusted from the caller:
    /// the caller decides "the store is unstamped" *before* its embed and its
    /// chunk upsert, then stamps after them, and anything may stamp the store
    /// in between — a `reindex --embeddings-only` for a newly configured
    /// model, or a concurrent first embed. Acting on the stale answer wrote
    /// the ingest's model over that newer stamp, so the manifest no longer
    /// described the vectors. Lock rules as [`Self::set_embedding_fingerprint`].
    pub async fn stamp_embedding_fingerprint_if_unset(
        &self,
        fingerprint: manifest::EmbeddingFingerprint,
    ) -> Result<bool> {
        let _lock = write_lock::acquire_write_lock(&self.project_id).await?;

        let manifest_path = paths::project_dir(&self.project_dir).join("manifest.toml");
        let mut manifest = manifest::load_manifest(&manifest_path).await?;
        if manifest.embedding.is_some() {
            return Ok(false);
        }
        manifest.embedding = Some(fingerprint);
        manifest::save_manifest(&manifest_path, &manifest).await?;
        Ok(true)
    }
}
