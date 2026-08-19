# Content-addressed index checksums — implementation plan

> **Status: PLANNED.** Companion to
> `2026-08-19-content-addressed-index-checksums.md`, which holds the analysis
> and the rationale. This document is the build order: what changes, in which
> file, in which phase, with the tests and the invariants that gate each one.

## Contents

- [0. What changed since the analysis](#0-what-changed-since-the-analysis)
- [1. Design decisions and their alternatives](#1-design-decisions-and-their-alternatives)
- [2. Phase 1 — detection (schema `0.8.0`)](#2-phase-1--detection-schema-080)
- [3. `check_staleness` — the tiered check](#3-check_staleness--the-tiered-check)
- [4. Phase 2 — skip the re-embed](#4-phase-2--skip-the-re-embed)
- [5. Phase 3 (optional) — skip the metadata rebuild](#5-phase-3-optional--skip-the-metadata-rebuild)
- [6. Concurrency and locking](#6-concurrency-and-locking)
- [7. Scope interactions](#7-scope-interactions)
- [8. Failure-mode matrix](#8-failure-mode-matrix)
- [9. Costs — this is not free](#9-costs--this-is-not-free)
- [10. Commit-sized sequencing](#10-commit-sized-sequencing)
- [11. Docs to update](#11-docs-to-update)
- [12. Rollback](#12-rollback)

## 0. What changed since the analysis

Three corrections, all from re-reading the call sites and the LanceDB API.

**0.1 The metadata-rebuild skip is demoted to optional.** The analysis phased
the work detection → incremental metadata rebuild → embed skip. Wrong order: the
middle phase buys the least and costs the most. The metadata rebuild is ~36 ms
per 1,000 memories (parse ~20 ms + stems ~16 ms, `benches/parallel_simd.rs`),
while making it incremental means **`reindex` stops being an unconditional
from-scratch rebuild** — and reindex is the documented repair path for a corrupt
index row, a stale duplicate file, and a drifted project key
(`crates/engram-cli/src/commands/repair.rs:210`, `commands/doctor.rs:395`).

All four blind spots close with detection plus the embed skip. The metadata skip
closes none of them. It moves to phase 3, opt-in, and may never be worth
building.

**0.2 `check_staleness` gets a real design, not a deferral.** It is the message
users actually see, on every `query`/`list`/`get`, and today it is wrong in both
directions. §3 gives it a tiered check that is exact under a declared byte
budget and near-free below it.

**0.3 `embed_sha256` moves to the chunks table.** The analysis put it on the
memories row next to `has_embedding`. That is the wrong table, for a reason that
only shows up in the write API — see §1.2. It goes in the chunks table, written
in the same `RecordBatch` as the vectors it describes.

## 1. Design decisions and their alternatives

### 1.1 Why SHA-256 of file bytes, and not the cheaper candidates

| Candidate | Verdict |
| --- | --- |
| **mtime** | Rejected. Memory files are committed artifacts that travel with a clone; `git checkout`/`clone`/`rsync` rewrite mtime without changing content (spurious rebuild), and content-preserving tooling makes file-vs-row mtime comparison unreliable in the other direction. |
| **`updated_at` frontmatter** | Rejected. Writer-controlled — a hand edit or a git-applied change alters the body without touching it — and reading it costs the parse we wanted to skip. It stays what it is today: a race guard in `upsert_chunks_if_current` (`store.rs:1677`), not a skip predicate. |
| **file size alone** | Rejected as the authority, **adopted as a discriminator**. Collides trivially on same-length edits, but it is one `statx` and it is what makes §3's hot-path tier affordable. |
| **SHA-256 of file bytes** | Adopted. Exact, cheap on bytes already in memory, survives clones, and is the instrument the conversation index already uses (`harvest::index_text_digest`, `src/ops/harvest.rs:738`). |

`sha2`, hex-encoded, no new dependency. **Note the crate gotcha:** `sha2` 0.11's
`finalize` returns a `hybrid_array::Array` with no `LowerHex`, so `{:x}` does not
compile. All three existing digest sites document this
(`harvest.rs:739`, `transcript_archive::hex_digest`, `project_id::hash_to_id`);
the new helper must too.

### 1.2 Why `embed_sha256` lives on the chunks table

The memories row looked like the obvious home — right next to `has_embedding`,
which it resembles. The write API says otherwise.

`set_has_embedding_batch` (`lance_index.rs:1101`) is
`table.update().only_if(filter).column("has_embedding", "true")` — a **SQL
update setting one column to one constant for every matched row**. That is why
it takes `value: bool` for the whole batch. `embed_sha256` is a *different value
per memory*, so this mechanism cannot express it. The alternatives on the
memories table are one update per memory (500 round trips per reindex batch) or
a read-patch-write through `merge_insert` with full `IndexEntry` rows, which
`upsert_chunks_batch` does not have and which reintroduces a lost-update race.

Putting it on the chunks table dissolves the problem and buys a correctness
property:

- **Written atomically with the vectors it describes.** It rides the existing
  per-memory `RecordBatch` in `upsert_chunks_batch` (`lance_index.rs:1012`). It
  is structurally impossible to have vectors whose digest disagrees, or a digest
  with no vectors — which is exactly the crash-window inconsistency
  `has_embedding` *does* have (chunk write and flag update are two commits).
- **Deleted automatically.** `delete_chunks` removes the rows; no separate
  clearing step, no third place to forget.
- **Doesn't widen the memories table** or its migration.

Cost: the digest is denormalised across a memory's chunk rows (~2 rows/memory
under the default composition, 64 hex chars each). Lance dictionary-encodes
repeated strings; this is noise.

Read path: `LanceIndex::list_embed_digests() -> Vec<(String, String)>` — one
projection scan of `(memory_id, embed_sha256)`, deduped. Same cost class as the
`list_chunk_memory_ids()` scan that `reindex_with` already performs
(`store.rs:1568`), and it is only read by reindex and doctor, never by a query.

**Chunks-table schema evolution.** `ensure_chunks_table_exists`
(`lance_index.rs:466`) is create-if-missing only, and the memories-table
migration (rebuild from `.md` files) has no analogue here — you cannot recreate
vectors without re-embedding, which is the entire point. So the column is added
in place. `lancedb` 0.31 supports this:

```rust
use lancedb::table::NewColumnTransform;   // re-export of lance::dataset::NewColumnTransform

// Idempotent: probe the live Arrow schema first, exactly as
// `chunks_table_dimensions()` already does for the vector width.
if table.schema().await?.field_with_name("embed_sha256").is_err() {
    table.add_columns(
        NewColumnTransform::AllNulls(Arc::new(Schema::new(vec![
            Field::new("embed_sha256", DataType::Utf8, true),
        ]))),
        None,
    ).await?;
}
```

`AllNulls` is the correct fill: for a pre-0.8.0 chunks table we genuinely do not
know what text produced the vectors, and NULL means "unknown", which every
predicate treats as "must re-embed".

### 1.3 What the embed digest covers

```text
embed_sha256 = sha256( model_id ‖ 0 ‖ dimensions ‖ 0 ‖ composition_id
                       ‖ 0 ‖ chunk_tokens ‖ 0 ‖ texts.join("\0") )
```

One column, one comparison, four drift axes: text changed, model swapped,
composition flipped, `[embeddings].max_tokens` retuned. The first three are also
covered store-wide by `EmbeddingFingerprint`; the fourth is **not** — a
`max_tokens` change re-chunks today with nothing detecting it
(`effective_chunk_tokens`, `engine.rs:1711`). Salting rather than relying on the
store fingerprint also makes the check *per row*, which is what makes an
interrupted reindex resumable (blind spot 3).

Computed by the **engine**, which owns `embedding_texts` (`engine.rs:1731`) and
the provider identity; passed to storage as an opaque string. Storage stores it
and never computes it, so the dependency edge keeps pointing inward.

### 1.4 Make the content stamp unforgettable

`IndexEntry::from(&Memory)` cannot know the file bytes, so `content_sha256` must
come from whoever wrote or read them. That is the shape of the `has_embedding`
invariant — which the R2/R3 plan flagged as its riskiest part, and which is
hand-patched at all five production call sites.

Do not repeat that. Replace the `From<&Memory>` impl with a constructor that
**requires** the digest, so the compiler catches an omission:

```rust
impl IndexEntry {
    pub fn for_file(memory: &Memory, digest: &FileDigest) -> Self;
}
```

Five production call sites, all already holding or able to cheaply hold the
bytes:

| Site | Source of bytes |
| --- | --- |
| `store.rs:566` `create` | `content` from `write_memory_file` (`:516`) |
| `store.rs:675` `create_batch` | `content` (`:631`) |
| `store.rs:976` `update_many` | returned from `write_updated_file_locked` |
| `store.rs:1092` `sync_index_row_locked` | returned from `write_updated_locked` |
| `store.rs:1997` `reindex_dir` phase 5 | bytes read in phase 2 (`:1905`) |

`write_updated_locked` / `write_updated_file_locked` change from `Result<()>` to
`Result<FileDigest>` so the digest reaches the index-row half. `atomic_write`
writes `content` verbatim, so `FileDigest::of(content.as_bytes())` **is** the
on-disk identity — no re-read anywhere on the write path.

Two test-only call sites (`lance_index.rs:2693`, `:3381`) get a
`#[cfg(test)] fn for_test(memory)` helper stamping a fixed digest, so fixtures
stay one-liners.

## 2. Phase 1 — detection (schema `0.8.0`)

Pure detection. No existing behaviour changes; reindex still rebuilds
everything. This is what makes phases 2–3 *verifiable*: you can assert the skip
decisions agree with a full rebuild before trusting any of them.

### 2.1 The digest helper

New `crates/engram-storage/src/digest.rs`, re-exported as
`engramdb::storage::FileDigest`:

```rust
/// A memory file's content identity: the SHA-256 of its exact bytes on disk,
/// plus their length.
///
/// `len` is not redundant with `sha256`. It is the cheap discriminator the
/// hot-path staleness check uses (one `statx` per file, no reads), with
/// `sha256` as the authority for the bounded deep tier. See
/// `MemoryStore::check_staleness`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDigest {
    pub sha256: String,
    pub len: u64,
}

impl FileDigest {
    /// From the exact byte string that was (or will be) written to disk.
    ///
    /// NEVER from a re-serialization of a parsed memory: a file written by an
    /// older binary or edited by hand need not round-trip to identical bytes,
    /// so hashing `write_memory_file(parsed)` would mark every such file dirty
    /// forever and no skip would ever fire.
    pub fn of(bytes: &[u8]) -> Self;
}
```

### 2.2 Columns and schema bump

`crates/engram-storage/src/lance_index.rs`:

- `memories_schema()` (`:379`) — two nullable fields: `content_sha256: Utf8`,
  `content_len: Int64`.
- `chunks_schema()` (`:430`) — one nullable field: `embed_sha256: Utf8`, plus
  the idempotent `add_columns` evolution of §1.2 in `ensure_chunks_table_exists`.
- `IndexEntry` (`:77`) — `content_sha256: Option<String>`,
  `content_len: Option<u64>`, both `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- `entries_to_batch` (`:2004`) — two more column builders plus the decode side.
- New narrow projection, mirroring `SourceSessionLink` (`:149`). **Do not widen
  `FILTERING_COLUMNS` (`:1785`)** — nothing on the query path reads these, and
  widening the hot projection to serve a maintenance scan costs every query a
  column it never uses:

```rust
/// One memory's index-currency digests (schema v0.8.0).
///
/// Its own narrow projection rather than fields on `IndexForFiltering`: read
/// only by reindex, doctor, and the staleness check — never by a query.
pub struct IndexDigest {
    pub memory_id: String,
    pub content_sha256: Option<String>,
    pub content_len: Option<u64>,
    pub has_embedding: bool,
}
```

with `LanceIndex::list_digests()` (one projection scan) and
`list_embed_digests()` (§1.2).

`manifest.rs:41` — `CURRENT_SCHEMA_VERSION` → `0.8.0`.
`migrate_schema_if_needed` (`store.rs:1446`) then rebuilds every existing store's
memories table on next open and backfills both content columns for free.

### 2.3 `doctor`

`src/ops/doctor.rs:12` — `DoctorResult` gains:

```rust
/// Files whose bytes no longer hash to what the index row recorded — edited
/// in place, or restored by git, with nothing updating the index.
pub drifted_entries: Vec<String>,
/// Memories whose stored vectors were computed from different text, a
/// different model, a different composition, or a different chunk size.
pub stale_vectors: Vec<String>,
```

Both fold into `healthy`, so they reach `doctor --fix`'s existing reindex action
(`commands/doctor.rs:481-489`) and the throttled auto-maintenance warning
(`src/ops/maintenance.rs`, step 2) with no new plumbing — that pass already calls
`doctor` and already tells the user to run `engramdb reindex`.

`stale_vectors` needs an engine (for `chunk_tokens` and the live fingerprint).
The environment doctor path has one; the bare `doctor(&store)` path leaves the
list empty rather than guessing — the same graceful-skip contract used
everywhere else, and **the emptiness must be distinguishable from "checked, none
found"** in the rendered output, or it asserts an absence it cannot support.

### 2.4 `reindex --check`

A dry run: enumerate, hash, print what *would* be rebuilt and re-embedded, write
nothing. The direct answer to "easily find out updated-but-not-indexed items".

`run_reindex` (`commands/reindex.rs:22`) already carries 10 parameters under
`#[allow(clippy::too_many_arguments)]`. Adding `check` and (phase 2) `force`
makes 12. **Introduce a `ReindexOptions` struct and drop the allow** — four call
sites (`lib.rs:645`, `commands/doctor.rs:399`, two in `commands/reindex.rs`
tests) update mechanically.

### 2.5 Tests (phase 1)

Storage, `crates/engram-storage/src/store.rs::tests`:

- `content_digest_stamped_on_create_matches_file_bytes`
- `content_digest_stamped_on_update_and_batch_update`
- `content_digest_survives_title_rename` — the filename changes and the old file
  is removed; the row must carry the *new* file's digest.
- `reindex_backfills_content_digest_from_disk`
- `schema_migration_on_open_backfills_content_digest` — build a **real**
  pre-0.8.0 table by stripping the columns from the live Arrow schema, as
  `store::tests::downgrade_to_0_6_0` does. A manifest-stamp-only downgrade does
  not exercise the failure this guards.
- `chunks_table_add_columns_is_idempotent` — open twice, assert one column and
  no error.
- `hand_edited_file_is_reported_as_drifted` — write a memory, rewrite the `.md`
  behind the store's back, assert `doctor` names it. **Blind spot 1; this is the
  test that must be red before the change.**
- `delete_then_create_keeping_count_equal_is_detected` — today's count check is
  blind to this; §3's id-set tier catches it.
- `same_length_edit_detected_by_content_tier_not_size_tier` — pins the exact
  boundary between §3's tiers C and D.

Engine, `src/retrieval/engine.rs::tests`:

- `embed_digest_changes_with_text_model_composition_and_chunk_tokens` — four
  assertions, one per axis.
- `embed_digest_stable_across_identical_inputs`.

CLI: tier 2 (`tests/cli/reindex.rs` + `tests/cli/snapshot/`) for `reindex --check`
output and exit code; tier 1 renderer snapshots for the new `doctor` sections.

## 3. `check_staleness` — the tiered check

`MemoryStore::check_staleness` (`store.rs:2017`) runs on every `query`, `list`,
and `get` (`commands/{query,list,get}.rs`). Today it compares a `.md` **file
count** to a row count (`staleness_message`, `store.rs:2307`) and is wrong in
both directions: blind to any in-place edit, and blind to a delete+create pair
that keeps the count equal.

Four tiers over one enumeration:

```text
enumerate .md with metadata()  -> (id_from_stem, len) per file   [1 statx/file]
index.list_digests()           -> (id, len, sha256) per row      [1 narrow scan]

A) count mismatch        -> today's message                       [~free]
B) id-set mismatch       -> "N indexed / M on disk, ids differ"    [~free, NEW]
C) len mismatch          -> "N memories changed since indexing"    [~free, NEW]
D) total bytes <= budget -> hash the len-matched files             [bounded, NEW]
   else                  -> report C, and say the deep tier was
                            skipped, naming the budget
```

Tier D is what makes it exact; tier C is what makes it cheap enough to leave on.
A same-length edit is the only case C misses, and D catches it below the budget.
Above the budget the message **says so** rather than implying a clean bill of
health — a check that silently narrows its own scope is precisely the failure
mode this codebase names as a bug.

New config section (there is no `[index]` today):

```toml
[index]
# "counts" | "size" | "content" — how hard check_staleness works on the hot path.
staleness_check = "content"
# Byte budget for the "content" tier. Above this, fall back to "size" and say so.
staleness_max_bytes = 8388608   # 8 MiB ≈ 4,000 memories
```

Costs at the default: one extra `statx` per file, one narrow LanceDB projection
replacing the existing `count()`, and — under budget — a page-cache-warm read
plus SHA-256 (~1.5 GB/s with SHA-NI, standard on x86-64 and ARM). Single-digit
milliseconds on a typical store, degrading by declared rule rather than silent
truncation.

`ops::doctor` remains the unbudgeted authority, and it already rides the
throttled maintenance pass, so an over-budget store still gets an exact answer
every `[maintenance].interval_secs` (default 6 h) and on demand.

**Cache note.** MCP `serve` is long-lived, so a per-process memo would amortise
this to near zero there; the CLI is one process per invocation and would not
benefit. Invalidation is the hard part (any writer, including another process,
dirties it). Deferred, and explicitly out of phase 1.

## 4. Phase 2 — skip the re-embed

The payoff. Gated on phase 1 shipping with observably correct digests.

### 4.1 The predicate

In `ops::reindex` (`src/ops/reindex.rs:87-179`):

```text
skip_embed(memory) =
     !force
  && row.has_embedding
  && embed_digests[memory.id] == Some(embed_digest(memory, fingerprint, chunk_tokens))
```

`row.has_embedding` is load-bearing and not redundant: without it, a memory whose
last reindex ran with the provider down (content stamped, vectors never written)
would match forever and never acquire vectors.

### 4.2 The `clear_chunks` restructuring

`store.clear_chunks()` (`reindex.rs:98`) drops the whole chunks table, which is
incompatible with skipping. It becomes conditional:

- **Skip-enabled** — no `clear_chunks`. `upsert_chunks_batch` already replaces a
  memory's chunks atomically and drops surplus ones
  (`store::tests::upsert_chunks_batch_drops_surplus_chunks`), and the orphan
  prune already exists (`store.rs:1607`).
- **`--force`** — unchanged, today's behaviour exactly.
- **Dimension change** — `chunks_table_dimensions()` disagreeing with the live
  provider *must* recreate the table, so that run forces the full path and skips
  nothing. Detection already written (`reindex.rs:109-124`); promote it from the
  `--embeddings-only` branch to both.
- **Foreign checkout** — unchanged. Already non-destructive; the orphan prune
  must stay skipped (the other clone's files are invisible here, so its chunk
  rows would all look like orphans).

### 4.3 Reporting

`ReindexResult` gains `skipped: usize`. A skip that is not counted is a silent
loss path, and the rule in this codebase is that a loss path the code does not
declare is a bug. Rendered by the CLI, returned by MCP `memory_reindex`
(`crates/engram-mcp/src/server.rs:2804`).

**`doctor --fix` and `repair` must pass `force: true`.** Both invoke reindex
specifically *as a repair*, and a repair that trusts the stamp it is trying to
repair is not a repair.

### 4.4 Tests (phase 2)

- `reindex_skips_unchanged_memories` — two runs; second reports
  `embedded: 0, skipped: N`, vectors byte-identical.
- `reindex_reembeds_only_the_changed_memory`
- `reindex_reembeds_when_model_changes` — stub provider, different `model_id()`.
- `reindex_reembeds_when_composition_flips` — `metadata_vector` toggled.
- `reindex_reembeds_when_chunk_tokens_change` — the axis the store fingerprint
  misses.
- `reindex_reembeds_memory_with_content_match_but_no_vectors` —
  provider-was-down; `has_embedding == false` must defeat the digest match.
- `reindex_force_reembeds_everything`
- `interrupted_reindex_resumes_without_redoing_completed_memories` — provider
  fails after N successes; the retry re-embeds exactly the remainder. Blind
  spot 3.
- `reindex_with_dimension_change_ignores_skips_and_recreates_table`
- `reindex_skip_matches_force_result` — **the gate.** On a fixture store, a
  skip-enabled reindex and a `--force` reindex produce identical chunk tables.

## 5. Phase 3 (optional) — skip the metadata rebuild

Opt-in behind `reindex --incremental`, default off, and a legitimate candidate
for never being built: ~36 ms per 1,000 memories against the from-scratch-rebuild
guarantee.

If built, the predicate adds the derivation stamps:

```text
skip_row(file) =
     manifest.schema_version == CURRENT_SCHEMA_VERSION
  && manifest.normalizer     == NORMALIZER_STAMP
  && row.content_sha256      == Some(sha256(file_bytes))
```

The id comes from the filename stem via `extract_id_from_stem` /
`stem_matches_id_prefix` (`crates/engram-storage/src/memory_file/mod.rs:124`,
`:138`) — no parse needed, which is the entire point.

Two things it must not break:

- **Enumeration is never skipped.** `reindex_dir` phase 4 resolves two files
  claiming one ID by mtime and *deletes* the loser (`store.rs:1966`). A skip
  short-circuits parse, not enumeration; the duplicate check runs off the
  stem-derived id and stays intact.
- **`clear_memories` becomes conditional**, replaced by upsert-changed +
  delete-rows-whose-file-vanished. That mode already exists and is exercised —
  it is the non-destructive path taken under a foreign checkout (`store.rs:1544`).

## 6. Concurrency and locking

Four interactions, checked against the existing lock discipline
(`write_lock.rs`: mutating ops take a per-project advisory `flock(2)`; reads are
lock-free on LanceDB MVCC).

**6.1 The metadata rebuild is fully locked — no new race.** `reindex_with`
acquires the write lock at `store.rs:1522` and holds it across both `reindex_dir`
calls. Phase 2 (read bytes) and phase 5 (upsert rows) are inside the same lock,
so a row can never record a digest for bytes a concurrent writer replaced in
between.

**6.2 Reindex releases the lock before embedding — accounting only.**
`store.reindex()` returns, then `ops::reindex` loads via `get_batch` (lock-free)
and embeds. A concurrent `update` in that window changes both file and row;
`upsert_chunks_batch` then re-reads under the lock and drops the stale entry
(`store.rs:1733-1741`). The memory is correctly left with its own writer's
vectors. Only the reported `embedded`/`skipped` counts can be off by that
memory, which is honest — it *was* skipped by this run.

**6.3 The skip decision is read outside the lock.** `list_digests()` /
`list_embed_digests()` are lock-free reads. A decision made against a row that
changes immediately after is resolved by 6.2's re-read under lock: worst case we
embed something that did not need it. Fails toward extra work, never toward
staleness.

**6.4 Detached ingest keeps its snapshot semantics.** `create` spawns an ingest
task that embeds and calls `upsert_chunks_if_current`, which re-reads under the
lock and compares `updated_at` (`store.rs:1684`). The embed digest is derived
from the *same snapshot* that produced the vectors, and is written in the same
`RecordBatch`, so it can never describe a different version than the vectors
beside it.

**Invariant to state in the doc comments and assert in tests:** a chunk row's
`embed_sha256` always describes the vectors in that same row. It is the one
guarantee that makes the phase-2 predicate sound, and it is bought by §1.2's
table choice, not by discipline.

## 7. Scope interactions

- **Worktrees.** `cli::run` routes ops to the main worktree
  (`storage::worktree`), so digests are computed against the main worktree's
  files. No change; linked worktrees never hold their own store.
- **Foreign checkout (shared project ID).** Digests are per-row, so the other
  clone's rows carry *its* digests. This checkout's reindex must not judge them:
  the existing `checkout_conflict()` guard already scopes the rebuild to local
  files and skips the orphan prune, and `doctor`'s new drift lists must be
  scoped the same way or every one of the other clone's rows is reported as
  drifted. **`staleness_message` already suppresses under a conflict
  (`store.rs:2312`) — the new tiers must inherit that suppression, not just
  tier A.**
- **Global and group stores.** `MemoryStore::open_global` uses the same code
  path; nothing store-kind-specific. `reindex --global` inherits the behaviour.
- **Personal memories.** Live under `<global_data_dir>/projects/<id>/personal/`
  and are enumerated by the second `reindex_dir` call and the second
  `count_md_files` in `check_staleness`. Both tiers must cover them, or a
  personal-memory edit is invisible.

## 8. Failure-mode matrix

Every column is `Option`; NULL means *unknown*, and every predicate compares
against `Some(..)`, so unknown falls through to "rebuild" by construction.

| Situation | `content_sha256` | `embed_sha256` | Outcome |
| --- | --- | --- | --- |
| Pre-0.8.0 store | backfilled by migration | NULL | Full re-embed on next reindex. Correct — vectors' provenance is genuinely unknown. |
| Provider down at last reindex | stamped | absent (no chunk rows) | `has_embedding == false` defeats the match; re-embeds. |
| Interrupted reindex | stamped | stamped for completed only | Retry re-embeds exactly the remainder. |
| Hand-edited `.md` | mismatch | (stale) | `doctor` names it; reindex rebuilds row + vectors. |
| Frontmatter-only edit (`criticality`) | mismatch | match | Row rebuilt, embed skipped. Precisely the win the separate digests buy. |
| `metadata_vector` flipped | match | mismatch | Row kept, everything re-embedded. |
| `max_tokens` retuned | match | mismatch | Re-embeds. The axis the store fingerprint misses today. |
| Dimension change | match | mismatch | Forced full path; table recreated, nothing skipped. |
| Corrupt chunk row, digest intact | match | match | **Skipped.** Accepted limitation; `--force` is the repair, and `doctor --fix`/`repair` pass it. |
| Parser/stemmer changed, bytes same | match | match | `schema_version`/`normalizer` stamp mismatch forces a full rebuild (§9.2). |

## 9. Costs — this is not free

1. **`reindex` stops being unconditionally from-scratch** (phase 2 for vectors,
   phase 3 for rows). It is the documented repair path. Mitigations: `--force`
   keeps today's semantics; `doctor --fix` and `repair` pass it; the equivalence
   test in §4.4 proves the skip path agrees with the forced one on healthy
   stores. The residual is row 9 of §8, accepted knowingly.
2. **Derivation drift becomes load-bearing.** The row is derived from bytes
   *through code* — parser, stemmer, column set. If that code changes and the
   bytes do not, the hash matches and we skip, silently serving rows built by the
   old derivation. The interlock exists (`migrate_schema_if_needed` forces a
   rebuild when `schema_version` or `normalizer` drifts, `store.rs:1464`), but
   the convention "bump `CURRENT_SCHEMA_VERSION` when `IndexEntry` derivation
   changes" stops being tidy and becomes a correctness requirement. Needs a test
   asserting a stamp change forces a full rebuild *with matching digests
   present*, and a line in `CLAUDE.md`.
3. **A new cross-cutting write invariant.** Same class as `has_embedding`, which
   the R2/R3 plan called its riskiest part. Mitigated structurally: §1.4 makes
   the content stamp a constructor argument (compiler-enforced) and §1.2 makes
   the embed stamp physically inseparable from its vectors.
4. **A schema bump rebuilds every store on next open.** Established mechanism,
   seconds, vectors preserved — but a real event for every user on upgrade.
   Column storage is ~136 bytes/row (≈1.4 MB at 10,000 memories); negligible.

## 10. Commit-sized sequencing

Each step compiles, passes `cargo fmt --all` and
`cargo clippy --workspace --all-targets --all-features -- -D warnings`, and
leaves the tree green.

| # | Commit | Notes |
| --- | --- | --- |
| 1 | `digest.rs` + `FileDigest` + unit tests | Leaf, no callers yet |
| 2 | Memories columns + `IndexEntry` fields + `IndexDigest` projection + schema bump to `0.8.0` | Migration test here |
| 3 | `IndexEntry::for_file`, thread digests through the 5 write sites | The compiler drives this one |
| 4 | Chunks column + idempotent `add_columns` + `list_embed_digests` | §1.2 |
| 5 | `embed_digest()` in the engine, threaded through `upsert_chunks_batch` / `upsert_chunks_if_current` | Engine-side tests |
| 6 | `doctor` drift + stale-vector detection | Renderer snapshots |
| 7 | `check_staleness` tiers + `[index]` config | §3 |
| 8 | `ReindexOptions` + `reindex --check` | CLI snapshots |
| — | **Phase 1 complete — ship/review gate** | |
| 9 | Skip predicate + conditional `clear_chunks` + `skipped` reporting | |
| 10 | `--force`, and pass it from `doctor --fix` and `repair` | |
| 11 | Equivalence + resumability tests | The phase-2 gate |

Steps 6–8 touch snapshot-pinned output: `cargo insta test --accept
--test-runner nextest`, **run twice** — non-determinism only shows on the second
run.

## 11. Docs to update

- **`.claude/CLAUDE.md`** — the storage section's schema-migration paragraph
  (0.8.0 and what it added), and a line making the derivation-stamp rule explicit
  now that it is load-bearing (§9.2).
- **`docs/contributors/architecture.md`** — index currency as a concept.
- **`docs/users/`** — `reindex --check`, `--force`, the `[index]` config section.
- **`docs/contributors/testing.md`** — only if the new fixtures need a helper.

## 12. Rollback

Per phase, cheaply:

- **Phase 2** — revert the predicate; `clear_chunks` returns to unconditional.
  The columns stay and remain correct (they are written either way); only the
  skip disappears. No data migration.
- **Phase 1** — the columns are additive and nullable. A binary without them
  ignores them; `CURRENT_SCHEMA_VERSION` back to `0.7.0` makes
  `schema_version_is_current` treat the newer stamp as "at or ahead", which is
  already handled (`store.rs:1451-1456` leaves a newer store untouched). The
  chunks column persists harmlessly since nothing selects it.

There is no state written that an older binary can be broken by, which is what
makes phase 1 safe to ship ahead of the rest.
