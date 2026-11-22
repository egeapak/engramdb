//! The derived `edges` table: maintenance on the write path and its reads.

use super::*;

impl MemoryStore {
    /// Replace the `edges` rows owned by `memories` with what their files now
    /// declare (`edge_index` module docs). Callers MUST hold the write lock.
    ///
    /// Runs on every write rather than only on writes that touched an edge
    /// field: `src_invalidated` follows `invalidated_at`, and an ordinary
    /// update can change that too. A memory that had no edges and still has
    /// none costs one read and no commit (`replace_owned`).
    ///
    /// **Never fails the write.** The table is derived and the `.md` file is
    /// already on disk when this runs: an error here used to fail a create
    /// whose file existed, and the retry then created the memory twice. A
    /// failure is logged; the next `reindex` rebuilds the table, and `doctor`'s
    /// "Memory edges" check reports a table that disagrees with the files.
    pub(super) async fn sync_edges_locked(&self, memories: &[Memory]) -> Result<()> {
        let owners: Vec<String> = memories.iter().map(|m| m.id.clone()).collect();
        let rows: Vec<crate::edge_index::EdgeRow> = memories
            .iter()
            .flat_map(crate::edge_index::edges_of)
            .collect();
        if let Err(e) =
            crate::edge_index::replace_owned(self.lance_index.connection(), &owners, &rows).await
        {
            tracing::warn!(
                "edges table not updated for {} memor{} (the files are written; run \
                 `engramdb reindex` to rebuild it): {e:#}",
                owners.len(),
                if owners.len() == 1 { "y" } else { "ies" }
            );
        }
        Ok(())
    }

    /// Every edge in this store, deduplicated (`edge_index::dedupe`). Empty
    /// when the table does not exist yet. Read-only: creates nothing, so it
    /// is safe on a store from [`Self::open_read_only`].
    pub async fn list_edges(&self) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_all(self.lance_index.connection())
            .await
            .map(crate::edge_index::dedupe)
            .map_err(StorageError::index("edges read"))
    }

    /// Every row of the edges table, **not** deduplicated: one per declaring
    /// file. What the personal-id leak check needs — dedupe folds a
    /// `superseded_by` mirror into its `supersedes` row, and the mirror is
    /// the one written into the *other* memory's file.
    pub async fn list_edge_rows(&self) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_all(self.lance_index.connection())
            .await
            .map_err(StorageError::index("edges read"))
    }

    /// The rows with `id` at either end, not deduplicated (see
    /// [`Self::list_edge_rows`]).
    pub async fn list_edge_rows_touching(
        &self,
        id: &str,
    ) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_touching(self.lance_index.connection(), id)
            .await
            .map_err(StorageError::index("edges read"))
    }

    /// The edges touching any of `ids` (full ids), deduplicated, in one
    /// query (`edge_index::load_touching_any`). Read-only.
    pub async fn edges_touching_any(
        &self,
        ids: &[String],
    ) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_touching_any(self.lance_index.connection(), ids)
            .await
            .map(crate::edge_index::dedupe)
            .map_err(StorageError::index("edges read"))
    }

    /// The edges with `id` (a full id) at either end, deduplicated.
    pub async fn edges_touching(&self, id: &str) -> Result<Vec<crate::edge_index::EdgeRow>> {
        crate::edge_index::load_touching(self.lance_index.connection(), id)
            .await
            .map(crate::edge_index::dedupe)
            .map_err(StorageError::index("edges read"))
    }

    /// Drop the edge rows the deleted memories' files declared. Edges *into*
    /// them, declared by other files, stay: a dangling edge is legal, and the
    /// readers report it rather than this path silently rewriting other
    /// memories' files.
    ///
    /// Like [`Self::sync_edges_locked`], a failure is logged, never returned:
    /// the files are already gone.
    pub(super) async fn delete_edges_locked(&self, ids: &[String]) -> Result<()> {
        if let Err(e) = crate::edge_index::delete_owned(self.lance_index.connection(), ids).await {
            tracing::warn!(
                "edges of {} deleted memor{} left in the edges table (run `engramdb reindex`): \
                 {e:#}",
                ids.len(),
                if ids.len() == 1 { "y" } else { "ies" }
            );
        }
        Ok(())
    }
}
