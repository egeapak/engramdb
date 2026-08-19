# Content-addressed index checksums — implementation plan

> **Status: PLANNED.** Companion to
> `2026-08-19-content-addressed-index-checksums.md`, which holds the analysis
> and the rationale. This document is the build order: what changes, in which
> file, in which phase, with the tests that gate each one.

## What changed since the analysis

Two corrections, both from re-reading the call sites.

**1. The metadata-rebuild skip is demoted.** The analysis phased the work as
detection → incremental metadata rebuild → embed skip. That is the wrong order,
because the middle phase is the one that buys almost nothing and costs the most.
The metadata rebuild is ~36 ms per 1,000 memories (parse ~20 ms + stems ~16 ms,
`benches/parallel_simd.rs`), while making it incremental means **`reindex` stops
being an unconditional from-scratch rebuild** — and reindex is the documented
repair path for a corrupt index row, a stale duplicate file, and a drifted
project key (`crates/engram-cli/src/commands/repair.rs:210`,
`commands/doctor.rs:395`).

All four blind spots are closed by detection plus the embed skip. The metadata
skip closes none of them. It moves to an optional phase 3, opt-in behind a flag,
and may simply never be worth building.

**2. `check_staleness` gets a real design, not a deferral.** The analysis said
"leave it count-based, drift is doctor's job". That does not meet the bar here:
`check_staleness` is the message users actually see, on every `query`, `list`,
and `get`, and today it is wrong in both directions. It gets a tiered check
(§3) that is exact under a declared byte budget and near-free below it.

## Non-goals

- Hashing on the query path unconditionally. A 10,000-memory store is ~20 MB;
  reading and hashing that on every `get` is a latency regression traded for a
  guarantee `doctor` already provides.
- Changing what is embedded, how it is chunked, or the composition. The digest
  observes `embedding_texts`; it does not alter it.
- Touching the `conversations` table. It is already content-addressed and is the
  model being copied, not a thing being changed.
- A new hash algorithm. SHA-256 via `sha2`, hex-encoded, matching
  `harvest::index_text_digest` (`src/ops/harvest.rs:738`),
  `transcript_archive::hex_digest`, and `project_id::hash_to_id`. (`sha2` 0.11's
  `finalize` returns a `hybrid_array::Array` with no `LowerHex`, so `{:x}` does
  not compile — all three existing sites document this; the new helper must too.)

## Phase 1 — Detection (schema `0.8.0`)

Pure detection. No existing behaviour changes; reindex still rebuilds
everything. This phase is what makes phases 2–3 *verifiable*: you can assert the
skip decisions match a full rebuild before trusting any of them.

### 1.1 The digest helper

New `crates/engram-storage/src/digest.rs`:

```rust
/// A memory file's content identity: the SHA-256 of its exact bytes on disk,
/// plus their length.
///
/// `len` is not redundant. It is the cheap discriminator the hot-path
/// staleness check uses (one `statx` per file, no reads), with `sha256` as the
/// authority for the bounded deep check. See `MemoryStore::check_staleness`.
pub struct FileDigest { pub sha256: String, pub len: u64 }

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

Re-exported as `engramdb::storage::FileDigest`.

### 1.2 Columns and schema bump

`crates/engram-storage/src/lance_index.rs`:

- `memories_schema()` (`:379`) — three fields, all nullable:
  `content_sha256: Utf8`, `content_len: Int64`, `embed_sha256: Utf8`.
- `IndexEntry` (`:77`) — matching fields, `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- `entries_to_batch` (`:2004`) — three more column builders, plus the decode side.
- New narrow projection, mirroring `SourceSessionLink` (`:149`) — **do not widen
  `FILTERING_COLUMNS` (`:1785`)**; nothing on the query path reads these:

```rust
/// One memory's index-currency digests (schema v0.8.0).
pub struct IndexDigest {
    pub memory_id: String,
    pub content_sha256: Option<String>,
    pub content_len: Option<u64>,
    pub embed_sha256: Option<String>,
    pub has_embedding: bool,
}
```
  with `LanceIndex::list_digests() -> Result<Vec<IndexDigest>>` (one projection
  scan) and `set_embed_sha256_batch` alongside the existing
  `set_has_embedding_batch` (`:1101`).

`crates/engram-storage/src/manifest.rs:41` — `CURRENT_SCHEMA_VERSION` → `0.8.0`.
`migrate_schema_if_needed` (`store.rs:1446`) then rebuilds every existing store's
memories table on next open and backfills `content_sha256`/`content_len` for
free. `embed_sha256` backfills as `NULL`, which is correct: we genuinely do not
know what text produced pre-0.8.0 vectors.

### 1.3 Make the stamp unforgettable

`IndexEntry::from(&Memory)` cannot know the file bytes, so `content_sha256` has
to be supplied by whoever wrote or read them — exactly the shape of the
`has_embedding` invariant, which the R2/R3 plan called out as the risky part and
which is patched by hand at all five production call sites (`store.rs:566`,
`:675`, `:976`, `:1092`, `:1997`).

Do not repeat that. Replace the `From<&Memory>` impl with a constructor that
**requires** the digest, so a new call site cannot silently omit it:

```rust
impl IndexEntry {
    pub fn for_file(memory: &Memory, digest: &FileDigest) -> Self;
}
```

Five call sites, all of which already hold or can cheaply hold the bytes:

| Site | Source of bytes |
| --- | --- |
| `store.rs:566` `create` | `content` from `write_memory_file` (`:516`) |
| `store.rs:675` `create_batch` | `content` (`:631`) |
| `store.rs:976` `update_many` | returned from `write_updated_file_locked` |
| `store.rs:1092` `sync_index_row_locked` | returned from `write_updated_locked` |
| `store.rs:1997` `reindex_dir` phase 5 | the bytes read in phase 2 (`:1905`) |

`write_updated_locked` / `write_updated_file_locked` change from `Result<()>` to
`Result<FileDigest>` so the digest reaches the index-row half. `atomic_write`
writes `content` verbatim, so `FileDigest::of(content.as_bytes())` **is** the
on-disk identity — no re-read.

`reindex_dir` gets the digest for free: phase 2 already reads every file's bytes
into memory. Hash in phase 3 (the rayon parse pass) so it parallelises with the
parse it rides along with.

### 1.4 `embed_sha256`

Computed by the **engine** (which owns `embedding_texts`,
`src/retrieval/engine.rs:1731`, and the provider identity) and passed to storage
as an opaque string, so the dependency edge keeps pointing inward — storage
stores it and never computes it.

```text
embed_sha256 = sha256( model_id ‖ 0 ‖ dimensions ‖ 0 ‖ composition_id
                       ‖ 0 ‖ chunk_tokens ‖ 0 ‖ texts.join("\0") )
```

One column covering all four ways vectors go stale: text changed, model swapped,
composition flipped, `[embeddings].max_tokens` retuned. `chunk_tokens` is in
there deliberately — it is the one axis the store-wide `EmbeddingFingerprint`
does not cover, so a `max_tokens` change currently re-chunks with nothing
detecting it.

Threaded through the existing chunk-write entries:

```rust
pub async fn upsert_chunks_batch(
    &self,
    entries: Vec<(String, DateTime<Utc>, Vec<Vec<f32>>, String /* embed_sha256 */)>,
) -> Result<Vec<String>>;
```

Set on the same row update that already maintains `has_embedding`; cleared to
`NULL` by `delete_chunks`. **Stamped per memory only on that memory's own
successful write** — mirroring how `ops::reindex` refuses to stamp the store
fingerprint on partial failure (`reindex_does_not_stamp_fingerprint_when_embeddings_fail`).
That per-memory honesty is precisely what makes an interrupted reindex resumable
(blind spot 3).

### 1.5 `doctor`

`src/ops/doctor.rs:12` — `DoctorResult` gains:

```rust
/// Files whose bytes no longer hash to what the index row recorded — edited
/// in place, or restored by git, with nothing updating the index.
pub drifted_entries: Vec<String>,
/// Memories whose stored vectors were computed from different text, a
/// different model, or a different composition than the current ones.
pub stale_vectors: Vec<String>,
```

Both fold into `healthy`, so they reach `doctor --fix`'s existing reindex action
(`commands/doctor.rs:481-489`) and the throttled auto-maintenance warning
(`src/ops/maintenance.rs`, step 2) with no new plumbing — that pass already
calls `doctor` and already tells the user to run `engramdb reindex`.

`stale_vectors` needs an engine (for `chunk_tokens` and the fingerprint). Doctor's
environment path has one; the bare `doctor(&store)` path leaves the vector list
empty rather than guessing — same graceful-skip contract as everywhere else.

### 1.6 `reindex --check`

A dry run: enumerate, hash, print what *would* be rebuilt and re-embedded, write
nothing. This is the direct answer to "easily find out updated-but-not-indexed
items".

`run_reindex` (`crates/engram-cli/src/commands/reindex.rs:22`) already carries 10
parameters under `#[allow(clippy::too_many_arguments)]`. Adding `check` and
(phase 2) `force` makes 12. **Introduce a `ReindexOptions` struct instead** and
drop the allow — the four call sites (`lib.rs:645`, `commands/doctor.rs:399`,
and two in `commands/reindex.rs` tests) all update mechanically.

### 1.7 Tests (phase 1)

Core, `crates/engram-storage/src/store.rs::tests`:

- `content_digest_stamped_on_create_matches_file_bytes`
- `content_digest_stamped_on_update_and_batch_update`
- `reindex_backfills_content_digest_from_disk`
- `schema_migration_on_open_backfills_content_digest` — build a real pre-0.8.0
  table by stripping the columns from the live Arrow schema, as
  `downgrade_to_0_6_0` does. Not a manifest-stamp-only downgrade.
- `hand_edited_file_is_reported_as_drifted` — write a memory, rewrite the `.md`
  behind the store's back, assert `doctor` names it. This is blind spot 1, and
  it is the test that must be red before the change.
- `delete_then_create_keeping_count_equal_is_detected` — today's count check is
  blind to this; the id-set comparison in §3 catches it.

Engine, `src/retrieval/engine.rs::tests`:

- `embed_digest_changes_with_text_model_composition_and_chunk_tokens` — four
  assertions, one per axis.
- `embed_digest_stable_across_identical_inputs`.

CLI: tier 2 (`crates/engram-cli/tests/cli/reindex.rs` +
`tests/cli/snapshot/`) for `reindex --check` output and exit code; tier 1
renderer snapshots for the new `doctor` sections.

## Phase 2 — Skip the re-embed

The payoff. Everything here is gated on phase 1 shipping and its digests being
observably correct.

### 2.1 The predicate

In `ops::reindex` (`src/ops/reindex.rs:87-179`):

```text
skip_embed(memory) =
     !force
  && row.has_embedding
  && row.embed_sha256 == Some(embed_digest(memory, fingerprint, chunk_tokens))
```

`row.has_embedding` is load-bearing and not redundant: without it, a memory whose
last reindex ran with the provider down (content stamped, vectors never written)
would match on content forever and never acquire vectors.

### 2.2 The `clear_chunks` restructuring

`store.clear_chunks()` (`reindex.rs:98`) drops the whole chunks table, which is
incompatible with skipping. It becomes conditional:

- **Skip-enabled path** — no `clear_chunks`. `upsert_chunks_batch` already
  replaces a memory's chunks atomically and drops surplus ones
  (`store::tests::upsert_chunks_batch_drops_surplus_chunks`), and the orphan
  prune already exists (`store.rs:1607`).
- **Forced full path** — unchanged.
- **Dimension change** — `chunks_table_dimensions()` disagreeing with the live
  provider *must* recreate the table, so that run forces the full path and skips
  nothing. The detection is already written (`reindex.rs:109-124`); promote it
  from the `--embeddings-only` branch to both.
- **Foreign checkout** — unchanged. Already non-destructive; the orphan prune
  must stay skipped (the other clone's files are invisible here, so its chunk
  rows would all look like orphans).

### 2.3 Reporting

`ReindexResult` gains `skipped: usize` (and `skipped_embeddings`), rendered by
the CLI and returned by MCP `memory_reindex`
(`crates/engram-mcp/src/server.rs:2804`). A skip that is not counted is a silent
loss path, and this repo's rule is that a loss path the code does not declare is
a bug.

`--force` restores today's unconditional behaviour. **`doctor --fix` and
`repair` must pass `force: true`** — both invoke reindex specifically as a
repair, and a repair that trusts the stamp it is trying to repair is not a
repair.

### 2.4 Tests (phase 2)

- `reindex_skips_unchanged_memories` — two runs, second reports
  `embedded: 0, skipped: N`, vectors byte-identical.
- `reindex_reembeds_only_the_changed_memory`.
- `reindex_reembeds_when_model_changes` — same text, different `model_id()` on
  the stub provider.
- `reindex_reembeds_when_composition_flips` — `metadata_vector` toggled.
- `reindex_reembeds_when_chunk_tokens_change` — the axis the store fingerprint
  misses.
- `reindex_reembeds_memory_with_content_match_but_no_vectors` — the
  provider-was-down case; `has_embedding == false` must defeat the content match.
- `reindex_force_reembeds_everything`.
- `interrupted_reindex_resumes_without_redoing_completed_memories` — drive a
  provider that fails after N successes, assert the retry re-embeds exactly the
  remainder. Blind spot 3.
- `reindex_with_dimension_change_ignores_skips_and_recreates_table`.
- **Equivalence test:** on a fixture store, a skip-enabled reindex and a
  `--force` reindex produce identical chunk tables. This is the one that makes
  the optimisation trustworthy.

## Phase 3 (optional) — Skip the metadata rebuild

Opt-in behind `reindex --incremental`, default off, and a legitimate candidate
for never being built. It saves ~36 ms per 1,000 memories and costs the
from-scratch-rebuild guarantee.

If built: `reindex_dir` skips **parse + stem + row write** when
`row.content_sha256 == sha256(file_bytes)` *and* `manifest.schema_version ==
CURRENT_SCHEMA_VERSION` *and* `manifest.normalizer == NORMALIZER_STAMP`. The id
comes from the filename stem via `extract_id_from_stem` /
`stem_matches_id_prefix` (`crates/engram-storage/src/memory_file/mod.rs:124`,
`:138`) — no parse needed, which is the entire point.

Two things it must not break:

- **Enumeration, never skipped.** Phase 4 of `reindex_dir` resolves two files
  claiming one ID by mtime and *deletes* the loser (`store.rs:1966`). A skip
  short-circuits parse, not enumeration; the duplicate check runs off the
  stem-derived id and stays intact.
- **`clear_memories` becomes conditional**, replaced by upsert-changed +
  delete-rows-whose-file-vanished. That mode already exists and is already
  exercised — it is the non-destructive path taken under a foreign checkout
  (`store.rs:1544`).

## 3. `check_staleness` — the tiered check

`MemoryStore::check_staleness` (`store.rs:2017`) runs on every `query`, `list`,
and `get` (`commands/{query,list,get}.rs`). Today it compares a `.md` **file
count** to a row count (`staleness_message`, `store.rs:2307`) and is wrong in
both directions: blind to any in-place edit, and blind to a delete+create pair
that keeps the count equal.

Replace with four tiers over one enumeration:

```text
enumerate .md with metadata()  -> (id_from_stem, len) per file   [1 statx/file]
index.list_digests()           -> (id, len, sha256) per row      [1 narrow scan]

A) count mismatch        -> today's message                       [~free]
B) id-set mismatch       -> "N indexed / M on disk, ids differ"    [~free, NEW]
C) len mismatch          -> "N memories changed since indexing"    [~free, NEW]
D) total bytes <= budget -> hash the len-matched files            [bounded, NEW]
   else                  -> report C and say the deep check was
                            skipped, naming the budget
```

Tier D is what makes it *exact*; tier C is what makes it cheap enough to leave
on. A same-length edit is the only case tier C misses, and tier D catches it
below the budget. Above the budget the message **says so** rather than implying
a clean bill of health — a check that silently narrows its own scope is the
failure mode this repo names explicitly.

New config section (there is no `[index]` today):

```toml
[index]
# "counts" | "size" | "content" — how hard check_staleness works on the hot path.
staleness_check = "content"
# Byte budget for the "content" tier. Above this, fall back to "size" and say so.
staleness_max_bytes = 8_388_608   # 8 MiB ≈ 4,000 memories
```

Costs at the default: one extra `statx` per file, one narrow LanceDB projection
replacing the existing `count()`, and — under 8 MiB — a page-cache-warm read
plus SHA-256 (~1.5 GB/s with SHA-NI, standard on x86-64 and ARM). Single-digit
milliseconds on a typical store, and it degrades by declared rule rather than by
silent truncation.

`ops::doctor` remains the unbudgeted authority, and it already rides the
throttled maintenance pass, so a store above the budget still gets an exact
answer every `[maintenance].interval_secs` (default 6 h) and on demand.

## 4. Costs — this is not free

The four blind spots do close. Four things get worse or riskier, and they should
be weighed rather than waved past.

1. **`reindex` stops being unconditionally from-scratch** (phase 2 for vectors,
   phase 3 for rows). It is the documented repair path. If a chunk row is corrupt
   but the digest matches its stamp, a skip-enabled reindex will not repair it.
   *Mitigation:* `--force` keeps today's semantics; `doctor --fix` and `repair`
   pass it; the equivalence test in §2.4 proves the skip path agrees with the
   forced one on healthy stores.
2. **Derivation drift becomes load-bearing.** The row is derived from bytes
   *through code* — parser, stemmer, column set. If that code changes and the
   bytes do not, the hash matches and we skip, serving rows built by the old
   derivation, silently. The interlock exists (`migrate_schema_if_needed` forces
   a rebuild when `schema_version` or `normalizer` drifts, `store.rs:1464`), but
   the existing convention "bump `CURRENT_SCHEMA_VERSION` when `IndexEntry`
   derivation changes" stops being merely tidy and becomes a correctness
   requirement. Needs a test asserting a stamp change forces a full rebuild
   *with matching digests present*, and a line in `CLAUDE.md`.
3. **A new cross-cutting write invariant.** `content_sha256` at every file write,
   `embed_sha256` at every chunk write and delete — the same class of invariant
   as `has_embedding`, which the R2/R3 plan flagged as its riskiest part.
   *Mitigation:* §1.3 makes it a constructor argument rather than a convention,
   so the compiler catches an omission.
4. **A schema bump rebuilds every existing store on next open.** Established
   mechanism, seconds, vectors preserved — but it is a real event for every user
   on upgrade. Storage cost of the columns is ~136 bytes/row (≈1.4 MB at 10,000
   memories); negligible.

## 5. Order and gating

| Phase | Gate to proceed |
| --- | --- |
| 1 — detection | `doctor` names a hand-edited file; migration backfills; all phase-1 tests green; no existing behaviour changed |
| 3′ — `check_staleness` tiers | Ships with phase 1; it is detection, not optimisation |
| 2 — embed skip | Equivalence test green: skip-enabled and `--force` reindex produce identical chunk tables on a fixture store |
| 3 — metadata skip | Optional. Only if profiling on a real store shows the metadata rebuild actually matters |

Phase 1 is self-contained and worth shipping alone: it changes no behaviour, it
answers the "which items are updated but not indexed" question in full, and it
is what makes phase 2 verifiable.

Both later phases touch snapshot-pinned CLI output, so each needs
`cargo insta test --accept --test-runner nextest` **run twice** — non-determinism
only shows on the second run. Every phase must pass `cargo fmt --all` and
`cargo clippy --workspace --all-targets --all-features -- -D warnings` before it
is considered done.
