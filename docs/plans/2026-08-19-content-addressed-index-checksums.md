# Content-addressed checksums for the memories index

> **Status: ANALYSIS / PROPOSED.** Nothing in this document is implemented.
> It answers three questions: what the index and reindex paths do today, what
> staleness they can and cannot see, and what it would take to make reindex
> content-addressed so it skips unchanged memories and can name the
> updated-but-not-indexed ones.

## Short answer

**We already do this — for conversations, not for memories.**

The `conversations` table (harvest / conversation search) is fully
content-addressed: it carries a `digest_sha256` column
(`crates/engram-storage/src/conversation_index.rs:89`), and
`ops::harvest_index::prepare_row` (`src/ops/harvest_index.rs:362-385`) hashes
the reduction text, compares it to the stored row, and returns
`IndexAction::Unchanged` — no embed, no write — when they match, with a `force`
flag to override. That is exactly the mechanism this document proposes
generalising.

The **memories** index has no content hash anywhere. `reindex` is
unconditionally a full rebuild: drop the metadata table, re-read and re-parse
every `.md`, re-derive every keyword stem, and — on the full path — drop the
chunks table and re-embed *every* memory. Staleness detection is a **count
comparison** and an **ID-set comparison**, neither of which can see a memory
whose file changed in place.

So: we can do it, the precedent is in-tree, and the interlocks it needs
(derivation stamps, a schema-migration path, a per-store embedding fingerprint)
already exist. The cost is one schema-version bump and two nullable columns.

## 1. What exists today

### 1.1 Three indexes, three different maturity levels

| Index | Table | Content-addressed? |
| --- | --- | --- |
| Memory metadata | `memories` | **No.** No hash column. Rebuilt wholesale. |
| Memory vectors | `chunks` | **No.** `has_embedding` records *presence*, never *validity*. |
| Conversation search | `conversations` | **Yes.** `digest_sha256`, with a skip path and a `force` override. |

### 1.2 What `reindex` actually does

`MemoryStore::reindex` → `reindex_with(false)` (`store.rs:1505`, `1521`):

1. Take the per-project write lock.
2. `lance_index.clear_memories()` — **drops the whole memories table**
   (`store.rs:1556`). Under a foreign checkout this degrades to upsert-only
   (`store.rs:1544`); a schema migration passes `force_schema_reset = true` and
   clears regardless (`store.rs:1541`).
3. Snapshot `list_chunk_memory_ids()` once, so rebuilt rows can be stamped with
   an authoritative `has_embedding` (`store.rs:1568`).
4. `reindex_dir` for shared and personal memories (`store.rs:1881`). Five
   phases: enumerate `*.md` → read + stat (bounded-concurrency) → parse in
   parallel via rayon → resolve duplicate IDs by mtime → build `IndexEntry`s in
   parallel (this derives `keyword_stems`) → one batched `merge_insert`.
5. Prune orphan chunks whose memory no longer exists on disk.
6. Rewrite `manifest.toml` stats.

`ops::reindex` (`src/ops/reindex.rs:41`) wraps that and adds the vector half:

- **Full reindex, provider available:** `store.clear_chunks()`
  (`reindex.rs:98`) then `engine.embed_memories(&every_memory)`
  (`reindex.rs:163`) — one batched inference over the entire store.
- **`--embeddings-only`:** re-embeds every memory in place, recreating the
  chunks table first only if `chunks_table_dimensions()` disagrees with the live
  provider (`reindex.rs:109-124`).

There is no per-memory "is this still current?" question anywhere in either
half. Every memory is re-parsed, re-stemmed, and re-embedded on every run.

### 1.3 Cost profile

The costs are documented at the call sites in `reindex_dir`:

- **Parse** — ~20 ms per 1,000 memories serially, ~6 ms across four cores
  (`benches/parallel_simd.rs`, `parse/*`). Largest component of the *metadata*
  rebuild.
- **Keyword stems** — ~16 ms per 1,000 serially, ~5.7 ms across four cores
  (`stems/*`). Second largest.
- **Embedding** — not in that bench, but it is the dominant term by orders of
  magnitude. Under the default `metadata_vector` composition every memory is at
  least two chunks (`embedding_texts`, `src/retrieval/engine.rs:1731`), so a
  1,000-memory store is ≥2,000 MiniLM forward passes. The analogous measurement
  on the harvest side has the embed at 71% of `harvest index --all` wall time
  (`benches/harvest_paths.rs`, `index_embed_share/*`), and that path embeds one
  vector per session rather than two-plus per record.

**This asymmetry decides the design.** Skipping the metadata rebuild saves tens
of milliseconds per thousand memories. Skipping the *re-embed* is where the win
is, and it is the half with no staleness signal at all today.

SHA-256 over the files is not a meaningful new cost: `reindex_dir` already reads
every file's bytes into memory in phase 2, and memory files are single-digit
KB. Hashing rides along on a read that already happened.

### 1.4 What staleness detection exists

Three surfaces, all structural rather than content-based:

- **`MemoryStore::check_staleness`** (`store.rs:2017`) → `staleness_message`
  (`store.rs:2307`): compares `.md` file **count** to index row **count** and
  warns if they differ. Runs on the hot path — `query`, `list`, `get` all call
  it (`crates/engram-cli/src/commands/{query,list,get}.rs`).
- **`ops::doctor`** (`src/ops/doctor.rs:30`): compares **ID sets** — index rows
  with no backing file (`stale_entries`) and files with no index row
  (`orphaned_files`).
- **`expected_embedding_fingerprint`** (`src/ops/mod.rs:359`): compares the
  store-wide recorded `model | dimensions | composition` against the live
  provider and tells the user to run `reindex --embeddings-only` on a mismatch.

### 1.5 The four blind spots

Everything above is blind to in-place change, because counts and ID sets are
invariant under it:

1. **A memory edited in place.** `update` rewrites the `.md` *and* the index row
   together, so this is normally consistent — but a hand edit, a `git checkout`,
   a `git merge`, a rebase, or a restore from backup rewrites bytes with nothing
   updating the row. Count unchanged, ID set unchanged, `doctor` reports
   healthy, and the index serves the old summary/tags/stems.
2. **Vectors that no longer describe the text.** `has_embedding` is a boolean
   *presence* mirror (`store.rs:2002`, `lance_index.rs:1385`). Nothing records
   *which text* the stored vectors were computed from. A memory whose content
   changed without a re-embed keeps `has_embedding = true` and ranks on its old
   vector, indefinitely, with no surface reporting it.
3. **An interrupted reindex.** Kill a run after 900 of 1,000 memories and every
   survivor looks identical to every re-embedded one: `has_embedding` is true
   for both. The retry redoes all 1,000.
4. **`updated_at` is writer-controlled, not derived.**
   `upsert_chunks_if_current` (`store.rs:1677`) and `upsert_chunks_batch`
   (`store.rs:1720`) do compare `updated_at` — but as a *race* guard between a
   detached ingest task and a concurrent write, not as a skip predicate. It
   cannot serve as one: a hand edit or a git-applied change alters the body
   without touching the frontmatter timestamp, and reading `updated_at` requires
   the parse we were hoping to skip.

## 2. Why a content hash, and not the cheaper candidates

- **mtime** — free, already stat'ed in phase 2, and wrong. `git checkout`,
  `git clone`, `rsync`, and container image builds all rewrite mtime without
  changing content (spurious full rebuild) and, in the other direction,
  content-preserving tools plus checkout ordering make mtime comparisons between
  a file and a database row unreliable. Memory files are *committed artifacts
  that travel with a clone*; mtime does not survive that trip meaningfully.
- **`updated_at` from the frontmatter** — requires the parse, and is
  writer-controlled (blind spot 4).
- **File size** — collides trivially on edits.
- **SHA-256 of the file bytes** — exact, cheap on bytes already in memory,
  survives clones and checkouts, and is the same instrument the conversation
  index already uses (`harvest::index_text_digest`, `src/ops/harvest.rs:738`).

## 3. Proposed design

### 3.1 Two columns, schema `0.8.0`

Add to `memories_schema()` (`lance_index.rs:379`) and `IndexEntry`
(`lance_index.rs:77`), both `DataType::Utf8`, nullable:

**`content_sha256`** — hex SHA-256 of the **exact bytes of the `.md` file** this
row was derived from. Answers "is this row current with the file?".

**`embed_sha256`** — hex SHA-256 of a canonical join of
`embedding_texts(memory, chunk_tokens, metadata_vector)`
(`src/retrieval/engine.rs:1731`), **salted with the embedding fingerprint**:

```text
embed_sha256 = sha256( model_id ‖ 0x00 ‖ dimensions ‖ 0x00 ‖ composition_id
                       ‖ 0x00 ‖ chunk_tokens ‖ 0x00 ‖ texts.join("\0") )
```

Answers "do this memory's stored vectors describe its current text, under the
current model and composition?" — one column and one comparison covering all
four ways vectors go stale (text changed, model swapped, composition flipped,
`[embeddings].max_tokens` retuned). `chunk_tokens` is in there deliberately: it
is the one axis the store-wide `EmbeddingFingerprint` does *not* cover today, so
a `max_tokens` change currently re-chunks with nothing detecting it.

Bump `manifest::CURRENT_SCHEMA_VERSION` (`crates/engram-storage/src/manifest.rs:41`)
to `0.8.0`. `migrate_schema_if_needed` (`store.rs:1446`) then rebuilds every
existing store's memories table on next open and backfills `content_sha256` for
free. `embed_sha256` backfills as `NULL` (= unknown = must re-embed), which is
correct: we genuinely do not know what text produced the pre-0.8.0 vectors.

Read it through **its own narrow projection**, not by widening
`FILTERING_COLUMNS` (`lance_index.rs:1785`). The repo already made this call for
`SourceSessionLink` (`lance_index.rs:149`): nothing on the query path reads
these columns, and widening the hot projection to serve a maintenance scan costs
every query a column it never uses.

```rust
/// One memory's index-currency digests (schema v0.8.0).
pub struct IndexDigest {
    pub memory_id: String,
    pub content_sha256: Option<String>,
    pub embed_sha256: Option<String>,
    pub has_embedding: bool,
}
```

### 3.2 Write sites

`content_sha256` is stamped wherever a `.md` is written, from the string that is
actually written — three sites, all of which already hold it:

- `store.rs:516` (`create`)
- `store.rs:631` (batch create)
- `store.rs:1059` (`write_updated_locked`)

and in `reindex_dir` phase 2 (`store.rs:1905`), from the bytes just read.

> **Hash the bytes, never a re-serialization.** Stamping
> `sha256(write_memory_file(parsed))` instead would permanently mark every file
> this binary did not itself write — hand-edited, or written by an older
> version — as dirty, because those files need not round-trip to identical
> bytes. Both the write-time stamp and the reindex-time hash must be over the
> literal byte sequence on disk.

`embed_sha256` is computed by the **engine** (which owns `embedding_texts` and
the provider identity) and passed down to storage as an opaque string, so the
dependency edge keeps pointing inward. It rides the existing chunk-write
entries:

```rust
// engram-storage: stores it, never computes it.
pub async fn upsert_chunks_batch(
    &self,
    entries: Vec<(String, DateTime<Utc>, Vec<Vec<f32>>, String /* embed_sha256 */)>,
) -> Result<Vec<String>>;
```

Set alongside `has_embedding` on the same row update that already happens
(`lance_index.rs:1101` `set_has_embedding_batch`), and cleared to `NULL` by
`delete_chunks`.

### 3.3 The skip predicates

**Metadata row rebuild**, in `reindex_dir`, incremental mode only:

```text
skip_row(file) =
     manifest.schema_version == CURRENT_SCHEMA_VERSION
  && manifest.normalizer     == NORMALIZER_STAMP
  && row_for(id_from_stem(file)).content_sha256 == Some(sha256(file_bytes))
```

The id comes from the filename stem via the existing
`extract_id_from_stem` / `stem_matches_id_prefix` helpers
(`crates/engram-storage/src/memory_file/mod.rs:124`, `:138`) — no parse needed,
which is the point.

**Re-embed**, in `ops::reindex`:

```text
skip_embed(memory) =
     !force
  && row.has_embedding
  && row.embed_sha256 == Some(embed_digest(memory, current_fingerprint, chunk_tokens))
```

`row.has_embedding` is load-bearing and not redundant: without it, a memory
whose last reindex ran with the provider down (content stamped, vectors never
written) would match on content forever and never acquire vectors.

### 3.4 Restructuring the destructive steps

Both skips require *not* clearing first, so the two `clear_*` calls become
conditional:

- **`clear_memories()`** → replaced in incremental mode by upsert-changed +
  delete-rows-whose-file-vanished. This mode already exists and is already
  exercised: it is exactly the non-destructive path taken under a foreign
  checkout (`store.rs:1544`).
- **`clear_chunks()`** (`reindex.rs:98`) → dropped in incremental mode.
  `upsert_chunks_batch` already replaces a memory's chunks atomically and drops
  surplus ones (`store::tests::upsert_chunks_batch_drops_surplus_chunks`), and
  the orphan prune already exists at `store.rs:1607`. **Exception:** when
  `chunks_table_dimensions()` disagrees with the live provider the table *must*
  be recreated, so that run forces the full path and skips nothing. The
  detection is already written (`reindex.rs:109-124`); it just needs promoting
  from the `--embeddings-only` branch to both.

A `reindex --force` flag preserves today's unconditional behaviour as the
escape hatch, mirroring `harvest index --force`.

### 3.5 Answering "which items are updated but not indexed"

This is the half that has no answer at all today, and the columns make it a
single comparison:

- **`engramdb reindex --check`** (dry run) — enumerate, hash, and print the ids
  that would be rebuilt and the ids that would be re-embedded, without writing.
  Direct answer to the question, and the natural place for it.
- **`ops::doctor`** — extend `DoctorResult` (`doctor.rs:12`) with
  `drifted_entries: Vec<String>` (file hash ≠ row hash) and `stale_vectors:
  Vec<String>` (`embed_sha256` mismatch). Same class of finding as
  `orphaned_files`, so it folds into `healthy` and into `doctor --fix`'s
  existing reindex action.
- **`check_staleness`** (`store.rs:2017`) — **leave count-based.** It runs on
  every `query`, `list`, and `get`; hashing every file per query trades a
  correct answer for a latency regression on the hot path. Content drift is a
  `doctor` finding, not a per-query one.
- **MCP `memory_reindex`** (`crates/engram-mcp/src/server.rs:2804`) — report the
  same skipped/rebuilt/re-embedded counts.

## 4. Hazards

Ordered by how quietly they fail.

1. **Derived-data drift — the one that matters.** The row is derived from the
   file bytes *through code*: the parser, the stemmer, the column set. If that
   code changes and the bytes do not, the hash matches and we skip — serving
   rows built by the old derivation, silently and forever. The interlock already
   exists and must be respected absolutely: `migrate_schema_if_needed`
   (`store.rs:1446`) forces a destructive rebuild when either
   `manifest.schema_version` or `manifest.normalizer` (`store.rs:1464`) drifts,
   and both are stamped only on success. **Adding a content hash makes the
   existing rule "bump `CURRENT_SCHEMA_VERSION` when `IndexEntry` derivation
   changes" load-bearing rather than merely tidy**, so it needs a test asserting
   that a stamp change forces a full rebuild *with* matching hashes present.
2. **Foreign checkout.** Incremental mode and the shared-ID degraded mode are
   the same mode, which is convenient, but the orphan prune must stay skipped —
   the other clone's files are not visible here and its chunk rows would all
   look like orphans.
3. **Duplicate-ID files.** `reindex_dir` phase 4 resolves two files claiming one
   ID by mtime and *deletes* the loser (`store.rs:1966`). A skip must therefore
   short-circuit **parse**, never **enumeration** — the duplicate check runs off
   the stem-derived id and stays intact.
4. **Partial-failure honesty.** `ops::reindex` deliberately does not stamp the
   fingerprint when any memory failed
   (`reindex_does_not_stamp_fingerprint_when_embeddings_fail`). Per-row
   `embed_sha256` must follow the same discipline — stamped per memory only on
   that memory's successful write, so a partial run leaves the failures honestly
   unstamped and the retry picks up exactly them. This is also what makes
   blind spot 3 (interrupted reindex) resumable.
5. **NULL means unknown, not clean.** Pre-0.8.0 rows, and any row whose write
   raced, carry `NULL`. Every predicate above compares against `Some(..)`, so
   `NULL` falls through to "rebuild" by construction.

## 5. Phasing

| Phase | Scope | Delivers |
| --- | --- | --- |
| 1 | `content_sha256` column, schema `0.8.0`, write-site stamps, `doctor` drift detection, `reindex --check` | Answers "which items are updated but not indexed" — the detection half, no behaviour change to reindex |
| 2 | Incremental metadata rebuild gated on the stamps | Skips parse + stem + row write for unchanged files |
| 3 | `embed_sha256` column, conditional `clear_chunks`, `--force` | Skips the re-embed — the order-of-magnitude win and the resumable-reindex property |

Phase 1 is self-contained and useful on its own: it is pure detection, it
changes no existing behaviour, and it is what makes phases 2 and 3 verifiable
(you can assert the skip decisions match a full rebuild before you trust them).

Phases 2 and 3 both touch snapshot-pinned CLI output, so they need
`cargo insta test --accept --test-runner nextest` and a re-run (the harness
caveat: non-determinism only shows on the second run).

## 6. Open decisions

1. **Two columns or one?** `content_sha256` + `has_embedding` alone would cover
   the re-embed skip, at the cost of re-embedding on frontmatter-only edits
   (`criticality`, `status`) and of losing resumability across a model change.
   The proposal takes both columns because the second property is the one that
   makes an interrupted reindex cheap. A one-column variant is a legitimate
   smaller first cut.
2. **Should incremental be the default, or opt-in behind `--incremental`?** The
   conservative order is: ship it behind a flag, verify skip decisions match a
   full rebuild on real stores, then flip the default and keep `--force`.
3. **Does `check_staleness` want a cheap deep mode?** Hashing on every query is
   out. An opt-in `--deep` on `list`/`query`, or a sampled check, is possible —
   but `doctor` is probably the right and only home.
