//! Searchable index over past Claude Code conversations.
//!
//! One row per session in a `conversations` table, alongside the `memories`
//! and `chunks` tables in the project's existing `lancedb/` directory. The
//! table is **additive**: nothing else is rewritten to add it, which is why
//! the `CURRENT_SCHEMA_VERSION` bump that accompanies it costs a metadata
//! reindex and no re-embed.
//!
//! # Two vectors, one row
//!
//! | Column | Source | Present |
//! |---|---|---|
//! | `digest_vec` | the deterministic, code-generated digest | always |
//! | `summary_vec` | the curated summary a human or agent wrote | after review |
//!
//! Two columns rather than one embedding of digest+summary concatenated:
//! editing a summary would otherwise invalidate the digest vector, re-embedding
//! the whole conversation to fix a typo instead of two sentences.
//!
//! The digest vector's source is deliberately the *deterministic* reduction and
//! never the agent's prose. That is what keeps `reindex --archive-only`
//! meaningful — an agent-authored summary is not regenerable by code, so a
//! rebuild from the stored transcript copies could not reproduce it, while the
//! digest is reproducible by construction.
//!
//! # Which project's index
//!
//! The table lives under the **root** project id, the same root the ledger and
//! the transcript copies are keyed by. A worktree and its main checkout share
//! one memory store, one ledger, and therefore one conversation index; keying
//! any of the three differently makes each half re-offer what the other
//! settled. Callers pass `SessionScope::root_project_id`; this module never
//! resolves a project itself.

use crate::paths;
use anyhow::{anyhow, Context, Result};
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator,
    StringArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Schema};
use chrono::{DateTime, Utc};
use futures_util::stream::StreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{connect, Connection, Table};
use std::sync::Arc;

/// Name of the table inside the project's LanceDB directory.
const TABLE: &str = "conversations";

/// Recall knobs mirroring [`crate::lance_index`]: meaningful only once an IVF
/// index exists, harmless for the exact flat-KNN path a few thousand
/// conversations will always take.
const NPROBES: usize = 48;
const REFINE_FACTOR: u32 = 4;

/// One indexed conversation, as written.
///
/// Every string field is stored as it is handed over — sanitizing transcript
/// text is `ops`' job and happens before it reaches here, in the same place the
/// digest is rendered.
#[derive(Debug, Clone)]
pub struct ConversationRow {
    pub session_id: String,
    /// The root project id this session was attributed to. Stored even though
    /// the table already sits under that project's directory, so a search that
    /// fans several projects in can say which one a hit came from without
    /// tracking it out-of-band.
    pub project_id: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub indexed_at: DateTime<Utc>,
    pub user_turns: u32,
    pub assistant_turns: u32,
    /// First human turn, for result display.
    pub first_prompt: Option<String>,
    /// Characters of indexed text behind [`Self::digest_vec`].
    pub indexed_chars: u32,
    /// Whether the indexed text is the whole session or a budgeted prefix.
    /// A loss the row has to declare: a search that missed something because
    /// the tail was cut must be distinguishable from one where the topic was
    /// genuinely absent.
    pub indexed_complete: bool,
    /// SHA-256 of the exact text that produced [`Self::digest_vec`]. What makes
    /// `harvest index` idempotent without `--force`: identical text means the
    /// vector would be identical, so the embed is skipped.
    pub digest_sha256: String,
    pub summary: Option<String>,
    pub summary_updated_at: Option<DateTime<Utc>>,
    pub digest_vec: Vec<f32>,
    pub summary_vec: Option<Vec<f32>>,
}

/// Which vector matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedOn {
    Digest,
    Summary,
}

/// One search hit: the stored row minus its vectors, plus the score.
#[derive(Debug, Clone)]
pub struct ConversationHit {
    pub session_id: String,
    pub project_id: String,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub first_prompt: Option<String>,
    pub summary: Option<String>,
    pub indexed_complete: bool,
    pub score: f64,
    pub matched_on: MatchedOn,
}

/// Handle on one project's `conversations` table.
#[derive(Clone)]
pub struct ConversationIndex {
    connection: Arc<Connection>,
    dimensions: usize,
}

impl ConversationIndex {
    /// Open (creating if absent) the conversation table for a **root** project
    /// id.
    pub async fn open(root_project_id: &str, dimensions: usize) -> Result<Self> {
        let dir = paths::lancedb_dir(root_project_id)?;
        Self::open_at(&dir, dimensions).await
    }

    /// Does this project already have a conversations table?
    ///
    /// Consulted before opening one for a project that is not the caller's:
    /// [`Self::open`] *creates* the table, so a machine-wide search would
    /// otherwise leave an empty one behind in every project it merely looked
    /// at.
    pub fn exists(root_project_id: &str) -> bool {
        paths::lancedb_dir(root_project_id)
            .map(|d| Self::table_path_in(&d).exists())
            .unwrap_or(false)
    }

    /// Where the table sits inside a project's `lancedb/` directory.
    ///
    /// Exposed because `paths::holds_irreplaceable_data` has to recognise this
    /// table without opening it: the curated summaries in `summary_vec` are
    /// authored, not derived, so a data directory holding this table must
    /// survive a sweep even though everything else under `lancedb/` rebuilds
    /// from the `.md` files.
    pub fn table_path_in(lancedb_dir: &std::path::Path) -> std::path::PathBuf {
        lancedb_dir.join(format!("{TABLE}.lance"))
    }

    /// [`Self::open`] against an explicit LanceDB directory, so tests need no
    /// registry and no project id.
    pub async fn open_at(db_dir: &std::path::Path, dimensions: usize) -> Result<Self> {
        if dimensions == 0 {
            anyhow::bail!("conversation index needs a non-zero embedding width");
        }
        let index = Self::connect_at(db_dir, dimensions).await?;
        index.ensure_table().await?;
        Ok(index)
    }

    /// Open an **existing** table at whatever width it was created with, or
    /// `None` when the project has no table at all. Never creates one.
    ///
    /// The width-agnostic handle. Deleting a row, or reading the curated
    /// summaries off one, is meaningful whatever the stored width is, and
    /// [`Self::open_at`] refuses to hand out a handle whose configured width
    /// disagrees with the table's — so the two operations that have to work
    /// *during* such a disagreement (dropping a session, and carrying its
    /// summaries across a rebuild) come through here.
    pub async fn open_existing_at(db_dir: &std::path::Path) -> Result<Option<Self>> {
        // Checked on the filesystem before connecting: `connect` creates the
        // database directory, and a caller merely asking whether a table is
        // there must not leave one behind.
        if !db_dir.join(format!("{TABLE}.lance")).exists() {
            return Ok(None);
        }
        // A width of 1 is a placeholder: the stored width replaces it below,
        // and the table is never created on this path.
        let probe = Self::connect_at(db_dir, 1).await?;
        let Ok(table) = probe.connection.open_table(TABLE).execute().await else {
            return Ok(None);
        };
        let dimensions = table_dimensions(&table).await?.context(
            "the conversations table has no fixed-width digest_vec column; it was not written \
             by this program",
        )?;
        Ok(Some(Self {
            connection: probe.connection,
            dimensions,
        }))
    }

    /// [`Self::open_existing_at`] for a **root** project id.
    pub async fn open_existing(root_project_id: &str) -> Result<Option<Self>> {
        if !Self::exists(root_project_id) {
            return Ok(None);
        }
        let dir = paths::lancedb_dir(root_project_id)?;
        Self::open_existing_at(&dir).await
    }

    /// Connect to the LanceDB directory without touching the table.
    async fn connect_at(db_dir: &std::path::Path, dimensions: usize) -> Result<Self> {
        let path = db_dir.to_str().context("LanceDB path is not valid UTF-8")?;
        let connection = connect(path)
            .execute()
            .await
            .context("Failed to connect to LanceDB")?;
        Ok(Self {
            connection: Arc::new(connection),
            dimensions,
        })
    }

    /// Drop the whole table, rows and all.
    ///
    /// The only remediation for a stored vector width that no longer matches
    /// the configured one — Lance cannot widen a `FixedSizeList` in place, and
    /// the rows are re-derivable from the stored transcript copies. Curated
    /// summaries are *not* re-derivable, so a caller that drops the table must
    /// read them off with [`Self::curated_summaries`] first and re-attach them
    /// after the rebuild.
    pub async fn drop_table(&self) -> Result<()> {
        self.connection
            .drop_table(TABLE, &[])
            .await
            .context("Failed to drop the LanceDB conversations table")
    }

    /// Every curated summary in the table, as `(session_id, summary)`.
    ///
    /// Read before a [`Self::drop_table`], because an agent-authored summary is
    /// the one thing in a row that no rebuild can recreate.
    pub async fn curated_summaries(&self) -> Result<Vec<(String, String)>> {
        let table = self.table().await?;
        let mut stream = table
            .query()
            .select(Select::Columns(vec!["session_id".into(), "summary".into()]))
            .execute()
            .await
            .context("Failed to scan the conversations table")?;
        let mut out = Vec::new();
        while let Some(batch) = stream.next().await {
            let batch = batch.context("Failed to read a conversations batch")?;
            for i in 0..batch.num_rows() {
                let (Some(id), Some(summary)) = (
                    column_text(&batch, "session_id", i),
                    column_text(&batch, "summary", i),
                ) else {
                    continue;
                };
                out.push((id, summary));
            }
        }
        Ok(out)
    }

    fn schema(&self) -> Arc<Schema> {
        let vector = |name: &str, nullable: bool| {
            Field::new(
                name,
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    self.dimensions as i32,
                ),
                nullable,
            )
        };
        Arc::new(Schema::new(vec![
            Field::new("session_id", DataType::Utf8, false),
            Field::new("project_id", DataType::Utf8, false),
            Field::new("cwd", DataType::Utf8, true),
            Field::new("git_branch", DataType::Utf8, true),
            Field::new("started_at", DataType::Utf8, true),
            Field::new("ended_at", DataType::Utf8, true),
            Field::new("indexed_at", DataType::Utf8, false),
            Field::new("user_turns", DataType::UInt32, false),
            Field::new("assistant_turns", DataType::UInt32, false),
            Field::new("first_prompt", DataType::Utf8, true),
            Field::new("indexed_chars", DataType::UInt32, false),
            Field::new("indexed_complete", DataType::Boolean, false),
            Field::new("digest_sha256", DataType::Utf8, false),
            Field::new("summary", DataType::Utf8, true),
            Field::new("summary_updated_at", DataType::Utf8, true),
            vector("digest_vec", false),
            // Nullable, and that is the whole point of the column: a session
            // nobody has reviewed still has to be searchable.
            vector("summary_vec", true),
        ]))
    }

    /// Create the table, or check that the one already there has this handle's
    /// vector width.
    ///
    /// The check is the point. Lance opens an existing table AS-IS, so after a
    /// `[embeddings].dimensions` change (or a provider swap) the stored width
    /// can differ from the configured one — and then *every* `upsert` and
    /// `search` fails, one confusing Arrow error at a time, with the actual
    /// cause never named. Failing at open says what is wrong and what fixes
    /// it. The same reasoning as `LanceIndex::chunks_table_dimensions`, which
    /// guards the memories store's vectors.
    async fn ensure_table(&self) -> Result<()> {
        if let Ok(table) = self.connection.open_table(TABLE).execute().await {
            if let Some(stored) = table_dimensions(&table).await? {
                if stored != self.dimensions {
                    anyhow::bail!(
                        "the conversations table stores {stored}-dimension vectors but this \
                         project is configured for {} — every conversation-index write and \
                         search would fail against it. Run `engramdb reindex --archive-only` \
                         to rebuild the rows at the new width from the stored transcript \
                         copies.",
                        self.dimensions
                    );
                }
            }
            return Ok(());
        }
        self.connection
            .create_empty_table(TABLE, self.schema())
            .execute()
            .await
            .map(|_| ())
            .context("Failed to create the LanceDB conversations table")
    }

    async fn table(&self) -> Result<Table> {
        self.connection
            .open_table(TABLE)
            .execute()
            .await
            .context("Failed to open the LanceDB conversations table")
    }

    /// Embedding width this table was created with.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }

    /// Insert or replace one session's row.
    ///
    /// Keyed by `session_id`, so re-indexing a session replaces its row rather
    /// than accumulating duplicates — the idempotency the command surface
    /// promises.
    pub async fn upsert(&self, row: &ConversationRow) -> Result<()> {
        // Checked here rather than left to Arrow: `FixedSizeListArray::new`
        // *panics* on a width mismatch, and the release profile is
        // `panic = "abort"`, so a provider/config disagreement would abort the
        // process instead of reporting a fixable error.
        self.check_width(&row.digest_vec, "digest_vec")?;
        if let Some(v) = &row.summary_vec {
            self.check_width(v, "summary_vec")?;
        }
        let batch = self.row_to_batch(row)?;
        let table = self.table().await?;
        let schema = batch.schema();
        let batches = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
        let mut op = table.merge_insert(&["session_id"]);
        op.when_matched_update_all(None);
        op.when_not_matched_insert_all();
        op.execute(Box::new(batches))
            .await
            .context("Failed to upsert a conversation row")?;
        Ok(())
    }

    fn check_width(&self, v: &[f32], column: &str) -> Result<()> {
        if v.len() != self.dimensions {
            anyhow::bail!(
                "conversation {column} has {} dimensions but the index expects {} — the \
                 embedding provider and [embeddings].dimensions disagree",
                v.len(),
                self.dimensions
            );
        }
        Ok(())
    }

    fn vector_array(&self, values: Vec<Option<Vec<f32>>>) -> ArrayRef {
        let width = self.dimensions;
        let mut flat: Vec<f32> = Vec::with_capacity(values.len() * width);
        let mut validity: Vec<bool> = Vec::with_capacity(values.len());
        for v in values {
            match v {
                Some(v) => {
                    flat.extend_from_slice(&v);
                    validity.push(true);
                }
                // A null entry still occupies its slot in the flat child array;
                // the null buffer is what makes it read back as absent.
                None => {
                    flat.extend(std::iter::repeat_n(0.0, width));
                    validity.push(false);
                }
            }
        }
        let nulls = arrow_buffer::NullBuffer::from(validity);
        Arc::new(FixedSizeListArray::new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            width as i32,
            Arc::new(Float32Array::from(flat)) as ArrayRef,
            Some(nulls),
        ))
    }

    fn row_to_batch(&self, row: &ConversationRow) -> Result<RecordBatch> {
        let opt_time = |t: &Option<DateTime<Utc>>| t.map(|t| t.to_rfc3339());
        let columns: Vec<ArrayRef> = vec![
            Arc::new(StringArray::from(vec![row.session_id.as_str()])),
            Arc::new(StringArray::from(vec![row.project_id.as_str()])),
            Arc::new(StringArray::from(vec![row.cwd.clone()])),
            Arc::new(StringArray::from(vec![row.git_branch.clone()])),
            Arc::new(StringArray::from(vec![opt_time(&row.started_at)])),
            Arc::new(StringArray::from(vec![opt_time(&row.ended_at)])),
            Arc::new(StringArray::from(vec![row.indexed_at.to_rfc3339()])),
            Arc::new(UInt32Array::from(vec![row.user_turns])),
            Arc::new(UInt32Array::from(vec![row.assistant_turns])),
            Arc::new(StringArray::from(vec![row.first_prompt.clone()])),
            Arc::new(UInt32Array::from(vec![row.indexed_chars])),
            Arc::new(arrow_array::BooleanArray::from(vec![row.indexed_complete])),
            Arc::new(StringArray::from(vec![row.digest_sha256.as_str()])),
            Arc::new(StringArray::from(vec![row.summary.clone()])),
            Arc::new(StringArray::from(vec![opt_time(&row.summary_updated_at)])),
            self.vector_array(vec![Some(row.digest_vec.clone())]),
            self.vector_array(vec![row.summary_vec.clone()]),
        ];
        RecordBatch::try_new(self.schema(), columns)
            .context("Failed to build a conversation RecordBatch")
    }

    /// The digest checksum recorded for one session, if it is indexed.
    ///
    /// What `harvest index` consults to stay idempotent: identical text means
    /// an identical vector, so the embed is skipped unless `--force`.
    pub async fn digest_sha(&self, session_id: &str) -> Result<Option<String>> {
        Ok(self.fetch(session_id).await?.map(|r| r.digest_sha256))
    }

    /// Read one row back, vectors included.
    ///
    /// `harvest summary` needs it: replacing the summary must not disturb
    /// `digest_vec`, and `merge_insert` writes whole rows.
    pub async fn fetch(&self, session_id: &str) -> Result<Option<ConversationRow>> {
        let table = self.table().await?;
        let mut stream = table
            .query()
            .only_if_expr(lancedb::expr::col("session_id").eq(lancedb::expr::lit(session_id)))
            .limit(1)
            .execute()
            .await
            .context("Failed to fetch a conversation row")?;
        while let Some(batch) = stream.next().await {
            let batch = batch.context("Failed to read a conversations batch")?;
            if batch.num_rows() > 0 {
                return Ok(Some(read_row(&batch, 0)?));
            }
        }
        Ok(None)
    }

    /// Drop one session's row.
    pub async fn delete(&self, session_id: &str) -> Result<bool> {
        let existed = self.fetch(session_id).await?.is_some();
        let table = self.table().await?;
        table
            .delete(&format!(
                "session_id = '{}'",
                session_id.replace('\'', "''")
            ))
            .await
            .context("Failed to delete a conversation row")?;
        Ok(existed)
    }

    /// Number of indexed conversations.
    pub async fn count(&self) -> Result<usize> {
        let table = self.table().await?;
        table
            .count_rows(None)
            .await
            .context("Failed to count conversations")
    }

    /// Search both vectors and merge.
    ///
    /// Each column is queried separately and the results are folded per
    /// session: the better score wins, and an exact tie resolves to the
    /// summary because a human wrote it, so a match there is higher precision.
    ///
    /// `since` is a **prefilter**, not a post-filter. Applied after the k-NN
    /// `limit` it silently produced empty results: the nearest `limit`
    /// conversations can all be older than the window while a match inside it
    /// sits at rank `limit + 1`, and the caller then reports "no indexed
    /// conversation matched" — an assertion of absence it cannot support.
    /// `harvest list --since` has always filtered before its limit; this is
    /// conversations catching up.
    ///
    /// The predicate is resolved to a session-id set first because `ended_at`
    /// is an RFC 3339 *string* (the convention the memories table already uses
    /// for timestamps), so comparing it lexically inside the query engine
    /// would be wrong across offsets. One row per session makes that scan
    /// cheap, and the *comparison* still happens in Rust on parsed timestamps.
    pub async fn search(
        &self,
        query: &[f32],
        limit: usize,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<ConversationHit>> {
        self.check_width(query, "query vector")?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let window = match since {
            Some(cutoff) => self.since_window(cutoff).await?,
            None => Window::All,
        };
        let filter = match &window {
            // No session ended inside the window, so no query can match one.
            Window::None => return Ok(Vec::new()),
            Window::All => None,
            Window::Only(expr) => Some(expr.as_str()),
        };
        let mut best: std::collections::HashMap<String, ConversationHit> =
            std::collections::HashMap::new();
        for (column, matched_on) in [
            ("digest_vec", MatchedOn::Digest),
            ("summary_vec", MatchedOn::Summary),
        ] {
            for hit in self
                .search_column(column, matched_on, query, limit, filter)
                .await?
            {
                match best.entry(hit.session_id.clone()) {
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(hit);
                    }
                    std::collections::hash_map::Entry::Occupied(mut o) => {
                        let keep = hit.score > o.get().score
                            || (hit.score == o.get().score && hit.matched_on == MatchedOn::Summary);
                        if keep {
                            o.insert(hit);
                        }
                    }
                }
            }
        }
        let mut out: Vec<ConversationHit> = best.into_values().collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Deterministic order for equal scores, so paging and test
                // assertions do not depend on HashMap iteration order.
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// Which sessions ended inside the `since` window, as a query predicate.
    ///
    /// Emits whichever of `IN` / `NOT IN` names the smaller set, so the common
    /// shapes — a short recent window over a long history, or a window wide
    /// enough to keep almost everything — both cost a short predicate.
    async fn since_window(&self, cutoff: DateTime<Utc>) -> Result<Window> {
        let table = self.table().await?;
        let mut stream = table
            .query()
            .select(Select::Columns(vec![
                "session_id".into(),
                "ended_at".into(),
            ]))
            .execute()
            .await
            .context("Failed to scan the conversations table")?;
        let (mut inside, mut outside) = (Vec::new(), Vec::new());
        while let Some(batch) = stream.next().await {
            let batch = batch.context("Failed to read a conversations batch")?;
            for i in 0..batch.num_rows() {
                let Some(id) = column_text(&batch, "session_id", i) else {
                    continue;
                };
                let ended = column_text(&batch, "ended_at", i)
                    .as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.with_timezone(&Utc));
                // A session with no end timestamp cannot be shown to fall
                // inside the window, so it is excluded rather than assumed
                // recent — the same rule `harvest list` applies to `--since`.
                if ended.is_some_and(|end| end >= cutoff) {
                    inside.push(id);
                } else {
                    outside.push(id);
                }
            }
        }
        Ok(match (inside.is_empty(), outside.is_empty()) {
            (true, _) => Window::None,
            (_, true) => Window::All,
            _ if inside.len() <= outside.len() => {
                Window::Only(format!("session_id IN ({})", quoted_list(&inside)))
            }
            _ => Window::Only(format!("session_id NOT IN ({})", quoted_list(&outside))),
        })
    }

    async fn search_column(
        &self,
        column: &str,
        matched_on: MatchedOn,
        query: &[f32],
        limit: usize,
        filter: Option<&str>,
    ) -> Result<Vec<ConversationHit>> {
        let table = self.table().await?;
        let mut search = table
            .vector_search(query.to_vec())
            .context("Failed to build the conversation search")?
            .column(column)
            .limit(limit)
            .nprobes(NPROBES)
            .refine_factor(REFINE_FACTOR);
        if let Some(filter) = filter {
            // Prefilter (LanceDB's default for `only_if`), so `limit` is spent
            // on rows that already satisfy the predicate.
            search = search.only_if(filter);
        }
        let mut stream = search
            .execute()
            .await
            .context("Failed to execute the conversation search")?;

        let mut hits = Vec::new();
        while let Some(batch) = stream.next().await {
            let batch = batch.context("Failed to read a conversations batch")?;
            let distances = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<Float32Array>())
                .context("conversation search result has no _distance column")?;
            for i in 0..batch.num_rows() {
                // A null `summary_vec` still produces a row in some LanceDB
                // plans, with a null distance. Treating that as a hit would
                // rank every unreviewed session against every query.
                if distances.is_null(i) {
                    continue;
                }
                let row = read_row(&batch, i)?;
                if matched_on == MatchedOn::Summary && row.summary_vec.is_none() {
                    continue;
                }
                hits.push(ConversationHit {
                    session_id: row.session_id,
                    project_id: row.project_id,
                    cwd: row.cwd,
                    git_branch: row.git_branch,
                    started_at: row.started_at,
                    ended_at: row.ended_at,
                    first_prompt: row.first_prompt,
                    summary: row.summary,
                    indexed_complete: row.indexed_complete,
                    score: 1.0 / (1.0 + distances.value(i) as f64),
                    matched_on,
                });
            }
        }
        Ok(hits)
    }
}

/// What a `since` cutoff reduces to once the sessions are known.
enum Window {
    /// Nothing ended inside the window.
    None,
    /// Everything did, so no predicate is needed.
    All,
    /// This SQL predicate names the surviving sessions.
    Only(String),
}

/// SQL string literals for a set of session ids.
///
/// The writers validate session ids, but a row can also arrive from a restored
/// backup or a hand-edited store, so the quote is doubled rather than assumed
/// absent.
fn quoted_list(ids: &[String]) -> String {
    ids.iter()
        .map(|id| format!("'{}'", id.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One non-null string cell, or `None` when the column is absent or null.
fn column_text(batch: &RecordBatch, name: &str, i: usize) -> Option<String> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .filter(|a| !a.is_null(i))
        .map(|a| a.value(i).to_string())
}

/// The vector width the table's `digest_vec` column was created with.
async fn table_dimensions(table: &Table) -> Result<Option<usize>> {
    let schema = table
        .schema()
        .await
        .context("Failed to read the LanceDB conversations table schema")?;
    Ok(schema
        .field_with_name("digest_vec")
        .ok()
        .and_then(|field| match field.data_type() {
            DataType::FixedSizeList(_, width) => Some(*width as usize),
            _ => None,
        }))
}

/// Decode one row of a `conversations` batch.
///
/// Vector columns are optional in the projection: a search result selects them
/// too, but a caller that only asked for metadata should not fail here.
fn read_row(batch: &RecordBatch, i: usize) -> Result<ConversationRow> {
    let text = |name: &str| -> Option<String> {
        batch
            .column_by_name(name)
            .and_then(|c| c.as_any().downcast_ref::<StringArray>())
            .filter(|a| !a.is_null(i))
            .map(|a| a.value(i).to_string())
    };
    let time = |name: &str| -> Option<DateTime<Utc>> {
        text(name)
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
    };
    let number = |name: &str| -> u32 {
        batch
            .column_by_name(name)
            .and_then(|c| c.as_any().downcast_ref::<UInt32Array>())
            .filter(|a| !a.is_null(i))
            .map(|a| a.value(i))
            .unwrap_or(0)
    };
    let vector = |name: &str| -> Option<Vec<f32>> {
        let list = batch
            .column_by_name(name)?
            .as_any()
            .downcast_ref::<FixedSizeListArray>()?;
        if list.is_null(i) {
            return None;
        }
        let values = list.value(i);
        let floats = values.as_any().downcast_ref::<Float32Array>()?;
        Some(floats.values().to_vec())
    };
    Ok(ConversationRow {
        session_id: text("session_id")
            .ok_or_else(|| anyhow!("conversation row without a session_id"))?,
        project_id: text("project_id").unwrap_or_default(),
        cwd: text("cwd"),
        git_branch: text("git_branch"),
        started_at: time("started_at"),
        ended_at: time("ended_at"),
        indexed_at: time("indexed_at").unwrap_or_else(Utc::now),
        user_turns: number("user_turns"),
        assistant_turns: number("assistant_turns"),
        first_prompt: text("first_prompt"),
        indexed_chars: number("indexed_chars"),
        indexed_complete: batch
            .column_by_name("indexed_complete")
            .and_then(|c| c.as_any().downcast_ref::<arrow_array::BooleanArray>())
            .filter(|a| !a.is_null(i))
            .map(|a| a.value(i))
            .unwrap_or(true),
        digest_sha256: text("digest_sha256").unwrap_or_default(),
        summary: text("summary"),
        summary_updated_at: time("summary_updated_at"),
        digest_vec: vector("digest_vec").unwrap_or_default(),
        summary_vec: vector("summary_vec"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const DIM: usize = 4;

    fn row(id: &str, digest: [f32; DIM]) -> ConversationRow {
        ConversationRow {
            session_id: id.into(),
            project_id: "proj".into(),
            cwd: Some("/repo".into()),
            git_branch: None,
            started_at: None,
            ended_at: Some(Utc::now()),
            indexed_at: Utc::now(),
            user_turns: 3,
            assistant_turns: 4,
            first_prompt: Some(format!("prompt for {id}")),
            indexed_chars: 100,
            indexed_complete: true,
            digest_sha256: format!("sha-{id}"),
            summary: None,
            summary_updated_at: None,
            digest_vec: digest.to_vec(),
            summary_vec: None,
        }
    }

    async fn index(tmp: &TempDir) -> ConversationIndex {
        ConversationIndex::open_at(&tmp.path().join("lancedb"), DIM)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn reindexing_a_session_replaces_its_row() {
        let tmp = TempDir::new().unwrap();
        let idx = index(&tmp).await;
        idx.upsert(&row("a", [1.0, 0.0, 0.0, 0.0])).await.unwrap();
        let mut second = row("a", [0.0, 1.0, 0.0, 0.0]);
        second.digest_sha256 = "sha-second".into();
        idx.upsert(&second).await.unwrap();

        assert_eq!(idx.count().await.unwrap(), 1, "duplicate row for one id");
        assert_eq!(
            idx.digest_sha("a").await.unwrap().as_deref(),
            Some("sha-second")
        );
    }

    #[tokio::test]
    async fn a_row_with_no_summary_is_still_searchable() {
        // The whole reason `summary_vec` is nullable: a session nobody
        // reviewed must still answer "did we ever discuss X".
        let tmp = TempDir::new().unwrap();
        let idx = index(&tmp).await;
        idx.upsert(&row("a", [1.0, 0.0, 0.0, 0.0])).await.unwrap();

        let hits = idx.search(&[1.0, 0.0, 0.0, 0.0], 5, None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "a");
        assert_eq!(hits[0].matched_on, MatchedOn::Digest);
    }

    #[tokio::test]
    async fn a_summary_match_beats_a_worse_digest_match() {
        let tmp = TempDir::new().unwrap();
        let idx = index(&tmp).await;
        let mut r = row("a", [0.0, 0.0, 1.0, 0.0]);
        r.summary = Some("the build broke on protoc".into());
        r.summary_vec = Some(vec![1.0, 0.0, 0.0, 0.0]);
        idx.upsert(&r).await.unwrap();

        let hits = idx.search(&[1.0, 0.0, 0.0, 0.0], 5, None).await.unwrap();
        assert_eq!(hits.len(), 1, "one row must not yield two hits");
        assert_eq!(hits[0].matched_on, MatchedOn::Summary);
        assert_eq!(
            hits[0].summary.as_deref(),
            Some("the build broke on protoc")
        );
    }

    #[tokio::test]
    async fn an_exact_tie_resolves_to_the_summary() {
        let tmp = TempDir::new().unwrap();
        let idx = index(&tmp).await;
        let mut r = row("a", [1.0, 0.0, 0.0, 0.0]);
        r.summary = Some("same vector both ways".into());
        r.summary_vec = Some(vec![1.0, 0.0, 0.0, 0.0]);
        idx.upsert(&r).await.unwrap();

        let hits = idx.search(&[1.0, 0.0, 0.0, 0.0], 5, None).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].matched_on,
            MatchedOn::Summary,
            "a human wrote the summary, so a tie must break toward it"
        );
    }

    #[tokio::test]
    async fn since_excludes_older_and_undated_sessions() {
        let tmp = TempDir::new().unwrap();
        let idx = index(&tmp).await;
        let mut old = row("old", [1.0, 0.0, 0.0, 0.0]);
        old.ended_at = Some(Utc::now() - chrono::Duration::days(40));
        let mut undated = row("undated", [1.0, 0.0, 0.0, 0.0]);
        undated.ended_at = None;
        idx.upsert(&old).await.unwrap();
        idx.upsert(&undated).await.unwrap();
        idx.upsert(&row("recent", [1.0, 0.0, 0.0, 0.0]))
            .await
            .unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(30);
        let hits = idx
            .search(&[1.0, 0.0, 0.0, 0.0], 10, Some(cutoff))
            .await
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.session_id.as_str()).collect();
        assert_eq!(ids, vec!["recent"]);
    }

    #[tokio::test]
    async fn since_survives_a_candidate_set_full_of_nearer_old_sessions() {
        // The window must narrow the k-NN, not trim its output. With more
        // old-but-nearer conversations than `limit`, a post-filter returns
        // nothing at all — and the caller then reports "no indexed
        // conversation matched", an assertion of absence it cannot support.
        let tmp = TempDir::new().unwrap();
        let idx = index(&tmp).await;
        let long_ago = Utc::now() - chrono::Duration::days(40);
        for n in 0..12 {
            // Exactly the query vector: every one of these outranks the hit.
            let mut old = row(&format!("old-{n:02}"), [1.0, 0.0, 0.0, 0.0]);
            old.ended_at = Some(long_ago);
            idx.upsert(&old).await.unwrap();
        }
        let mut recent = row("recent", [0.9, 0.1, 0.0, 0.0]);
        recent.ended_at = Some(Utc::now());
        idx.upsert(&recent).await.unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(7);
        let hits = idx
            .search(&[1.0, 0.0, 0.0, 0.0], 10, Some(cutoff))
            .await
            .unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.session_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["recent"],
            "the window was applied after the limit"
        );
    }

    #[tokio::test]
    async fn a_window_that_keeps_almost_everything_still_excludes_the_rest() {
        // The `NOT IN` half of the predicate: with the eligible set larger
        // than the excluded one, the filter names the exclusions instead.
        let tmp = TempDir::new().unwrap();
        let idx = index(&tmp).await;
        for n in 0..5 {
            idx.upsert(&row(&format!("in-{n}"), [1.0, 0.0, 0.0, 0.0]))
                .await
                .unwrap();
        }
        let mut old = row("out", [1.0, 0.0, 0.0, 0.0]);
        old.ended_at = Some(Utc::now() - chrono::Duration::days(40));
        idx.upsert(&old).await.unwrap();

        let cutoff = Utc::now() - chrono::Duration::days(7);
        let hits = idx
            .search(&[1.0, 0.0, 0.0, 0.0], 10, Some(cutoff))
            .await
            .unwrap();
        let mut ids: Vec<&str> = hits.iter().map(|h| h.session_id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["in-0", "in-1", "in-2", "in-3", "in-4"]);
    }

    #[tokio::test]
    async fn opening_at_a_different_width_names_the_remediation() {
        // Lance opens an existing table AS-IS, so without this check the
        // disagreement only surfaces as an Arrow error on every upsert and
        // every search, with the actual cause never named.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("lancedb");
        let idx = ConversationIndex::open_at(&dir, DIM).await.unwrap();
        idx.upsert(&row("a", [1.0, 0.0, 0.0, 0.0])).await.unwrap();
        drop(idx);

        let err = ConversationIndex::open_at(&dir, DIM * 2)
            .await
            .err()
            .expect("a width disagreement must not open")
            .to_string();
        assert!(err.contains("4-dimension"), "{err}");
        assert!(err.contains("configured for 8"), "{err}");
        assert!(err.contains("reindex --archive-only"), "{err}");
    }

    #[tokio::test]
    async fn an_existing_table_opens_at_its_stored_width() {
        // The width-agnostic handle: deleting a row and reading the curated
        // summaries off one both have to work *during* a width disagreement,
        // because they are what the repair path is made of.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("lancedb");
        let idx = ConversationIndex::open_at(&dir, DIM).await.unwrap();
        let mut r = row("a", [1.0, 0.0, 0.0, 0.0]);
        r.summary = Some("what this settled".into());
        r.summary_vec = Some(vec![1.0, 0.0, 0.0, 0.0]);
        idx.upsert(&r).await.unwrap();
        idx.upsert(&row("b", [0.0, 1.0, 0.0, 0.0])).await.unwrap();
        drop(idx);

        let reopened = ConversationIndex::open_existing_at(&dir)
            .await
            .unwrap()
            .expect("a table is there");
        assert_eq!(reopened.dimensions(), DIM);
        assert_eq!(
            reopened.curated_summaries().await.unwrap(),
            vec![("a".to_string(), "what this settled".to_string())],
            "only the rows a human wrote a summary for"
        );
        assert!(reopened.delete("b").await.unwrap());

        reopened.drop_table().await.unwrap();
        // A fresh handle at the new width now succeeds where it could not
        // before, which is the whole point of dropping it.
        let wider = ConversationIndex::open_at(&dir, DIM * 2).await.unwrap();
        assert_eq!(wider.count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn open_existing_reports_no_table_rather_than_creating_one() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("lancedb");
        assert!(ConversationIndex::open_existing_at(&dir)
            .await
            .unwrap()
            .is_none());
        assert!(
            !dir.join(format!("{TABLE}.lance")).exists(),
            "probing for a table must not leave one behind"
        );
    }

    #[tokio::test]
    async fn a_width_mismatch_is_an_error_not_a_panic() {
        // `FixedSizeListArray::new` panics on a width mismatch and the release
        // profile is `panic = "abort"`, so this must never reach Arrow.
        let tmp = TempDir::new().unwrap();
        let idx = index(&tmp).await;
        let mut bad = row("a", [1.0, 0.0, 0.0, 0.0]);
        bad.digest_vec = vec![1.0, 2.0];
        let err = idx.upsert(&bad).await.unwrap_err().to_string();
        assert!(err.contains("dimensions"), "{err}");
    }

    #[tokio::test]
    async fn delete_reports_whether_a_row_was_there() {
        let tmp = TempDir::new().unwrap();
        let idx = index(&tmp).await;
        idx.upsert(&row("a", [1.0, 0.0, 0.0, 0.0])).await.unwrap();
        assert!(idx.delete("a").await.unwrap());
        assert!(!idx.delete("a").await.unwrap());
        assert_eq!(idx.count().await.unwrap(), 0);
    }
}
