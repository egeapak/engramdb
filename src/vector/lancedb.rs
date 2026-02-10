//! LanceDB vector store implementation
//!
//! This module provides an embedded vector database using LanceDB for semantic search.
//! LanceDB is async, so we maintain a tokio runtime for bridging to synchronous operations.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, Float64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use lancedb::{connect, Connection, Table};
use lancedb::query::{ExecutableQuery, QueryBase};
use tokio::runtime::Runtime;
use futures::stream::StreamExt;

use super::{VectorMatch, VectorMetadata, VectorStore};

/// LanceDB-backed vector store
///
/// Stores embedding vectors with metadata in a LanceDB table.
/// Uses a dedicated tokio runtime to bridge async LanceDB operations to sync API.
pub struct LanceDbStore {
    /// LanceDB connection
    connection: Arc<Connection>,
    /// Table name (usually "memories")
    table_name: String,
    /// Expected vector dimensions
    dimensions: usize,
    /// Tokio runtime for async operations
    runtime: Runtime,
}

impl LanceDbStore {
    /// Create a new LanceDB vector store
    ///
    /// # Arguments
    /// * `db_path` - Path to the LanceDB database directory
    /// * `table_name` - Name of the table to use (e.g., "memories")
    /// * `dimensions` - Vector dimensions (e.g., 384 for all-MiniLM-L6-v2)
    ///
    /// # Returns
    /// A new LanceDbStore instance, or an error if initialization fails
    pub fn new(db_path: PathBuf, table_name: String, dimensions: usize) -> Result<Self> {
        // Create a dedicated tokio runtime for async operations
        let runtime = Runtime::new().context("Failed to create tokio runtime")?;

        // Connect to LanceDB
        let db_path_str = db_path
            .to_str()
            .context("Invalid database path (not valid UTF-8)")?;

        let connection = runtime.block_on(async {
            connect(db_path_str)
                .execute()
                .await
                .context("Failed to connect to LanceDB")
        })?;

        let connection = Arc::new(connection);

        let store = Self {
            connection,
            table_name: table_name.clone(),
            dimensions,
            runtime,
        };

        // Ensure table exists
        store.ensure_table_exists()?;

        Ok(store)
    }

    /// Get the Arrow schema for the memories table
    fn schema(&self) -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    self.dimensions as i32,
                ),
                false,
            ),
            Field::new("type", DataType::Utf8, false),
            Field::new("criticality", DataType::Float64, false),
            Field::new("physical", DataType::Utf8, false), // JSON-serialized array
            Field::new("logical", DataType::Utf8, false),  // JSON-serialized array
            Field::new("tags", DataType::Utf8, false),     // JSON-serialized array
        ]))
    }

    /// Ensure the table exists, create if needed
    fn ensure_table_exists(&self) -> Result<()> {
        let table_name = self.table_name.clone();
        let schema = self.schema();
        let connection = Arc::clone(&self.connection);

        self.runtime.block_on(async move {
            // Try to open existing table
            match connection.open_table(&table_name).execute().await {
                Ok(_) => {
                    // Table exists
                    Ok(())
                }
                Err(_) => {
                    // Table doesn't exist, create it with empty data
                    let empty_batch = RecordBatch::new_empty(schema.clone());
                    let schema_ref = schema.clone();
                    let batches = RecordBatchIterator::new(
                        vec![Ok(empty_batch)].into_iter(),
                        schema_ref,
                    );
                    connection
                        .create_table(&table_name, batches)
                        .execute()
                        .await
                        .context("Failed to create LanceDB table")?;
                    Ok(())
                }
            }
        })
    }

    /// Open the table for operations
    fn open_table(&self) -> Result<Table> {
        let table_name = self.table_name.clone();
        let connection = Arc::clone(&self.connection);

        self.runtime.block_on(async move {
            connection
                .open_table(&table_name)
                .execute()
                .await
                .context("Failed to open LanceDB table")
        })
    }

    /// Create a RecordBatch from a single vector and metadata
    fn create_batch(&self, id: &str, vector: Vec<f32>, metadata: VectorMetadata) -> Result<RecordBatch> {
        // Validate vector dimensions
        if vector.len() != self.dimensions {
            anyhow::bail!(
                "Vector dimension mismatch: expected {}, got {}",
                self.dimensions,
                vector.len()
            );
        }

        // Create arrays
        let id_array = StringArray::from(vec![id]);

        // Create FixedSizeListArray for the vector
        let values = Float32Array::from(vector);
        let vector_array = FixedSizeListArray::new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            self.dimensions as i32,
            Arc::new(values) as ArrayRef,
            None,
        );

        let type_array = StringArray::from(vec![metadata.type_.as_str()]);
        let criticality_array = Float64Array::from(vec![metadata.criticality]);

        // Serialize arrays to JSON
        let physical_json = serde_json::to_string(&metadata.physical)
            .context("Failed to serialize physical scope")?;
        let logical_json = serde_json::to_string(&metadata.logical)
            .context("Failed to serialize logical scope")?;
        let tags_json = serde_json::to_string(&metadata.tags)
            .context("Failed to serialize tags")?;

        let physical_array = StringArray::from(vec![physical_json.as_str()]);
        let logical_array = StringArray::from(vec![logical_json.as_str()]);
        let tags_array = StringArray::from(vec![tags_json.as_str()]);

        // Create RecordBatch
        let batch = RecordBatch::try_new(
            self.schema(),
            vec![
                Arc::new(id_array) as ArrayRef,
                Arc::new(vector_array) as ArrayRef,
                Arc::new(type_array) as ArrayRef,
                Arc::new(criticality_array) as ArrayRef,
                Arc::new(physical_array) as ArrayRef,
                Arc::new(logical_array) as ArrayRef,
                Arc::new(tags_array) as ArrayRef,
            ],
        )
        .context("Failed to create RecordBatch")?;

        Ok(batch)
    }
}

impl VectorStore for LanceDbStore {
    fn upsert(&self, id: &str, vector: Vec<f32>, metadata: VectorMetadata) -> Result<()> {
        let batch = self.create_batch(id, vector, metadata)?;
        let table = self.open_table()?;

        self.runtime.block_on(async move {
            // LanceDB upsert: delete existing and add new
            // First try to delete (ignore errors if doesn't exist)
            let _ = table.delete(&format!("id = '{}'", id)).await;

            // Add new record
            let schema_ref = batch.schema();
            let batches = RecordBatchIterator::new(
                vec![Ok(batch)].into_iter(),
                schema_ref,
            );
            table
                .add(batches)
                .execute()
                .await
                .context("Failed to upsert vector")?;

            Ok(())
        })
    }

    fn search(&self, query: Vec<f32>, limit: usize) -> Result<Vec<VectorMatch>> {
        // Validate query dimensions
        if query.len() != self.dimensions {
            anyhow::bail!(
                "Query vector dimension mismatch: expected {}, got {}",
                self.dimensions,
                query.len()
            );
        }

        let table = self.open_table()?;

        self.runtime.block_on(async move {
            // Perform vector search
            let mut stream = table
                .vector_search(query.clone())?
                .limit(limit)
                .execute()
                .await
                .context("Failed to execute vector search")?;

            let mut matches = Vec::new();

            // Collect results from the stream
            while let Some(batch_result) = stream.next().await {
                let batch = batch_result.context("Failed to read search result batch")?;

                // Extract id and score columns
                let ids = batch
                    .column_by_name("id")
                    .context("Missing 'id' column in search results")?
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .context("Failed to cast 'id' column to StringArray")?;

                let scores = batch
                    .column_by_name("_distance")
                    .context("Missing '_distance' column in search results")?
                    .as_any()
                    .downcast_ref::<Float32Array>()
                    .context("Failed to cast '_distance' column to Float32Array")?;

                // Collect matches from this batch
                for i in 0..batch.num_rows() {
                    let id = ids.value(i).to_string();
                    let distance = scores.value(i) as f64;

                    // Convert distance to similarity score
                    // LanceDB returns L2 distance, convert to cosine similarity approximation
                    // For normalized vectors: similarity ≈ 1 - (distance² / 4)
                    // Or use: similarity = 1 / (1 + distance)
                    let score = 1.0 / (1.0 + distance);

                    matches.push(VectorMatch { id, score });
                }
            }

            // Sort by score descending (should already be sorted, but ensure)
            matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

            Ok(matches)
        })
    }

    fn delete(&self, id: &str) -> Result<()> {
        let table = self.open_table()?;
        let id_owned = id.to_string();

        self.runtime.block_on(async move {
            table
                .delete(&format!("id = '{}'", id_owned))
                .await
                .context("Failed to delete vector")?;

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_lancedb_store_creation() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().to_path_buf();

        let store = LanceDbStore::new(db_path, "memories".to_string(), 384);
        assert!(store.is_ok());
    }

    #[test]
    fn test_upsert_and_search() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().to_path_buf();

        let store = LanceDbStore::new(db_path, "memories".to_string(), 384).unwrap();

        // Create a test vector
        let vector = vec![0.1f32; 384];
        let metadata = VectorMetadata {
            type_: "decision".to_string(),
            criticality: 0.8,
            physical: vec!["src/lib.rs".to_string()],
            logical: vec!["core".to_string()],
            tags: vec!["test".to_string()],
        };

        // Upsert
        let result = store.upsert("test-id", vector.clone(), metadata);
        assert!(result.is_ok());

        // Search
        let results = store.search(vector, 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "test-id");
    }
}
