# Model-stack alternatives: smaller / faster / better candidates

*2026-07-26 · benchmark assets: `examples/embed_matrix.rs`, `examples/embed_model_bench.rs`, corpus `examples/data/embed_eval.json`*

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

| # | Change | Evidence | Cost |
|---|--------|----------|------|
| 1 | **Switch the reranker default to `jina-reranker-v1-turbo-en`** (from `bge-reranker-base`) | 7.3× smaller (145 MB vs 1060 MB), 6.2× faster per pair (80 vs 494 ms), 28× faster to load, and *slightly better* quality (nDCG@10 0.910 vs 0.904). Strictly dominant on every axis measured. | One-line config default. **Zero migration** — rerank scores are not persisted, and the name is already accepted by `resolve_reranker_model`. |
| 2 | **Switch the embedding default to `all-MiniLM-L12-v2-q`** | Best model on the corpus: MRR@10 0.958 vs 0.920, P@1 0.938 vs 0.875, nDCG 0.942 vs 0.915 — and **bit-stable across repeats** where the current L6 default swings ±0.023 MRR run-to-run on identical input. | +10 MB (22 → 32 MB), 2.0× warm embed latency (2.83 → 5.70 ms), one `reindex --embeddings-only`. Same 384 dims, same 256-token window ⇒ no config/schema change. |
| 3 | **Don't** switch to snowflake-arctic-embed (xs or s) | arctic-xs-q is genuinely free on cost (same 22 MB, 0.94× warm latency, 1.6× faster cold start) but only reaches parity on quality, and *only with* a query instruction prefix EngramDB does not currently emit. arctic-s-q is worse than arctic-xs-q at 2.2× the latency. | — |
| 4 | **Don't** switch to static (model2vec) embeddings as the default | 100–450× faster and ONNX-free, but costs 6.6 pts MRR and collapses paraphrase queries (MRR 0.66 → 0.28). Worth keeping on the shelf for the tract/no-ORT build, not for the default path. | — |
| 5 | Re-confirmed: **don't** switch to bge-small / nomic / mxbai / fp32 MiniLM | Reproduced E2 with cleaner instrumentation. bge-small-q is 5.7× slower for no reliable gain; nomic-q is catastrophic here (MRR 0.642 at best); fp32 MiniLM is 1.31× slower and *worse* than L12-q. | — |
| 6 | Note, not a change: **the current int8 L6 default is run-to-run nondeterministic** | Same host, same sorted inputs, three runs: MRR@10 0.875 / 0.920 / 0.920 (and 0.848 / 0.911 in earlier runs). L12-q returned 0.958 three times out of three. | Explains part of the drift between this report's minilm-q numbers and embedding-analysis.md's. |

## Current stack (what is actually loaded, and why)

| Role | Model | Size on disk | Default? | Why this one |
|------|-------|--------------|----------|--------------|
| Embedding | `all-MiniLM-L6-v2` **int8** (`Xenova/all-MiniLM-L6-v2`) | 22 MB | **on** | Lever-B A/B: 1.4–1.9× faster and ~4× smaller than fp32 at cosine ≈ 0.99 |
| Embedding (tract) | `all-MiniLM-L6-v2` **fp32** | 86 MB | Intel Mac only | No native ORT; int8 has no tract build |
| Reranker | `bge-reranker-base` fp32 (`BAAI/bge-reranker-base`) | **1060 MB** | off | Historical default; the fp32 export is the only one that loads under tract |
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
- **Repeats** — the three shortlisted models were re-run 3× to separate real
  deltas from int8 nondeterminism. This turned out to matter (recommendation 6).

Static-embedding and reranker candidates were evaluated in a Python mirror of
the same harness (`chunk_text` incl. runt rebalance, same compositions, same
max aggregation, same P@1 / R@5 / MRR@10 / nDCG@10 definitions). Its MiniLM
control reproduces the Rust harness's *effect sizes* exactly — base → fieldvec
is +0.148 MRR in both — so relative comparisons carry over; absolute values sit
a few points high because pooling/normalization details differ.

**Noise discipline** is inherited from the earlier study: with n=48 queries,
overall deltas below ~5 pts R@5 / 0.05 MRR are noise. The measured run-to-run
spread of the int8 L6 default (±0.023 MRR) is consistent with that floor.

## Results

### R1 · Embedding models — `all-MiniLM-L12-v2-q` wins

Quality at the production cell (`fieldvec_c256`, max agg). Shortlisted models
show the median of 3 repeats and the observed spread; others are a single run.

| Model | dim | P@1 | R@5 | MRR@10 | nDCG@10 | MRR spread over repeats |
|---|---|---|---|---|---|---|
| `minilm-q` (**today's default**) | 384 | 0.875 | 0.920 | 0.920 | 0.915 | 0.875 – 0.920 |
| `minilm-fp32` | 384 | 0.896 | 0.931 | 0.930 | 0.927 | — |
| **`minilm-l12-q`** | 384 | **0.938** | 0.920 | **0.958** | **0.942** | **0.958 / 0.958 / 0.958** |
| `arctic-xs-q` (raw) | 384 | 0.875 | 0.920 | 0.903 | 0.899 | 0.896 – 0.913 |
| `arctic-xs-q` (+ query prefix) | 384 | 0.896 | 0.899 | 0.918 | 0.909 | 0.911 – 0.927 |
| `arctic-s-q` (+ query prefix) | 384 | 0.896 | 0.889 | 0.913 | 0.892 | — |
| `bge-small-q` (+ query prefix) | 384 | 0.896 | 0.948 | 0.924 | 0.919 | — |
| `nomic-q` (+ prefixes) | 768 | 0.625 | 0.674 | 0.642 | 0.676 | — |

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

- **L12-q is the only model that beats the default on quality by more than the
  noise floor** (+0.038 MRR at the median, +0.083 against the default's worst
  draw; +0.063 P@1; +0.027 nDCG), and it beats fp32 L6 too — so the gain is
  depth, not precision. It costs 2.02× warm latency, but in absolute terms that
  is **2.83 → 5.70 ms** on the query path and 11.6 → 21.9 ms/text on the create
  path, which the async ingest already defers. Cold start actually *improves*
  (171 → 146 ms), which matters for the daemon-less CLI.
- **arctic-xs-q is the "free" candidate that isn't worth taking.** Same 22 MB,
  0.94× warm latency, 1.66× faster cold start — but its quality only reaches
  parity, and only with the instruction prefix. Prefixes are the caller's job
  in EngramDB (fastembed adds none), so adopting it means adding query-side
  prefix plumbing *and* the asymmetry that documents must not get it. Parity is
  not worth new surface area.
- **bge-small-q's cost is now explained.** Its 5.74× warm latency on identical
  inputs confirms the earlier 6.8× was real, not a chunking artifact — and the
  cache shows why the "-Q" model is 63 MB: `Qdrant/bge-small-en-v1.5-onnx-Q`
  ships `model_optimized.onnx`, which is not an int8 export at all.
- **nomic-q is reproducibly bad here** (best cell MRR 0.642), matching E2.

### R2 · Determinism — the current default is not stable, L12-q is

Three back-to-back runs, same host, same sorted/deduped batches (the harness
already controls batch composition), production cell:

| Model | run 1 | run 2 | run 3 |
|---|---|---|---|
| `minilm-q` MRR@10 | 0.875 | 0.920 | 0.920 |
| `arctic-xs-q` MRR@10 (raw) | 0.913 | 0.903 | 0.896 |
| `minilm-l12-q` MRR@10 | 0.958 | 0.958 | 0.958 |

The residual nondeterminism is ONNX Runtime's multi-threaded int8 reduction
order; it only becomes visible because near-ties in the ranking flip whole
queries. The practical reading is that **the 6-layer int8 model produces score
margins narrow enough for numerical noise to reorder results**, and the
12-layer one does not. This is a quality property in its own right, and it is
also why this report's `minilm-q` figures differ from embedding-analysis.md's
(0.894 MRR there) by more than that document's stated ±0.02 caveat.

### R3 · Reranker — `jina-reranker-v1-turbo-en` strictly dominates the default

Production pipeline mirrored: bi-encoder ranking → top-20 re-scored by the
cross-encoder → blended `(1 - w)·base + w·sigmoid(logit)` at the default
`weight = 0.5`, document text per `rerank_document`.

| Reranker | disk | load ms | ms/pair | ms/query @ top-20 | P@1 | R@5 | MRR@10 | nDCG@10 |
|---|---|---|---|---|---|---|---|---|
| *(no rerank — bi-encoder only)* | — | — | — | — | 0.812 | 0.868 | 0.854 | 0.860 |
| `bge-reranker-base` (**today's default**) | 1060 MB | 10023 | 494 | 9875 | 0.875 | 0.889 | 0.915 | 0.904 |
| **`jina-reranker-v1-turbo-en`** | **145 MB** | **354** | **80** | **1604** | **0.896** | **0.906** | **0.920** | **0.910** |

There is no axis on which the current default wins. The size difference is the
important part: a **1 GB** first-run download is a plausible reason reranking
stays off in practice, and `rerank.top_n` defaults to **50**, which at 494
ms/pair is ~25 s of cross-encoder work per query — unusable. At 80 ms/pair the
same top-50 is ~4 s, and a top-20 is ~1.6 s. (These are 4-core sandbox
absolutes; the ratios are the transferable part.)

`resolve_reranker_model` already accepts the name, so this is a default change,
not a feature. Caveat: the **tract** path hard-codes `bge-reranker-base`
(`rerank.rs::tract_reranker`) because that fp32 export is the one verified to
load under tract; flipping the default means either verifying the jina export
under tract or keeping the tract branch pinned and documenting the split.

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

## Caveats

- Same synthetic 60-memory / 48-query corpus as the earlier study, with the
  same limits: overall deltas below ~5 pts R@5 / 0.05 MRR are directional only.
  The L12-q MRR delta (+0.038 at the median) sits just under that floor on its
  own; what carries it is that it is **consistent across P@1 / MRR / nDCG,
  reproduced 3/3 with zero variance, and beats fp32 L6 as well**.
- Semantic-only for the embedding numbers: composite scoring (scope, recency,
  trust) is not in the loop and will shrink end-to-end deltas.
- All absolute latencies are from a 4-core Linux sandbox and are pessimistic;
  the ratios are what transfers. Reranker and static-embedding numbers come
  from the Python mirror, not the Rust harness.
- The reranker comparison is a paired test on one fixed bi-encoder candidate
  list, so the bge-vs-jina delta is sound even though the underlying baseline
  draw was a single run.

## Reproducing

```bash
# stage models per CLAUDE.md's web-sandbox notes, then:
cargo run --release --example embed_matrix          # quality (all models)
cargo run --release --example embed_model_bench     # latency / footprint

EMBED_EVAL_MODELS=minilm-q,minilm-l12-q,arctic-xs-q cargo run --release --example embed_matrix
EMBED_BENCH_MODELS=minilm-q,minilm-l12-q EMBED_BENCH_ITERS=200 \
  cargo run --release --example embed_model_bench
```

Adopting recommendation 2 is a one-line change to `DEFAULT_ONNX_EMBEDDING`
(`crates/engram-models/src/embeddings/onnx.rs`) plus a docs pass; existing
stores detect the `model_id()` change through the manifest fingerprint and are
fixed by `engramdb reindex --embeddings-only`, exactly as the fp32→int8 switch
was. Recommendation 1 is a one-line change to `RerankConfig::default().model`
with no migration at all.
