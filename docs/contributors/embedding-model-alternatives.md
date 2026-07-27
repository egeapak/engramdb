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
| 0 | **Per-model, per-action cost table** for tuning defaults — `examples/model_cost_matrix.rs` (new) | See R8. Headline: T5 titling costs **241 ms per create** against 0.12 ms for keyword; rerank at the shipped `top_n = 50` costs ~0.9 s/query even with the fast reranker. | — |
| 1 | **Reranker default → `jina-reranker-v1-turbo-en`** (from `bge-reranker-base`) — **applied** | 7.3× smaller (145 MB vs 1060 MB), 3.4× faster per pair (82 vs 278 ms), 11× faster to load. And at the shipped `weight = 0.5` it is the *only* one that helps at all: bge-reranker-base reproduces the un-reranked ranking exactly, so the old default bought nothing for ~5.6 s/query. | One-line config default. **Zero migration** — rerank scores are not persisted. |
| 2 | **Embedding default → `all-MiniLM-L12-v2-q`** (from `-L6-`) — **applied** | Wins **6/6 paired runs** with non-overlapping ranges: MRR@10 mean 0.953 (range 0.938–0.958) vs 0.907 (0.875–0.920); P@1 0.931 vs 0.868. Beats fp32 L6 too, so the gain is depth, not quantization precision. | +10 MB (22 → 32 MB), 2.0× warm embed latency (2.83 → 5.70 ms), one `reindex --embeddings-only`. Same 384 dims and 256-token window ⇒ no config/schema change. Pin the old model with `provider = "all-minilm-l6"`. |
| 3 | **Don't** switch to snowflake-arctic-embed (xs or s), on **either** the ONNX or the tract/Intel-Mac path | ONNX: arctic-xs-q is free on cost (22 MB, 0.94× warm latency) but lands at MRR 0.914 prefixed / 0.903 raw over 6 runs — indistinguishable from the L6 model it would replace (0.907), well below L12 (0.953). tract: arctic-xs-fp32 loads and matches L6-fp32 on cost exactly (86 MB, 894 vs 906 ms) but loses on quality (MRR 0.920 prefixed / 0.889 raw vs **0.930**, nDCG 0.904 vs 0.927) and needs query-prefix plumbing EngramDB does not have. See R7. | — |
| 4 | **Don't** switch to static (model2vec) embeddings as the default | 100–450× faster and ONNX-free, but costs 6.6 pts MRR and collapses paraphrase queries (MRR 0.66 → 0.28). Worth keeping on the shelf for the tract/no-ORT build, not for the default path. | — |
| 5 | Re-confirmed: **don't** switch to bge-small / nomic / mxbai / fp32 MiniLM | Reproduced E2 with cleaner instrumentation. bge-small-q is 5.7× slower for no reliable gain; nomic-q is catastrophic here (MRR 0.642 at best); fp32 MiniLM is 1.31× slower and *worse* than L12-q. | — |
| 6 | **Quantized embeddings were not reproducible under CPU load — root-caused to the pyke prebuilt ONNX Runtime, not the model** | Same model files, same CPU, same load: pyke 1.24.2/1.24.4 corrupt both int8 and uint8 (44/60 distinct vectors for one text); the **official 1.24.2 build — same version** — gives 1/40, cosine 1.000000 for both. | Mitigated: default moved to the uint8 export (also better quality). **Real fix is R9** — link the official runtime. |
| 7 | **Use a system ONNX Runtime** (originally via an `ort/pkg-config` build-time link; shipped instead as the run-time-loaded default, see below). This is the actual fix for R6 | Verified against Microsoft's official tarball *and* the real Homebrew bottle: both give 1/40 distinct, cosine 1.000000, for int8 **and** uint8. `ort` links them unchanged; the real `engramdb` binary builds and runs. Both MIT. | Gives up the single static binary (shared libs only). Homebrew additionally ships an **Intel-Mac** bottle — the platform the repo documents as having no ORT 1.24 prebuilt at all. See R9. |

## Current stack (what is actually loaded, and why)

| Role | Model | Size on disk | Default? | Why this one |
|------|-------|--------------|----------|--------------|
| Embedding | `all-MiniLM-L6-v2` **int8** | 22 MB | was **on**, now `all-minilm-l6` | Lever-B A/B: 1.4–1.9× faster and ~4× smaller than fp32 at cosine ≈ 0.99 |
| Embedding | **`all-MiniLM-L12-v2` uint8** (`Xenova/…`, `model_uint8.onnx`) | 32 MB | **on** (new) | R1 (depth) + R6 (quantization scheme) |
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

### R1 · Embedding models — the 12-layer MiniLM wins

Quality at the production cell (`fieldvec_c256`, max agg). The three
shortlisted models were run **6 times each**; they report the mean and the
full observed range. Others are a single run.

| Model | dim | P@1 | MRR@10 | MRR range (n=6) | R@5 | nDCG@10 |
|---|---|---|---|---|---|---|
| `minilm-q` (L6, int8) | 384 | 0.868 | 0.907 | 0.875 – 0.920 | 0.920 | 0.910 |
| **`minilm-l12-q`** (L12, int8 — superseded by the uint8 build, R6) | 384 | **0.931** | **0.953** | **0.938 – 0.958** | 0.913 | 0.933 |
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

### R6 · Quantized embeddings were not reproducible — root cause and fix

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

**It is a known ONNX Runtime bug in the *signed*-int8 path, and there is a fix.**
The bisect (`crates/engram-models/examples/int8_determinism_bisect.rs`) first
ruled out everything above the runtime: EngramDB's provider, `fastembed`,
tokenization and pooling all drop out, because a raw `ort::Session` fed an input
tensor **tokenized once and reused verbatim** still returns different vectors
(38/40 distinct for int8, 1/40 for fp32). No session option helps — sweeping
`intra_threads`, `GraphOptimizationLevel::Disable`, `memory_pattern=false` and
ORT's own `deterministic_compute` leaves every int8 cell broken.

Corruption tracks **preemption**: holding ORT to one thread and varying only the
competing threads on a 4-core host gives 1/20 distinct at 0–1 load threads, 2–4
at 4, and 10–13 at 16. Giving ORT a dedicated core while other processes
saturate the machine is nearly clean; sharing a core with the load is worst.

That looked like
[onnxruntime#6004](https://github.com/microsoft/onnxruntime/issues/6004) —
signed-INT8 quantized models reading uninitialized memory (valgrind-confirmed),
with UINT8 reported unaffected — and our exports were exactly the reported
shape: `DynamicQuantizeLinear` + `MatMulInteger` over signed INT8 initializers.
Switching to the uint8 export (`Xenova/all-MiniLM-{L6,L12}-v2` already publish
`onnx/model_uint8.onnx` at the same size) did fix reproducibility, and uint8
also *ranks better*:

| Export | distinct / 30 | min pairwise cosine | cosine vs fp32 | P@1 / MRR@10 / nDCG@10 |
|---|---|---|---|---|
| int8 `model_quantized.onnx` | 26–27 | −0.03 | 0.03–0.23 | 0.917 / 0.948 / 0.937 |
| **uint8 `model_uint8.onnx`** | **1** | **1.000000** | 0.977 | **0.958 / 0.969 / 0.953** |
| int8 + `reduce_range=True` | 24–25 | 0.05 | 0.18 | — |
| fp32 (control) | 1 | 1.000000 | 1.000 | 0.938 / 0.958 / 0.941 |

**But the export was not the real cause. The linked ONNX Runtime build was.**
Swapping only the runtime — same model files, same CPU, same 16-thread load —
gives:

| Linked ONNX Runtime | int8 | uint8 |
|---|---|---|
| **pyke prebuilt 1.24.2** (what `ort`'s `download-binaries` ships) | broken (44/60) | **broken (44/60)** |
| pyke prebuilt 1.24.4 (newest compatible with `ort` 2.0.0-rc.12) | broken | broken |
| Python wheel 1.28.0 | broken | clean |
| **official release tarball 1.24.2** — *the same version as the broken pyke build* | **1/40, cosine 1.000000** | **1/40, cosine 1.000000** |

With the official build of the **identical version**, *both* quantization
schemes are bit-reproducible at intra_threads 1 and 4, replicated across runs.
So this is not an ONNX Runtime version bug and not fundamentally a signed-vs-
unsigned issue — it is the **pyke prebuilt static library**, which on this
AVX-512/AMX host executes quantized graphs incorrectly. #6004's
uninitialized-memory defect is still the plausible underlying mechanism; the
pyke build is what exposes it.

Two consequences:

- **The fix is the runtime, not the model.** Embedding the official ONNX
  Runtime removes the problem for every quantized export at once. See R9 for the
  packaging assessment.
- **uint8 stays the default anyway**, on quality: 0.969 vs 0.948 MRR@10 at the
  same 32 MB, measured on a correct runtime. It is no longer load-bearing for
  correctness.

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
  bit-exact fp32 build of the default model. Measured cost against the int8
  default: **127 MB vs 32 MB** on disk, **7.89 vs 5.55 ms** warm per query
  (1.42×), **45.4 vs 23.0 ms/text** on the batch/create path (1.97×), and
  **365 vs 151 ms** cold start (2.4×). It fingerprints distinctly, so switching
  triggers a reindex.
- Honest tests. `test_embed_consistency` asserted 1e-6 element-wise equality on
  the int8 default; no cosine floor is safe there either, since the measured
  minimum reaches 0. It is now `test_embed_returns_usable_vector` (right shape,
  finite, non-zero — the wiring regression those tests actually guard), and the
  real determinism guard lives in `fp32_embedding_is_bit_exact`, on the model
  that provides it.

**Not resolved:** whether to move the default to fp32. On the evidence here
that is the only thing that actually fixes it — fp32 was exact in 100% of cells,
across both ORT builds, every session option, and every preemption level — but
it costs +95 MB of download, 1.4× query latency and 2.0× create latency
(measured, see the escape-hatch numbers above; an earlier draft of this document
under-stated the create-path cost as ~30%).

The blocker on deciding is **scope**: every number above comes from one host
(x86_64 Linux, Intel Xeon with AMX, inside a VM). Apple silicon has no
AMX-INT8 and takes a different MLAS path, and that is where most EngramDB
instances run. If the probe is clean there, int8 stays the right default and
this is a Linux/AMX caveat to document; if it reproduces, fp32 (or a non-AMX
build of ORT) becomes the correct default. Run:

```bash
PROBE_LOAD_THREADS=8 cargo run --release --example embed_determinism_probe
cargo run --release -p engram-models --example int8_determinism_bisect
```

Worth reporting upstream to `onnxruntime` either way — a byte-identical input
tensor returning different results is a runtime bug, not a quantization
trade-off.

### R7 · The tract / Intel-Mac path — arctic evaluated, and the L12 default walked back

The Intel-Mac build has no prebuilt ONNX Runtime, so it runs `tract`, which is
fp32-only. Two things make it a different decision from the ONNX default:
`tract` builds a runnable for the **fixed `[1, max_tokens]` shape** and pads
every input to it, so a one-line query costs a full 256-token forward pass; and
there is no int8 option at all.

Measured on the tract engine (`examples/tract_model_bench.rs`, new), all fp32:

| Model | disk | cold ms | warm mean | ms/text (batch 8) | quality: P@1 / MRR@10 / nDCG@10 |
|---|---|---|---|---|---|
| `all-MiniLM-L12-v2-fp32` | 127 MB | 2359 | 1843 ms | 1862 | ≈ 0.95 MRR |
| **`all-MiniLM-L6-v2-fp32`** | **86 MB** | 1159 | **906 ms** | 942 | **0.896 / 0.930 / 0.927** |
| `arctic-embed-xs-fp32` (+ prefix) | 86 MB | 1196 | 894 ms | 944 | 0.896 / 0.920 / 0.904 |
| `arctic-embed-xs-fp32` (raw) | 86 MB | 1196 | 894 ms | 944 | 0.833 / 0.889 / 0.889 |

Two findings:

**arctic-xs does load under tract, and is a genuine cost-peer of L6-fp32** —
identical 86 MB, 894 vs 906 ms. But it loses on quality even in fp32 (MRR 0.920
prefixed, 0.889 raw, vs L6's 0.930; nDCG 0.904 vs 0.927), and the prefixed
number needs query-side plumbing that does not exist. Same verdict as the ONNX
path, for the same reason: it is a cost-peer, not a quality win.

*Caveat on the first measurement:* at arctic's native 512-token window it
benchmarked 1.9× slower than the MiniLM specs. That was the fixed-shape padding,
not the model — on tract, `max_tokens` is a pure cost multiplier. `TRACT_ARCTIC_XS`
therefore declares 256, matching the chunk budget, and the numbers above are at
equal shape.

**The tract default is deliberately *not* the ONNX default.** Following the ONNX
switch to L12 would have cost Intel Macs 127 MB and 1843 ms per embed instead of
86 MB and 906 ms — a 2× regression on the one platform with no ONNX Runtime —
to buy about +0.02 MRR. `DEFAULT_TRACT_EMBEDDING` stays `all-MiniLM-L6-v2-fp32`;
`tract_default_is_dimension_compatible_with_onnx_default` pins the invariant that
actually matters (same 384 dims, same chunk budget) and lets model identity
differ. `provider = "all-minilm-l12"` selects the deeper model on tract for
anyone who wants it.

For scale, note tract is far slower than the "~3× ONNX" the user docs claim:
906 ms vs ~4 ms for the same fp32 model on ONNX Runtime, because ONNX runs the
true sequence length while tract pads to 256. That is pre-existing behavior, not
a regression, but it is the reason depth is expensive there.

### R8 · Per-model, per-action cost — the table for tuning defaults

`examples/model_cost_matrix.rs` measures every model against every action it
performs, on one host, with weights already cached. Read `per_unit` *within* a
row: "embed 1 query" uses a real (short) query, "embed batch of 16" uses real
256-token chunks, so the batch row's higher per-text cost is sequence length,
not batching overhead.

| Role | Model | Action | mean ms | p95 ms | per unit |
|---|---|---|---|---|---|
| embedding | **L12-u8 (default)** | load (cold session) | 220 | — | — |
| embedding | **L12-u8 (default)** | embed 1 query | **6.1** | 7.7 | 6.1 / text |
| embedding | **L12-u8 (default)** | embed batch of 16 chunks | 428 | 587 | 26.7 / text |
| embedding | L6-u8 | embed 1 query | 3.1 | 4.4 | 3.1 / text |
| embedding | L6-u8 | embed batch of 16 | 223 | 300 | 13.9 / text |
| embedding | L12-int8 | embed 1 query | 6.5 | 8.7 | 6.5 / text |
| embedding | L12-fp32 | embed 1 query | 8.2 | 11.1 | 8.2 / text |
| embedding | L12-fp32 | embed batch of 16 | 732 | 776 | 45.8 / text |
| embedding | arctic-xs-int8 | embed 1 query | 3.5 | 5.1 | 3.5 / text |
| reranker | **jina-turbo (default)** | load (cold) | 382 | — | — |
| reranker | **jina-turbo (default)** | score 1 pair | **18.7** | 21.5 | 18.7 / pair |
| reranker | bge-reranker-base | load (cold) | 5206 | — | — |
| reranker | bge-reranker-base | score 1 pair | 93.9 | 107 | 93.9 / pair |
| nli | deberta-xsmall-q (default) | load (cold) | 867 | — | — |
| nli | deberta-xsmall-q (default) | 1 premise/hypothesis pair | **173** | 253 | 173 / pair |
| nli | deberta-xsmall-fp32 | 1 pair | 226 | 321 | 226 / pair |
| title | **t5-small-q (default)** | load (cold) | 270 | — | — |
| title | **t5-small-q (default)** | generate 1 title | **241** | 312 | 241 / title |
| title | t5-small-fp32 | generate 1 title | 299 | 318 | 299 / title |
| title | keyword (RAKE, no model) | generate 1 title | **0.12** | 0.30 | 0.12 / title |

Reading it against the shipped config:

- **A query costs ~6 ms of embedding.** Everything else on the query path is
  opt-in: rerank at the default `top_n = 50` adds **~0.9 s** with jina-turbo
  (and would have added ~4.7 s with the old bge default). If reranking is ever
  turned on by default, `top_n` should come down with it — at 20 it is ~370 ms.
- **A create is dominated by titling, not embedding.** A 3-chunk memory embeds
  in ~80 ms; T5 titling adds **241 ms** on top, ~2000× the keyword strategy's
  0.12 ms. T5 is the single most expensive default in the stack. It is
  daemon-amortized for *load* (270 ms once) but the 241 ms is per memory, every
  memory. `engramdb add` already forces keyword; whether MCP `create` should is
  a live question this table is meant to inform.
- **NLI at 173 ms/pair × `max_comparisons = 10`** is up to ~1.7 s per challenge —
  which is why it is off by default.
- Quantization buys less than expected on the query path (6.1 ms uint8 vs 8.2 ms
  fp32, 1.3×) but a lot on the batch path (26.7 vs 45.8 ms/text, 1.7×), so the
  fp32 fallback is more affordable for query-heavy than for write-heavy use.

### R9 · Embedding the official ONNX Runtime — feasibility

R6 shows the fault is the **pyke prebuilt** that `ort`'s `download-binaries`
feature fetches, not the ONNX Runtime version and not the export. Replacing it
with Microsoft's own build fixes every quantized model at once. Verified here
end to end:

```bash
curl -LO https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/onnxruntime-linux-x64-1.24.2.tgz
tar -xzf onnxruntime-linux-x64-1.24.2.tgz
export ORT_STRATEGY=system ORT_PREFER_DYNAMIC_LINK=1 \
       ORT_LIB_LOCATION="$PWD/onnxruntime-linux-x64-1.24.2/lib"
cargo build --release --bin engramdb
LD_LIBRARY_PATH="$ORT_LIB_LOCATION" ./target/release/engramdb init   # add / query all work
```

**What works.** `ort` 2.0.0-rc.12 links against it unchanged — no code change,
no crate upgrade. The real `engramdb` binary builds and `init` / `add` /
`query` (semantic) all run. ONNX Runtime is **MIT-licensed**, so redistribution
is fine.

**What it costs.**

- **No more single static binary.** The official release tarballs ship *only*
  shared libraries (`libonnxruntime.so` / `.dylib` / `.dll`) — there is no
  `libonnxruntime.a` to link. The binary gains a runtime dependency
  (`ldd` → `libonnxruntime.so.1`) and refuses to start without it, so a release
  has to ship a ~22 MB shared library beside the executable and set an rpath
  (`$ORIGIN` on Linux, `@loader_path` on macOS) or place the DLL next to the
  `.exe` on Windows. `ort` has a `copy-dylibs` feature that does the copying.
- **~22 MB per platform** added to the release artifacts (vs the static build
  folding into the binary).

**Platform coverage.** Official builds exist for `linux-x64`,
`linux-aarch64`, `osx-arm64`, `win-x64`, `win-arm64` — everything EngramDB
targets **except Intel Mac**. `onnxruntime-osx-universal2` was published up to
**1.22.0** and 404s from 1.24.0 onward, and 1.22 predates the API 24 that
`ort` 2.0.0-rc.12 requires. So this does not retire the tract fallback: Intel
Mac keeps the pure-Rust fp32 path (R7), which is unaffected by this bug anyway
since tract is not ONNX Runtime.

**Externalizing it to a package manager (`brew install onnxruntime`) is the
cleaner form of the same fix**, and it is wired up: the workspace now has a
a `pkg-config` build-time link (`ort/pkg-config`), so the build links
whatever ONNX Runtime the system already has.

```bash
brew install onnxruntime
cargo build --release -p engram-cli --features system-onnxruntime   # historical; see below
```

Verified end to end here against the **real Homebrew bottle** (pulled from
ghcr.io and linked through its own `lib/pkgconfig/libonnxruntime.pc`, with
`@@HOMEBREW_CELLAR@@` substituted the way `brew` does at install time):

- `ort-sys` probes pkg-config *before* its download path, so this wins even
  though `fastembed` also enables `download-binaries`. It accepts any runtime
  with minor version ≥ 24; Homebrew ships **1.28.0**.
- **Homebrew's build is not affected by the corruption.** Same probe, same
  16-thread load: `1/40` distinct, cosine `1.000000`, for *both* int8 and uint8
  at intra_threads 1 and 4. Only the pyke prebuilt misbehaves.
- **Homebrew has an Intel-Mac bottle** (`amd64/darwin`), alongside
  arm64 macOS 14/15/26, and x86_64/arm64 Linux. That is the platform
  `crates/engram-onnx/Cargo.toml` currently documents as having *no* ORT 1.24
  prebuilt anywhere — and 1.28 satisfies the API 24 `ort` needs. So brew is
  also a route to a real ONNX path on Intel Mac, not just tract.
- Caveat: Homebrew's `libonnxruntime.so` links against its own `abseil`, `onnx`,
  `protobuf` and `re2` formulae, so it is not a standalone library you can copy
  into a tarball release. Under `brew` that is invisible (dependencies are
  declared and installed); for a self-contained archive, Microsoft's official
  tarball is the better source since it has no such dependencies.

**This is now the shipped default.** `bundled-onnxruntime` — downloading and
statically linking the pyke prebuilt — is opt-in and no longer used by anything;
the default `load-dynamic` strategy `dlopen`s a runtime at startup, release
archives ship no runtime at all, and the Homebrew
formula (`packaging/homebrew/engramdb.rb`) carries `depends_on "onnxruntime"`
and builds with default features. Scoop has no `onnxruntime` package at all,
so `packaging/scoop/onnxruntime.json` supplies one. See `packaging/README.md`.

The `pkg-config` link was prototyped as a `system-onnxruntime` feature and then
**dropped**, because it records `libonnxruntime` as a load-time dependency: the
dynamic loader resolves it before `main()` runs, so a missing runtime stops the
binary from starting at all — measured as `error while loading shared libraries`
on `engramdb --version`, with no `doctor` output and no opportunity for the
pre-flight probe. Run-time loading gets the same correct runtime with a failure
mode that can actually be reported. Only `load-dynamic` (default) and
`bundled-onnxruntime` remain.

The one real cost of moving off a statically linked runtime: `ort` panics when
the dylib is absent, and `panic = "abort"` in the release profile makes that
uncatchable, so a missing runtime would abort the process rather than fall back
to keyword search. `engram_onnx::runtime` closes that hole by locating and
validating the library (including an `OrtGetApiBase()->GetApi(24)` version
check) before `ort` is ever called.

**If a single static binary matters more than any of this**, the remaining
option is building ONNX Runtime from source with static libs in CI
(`ORT_LIB_LOCATION` accepts a directory containing `libonnxruntime.a`), or
reporting the defect to pyke. Both are larger projects.

**Recommendation.** Adopt it for the platforms that have official builds. It
is the only change that makes *all* quantized exports correct rather than
trading model quality for reproducibility, and it costs packaging work rather
than latency or accuracy. Until it lands, the shipped uint8 default is chosen
for quality (R1/R6) and `provider = "all-minilm-l12-fp32"` remains the
correctness-first option on a stock build.

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
cargo run --release --example model_cost_matrix         # every model x every action (R8)
cargo run --release --features tract --example tract_model_bench
PROBE_LOAD_THREADS=16 \
  cargo run --release --example embed_determinism_probe # reproducibility (R6)
cargo run --release -p engram-models --example int8_determinism_bisect

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
