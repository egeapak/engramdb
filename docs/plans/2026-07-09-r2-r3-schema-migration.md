# R2/R3: `decay` + `has_embedding` memories-table columns (schema-version bump)

> **Status: IMPLEMENTED (schema `0.2.0`).** Both columns, the backfill-on-open
> migration, the `has_embedding` write-path invariant, and both retrieval
> consumers (R2 no-query-Rank fast path, R3 chunk-scan elimination) are in the
> tree with tests. The design below is retained as the rationale; the one
> deliberate change from the original plan is that **migration is automatic on
> store open** (a reindex — seconds, no re-embed — triggered when the manifest's
> `schema_version` is behind), rather than a manual step, so the columns are
> always authoritative and consumers never see a half-migrated store.
>
> **Key symbols:** `manifest::CURRENT_SCHEMA_VERSION`,
> `MemoryStore::migrate_schema_if_needed` (store.rs), the `decay`/`has_embedding`
> fields on `IndexEntry`/`IndexForFiltering` + `LanceIndex::{has_chunks,
> set_has_embedding}` (lance_index.rs), `scoring::ScoreTarget` +
> `composite_score_target*`, and `RetrievalEngine::rank_scope_only_from_index`.
> **Tests:** `store::tests::{schema_migration_on_open_backfills_decay_and_has_embedding,
> has_embedding_flag_tracks_chunk_lifecycle}`,
> `retrieval::engine::tests::rank_from_index_matches_file_load`, and the
> `manifest` legacy-version test.

## Original risk picture (why it was deferred)

PR #56 landed the safe R2/R3 wins (filter pushdown, gated IVF index) and left
two columns documented at their call sites as "one schema-version bump with a
backfill-on-open migration":

- **`decay`** (R2) — lets the no-query Rank path score from the index instead of
  reading+parsing every `.md` file.
- **`has_embedding`** (R3) — lets a semantic query skip the per-query
  `list_chunk_memory_ids` chunks-table scan.

Three things make this genuinely risky, all in the most safety-critical code:

1. **No LanceDB migration machinery exists.** `ensure_table_exists`
   (`crates/engram-storage/src/lance_index.rs:270`) is create-if-missing only and
   never reconciles columns. Adding a column to `memories_schema()`
   (`lance_index.rs:235`) **breaks every existing store** immediately: reads that
   `select` the new column error ("column not found"), and the next
   `entries_to_batch` (`lance_index.rs:1131`) builds an N+1-column batch that
   `merge_insert` rejects against the old table. So the backfill MUST run on
   open, before any read/write touches the new column.

2. **`has_embedding` is a cross-table invariant on the async write path.** The
   memory index row is written in `create` (`store.rs:321`) *before* its chunks,
   which are embedded and committed **asynchronously** by `spawn_ingest` →
   `upsert_chunks_if_current` (`store.rs:853`). So `IndexEntry::from(&Memory)`
   (`lance_index.rs:152`) cannot know `has_embedding` — it must be maintained at
   `upsert_chunks` (set true) and `delete_chunks` (set false) via a per-row
   LanceDB column update, and rebuilt authoritatively at reindex. Getting this
   invariant wrong silently mis-scores semantic queries.

3. **R2's payoff needs a retrieval hot-path refactor, not just a column.**
   `composite_score` (`src/scoring/composite.rs:199`) reads `memory.created_at`,
   `memory.decay`, `memory.criticality`, `memory.physical`, `memory.logical`,
   `memory.provenance.source`, `memory.status`. To score the no-query Rank path
   from the index (`engine.rs:715` Step 3) without `get_batch`, the
   `IndexForFiltering` projection (`lance_index.rs:116`) must carry `decay`
   **plus** `created_at`, `provenance_source`, and `status` (currently absent),
   and scoring needs a from-projection entry point. The survivors still need a
   `get_batch` to materialize `ScoredMemory`, so the win is "read K files instead
   of N", K = `max_results`.

## Implementation plan

### Phase A — schema + write path (`crates/engram-storage/`)

- `memories_schema()` (`lance_index.rs:235`): add
  `Field::new("decay", DataType::Utf8, true)` (JSON of `Option<Decay>`, nullable
  like `expires_at`) and `Field::new("has_embedding", DataType::Boolean, false)`.
- `IndexEntry` (`lance_index.rs:72`) + `From<&Memory>` (`:152`): add `decay:
  Option<Decay>` from `memory.decay`; add `has_embedding: bool` **defaulted
  false** at from-Memory (chunks are async — see risk #2).
- `entries_to_batch` (`:1131`): serialize `decay` (`serde_json::to_string` of the
  `Option`, null when `None`) and `has_embedding` (`BooleanArray`).
- Extend the read projections/decoders that need the columns:
  `IndexForFiltering` (`:116`) gains `decay`, `created_at`, `provenance_source`,
  `status`; `list_for_filtering_where` (`:591`) select list + `batch_to_for_filtering`
  (`:1387`) decoder updated (null-safe `decay` like `expires_at`).
- `has_embedding` maintenance: after `upsert_chunks` (`store.rs:832`/`:853`) set
  the row's `has_embedding=true`; in `delete_chunks` (`:884`) set false. Use a
  LanceDB `update().only_if("id = '<escaped>'").column("has_embedding", "true")`
  (verify the 0.30 API; escape the id exactly like `vector_search`/`find_ids_by_prefix`).

### Phase B — backfill-on-open (`crates/engram-storage/`)

- Add `const CURRENT_SCHEMA_VERSION: &str` and bump it. Reuse the unused
  `Manifest.schema_version` (`manifest.rs:22`, default `"0.1.0"`) as the gate;
  keep the `#[serde(default)]` back-compat pattern (`manifest.rs:40`).
- On `MemoryStore` open (after `load_manifest`, `manifest.rs:137`): if the stored
  version is behind, run an **index-only rebuild** — `clear_memories`
  (`lance_index.rs:942`) recreates the table from the live schema, then
  `reindex_dir` (`store.rs:987`) re-derives every `IndexEntry` from the parsed
  `.md` files (`decay` comes free); compute `has_embedding` from the chunk-id set
  reindex already gathers for orphan pruning (`store.rs:793`). Do **not**
  re-embed. Then `save_manifest` with the new version (stamp only on success,
  mirroring `ops/reindex.rs:128`).
- The rebuild reads from the authoritative `.md` files, so it is safe and
  idempotent; a second open is a no-op (version matches).

### Phase C — wire the consumers (`src/retrieval/engine.rs`)

- **R2** (Step 3, `:715`): on the no-query Rank path, score `filtered_entries`
  (now carrying decay/created_at/provenance/status) via a new
  `composite_score`-from-projection path, sort, take top-k, then `get_batch`
  **only** the survivors to build `ScoredMemory`. Keep the current file-load path
  as the query/keyword path and as a fallback.
- **R3** (Step 4.5, `:789`): read `has_embedding` straight from the projection
  instead of `store.list_chunk_memory_ids()`.
- Remove the two deferral comments at `:717` and `:806` once done.

### Phase D — validation

- Migration test: an old-schema fixture store opens, backfills, both columns are
  populated, version bumps; second open is a no-op.
- **Equivalence tests (the load-bearing ones):** on a mixed store
  (types/criticality/expiry/decay/embedding-presence), the R2 index-scored order
  and scores are byte-identical to the file-load path, and the R3 embedded-set
  membership matches `list_chunk_memory_ids`.
- `has_embedding` invariant test: create→embed→has_embedding true; delete chunks
  → false; reindex rebuilds it correctly.
- Benches at ≥1k: `query_rank` drops (PR #56 recorded it flat "awaiting the decay
  column"); `query_semantic` no longer pays the chunk scan.
- Full `cargo nextest run --workspace --all-features` green (no scoring
  regression) — this migration MUST be validated against the whole suite, not a
  subset, because it changes the store-open and scoring hot paths.

## Risks

- **Breaking existing stores** if the backfill doesn't run before the first
  column-touching operation — the gate must be at store open, ahead of everything.
- **`has_embedding` drift** from the async ingest ordering — cover with the
  invariant test above; when in doubt the read path already tolerates a missing
  set (falls back to the no-evidence regime), so bias the maintenance toward
  never leaving a chunk-bearing memory marked false.
- **Scoring regression** on the R2 fast path — the equivalence test is the gate;
  keep the file-load path as the fallback so a projection miss degrades to
  correct-but-slower, never wrong.
