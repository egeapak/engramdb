//! The derived `edges` table (schema v0.9.0): every typed edge between
//! memories, queryable from both ends.
//!
//! # Derived, never authoritative
//!
//! Edges are *meaning*, so they live in the `.md` files: a memory's outbound
//! [`Link`]s in its hidden block, plus the older `supersedes` /
//! `superseded_by` fields, which stay canonical for that kind. This table is
//! rebuilt from those files by every `reindex` (including the on-open schema
//! migration) and maintained by every write, so it can never hold an edge no
//! file declares. Its only job is the question a file cannot answer cheaply:
//! *what points at this memory?*
//!
//! # Rows are owned by the file that declared them
//!
//! `owner_id` is the memory whose file produced the row. It is usually the
//! source, but not always: `B.superseded_by = A` is declared in **B**'s file
//! and describes the edge A → B. Every write replaces exactly the rows its
//! memory owns ([`replace_owned`]), so a rewrite of A can never delete the
//! row B's file contributes. The price is that one logical edge can appear
//! twice — `A.supersedes = [B]` and `B.superseded_by = A` both describe
//! A → B — which [`dedupe`] folds on read. That is the mirror the plan asks
//! for: neither field can drift from the table, because the table has no
//! state of its own.
//!
//! # Lifecycle
//!
//! - Created lazily, under the write lock, by the first write or reindex
//!   that needs it — never on open, so a read-only consumer ([`open`]) sees a
//!   missing table as "no edges" and leaves it missing.
//! - A deleted memory's *own* rows go with it; edges *into* it stay, now
//!   dangling, which is legal (it always was for `supersedes`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{
    Array, ArrayRef, BooleanArray, Float64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use chrono::{DateTime, Utc};
use futures_util::stream::StreamExt;
use lancedb::database::CreateTableMode;
use lancedb::expr::{col, is_in, lit};
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::{Connection, Table};
use serde::Serialize;

use engram_types::{Link, LinkKind, LinkOrigin, LinkStatus, Memory, ProvenanceSource};

/// Table name, beside `memories` and `chunks` in the store's `lancedb/`.
pub const EDGES_TABLE: &str = "edges";

/// `IN (…)` lists are chunked at this size (the same bound the memories
/// table's batched deletes use).
const IN_LIST_CHUNK: usize = 500;

/// Which file field declared an edge row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeField {
    /// A [`Link`] in the owner's `links` list.
    Links,
    /// An id in the owner's canonical `supersedes` list (owner → id).
    Supersedes,
    /// The owner's `superseded_by` (that id → owner).
    SupersededBy,
}

impl EdgeField {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Links => "links",
            Self::Supersedes => "supersedes",
            Self::SupersededBy => "superseded_by",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "supersedes" => Self::Supersedes,
            "superseded_by" => Self::SupersededBy,
            _ => Self::Links,
        }
    }
}

/// One row of the `edges` table.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EdgeRow {
    /// The memory whose file declared this row (module docs).
    pub owner_id: String,
    pub src_id: String,
    pub dst_id: String,
    /// `None` = same store as the source.
    pub dst_store: Option<String>,
    pub kind: LinkKind,
    pub confidence: f64,
    pub origin: LinkOrigin,
    pub status: LinkStatus,
    pub created_at: DateTime<Utc>,
    /// The source's validity window is closed. Known only when the owner IS
    /// the source; a `superseded_by` mirror row records `false` and the
    /// reader resolves it against the node set.
    pub src_invalidated: bool,
    pub note: Option<String>,
    pub field: EdgeField,
}

impl EdgeRow {
    /// Identity of the *logical* edge, ignoring which file declared it.
    pub fn edge_key(&self) -> (String, String, String, Option<String>) {
        (
            self.src_id.clone(),
            self.dst_id.clone(),
            self.kind.as_str().to_string(),
            self.dst_store.clone(),
        )
    }

    /// Primary key of the row (owner + field + logical edge).
    fn row_key(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.owner_id,
            self.field.as_str(),
            self.src_id,
            self.kind.as_str(),
            self.dst_store.as_deref().unwrap_or(""),
            self.dst_id
        )
    }
}

/// Every edge row `memory`'s file declares.
///
/// `supersedes` and `superseded_by` are mirrored here (module docs). When a
/// `links` entry of kind `supersedes` names the same target as the canonical
/// list — what `confirm_contradiction_direction` writes, to keep the
/// reviewer's note and the edge's origin — the link's row wins and the
/// canonical one is folded into it, so the owner contributes one row per edge.
pub fn edges_of(memory: &Memory) -> Vec<EdgeRow> {
    let now_invalid = memory.is_invalidated();
    let at = memory.updated_at;
    let mirror_origin = match memory.provenance.source {
        ProvenanceSource::Human => LinkOrigin::Human,
        _ => LinkOrigin::Agent,
    };
    let mut rows: Vec<EdgeRow> = Vec::new();
    let mut seen: HashSet<(String, String, String, Option<String>)> = HashSet::new();
    let mut push = |row: EdgeRow, rows: &mut Vec<EdgeRow>| {
        if seen.insert(row.edge_key()) {
            rows.push(row);
        }
    };
    for link in &memory.links {
        push(row_from_link(memory, link, now_invalid), &mut rows);
    }
    for target in &memory.supersedes {
        push(
            EdgeRow {
                owner_id: memory.id.clone(),
                src_id: memory.id.clone(),
                dst_id: target.clone(),
                dst_store: None,
                kind: LinkKind::Supersedes,
                confidence: 1.0,
                origin: mirror_origin.clone(),
                status: LinkStatus::Confirmed,
                created_at: at,
                src_invalidated: now_invalid,
                note: None,
                field: EdgeField::Supersedes,
            },
            &mut rows,
        );
    }
    if let Some(successor) = &memory.superseded_by {
        push(
            EdgeRow {
                owner_id: memory.id.clone(),
                src_id: successor.clone(),
                dst_id: memory.id.clone(),
                dst_store: None,
                kind: LinkKind::Supersedes,
                confidence: 1.0,
                origin: mirror_origin,
                status: LinkStatus::Confirmed,
                created_at: memory.invalidated_at.unwrap_or(at),
                src_invalidated: false,
                note: None,
                field: EdgeField::SupersededBy,
            },
            &mut rows,
        );
    }
    rows
}

fn row_from_link(memory: &Memory, link: &Link, src_invalidated: bool) -> EdgeRow {
    EdgeRow {
        owner_id: memory.id.clone(),
        src_id: memory.id.clone(),
        dst_id: link.to.clone(),
        dst_store: link.store.clone(),
        kind: link.kind.clone(),
        confidence: link.confidence,
        origin: link.origin.clone(),
        status: link.status.clone(),
        created_at: link.at.unwrap_or(memory.updated_at),
        src_invalidated,
        note: link.note.clone(),
        field: EdgeField::Links,
    }
}

/// Fold rows that describe the same logical edge (module docs), preferring
/// the row the *source* declared — it carries the edge's real metadata — and
/// otherwise the first seen. Order of first appearance is kept.
pub fn dedupe(rows: Vec<EdgeRow>) -> Vec<EdgeRow> {
    let mut index: HashMap<(String, String, String, Option<String>), usize> = HashMap::new();
    let mut out: Vec<EdgeRow> = Vec::new();
    for row in rows {
        match index.get(&row.edge_key()) {
            Some(&i) => {
                if out[i].owner_id != out[i].src_id && row.owner_id == row.src_id {
                    out[i] = row;
                }
            }
            None => {
                index.insert(row.edge_key(), out.len());
                out.push(row);
            }
        }
    }
    out
}

fn schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("row_key", DataType::Utf8, false),
        Field::new("owner_id", DataType::Utf8, false),
        Field::new("src_id", DataType::Utf8, false),
        Field::new("dst_id", DataType::Utf8, false),
        Field::new("dst_store", DataType::Utf8, true),
        Field::new("kind", DataType::Utf8, false),
        Field::new("confidence", DataType::Float64, false),
        Field::new("origin", DataType::Utf8, false),
        Field::new("status", DataType::Utf8, false),
        // RFC 3339 text, like every timestamp in the memories table.
        Field::new("created_at", DataType::Utf8, false),
        Field::new("src_invalidated", DataType::Boolean, false),
        Field::new("note", DataType::Utf8, true),
        Field::new("field", DataType::Utf8, false),
    ]))
}

fn to_batch(rows: &[EdgeRow]) -> Result<RecordBatch> {
    let s = |f: fn(&EdgeRow) -> String| -> ArrayRef {
        Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>()))
    };
    let o = |f: fn(&EdgeRow) -> Option<String>| -> ArrayRef {
        Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>()))
    };
    RecordBatch::try_new(
        schema(),
        vec![
            s(|r| r.row_key()),
            s(|r| r.owner_id.clone()),
            s(|r| r.src_id.clone()),
            s(|r| r.dst_id.clone()),
            o(|r| r.dst_store.clone()),
            s(|r| r.kind.as_str().to_string()),
            Arc::new(Float64Array::from(
                rows.iter().map(|r| r.confidence).collect::<Vec<_>>(),
            )),
            s(|r| r.origin.as_str().to_string()),
            s(|r| r.status.as_str().to_string()),
            s(|r| r.created_at.to_rfc3339()),
            Arc::new(BooleanArray::from(
                rows.iter().map(|r| r.src_invalidated).collect::<Vec<_>>(),
            )),
            o(|r| r.note.clone()),
            s(|r| r.field.as_str().to_string()),
        ],
    )
    .context("Failed to build edges RecordBatch")
}

fn reader(rows: &[EdgeRow]) -> Result<Box<dyn arrow_array::RecordBatchReader + Send>> {
    let batch = to_batch(rows)?;
    Ok(Box::new(RecordBatchIterator::new(
        vec![Ok(batch)].into_iter(),
        schema(),
    )))
}

/// Open the table if it exists, creating nothing. `Ok(None)` = no table =
/// no edges (a store not migrated to 0.9.0 yet, or a read-only view).
pub async fn open(conn: &Connection) -> Result<Option<Table>> {
    let names = conn
        .table_names()
        .execute()
        .await
        .context("Failed to list LanceDB tables")?;
    if !names.iter().any(|n| n == EDGES_TABLE) {
        return Ok(None);
    }
    Ok(Some(
        conn.open_table(EDGES_TABLE)
            .execute()
            .await
            .context("Failed to open LanceDB edges table")?,
    ))
}

/// Open the table, creating it empty when missing. Writers only, under the
/// project write lock — which is also why losing a create race is handled by
/// re-opening rather than treated as an error (the lock excludes EngramDB
/// writers, not a reindex already past it in another process's view).
async fn ensure(conn: &Connection) -> Result<Table> {
    if let Some(t) = open(conn).await? {
        return Ok(t);
    }
    match conn
        .create_empty_table(EDGES_TABLE, schema())
        .execute()
        .await
    {
        Ok(t) => Ok(t),
        Err(create_err) => conn
            .open_table(EDGES_TABLE)
            .execute()
            .await
            .map_err(|_| anyhow::anyhow!("Failed to create LanceDB edges table: {create_err}")),
    }
}

fn owner_filter(owner_ids: &[String]) -> lancedb::expr::DfExpr {
    is_in(col("owner_id"), owner_ids.iter().map(lit).collect())
}

/// Replace every row owned by `owner_ids` with `rows` (whose owners must all
/// be in `owner_ids`), in one commit per chunk of owners.
///
/// One `merge_insert` keyed on `row_key` that also deletes the owners' rows
/// absent from `rows` — so a rewrite is atomic: a reader sees the old rows or
/// the new ones, never neither. With nothing to insert it is a plain delete.
pub async fn replace_owned(
    conn: &Connection,
    owner_ids: &[String],
    rows: &[EdgeRow],
) -> Result<()> {
    if owner_ids.is_empty() {
        return Ok(());
    }
    // Nothing to insert into a store that has never held an edge: there is
    // nothing to delete either, and creating the table here would give every
    // never-linked store one on its first write.
    let table = if rows.is_empty() {
        match open(conn).await? {
            Some(t) => t,
            None => return Ok(()),
        }
    } else {
        ensure(conn).await?
    };
    for owners in owner_ids.chunks(IN_LIST_CHUNK) {
        let set: HashSet<&str> = owners.iter().map(String::as_str).collect();
        let these: Vec<EdgeRow> = rows
            .iter()
            .filter(|r| set.contains(r.owner_id.as_str()))
            .cloned()
            .collect();
        if these.is_empty() {
            table
                .delete(&owner_filter(owners))
                .await
                .context("Failed to delete edges")?;
            continue;
        }
        let mut op = table.merge_insert(&["row_key"]);
        op.when_matched_update_all(None);
        op.when_not_matched_insert_all();
        op.when_not_matched_by_source_delete_expr(owner_filter(owners));
        op.execute(reader(&these)?)
            .await
            .context("Failed to upsert edges")?;
    }
    Ok(())
}

/// Replace the whole table with `rows` in one commit (a full reindex).
///
/// A store with no table and no edges stays without one (a missing table
/// reads as "no edges"), and a missing table is created plainly rather than
/// overwritten: Lance logs a user-visible `WARN` for an overwrite of a dataset
/// that does not exist, which every reindex of a never-linked store would
/// otherwise print.
pub async fn rebuild(conn: &Connection, rows: &[EdgeRow]) -> Result<()> {
    let exists = open(conn).await?.is_some();
    if !exists && rows.is_empty() {
        return Ok(());
    }
    let mode = if exists {
        CreateTableMode::Overwrite
    } else {
        CreateTableMode::Create
    };
    conn.create_table(EDGES_TABLE, reader(rows)?)
        .mode(mode)
        .execute()
        .await
        .context("Failed to rebuild LanceDB edges table")?;
    Ok(())
}

/// Drop the rows owned by `owner_ids` (their memories were deleted).
pub async fn delete_owned(conn: &Connection, owner_ids: &[String]) -> Result<()> {
    let Some(table) = open(conn).await? else {
        return Ok(());
    };
    for owners in owner_ids.chunks(IN_LIST_CHUNK) {
        table
            .delete(&owner_filter(owners))
            .await
            .context("Failed to delete edges")?;
    }
    Ok(())
}

/// Every row in the table (not deduped). Empty when the table is missing.
pub async fn load_all(conn: &Connection) -> Result<Vec<EdgeRow>> {
    let Some(table) = open(conn).await? else {
        return Ok(Vec::new());
    };
    read(
        table
            .query()
            .execute()
            .await
            .context("Failed to query edges")?,
    )
    .await
}

/// The rows touching `id` from either end (not deduped).
pub async fn load_touching(conn: &Connection, id: &str) -> Result<Vec<EdgeRow>> {
    let Some(table) = open(conn).await? else {
        return Ok(Vec::new());
    };
    let filter = col("src_id").eq(lit(id)).or(col("dst_id").eq(lit(id)));
    read(
        table
            .query()
            .only_if_expr(filter)
            .execute()
            .await
            .context("Failed to query edges")?,
    )
    .await
}

async fn read(mut stream: lancedb::arrow::SendableRecordBatchStream) -> Result<Vec<EdgeRow>> {
    let mut out = Vec::new();
    while let Some(batch) = stream.next().await {
        let batch = batch.context("Failed to read edges batch")?;
        let str_col = |name: &str| -> Result<&StringArray> {
            batch
                .column_by_name(name)
                .with_context(|| format!("edges table: missing '{name}'"))?
                .as_any()
                .downcast_ref::<StringArray>()
                .with_context(|| format!("edges table: '{name}' is not Utf8"))
        };
        let owner = str_col("owner_id")?;
        let src = str_col("src_id")?;
        let dst = str_col("dst_id")?;
        let store = str_col("dst_store")?;
        let kind = str_col("kind")?;
        let origin = str_col("origin")?;
        let status = str_col("status")?;
        let created = str_col("created_at")?;
        let note = str_col("note")?;
        let field = str_col("field")?;
        let conf = batch
            .column_by_name("confidence")
            .and_then(|c| c.as_any().downcast_ref::<Float64Array>())
            .context("edges table: bad 'confidence'")?;
        let inval = batch
            .column_by_name("src_invalidated")
            .and_then(|c| c.as_any().downcast_ref::<BooleanArray>())
            .context("edges table: bad 'src_invalidated'")?;
        let opt = |a: &StringArray, i: usize| (!a.is_null(i)).then(|| a.value(i).to_string());
        for i in 0..batch.num_rows() {
            out.push(EdgeRow {
                owner_id: owner.value(i).to_string(),
                src_id: src.value(i).to_string(),
                dst_id: dst.value(i).to_string(),
                dst_store: opt(store, i),
                kind: LinkKind::parse_lenient(kind.value(i)),
                confidence: conf.value(i),
                origin: LinkOrigin::parse_lenient(origin.value(i)),
                status: LinkStatus::parse_lenient(status.value(i)),
                created_at: DateTime::parse_from_rfc3339(created.value(i))
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_default(),
                src_invalidated: inval.value(i),
                note: opt(note, i),
                field: EdgeField::parse(field.value(i)),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engram_types::{MemoryType, Provenance};

    fn mem(id: &str) -> Memory {
        let mut m = Memory::new(MemoryType::Decision, "s", "c", Provenance::human());
        m.id = id.to_string();
        m
    }

    #[test]
    fn supersedes_fields_are_mirrored_and_folded_with_a_link() {
        let mut a = mem("a");
        a.supersedes = vec!["b".into()];
        let mut link = Link::new("b", LinkKind::Supersedes, LinkOrigin::Nli);
        link.note = Some("reviewed".into());
        a.links.push(link);
        let rows = edges_of(&a);
        assert_eq!(rows.len(), 1, "link and canonical field describe one edge");
        assert_eq!(rows[0].field, EdgeField::Links);
        assert_eq!(rows[0].note.as_deref(), Some("reviewed"));

        let mut b = mem("b");
        b.superseded_by = Some("a".into());
        let b_rows = edges_of(&b);
        assert_eq!(b_rows[0].src_id, "a");
        assert_eq!(b_rows[0].dst_id, "b");
        assert_eq!(b_rows[0].owner_id, "b");

        // The two files describe the same edge; the source's row wins.
        let folded = dedupe(b_rows.into_iter().chain(rows).collect());
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].owner_id, "a");
    }

    #[tokio::test]
    async fn table_round_trips_and_replaces_by_owner() {
        let tmp = tempfile::TempDir::new().unwrap();
        let conn = lancedb::connect(tmp.path().to_str().unwrap())
            .execute()
            .await
            .unwrap();
        assert!(open(&conn).await.unwrap().is_none());
        assert!(load_all(&conn).await.unwrap().is_empty(), "missing = none");
        assert!(
            open(&conn).await.unwrap().is_none(),
            "a read creates nothing"
        );

        let mut a = mem("a");
        a.links.push(Link::new(
            "x",
            LinkKind::Other("refutes_v2".into()),
            LinkOrigin::Agent,
        ));
        a.links
            .push(Link::new("y", LinkKind::RelatedTo, LinkOrigin::Human));
        let mut b = mem("b");
        b.superseded_by = Some("a".into());
        let rows: Vec<EdgeRow> = edges_of(&a).into_iter().chain(edges_of(&b)).collect();
        rebuild(&conn, &rows).await.unwrap();
        let all = load_all(&conn).await.unwrap();
        assert_eq!(all.len(), 3);
        assert!(all
            .iter()
            .any(|r| r.kind == LinkKind::Other("refutes_v2".into())));

        // Rewriting `a` without the related_to link drops only a's row.
        a.links.truncate(1);
        replace_owned(&conn, &["a".into()], &edges_of(&a))
            .await
            .unwrap();
        let all = load_all(&conn).await.unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|r| r.owner_id == "b"));

        // Touching `a` from either end: a → x (outbound) and a → b (b's mirror).
        let touching = load_touching(&conn, "a").await.unwrap();
        assert_eq!(touching.len(), 2);

        delete_owned(&conn, &["a".into()]).await.unwrap();
        let all = load_all(&conn).await.unwrap();
        assert_eq!(
            all.len(),
            1,
            "b's row into the deleted a survives (dangling)"
        );
        replace_owned(&conn, &["b".into()], &[]).await.unwrap();
        assert!(load_all(&conn).await.unwrap().is_empty());
    }
}
