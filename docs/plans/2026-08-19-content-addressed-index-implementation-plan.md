# Content-addressed index checksums — implementation plan

> **Status: PLANNED, reviewed.** Companion to
> `2026-08-19-content-addressed-index-checksums.md`, which holds the analysis
> and the rationale. This document is the build order: what changes, in which
> file, in which phase, with the tests and the invariants that gate each one.
>
> Revision 2 folds in a four-lens review (storage/Arrow, concurrency, CLI/test,
> adversarial). §13 is the review log — what was refuted, what was corrected,
> and what it cost. Two of the review's findings are **pre-existing bugs** found
> along the way, unrelated to this feature but on its path (§13.1).
>
> **Revision 3 — four defaults changed by the adversarial pass (§14):**
>
> 1. **Skipping is opt-in.** `reindex` keeps today's full semantics; `reindex
>    --incremental` opts into it. The salt cannot capture ONNX *runtime*
>    identity, and this repo has a documented incident where a bad runtime
>    silently produced garbage vectors under an unchanged model id — after which
>    `reindex` is the repair. A default-skip would refuse to repair it (§14 B2).
> 2. **The content hash is over line-ending-normalized bytes.** Raw bytes make
>    `git core.autocrlf` mark an entire Windows store permanently drifted, and
>    CRLF files are an explicitly supported input (§14 B3).
> 3. **`[index].staleness_check` defaults to `"counts"`** — today's exact cost.
>    `lance_index.rs:574-585` carries a comment saying this precise regression
>    was made and fixed once already (§14 S2).
> 4. **Drift is `Option`, not `Vec`.** "Not checked" must be distinguishable
>    from "none found" at the type level, and must not fold into `healthy`
>    (§14 B1).

## Contents

- [0. What changed since the analysis](#0-what-changed-since-the-analysis)
- [1. Design decisions](#1-design-decisions)
- [2. Phase 1 — detection (schema `0.8.0`)](#2-phase-1--detection-schema-080)
- [3. `check_staleness` — the tiered check](#3-check_staleness--the-tiered-check)
- [4. Phase 2 — skip the re-embed](#4-phase-2--skip-the-re-embed)
- [5. Phase 3 (optional) — skip the metadata rebuild](#5-phase-3-optional--skip-the-metadata-rebuild)
- [6. Concurrency and locking](#6-concurrency-and-locking)
- [7. Scope interactions](#7-scope-interactions)
- [8. Failure-mode matrix](#8-failure-mode-matrix)
- [9. Costs](#9-costs)
- [10. Commit-sized sequencing](#10-commit-sized-sequencing)
- [11. Docs to update](#11-docs-to-update)
- [12. Rollback](#12-rollback)
- [13. Review log](#13-review-log)

## 0. What changed since the analysis

**0.1 The metadata-rebuild skip is demoted to optional.** The analysis phased the
work detection → incremental metadata rebuild → embed skip. Wrong order: the
middle phase buys the least and costs the most. The metadata rebuild is ~36 ms
per 1,000 memories (parse ~20 ms + stems ~16 ms, `benches/parallel_simd.rs`),
while making it incremental means **`reindex` stops being an unconditional
from-scratch rebuild** — and reindex is the documented repair path
(`commands/repair.rs:210`, `commands/doctor.rs:395`).

All four blind spots close with detection plus the embed skip. The metadata skip
closes none of them.

**0.2 `check_staleness` gets a real design, not a deferral** (§3). It is the
message users actually see, and today it is wrong in both directions.

**0.3 `embed_sha256` lives on the chunks table**, not the memories row — for a
different reason than revision 1 gave (§1.2, §13.2).

## 1. Design decisions

### 1.1 Why SHA-256 of file bytes

| Candidate | Verdict |
| --- | --- |
| **mtime** | Rejected. Memory files are committed artifacts; `git checkout`/`clone`/`rsync` rewrite mtime without changing content, and vice versa. |
| **`updated_at` frontmatter** | Rejected. Writer-controlled — a hand edit or git-applied change doesn't touch it — and reading it costs the parse we wanted to skip. It stays a race guard in `upsert_chunks_if_current` (`store.rs:1677`), not a skip predicate. |
| **file size alone** | Rejected as authority, **adopted as discriminator**. Collides on same-length edits, but it is one `statx` and it is what makes §3's hot-path tier affordable. |
| **SHA-256 of file bytes** | Adopted. Exact, cheap on bytes already in memory, survives clones, and matches `harvest::index_text_digest` (`src/ops/harvest.rs:738`). |

`sha2`, hex-encoded, no new dependency. **Crate gotcha:** `sha2` 0.11's
`finalize` returns a `hybrid_array::Array` with no `LowerHex`, so `{:x}` does not
compile. All three existing digest sites document this; the new helper must too.

### 1.2 Why `embed_sha256` lives on the chunks table

Revision 1 argued the memories table was impossible because
`set_has_embedding_batch` (`lance_index.rs:1101`) is a SQL update setting one
column to one *constant* for every matched row. **That premise was refuted:**
lance 8.0 supports partial-schema `merge_insert`, so a 2-column `(id,
embed_sha256)` batch is one commit
(`lance-8.0.0/src/dataset/write/merge_insert.rs:603-628`, `:1510-1527`,
`:1643-1670`). The mechanism exists.

The real reason is the one CLAUDE.md already states for `source_sessions`:
**the memories table is rebuilt from the `.md` files.** Every schema migration
and every `reindex` metadata rebuild calls `clear_memories()` and regenerates
rows from disk (`store.rs:1541`/`:1556`) — and a `.md` file cannot know what
text produced its vectors. An `embed_sha256` on the memories row would be wiped
by the very mechanism that adds columns, exactly as `has_embedding` is, and
would need explicit re-stamping from a chunk-table scan on every rebuild. On the
chunks table it survives by construction, because chunks are never rebuilt —
that is the whole reason `clear_memories` exists as distinct from `clear_chunks`.

Two further properties, both real:

- **Written atomically with the vectors it describes**, riding the existing
  per-memory `RecordBatch`. It cannot describe a different snapshot than the
  vectors beside it — closing the crash-window `has_embedding` still has.
- **Deleted with them.** No separate clearing step.

**Cost the review found, which revision 1 denied.** `vector_search`
(`lance_index.rs:1453`) calls `table.vector_search(query)` with **no
`.select()`**, and lancedb's default is `Select::All`
(`lancedb-0.31.0/src/query.rs:827`). A new chunks column would therefore be
materialised for up to `chunk_limit` (≤65,536) rows on **every semantic query**.
So revision 1's "never read by a query" was wrong.

Fix, in the same commit: narrow the projection to what the loop already reads.
Verified safe — `_distance` is auto-projected independently of the select list
(`disable_scoring_autoprojection`, `query.rs:807`, default `false`):

```rust
table.vector_search(query)?.select(Select::Columns(vec!["memory_id".into()]))
```

This is a hot-path improvement in its own right: the loop only ever reads
`memory_id` and `_distance` (`lance_index.rs:1474-1488`), yet today it
materialises the full FixedSizeList vector for every candidate chunk.

**Schema evolution — under the lock, not at open.** Revision 1 put the
`add_columns` call in `ensure_chunks_table_exists`. Two reviewers independently
flagged this as a blocker: `LanceIndex::new` is called with **no write lock** by
`open` (`store.rs:489`), `open_global` (`:331`) and `open_group` (`:434`) — only
`init*` holds it. `add_columns` commits an `Operation::Merge`, which
`check_merge_txn` makes a retryable conflict against any concurrent
`Append`/`Update`/`Delete`/`Merge`
(`lance-8.0.0/src/io/commit/conflict_resolver.rs:983-1002`). On the first upgrade
after the bump, a concurrent MCP session writing chunks makes the loser's
`LanceIndex::new` return `Err` → `MemoryStore::open` fails → **an ordinary read
command dies**. This codebase already fixed the identical hazard for table
*creation* with the lock (`store.rs:363-368`).

→ Do the evolution inside `migrate_schema_if_needed` (version-gated by the 0.8.0
bump, and its `reindex_with` already takes the lock), keep
`ensure_chunks_table_exists` create-only, and treat "column already exists" as
success. Cheap either way: `AllNulls` is metadata-only — it clones fragment
metadata, writes no data files, and commits once
(`lance-8.0.0/src/dataset/schema_evolution.rs:362-388`, `:411-425`). Indices
survive: `retain_relevant_indices` drops only indices whose field ids left the
schema, and added fields get fresh ids (`:401`).

Read path: `LanceIndex::list_embed_digests()` — one projection scan of
`(memory_id, embed_sha256)`, deduped. Same cost class as the
`list_chunk_memory_ids()` scan `reindex_with` already performs (`store.rs:1568`),
and read only by reindex and `reindex --dry-run`.

### 1.3 What the embed digest covers

```text
embed_sha256 = sha256( model_id ‖ 0 ‖ dimensions ‖ 0 ‖ composition_id
                       ‖ 0 ‖ chunk_tokens ‖ 0 ‖ texts.join("\0") )
```

Four drift axes in one comparison: text changed, model swapped, composition
flipped, `[embeddings].max_tokens` retuned. The first three are also covered
store-wide by `EmbeddingFingerprint`; the fourth is **not** — a `max_tokens`
change re-chunks today with nothing detecting it (`effective_chunk_tokens`,
`engine.rs:1711`). Per-row rather than store-wide is what makes the check
resumable and what lets `--dry-run` name individual memories.

Computed by the **engine**, which owns `embedding_texts` (`engine.rs:1731`) and
the provider identity; passed to storage as an opaque string.

### 1.4 Make the content stamp explicit at every site

`IndexEntry::from(&Memory)` cannot know the file bytes. That is the shape of the
`has_embedding` invariant, hand-patched at five call sites, which the R2/R3 plan
called its riskiest part.

Replace the `From<&Memory>` impl with two explicit constructors, so no call site
gets a silent default:

```rust
impl IndexEntry {
    /// The production constructor: the row records the identity of the file
    /// it was derived from.
    pub fn for_file(memory: &Memory, digest: &FileDigest) -> Self;

    /// Digest-less. For benchmarks and fixtures that have no file behind the
    /// memory. Production code must use `for_file` — a `None` digest disables
    /// drift detection for that row.
    #[doc(hidden)]
    pub fn without_digest(memory: &Memory) -> Self;
}
```

`without_digest` exists because `benches/parallel_simd.rs:483,498` calls
`IndexEntry::from` and `cargo clippy --workspace --all-targets` builds benches —
a `#[cfg(test)]` helper would not cover it. That bench is also the source of the
~36 ms/1,000 figure §0.1 rests on, so it should hash too, to keep measuring the
real phase-5 cost.

Five production call sites — **two of which are not the one-liners revision 1
implied**:

| Site | Source of bytes |
| --- | --- |
| `store.rs:566` `create` | `content` from `write_memory_file` (`:516`) — direct |
| `store.rs:675` `create_batch` | **`content` is scoped to the first loop and dropped at `:656`**; the `IndexEntry` loop at `:672-679` is a separate iteration. Accumulate a `HashMap<id, FileDigest>` in the first loop. |
| `store.rs:976` `update_many` | returned from `write_updated_file_locked` |
| `store.rs:1092` `sync_index_row_locked` | returned from `write_updated_locked` |
| `store.rs:1997` `reindex_dir` phase 5 | **not "the bytes read in phase 2"** — phase 3 discards `content` (`:1935-1946`) and phase 4's `by_id` never sees it. Hash **inside phase 3's `into_par_iter`**, where the bytes are already hot and rayon parallelises it for free, and carry the 72-byte `FileDigest` through the phase-4 dedup fold — which *deletes the loser file*, so the winner's digest must follow the winner. |

`write_updated_locked` / `write_updated_file_locked` change to
`Result<FileDigest>`. `atomic_write` writes `content` verbatim
(`store.rs:2180-2204`, `write_all(content.as_bytes())` then `persist`) — no
newline or encoding transformation — so `FileDigest::of(content.as_bytes())` is
the on-disk identity, with no re-read anywhere on the write path.

## 2. Phase 1 — detection (schema `0.8.0`)

Pure detection. No existing behaviour changes; reindex still rebuilds
everything. This is what makes phase 2 *verifiable*.

### 2.1 The digest helper

New `crates/engram-storage/src/digest.rs`, re-exported as
`engramdb::storage::FileDigest`:

```rust
/// A memory file's content identity: the SHA-256 of its exact bytes on disk,
/// plus their length.
///
/// `len` is not redundant with `sha256`. It is the cheap discriminator the
/// hot-path staleness check uses (one `statx` per file, no reads), with
/// `sha256` as the authority for the opt-in deep tier.
#[derive(Debug, Clone, PartialEq, Eq)]
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

### 2.2 Columns and schema bump

`crates/engram-storage/src/lance_index.rs`:

- `memories_schema()` (`:379`) — `content_sha256: Utf8` nullable,
  `content_len: UInt64` nullable. **UInt64, not Int64**: every unsigned count in
  this codebase already uses an unsigned Arrow type (`chunk_index: UInt32`,
  `user_turns`/`indexed_chars: UInt32`); there is no `Int64` column anywhere, and
  introducing one would add the first `as i64`/`as u64` cast pair in the Arrow
  layer.
- `chunks_schema()` (`:430`) — `embed_sha256: Utf8` nullable.
- `IndexEntry` (`:77`) — matching `Option` fields. Its doc comment says "all 23
  columns" and the struct is already 26; fix it to 28 while there.
- `entries_to_batch` (`:2004`) — two more column builders. **Non-optional and
  easy to forget:** `RecordBatch::try_new` validates array count against the
  schema at *runtime*, so a missed builder fails every `create`, not the build.
- Both chunk builders — `upsert_chunks` (`:969-977`) and `upsert_chunks_batch`
  (`:1072-1080`) — construct against `chunks_schema()` independently. Route them
  through **one shared batch-builder helper** with
  `debug_assert_eq!(batch.num_columns(), schema.fields().len())`. A miss here is
  silent: `embed_memory_with` errors, `spawn_ingest` only `tracing::warn!`s
  (`engine.rs:594-601`), `create` reports success, and the memory is never
  embedded and never appears in semantic search.
- New narrow projection, mirroring `SourceSessionLink` (`:149`). **Do not widen
  `FILTERING_COLUMNS` (`:1785`)** — every memories read names its columns, so
  added columns cost queries nothing there:

```rust
/// One memory's index-currency digest (schema v0.8.0).
pub struct IndexDigest {
    pub memory_id: String,
    pub content_sha256: Option<String>,
    pub content_len: Option<u64>,
    pub has_embedding: bool,
}
```

`manifest.rs:41` — `CURRENT_SCHEMA_VERSION` → `0.8.0`, with the chunks-column
evolution run from `migrate_schema_if_needed` under the lock (§1.2).

Verified safe: no memories-table read happens before `migrate_schema_if_needed`.
`MemoryStore::open` is `write_state_gitignore` → config load → `LanceIndex::new`
(create-if-missing, open AS-IS) → `migrate_schema_if_needed` (`store.rs:502`);
`init`/`init_global`/`open_global`/`open_group` are identical. The R2/R3 hazard
is real and already has a regression test proving a premature projection errors
(`store.rs:3138-3148`).

### 2.3 Where drift is reported — doctor vs. dry-run

Revision 1 put both new lists on `DoctorResult`. Split them, because only one is
computable there:

**`doctor` gets `drifted_entries`** (content hash mismatch). Needs no engine —
just the files and the rows.

**`reindex --dry-run` gets stale-vector reporting.** Computing an embed digest
needs `embedding_texts` + `effective_chunk_tokens`, which are engine-only — and
**no doctor path has an engine**: `run_environment_check` builds none
(`commands/doctor.rs:104-125`), and `doctor_environment` has no engine parameter
(`src/ops/doctor.rs:194-199`). Adding one would touch four call sites and 11
in-crate tests to serve a command that already exists and already has an engine.

**"No new plumbing" was false — four consumers hand-enumerate the lists** and
would report drift as a findings-free failure. All must be updated in the same
commit:

- `commands/doctor.rs:69-96` — pretty branch prints nothing, then
  `bail!("store is unhealthy ({} stale, {} orphaned)")` with both counts zero.
- `commands/doctor.rs:57-67` — JSON branch is a literal `json!{}` of five keys.
- `src/ops/maintenance.rs:246-253` — the auto-maintenance warning, same shape.
- `src/ops/doctor.rs:287-290` — `EnvironmentCheck "Store health"` details are
  hardcoded to `indexed`/`on disk`.
- `crates/engram-mcp/src/server.rs:2927-2975` — `memory_doctor` builds
  `json!({healthy, indexed, on_disk})` field by field. **Agents reach EngramDB
  primarily through MCP, so this is the main consumer of drift detection**, and
  revision 1 didn't mention it.

`stale_vectors` must **exclude `has_embedding == false` rows**. `create` returns
before its detached ingest embeds (`src/ops/create.rs:308`), so during a cold
ONNX load every just-created memory would otherwise report as stale-vectored —
an MCP agent calling `create` then `doctor` would see a false unhealthy every
time. "Not embedded yet" is a different state and is already reported separately.

### 2.4 `reindex --dry-run`

**Not `--check`.** This repo's dry-run flag is `--dry-run`, four times
(`projects discover`, `migrate`, `rollback`, `setup`), with snapshots encoding
the name (`cli__snapshot__admin__migrate_dry_run.snap`). There is no `--check`
anywhere in `app.rs`.

`run_reindex` (`commands/reindex.rs:22`) carries 10 parameters under
`#[allow(clippy::too_many_arguments)]`. Introduce **`ReindexParams`** — the
repo's naming is `<Cmd>Params` (`AddParams`, `QueryParams`, `DiscoverParams`,
`UpdateParams`), not `Options` — bringing `run_reindex` to 6 args so the allow
can go. **Five** call sites, not four: `lib.rs:645`, `commands/doctor.rs:399`,
and three tests (`reindex.rs:234`, `:270`, `:293`).

Output must go through `outln!`/`errln!`/`print_*` — the `formatter-output` CI
job fails the build on a bare print macro in `crates/engram-cli/src` — and must
respect the one-document rule in JSON mode, gating on
`formatter.wants_human_stdout()` **not** `!is_json()`. `run_reindex` already
models this at `:62` with a comment explaining that `doctor --fix` passes a
non-JSON delegate formatter; an unguarded `outln!` would corrupt
`doctor --fix --format json`, which reaches `run_reindex` via
`FixAction::Reindex`.

`ReindexInput` on MCP (`server.rs:493-504`) gains the same knobs, and the
response `json!` at `:2841-2846` gains the counts. Without `force` on MCP an
agent has no way to run a repair-grade reindex. No MCP tool-schema snapshot
exists, so this costs no snapshot churn.

### 2.5 `stats`

`engramdb stats`'s "Health Warnings" block (`commands/stats.rs:105-119`) is where
a user asks "what should I run next", and it already names the command to run.
Add "N memories drifted since indexing" there. Churns
`cli__snapshot__admin__stats_*`.

### 2.6 Tests (phase 1)

Storage, `crates/engram-storage/src/store.rs::tests`:

- `content_digest_stamped_on_create_matches_file_bytes`
- `content_digest_stamped_on_update_and_batch_update`
- `content_digest_stamped_on_create_batch` — the `HashMap` threading of §1.4.
- `content_digest_survives_title_rename` — filename changes, old file removed;
  the row must carry the *new* file's digest.
- `content_digest_follows_winner_of_duplicate_id_resolution` — phase 4 deletes
  the loser; the digest must be the winner's.
- `reindex_backfills_content_digest_from_disk`
- `schema_migration_on_open_backfills_content_digest` — build a **real**
  pre-0.8.0 table by stripping the columns from the live Arrow schema, as
  `downgrade_to_0_6_0` (`store.rs:3055-3098`) does.
- `chunks_column_evolution_is_idempotent_and_locked`
- `hand_edited_file_is_reported_as_drifted` — **blind spot 1; red before the
  change.**
- `delete_then_create_keeping_count_equal_is_detected`
- `same_length_edit_missed_by_size_tier_caught_by_content_tier` — pins the
  boundary between §3's tiers.
- `vector_search_projection_excludes_new_chunk_column` — guards §1.2's hot-path
  fix from regressing.

Engine, `src/retrieval/engine.rs::tests`:

- `embed_digest_changes_with_text_model_composition_and_chunk_tokens`
- `embed_digest_stable_across_identical_inputs`

CLI — **tier 2, not tier 1.** `DoctorResult` is not rendered by any tier-1
`print_*` renderer: `output.rs` has only `print_environment_doctor` (`:470`),
which iterates `result.sections` and never reads `store_check`, and all three
tier-1 fixtures set it to `None`. Files to regenerate:
`cli__snapshot__env__doctor_store__{json,plain,pretty}.snap`,
`cli__snapshot__env__doctor_environment__*`, `cli__snapshot__help__help_reindex.snap`,
`cli__snapshot__admin__reindex_full.snap`, `reindex_index_only.snap`,
`cli__snapshot__admin__stats_*`, and a new `reindex_dry_run`.

## 3. `check_staleness` — the tiered check

Four tiers over one enumeration:

```text
enumerate .md with metadata()  -> (id_from_stem, len) per file   [1 statx/file]
index.list_digests()           -> (id, len, sha256) per row      [1 projection]

A) count mismatch        -> today's message
B) id-set mismatch       -> "N indexed / M on disk, ids differ"   [NEW]
C) len mismatch          -> "N memories changed since indexing"   [NEW]
D) (opt-in) hash the len-matched files                            [NEW]
```

**Default is `"size"` (tiers A–C), not `"content"`.** Revision 1 defaulted to
exact hashing on the premise that it replaced a comparable scan. It doesn't:
`LanceIndex::count` (`lance_index.rs:579`) is `Table::count_rows(None)` →
per-fragment **metadata only**, reading no data files
(`lance-8.0.0/src/dataset.rs:1479-1486`). Tiers A–C already close the practical
blind spot (any edit that changes length — git checkout, hand edit, restore) for
one `statx` per file plus a narrow projection. Tier D reads and hashes every
file on every `query`/`list`/`get`, which is a latency regression the user should
opt into. `doctor` and `reindex --dry-run` remain the unbudgeted authority.

**The message must declare its tier.** A `"size"`-tier clean result is not proof
of no drift, and must not read as one.

**Read barrier (torn-read fix).** `check_staleness` is lock-free on read paths,
and `write_lock.rs` offers no try-acquire or shared mode. A `create`/`update`
committing between the stat pass and the digest read yields a spurious
"N memories changed" on a healthy store. Tier B has its own version: `create`
writes the file (`store.rs:517`) and the row (`:571`) with up to two dirent scans
between. → Read `list_digests()` **after** the enumerate pass, then re-read only
the mismatching rows once and drop any that now agree. The mismatch set is
normally empty, so this costs nothing on a healthy store and turns a torn read
into a no-op instead of a warning.

**Signature change (blocker in revision 1).** `MemoryStore` holds no config —
it is `{ project_dir, project_id, lance_index }` (`store.rs:106-113`) — and
`check_staleness(&self)` has three callers that load none (`list.rs:44`,
`query.rs:58`, `get.rs:36`). It becomes
`check_staleness(&self, cfg: &IndexConfig)` with the three call sites loading
config. This is step 7's largest actual change and was invisible in revision 1.

**Config.** New `[index]` section in `crates/engram-types/src/config.rs`:

```toml
[index]
staleness_check = "size"       # "counts" | "size" | "content"
staleness_max_bytes = 8388608  # budget for the "content" tier
```

It must join `EngramConfig::validate()` (`config.rs:1843-1858`) — a
`staleness_max_bytes` of `0` silently disables tier D forever — and needs
`Default`, `#[serde(rename_all = "snake_case")]` on the enum, and
`#[serde(default)]` on the `EngramConfig` field, matching every sibling.

It does **not** join `provider_cache_key`: that function exhaustively
destructures only `EmbeddingsConfig`/`NliConfig`/`RerankConfig`
(`src/ops/mod.rs:757+`), and `[index]` loads no model — the same rationale
already written down for `HarvestConfig`. Adding a top-level section will not
break the destructure. Worth one sentence so a reviewer doesn't flag it.

`engramdb config` prints a curated subset, so `[index]` appears only if added to
the renderer — otherwise `staleness_check` is a behaviour knob with no way to see
its effective value. Add it, and regenerate
`cli__snapshot__admin__config_effective__*`.

**MCP has no staleness signal at all.** `check_staleness` is CLI-only; no MCP
tool calls it. Either surface it on the MCP `query`/`list` responses or state
plainly that agents get drift only via `memory_doctor`.

## 4. Phase 2 — skip the re-embed

### 4.1 The predicate

```text
skip_embed(memory) =
     !force
  && embed_digests[memory.id] == Some(embed_digest(memory, fingerprint, chunk_tokens))
```

Revision 1 also required `row.has_embedding`. Dropped: the presence of an
`embed_digests` entry already covers the provider-was-down case and is *atomic
with the vectors*, whereas `has_embedding` is a separate commit and is currently
unreliable in exactly that direction (§13.1).

### 4.2 The `clear_chunks` restructuring

- **Skip-enabled** — no `clear_chunks`. `upsert_chunks_batch` already replaces a
  memory's chunks atomically and drops surplus ones; the orphan prune already
  exists (`store.rs:1607`).
- **`--force`** — unchanged, today's behaviour exactly.
- **Dimension change** — `chunks_table_dimensions()` disagreeing with the live
  provider must recreate the table, so that run skips nothing. Detection already
  written (`reindex.rs:109-124`); promote it to both branches.
- **Foreign checkout** — unchanged; orphan prune stays skipped.

### 4.3 Reporting and the `force` fan-out

`ReindexResult` gains `skipped: usize` — a skip that is not counted is a silent
loss path.

`ops::reindex` has **six** production callers, not the two revision 1 named:
`commands/reindex.rs:71`, `repair.rs:210`, `discover.rs:326`, `server.rs:1606`
(the `reindex_on_model_change = auto` startup path), `server.rs:2838`
(`memory_reindex`), plus the CLI's `doctor --fix` route. All must compile, and
the plan must state each one's `force` value. **`doctor --fix` and `repair` pass
`force: true`** — both invoke reindex *as a repair*, and a repair that trusts the
stamp it is repairing is not a repair.

### 4.4 Tests (phase 2)

- `reindex_skips_unchanged_memories`
- `reindex_reembeds_only_the_changed_memory`
- `reindex_reembeds_when_model_changes`
- `reindex_reembeds_when_composition_flips`
- `reindex_reembeds_when_chunk_tokens_change`
- `reindex_reembeds_memory_with_no_vectors`
- `reindex_force_reembeds_everything`
- `reindex_with_dimension_change_ignores_skips_and_recreates_table`
- `reindex_skip_matches_force_result` — **the gate.**
- `reindex_resumes_after_provider_failure` — **renamed.** True crash-resumability
  is not testable today and is not delivered: `ops::reindex` makes one
  `embed_memories` call which accumulates every vector and performs a single
  `upsert_chunks_batch` at the end (`engine.rs:499`), so a crash before it writes
  nothing. What *is* delivered is per-provider-failure resumability. Making a
  crash mid-reindex resumable requires chunking the embed loop and moving
  `set_has_embedding_batch` inside the per-500 loop — a separate change, listed
  as optional in §10.

## 5. Phase 3 (optional) — skip the metadata rebuild

Opt-in behind `reindex --incremental`, default off, and a legitimate candidate
for never being built.

```text
skip_row(file) =
     manifest.schema_version == CURRENT_SCHEMA_VERSION
  && manifest.normalizer     == NORMALIZER_STAMP
  && row.content_sha256      == Some(sha256(file_bytes))
```

Two things it must not break: **enumeration is never skipped** (phase 4 resolves
duplicate IDs by mtime and deletes the loser, `store.rs:1966`), and
`clear_memories` becomes conditional, replaced by the upsert-only mode that
already exists for foreign checkouts (`store.rs:1544`).

## 6. Concurrency and locking

**6.1 The metadata rebuild is fully locked.** Verified: `reindex_with`'s `_lock`
(`store.rs:1522`) is a plain binding with no early drop, covering both
`reindex_dir` calls (`:1581`, `:1593`), the orphan prune and
`update_manifest_stats`, returning at `:1627`. No await point releases it.

**Scope the guarantee, though.** The flock only excludes other EngramDB
processes. A `git checkout` or hand edit landing between phase 3's hash and
phase 5's upsert still stamps a digest for bytes no longer on disk —
self-correcting (the next `doctor` reports drift) but true for precisely the
class of writer this feature exists to detect. Say so in the doc comment.

**6.2 Reindex releases the lock before embedding — accounting only.** A
concurrent `update` in that window is caught by `upsert_chunks_batch`'s re-read
under the lock (`store.rs:1733-1741`), which drops the stale entry. Only the
reported counts shift, honestly.

**6.3 The skip decision is read outside the lock.** Resolved by 6.2's re-read:
worst case we embed something that did not need it. Fails toward extra work.

**6.4 Detached ingest keeps snapshot semantics.** Verified: chunk texts are
computed from the snapshot (`engine.rs:1825`), embedded (`:1851`), and the digest
over those same chunks rides the same `upsert_chunks_if_current` call (`:1861`),
whose guard re-reads under the lock (`store.rs:1683-1697`).

**Scope this one too.** The guard is `updated_at` *equality*, and
`MemoryStore::create` does not bump `updated_at` — `worktree.rs:104-111`
deliberately re-creates on equal timestamps. So a re-create with equal
`updated_at` and different content passes the guard. Pre-existing and documented
(`store.rs:1667-1676`); the digest makes it **self-healing on the next reindex
instead of permanent**, which is strictly better than today. State the guarantee
as "never a different `updated_at`", not "never a different version".

**Invariant:** a chunk row's `embed_sha256` always describes the vectors in that
same row. Bought by §1.2's table choice — but note this buys **atomicity, not
presence**: the shared batch builder and its `debug_assert` are what buy presence
(§2.2), because `RecordBatch::try_new` validates at runtime, not compile time.

## 7. Scope interactions

- **Worktree consolidation — a blocker revision 1 missed.** `worktree.rs:129`
  exports vectors from the worktree store and `:179` writes them into the main
  store. Those vectors were never embedded by this process, and `engram-storage`
  cannot compute a digest (no provider, no `chunk_tokens`). Nor can the digest
  simply be copied: the worktree store loads its **own** `config.toml`
  (`store.rs:472-483`), so `dimensions`/`max_tokens`/`metadata_vector` may
  differ. → `export_chunks_batch` also returns `embed_sha256`, copied only when
  both stores' embedding identity matches; NULL otherwise. Papering over with
  NULL unconditionally would re-embed every consolidated memory on the next
  reindex, quietly undoing the promise at `worktree.rs:188-190`.
- **`migrate` and `rollback` rewrite every file's bytes and never touch the
  index.** `migrate.rs:205` / `rollback.rs:231` `atomic_write` a re-serialized
  memory over every file under the project lock, with no index upsert and no
  reindex. From the instant either runs, **every** memory's `content_sha256` is
  stale: `doctor` reports N drifted and `healthy = false`, and `check_staleness`
  fires on every subsequent read. → Run `store.reindex()` at the end of both
  (releasing the lock first), or at minimum print "run `engramdb reindex`".
  Semantically this is a no-op drift — the parsed `Memory` is unchanged, so
  `embed_sha256` still matches and vectors are correctly kept (§8).
- **Foreign checkout.** `staleness_message` already suppresses under a conflict
  (`store.rs:2312`); **the new tiers must inherit that suppression, not just tier
  A**, and `doctor`'s drift list must be scoped the same way or every one of the
  other clone's rows reports as drifted.
- **Global / group stores** use the same code path. **Personal memories** live
  under `<global_data_dir>/projects/<id>/personal/` and must be covered by every
  tier, or a personal-memory edit is invisible.

## 8. Failure-mode matrix

Every column is `Option`; NULL means *unknown* and falls through to "rebuild".

| Situation | `content_sha256` | `embed_sha256` | Outcome |
| --- | --- | --- | --- |
| Pre-0.8.0 store | backfilled by migration | NULL | Full re-embed once. Correct — provenance genuinely unknown. |
| Provider down at last reindex | stamped | absent | Re-embeds. |
| **Ingest still in flight** (`create` returned, embed running) | stamped | absent | **Not reported as stale-vectored** (§2.3) — `has_embedding == false` is a different, separately reported state. |
| Interrupted reindex (provider failure) | stamped | stamped for completed only | Retry re-embeds the remainder. |
| Hand-edited `.md` | mismatch | (stale) | `doctor` names it; reindex rebuilds row + vectors. |
| Frontmatter-only edit | mismatch | match | Row rebuilt, embed skipped — the win the separate digests buy. |
| **`migrate` / `rollback` ran** | mismatch (all rows) | match | Row rebuild only; vectors correctly kept. Semantically a no-op drift. |
| `metadata_vector` flipped | match | mismatch | Everything re-embedded. |
| `max_tokens` retuned | match | mismatch | Re-embeds. The axis the store fingerprint misses. |
| Dimension change | match | mismatch | Forced full path; nothing skipped. |
| Worktree-consolidated, identical config | match | copied | Skipped — vectors correctly reused. |
| Worktree-consolidated, differing config | match | NULL | Re-embedded. |
| Corrupt chunk row, digest intact | match | match | **Skipped.** Accepted; `--force` is the repair. |
| Parser/stemmer changed, bytes same | match | match | Stamp mismatch forces a full rebuild (§9.2). |

## 9. Costs

1. **`reindex` stops being unconditionally from-scratch.** Mitigations:
   `--force`; `doctor --fix` and `repair` pass it; the equivalence test in §4.4.
   Residual is row 13 of §8, accepted knowingly.
2. **Derivation drift becomes load-bearing.** The convention "bump
   `CURRENT_SCHEMA_VERSION` when `IndexEntry` derivation changes" becomes a
   correctness requirement. Needs a test asserting a stamp change forces a full
   rebuild *with matching digests present*, and a line in `CLAUDE.md`.
3. **A new cross-cutting write invariant.** The content stamp is
   constructor-enforced (§1.4); the embed stamp is atomic-by-construction but its
   *presence* rests on a runtime assert, not the compiler (§6).
4. **A schema bump rebuilds every store on next open**, plus a one-time
   chunks-column evolution. Storage ~136 bytes/row (≈1.4 MB at 10,000 memories).
5. **`check_staleness` gets more expensive**: a narrow projection replaces a
   metadata-only row count, plus one `statx` per file. `"counts"` restores
   today's exact behaviour.

## 10. Commit-sized sequencing

Each step compiles and passes `cargo fmt --all` and
`cargo clippy --workspace --all-targets --all-features -- -D warnings`.

| # | Commit | Notes |
| --- | --- | --- |
| 0a | **Fix `delete_chunks_batch` to reset `has_embedding`** | Pre-existing bug (§13.1) |
| 0b | **Narrow `vector_search`'s projection to `memory_id`** | Pre-existing hot-path cost; prerequisite for step 4 |
| 1 | `digest.rs` + `FileDigest` + unit tests | Leaf |
| 2 | Memories columns + `IndexEntry` fields + **`entries_to_batch` builders** + `IndexDigest` + schema bump + **`for_file`/`without_digest` threaded through all 5 sites and the bench** | **Merged.** Revision 1 split these, but nothing writes a digest until the constructors land, so the migration test cannot pass at the old step 2. |
| 3 | Chunks column + shared batch builder + locked evolution in `migrate_schema_if_needed` + `list_embed_digests` + `embed_digest()` in the engine, threaded through both chunk write paths | **Merged.** Revision 1 split schema from value; between them `RecordBatch::try_new` gets a 4-field schema and 3 arrays → every chunk write fails at runtime. |
| 4 | Worktree consolidation digest copy (§7) | Would not compile after step 3 otherwise |
| 5 | `doctor` `drifted_entries` + all five hand-enumerating consumers | Tier-2 snapshots |
| 6 | `check_staleness` tiers + `[index]` config + `validate()` + `config` renderer | §3 |
| 7 | `ReindexParams` + `reindex --dry-run` (incl. stale-vector reporting) + `stats` health warning | CLI snapshots |
| 8 | `migrate`/`rollback` reindex-or-warn (§7) | |
| — | **Phase 1 complete — ship/review gate** | |
| 9 | Skip predicate + conditional `clear_chunks` + `skipped` reporting | |
| 10 | `--force` across all six `ops::reindex` callers + MCP `ReindexInput` | |
| 11 | Equivalence test + provider-failure resumability test | Phase-2 gate |
| 12 | *(optional)* Chunk the reindex embed loop for crash-resumability | §4.4 |

Steps 5–7 touch snapshot-pinned output: `cargo insta test --accept
--test-runner nextest`, **run twice**.

## 11. Docs to update

- **`.claude/CLAUDE.md`** — the schema-migration paragraph (0.8.0), and the
  derivation-stamp rule now that it is load-bearing (§9.2).
- **`docs/contributors/architecture.md`** — index currency as a concept.
- **`docs/users/configuration.md`** — the hand-maintained section list; `[index]`
  goes there.
- **`docs/users/`** — `reindex --dry-run`, `--force`.

## 12. Rollback

**Phase 2** — revert the predicate; `clear_chunks` returns to unconditional. The
columns stay and remain correct.

**Phase 1 — not as clean as revision 1 claimed.** After reverting,
`CURRENT_SCHEMA_VERSION` returns to `0.7.0`, the store's stamp reads `0.8.0` =
"at or ahead", so `migrate_schema_if_needed` leaves the 28-column table alone
(`store.rs:1455-1465`). The old binary's `entries_to_batch` then produces a
26-column source — a **valid subset schema**, both new columns nullable — so
`merge_insert` accepts it and copies the *target's* existing values for the
missing columns. Result: the upsert succeeds and **silently preserves stale
`content_sha256`/`content_len` on every updated row**. Re-upgrading finds
`schema_version == 0.8.0`, runs no migration, and in phase 2 trusts those stale
digests to skip re-embeds.

→ The columns are **preserved and wrong**, not ignored. A rollback must be
followed by `reindex --force` on re-upgrade, or the re-upgrade must use a fresh
schema version (`0.8.1`) so the migration re-runs.

## 13. Review log

### 13.1 Pre-existing bugs found on the way

Neither is caused by this feature; both are on its path and are fixed first
(§10 steps 0a/0b).

1. **`delete_chunks_batch` does not reset `has_embedding`.** `delete_chunks`
   (`lance_index.rs:1257`) calls `set_has_embedding(id, false)` at `:1266`;
   `delete_chunks_batch` (`:1272-1291`) does not — despite `upsert_chunks_batch`
   routing empty-chunk entries into it at `:1040-1045` with a doc comment
   claiming it mirrors `upsert_chunks`. A memory whose `embedding_texts` becomes
   empty via the batched path ends with zero chunk rows and
   `has_embedding == true`, and stays in `has_embedding`-gated semantic ranking.
2. **`vector_search` materialises every column of every candidate chunk.** No
   `.select()`, and lancedb defaults to `Select::All`. The loop reads only
   `memory_id` and `_distance`, yet the full FixedSizeList vector is decoded for
   up to 65,536 rows per semantic query.

### 13.2 What the review refuted

- **The stated reason `embed_sha256` can't live on the memories table.** lance
  8.0 does support partial-schema `merge_insert`. The decision stands on the
  rebuild-from-`.md` argument instead (§1.2). An accepted design resting on a
  wrong premise is a trap for whoever revisits it.
- **"Never read by a query"** — false for the chunks table (§1.2).
- **"No new plumbing" for `doctor`** — five consumers hand-enumerate (§2.3).
- **"The environment doctor path has an engine"** — it does not (§2.3).
- **Rollback is clean** — it is not (§12).
- **`count()` is comparable to a projection scan** — it is metadata-only (§3).
- **Tier-1 renderer snapshots for doctor** — `DoctorResult` reaches no tier-1
  renderer (§2.6).
- **"Bytes read in phase 2"** — phase 3 discards them (§1.4).
- **Two test-only `IndexEntry::from` sites** — there are also two in a bench that
  the CI clippy gate builds (§1.4).
- **The resumability test** — not achievable against a single terminal
  `upsert_chunks_batch` (§4.4).

### 13.3 Blockers folded in

`add_columns` at open (unlocked, fails read commands) → moved under the migration
lock. `check_staleness` cannot read config → signature change. Steps 2/3 and 4/5
not independently green → merged. Worktree consolidation has no digest to write →
§7. `--check` → `--dry-run`. MCP `memory_doctor` drops new fields → §2.3.

## 14. Review log — adversarial pass (revision 3)

### B1. `stale_vectors` could never be populated

No doctor path has an engine: `doctor_environment` (`src/ops/doctor.rs:194`)
takes none, the environment path calls the bare `doctor(s)` (`:274`), and
`maintenance.rs:244` does too. Worse, an always-empty list folding into
`healthy` means doctor reports a clean bill of health on a check it never ran.

Compounding it: `expected_embedding_fingerprint` (`src/ops/mod.rs:296-322`)
deliberately records the **ONNX** identity even when the backend would fall back
to Ollama at load. Doctor computing the expected digest from config while
`ops::reindex` computes it from the live provider gives an Ollama-fallback
machine "doctor: every memory stale" and "reindex: nothing to do" — two surfaces
contradicting each other permanently.

**Resolution.** Vector-staleness reporting lives on `reindex --dry-run` (§2.3),
which has a live engine. Both drift fields become
`Option<Vec<String>>` — `None` = not checked — and `None` never affects
`healthy`. The maintenance warning string (`maintenance.rs:247-252`) must render
the distinction rather than printing "0 stale, 0 orphaned — run reindex".

### B2. The skip would refuse to repair the one vector-corruption incident this repo has had — **flips the default**

`OnnxProvider::model_id()` is `format!("onnx/{}", spec.name)`
(`crates/engram-models/src/embeddings/onnx.rs:514`). It does **not** include the
execution provider (`onnx.rs:362-364`), `intra_threads` (`:359-361`), or **which
`libonnxruntime` was `dlopen`ed**.

That last is a documented, measured incident in `.claude/CLAUDE.md`: the pyke
prebuilt "executes quantized models incorrectly on AVX-512/AMX hosts … the same
text embeds to unrelated vectors (measured: 44/60 distinct embeddings of one
string; cosine below 0). It is the build, not the version or the model."

Scenario: a user embeds under a bad runtime, gets nonsense search results,
installs the correct runtime, and runs `engramdb reindex` — the documented
repair. Today that fixes it. With skipping on by default, `model_id`,
`dimensions`, `composition_id`, `chunk_tokens` and `texts` are all unchanged, so
**every memory is skipped, `embedded: 0`, exit 0**, and the store stays broken
forever with no surface reporting it. §8's "corrupt chunk row" row does not cover
this: it is not row corruption, it is the runtime changing under a stable model
id.

**Resolution — two parts.**

1. **Skipping becomes opt-in: `reindex --incremental`.** A bare `reindex` keeps
   today's full semantics, so the repair path is untouched by construction. This
   restores the conservative order the *analysis* document already recommended
   in its §6 open decision 2, which revision 1 silently flipped. `--force` is
   then unnecessary as a separate flag; `doctor --fix` and `repair` simply never
   pass `--incremental`.
2. **Add runtime identity to the salt** where it is cheaply available:
   `engram_onnx::Backend` plus the resolved dylib path/version that
   `engram_onnx::runtime` already probes (`engramdb doctor` reports it, so the
   value is in hand). This narrows the hole but cannot close it — a same-path
   runtime rebuilt in place is still invisible — which is why part 1 carries the
   safety and part 2 is only a refinement.

### B3. `core.autocrlf` would mark an entire Windows store permanently drifted

`memory_file/helpers.rs:161-190` carries a `crlf_file` path and
`memory_file/tests.rs:475-501` (`test_v2_parse_is_crlf_tolerant`) documents CRLF
as a supported input — "`git core.autocrlf` rewrites the on-disk file — must
parse identically". `atomic_write` writes LF verbatim, so the stamp records LF;
a later `git checkout` on Windows re-materialises the file as CRLF. Bytes
differ, `Memory` is identical, `IndexEntry` is identical.

Every memory then lands in `drifted_entries`, `healthy = false`, maintenance
nags every 6 h, and no repair sticks: reindex restamps CRLF, the next
store-written file is LF, the next checkout flips it back. The repo ships no
`.gitattributes` and `init` writes none.

**Resolution.** Hash a **line-ending-normalized** byte stream (`\r\n` → `\n`
before hashing; derive `content_len` from the normalized form too). This is not
the rejected "hash a re-serialization" — it is a byte transform the parser
already treats as identity, so it preserves the property that motivated the
raw-bytes rule (hand-edited and older-binary files still hash to what they are).
Test: `crlf_file_is_not_reported_as_drifted`, next to the existing CRLF test.
Separately, recommend a `.gitattributes` for `.engramdb/memories/*.md`.

### S2. `check_staleness` default drops to `"counts"`

`lance_index.rs:574-585` documents `count_rows` as deliberately
`O(table metadata)` *because* "this backs `check_staleness`, which runs on every
CLI `list`/`query`/`get`, so a full column scan here scaled every command with
store size." That regression was already made and fixed once.

**Resolution.** Default `"counts"` — today's exact cost and behaviour. `"size"`
and `"content"` are opt-in. Add a bench to `benches/` measuring the tiered check
against store size, and only then consider flipping the default to `"size"`.
State plainly in the docs that the hot path cannot be made exact for free and
that `doctor` / `reindex --dry-run` are the exact answer.

### S3. A richer check has more ways to silently say nothing

All three call sites are `if let Ok(Some(warning)) = store.check_staleness()` —
an `Err` prints nothing. Today the only failure is a `count_rows` error; the
tiered check adds `read_dir`, per-file `metadata()`, per-file read, hash, and
projection-scan failures, each of which would silence the warning and let the
user conclude the index is current.

**Resolution.** `check_staleness` never returns `Err` for a *tier* failure — it
degrades to the next-cheaper tier and reports which tier answered and why.
`Err` is reserved for "cannot answer at all", and the three call sites render it.

### S4. Foreign checkout: the scoping in §7 was the wrong scoping

Scoping to "rows with a local file" does nothing, because memory files are
committed artifacts that travel with a clone — §1.1 says so itself. The real
case is two checkouts of one repo on **different branches**, where one memory id
resolves to different bytes in each. Each clone then reports the other's rows as
drifted, permanently, and with skipping enabled `doctor --fix` re-embeds them in
a loop that alternates which branch's text the shared vectors describe.

Note also that `ops::doctor::doctor` does not consult `checkout_conflict()` at
all today (`src/ops/doctor.rs:30-77`), so the scoping §7 called "already
existing" would have to be added.

**Resolution.** Under a checkout conflict, drift is **informational**: reported
as "the shared index currently reflects the checkout at `<path>`", not folded
into `healthy`, not reaching `doctor --fix`, and not reaching the maintenance
nag. Blanket-suppressing tiers B–D on tier A's rule would instead convert a
wrong-but-loud state into a silent one.

### S5. Group and global stores: the salt is the *caller's* config

A group store is written by every subscribed project's process, each with its own
`config.toml`. `chunk_tokens` comes from the writing project's
`effective_chunk_tokens` (`engine.rs:458-459`) and `composition_id` from its
`metadata_vector`. Two projects with different `[embeddings].max_tokens` writing
one group store produce permanently divergent digests — and there is no
`reindex --group` path at all (`commands/reindex.rs:41-45` handles only
`--global` and the project dir), so group digests are never backfillable.

The codebase already knows shared stores embed under divergent configs — that is
what `ops::shared_store_fingerprint` and the group fingerprint-alignment check
(`ops/doctor.rs:1382-1480`) exist for.

**Resolution.** A subscribing project never judges a group/global store's vector
currency — only that store's own maintenance may. Excluded from `stale_vectors`
reporting. If group digests are to be maintainable at all, a `reindex --group`
path is a prerequisite and is out of scope here.

### S6. The digest parameter must be `Option<String>`

`worktree.rs:152-180` relocates existing vectors into the main store without
re-embedding — that is its documented purpose — and, being inside
`engram-storage`, cannot reach `embedding_texts` without inverting the DAG. A
`String` parameter forces either a DAG violation or a sentinel, and a sentinel is
dangerous because the phase-2 predicate is an equality test: two rows stamped
`""` compare equal.

**Resolution.** `Option<String>`; `None` = unknown = must re-embed, per §8's own
rule. §8 gains a row for relocated vectors, noting they appear as
stale-vectored until the next reindex even though the vectors are valid.
`LanceIndex::upsert_chunks` (the single-memory form, `lance_index.rs:917`, public
via `store.upsert_chunks`) is a fourth call site the column touches.

### S7. Phase 3 must re-stamp the two fields that are not functions of the bytes

`reindex_dir` phase 5 overrides two fields after constructing the entry:
`visibility` from the **directory** (`store.rs:1998`) and `has_embedding` from
the **chunks table** (`:2002`). Neither is derivable from the file bytes, so a
content-hash match proves nothing about either, and the schema/normalizer stamps
don't cover them — they are not derivation-code drift.

- **`has_embedding`**: R3's contract is that reindex rebuilds it. Under
  `skip_row`, a memory whose chunk rows vanished keeps `has_embedding = true`
  forever and stays eligible for gated semantic ranking with no vectors — and
  reindex is the only repair, which phase 3 would remove.
- **`visibility`**: a memory file hand-moved between the shared and personal
  dirs has no byte change, so it is skipped and keeps the wrong visibility.

**Resolution.** Even in incremental mode, re-stamp both for **every enumerated
file**, skipped or not. Both are free — the chunk-id snapshot and the enumerating
directory are already in hand; only parse and stems are skipped. Added to §5's
must-not-break list.

### S9 / N4. Two cost items §9 omitted

- **`doctor` becomes an unbudgeted full-store read.** Today it does zero file
  reads. `auto_maintain` calls it on the command path every
  `[maintenance].interval_secs` (default 6 h), so one unlucky `engramdb query`
  pays a full-store read + SHA-256 (~50 MB at 10k memories). → Give the
  *maintenance-path* doctor call the same declared budget as tier D; reserve the
  unbudgeted form for an explicit `doctor` / `reindex --dry-run`.
- **The 0.8.0 migration fans out across every project.**
  `ops::harvest_pin::evidence_links` opens every store in the `SessionScope`
  (`harvest_pin.rs:99` already notes "possible schema migration"), 8 at a time,
  from the SessionEnd hook; `open_group`/`open_global` migrate on open too. So
  the first post-upgrade session end runs `reindex_with(true)` — taking the
  per-project write lock — for every registered project, and the first
  post-upgrade query does the same for every subscribed group. §9.4's "seconds"
  understates it by a factor of N-projects on two latency-sensitive paths.
  (`archive_transcript` runs *before* `evidence_links` at `hook.rs:995-1010`, so
  a hook timeout during the storm costs only the retention sweep.)

### Smaller items folded in

- **N1** — `Nothing to reindex.` (`commands/reindex.rs:91-97`) becomes a lie once
  everything is skipped; the condition must consult `skipped`, and a
  `Skipped N unchanged memories.` line added.
- **N2** — tier B's id comes from the filename stem, but `extract_id_from_stem`
  (`memory_file/mod.rs:124-133`) never fails and `reindex_dir` keys by the
  **parsed** `memory.id` (`store.rs:1951`). A `notes.md` yields a phantom id and
  tier B reports "ids differ" forever. (`collect_orphans` already has this bug at
  `doctor.rs:100-103`.) → Exclude non-UUID stems from tier B, or resolve by parse.
- **N5** — a file that stats but cannot be read or hashed is currently neither
  stale nor orphaned nor drifted, i.e. silently counted as clean. Per the
  codebase's own rule that has to be declared: an `undetermined` list.
- **S8** — §12's rollback claim is now settled, not asserted: the storage pass
  confirmed a narrower source schema *is* accepted and the target's values are
  preserved, so the columns come back **stale, not absent**. §12 records this.

### What the review told us not to change

`embed_sha256` on the chunks table is load-bearing for a reason beyond §1.2's:
because `list_embed_digests()` reads the chunks table, a memory whose chunk rows
were deleted has **no** digest, so the predicate falls through to re-embed even
when `has_embedding` is stale-true. That is what keeps `--embeddings-only` — which
never refreshes `has_embedding`, since `store.reindex()` is skipped on that branch
(`ops/reindex.rs:72`) — from skipping a vectorless memory. Do not move it.
