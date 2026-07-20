//! Unified LanceDB index for metadata and vector storage.
//!
//! Stores memory metadata in a `memories` table and embedding vectors in a
//! separate `chunks` table (one row per text chunk per memory). At search
//! time the chunks table is queried and results are aggregated by memory_id
//! using max-score.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{
    Array, ArrayRef, BooleanArray, FixedSizeListArray, Float32Array, Float64Array, RecordBatch,
    RecordBatchIterator, StringArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Schema};
use futures_util::stream::StreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::table::OptimizeAction;
use lancedb::{connect, Connection, Table};

use chrono::{DateTime, Utc};
use engram_types::{
    Decay, Epistemic, Generality, Memory, MemoryType, ProvenanceSource, Status, Visibility,
};
use serde::{Deserialize, Serialize};

/// Minimum chunk-table row count before [`LanceIndex::optimize`] will
/// opportunistically build an ANN (IVF) vector index.
///
/// An IVF index makes vector search APPROXIMATE (recall < 100%), which would
/// change result ordering that the test suite and the NLI/contradiction flow
/// assert exactly. LanceDB automatically falls back to EXACT flat KNN whenever
/// no index exists, so gating creation behind this deliberately high threshold
/// keeps every small/normal store — hundreds to low-thousands of chunks — on
/// unchanged, 100%-recall exact search. The index is a large-scale scaling
/// win only; it never touches the row counts real projects or tests operate at.
pub const VECTOR_INDEX_MIN_ROWS: usize = 8192;

/// Minimum rows before an IVF index can be trained at all. IVF k-means needs a
/// population to cluster into partitions; below this [`LanceIndex::create_vector_index`]
/// is a graceful no-op (the store keeps using exact flat search).
const IVF_TRAINING_MIN_ROWS: usize = 256;

/// At/above this row count [`LanceIndex::create_vector_index`] builds an IVF-PQ
/// index (product quantization → compressed vectors, the memory win at scale).
/// Between [`IVF_TRAINING_MIN_ROWS`] and this, PQ codebook training has too few
/// samples to be reliable, so it falls back to IVF-Flat (raw vectors, highest
/// recall) — the "fall back to IvfFlat if PQ needs more rows than practical"
/// case.
const IVF_PQ_MIN_ROWS: usize = 4096;

/// `nprobes` for indexed vector search: how many IVF partitions to probe.
/// Higher = higher recall at more cost. Chosen generously — for the partition
/// counts a typical EngramDB store reaches (num_partitions ≈ sqrt(rows), so a
/// few tens up to a few hundred) this probes most/all partitions, keeping
/// recall near-exact. Correctness dominates raw speed here: an index must never
/// silently drop the true nearest neighbor. Harmless when no index exists —
/// LanceDB ignores it for flat KNN.
const VECTOR_SEARCH_NPROBES: usize = 48;

/// `refine_factor` for indexed vector search: after the IVF/PQ shortlist,
/// re-rank the top `refine_factor * limit` candidates using the ORIGINAL
/// (un-quantized) vectors before returning. This restores the recall an IVF-PQ
/// index would otherwise lose to quantization. Harmless for IVF-Flat / flat KNN
/// (their stored vectors are already exact).
const VECTOR_SEARCH_REFINE_FACTOR: u32 = 4;

/// Full metadata entry stored in LanceDB (all 23 columns).
///
/// Used only for writing to LanceDB. For reads, prefer the narrower
/// [`IndexSummary`] or [`IndexFilterable`] projections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: MemoryType,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub physical: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logical: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub criticality: f64,
    pub confidence: f64,
    pub provenance_source: ProvenanceSource,
    pub status: Status,
    pub visibility: Visibility,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Decay configuration (JSON-encoded in the index). Lets the no-query Rank
    /// path score from the projection without reading the `.md` file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decay: Option<Decay>,
    /// Whether this memory currently has embedding chunks. Maintained on
    /// chunk write/delete and rebuilt on reindex; mirrors chunk-table presence
    /// so a semantic query can read it from the memories projection.
    #[serde(default)]
    pub has_embedding: bool,
    // Added in schema v0.3.0 (epistemic memory classes). `watch_paths` is the
    // index name for `valid_while.invalidated_by` — renamed to avoid confusion
    // with the `invalidated_at` timestamp.
    #[serde(default)]
    pub epistemic: Epistemic,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub generality: Generality,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalidated_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch_paths: Vec<String>,
    // Added in schema v0.4.0 (multi-project memories). JSON of
    // `Option<Vec<String>>`, null when None. Restricts which projects/groups a
    // group- or everyone-store memory surfaces for; ignored (positionally
    // scoped) in an ordinary single-project store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
}

/// Lightweight metadata for aggregation/stats queries (7 columns).
///
/// Omits summary, physical, tags, confidence, provenance_source, visibility,
/// and updated_at — the fields rarely needed for counting and scope collection.
#[derive(Debug, Clone)]
pub struct IndexSummary {
    pub id: String,
    pub type_: MemoryType,
    pub status: Status,
    pub logical: Vec<String>,
    pub criticality: f64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Projection for index-level filtering (18 columns).
///
/// Contains the fields that [`apply_index_filters`] reads plus `id` for
/// tracking, `expires_at`/`invalidated_at` for pre-filtering dead entries
/// before any disk I/O, and the scoring/hook fields noted below.
#[derive(Debug, Clone)]
pub struct IndexForFiltering {
    pub id: String,
    pub type_: MemoryType,
    pub physical: Vec<String>,
    pub logical: Vec<String>,
    pub tags: Vec<String>,
    pub criticality: f64,
    pub expires_at: Option<DateTime<Utc>>,
    // R2/R3 scoring-from-projection fields (schema v0.2.0). Together with the
    // fields above these are exactly what `composite_score` reads, so the
    // no-query Rank path can score without loading the `.md` file.
    pub decay: Option<Decay>,
    pub created_at: DateTime<Utc>,
    pub provenance_source: ProvenanceSource,
    pub status: Status,
    pub has_embedding: bool,
    // Schema v0.3.0 epistemic fields. `epistemic`/`verified_at` feed
    // `ScoreTarget`; `invalidated_at` feeds the default-exclusion predicate;
    // `generality`/`origin_task` gate hook injection; `watch_paths` is
    // glob-matched by the PostToolUse hook — all without loading `.md` files.
    pub epistemic: Epistemic,
    pub verified_at: Option<DateTime<Utc>>,
    pub generality: Generality,
    pub origin_task: Option<String>,
    pub invalidated_at: Option<DateTime<Utc>>,
    pub watch_paths: Vec<String>,
    /// Multi-project audience (schema v0.4.0). `None` ⇒ visible to the whole
    /// store's group; `Some(list)` ⇒ only surfaces for a project/group whose id
    /// is in the list. Read here so the multi-store fan-in can filter group- and
    /// everyone-store candidates in Rust without loading the `.md` file.
    pub audience: Option<Vec<String>>,
}

/// Filterable/displayable entry (15 columns).
///
/// Contains every field needed for filtering, sorting, and display.
/// Omits only `provenance_source` and `confidence` which no caller reads
/// after listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexFilterable {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: MemoryType,
    /// Epistemic class (schema v0.3.0). Always serialized — §5.4 requires
    /// json/MCP list output to include it — and rendered as an off-diagonal
    /// `[fact]`-style tag in pretty output.
    #[serde(default)]
    pub epistemic: Epistemic,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub physical: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logical: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub criticality: f64,
    pub status: Status,
    pub visibility: Visibility,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Valid-time start (schema v0.3.0); `None` ⇒ `created_at`. Carried in
    /// the displayable projection for output tagging and time-travel filters.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub valid_from: Option<DateTime<Utc>>,
    /// Valid-time end (schema v0.3.0). Carried so `list` can exclude closed
    /// windows by default and tag them `[invalidated <date>]` when included
    /// (§5.4) without loading files.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub invalidated_at: Option<DateTime<Utc>>,
}

impl From<&Memory> for IndexEntry {
    fn from(memory: &Memory) -> Self {
        Self {
            id: memory.id.clone(),
            type_: memory.type_,
            summary: memory.summary.clone(),
            physical: memory.physical.clone(),
            logical: memory.logical.clone(),
            tags: memory.tags.clone(),
            criticality: memory.criticality,
            confidence: memory.confidence,
            provenance_source: memory.provenance.source,
            status: memory.status,
            visibility: memory.visibility,
            created_at: memory.created_at,
            updated_at: memory.updated_at,
            expires_at: memory.expires_at,
            decay: memory.decay.clone(),
            // Chunks are written separately (and asynchronously), so a
            // from-Memory entry can't know embedding state. The write
            // orchestration sets this: `create`/`update` via a chunk-presence
            // check, `upsert_chunks`/`delete_chunks` via a targeted update, and
            // reindex from the chunk-id set. Defaults to false (no chunks yet).
            has_embedding: false,
            epistemic: memory.epistemic,
            verified_at: memory.verified_at,
            generality: memory
                .valid_while
                .as_ref()
                .map(|v| v.generality)
                .unwrap_or_default(),
            origin_task: memory
                .valid_while
                .as_ref()
                .and_then(|v| v.origin_task.clone()),
            valid_from: memory.valid_from,
            invalidated_at: memory.invalidated_at,
            watch_paths: memory
                .valid_while
                .as_ref()
                .map(|v| v.invalidated_by.clone())
                .unwrap_or_default(),
            audience: memory.audience.clone(),
        }
    }
}

/// A vector search result with ID and similarity score.
#[derive(Debug, Clone)]
pub struct VectorMatch {
    /// Memory ID
    pub id: String,
    /// Cosine similarity score (higher is better)
    pub score: f64,
}

/// Aggregate result of [`LanceIndex::optimize`] across the memories and
/// chunks tables. All counters are zero when there was nothing to reclaim.
#[derive(Debug, Clone, Copy, Default)]
pub struct IndexOptimizeStats {
    /// Fragments merged away by compaction.
    pub fragments_removed: usize,
    /// Data files removed by compaction (including deletion files).
    pub files_removed: usize,
    /// Bytes freed by pruning old dataset versions.
    pub bytes_removed: u64,
    /// Number of old dataset versions pruned.
    pub old_versions_removed: u64,
}

/// Unified LanceDB wrapper for metadata and vector storage.
///
/// Stores memory index entries in a `memories` table and embedding vectors
/// in a separate `chunks` table. Vector search queries the chunks table and
/// aggregates results by memory_id using max-score.
#[derive(Clone)]
pub struct LanceIndex {
    connection: Arc<Connection>,
    table_name: String,
    chunks_table_name: String,
    dimensions: usize,
}

impl LanceIndex {
    /// Create or open a LanceIndex at the given path.
    pub async fn new(db_path: &Path, dimensions: usize) -> Result<Self> {
        let db_path_str = db_path
            .to_str()
            .context("Invalid database path (not valid UTF-8)")?;

        let connection = connect(db_path_str)
            .execute()
            .await
            .context("Failed to connect to LanceDB")?;

        let connection = Arc::new(connection);
        let store = Self {
            connection,
            table_name: "memories".to_string(),
            chunks_table_name: "chunks".to_string(),
            dimensions,
        };

        store.ensure_table_exists().await?;
        store.ensure_chunks_table_exists().await?;
        Ok(store)
    }

    /// Arrow schema for the memories table (metadata only, no vector).
    fn memories_schema(&self) -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("summary", DataType::Utf8, false),
            Field::new("type", DataType::Utf8, false),
            Field::new("status", DataType::Utf8, false),
            Field::new("provenance_source", DataType::Utf8, false),
            Field::new("visibility", DataType::Utf8, false),
            Field::new("criticality", DataType::Float64, false),
            Field::new("confidence", DataType::Float64, false),
            Field::new("physical", DataType::Utf8, false),
            Field::new("logical", DataType::Utf8, false),
            Field::new("tags", DataType::Utf8, false),
            Field::new("created_at", DataType::Utf8, false),
            Field::new("updated_at", DataType::Utf8, false),
            Field::new("expires_at", DataType::Utf8, true),
            // Added in schema v0.2.0 (R2/R3). `decay` is the JSON of
            // `Option<Decay>` (null when None) — the last scoring input not
            // previously in the index, letting the no-query Rank path score
            // straight from the projection. `has_embedding` mirrors chunk-table
            // presence so a semantic query needn't scan the chunks table.
            Field::new("decay", DataType::Utf8, true),
            Field::new("has_embedding", DataType::Boolean, false),
            // Added in schema v0.3.0 (epistemic memory classes). Enum-valued
            // columns store the lowercase serde names; `watch_paths` follows
            // the existing multi-value convention (`physical`/`tags`):
            // serde_json-encoded Utf8, glob-matched in Rust — never in SQL.
            Field::new("epistemic", DataType::Utf8, false),
            Field::new("verified_at", DataType::Utf8, true),
            Field::new("generality", DataType::Utf8, false),
            Field::new("origin_task", DataType::Utf8, true),
            Field::new("valid_from", DataType::Utf8, true),
            Field::new("invalidated_at", DataType::Utf8, true),
            Field::new("watch_paths", DataType::Utf8, false),
            // Added in schema v0.4.0 (multi-project memories). Nullable JSON of
            // `Option<Vec<String>>` — null when the memory has no audience
            // restriction — following the `decay` nullable-Utf8 convention.
            Field::new("audience", DataType::Utf8, true),
        ]))
    }

    /// Arrow schema for the chunks table (embedding vectors per chunk).
    fn chunks_schema(&self) -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("memory_id", DataType::Utf8, false),
            Field::new("chunk_index", DataType::UInt32, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    self.dimensions as i32,
                ),
                false,
            ),
        ]))
    }

    async fn ensure_table_exists(&self) -> Result<()> {
        let table_name = self.table_name.clone();
        let schema = self.memories_schema();
        let connection = Arc::clone(&self.connection);

        match connection.open_table(&table_name).execute().await {
            Ok(_) => Ok(()),
            Err(_) => {
                connection
                    .create_empty_table(&table_name, schema)
                    .execute()
                    .await
                    .context("Failed to create LanceDB memories table")?;
                Ok(())
            }
        }
    }

    async fn ensure_chunks_table_exists(&self) -> Result<()> {
        let table_name = self.chunks_table_name.clone();
        let schema = self.chunks_schema();
        let connection = Arc::clone(&self.connection);

        match connection.open_table(&table_name).execute().await {
            Ok(_) => Ok(()),
            Err(_) => {
                connection
                    .create_empty_table(&table_name, schema)
                    .execute()
                    .await
                    .context("Failed to create LanceDB chunks table")?;
                Ok(())
            }
        }
    }

    async fn open_table(&self) -> Result<Table> {
        let table_name = self.table_name.clone();
        let connection = Arc::clone(&self.connection);

        connection
            .open_table(&table_name)
            .execute()
            .await
            .context("Failed to open LanceDB memories table")
    }

    async fn open_chunks_table(&self) -> Result<Table> {
        let table_name = self.chunks_table_name.clone();
        let connection = Arc::clone(&self.connection);

        connection
            .open_table(&table_name)
            .execute()
            .await
            .context("Failed to open LanceDB chunks table")
    }

    /// Upsert a metadata entry (no vector — vectors go in the chunks table).
    ///
    /// Uses `merge_insert` for atomic upsert — no gap where the entry is missing.
    pub async fn upsert(&self, entry: &IndexEntry) -> Result<()> {
        let batch = self.entry_to_batch(entry)?;
        let table = self.open_table().await?;

        let schema_ref = batch.schema();
        let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema_ref);
        let mut op = table.merge_insert(&["id"]);
        op.when_matched_update_all(None);
        op.when_not_matched_insert_all();
        op.execute(Box::new(batches))
            .await
            .context("Failed to upsert entry")?;
        Ok(())
    }

    /// Upsert a batch of entries in ONE `merge_insert` commit.
    ///
    /// LanceDB commits a new immutable dataset version per mutating call, so
    /// upserting N entries one-by-one (as reindex once did) costs N commit
    /// round-trips and creates N versions that `optimize` must then compact
    /// away. One batched call does one commit.
    pub async fn upsert_batch(&self, entries: &[IndexEntry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let batch = self.entries_to_batch(entries)?;
        let table = self.open_table().await?;

        let schema_ref = batch.schema();
        let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema_ref);
        let mut op = table.merge_insert(&["id"]);
        op.when_matched_update_all(None);
        op.when_not_matched_insert_all();
        op.execute(Box::new(batches))
            .await
            .context("Failed to upsert entry batch")?;
        Ok(())
    }

    /// Delete an entry by ID from the memories table.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let table = self.open_table().await?;
        let escaped_id = id.replace('\'', "''");

        table
            .delete(&format!("id = '{}'", escaped_id))
            .await
            .context("Failed to delete entry")?;
        Ok(())
    }

    /// Return the number of entries in the memories table.
    ///
    /// Uses `count_rows` (O(table metadata)) rather than streaming a column —
    /// this backs `check_staleness`, which runs on every CLI `list`/`query`/
    /// `get`, so a full column scan here scaled every command with store size.
    pub async fn count(&self) -> Result<usize> {
        let table = self.open_table().await?;
        table
            .count_rows(None)
            .await
            .context("Failed to count LanceDB table rows")
    }

    /// Count rows and collect the distinct logical scopes in one
    /// single-column scan (`logical` only).
    ///
    /// Backs the per-mutation manifest-stats refresh, which needs exactly
    /// these two aggregates — deriving them from the 7-column summary
    /// projection made every create/update/delete pay a full-row scan.
    pub async fn count_and_logical_scopes(
        &self,
    ) -> Result<(usize, std::collections::HashSet<String>)> {
        let table = self.open_table().await?;

        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec!["logical".into()]))
            .execute()
            .await
            .context("Failed to query LanceDB table for logical scopes")?;

        let mut count = 0usize;
        let mut scopes = std::collections::HashSet::new();
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read batch")?;
            count += batch.num_rows();
            let logicals = batch
                .column_by_name("logical")
                .context("Missing 'logical' column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("Failed to cast 'logical'")?;
            for i in 0..batch.num_rows() {
                let parsed: Vec<String> = serde_json::from_str(logicals.value(i))
                    .context("Failed to parse 'logical' JSON")?;
                scopes.extend(parsed);
            }
        }
        Ok((count, scopes))
    }

    /// List all memory IDs in the memories table.
    pub async fn list_ids(&self) -> Result<Vec<String>> {
        let table = self.open_table().await?;

        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec!["id".into()]))
            .execute()
            .await
            .context("Failed to query LanceDB table for IDs")?;

        let mut ids = Vec::new();
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read batch")?;
            let id_col = batch
                .column_by_name("id")
                .context("Missing 'id' column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("Failed to cast 'id'")?;
            for i in 0..batch.num_rows() {
                ids.push(id_col.value(i).to_string());
            }
        }
        Ok(ids)
    }

    /// Find IDs matching a given prefix via a LanceDB WHERE clause.
    ///
    /// Returns all matching IDs; callers handle 0/1/many disambiguation.
    pub async fn find_ids_by_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let table = self.open_table().await?;
        // Escape SQL special chars for LIKE pattern and single-quote for string literal
        let escaped = prefix
            .replace('\'', "''")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let filter = format!("id LIKE '{}%'", escaped);

        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec!["id".into()]))
            .only_if(filter)
            .execute()
            .await
            .context("Failed to query LanceDB table for prefix match")?;

        let mut ids = Vec::new();
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read batch")?;
            let id_col = batch
                .column_by_name("id")
                .context("Missing 'id' column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("Failed to cast 'id'")?;
            for i in 0..batch.num_rows() {
                ids.push(id_col.value(i).to_string());
            }
        }
        Ok(ids)
    }

    /// List entries with lightweight metadata (7 columns).
    ///
    /// Returns [`IndexSummary`] entries suitable for aggregation and stats.
    pub async fn list_summary(&self) -> Result<Vec<IndexSummary>> {
        let table = self.open_table().await?;

        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".into(),
                "type".into(),
                "status".into(),
                "logical".into(),
                "criticality".into(),
                "created_at".into(),
                "expires_at".into(),
            ]))
            .execute()
            .await
            .context("Failed to query LanceDB table for summaries")?;

        let mut entries = Vec::new();
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read batch")?;
            entries.extend(batch_to_summaries(&batch)?);
        }
        Ok(entries)
    }

    /// List entries with all filterable/displayable columns (15 columns).
    ///
    /// Omits only `provenance_source` and `confidence` which no caller reads.
    pub async fn list_filterable(&self) -> Result<Vec<IndexFilterable>> {
        let table = self.open_table().await?;

        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".into(),
                "summary".into(),
                "type".into(),
                "epistemic".into(),
                "status".into(),
                "visibility".into(),
                "criticality".into(),
                "physical".into(),
                "logical".into(),
                "tags".into(),
                "created_at".into(),
                "updated_at".into(),
                "expires_at".into(),
                "valid_from".into(),
                "invalidated_at".into(),
            ]))
            .execute()
            .await
            .context("Failed to query LanceDB table for filterable entries")?;

        let mut entries = Vec::new();
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read batch")?;
            entries.extend(batch_to_filterable(&batch)?);
        }
        Ok(entries)
    }

    /// List entries with the columns needed for filtering and projection scoring.
    ///
    /// Returns [`IndexForFiltering`] entries containing only the fields needed
    /// by `apply_index_filters`: id, type, physical, logical, tags, criticality.
    /// Skips summary, status, visibility, dates — saving disk I/O and parsing.
    pub async fn list_for_filtering(&self) -> Result<Vec<IndexForFiltering>> {
        self.list_for_filtering_where(None).await
    }

    /// Like [`Self::list_for_filtering`], but with an optional LanceDB
    /// `WHERE`-clause pushdown so selective scalar filters (type, criticality,
    /// expiry) narrow the row set inside the index scan instead of streaming
    /// every row into Rust.
    ///
    /// The `predicate` string is a DataFusion SQL boolean expression over the
    /// `memories` table columns. Callers MUST build it from trusted, already
    /// validated data (e.g. enum-formatted type names, numeric literals) using
    /// the same single-quote-escaping discipline as [`Self::vector_search`] and
    /// [`Self::find_ids_by_prefix`] — no raw user strings. A `None` predicate
    /// scans the whole table (identical to `list_for_filtering`).
    ///
    /// The pushdown is a pure narrowing optimization: the caller still applies
    /// the equivalent filters in Rust (`apply_index_filters`), so a
    /// conservatively-permissive predicate never changes the final result set.
    pub async fn list_for_filtering_where(
        &self,
        predicate: Option<String>,
    ) -> Result<Vec<IndexForFiltering>> {
        let table = self.open_table().await?;

        let mut query = table.query().select(lancedb::query::Select::Columns(vec![
            "id".into(),
            "type".into(),
            "physical".into(),
            "logical".into(),
            "tags".into(),
            "criticality".into(),
            "expires_at".into(),
            "decay".into(),
            "created_at".into(),
            "provenance_source".into(),
            "status".into(),
            "has_embedding".into(),
            "epistemic".into(),
            "verified_at".into(),
            "generality".into(),
            "origin_task".into(),
            "invalidated_at".into(),
            "watch_paths".into(),
            "audience".into(),
        ]));
        if let Some(pred) = predicate {
            query = query.only_if(pred);
        }

        let mut stream = query
            .execute()
            .await
            .context("Failed to query LanceDB table for filtering entries")?;

        let mut entries = Vec::new();
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read batch")?;
            entries.extend(batch_to_for_filtering(&batch)?);
        }
        Ok(entries)
    }

    /// Upsert embedding chunks for a memory.
    ///
    /// Uses `merge_insert` for atomic upsert. The `when_not_matched_by_source_delete`
    /// filter scoped to the specific `memory_id` removes stale chunks when chunk
    /// count changes. Empty chunks case uses `delete_chunks` as a fast path.
    pub async fn upsert_chunks(&self, memory_id: &str, chunks: Vec<Vec<f32>>) -> Result<()> {
        if chunks.is_empty() {
            self.delete_chunks(memory_id).await?;
            return Ok(());
        }

        // Validate every vector against the index's fixed width BEFORE any
        // Arrow construction: `FixedSizeListArray::new` PANICS on a length
        // mismatch, which would take down the (often background) ingest task
        // and leave the memory stored but silently unsearchable. The read
        // path (`vector_search`) already bails cleanly on a dimension
        // mismatch — the write path must match. A mismatch here usually
        // means the embedding provider and `[embeddings].dimensions` in
        // config.toml disagree (the index table is created from the config
        // value).
        for (i, chunk) in chunks.iter().enumerate() {
            if chunk.len() != self.dimensions {
                anyhow::bail!(
                    "Embedding dimension mismatch for memory '{}': chunk {} has {} dimensions \
                     but the index expects {}. The embedding provider and the configured \
                     [embeddings].dimensions disagree — set [embeddings].dimensions = {} in \
                     config.toml, then run `engramdb reindex --embeddings-only`.",
                    memory_id,
                    i,
                    chunk.len(),
                    self.dimensions,
                    chunk.len()
                );
            }
        }

        let table = self.open_chunks_table().await?;
        let schema = self.chunks_schema();

        let num_chunks = chunks.len();

        // Build arrays
        let memory_ids: Vec<&str> = vec![memory_id; num_chunks];
        let memory_id_array = StringArray::from(memory_ids);
        let chunk_indices: Vec<u32> = (0..num_chunks as u32).collect();
        let chunk_index_array = UInt32Array::from(chunk_indices);

        // Build the vector FixedSizeList array for all chunks
        let all_values: Vec<f32> = chunks.into_iter().flatten().collect();
        let values_array = Float32Array::from(all_values);
        let vector_array = FixedSizeListArray::new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            self.dimensions as i32,
            Arc::new(values_array) as ArrayRef,
            None,
        );

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(memory_id_array) as ArrayRef,
                Arc::new(chunk_index_array) as ArrayRef,
                Arc::new(vector_array) as ArrayRef,
            ],
        )
        .context("Failed to create chunks RecordBatch")?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
        let mut op = table.merge_insert(&["memory_id", "chunk_index"]);
        op.when_matched_update_all(None);
        op.when_not_matched_insert_all();
        let escaped_id = memory_id.replace('\'', "''");
        op.when_not_matched_by_source_delete(Some(format!("memory_id = '{}'", escaped_id)));
        op.execute(Box::new(batches))
            .await
            .context("Failed to upsert chunks")?;

        // Keep the memories-table R3 flag in sync: this memory now has chunks.
        self.set_has_embedding(memory_id, true).await?;

        Ok(())
    }

    /// List all distinct memory_ids present in the chunks table.
    pub async fn list_chunk_memory_ids(&self) -> Result<Vec<String>> {
        let table = self.open_chunks_table().await?;

        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec!["memory_id".into()]))
            .execute()
            .await
            .context("Failed to query chunks table for memory_ids")?;

        let mut ids = std::collections::HashSet::new();
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read chunk batch")?;
            let id_col = batch
                .column_by_name("memory_id")
                .context("Missing 'memory_id' column in chunks")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("Failed to cast 'memory_id'")?;
            for i in 0..batch.num_rows() {
                ids.insert(id_col.value(i).to_string());
            }
        }
        Ok(ids.into_iter().collect())
    }

    /// Return every embedding chunk for `memory_id`, ordered by `chunk_index`.
    ///
    /// Empty when the memory has no embeddings. Used to relocate vectors when
    /// consolidating a worktree's stray store into the main project so the
    /// migrated memories stay searchable without re-embedding.
    pub async fn chunks_for_memory(&self, memory_id: &str) -> Result<Vec<Vec<f32>>> {
        let table = self.open_chunks_table().await?;
        let escaped_id = memory_id.replace('\'', "''");

        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "chunk_index".into(),
                "vector".into(),
            ]))
            .only_if(format!("memory_id = '{}'", escaped_id))
            .execute()
            .await
            .context("Failed to query chunks for memory")?;

        let mut rows: Vec<(u32, Vec<f32>)> = Vec::new();
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read chunk batch")?;
            let idx_col = batch
                .column_by_name("chunk_index")
                .context("Missing 'chunk_index' column in chunks")?
                .as_any()
                .downcast_ref::<UInt32Array>()
                .context("Failed to cast 'chunk_index'")?;
            let vec_col = batch
                .column_by_name("vector")
                .context("Missing 'vector' column in chunks")?
                .as_any()
                .downcast_ref::<FixedSizeListArray>()
                .context("Failed to cast 'vector'")?;
            for i in 0..batch.num_rows() {
                let values = vec_col.value(i);
                let floats = values
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .context("Failed to cast chunk vector values")?;
                rows.push((idx_col.value(i), floats.values().to_vec()));
            }
        }

        rows.sort_by_key(|(idx, _)| *idx);
        Ok(rows.into_iter().map(|(_, v)| v).collect())
    }

    /// Delete all chunks for a given memory_id.
    pub async fn delete_chunks(&self, memory_id: &str) -> Result<()> {
        let table = self.open_chunks_table().await?;
        let escaped_id = memory_id.replace('\'', "''");

        table
            .delete(&format!("memory_id = '{}'", escaped_id))
            .await
            .context("Failed to delete chunks")?;

        // Keep the memories-table R3 flag in sync (no-op if the memory row is
        // gone, e.g. this is part of a full delete).
        self.set_has_embedding(memory_id, false).await?;
        Ok(())
    }

    /// Delete the chunks of many memories in ONE delete commit
    /// (`memory_id IN (...)`), instead of one dataset version per ID.
    pub async fn delete_chunks_batch(&self, memory_ids: &[String]) -> Result<()> {
        if memory_ids.is_empty() {
            return Ok(());
        }
        let table = self.open_chunks_table().await?;
        // Bound the predicate size: a GC sweep can pass thousands of 36-char
        // UUIDs, and an unbounded `IN (...)` builds a hundreds-of-KB SQL
        // string DataFusion must parse in one go. Chunked deletes keep each
        // statement small; each chunk commits separately, which is fine —
        // callers treat chunk deletion as idempotent cleanup.
        const DELETE_CHUNK_SIZE: usize = 500;
        for batch in memory_ids.chunks(DELETE_CHUNK_SIZE) {
            let list = batch
                .iter()
                .map(|id| format!("'{}'", id.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ");
            table
                .delete(&format!("memory_id IN ({list})"))
                .await
                .context("Failed to delete chunk batch")?;
        }
        Ok(())
    }

    /// Whether the chunks table has any row for `memory_id`.
    ///
    /// Used by the `create`/`update` write path to set the memories-table
    /// `has_embedding` flag correctly (an update to a memory that already has
    /// chunks must not reset the flag to false).
    pub async fn has_chunks(&self, memory_id: &str) -> Result<bool> {
        let table = self.open_chunks_table().await?;
        let escaped_id = memory_id.replace('\'', "''");
        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec!["memory_id".into()]))
            .only_if(format!("memory_id = '{}'", escaped_id))
            .limit(1)
            .execute()
            .await
            .context("Failed to query chunks for has_chunks")?;
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read chunk batch")?;
            if batch.num_rows() > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the chunks table holds ANY vectors at all (O(table metadata)
    /// via `count_rows`).
    ///
    /// Distinguishes a store that has never embedded (a fresh project — no
    /// fingerprint AND no vectors is normal, not "legacy") from a genuinely
    /// legacy store whose existing vectors are of unknown model vintage.
    pub async fn has_any_chunks(&self) -> Result<bool> {
        let table = self.open_chunks_table().await?;
        let rows = table
            .count_rows(None)
            .await
            .context("Failed to count chunk rows")?;
        Ok(rows > 0)
    }

    /// Set the memories-table `has_embedding` flag for one memory. A no-op when
    /// no row matches (e.g. the memory was concurrently deleted). Keeps the
    /// R3 projection column in sync with chunk-table presence.
    pub async fn set_has_embedding(&self, memory_id: &str, value: bool) -> Result<()> {
        let table = self.open_table().await?;
        let escaped_id = memory_id.replace('\'', "''");
        table
            .update()
            .only_if(format!("id = '{}'", escaped_id))
            .column("has_embedding", if value { "true" } else { "false" })
            .execute()
            .await
            .context("Failed to update has_embedding")?;
        Ok(())
    }

    /// Perform ANN vector search against the chunks table.
    ///
    /// Queries the chunks table, groups results by memory_id, and takes the
    /// max score per memory. Returns one `VectorMatch` per unique memory,
    /// sorted by score descending, truncated to `limit`.
    ///
    /// When `restrict_to` is `Some`, the search is pushed down to only the
    /// chunks whose `memory_id` is in the given set (via a LanceDB
    /// `memory_id IN (...)` predicate), so the top-k window is spent
    /// entirely on candidates the caller actually cares about instead of
    /// being saturated by filtered-out memories. `Some(&[])` short-circuits
    /// to an empty result; `None` searches the whole store.
    pub async fn vector_search(
        &self,
        query: Vec<f32>,
        limit: usize,
        restrict_to: Option<&[String]>,
    ) -> Result<Vec<VectorMatch>> {
        if query.len() != self.dimensions {
            anyhow::bail!(
                "Query vector dimension mismatch: expected {}, got {}",
                self.dimensions,
                query.len()
            );
        }

        // An explicitly empty restriction set can match nothing — and
        // `IN ()` is not valid SQL — so bail before touching the table.
        if restrict_to.is_some_and(|ids| ids.is_empty()) {
            return Ok(Vec::new());
        }

        let table = self.open_chunks_table().await?;

        // Fetch more rows than needed to ensure enough unique memories after
        // dedup. `limit` is ultimately user-influenced, so saturate the
        // multiply (plain `* 5` panics in debug / wraps in release) and clamp
        // to a bound LanceDB's query plan can represent — it casts the limit
        // to an `i32` top-k internally, so e.g. `usize::MAX` would wrap to a
        // negative k. The clamp only bounds the over-fetch; callers asking
        // for absurd limits still get the best `MAX_CHUNK_FETCH` chunks.
        const MAX_CHUNK_FETCH: usize = 65_536;
        let chunk_limit = limit.saturating_mul(5).min(MAX_CHUNK_FETCH);
        // Recall knobs: only meaningful once an IVF index exists on the vector
        // column (see `create_vector_index`); harmless for the exact flat-KNN
        // path every small/normal store uses, where LanceDB ignores them. They
        // are tuned to preserve recall — probe many partitions and re-rank the
        // shortlist with un-quantized vectors — so an index never silently
        // changes which memories the search returns.
        let mut vector_query = table
            .vector_search(query)?
            .limit(chunk_limit)
            .nprobes(VECTOR_SEARCH_NPROBES)
            .refine_factor(VECTOR_SEARCH_REFINE_FACTOR);
        if let Some(ids) = restrict_to {
            // Memory ids are server-generated UUIDs, but reuse the same
            // single-quote escaping discipline as every other predicate in
            // this file rather than trusting that.
            let id_list = ids
                .iter()
                .map(|id| format!("'{}'", id.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ");
            vector_query = vector_query.only_if(format!("memory_id IN ({})", id_list));
        }
        let mut stream = vector_query
            .execute()
            .await
            .context("Failed to execute vector search")?;

        // Aggregate: max score per memory_id
        let mut score_map: HashMap<String, f64> = HashMap::new();
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read search result batch")?;

            let ids = batch
                .column_by_name("memory_id")
                .context("Missing 'memory_id' column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("Failed to cast 'memory_id' column")?;

            let distances = batch
                .column_by_name("_distance")
                .context("Missing '_distance' column")?
                .as_any()
                .downcast_ref::<Float32Array>()
                .context("Failed to cast '_distance' column")?;

            for i in 0..batch.num_rows() {
                let memory_id = ids.value(i).to_string();
                let distance = distances.value(i) as f64;
                let score = 1.0 / (1.0 + distance);
                let entry = score_map.entry(memory_id).or_insert(0.0);
                if score > *entry {
                    *entry = score;
                }
            }
        }

        let mut matches: Vec<VectorMatch> = score_map
            .into_iter()
            .map(|(id, score)| VectorMatch { id, score })
            .collect();

        matches.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        matches.truncate(limit);
        Ok(matches)
    }

    /// Drop and recreate both memories and chunks tables.
    ///
    /// Destroys all embedding vectors. Only call this when the caller can
    /// re-embed everything afterwards (or explicitly wants a full wipe) —
    /// for a metadata-only rebuild use [`Self::clear_memories`] so existing
    /// vectors survive.
    pub async fn clear(&self) -> Result<()> {
        self.clear_memories().await?;
        self.clear_chunks().await?;
        Ok(())
    }

    /// Drop and recreate only the memories (metadata) table.
    ///
    /// Leaves the chunks (vectors) table untouched, so a reindex that
    /// rebuilds metadata from the markdown files does not destroy
    /// embeddings. Chunks are keyed by `memory_id`, not by table identity.
    pub async fn clear_memories(&self) -> Result<()> {
        let connection = Arc::clone(&self.connection);

        let _ = connection.drop_table(&self.table_name, &[]).await;
        let memories_schema = self.memories_schema();
        connection
            .create_empty_table(&self.table_name, memories_schema)
            .execute()
            .await
            .context("Failed to recreate LanceDB memories table")?;

        Ok(())
    }

    /// The vector width of the on-disk chunks table, or `None` when the
    /// table doesn't exist (or has no vector column).
    ///
    /// `ensure_chunks_table_exists` opens an existing table AS-IS, so after
    /// a `[embeddings].dimensions` change the stored width can differ from
    /// the configured `self.dimensions` — every upsert then fails against
    /// the old schema. Callers about to re-embed everything use this to
    /// decide whether the table must be recreated first (`clear_chunks`).
    pub async fn chunks_table_dimensions(&self) -> Result<Option<usize>> {
        let connection = Arc::clone(&self.connection);
        let table = match connection
            .open_table(&self.chunks_table_name)
            .execute()
            .await
        {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };
        let schema = table
            .schema()
            .await
            .context("Failed to read LanceDB chunks table schema")?;
        Ok(schema
            .field_with_name("vector")
            .ok()
            .and_then(|field| match field.data_type() {
                DataType::FixedSizeList(_, width) => Some(*width as usize),
                _ => None,
            }))
    }

    /// Drop and recreate only the chunks (vectors) table.
    ///
    /// Recreates the table with the currently configured dimensions, so this
    /// is also the remediation path when the embedding dimensions change.
    /// Destroys all vectors — only call when a re-embed is about to follow.
    pub async fn clear_chunks(&self) -> Result<()> {
        let connection = Arc::clone(&self.connection);

        let _ = connection.drop_table(&self.chunks_table_name, &[]).await;
        let chunks_schema = self.chunks_schema();
        connection
            .create_empty_table(&self.chunks_table_name, chunks_schema)
            .execute()
            .await
            .context("Failed to recreate LanceDB chunks table")?;

        // Every vector is gone — reset the memories-table `has_embedding`
        // mirror to match. Callers re-embed and re-set it per success;
        // without this, a memory whose re-embed then FAILS (or a crash
        // mid-loop) stays flagged `true` with zero chunks and is scored as
        // "checked, found nothing" (sem = 0.0) instead of "no evidence"
        // (sem = None) until a full reindex rebuilds the flags.
        let table = self.open_table().await?;
        table
            .update()
            .only_if("has_embedding = true")
            .column("has_embedding", "false")
            .execute()
            .await
            .context("Failed to reset has_embedding after chunk clear")?;

        Ok(())
    }

    /// Compact small fragments and prune old dataset versions for both the
    /// memories and chunks tables.
    ///
    /// Every mutating LanceDB operation (merge_insert, delete, add) commits a
    /// new immutable dataset version, so without periodic maintenance disk
    /// usage grows monotonically with write count. This runs
    /// `OptimizeAction::All`, which is compaction + version pruning + index
    /// optimization with library defaults. Version pruning keeps versions
    /// newer than 7 days (the lancedb default), which is safe for concurrent
    /// MVCC readers: a reader holding an older-than-7-days snapshot in an
    /// active query would be pathological.
    ///
    /// Returns aggregate statistics across both tables (zeroes when there is
    /// nothing to reclaim). Callers should treat failures as non-fatal —
    /// optimization is maintenance, not correctness.
    pub async fn optimize(&self) -> Result<IndexOptimizeStats> {
        let mut total = IndexOptimizeStats::default();
        for (table, label) in [
            (self.open_table().await?, "memories"),
            (self.open_chunks_table().await?, "chunks"),
        ] {
            let stats = table
                .optimize(OptimizeAction::All)
                .await
                .with_context(|| format!("Failed to optimize LanceDB {label} table"))?;
            if let Some(c) = stats.compaction {
                total.fragments_removed += c.fragments_removed;
                total.files_removed += c.files_removed;
            }
            if let Some(p) = stats.prune {
                total.bytes_removed += p.bytes_removed;
                total.old_versions_removed += p.old_versions;
            }
        }

        // Opportunistically build an ANN vector index once the chunks table is
        // large enough that exhaustive flat KNN would dominate query latency.
        // Best-effort, exactly like the compaction/prune loop above: any failure
        // is logged and swallowed — indexing is a scaling optimization, not
        // correctness (search stays exact flat until an index exists). Gated on
        // VECTOR_INDEX_MIN_ROWS so every normal-sized store keeps 100%-recall
        // exact search with unchanged result ordering; only stores past the
        // threshold ever get the approximate index. `optimize` is already the
        // single maintenance entry point (gc / reindex / auto-maintain all call
        // it), so no new call site is needed.
        match self.open_chunks_table().await {
            Ok(chunks) => match chunks.count_rows(None).await {
                Ok(rows) if rows >= VECTOR_INDEX_MIN_ROWS => {
                    if let Err(e) = self.create_vector_index().await {
                        tracing::warn!("optimize: vector index creation failed (continuing): {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("optimize: failed to count chunk rows for indexing: {e}");
                }
            },
            Err(e) => {
                tracing::warn!("optimize: failed to open chunks table for indexing: {e}");
            }
        }

        Ok(total)
    }

    /// Create an approximate-nearest-neighbor (IVF) index on the chunks table's
    /// `vector` column.
    ///
    /// Idempotent and graceful. Returns `Ok(())` without building anything when
    /// either documented no-op condition holds (both logged at debug):
    /// - the table has fewer than [`IVF_TRAINING_MIN_ROWS`] rows (IVF k-means
    ///   has nothing to cluster), or
    /// - an index already covers the `vector` column.
    ///
    /// Neither "too few rows" nor "already indexed" is ever surfaced as a hard
    /// error, because index creation is a best-effort scaling optimization:
    /// LanceDB falls back to exact flat KNN whenever no index is present, so a
    /// skipped build is a correctness-preserving outcome, not a failure.
    ///
    /// Uses IVF-PQ at/above [`IVF_PQ_MIN_ROWS`] (product-quantized, compact) and
    /// IVF-Flat below it (raw vectors, highest recall — PQ codebook training is
    /// unreliable with too few samples). `num_partitions` follows the LanceDB
    /// default of sqrt(rows), floored to at least 1.
    ///
    /// NOTE: an IVF index makes vector search APPROXIMATE. Callers that must
    /// preserve exact ordering rely on this only being built past
    /// [`VECTOR_INDEX_MIN_ROWS`] (see [`Self::optimize`]); the recall knobs in
    /// [`Self::vector_search`] (`nprobes` + `refine_factor`) keep recall high
    /// once it exists.
    pub async fn create_vector_index(&self) -> Result<()> {
        let table = self.open_chunks_table().await?;

        let rows = table
            .count_rows(None)
            .await
            .context("Failed to count chunk rows before vector indexing")?;
        if rows < IVF_TRAINING_MIN_ROWS {
            tracing::debug!(
                "create_vector_index: {rows} chunk rows < {IVF_TRAINING_MIN_ROWS} IVF training \
                 minimum; keeping exact flat search"
            );
            return Ok(());
        }

        // Idempotent: skip if any index already covers the vector column.
        let existing = table
            .list_indices()
            .await
            .context("Failed to list existing chunk-table indices")?;
        if existing
            .iter()
            .any(|idx| idx.columns.iter().any(|c| c == "vector"))
        {
            tracing::debug!("create_vector_index: vector index already exists; nothing to do");
            return Ok(());
        }

        // num_partitions ≈ sqrt(rows) (the LanceDB default), floored so a table
        // just past the training minimum still gets a sane cluster count.
        let num_partitions = ((rows as f64).sqrt().round() as u32).max(1);

        let index = if rows >= IVF_PQ_MIN_ROWS {
            lancedb::index::Index::IvfPq(
                lancedb::index::vector::IvfPqIndexBuilder::default().num_partitions(num_partitions),
            )
        } else {
            lancedb::index::Index::IvfFlat(
                lancedb::index::vector::IvfFlatIndexBuilder::default()
                    .num_partitions(num_partitions),
            )
        };
        let index_kind = if rows >= IVF_PQ_MIN_ROWS {
            "IVF-PQ"
        } else {
            "IVF-Flat"
        };

        table
            .create_index(&["vector"], index)
            .execute()
            .await
            .context("Failed to create IVF vector index on chunks table")?;
        tracing::debug!(
            "create_vector_index: built {index_kind} index on {rows} chunk rows \
             ({num_partitions} partitions)"
        );
        Ok(())
    }

    // ---- Arrow conversion helpers ----

    fn entry_to_batch(&self, entry: &IndexEntry) -> Result<RecordBatch> {
        self.entries_to_batch(std::slice::from_ref(entry))
    }

    /// Build one RecordBatch holding every entry (columnar, N rows).
    fn entries_to_batch(&self, entries: &[IndexEntry]) -> Result<RecordBatch> {
        let n = entries.len();
        let mut ids = Vec::with_capacity(n);
        let mut summaries = Vec::with_capacity(n);
        let mut types = Vec::with_capacity(n);
        let mut statuses = Vec::with_capacity(n);
        let mut provenances = Vec::with_capacity(n);
        let mut visibilities = Vec::with_capacity(n);
        let mut criticalities = Vec::with_capacity(n);
        let mut confidences = Vec::with_capacity(n);
        let mut physicals = Vec::with_capacity(n);
        let mut logicals = Vec::with_capacity(n);
        let mut tags = Vec::with_capacity(n);
        let mut created_ats = Vec::with_capacity(n);
        let mut updated_ats = Vec::with_capacity(n);
        let mut expires_ats: Vec<Option<String>> = Vec::with_capacity(n);
        let mut decays: Vec<Option<String>> = Vec::with_capacity(n);
        let mut has_embeddings = Vec::with_capacity(n);
        let mut epistemics = Vec::with_capacity(n);
        let mut verified_ats: Vec<Option<String>> = Vec::with_capacity(n);
        let mut generalities = Vec::with_capacity(n);
        let mut origin_tasks: Vec<Option<String>> = Vec::with_capacity(n);
        let mut valid_froms: Vec<Option<String>> = Vec::with_capacity(n);
        let mut invalidated_ats: Vec<Option<String>> = Vec::with_capacity(n);
        let mut watch_paths_col = Vec::with_capacity(n);
        let mut audiences: Vec<Option<String>> = Vec::with_capacity(n);

        for entry in entries {
            ids.push(entry.id.clone());
            summaries.push(entry.summary.clone());
            types.push(format!("{:?}", entry.type_).to_lowercase());
            statuses.push(format!("{:?}", entry.status).to_lowercase());
            provenances.push(format!("{:?}", entry.provenance_source).to_lowercase());
            visibilities.push(format!("{:?}", entry.visibility).to_lowercase());
            criticalities.push(entry.criticality);
            confidences.push(entry.confidence);
            physicals.push(
                serde_json::to_string(&entry.physical).context("Failed to serialize physical")?,
            );
            logicals.push(
                serde_json::to_string(&entry.logical).context("Failed to serialize logical")?,
            );
            tags.push(serde_json::to_string(&entry.tags).context("Failed to serialize tags")?);
            created_ats.push(entry.created_at.to_rfc3339());
            updated_ats.push(entry.updated_at.to_rfc3339());
            expires_ats.push(entry.expires_at.map(|dt| dt.to_rfc3339()));
            decays.push(match &entry.decay {
                Some(d) => Some(serde_json::to_string(d).context("Failed to serialize decay")?),
                None => None,
            });
            has_embeddings.push(entry.has_embedding);
            epistemics.push(entry.epistemic.as_str().to_string());
            verified_ats.push(entry.verified_at.map(|dt| dt.to_rfc3339()));
            generalities.push(entry.generality.as_str().to_string());
            origin_tasks.push(entry.origin_task.clone());
            valid_froms.push(entry.valid_from.map(|dt| dt.to_rfc3339()));
            invalidated_ats.push(entry.invalidated_at.map(|dt| dt.to_rfc3339()));
            watch_paths_col.push(
                serde_json::to_string(&entry.watch_paths)
                    .context("Failed to serialize watch_paths")?,
            );
            audiences.push(match &entry.audience {
                Some(a) => Some(serde_json::to_string(a).context("Failed to serialize audience")?),
                None => None,
            });
        }

        let batch = RecordBatch::try_new(
            self.memories_schema(),
            vec![
                Arc::new(StringArray::from(ids)) as ArrayRef,
                Arc::new(StringArray::from(summaries)) as ArrayRef,
                Arc::new(StringArray::from(types)) as ArrayRef,
                Arc::new(StringArray::from(statuses)) as ArrayRef,
                Arc::new(StringArray::from(provenances)) as ArrayRef,
                Arc::new(StringArray::from(visibilities)) as ArrayRef,
                Arc::new(Float64Array::from(criticalities)) as ArrayRef,
                Arc::new(Float64Array::from(confidences)) as ArrayRef,
                Arc::new(StringArray::from(physicals)) as ArrayRef,
                Arc::new(StringArray::from(logicals)) as ArrayRef,
                Arc::new(StringArray::from(tags)) as ArrayRef,
                Arc::new(StringArray::from(created_ats)) as ArrayRef,
                Arc::new(StringArray::from(updated_ats)) as ArrayRef,
                Arc::new(StringArray::from(expires_ats)) as ArrayRef,
                Arc::new(StringArray::from(decays)) as ArrayRef,
                Arc::new(BooleanArray::from(has_embeddings)) as ArrayRef,
                Arc::new(StringArray::from(epistemics)) as ArrayRef,
                Arc::new(StringArray::from(verified_ats)) as ArrayRef,
                Arc::new(StringArray::from(generalities)) as ArrayRef,
                Arc::new(StringArray::from(origin_tasks)) as ArrayRef,
                Arc::new(StringArray::from(valid_froms)) as ArrayRef,
                Arc::new(StringArray::from(invalidated_ats)) as ArrayRef,
                Arc::new(StringArray::from(watch_paths_col)) as ArrayRef,
                Arc::new(StringArray::from(audiences)) as ArrayRef,
            ],
        )
        .context("Failed to create RecordBatch")?;

        Ok(batch)
    }
}

/// Convert a RecordBatch to a Vec of IndexSummary (7 columns).
fn batch_to_summaries(batch: &RecordBatch) -> Result<Vec<IndexSummary>> {
    let ids = batch
        .column_by_name("id")
        .context("Missing 'id' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'id'")?;
    let types = batch
        .column_by_name("type")
        .context("Missing 'type' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'type'")?;
    let statuses = batch
        .column_by_name("status")
        .context("Missing 'status' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'status'")?;
    let logicals = batch
        .column_by_name("logical")
        .context("Missing 'logical' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'logical'")?;
    let criticalities = batch
        .column_by_name("criticality")
        .context("Missing 'criticality' column")?
        .as_any()
        .downcast_ref::<Float64Array>()
        .context("Failed to cast 'criticality'")?;
    let created_ats = batch
        .column_by_name("created_at")
        .context("Missing 'created_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'created_at'")?;
    let expires_ats = batch
        .column_by_name("expires_at")
        .context("Missing 'expires_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'expires_at'")?;

    let mut entries = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        let logical: Vec<String> = serde_json::from_str(logicals.value(i))
            .context("Failed to parse logical scope JSON")?;
        let created_at: DateTime<Utc> = chrono::DateTime::parse_from_rfc3339(created_ats.value(i))
            .context("Failed to parse created_at")?
            .with_timezone(&Utc);
        let expires_at: Option<DateTime<Utc>> = if expires_ats.is_null(i) {
            None
        } else {
            Some(
                chrono::DateTime::parse_from_rfc3339(expires_ats.value(i))
                    .context("Failed to parse expires_at")?
                    .with_timezone(&Utc),
            )
        };

        entries.push(IndexSummary {
            id: ids.value(i).to_string(),
            type_: parse_memory_type(types.value(i))?,
            status: parse_status(statuses.value(i))?,
            logical,
            criticality: criticalities.value(i),
            created_at,
            expires_at,
        });
    }
    Ok(entries)
}

/// Convert a RecordBatch to a Vec of IndexFilterable (15 columns).
fn batch_to_filterable(batch: &RecordBatch) -> Result<Vec<IndexFilterable>> {
    let ids = batch
        .column_by_name("id")
        .context("Missing 'id' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'id'")?;
    let epistemics = batch
        .column_by_name("epistemic")
        .context("Missing 'epistemic' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'epistemic'")?;
    let summaries = batch
        .column_by_name("summary")
        .context("Missing 'summary' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'summary'")?;
    let types = batch
        .column_by_name("type")
        .context("Missing 'type' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'type'")?;
    let statuses = batch
        .column_by_name("status")
        .context("Missing 'status' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'status'")?;
    let visibilities = batch
        .column_by_name("visibility")
        .context("Missing 'visibility' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'visibility'")?;
    let criticalities = batch
        .column_by_name("criticality")
        .context("Missing 'criticality' column")?
        .as_any()
        .downcast_ref::<Float64Array>()
        .context("Failed to cast 'criticality'")?;
    let physicals = batch
        .column_by_name("physical")
        .context("Missing 'physical' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'physical'")?;
    let logicals = batch
        .column_by_name("logical")
        .context("Missing 'logical' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'logical'")?;
    let tags_col = batch
        .column_by_name("tags")
        .context("Missing 'tags' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'tags'")?;
    let created_ats = batch
        .column_by_name("created_at")
        .context("Missing 'created_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'created_at'")?;
    let updated_ats = batch
        .column_by_name("updated_at")
        .context("Missing 'updated_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'updated_at'")?;
    let expires_ats = batch
        .column_by_name("expires_at")
        .context("Missing 'expires_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'expires_at'")?;
    let valid_froms = batch
        .column_by_name("valid_from")
        .context("Missing 'valid_from' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'valid_from'")?;
    let invalidated_ats_col = batch
        .column_by_name("invalidated_at")
        .context("Missing 'invalidated_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'invalidated_at'")?;

    let mut entries = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        let physical: Vec<String> = serde_json::from_str(physicals.value(i))
            .context("Failed to parse physical scope JSON")?;
        let logical: Vec<String> = serde_json::from_str(logicals.value(i))
            .context("Failed to parse logical scope JSON")?;
        let tags: Vec<String> =
            serde_json::from_str(tags_col.value(i)).context("Failed to parse tags JSON")?;
        let created_at: DateTime<Utc> = chrono::DateTime::parse_from_rfc3339(created_ats.value(i))
            .context("Failed to parse created_at")?
            .with_timezone(&Utc);
        let updated_at: DateTime<Utc> = chrono::DateTime::parse_from_rfc3339(updated_ats.value(i))
            .context("Failed to parse updated_at")?
            .with_timezone(&Utc);
        let expires_at: Option<DateTime<Utc>> = if expires_ats.is_null(i) {
            None
        } else {
            Some(
                chrono::DateTime::parse_from_rfc3339(expires_ats.value(i))
                    .context("Failed to parse expires_at")?
                    .with_timezone(&Utc),
            )
        };
        let valid_from: Option<DateTime<Utc>> = if valid_froms.is_null(i) {
            None
        } else {
            Some(
                chrono::DateTime::parse_from_rfc3339(valid_froms.value(i))
                    .context("Failed to parse valid_from")?
                    .with_timezone(&Utc),
            )
        };
        let invalidated_at: Option<DateTime<Utc>> = if invalidated_ats_col.is_null(i) {
            None
        } else {
            Some(
                chrono::DateTime::parse_from_rfc3339(invalidated_ats_col.value(i))
                    .context("Failed to parse invalidated_at")?
                    .with_timezone(&Utc),
            )
        };

        entries.push(IndexFilterable {
            id: ids.value(i).to_string(),
            type_: parse_memory_type(types.value(i))?,
            epistemic: parse_epistemic(epistemics.value(i))?,
            summary: summaries.value(i).to_string(),
            physical,
            logical,
            tags,
            criticality: criticalities.value(i),
            status: parse_status(statuses.value(i))?,
            visibility: parse_visibility(visibilities.value(i))?,
            created_at,
            updated_at,
            expires_at,
            valid_from,
            invalidated_at,
        });
    }
    Ok(entries)
}

/// Convert a RecordBatch to a Vec of IndexForFiltering (18 columns).
fn batch_to_for_filtering(batch: &RecordBatch) -> Result<Vec<IndexForFiltering>> {
    let ids = batch
        .column_by_name("id")
        .context("Missing 'id' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'id'")?;
    let types = batch
        .column_by_name("type")
        .context("Missing 'type' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'type'")?;
    let physicals = batch
        .column_by_name("physical")
        .context("Missing 'physical' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'physical'")?;
    let logicals = batch
        .column_by_name("logical")
        .context("Missing 'logical' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'logical'")?;
    let tags_col = batch
        .column_by_name("tags")
        .context("Missing 'tags' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'tags'")?;
    let criticalities = batch
        .column_by_name("criticality")
        .context("Missing 'criticality' column")?
        .as_any()
        .downcast_ref::<Float64Array>()
        .context("Failed to cast 'criticality'")?;
    let expires_ats = batch
        .column_by_name("expires_at")
        .context("Missing 'expires_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'expires_at'")?;
    let decays = batch
        .column_by_name("decay")
        .context("Missing 'decay' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'decay'")?;
    let created_ats = batch
        .column_by_name("created_at")
        .context("Missing 'created_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'created_at'")?;
    let provenances = batch
        .column_by_name("provenance_source")
        .context("Missing 'provenance_source' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'provenance_source'")?;
    let statuses = batch
        .column_by_name("status")
        .context("Missing 'status' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'status'")?;
    let has_embeddings = batch
        .column_by_name("has_embedding")
        .context("Missing 'has_embedding' column")?
        .as_any()
        .downcast_ref::<BooleanArray>()
        .context("Failed to cast 'has_embedding'")?;
    let epistemics = batch
        .column_by_name("epistemic")
        .context("Missing 'epistemic' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'epistemic'")?;
    let verified_ats = batch
        .column_by_name("verified_at")
        .context("Missing 'verified_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'verified_at'")?;
    let generalities = batch
        .column_by_name("generality")
        .context("Missing 'generality' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'generality'")?;
    let origin_tasks = batch
        .column_by_name("origin_task")
        .context("Missing 'origin_task' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'origin_task'")?;
    let invalidated_ats = batch
        .column_by_name("invalidated_at")
        .context("Missing 'invalidated_at' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'invalidated_at'")?;
    let watch_paths_col = batch
        .column_by_name("watch_paths")
        .context("Missing 'watch_paths' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'watch_paths'")?;
    let audience_col = batch
        .column_by_name("audience")
        .context("Missing 'audience' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'audience'")?;

    let mut entries = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        let physical: Vec<String> = serde_json::from_str(physicals.value(i))
            .context("Failed to parse physical scope JSON")?;
        let logical: Vec<String> = serde_json::from_str(logicals.value(i))
            .context("Failed to parse logical scope JSON")?;
        let tags: Vec<String> =
            serde_json::from_str(tags_col.value(i)).context("Failed to parse tags JSON")?;
        let expires_at: Option<DateTime<Utc>> = if expires_ats.is_null(i) {
            None
        } else {
            Some(
                chrono::DateTime::parse_from_rfc3339(expires_ats.value(i))
                    .context("Failed to parse expires_at")?
                    .with_timezone(&Utc),
            )
        };
        // Degrade a bad row, don't brick the store: memory files are
        // untrusted input, and a hand-edited `floor = nan` survives the
        // write side (serde_json emits non-finite floats as `null` without
        // error) but fails deserialization here. Erroring would fail the
        // whole batch — and this function feeds `list_for_filtering`, the
        // entry point of EVERY retrieval — so one corrupt memory file would
        // take down every query on the store with an error naming no
        // memory. Score the row as undecayed instead and say which one.
        let decay: Option<Decay> = if decays.is_null(i) {
            None
        } else {
            match serde_json::from_str(decays.value(i)) {
                Ok(d) => Some(d),
                Err(e) => {
                    tracing::warn!(
                        "ignoring unparseable decay JSON for memory {} (scoring it undecayed; \
                         fix the memory file and reindex): {}",
                        ids.value(i),
                        e
                    );
                    None
                }
            }
        };
        let created_at: DateTime<Utc> = chrono::DateTime::parse_from_rfc3339(created_ats.value(i))
            .context("Failed to parse created_at")?
            .with_timezone(&Utc);
        let verified_at: Option<DateTime<Utc>> = if verified_ats.is_null(i) {
            None
        } else {
            Some(
                chrono::DateTime::parse_from_rfc3339(verified_ats.value(i))
                    .context("Failed to parse verified_at")?
                    .with_timezone(&Utc),
            )
        };
        let invalidated_at: Option<DateTime<Utc>> = if invalidated_ats.is_null(i) {
            None
        } else {
            Some(
                chrono::DateTime::parse_from_rfc3339(invalidated_ats.value(i))
                    .context("Failed to parse invalidated_at")?
                    .with_timezone(&Utc),
            )
        };
        let watch_paths: Vec<String> = serde_json::from_str(watch_paths_col.value(i))
            .context("Failed to parse watch_paths JSON")?;
        // Nullable JSON; a hand-edited unparseable value degrades to "no
        // audience restriction" rather than bricking every query on the store
        // (mirrors the `decay` degrade-don't-brick handling above).
        let audience: Option<Vec<String>> = if audience_col.is_null(i) {
            None
        } else {
            match serde_json::from_str(audience_col.value(i)) {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::warn!(
                        "ignoring unparseable audience JSON for memory {} (treating it as \
                         unrestricted; fix the memory file and reindex): {}",
                        ids.value(i),
                        e
                    );
                    None
                }
            }
        };

        entries.push(IndexForFiltering {
            id: ids.value(i).to_string(),
            type_: parse_memory_type(types.value(i))?,
            physical,
            logical,
            tags,
            criticality: criticalities.value(i),
            expires_at,
            decay,
            created_at,
            provenance_source: parse_provenance_source(provenances.value(i))?,
            status: parse_status(statuses.value(i))?,
            has_embedding: has_embeddings.value(i),
            epistemic: parse_epistemic(epistemics.value(i))?,
            verified_at,
            generality: parse_generality(generalities.value(i))?,
            origin_task: if origin_tasks.is_null(i) {
                None
            } else {
                Some(origin_tasks.value(i).to_string())
            },
            invalidated_at,
            watch_paths,
            audience,
        });
    }
    Ok(entries)
}

// ---- String parsing helpers ----

fn parse_memory_type(s: &str) -> Result<MemoryType> {
    match s {
        "decision" => Ok(MemoryType::Decision),
        "convention" => Ok(MemoryType::Convention),
        "hazard" => Ok(MemoryType::Hazard),
        "context" => Ok(MemoryType::Context),
        "intent" => Ok(MemoryType::Intent),
        "relationship" => Ok(MemoryType::Relationship),
        "debug" => Ok(MemoryType::Debug),
        "preference" => Ok(MemoryType::Preference),
        _ => anyhow::bail!("Unknown memory type: {}", s),
    }
}

fn parse_status(s: &str) -> Result<Status> {
    match s {
        "active" => Ok(Status::Active),
        "challenged" => Ok(Status::Challenged),
        "needsreview" | "needs_review" => Ok(Status::NeedsReview),
        _ => anyhow::bail!("Unknown status: {}", s),
    }
}

fn parse_epistemic(s: &str) -> Result<Epistemic> {
    match s {
        "fact" => Ok(Epistemic::Fact),
        "observation" => Ok(Epistemic::Observation),
        "decision" => Ok(Epistemic::Decision),
        _ => anyhow::bail!("Unknown epistemic class: {}", s),
    }
}

fn parse_generality(s: &str) -> Result<Generality> {
    match s {
        "project" => Ok(Generality::Project),
        "task" => Ok(Generality::Task),
        _ => anyhow::bail!("Unknown generality: {}", s),
    }
}

fn parse_visibility(s: &str) -> Result<Visibility> {
    match s {
        "shared" => Ok(Visibility::Shared),
        "personal" => Ok(Visibility::Personal),
        _ => anyhow::bail!("Unknown visibility: {}", s),
    }
}

fn parse_provenance_source(s: &str) -> Result<ProvenanceSource> {
    match s {
        "human" => Ok(ProvenanceSource::Human),
        "agent" => Ok(ProvenanceSource::Agent),
        "inferred" => Ok(ProvenanceSource::Inferred),
        "imported" => Ok(ProvenanceSource::Imported),
        _ => anyhow::bail!("Unknown provenance source: {}", s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    fn create_test_entry(id: &str) -> IndexEntry {
        let memory = Memory {
            id: id.to_string(),
            type_: MemoryType::Decision,
            epistemic: MemoryType::Decision.default_epistemic(),
            valid_while: None,
            valid_from: None,
            invalidated_at: None,
            superseded_by: None,
            summary: "Test summary".to_string(),
            title: None,
            content: "Test content".to_string(),
            details: None,
            physical: vec!["/".to_string()],
            logical: vec!["test.module".to_string()],
            tags: vec!["test".to_string()],
            criticality: 0.7,
            decay: None,
            provenance: Provenance::human(),
            confidence: 0.9,
            supersedes: vec![],
            status: Status::Active,
            visibility: Visibility::Shared,
            audience: None,
            challenges: vec![],
            verified_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            accessed_at: Utc::now(),
            expires_at: None,
        };
        IndexEntry::from(&memory)
    }

    #[tokio::test]
    async fn test_lance_index_creation() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await;
        assert!(lance.is_ok());
    }

    #[tokio::test]
    async fn test_upsert_and_list() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let entry = create_test_entry("test-1");
        lance.upsert(&entry).await.unwrap();

        let entries = lance.list_filterable().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "test-1");
        assert_eq!(entries[0].summary, "Test summary");
        assert_eq!(entries[0].visibility, Visibility::Shared);
        assert_eq!(entries[0].epistemic, engram_types::Epistemic::Decision);
    }

    /// §5.4: the filterable projection must carry the epistemic class so list
    /// output can include it (json: always; pretty: off-diagonal tag).
    #[tokio::test]
    async fn test_list_filterable_carries_off_diagonal_epistemic() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let mut entry = create_test_entry("test-offdiag");
        entry.epistemic = engram_types::Epistemic::Observation;
        lance.upsert(&entry).await.unwrap();

        let entries = lance.list_filterable().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].epistemic, engram_types::Epistemic::Observation);
    }

    /// Schema v0.4.0: the `audience` column must survive the write→read round
    /// trip through the filtering projection (where the multi-store fan-in reads
    /// it), including the None case that stores a null cell.
    #[tokio::test]
    async fn test_audience_roundtrip_through_for_filtering() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        // None ⇒ null cell ⇒ reads back as None.
        let mut none_entry = create_test_entry("aud-none");
        none_entry.audience = None;
        lance.upsert(&none_entry).await.unwrap();

        // Some(list) ⇒ JSON cell ⇒ reads back intact.
        let mut some_entry = create_test_entry("aud-some");
        some_entry.audience = Some(vec!["proj-a".to_string(), "group-x".to_string()]);
        lance.upsert(&some_entry).await.unwrap();

        let entries = lance.list_for_filtering().await.unwrap();
        let by_id = |id: &str| entries.iter().find(|e| e.id == id).unwrap().clone();
        assert_eq!(by_id("aud-none").audience, None);
        assert_eq!(
            by_id("aud-some").audience,
            Some(vec!["proj-a".to_string(), "group-x".to_string()])
        );
    }

    #[tokio::test]
    async fn test_upsert_replaces_existing() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let mut entry = create_test_entry("test-1");
        lance.upsert(&entry).await.unwrap();

        entry.summary = "Updated summary".to_string();
        lance.upsert(&entry).await.unwrap();

        let entries = lance.list_filterable().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "Updated summary");
    }

    #[tokio::test]
    async fn test_delete() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let entry = create_test_entry("test-1");
        lance.upsert(&entry).await.unwrap();
        lance.delete("test-1").await.unwrap();

        assert_eq!(lance.count().await.unwrap(), 0);
    }

    /// `optimize` is the disk-space reclamation path for the append-only
    /// Lance versions. It must succeed both on freshly-created empty tables
    /// and after a burst of versioned writes (upserts, chunk writes,
    /// deletes), and must not disturb live data. Actual byte reclamation is
    /// environment-dependent (version pruning retains 7 days), so only the
    /// Ok contract and data integrity are asserted.
    #[tokio::test]
    async fn test_optimize_on_empty_and_after_writes() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        // Empty tables: optimize is a safe no-op.
        let stats = lance.optimize().await.unwrap();
        assert_eq!(stats.bytes_removed, 0, "nothing to prune on a fresh index");

        // A burst of version-creating writes across both tables.
        for i in 0..5 {
            let entry = create_test_entry(&format!("opt-{i}"));
            lance.upsert(&entry).await.unwrap();
            lance
                .upsert_chunks(&format!("opt-{i}"), vec![vec![0.1f32; 384]])
                .await
                .unwrap();
        }
        lance.delete("opt-0").await.unwrap();
        lance.delete_chunks("opt-0").await.unwrap();

        lance.optimize().await.unwrap();

        // Live rows survive compaction.
        assert_eq!(lance.count().await.unwrap(), 4);
        assert_eq!(lance.chunks_for_memory("opt-1").await.unwrap().len(), 1);
        assert!(lance.chunks_for_memory("opt-0").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_clear() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        lance.upsert(&create_test_entry("a")).await.unwrap();
        lance.upsert(&create_test_entry("b")).await.unwrap();
        lance
            .upsert_chunks("a", vec![vec![0.1f32; 384]])
            .await
            .unwrap();

        lance.clear().await.unwrap();

        assert_eq!(lance.count().await.unwrap(), 0);
    }

    /// `clear_memories` must rebuild only the metadata table — embedding
    /// vectors are expensive to recompute and must survive a metadata-only
    /// reindex (the data-loss bug this split fixes).
    #[tokio::test]
    async fn test_clear_memories_preserves_chunks() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        lance.upsert(&create_test_entry("a")).await.unwrap();
        lance
            .upsert_chunks("a", vec![vec![0.1f32; 384], vec![0.2f32; 384]])
            .await
            .unwrap();

        lance.clear_memories().await.unwrap();

        assert_eq!(lance.count().await.unwrap(), 0, "metadata must be cleared");
        assert_eq!(
            lance.list_chunk_memory_ids().await.unwrap(),
            vec!["a".to_string()],
            "chunks must survive a metadata-only clear"
        );
        assert_eq!(
            lance.chunks_for_memory("a").await.unwrap().len(),
            2,
            "all chunk rows must survive"
        );
    }

    /// `clear_chunks` is the inverse: vectors are dropped, metadata stays.
    #[tokio::test]
    async fn test_clear_chunks_preserves_memories() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        lance.upsert(&create_test_entry("a")).await.unwrap();
        lance
            .upsert_chunks("a", vec![vec![0.1f32; 384]])
            .await
            .unwrap();

        lance.clear_chunks().await.unwrap();

        assert_eq!(lance.count().await.unwrap(), 1, "metadata must survive");
        assert!(
            lance.list_chunk_memory_ids().await.unwrap().is_empty(),
            "chunks must be cleared"
        );
    }

    #[tokio::test]
    async fn test_upsert_chunks() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let entry = create_test_entry("chunk-test");
        lance.upsert(&entry).await.unwrap();

        // Insert two chunks
        let chunks = vec![vec![0.1f32; 384], vec![0.2f32; 384]];
        lance.upsert_chunks("chunk-test", chunks).await.unwrap();

        // Should be searchable
        let matches = lance
            .vector_search(vec![0.1f32; 384], 10, None)
            .await
            .unwrap();
        assert!(!matches.is_empty());
        assert_eq!(matches[0].id, "chunk-test");
    }

    /// A wrong-width vector must be rejected with a descriptive error, not
    /// crash the process: `FixedSizeListArray::new` panics on a length
    /// mismatch, and upserts often run in background ingest tasks where a
    /// panic leaves the memory stored but silently unsearchable.
    #[tokio::test]
    async fn test_upsert_chunks_dimension_mismatch_errors_cleanly() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let err = lance
            .upsert_chunks("dim-mismatch-mem", vec![vec![0.1f32; 1024]])
            .await
            .expect_err("wrong-width vector must return Err, not panic");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("dim-mismatch-mem"),
            "error must name the memory id: {msg}"
        );
        assert!(msg.contains("384"), "error must state expected dims: {msg}");
        assert!(msg.contains("1024"), "error must state actual dims: {msg}");

        // The index must remain usable: a correct-width upsert for the same
        // memory still works after the rejected write.
        lance
            .upsert_chunks("dim-mismatch-mem", vec![vec![0.1f32; 384]])
            .await
            .unwrap();
        let matches = lance
            .vector_search(vec![0.1f32; 384], 10, None)
            .await
            .unwrap();
        assert!(matches.iter().any(|m| m.id == "dim-mismatch-mem"));
    }

    /// Mixed batches are all-or-nothing: one bad chunk among good ones must
    /// reject the whole upsert (a partial write would mis-shape the
    /// FixedSizeList values buffer).
    #[tokio::test]
    async fn test_upsert_chunks_rejects_mixed_width_batch() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let err = lance
            .upsert_chunks("mixed-width-mem", vec![vec![0.1f32; 384], vec![0.2f32; 16]])
            .await
            .expect_err("a batch containing a wrong-width chunk must be rejected");
        let msg = format!("{err:#}");
        assert!(msg.contains("mixed-width-mem"), "{msg}");
        assert!(
            msg.contains("chunk 1"),
            "error must point at the bad chunk: {msg}"
        );

        // Nothing from the rejected batch was written.
        let chunks = lance.chunks_for_memory("mixed-width-mem").await.unwrap();
        assert!(chunks.is_empty());
    }

    #[tokio::test]
    async fn test_delete_chunks() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let entry = create_test_entry("del-chunk");
        lance.upsert(&entry).await.unwrap();
        lance
            .upsert_chunks("del-chunk", vec![vec![0.1f32; 384]])
            .await
            .unwrap();

        // Verify searchable
        let matches = lance
            .vector_search(vec![0.1f32; 384], 10, None)
            .await
            .unwrap();
        assert!(!matches.is_empty());

        // Delete chunks
        lance.delete_chunks("del-chunk").await.unwrap();

        // Should no longer appear in search
        let matches = lance
            .vector_search(vec![0.1f32; 384], 10, None)
            .await
            .unwrap();
        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn test_vector_search_max_score_aggregation() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        // Memory A has two chunks: one close to query, one far
        let entry_a = create_test_entry("mem-a");
        lance.upsert(&entry_a).await.unwrap();
        let chunk_close = vec![0.5f32; 384];
        let chunk_far = vec![-0.5f32; 384];
        lance
            .upsert_chunks("mem-a", vec![chunk_close, chunk_far])
            .await
            .unwrap();

        // Memory B has one chunk that's moderately close
        let entry_b = create_test_entry("mem-b");
        lance.upsert(&entry_b).await.unwrap();
        let chunk_mid = vec![0.3f32; 384];
        lance.upsert_chunks("mem-b", vec![chunk_mid]).await.unwrap();

        // Search for something close to chunk_close
        let query = vec![0.5f32; 384];
        let matches = lance.vector_search(query, 10, None).await.unwrap();

        assert_eq!(matches.len(), 2);
        // mem-a should rank first (its best chunk is closer to query)
        assert_eq!(matches[0].id, "mem-a");
        assert_eq!(matches[1].id, "mem-b");
        // Max-score: mem-a's score should be from its close chunk, not averaged
        assert!(matches[0].score > matches[1].score);
    }

    /// `limit` flows in from user input: `usize::MAX` must neither overflow
    /// the 5x chunk over-fetch (debug panic / release wrap) nor exceed what
    /// LanceDB's i32 top-k plan can represent.
    #[tokio::test]
    async fn test_vector_search_huge_limit_no_panic() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        lance.upsert(&create_test_entry("mem-a")).await.unwrap();
        lance
            .upsert_chunks("mem-a", vec![vec![0.5f32; 384]])
            .await
            .unwrap();
        lance.upsert(&create_test_entry("mem-b")).await.unwrap();
        lance
            .upsert_chunks("mem-b", vec![vec![0.3f32; 384]])
            .await
            .unwrap();

        let matches = lance
            .vector_search(vec![0.5f32; 384], usize::MAX, None)
            .await
            .unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].id, "mem-a");
    }

    /// `restrict_to` pushes the candidate set down into the LanceDB
    /// predicate: only restricted ids come back, each with its real
    /// similarity score, even when ids outside the set are closer to the
    /// query (and would otherwise saturate the top-k window).
    #[tokio::test]
    async fn test_vector_search_restrict_to_subset() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        // "near" is closest to the query, "mid" next, "far" farthest.
        for (id, val) in [("near", 0.5f32), ("mid", 0.3), ("far", -0.5)] {
            lance.upsert(&create_test_entry(id)).await.unwrap();
            lance.upsert_chunks(id, vec![vec![val; 384]]).await.unwrap();
        }

        let restrict = vec!["mid".to_string(), "far".to_string()];
        // limit 1: without pushdown, "near" would consume the window and
        // post-filtering would leave nothing.
        let matches = lance
            .vector_search(vec![0.5f32; 384], 1, Some(&restrict))
            .await
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, "mid", "best match WITHIN the restriction");
        assert!(matches[0].score > 0.0);

        // Both restricted ids with headroom; "near" must never appear.
        let matches = lance
            .vector_search(vec![0.5f32; 384], 10, Some(&restrict))
            .await
            .unwrap();
        let ids: Vec<&str> = matches.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["mid", "far"]);
    }

    /// An explicitly empty restriction set matches nothing (and must not
    /// generate the invalid `IN ()` predicate).
    #[tokio::test]
    async fn test_vector_search_restrict_to_empty_set() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        lance.upsert(&create_test_entry("mem-a")).await.unwrap();
        lance
            .upsert_chunks("mem-a", vec![vec![0.5f32; 384]])
            .await
            .unwrap();

        let matches = lance
            .vector_search(vec![0.5f32; 384], 10, Some(&[]))
            .await
            .unwrap();
        assert!(matches.is_empty());
    }

    /// Restriction ids flow through the same quote-escaping discipline as
    /// every other `only_if` site: a literal quote in an id must neither
    /// error at the SQL layer nor match the wrong row.
    #[tokio::test]
    async fn test_vector_search_restrict_to_with_quote_in_id() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let quoted = "abc'def";
        for id in [quoted, "plain"] {
            lance.upsert(&create_test_entry(id)).await.unwrap();
            lance
                .upsert_chunks(id, vec![vec![0.5f32; 384]])
                .await
                .unwrap();
        }

        let restrict = vec![quoted.to_string()];
        let matches = lance
            .vector_search(vec![0.5f32; 384], 10, Some(&restrict))
            .await
            .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].id, quoted);
    }

    /// Deterministic vectors from a fixed-seed xorshift64* PRNG (no external
    /// rng dependency). 384-dim random floats in [-1, 1); collisions are
    /// statistically impossible, so every generated vector is distinct.
    fn seeded_vectors(seed: u64, n: usize, dim: usize) -> Vec<Vec<f32>> {
        let mut state = seed;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            let x = state.wrapping_mul(0x2545F4914F6CDD1D);
            // Top 24 bits → [0, 1) → [-1, 1).
            ((x >> 40) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
        };
        (0..n).map(|_| (0..dim).map(|_| next()).collect()).collect()
    }

    /// Seed `count` single-chunk memories (`{prefix}-NNNN`) into the chunks
    /// table in ONE commit. Seeding via per-memory `upsert_chunks` is one
    /// LanceDB merge_insert commit each (~0.3s), so 300-500 rows would take
    /// minutes and dominate index tests; one batched commit keeps them fast.
    async fn seed_chunks_batch(lance: &LanceIndex, prefix: &str, seed: u64, count: usize) {
        let vectors = seeded_vectors(seed, count, 384);
        let table = lance.open_chunks_table().await.unwrap();
        let schema = lance.chunks_schema();
        let memory_id_array = StringArray::from(
            (0..count)
                .map(|i| format!("{prefix}-{i:04}"))
                .collect::<Vec<_>>(),
        );
        let chunk_index_array = UInt32Array::from(vec![0u32; count]);
        let all_values: Vec<f32> = vectors.iter().flatten().copied().collect();
        let vector_array = FixedSizeListArray::new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            384,
            Arc::new(Float32Array::from(all_values)) as ArrayRef,
            None,
        );
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(memory_id_array) as ArrayRef,
                Arc::new(chunk_index_array) as ArrayRef,
                Arc::new(vector_array) as ArrayRef,
            ],
        )
        .unwrap();
        let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
        let mut op = table.merge_insert(&["memory_id", "chunk_index"]);
        op.when_not_matched_insert_all();
        op.execute(Box::new(batches)).await.unwrap();
    }

    /// The IVF index is APPROXIMATE, so this proves the index plus the
    /// `nprobes` + `refine_factor` recall knobs still return the true nearest
    /// neighbor(s). Builds ~512 distinct 384-dim vectors, records the EXACT
    /// flat-KNN top-5 for a query BEFORE any index exists, builds the index via
    /// `create_vector_index` (bypassing the 8192 opportunistic gate), then
    /// re-runs the same query and asserts the top-1 NN survives and recall@5 is
    /// >= 4/5. Fixed RNG seed → deterministic.
    #[tokio::test]
    async fn test_vector_index_preserves_recall() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        // Seed 512 single-chunk memories in ONE batch commit (see helper).
        seed_chunks_batch(&lance, "mem", 0x9E3779B97F4A7C15, 512).await;

        // A deterministic query vector from a different seed (not identical to
        // any stored vector).
        let query = seeded_vectors(0xD1B54A32D192ED03, 1, 384).pop().unwrap();

        // Ground truth: exact flat KNN, since no index exists yet.
        let before = lance.vector_search(query.clone(), 5, None).await.unwrap();
        assert_eq!(before.len(), 5, "expected 5 flat-search neighbors");
        let before_ids: Vec<String> = before.iter().map(|m| m.id.clone()).collect();

        // Build the ANN index explicitly (bypasses the VECTOR_INDEX_MIN_ROWS gate).
        lance.create_vector_index().await.unwrap();

        // Same query, now with the index + recall knobs active.
        let after = lance.vector_search(query, 5, None).await.unwrap();
        assert_eq!(
            after.len(),
            5,
            "indexed search must still return 5 neighbors"
        );
        let after_ids: Vec<String> = after.iter().map(|m| m.id.clone()).collect();

        // True nearest neighbor must survive indexing.
        assert_eq!(
            after_ids[0], before_ids[0],
            "top-1 NN must survive indexing: before={before_ids:?} after={after_ids:?}"
        );

        // recall@5 must be >= 4/5.
        let overlap = after_ids
            .iter()
            .filter(|id| before_ids.contains(id))
            .count();
        assert!(
            overlap >= 4,
            "indexed recall@5 too low: {overlap}/5 (before={before_ids:?} after={after_ids:?})"
        );
    }

    /// `create_vector_index` is idempotent and a graceful no-op below the IVF
    /// training minimum: a tiny table (well under 256 rows) returns Ok without
    /// building anything, and a second call after a real build is also Ok.
    #[tokio::test]
    async fn test_create_vector_index_idempotent_and_small_store_noop() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        // Empty / tiny table: no-op, no error.
        lance.create_vector_index().await.unwrap();
        seed_chunks_batch(&lance, "small", 1, 8).await;
        lance.create_vector_index().await.unwrap();
        // Search still works (exact flat) after the no-op.
        let hits = lance
            .vector_search(vec![0.1f32; 384], 3, None)
            .await
            .unwrap();
        assert!(!hits.is_empty());

        // A real build followed by a second call must both be Ok (idempotent).
        let temp_dir2 = TempDir::new().unwrap();
        let lance2 = LanceIndex::new(temp_dir2.path(), 384).await.unwrap();
        seed_chunks_batch(&lance2, "big", 2, 300).await;
        lance2.create_vector_index().await.unwrap();
        lance2.create_vector_index().await.unwrap(); // already indexed → Ok no-op
    }

    #[tokio::test]
    async fn test_search_empty_store() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let results = lance.vector_search(vec![0.1f32; 384], 10, None).await;
        assert!(results.is_ok());
        assert!(results.unwrap().is_empty());
    }

    #[test]
    fn test_entry_from_memory() {
        let memory = Memory::new(
            MemoryType::Decision,
            "Test summary",
            "Test content",
            Provenance::human(),
        );
        let entry = IndexEntry::from(&memory);

        assert_eq!(entry.id, memory.id);
        assert_eq!(entry.type_, MemoryType::Decision);
        assert_eq!(entry.summary, "Test summary");
        assert_eq!(entry.visibility, Visibility::Shared);
        assert_eq!(entry.provenance_source, ProvenanceSource::Human);
        assert_eq!(entry.status, Status::Active);
    }

    #[tokio::test]
    async fn test_visibility_roundtrip() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let mut entry = create_test_entry("personal-mem");
        entry.visibility = Visibility::Personal;
        lance.upsert(&entry).await.unwrap();

        let entries = lance.list_filterable().await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].visibility, Visibility::Personal);
    }

    /// SQL-injection / quote-escaping guard: every site that interpolates a
    /// memory id into a LanceDB `only_if` filter (delete, find_by_prefix,
    /// chunks_for_memory, delete_chunks) calls `replace('\'', "''")`. If
    /// that escaping ever regresses, an id like `foo'bar` either errors at
    /// the SQL layer or silently matches the wrong row. These tests drive
    /// every quote-escape site with a literal quote in the id.
    #[tokio::test]
    async fn test_delete_with_quote_in_id() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let quoted = "foo'bar'baz";
        let entry = create_test_entry(quoted);
        lance.upsert(&entry).await.unwrap();
        assert_eq!(lance.count().await.unwrap(), 1);

        lance.delete(quoted).await.unwrap();
        assert_eq!(lance.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn test_chunks_with_quote_in_id_round_trip() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let quoted = "abc'def";
        let entry = create_test_entry(quoted);
        lance.upsert(&entry).await.unwrap();

        let v = vec![0.5f32; 384];
        lance.upsert_chunks(quoted, vec![v.clone()]).await.unwrap();

        let read = lance.chunks_for_memory(quoted).await.unwrap();
        assert_eq!(
            read.len(),
            1,
            "must read the chunk back through quote-escape"
        );
        assert_eq!(read[0].len(), 384);

        // Delete chunks via quote-escape path as well.
        lance.delete_chunks(quoted).await.unwrap();
        let after = lance.chunks_for_memory(quoted).await.unwrap();
        assert!(after.is_empty(), "delete_chunks must round-trip the escape");
    }

    #[tokio::test]
    async fn test_find_ids_by_prefix_with_quote() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        // Two ids that share a quote-bearing prefix.
        let a = "x'a";
        let b = "x'b";
        lance.upsert(&create_test_entry(a)).await.unwrap();
        lance.upsert(&create_test_entry(b)).await.unwrap();
        // A control row that must NOT match.
        lance
            .upsert(&create_test_entry("y-no-match"))
            .await
            .unwrap();

        let hits = lance.find_ids_by_prefix("x'").await.unwrap();
        let set: std::collections::HashSet<_> = hits.into_iter().collect();
        assert!(set.contains(a));
        assert!(set.contains(b));
        assert!(!set.contains("y-no-match"));
    }

    /// `vector_search` bails when the query dimension doesn't match the
    /// index dimension. Lock that early-exit since it's the one place that
    /// catches caller dimension bugs before they corrupt search results.
    #[tokio::test]
    async fn test_vector_search_dimension_mismatch_errors() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let result = lance.vector_search(vec![0.1f32; 16], 5, None).await;
        assert!(result.is_err(), "wrong-dim query must error");
        let msg = format!("{:?}", result.err().unwrap());
        assert!(
            msg.contains("dimension"),
            "error must mention dimension: {}",
            msg
        );
    }
}
