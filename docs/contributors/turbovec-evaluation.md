# turbovec for memory vectors: evaluated, not adopted

*2026-08-18 · benchmark assets: `docs/contributors/turbovec-probe/` (standalone crate: recall, fixed overhead, allowlist selectivity, and per-project cost vs LanceDB)*

[turbovec](https://github.com/RyanCodrai/turbovec) is a Rust implementation of
Google Research's [TurboQuant](https://arxiv.org/abs/2504.19874): vectors are
normalized, randomly rotated, optionally calibrated per coordinate (TQ+), then
Lloyd-Max scalar-quantized to 2/3/4 bits and bit-packed. Search rotates the
query once and scans the packed codes with hand-written SIMD kernels. It is a
**flat** index — no IVF, no HNSW — so it is the same algorithmic class as what
EngramDB runs today, executed on 8–16× smaller data.

This document evaluates it for two candidate uses: the memory/chunk vectors
(`crates/engram-storage/src/lance_index.rs`) and the harvest conversation index
(`crates/engram-storage/src/conversation_index.rs`).

**Conclusion: don't adopt.** The crate is careful, well-tested and honestly
documented, and its benchmarks reproduce. But every claim that would justify it
describes a regime EngramDB is two to four orders of magnitude away from, and
the accuracy cost at *our* dimensionality and corpus size is severe enough to be
disqualifying on its own.

## TL;DR — ranked findings

| # | Finding | Evidence | Consequence |
|---|---------|----------|-------------|
| 1 | **The compression argument inverts at our scale.** turbovec carries a fixed per-index overhead of **248 KB (2-bit) / 451 KB (4-bit)** at d=384, independent of vector count. Break-even against raw f32 is **~339 vectors at 4-bit**, ~174 at 2-bit. | R1 | A project below ~340 chunks uses *more* space with turbovec. The whole corpus is 0.3–4 MB of f32 anyway. |
| 2 | **4-bit costs 12–25% of top-1 results at d=384.** Measured recall@10 0.86–0.96 and **top-1 0.74–0.96** against exact cosine, degrading as n grows. 2-bit is unusable (top-1 0.36–0.60). | R2 | For a store whose job is surfacing *the* relevant convention before a file edit, a 1-in-6 top-1 miss is a product regression, not a tuning knob. The reranker cannot recover a memory that never entered the shortlist. |
| 3 | **The allowlist prefilter — the headline feature — is real but sublinear, and we already have it.** At 1% selectivity it is only ~2× faster than an unfiltered scan, not ~100×; at 100% allowed it is *slower* than no filter. And it returns `Err(UnknownId)` for any id absent from the index, failing the whole query. | R3 | `restrict_to` (`lance_index.rs:1458`) and `session_id IN/NOT IN` (`conversation_index.rs:607`) are genuine pushdowns already, with regression tests. And our restrict lists legitimately contain ids with `has_embedding = false`, which turbovec would reject outright. |
| 4 | **Score semantics differ everywhere except two points.** We use `1/(1+‖q−v‖²)`; turbovec returns the inner product. For unit vectors these are `1/(3−2c)` vs `c`, which agree only at `c = 0.5` and `c = 1.0`, with the gain differing by 2–4.5× across MiniLM's working range. | R4 | Every weight in `retrieval.scoring` — user-facing config with published defaults — would need retuning. The semantic term also loses its `[0.2, 1.0]` floor and can go negative. |
| 5 | **Adoption forces a lock onto a deliberately lock-free read path.** turbovec is sync (needs `spawn_blocking`), all mutations take `&mut self` (needs `RwLock`), and `sync()` explicitly does not support concurrent writers. | R5 | EngramDB runs one MCP server *per Claude Code session* plus CLI, hooks and daemon against one store. Today reads are lock-free via LanceDB MVCC. |
| 6 | **`u64` ids vs our string ids, and a fourth consistency obligation.** `IdMapIndex` keys on `u64`; memory ids are UUIDv7 strings that appear in filenames and the `memory_id` column. `IdMapIndex` has no `from_parts`, so its id↔slot map is private state. | R5 | Either a truncating hash (a collision returns the *wrong memory*) or a persisted side-table that schema migration must rebuild — and migration rebuilds from the `.md` files, which a `.tvim` blob is not. |
| 7 | **The one place with a plausible shape is machine-wide harvest search**, where per-project LanceDB opens dominate and recall loss is cheap. | R6 | Even there, cache the `Table` handle first. That is a smaller change than a dependency. |

Two findings **unrelated to turbovec** surfaced during the review and are worth
fixing either way; both are written up in [Incidental findings](#incidental-findings).

## What EngramDB actually runs today

Worth stating plainly, because three of the arguments for turbovec assume
otherwise:

- **There is no ANN index in practice.** `create_vector_index` is gated behind
  `VECTOR_INDEX_MIN_ROWS = 8192` chunks (`lance_index.rs:41`), and its own doc
  comment says the gate exists to keep "hundreds to low-thousands of chunks" on
  "unchanged, 100%-recall exact search". The `conversations` table never builds
  one at all. **Both paths are already exact flat scans.**
- **The metric is L2, not cosine.** Nothing in the workspace calls
  `distance_type`, and LanceDB defaults to (squared) L2; `lance_index.rs:1492`
  converts with `1.0 / (1.0 + distance)`.
- **The vectors are unit-norm.** `fastembed` applies `common::normalize`
  unconditionally (`text_embedding/output.rs:44`), so inner product and L2 are
  rank-equivalent — the *ordering* is compatible, the *scores* are not.
- **The daemon holds no indices.** `src/daemon/` hosts models only; grepping it
  for `LanceIndex`/`ConversationIndex`/`MemoryStore` returns nothing.
- **The file-edit hook never runs a vector search.** `PreToolUse` builds its
  engine with `build_engine_without_providers` and takes the no-query Rank path
  (`rank_scope_only_from_index`), which touches neither the embedder nor the
  chunks table.

That last point deserves emphasis: **the single most important file-scoped
retrieval in the product does no vector work at all.** A faster or smaller
vector index cannot change it by a microsecond.

## Method

All measurements on the web sandbox: Intel Xeon @ 2.80 GHz, 4 vCPU, **AVX2 but
no AVX-512** (so turbovec takes its AVX2 kernels, not the VNNI path its x86
benchmarks use). turbovec 1.0.0 from crates.io, `--release`, `opt-level = 3`.

Corpora are synthetic at d=384 — EngramDB's configured dimensionality
(`config.rs:890`) — in two geometries: isotropic unit Gaussians, and an
anisotropic variant (coordinate *i* scaled by `i^-0.5`) as a crude stand-in for
real sentence-embedding anisotropy. Ground truth is an exact f32 inner-product
top-k over the same corpus. 50 queries per cell for recall, 200–300 reps for
latency after warm-up.

**Caveat stated up front:** synthetic vectors are a proxy. Isotropic Gaussians
at d=384 are a pathological near-tie regime (random pairs sit at cos ≈ 0 ± 0.05),
which makes top-k recall artificially hard; the anisotropic cells are closer to
real embeddings and score better. Real MiniLM-L12 embeddings are *more*
anisotropic than this proxy, and anisotropy is where TQ+ calibration helps most —
so the 4-bit figures below are mildly pessimistic for a well-calibrated index and
mildly optimistic for the uncalibrated day-1 state. The ranking of the
conclusions does not change. Re-running against this project's own corpus is
listed under [Reproducing](#reproducing).

## Results

### R1 · The compression argument inverts below ~340 vectors

turbovec's file size is `fixed + marginal × n`. Measured at d=384:

| bit width | fixed overhead | marginal bytes/vector | break-even vs raw f32 |
|---|---|---|---|
| 2-bit | **248,499 B** | 108.3 | **~174 vectors** |
| 4-bit | **451,155 B** | 204.5 | **~339 vectors** |

The fixed part is the rotation/codebook state and is paid whether the index
holds one vector or a million:

```
  bits=4 n=0     bytes=451155    raw_f32=0         ratio=0.00x
  bits=4 n=1     bytes=451155    raw_f32=1536      ratio=0.00x
  bits=4 n=100   bytes=470739    raw_f32=153600    ratio=0.33x
  bits=4 n=500   bytes=549075    raw_f32=768000    ratio=1.40x
  bits=4 n=2000  bytes=855891    raw_f32=3072000   ratio=3.59x
```

The advertised 16× is a 2-bit figure at large n; we measure 13.9× at 2-bit and
7.4× at 4-bit at n=100,000, and **0.33×** — i.e. three times *larger* — at
n=100.

Now scale it. Chunks are ~2–4 per memory (`metadata_vector = true` plus
`chunk_text` at ~192 words), so a realistic project sits at 10²–10⁴ vectors:

| memories | ~chunks | raw f32 | 4-bit | saving |
|---|---|---|---|---|
| 100 | 250 | 0.38 MB | 0.50 MB | **−0.12 MB** |
| 1,000 | 2,500 | 3.8 MB | 0.96 MB | 2.9 MB |
| 5,000 | 12,500 | 19 MB | 3.0 MB | 16 MB |

turbovec's headline — *"a 10 million document corpus takes 31 GB as float32;
turbovec fits it in 4 GB"* — is a real and impressive claim about a corpus four
orders of magnitude larger than ours. At our scale we would be trading single-digit
megabytes for the accuracy in R2.

### R2 · 4-bit costs 12–25% of top-1 results at d=384; 2-bit is unusable

recall@10 and top-1 agreement against exact cosine, TQ+ calibration as noted:

| n | 4-bit iso | 4-bit aniso | 2-bit iso | 2-bit aniso |
|---|---|---|---|---|
| 500 | 0.880 / **0.78** | 0.960 / **0.96** | 0.692 / 0.58 | 0.862 / 0.82 |
| 2,000 | 0.868 / **0.86** | 0.938 / **0.88** | 0.612 / 0.50 | 0.814 / 0.74 |
| 10,000 | 0.860 / **0.72** | 0.910 / **0.86** | 0.540 / 0.46 | 0.760 / 0.68 |
| 100,000 | 0.796 / **0.74** | 0.892 / **0.90** | 0.502 / 0.34 | 0.682 / 0.60 |

*(recall@10 / top-1, calibrated)*

Three things matter here:

1. **Top-1 is the number to read, not recall@10.** EngramDB injects 5 memories
   into a hook and 10 into a query. A 0.86 recall@10 that misses the *best*
   memory 14–26% of the time is worse than the aggregate suggests.
2. **It degrades with n.** 4-bit isotropic goes 0.88 → 0.80 recall from n=500 to
   n=100,000. A memory store grows monotonically over a project's life, so this
   gets worse, never better.
3. **turbovec's own tests concede the trend.** `tests/recall_sanity.rs` states
   ~0.85 (4-bit) / ~0.55 (2-bit) recall@10 at d=**1536**, n=2000 — already a
   warning at four times our dimensionality, and the README explicitly names low
   dimension as "the harder regime — at low dim the asymptotic Beta assumption is
   looser."

Score error is small in absolute terms (4-bit: mean |Δ| 0.004, p95 0.011) but
MiniLM adjacent-rank cosine gaps in a topical corpus are routinely below 0.05,
so p95 error is a fifth of a typical rank-1-vs-rank-3 gap. That is the mechanism
behind the top-1 numbers.

### R3 · The allowlist is real but sublinear — and we already have a better one

turbovec's README describes filtering as short-circuiting whole blocks so that
"selective allowlists therefore avoid most of the SIMD cost". Measured
(4-bit, k=10, query time by allowed fraction):

| n | no filter | 100% allowed | 50% | 10% | 1% |
|---|---|---|---|---|---|
| 2,000 | 129.9 µs | 150.1 µs | 137.9 µs | 114.0 µs | 109.3 µs |
| 10,000 | 527.9 µs | 494.0 µs | 436.4 µs | 303.5 µs | 269.0 µs |
| 50,000 | 1.92 ms | 2.09 ms | 1.63 ms | 1.08 ms | 1.10 ms |

The speedup is **real but sublinear**: allowing 1% of the corpus costs ~50% of a
full scan, not ~1%. A true prefilter would be ~100× faster there. And at 100%
allowed the mask-building overhead makes it *slower* than not filtering. So
"avoid most of the SIMD cost" overstates it — this is a cheap candidate gate,
not proportional work reduction.

Two further problems for our use:

- **Error semantics are hostile.** `search_with_allowlist` returns
  `Err(SearchError::UnknownId(id))` if *any* id is absent from the index
  (verified directly). Our `restrict_to` list comes from a metadata prefilter and
  routinely contains memories with `has_embedding = false` — that column exists
  precisely because embedding presence is optional. LanceDB's `is_in(...)`
  silently matches nothing for those; turbovec fails the entire query.
- **We already have the semantics.** `engine.rs:1191-1200` builds an id
  allowlist and `lance_index.rs:1458-1464` pushes it into the query plan as
  `is_in(col("memory_id"), …)`, a genuine prefilter. Two regression tests pin
  exactly the crowding-out behaviour turbovec's allowlist would sell —
  `test_vector_search_pushdown_only_filter_still_restricts` and
  `since_survives_a_candidate_set_full_of_nearer_old_sessions`.

The residual gap is not the mechanism but its ceiling: the allowlist is disabled
above `VECTOR_RESTRICT_MAX_IDS = 500` candidates (`engine.rs:42`). Raising or
adapting that constant uses machinery already in place and needs no dependency.

### R4 · Score semantics agree at exactly two points

Both systems consume unit vectors, so with `c = cos(q, v)`:

| | score |
|---|---|
| EngramDB today | `1 / (1 + ‖q−v‖²)` = `1 / (3 − 2c)` |
| turbovec | `c` (inner product) |

These agree only at `c = 0.5` and `c = 1.0`. Their *gains* differ across the
range that matters:

| c | `d/dc` today | `d/dc` turbovec | ratio |
|---|---|---|---|
| 0.0 | 0.222 | 1.0 | 4.5× |
| 0.3 | 0.347 | 1.0 | 2.9× |
| 0.5 | 0.500 | 1.0 | 2.0× |
| 0.7 | 0.781 | 1.0 | 1.28× |

MiniLM cosines for a project corpus cluster around `c ∈ [0.15, 0.65]` — exactly
where the ratio is 2–4×. Swapping the transform re-weights the semantic signal
against keyword and relevance by that factor, so every weight in
`retrieval.scoring` would need retuning, and those are user-facing config with
published defaults.

There is also a structural loss. `1/(3−2c)` is bounded in `[0.2, 1.0]`: the
semantic term can never contribute less than `0.55 × 0.2` to base in `with_query`
mode. The inner product ranges over `[−1, 1]`, so a negative cosine — routine
between unrelated memories — makes the term *subtract*. `composite.rs:218-224`
documents `semantic_score = Some(0.0)` as a load-bearing state meaning "checked,
found nothing" that must score strictly below `None`; `Some(-0.3)` becomes
reachable and nothing expects it.

A closed-form conversion exists (`1/(1+√(2−2·ip))` recovers today's scale), so
this is *tractable* — but it is recalibration work in service of R1 and R2, which
are the reasons not to.

### R5 · Architectural friction

- **Sync API into an async storage layer.** `LanceIndex::vector_search` is
  `pub async fn`; turbovec is entirely synchronous *and* internally uses rayon.
  Every call needs `spawn_blocking`, and rayon-inside-a-Tokio-worker is the
  classic deadlock shape.
- **A writer lock on a lock-free read path.** `search(&self)` is fine, but every
  mutation takes `&mut self`, so `create`/`update`/`delete` need
  `Arc<RwLock<IdMapIndex>>`. The stated concurrency model is "reads are lock-free
  and rely on LanceDB MVCC" (`write_lock.rs`, CLAUDE.md). This is a direct
  regression.
- **No multi-process story.** `sync()`'s own doc: "one process syncs a given path
  at a time… two processes syncing the same path concurrently is unsupported."
  EngramDB runs one stdio MCP server per Claude Code session, plus CLI, hooks and
  the daemon, concurrently against one project.
- **A fourth consistency obligation.** LanceDB is not "a vector index" here — it
  holds the metadata-for-filtering columns, the seven epistemic columns, `decay`,
  `has_embedding`, `source_sessions`, the `conversations` table, the scalar/FM
  indices, MVCC and the manifest. turbovec replaces one method,
  `vector_search`, while adding a file that must stay consistent with the Lance
  table, the `.md` files and the ledger. CLAUDE.md already documents at length
  what it cost when two stores came apart.
- **Migration has nowhere to put it.** `CURRENT_SCHEMA_VERSION` migration works
  by rebuilding the memories table *from the `.md` files* — that is why
  `source_sessions` is a field and not a join table. A `.tvim` blob is neither a
  Lance column nor a memory file, and `IdMapIndex` has no `from_parts`, so its
  id↔slot map is private state that cannot be reconstructed from our data.
- **`panic = "abort"` meets a panicking API.** `search`, `add` and
  `add_with_ids` panic rather than returning `Result`; only the `try_*` forms are
  safe. A malformed query from an MCP caller would hard-abort the server. This is
  the same hazard class already documented for `ort`'s dylib loader.

**Dependency cost is the one objection that does *not* hold.** turbovec adds 13
crates (8 new names — `nalgebra`, `simba`, `statrs`, `wide`, `safe_arch`,
`approx`, `num-rational`, `nalgebra-macros` — plus duplicate versions of `rand`
0.8, `rand_core` 0.6, `rand_chacha` 0.3, `rand_distr` 0.4 and `ordered-float` 4,
each of which we already carry at newer versions). All licenses are in
`deny.toml`'s allow list, no crate carries an unpatched advisory, and LTO strips
most of it: measured **+335 KB** on a ~57.7 MB binary (+0.58%). Cold build of the
whole subtree is ~29 s. That is affordable; it is simply not worth buying.

The one latent trap: turbovec `=`-pins `rand_chacha 0.3.1` and `statrs 0.17.1`.
An advisory against either fails our `cargo deny` with no local remediation —
exactly the `quick-xml`/`pprof` situation `deny.toml` already documents.

### R6 · Where a genuine win might exist

Machine-wide harvest search (`ops::harvest_index::search_other_projects`) is the
one path whose cost grows with something turbovec changes. Per project it pays
`lancedb::connect` + **three** `open_table` calls (four with `--since`) + two
DataFusion vector plans, because `ConversationIndex::table()` re-opens on every
call and only the `Connection` is cached. With `--since` it additionally
full-scans `session_id` + `ended_at` and builds a SQL `IN` list that can reach
~100 KB of literals at 5,000 rows.

That is the shape where a memory-mapped flat file beats N table opens, and it is
the one place recall loss is cheap — you are surfacing a transcript to read, not
a convention gating a file edit.

But two things must be true first, and only one is testable without a decision:

1. **Measure the LanceDB open cost.** [Pending — see Caveats.]
2. **Try the cheaper fix.** Caching the `Table` handle per `ConversationIndex`,
   or opening lazily, removes most of the per-project cost without a dependency.
   If that closes the gap, the question is settled.

## Incidental findings

Neither is about turbovec; both were found while tracing the code and are worth
fixing independently.

### F1 · Root-scoped memories are arithmetically excluded from the file-edit hook

The `PreToolUse` hook takes `rank_scope_only_from_index`, whose score is
`base × scope × trust × situation`, retained only at `>= relevance_threshold`.
With defaults — `depth_decay_base = 0.82`, `depth_decay_floor = 0.3`
(`config.rs:492-493`), `relevance_threshold = 0.45` (`config.rs:705`), scope-only
weights `relevance: 1.0` — and `composite.rs:402` applying scope as a **bare**
multiplier when a path is present (`scope_multiplier_floor` is reachable only via
the *logical* axis):

| file depth | scope score | best possible final | vs 0.45 |
|---|---|---|---|
| 2 (`src/main.rs`) | 0.6724 | 0.672 | passes |
| 4 (`src/api/auth/handlers.rs`) | 0.4521 | 0.452 | *barely* |
| **5** (`crates/engram-cli/src/commands/hook.rs`) | **0.3707** | **0.371** | **dropped** |
| ≥6 | 0.3 (floor) | 0.300 | **dropped** |

A new memory's `physical` defaults to `["/"]` (`memory.rs:297`). So for any file
at depth ≥ 5 — most of this repository — **no default-scoped memory can clear the
threshold regardless of criticality, trust or epistemic class**, and the hook
silently injects nothing.

This is the same defect class as the logical-axis bug already fixed:
`test_rank_logical_only_returns_matches_above_threshold` (`engine.rs:2741`)
documents it verbatim — *"every logical-only result fell below the default
relevance_threshold (0.45)… the threshold is the bug"* — and was fixed by adding
`scope_multiplier_floor`. The physical axis never got the equivalent.

Derived from constants and the pinned proximity tests (`physical.rs:573-591`),
not from a live run. **Reproduce with a test before fixing.**

### F2 · `VectorMatch::score` is documented as cosine and is not

`lance_index.rs:312` says "Cosine similarity score (higher is better)". It is
`1/(1 + squared_L2)`: `vector_search` never sets `distance_type`, LanceDB
defaults to L2, and Lance's L2 is squared. `docs/contributors/parallelization-simd.md`
already states this correctly, so the comment is stale rather than a new
discovery — but anyone reasoning about scoring from it will get the arithmetic
wrong, as the initial brief for this review did.

## Caveats

- **Synthetic corpora.** See [Method](#method). The decisive re-run is against
  this project's own MiniLM-L12 embeddings.
- **No AVX-512 on the measurement host.** turbovec's x86 benchmarks use AVX-512
  VNNI; we measured its AVX2 path. Latency figures are therefore conservative for
  turbovec; the R1 size figures and R2 recall figures are unaffected
  (quantization is architecture-independent).
- **The LanceDB open cost is unmeasured.** This is the one number that decides
  R6, and it is missing.
- **turbovec 1.0.0 is four months old** (first release 2026-04-13) and 1.0.0
  shipped 2026-08-18 — the day of this evaluation. 14 releases, an MSRV move from
  1.70 to 1.89, and a full dependency-stack rewrite (`faer`/`ndarray`/BLAS
  dropped) inside that window. Its own test suite is unusually serious for a 1.0
  (44 integration files including adversarial durability fuzzing and kernel
  correctness), but no third party has exercised it in production.

## Reproducing

All four measurements live in [`turbovec-probe/`](./turbovec-probe/), a
standalone crate with its own `[workspace]` table so `turbovec` never enters the
workspace lockfile — the same isolation `fuzz/` uses.

```bash
cd docs/contributors/turbovec-probe
cargo run --release --bin probe      # R2: recall@10 + top-1 vs exact cosine
cargo run --release --bin overhead   # R1: fixed bytes/index; vs a plain f32 scan
cargo run --release --bin allowlist  # R3: selectivity; UnknownId semantics
cargo run --release --bin vs_lance   # R6: per-project cost vs LanceDB (needs protoc)
```

No ONNX runtime and no staged model are needed — every corpus is synthetic,
which is exactly the limitation below.

The decisive follow-up, if anyone reopens this: replace the synthetic corpus with
real embeddings from this project's own store and re-run R2. If 4-bit top-1
agreement against exact cosine stays below ~0.95 on real vectors, the question is
closed without needing R6 measured at all.
