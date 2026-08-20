# Is a 1024-dim embedding worth 8×? Measured on this project's own memories

*2026-08-20 · corpus + labels: `tools/eval-corpus/` · harness: `examples/embed_matrix.rs`*

The [turbovec evaluation](./turbovec-evaluation.md) R7 showed that quantization
recall improves with embedding width, which raised a fair question: if wider is
better, should the *default* be wider? EngramDB embeds at 384 dims, memory
creation is rare, and the [latency profile](./query-latency-profile.md) put the
embedding forward pass at only ~19% of a query — so an 8× more expensive model
might be affordable if it retrieves better.

It does not retrieve better. **Dimension is not the variable that matters.**

## TL;DR

| # | Finding |
|---|---------|
| 1 | **Two of the three 1024-dim models are *worse* than the 384-dim default.** `gte-large-q` loses 16.7 points of P@1. Width does not predict quality; the specific model does. |
| 2 | **The one that wins, wins slightly.** `bge-large-q` is +4.1 P@1 / +2.2 nDCG — about three queries out of 72 — for ~19× the disk and ~53× the warm query latency. |
| 3 | **`paraphrase` is the weakest archetype for every model (0.29–0.52 nDCG), and width barely moves it (+0.018 at best).** The semantic gap that matters is not a width problem, so the lever does not reach it. |
| 4 | **The layered small-then-large design does not pay off**, for a structural reason independent of these numbers — see [The layered proposal](#the-layered-proposal). |
| 5 | Incidental: the earlier "tag_only blind spot" was a **defect in the eval corpus, not the product**. With discriminative tags it scores 0.907 on the shipped default. |

## Method

157→161 memories generated mechanically from this repository's own commits and
contributor docs, so nothing being scored is hand-tuned; 72 queries authored
across seven archetypes with graded relevance. Generator, labels and validator
are in [`tools/eval-corpus/`](../../tools/eval-corpus/). The validator rejects
unknown ids, quota misses and — the check that matters — *leaky paraphrases*,
where a "paraphrase" reuses its target's wording and would therefore measure
lexical overlap rather than semantics. It reports zero.

Scoring is `embed_matrix`'s production-shaped path: metadata vector +
`chunk_text` content chunks, every vector searched independently, max
aggregation per memory — the same shape as `LanceIndex::vector_search`.

Run under the **debug** profile. Embeddings come from ONNX Runtime (native
code), so the Rust opt-level does not change a vector or any quality metric; it
changes only harness wall-clock, which is not what this document measures.
Latency figures quoted here come from the release-profile
[`embed_model_bench`](./query-latency-profile.md) run instead.

## Overall

161 memories, 72 queries, `fieldvec_c256`, max aggregation:

| model | dims | P@1 | R@5 | MRR@10 | nDCG@10 | warm p50 | ONNX on disk |
|---|---|---|---|---|---|---|---|
| **`all-MiniLM-L12-v2-u8`** *(shipped)* | 384 | 0.806 | 0.799 | 0.767 | 0.799 | **3.72 ms** | **34 MB** |
| `bge-large-en-v1.5-q` | 1024 | **0.847** | 0.794 | **0.793** | **0.821** | 199 ms | 638 MB |
| `mxbai-embed-large-v1-q` | 1024 | 0.764 | **0.812** | 0.768 | 0.794 | 29 ms | 337 MB |
| `gte-large-en-v1.5-q` | 1024 | 0.639 | 0.681 | 0.650 | 0.684 | 32 ms | 446 MB |

The spread *within* 1024 dims (0.639 → 0.847 P@1) is four times the gap between
the best 1024 model and the 384 default (0.806 → 0.847). Picking on width would
be picking on the wrong axis.

`bge-large-q` also deserves an asterisk: its "-Q" repo ships
`model_optimized.onnx`, which is optimized **fp32**, not a quantized export.
That is why it is 638 MB and 5–7× slower per text than the other two — it is
not really a peer of the quantized models it is tabled with.

## By archetype

nDCG@10. `best-384` is the best 1024 model minus the shipped default:

| archetype | n | minilm-384 | bge-1024 | mxbai-1024 | gte-1024 | best − 384 |
|---|---|---|---|---|---|---|
| `title_echo` | 8 | **0.952** | 0.867 | 0.917 | 0.933 | **−0.018** |
| `keyword` | 8 | 0.891 | 0.893 | 0.880 | 0.893 | +0.002 |
| `tag_only` | 8 | 0.907 | 0.932 | 0.953 | **0.970** | +0.063 |
| `natural` | 12 | 0.774 | **0.837** | 0.811 | 0.783 | +0.063 |
| **`paraphrase`** | 12 | **0.501** | **0.520** | 0.469 | 0.286 | **+0.018** |
| `buried_fact` | 12 | 0.771 | **0.826** | 0.727 | 0.469 | +0.055 |
| `distractor_trap` | 12 | 0.913 | **0.949** | 0.926 | 0.704 | +0.036 |

Three things fall out.

**`paraphrase` is the floor for every model, and width does not lift it.** At
0.501 the shipped default finds the right memory about half the time when the
query shares no vocabulary with it — and the best 1024 model manages 0.520.
That +0.018 is well inside noise at n=12 (one query is worth ~0.08). This is the
archetype a wider model was supposed to help, and it is the one it helps least.

**The default is the best model at `title_echo`** (0.952, beating all three
wider models). Whatever the wider models buy, it is not lexical-overlap
retrieval.

**`gte-large-q` collapses on exactly the hard archetypes** — 0.286 paraphrase,
0.469 buried_fact, 0.704 distractor_trap — while scoring *best* of all four on
`tag_only`. A model can be strong on short-token matching and weak on semantics;
"1024 dims" tells you nothing about which.

Every archetype cell is 8–12 queries, so a 0.06 delta is roughly one query.
Individual cells are indicative only. What is solid is the overall n=72 ordering
and the fact that `paraphrase` is the floor for all four models by a wide margin.

## The layered proposal

The idea was: embed with the small model on `create` (cheap, immediate), then
have a periodic maintenance pass re-embed with the big model and drop the small
vectors. Creation is rare, so paying 8× there would be affordable.

It does not work, for a reason that holds regardless of the quality numbers.
**Vectors from different models are different spaces.** A query must be embedded
with the same model as the index it is searched against, and the chunks table
pins its width at creation (`ensure_table` enforces it; `EmbeddingFingerprint`
guards mismatches). So:

- Once maintenance has re-embedded everything to 1024 and dropped the 384
  vectors, the steady state is *1024-only* and **every query pays the big
  model's embed cost**. The small model saved latency only on creates — the
  operation that was already cheap and rare.
- Before that point the store is mixed, which is worse than either alone: the
  query has to be embedded twice and two incomparable ranked lists merged.

So the layering optimizes the operation that was not the problem, and leaves the
one that was.

The variant that *does* respect the constraint is the opposite split: keep 384
for the vector index (cheap recall over the whole corpus) and spend on precision
at the top of the ranking — which is what the cross-encoder reranker already
does, gated to `top_n = 10`, running once per query over a shortlist instead of
over every memory.

But note what that cannot fix. A reranker only reorders what the vector search
already surfaced, so it cannot recover a `paraphrase` miss where the right
memory never entered the top-k. **Paraphrase is a recall problem**, and neither
a wider model (+0.018) nor a reranker addresses it. If paraphrase retrieval is
worth improving, the lever is something else — query expansion, a retrieval-tuned
model rather than merely a wider one, or accepting that keyword and scope
signals carry those queries.

## Recommendation

**Keep the 384-dim default.** Two of three wider models are worse; the one that
is better costs ~19× disk and ~53× warm query latency for about three queries out
of 72, concentrated in archetypes the existing keyword and rerank signals already
serve. All three remain selectable (`mxbai-embed-large-q`, `gte-large-en-q`,
`bge-large-en-q`) for anyone who wants to re-run this on their own corpus.

If a model swap is ever revisited, revisit it on **retrieval quality per
millisecond against a project's own memories**, the way
[`embedding-model-alternatives.md`](./embedding-model-alternatives.md) decides
these — not on dimension.

## Caveats

- **The corpus is this project's, and the labels are authored.** That makes the
  *relative* ordering of models trustworthy and absolute scores not comparable
  to any public benchmark. It is the right shape for the question asked, which
  was whether 1024 beats 384 here.
- **n=72 overall, 8–12 per archetype.** Overall deltas of ~4 points are ~3
  queries; archetype deltas of ~0.06 are ~1 query.
- **`bge-large-q` is fp32, not quantized** — see above. Its latency is not
  representative of what a genuinely quantized 1024-dim model would cost.
- **One machine, 4 vCPU, AVX2, no AVX-512.**

## Reproducing

```bash
python3 tools/eval-corpus/build_corpus.py          # regenerate memories
python3 tools/eval-corpus/assemble.py              # validate labels + assemble
./tools/eval-corpus/stage_models.sh                # pre-stage HF weights

ORT_DYLIB_PATH=…/libonnxruntime.so ENGRAMDB_OFFLINE=1 \
EMBED_EVAL_DATA=tools/eval-corpus/engramdb_eval.json \
EMBED_EVAL_VARIANTS=fieldvec_c256 \
EMBED_EVAL_MODELS=minilm-l12-u8 \
EMBED_EVAL_OUT=tools/eval-corpus/res_minilm.json \
  cargo run --example embed_matrix
```

One model per invocation, each to its own results file — the sandbox this was
run in reverted twice mid-sweep, and per-model outputs meant no completed model
had to be re-run.
