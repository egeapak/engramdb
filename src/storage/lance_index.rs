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
    Array, ArrayRef, FixedSizeListArray, Float32Array, Float64Array, RecordBatch,
    RecordBatchIterator, StringArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Schema};
use futures_util::stream::StreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{connect, Connection, Table};

use crate::types::{Memory, MemoryType, ProvenanceSource, Status, Visibility};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Full metadata entry stored in LanceDB (all 14 columns).
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

/// Minimal projection for index-level filtering (7 columns).
///
/// Contains only the fields that [`apply_index_filters`] reads plus `id`
/// for tracking and `expires_at` for pre-filtering expired entries before
/// any disk I/O.
#[derive(Debug, Clone)]
pub struct IndexForFiltering {
    pub id: String,
    pub type_: MemoryType,
    pub physical: Vec<String>,
    pub logical: Vec<String>,
    pub tags: Vec<String>,
    pub criticality: f64,
    pub expires_at: Option<DateTime<Utc>>,
}

/// Filterable/displayable entry (12 columns).
///
/// Contains every field needed for filtering, sorting, and display.
/// Omits only `provenance_source` and `confidence` which no caller reads
/// after listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexFilterable {
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
    pub status: Status,
    pub visibility: Visibility,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
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
    pub async fn upsert(&self, entry: &IndexEntry) -> Result<()> {
        let batch = self.entry_to_batch(entry)?;
        let table = self.open_table().await?;
        let id = entry.id.clone();

        let _ = table.delete(&format!("id = '{}'", id)).await;

        let schema_ref = batch.schema();
        let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema_ref);
        table
            .add(batches)
            .execute()
            .await
            .context("Failed to upsert entry")?;
        Ok(())
    }

    /// Delete an entry by ID from the memories table.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let table = self.open_table().await?;

        table
            .delete(&format!("id = '{}'", id))
            .await
            .context("Failed to delete entry")?;
        Ok(())
    }

    /// Return the number of entries in the memories table.
    ///
    /// Selects only the `id` column and counts rows without deserialization.
    pub async fn count(&self) -> Result<usize> {
        let table = self.open_table().await?;

        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec!["id".into()]))
            .execute()
            .await
            .context("Failed to query LanceDB table for count")?;

        let mut count = 0usize;
        while let Some(batch_result) = stream.next().await {
            let batch = batch_result.context("Failed to read batch")?;
            count += batch.num_rows();
        }
        Ok(count)
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

    /// List entries with all filterable/displayable columns (12 columns).
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
                "status".into(),
                "visibility".into(),
                "criticality".into(),
                "physical".into(),
                "logical".into(),
                "tags".into(),
                "created_at".into(),
                "updated_at".into(),
                "expires_at".into(),
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

    /// List entries with minimal columns for filtering (6 columns).
    ///
    /// Returns [`IndexForFiltering`] entries containing only the fields needed
    /// by `apply_index_filters`: id, type, physical, logical, tags, criticality.
    /// Skips summary, status, visibility, dates — saving disk I/O and parsing.
    pub async fn list_for_filtering(&self) -> Result<Vec<IndexForFiltering>> {
        let table = self.open_table().await?;

        let mut stream = table
            .query()
            .select(lancedb::query::Select::Columns(vec![
                "id".into(),
                "type".into(),
                "physical".into(),
                "logical".into(),
                "tags".into(),
                "criticality".into(),
                "expires_at".into(),
            ]))
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
    /// Deletes any existing chunks for this memory_id, then inserts new rows
    /// (one per chunk vector with ascending chunk_index).
    pub async fn upsert_chunks(&self, memory_id: &str, chunks: Vec<Vec<f32>>) -> Result<()> {
        // Delete existing chunks for this memory
        self.delete_chunks(memory_id).await?;

        if chunks.is_empty() {
            return Ok(());
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
        table
            .add(batches)
            .execute()
            .await
            .context("Failed to insert chunks")?;

        Ok(())
    }

    /// Delete all chunks for a given memory_id.
    pub async fn delete_chunks(&self, memory_id: &str) -> Result<()> {
        let table = self.open_chunks_table().await?;

        let _ = table.delete(&format!("memory_id = '{}'", memory_id)).await;
        Ok(())
    }

    /// Perform ANN vector search against the chunks table.
    ///
    /// Queries the chunks table, groups results by memory_id, and takes the
    /// max score per memory. Returns one `VectorMatch` per unique memory,
    /// sorted by score descending, truncated to `limit`.
    pub async fn vector_search(&self, query: Vec<f32>, limit: usize) -> Result<Vec<VectorMatch>> {
        if query.len() != self.dimensions {
            anyhow::bail!(
                "Query vector dimension mismatch: expected {}, got {}",
                self.dimensions,
                query.len()
            );
        }

        let table = self.open_chunks_table().await?;

        // Fetch more rows than needed to ensure enough unique memories after dedup
        let chunk_limit = limit * 5;
        let mut stream = table
            .vector_search(query)?
            .limit(chunk_limit)
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

    /// Drop and recreate both memories and chunks tables (for reindex).
    pub async fn clear(&self) -> Result<()> {
        let connection = Arc::clone(&self.connection);

        // Drop and recreate memories table
        let _ = connection.drop_table(&self.table_name, &[]).await;
        let memories_schema = self.memories_schema();
        connection
            .create_empty_table(&self.table_name, memories_schema)
            .execute()
            .await
            .context("Failed to recreate LanceDB memories table")?;

        // Drop and recreate chunks table
        let _ = connection.drop_table(&self.chunks_table_name, &[]).await;
        let chunks_schema = self.chunks_schema();
        connection
            .create_empty_table(&self.chunks_table_name, chunks_schema)
            .execute()
            .await
            .context("Failed to recreate LanceDB chunks table")?;

        Ok(())
    }

    // ---- Arrow conversion helpers ----

    fn entry_to_batch(&self, entry: &IndexEntry) -> Result<RecordBatch> {
        let id_array = StringArray::from(vec![entry.id.as_str()]);
        let summary_array = StringArray::from(vec![entry.summary.as_str()]);
        let type_array = StringArray::from(vec![format!("{:?}", entry.type_).to_lowercase()]);
        let status_array = StringArray::from(vec![format!("{:?}", entry.status).to_lowercase()]);
        let provenance_array =
            StringArray::from(vec![format!("{:?}", entry.provenance_source).to_lowercase()]);
        let visibility_array =
            StringArray::from(vec![format!("{:?}", entry.visibility).to_lowercase()]);
        let criticality_array = Float64Array::from(vec![entry.criticality]);
        let confidence_array = Float64Array::from(vec![entry.confidence]);

        let physical_json =
            serde_json::to_string(&entry.physical).context("Failed to serialize physical")?;
        let logical_json =
            serde_json::to_string(&entry.logical).context("Failed to serialize logical")?;
        let tags_json = serde_json::to_string(&entry.tags).context("Failed to serialize tags")?;

        let physical_array = StringArray::from(vec![physical_json.as_str()]);
        let logical_array = StringArray::from(vec![logical_json.as_str()]);
        let tags_array = StringArray::from(vec![tags_json.as_str()]);

        let created_at_str = entry.created_at.to_rfc3339();
        let updated_at_str = entry.updated_at.to_rfc3339();
        let created_at_array = StringArray::from(vec![created_at_str.as_str()]);
        let updated_at_array = StringArray::from(vec![updated_at_str.as_str()]);
        let expires_at_str = entry.expires_at.map(|dt| dt.to_rfc3339());
        let expires_at_array: StringArray = match &expires_at_str {
            Some(s) => StringArray::from(vec![Some(s.as_str())]),
            None => StringArray::from(vec![Option::<&str>::None]),
        };

        let batch = RecordBatch::try_new(
            self.memories_schema(),
            vec![
                Arc::new(id_array) as ArrayRef,
                Arc::new(summary_array) as ArrayRef,
                Arc::new(type_array) as ArrayRef,
                Arc::new(status_array) as ArrayRef,
                Arc::new(provenance_array) as ArrayRef,
                Arc::new(visibility_array) as ArrayRef,
                Arc::new(criticality_array) as ArrayRef,
                Arc::new(confidence_array) as ArrayRef,
                Arc::new(physical_array) as ArrayRef,
                Arc::new(logical_array) as ArrayRef,
                Arc::new(tags_array) as ArrayRef,
                Arc::new(created_at_array) as ArrayRef,
                Arc::new(updated_at_array) as ArrayRef,
                Arc::new(expires_at_array) as ArrayRef,
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

/// Convert a RecordBatch to a Vec of IndexFilterable (12 columns).
fn batch_to_filterable(batch: &RecordBatch) -> Result<Vec<IndexFilterable>> {
    let ids = batch
        .column_by_name("id")
        .context("Missing 'id' column")?
        .as_any()
        .downcast_ref::<StringArray>()
        .context("Failed to cast 'id'")?;
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

        entries.push(IndexFilterable {
            id: ids.value(i).to_string(),
            type_: parse_memory_type(types.value(i))?,
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
        });
    }
    Ok(entries)
}

/// Convert a RecordBatch to a Vec of IndexForFiltering (7 columns).
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

        entries.push(IndexForFiltering {
            id: ids.value(i).to_string(),
            type_: parse_memory_type(types.value(i))?,
            physical,
            logical,
            tags,
            criticality: criticalities.value(i),
            expires_at,
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

fn parse_visibility(s: &str) -> Result<Visibility> {
    match s {
        "shared" => Ok(Visibility::Shared),
        "personal" => Ok(Visibility::Personal),
        _ => anyhow::bail!("Unknown visibility: {}", s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Memory, MemoryType, Provenance};
    use tempfile::TempDir;

    fn create_test_entry(id: &str) -> IndexEntry {
        let memory = Memory {
            id: id.to_string(),
            type_: MemoryType::Decision,
            summary: "Test summary".to_string(),
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
        let matches = lance.vector_search(vec![0.1f32; 384], 10).await.unwrap();
        assert!(!matches.is_empty());
        assert_eq!(matches[0].id, "chunk-test");
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
        let matches = lance.vector_search(vec![0.1f32; 384], 10).await.unwrap();
        assert!(!matches.is_empty());

        // Delete chunks
        lance.delete_chunks("del-chunk").await.unwrap();

        // Should no longer appear in search
        let matches = lance.vector_search(vec![0.1f32; 384], 10).await.unwrap();
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
        let matches = lance.vector_search(query, 10).await.unwrap();

        assert_eq!(matches.len(), 2);
        // mem-a should rank first (its best chunk is closer to query)
        assert_eq!(matches[0].id, "mem-a");
        assert_eq!(matches[1].id, "mem-b");
        // Max-score: mem-a's score should be from its close chunk, not averaged
        assert!(matches[0].score > matches[1].score);
    }

    #[tokio::test]
    async fn test_search_empty_store() {
        let temp_dir = TempDir::new().unwrap();
        let lance = LanceIndex::new(temp_dir.path(), 384).await.unwrap();

        let results = lance.vector_search(vec![0.1f32; 384], 10).await;
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
}
