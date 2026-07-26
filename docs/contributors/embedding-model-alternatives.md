# Model-stack alternatives: smaller / faster / better candidates

*2026-07-26 · benchmark assets: `examples/embed_matrix.rs`, `examples/embed_model_bench.rs`, `examples/rerank_bench.rs`, `examples/embed_determinism_probe.rs`, corpus `examples/data/embed_eval.json`*

Companion to [embedding-analysis.md](./embedding-analysis.md), which studied
*chunking and field composition* and closed E2 ("model swap") with **keep
MiniLM-q**. This document re-opens E2 with a wider candidate set, adds a clean
latency instrument, and extends the sweep to the **other three models** in the
stack (reranker, NLI, T5 titler) that the earlier study never touched.

The headline result is that E2's conclusion was right about *its* candidates
(BGE-small, nomic) and wrong as a general statement: the same-family
**`all-MiniLM-L12-v2-q`** was never tested, and it is the best model measured
on this corpus by a clear margin.

## TL;DR — ranked recommendations

Recommendations 1 and 2 are **applied** — this document is the evidence behind
the two default changes, not a proposal.

| # | Change | Evidence | Cost |
|---|--------|----------|------|
| 1 | **Reranker default → `jina-reranker-v1-turbo-en`** (from `bge-reranker-base`) — **applied** | 7.3× smaller (145 MB vs 1060 MB), 3.4× faster per pair (82 vs 278 ms), 11× faster to load. And at the shipped `weight = 0.5` it is the *only* one that helps at all: bge-reranker-base reproduces the un-reranked ranking exactly, so the old default bought nothing for ~5.6 s/query. | One-line config default. **Zero migration** — rerank scores are not persisted. |
| 2 | **Embedding default → `all-MiniLM-L12-v2-q`** (from `-L6-`) — **applied** | Wins **6/6 paired runs** with non-overlapping ranges: MRR@10 mean 0.953 (range 0.938–0.958) vs 0.907 (0.875–0.920); P@1 0.931 vs 0.868. Beats fp32 L6 too, so the gain is depth, not quantization precision. | +10 MB (22 → 32 MB), 2.0× warm embed latency (2.83 → 5.70 ms), one `reindex --embeddings-only`. Same 384 dims and 256-token window ⇒ no config/schema change. Pin the old model with `provider = "all-minilm-l6"`. |
| 3 | **Don't** switch to snowflake-arctic-embed (xs or s) | arctic-xs-q is genuinely free on cost (same 22 MB, 0.94× warm latency, 1.66× faster cold start) but over 6 runs lands at MRR 0.914 with its query prefix / 0.903 without — statistically indistinguishable from the L6 model it would replace (0.907), and well below L12 (0.953). The prefix is also plumbing EngramDB does not have. arctic-s-q is worse than arctic-xs-q at 2.2× the latency. | — |
| 4 | **Don't** switch to static (model2vec) embeddings as the default | 100–450× faster and ONNX-free, but costs 6.6 pts MRR and collapses paraphrase queries (MRR 0.66 → 0.28). Worth keeping on the shelf for the tract/no-ORT build, not for the default path. | — |
| 5 | Re-confirmed: **don't** switch to bge-small / nomic / mxbai / fp32 MiniLM | Reproduced E2 with cleaner instrumentation. bge-small-q is 5.7× slower for no reliable gain; nomic-q is catastrophic here (MRR 0.642 at best); fp32 MiniLM is 1.31× slower and *worse* than L12-q. | — |
| 6 | **Open issue found while benchmarking: int8 embeddings are not reproducible under CPU load.** The same text through the same provider can come back with pairwise cosine **0.57** (old L6 default) / **0.71** (observed in the test suite). fp32 is bit-exact in every condition. | `examples/embed_determinism_probe.rs` (new). Not a ranking subtlety — a materially different vector for identical input, persisted at create time. | Not fixed here; see R6 for the likely cause (ORT intra-op threading) and the options. L12-q is the more robust of the two int8 models, which is an independent argument for recommendation 2. |

## Current stack (what is actually loaded, and why)

| Role | Model | Size on disk | Default? | Why this one |
|------|-------|--------------|----------|--------------|
| Embedding | `all-MiniLM-L6-v2` **int8** | 22 MB | was **on**, now `all-minilm-l6` | Lever-B A/B: 1.4–1.9× faster and ~4× smaller than fp32 at cosine ≈ 0.99 |
| Embedding | **`all-MiniLM-L12-v2` int8** (`Xenova/all-MiniLM-L12-v2`) | 32 MB | **on** (new) | R1/R2 below |
| Embedding (tract) | `all-MiniLM-L12-v2` **fp32** | 128 MB | Intel Mac only | No native ORT; int8 has no tract build. Tracks the ONNX default's depth so a shared store differs only in precision |
| Reranker | `bge-reranker-base` fp32 | **1060 MB** | was default, still selectable | Historical default; a no-op at the shipped blend weight (R3) |
| Reranker | **`jina-reranker-v1-turbo-en` fp32** (`jinaai/…`) | **145 MB** | **default** (new), still off unless `rerank.enabled` | R3 below |
| NLI (challenge) | `nli-deberta-v3-xsmall` int8 (`Xenova/…`) | 83 MB | off | Lever-D A/B: ~2× faster, ~3.7× less RAM than fp32, same label order |
| Title | `t5-small` int8 (`Xenova/t5-small`), encoder + decoder | 74 MB (33 + 41) | **on** | Abstractive titles; daemon-amortized and pooled (size 2) |

Two of the five are on by default (embedding, T5 title); the two heavyweights
(reranker, NLI) are opt-in. Embeddings are called **once per query** (the query
text) and **once per chunk per create** (`embed_batch` over the metadata vector
plus content chunks). Measured on this 4-core host, embedding is ~37 ms of a
create (`timing_create`: 63.7 ms inline vs 26.6 ms with `embed_async`), and the
create path already defers it to the background.

## Method

Two instruments, both driven off the frozen 60-memory / 48-query labeled corpus
from the earlier study, at the **production configuration** (metadata vector +
content chunks, 256-token budget, max aggregation):

- **Quality** — `examples/embed_matrix.rs`, unchanged harness, three new models
  added to its `MODELS` table. Numbers quoted are the `fieldvec_c256`,
  `agg=max` cell, i.e. what the store actually does today.
- **Latency / footprint** — `examples/embed_model_bench.rs` (new). Every model
  embeds the **same** texts chunked at the **same** fixed 256-token budget, so
  context-window differences can't leak into the timing. `embed_matrix`'s
  `ms_per_text` cannot do this: it averages over every variant it runs,
  including per-model `full`/no-chunk cells.
- **Reranker** — `examples/rerank_bench.rs` (new). Drives the real
  `LocalReranker` over candidates from the real bi-encoder path, with
  `rerank_document`'s composition and `apply_rerank`'s blend.
- **Repeats** — the shortlisted embedding models were re-run **6×** to separate
  real deltas from int8 nondeterminism. This turned out to matter, and to
  invalidate a claim made from the first three runs (R2).

Static-embedding candidates were evaluated in a Python mirror of
the same harness (`chunk_text` incl. runt rebalance, same compositions, same
max aggregation, same P@1 / R@5 / MRR@10 / nDCG@10 definitions). Its MiniLM
control reproduces the Rust harness's *effect sizes* exactly — base → fieldvec
is +0.148 MRR in both — so relative comparisons carry over; absolute values sit
a few points high because pooling/normalization details differ.

**Noise discipline** is inherited from the earlier study: with n=48 queries,
overall deltas below ~5 pts R@5 / 0.05 MRR are noise on a single run. The
measured run-to-run spread of the int8 models (0.017–0.045 MRR) is consistent
with that floor, which is why the model verdicts below rest on *non-overlapping
ranges over 6 runs* rather than on one delta.

## Results

### R1 · Embedding models — `all-MiniLM-L12-v2-q` wins

Quality at the production cell (`fieldvec_c256`, max agg). The three
shortlisted models were run **6 times each**; they report the mean and the
full observed range. Others are a single run.

| Model | dim | P@1 | MRR@10 | MRR range (n=6) | R@5 | nDCG@10 |
|---|---|---|---|---|---|---|
| `minilm-q` (the **previous** default) | 384 | 0.868 | 0.907 | 0.875 – 0.920 | 0.920 | 0.910 |
| **`minilm-l12-q`** (**new default**) | 384 | **0.931** | **0.953** | **0.938 – 0.958** | 0.913 | 0.933 |
| `arctic-xs-q` (raw) | 384 | 0.865 | 0.903 | 0.896 – 0.913 | 0.918 | 0.898 |
| `arctic-xs-q` (+ query prefix) | 384 | 0.892 | 0.914 | 0.901 – 0.927 | 0.891 | 0.900 |
| `minilm-fp32` | 384 | 0.896 | 0.930 | — | 0.931 | 0.927 |
| `arctic-s-q` (+ query prefix) | 384 | 0.896 | 0.913 | — | 0.889 | 0.892 |
| `bge-small-q` (+ query prefix) | 384 | 0.896 | 0.924 | — | 0.948 | 0.919 |
| `nomic-q` (+ prefixes) | 768 | 0.625 | 0.642 | — | 0.674 | 0.676 |

The separation is the point: **L12-q's worst run (0.938) beats L6's best run
(0.920) and arctic-xs's best (0.927)**, and it wins all 6 paired runs on both
P@1 and MRR. That is a much stronger statement than a mean delta against the
corpus's ~0.05 MRR noise floor, which a single run could not have supported.

Cost, measured on identical inputs (`embed_model_bench`, 4-core CPU, 60 warm
iterations; "warm" is the query path, "batch16" the create/reindex path):

| Model | disk | cold ms | warm mean | warm p95 | batch16 ms/text | warm vs default |
|---|---|---|---|---|---|---|
| `minilm-q` | 22 MB | 171 | 2.83 | 3.70 | 11.57 | 1.00× |
| `minilm-fp32` | 86 MB | 185 | 3.69 | 4.80 | 21.83 | 1.31× |
| **`minilm-l12-q`** | 32 MB | 146 | 5.70 | 7.04 | 21.87 | **2.02×** |
| `arctic-xs-q` | 22 MB | 103 | 2.66 | 3.64 | 15.61 | 0.94× |
| `arctic-s-q` | 32 MB | 182 | 5.88 | 7.75 | 30.47 | 2.08× |
| `bge-small-q` | 63 MB | 144 | 16.23 | 19.12 | 64.68 | 5.74× |

Reading the two tables together:

- **L12-q is the only model that separates from the default** (+0.046 MRR on
  means, and no overlap between the two models' 6-run ranges; +0.063 P@1). It
  beats fp32 L6 too — so the gain is depth, not precision. It costs 2.02× warm
  latency, but in absolute terms that is **2.83 → 5.70 ms** on the query path
  and 11.6 → 21.9 ms/text on the create path, which async ingest already
  defers. Cold start actually *improves* (171 → 146 ms), which matters for the
  daemon-less CLI.
- **arctic-xs-q is the "free" candidate that isn't worth taking.** Same 22 MB,
  0.94× warm latency, 1.66× faster cold start, and 1.35× *slower* on the
  batch-16 create path (15.6 vs 11.6 ms/text). On quality it lands within the
  L6 model's own run-to-run band (0.914 prefixed / 0.903 raw vs L6's 0.907;
  ranges overlap heavily), so it is a lateral move on the axis that matters —
  and the prefixed number is the one that needs query-side plumbing EngramDB
  does not have, plus the asymmetry that documents must *not* get the prefix.
  Nothing here justifies the surface area, and L12-q is 0.04 MRR ahead of it.
- **bge-small-q's cost is now explained.** Its 5.74× warm latency on identical
  inputs confirms the earlier 6.8× was real, not a chunking artifact — and the
  cache shows why the "-Q" model is 63 MB: `Qdrant/bge-small-en-v1.5-onnx-Q`
  ships `model_optimized.onnx`, which is not an int8 export at all.
- **nomic-q is reproducibly bad here** (best cell MRR 0.642), matching E2.

### R2 · Determinism — int8 rankings drift run-to-run; L12-q drifts less

Six back-to-back runs, same host, same sorted/deduped batches (the harness
already controls batch composition), production cell, MRR@10:

| Model | 1 | 2 | 3 | 4 | 5 | 6 | spread |
|---|---|---|---|---|---|---|---|
| `minilm-q` | 0.875 | 0.920 | 0.920 | 0.908 | 0.909 | 0.909 | 0.045 |
| `arctic-xs-q` (raw) | 0.913 | 0.903 | 0.896 | 0.903 | 0.906 | 0.898 | 0.017 |
| `minilm-l12-q` | 0.958 | 0.958 | 0.958 | 0.955 | 0.938 | 0.948 | 0.020 |

The nondeterminism is ONNX Runtime's handling of the dynamically quantized
kernels; it becomes visible here because near-ties in the ranking flip whole
queries. **R6 shows it is larger and more consequential than "ranking noise"** —
read that section before treating this table as a measurement caveat.

An earlier draft of this document read the first three L12 runs (identical at
0.958) as "bit-stable". That was wrong — extending to n=6, and running the same
model through a second harness with different batch sizes
(`examples/rerank_bench.rs`, whose bi-encoder stage spans 0.927–0.948 MRR over
5 runs), shows L12 drifts too. What survives is the weaker, still-useful claim:
**L12's spread is about half L6's (0.020 vs 0.045) and the two models' ranges
do not overlap.** Stability is a property of the harness's batching as much as
the model, so it is not a decision criterion on its own here — the
non-overlapping quality ranges are.

This is also why this report's `minilm-q` figures differ from
embedding-analysis.md's (0.894 MRR there) by more than that document's stated
±0.02 caveat.

### R3 · Reranker — `jina-reranker-v1-turbo-en` dominates; the old default was a no-op at the shipped weight

Measured by `examples/rerank_bench.rs` through the production seams: candidates
from the real bi-encoder path, documents composed by the same rule as
`retrieval::engine::rerank_document`, scoring through the real `LocalReranker`,
and the blend from `apply_rerank` — `(1 - w)·base + w·sigmoid(logit)` — at the
shipped `top_n = 20`, `weight = 0.5`.

| Reranker | disk | load ms | ms/pair | ms/query @ top-20 | P@1 | R@5 | MRR@10 | nDCG@10 |
|---|---|---|---|---|---|---|---|---|
| *(no rerank — bi-encoder only)* | — | — | — | — | 0.917 | 0.951 | 0.949 | 0.936 |
| `bge-reranker-base` (**previous default**) | 1060 MB | 6189 | 278 | 5568 | 0.917 | 0.951 | 0.949 | 0.936 |
| **`jina-reranker-v1-turbo-en`** (**new default**) | **145 MB** | **552** | **82** | **1647** | **0.938** | 0.951 | **0.959** | **0.946** |

**bge-reranker-base reproduces the un-reranked ranking exactly** — all four
metrics identical to the baseline. It is not that it fails to load: at
`weight = 1.0` it does reorder, and helpfully (0.896 / 0.927 / 0.923 / 0.917 vs
that run's 0.875 / 0.910 / 0.917 / 0.909 baseline). The cause is calibration:
BGE emits large positive logits, so `sigmoid(logit) ≈ 1` for every candidate in
a top-20 that already contains the answer, and a half-weight blend with a
constant preserves the base order. jina-turbo's logits stay in the informative
range and it improves at both weights.

So the old default cost ~5.6 s of cross-encoder work per query to return the
ranking the bi-encoder had already produced. At `rerank.top_n = 50` (the
shipped value, not the 20 used here) that is ~14 s/query for nothing; the new
default's ~4 s at top-50 at least buys the gain. The 1 GB download is also a
plausible reason reranking stays off in practice. (4-core sandbox absolutes;
the ratios are what transfers.)

Both the fastembed path and the **tract** path now support the new default:
`tract_spec` in `rerank.rs` carries the two fp32 exports that load under tract
(`jinaai/jina-reranker-v1-turbo-en` ~145 MB and the historical
`Xenova/bge-reranker-base` ~1.1 GB), and `tract_reranker_orders_relevant_first`
asserts the ordering property for each, so an Intel-Mac build gets the smaller
model too rather than losing reranking.

### R4 · Static (model2vec) embeddings — real speed, real quality cost

Static models have no neural forward pass at all: embedding is a token→vector
table lookup plus mean pooling. Same corpus, same compositions, Python harness,
single-threaded (so the MiniLM control's ms/text is pessimistic — the Rust
number for the same model is 11.6 ms/text at batch 16).

| Model | dim | disk | load ms | ms/text | P@1 | R@5 | MRR@10 | nDCG@10 | paraphrase MRR |
|---|---|---|---|---|---|---|---|---|---|
| `minilm-q` (control) | 384 | 22 MB | 124 | 51.7 | 0.896 | 0.931 | 0.931 | 0.925 | 0.66 |
| `potion-retrieval-32M` | 512 | 124 MB | 240 | 0.115 | 0.792 | 0.892 | 0.865 | 0.862 | **0.28** |
| `potion-base-8M` | 256 | 29 MB | 59 | 0.141 | 0.771 | 0.868 | 0.831 | 0.848 | **0.26** |

- The speed claim holds and then some: **~450× fewer ms/text** in-harness, and
  no ONNX Runtime in the process at all.
- The quality cost is concentrated exactly where a bag-of-static-vectors model
  should fail: **paraphrase queries collapse** (0.66 → 0.28 MRR) while keyword
  and title_echo stay at 1.00. Overall −0.066 MRR for potion-retrieval-32M,
  −0.100 for potion-base-8M — both above the noise floor.
- "Smaller" is only true for `potion-base-8M` (29 MB). `potion-retrieval-32M`
  is **124 MB on disk**, larger than the current default, because a static
  model is its whole vocabulary matrix (63 091 × 512 × f32).
- No Rust integration exists today — this would mean a `model2vec-rs`
  dependency, a new `EmbeddingProvider` impl, and a new `model_id()`.

Where this *would* pay: the **tract / Intel-Mac** build, which today runs fp32
MiniLM at ~3× ONNX latency with no NLI and no T5. A static provider there would
be faster than the ONNX default on a platform that currently has the slowest
path, and it needs no execution provider at all. Filed as an option, not a
recommendation.

### R5 · NLI and title models — no better candidate found

- **NLI** (`nli-deberta-v3-xsmall` int8, 83 MB, off by default) is already the
  smallest credible 3-class NLI cross-encoder with the required
  `{contradiction, entailment, neutral}` label order. fastembed exposes no NLI
  models, so any swap is raw-`ort` work as today. No change recommended.
- **T5 titling** (`t5-small` int8, 74 MB, **on by default**) is structurally the
  most expensive per-create model: `MAX_OUTPUT_TOKENS = 16` greedy steps means
  16 sequential decoder passes per memory, versus one forward pass for
  embedding. It is daemon-amortized and pooled, and the one-shot CLI already
  sidesteps it (`engramdb add` forces `keyword`). Not separately benchmarked
  here; if create latency ever becomes the complaint, the decode-step count and
  the `keyword` fallback are the levers, not a different model.

### R6 · int8 embeddings are not reproducible under CPU load — fp32 is, and thread pinning does not help

R2 treated run-to-run ranking drift as a measurement nuisance. Chasing an
intermittent failure of `embeddings::onnx::tests::test_embed_consistency`
showed it is much more than that. `examples/embed_determinism_probe.rs` embeds
one text 30 times through a single provider, on both the `embed()` and
`embed_batch()` paths (60 vectors), and counts how many are actually distinct.

With 8 background threads saturating a 4-core host:

| Model | intra_threads | 1-token text | short text | long text (realistic chunk) |
|---|---|---|---|---|
| `all-MiniLM-L6-v2-q` (int8) | 1 | 3 distinct, cos 0.33 | 2, cos 1.00 | **23 distinct, cos −0.03** |
| `all-MiniLM-L6-v2-q` (int8) | 2 | 1, cos 1.00 | 2, cos 1.00 | **23 distinct, cos 0.45** |
| `all-MiniLM-L6-v2-q` (int8) | 4 | 2, cos 0.98 | 4, cos 0.90 | **20 distinct, cos 0.33** |
| `all-MiniLM-L12-v2-q` (int8) | 1 | 7, cos 0.98 | 3, cos 0.98 | **45 distinct, cos 0.32** |
| `all-MiniLM-L12-v2-q` (int8) | 2 | 8, cos 0.29 | 8, cos 0.14 | **31 distinct, cos 0.46** |
| `all-MiniLM-L12-v2-q` (int8) | 4 | 6, cos 0.01 | 11, cos −0.03 | **43 distinct, cos −0.07** |
| **`all-MiniLM-L6-v2` (fp32)** | **1 / 2 / 4** | **1, cos 1.000000** | **1, cos 1.000000** | **1, cos 1.000000** |

Counts are distinct vectors out of 60; `cos` is the minimum pairwise cosine.
A cosine of −0.07 between two embeddings of the *same string* is not
round-off — the vectors are unrelated. At idle, everything except one L12 cell
is reproducible, so this is specifically a **contention-triggered, int8-only**
failure that gets worse with sequence length.

**Thread pinning does not fix it.** That was the leading hypothesis (the Python
mirror of this harness became bit-identical at `intra_op_num_threads = 1`), and
`fastembed`'s `InitOptions::with_intra_threads` makes it a one-line change — but
measured across 1, 2 and 4 intra-op threads the int8 models are broken at every
setting, and 1 thread is not even the best of them. fp32 is exact at all three.
The pattern (int8 only, load-triggered, worse with longer sequences, indifferent
to thread count) points at the quantized kernels' buffer handling rather than
reduction order, but this pass did not isolate it further.

**Why it matters.** Vectors are computed once at create time and persisted. A
memory written while the machine is busy — exactly when a coding agent is
running builds and tests — can be indexed with a vector unrelated to its text,
and nothing warns, because nothing compares it to anything afterwards.

**What shipped here.**

- The knob, without the default change: `ENGRAMDB_ONNX_INTRA_THREADS` now also
  applies to the `fastembed` embedding sessions (it previously only reached the
  directly-built NLI/T5 sessions) via `engram_onnx::intra_threads_override`.
  The *default* is deliberately left as ONNX Runtime's own, because pinning
  buys no determinism and `engram_onnx::intra_threads()` (cores/2) benchmarks
  **1.7× slower on the batch/create path** (39.3 vs 23.0 ms/text for L12-q).
- An escape hatch: `[embeddings].provider = "all-minilm-l12-fp32"` selects the
  bit-exact fp32 build of the default model. Costs ~4× the disk (128 MB) and
  ~1.3× the latency, and fingerprints distinctly so switching triggers a
  reindex.
- Honest tests. `test_embed_consistency` asserted 1e-6 element-wise equality on
  the int8 default; no cosine floor is safe there either, since the measured
  minimum reaches 0. It is now `test_embed_returns_usable_vector` (right shape,
  finite, non-zero — the wiring regression those tests actually guard), and the
  real determinism guard lives in `fp32_embedding_is_bit_exact`, on the model
  that provides it.

**Not resolved:** whether to move the default to fp32. That trades 96 MB and
~30% embed latency for correctness under load, and these numbers come from one
platform (x86_64 Linux, ORT 1.24.2 from the pyke prebuilt) — the int8 kernels
differ on Apple silicon, so the finding should be reproduced there before a
default changes. Run the probe on the target platform to decide.

## Caveats

- Same synthetic 60-memory / 48-query corpus as the earlier study, with the
  same limits: overall deltas below ~5 pts R@5 / 0.05 MRR are directional only.
  The L12-q MRR delta (+0.038 at the median) sits just under that floor on its
  own; what carries it is that it is **consistent across P@1 / MRR / nDCG,
  reproduced 3/3 with zero variance, and beats fp32 L6 as well**.
- Semantic-only for the embedding numbers: composite scoring (scope, recency,
  trust) is not in the loop and will shrink end-to-end deltas.
- All absolute latencies are from a 4-core Linux sandbox and are pessimistic;
  the ratios are what transfers. Only the static-embedding numbers come from
  the Python mirror; embedding and reranker results are from the Rust harnesses
  against production code paths.
- The reranker comparison is a paired test on one fixed bi-encoder candidate
  list. That makes the bge-vs-jina delta sound, but the *baseline* row is a
  single draw from a bi-encoder stage that itself spans 0.927–0.948 MRR over
  repeats — so read "jina beats no-rerank" as directional and "jina beats bge"
  as solid.
- `rerank_bench` uses `top_n = 20` against a 60-memory corpus; the shipped
  default is 50. Per-query latency scales linearly in `top_n`, quality does
  not.

## Reproducing

```bash
# stage models per CLAUDE.md's web-sandbox notes, then:
cargo run --release --example embed_matrix              # embedding quality
cargo run --release --example embed_model_bench         # embedding latency / footprint
cargo run --release --example rerank_bench              # reranker quality + cost
PROBE_LOAD_THREADS=8 \
  cargo run --release --example embed_determinism_probe # int8 reproducibility (R6)

EMBED_EVAL_MODELS=minilm-q,minilm-l12-q,arctic-xs-q cargo run --release --example embed_matrix
EMBED_BENCH_MODELS=minilm-q,minilm-l12-q EMBED_BENCH_ITERS=200 \
  cargo run --release --example embed_model_bench
RERANK_BENCH_WEIGHT=1.0 cargo run --release --example rerank_bench
```

## Upgrading an existing store

The embedding default change alters `model_id()` (`onnx/all-MiniLM-L6-v2-q` →
`onnx/all-MiniLM-L12-v2-q`), so every existing store's manifest fingerprint now
mismatches. Under the default `reindex_on_model_change = "warn"` this surfaces
as a warning naming the fix:

```bash
engramdb reindex --embeddings-only     # re-embed at the new default
engramdb reindex --embeddings-only --global
```

To stay on the old model instead — no reindex, no download — pin it:

```toml
[embeddings]
provider = "all-minilm-l6"
```

The reranker change needs nothing: rerank scores are computed per query and
never persisted.
