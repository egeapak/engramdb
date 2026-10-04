# Phase 1: ONNX Runtime vs Burn vs Candle (embeddings)

Can a pure-Rust engine replace ONNX Runtime for EngramDB's default embedding
model, and what does it cost? This experiment measures the one model that has
a working pure-Rust path today: **all-MiniLM-L12-v2**.

Standalone crate (its own `[workspace]`), so none of these dependencies enter
the EngramDB build or lockfile.

## Engines

| Engine | Crate / version | Weights | Precision | Native libraries |
|---|---|---|---|---|
| `ort-u8` | `ort` 2.0.0-rc.13, `load-dynamic` | `Xenova/all-MiniLM-L12-v2` `onnx/model_uint8.onnx` | uint8 (dynamic quant) | `libonnxruntime.so` 1.24.2 (official build) |
| `ort-f32` | same | same repo, `onnx/model.onnx` | fp32 | same |
| `burn-flex` | `burn` 0.21.0, Flex backend | `sentence-transformers/all-MiniLM-L12-v2` `model.safetensors` | fp32 | none |
| `burn-ndarray` | `burn` 0.21.0, NdArray backend | same | fp32 | none |
| `candle` | `candle-transformers` 0.11.0 `BertModel`, CPU | same | fp32 | none |

- `ort-u8` is what EngramDB ships today (`DEFAULT_ONNX_EMBEDDING`).
- `ort-f32` is the fair kernel comparison. Burn and Candle can only run fp32
  here, so `ort-f32` isolates the engine from the precision.
- Burn cannot import the uint8 ONNX file: `burn-onnx` does not support
  `DynamicQuantizeLinear`, and Burn's quantization dequantizes to fp32 before
  every operation anyway. Burn therefore runs a hand-written BERT (stock
  `TransformerEncoder`) with `burn-store` safetensors loading, adapted from
  `tracel-ai/models` `minilm-burn`.
- Candle has a quantized path (GGUF), but not for BERT; it runs fp32.

## What is held equal

- **Tokens.** One `tokenizer.json`, the same crate (`tokenizers` 0.23, `onig`),
  truncation at 256 tokens, padding to the longest sequence in the batch —
  the settings fastembed applies in production.
- **Pooling.** Each engine returns `last_hidden_state`. Masked mean pooling
  and L2 normalization run once, in shared code (`src/common.rs`).
- **Threads.** Each engine's default: ORT's intra-op default (as production's
  embedding path), rayon's default pool for Burn and Candle. All use every
  core. Override with `ENGINE_THREADS` (ORT) or `RAYON_NUM_THREADS`.
- **Process.** One engine per process, so memory readings are not shared.
- **Build.** EngramDB's release profile (`lto = true`, `opt-level = 3`,
  `codegen-units = 1`, `strip`, `panic = "abort"`), clean build per engine
  family in its own target directory. Crate download time is excluded.

## Metrics

| Metric | How |
|---|---|
| Load time | Model files to ready engine. Tokenizer load excluded (shared). |
| First call | First embedding after load (lazy allocation, kernel warm-up). |
| Query latency | Each of the 48 eval queries, one at a time, `ITERS` rounds; p50 / p95. Short texts: the `query` path. |
| Document latency | Each of the 60 eval memories (`title. summary content`), one at a time; p50 / p95. The `create` path. |
| Batch-16 latency | First 16 documents as one batch, `4 × ITERS` times. The `reindex` path. |
| Corpus throughput | All 60 documents in batches of 16, documents per second. |
| Memory | `VmRSS` after load, `VmHWM` (peak RSS) at the end, from `/proc/self/status`. |
| Size | Weights file; stripped release binary; a `baseline` binary with no engine for subtraction. |
| Build time | Clean `cargo build --release` wall time per engine family. |
| Agreement | Cosine of every document and query vector against `ort-f32`. |
| Retrieval quality | P@1, R@5, MRR@10, nDCG@10 on `examples/data/embed_eval.json`, same definitions as `examples/embed_matrix.rs`. |
| Determinism | One document embedded `PROBE_TRIALS` times while `PROBE_LOAD_THREADS` busy threads compete for the CPU; count of bit-distinct vectors (1 = deterministic). |

## Running

```sh
./stage_models.sh   # models + official ONNX Runtime 1.24.2 into models/
./run.sh            # builds, benches, writes results/summary.md
```

`SKIP_BUILD=1` reuses binaries, `BUILD_ONLY=1` only builds,
`ENGINES="ort-u8 candle"` picks engines, `ITERS=5` adds rounds. Linux only for
the memory figures (`/proc`); everything else is portable.

## Caveats

- Candle memory-maps the safetensors file, so its RSS includes file-backed
  pages the kernel can drop under pressure; Burn copies weights into tensors.
- `ort-u8` and the fp32 engines differ in precision, not only engine. Compare
  engines with `ort-f32`; compare against the shipped default with `ort-u8`.
- A different model id means a different stored fingerprint: switching an
  existing store to any of these engines requires
  `engramdb reindex --embeddings-only`.
- Throughput under the daemon's session pool (`cores/2` sessions) is not
  measured here; this is single-session behavior.

## Results (2026-10-04)

Raw tables: `results/summary.md` (default threads, `ITERS=5`) and
`results-1thread/summary.md` (`ENGINE_THREADS=1 RAYON_NUM_THREADS=1`,
`ITERS=3`). Machine: 4-vCPU Intel Xeon @ 2.10 GHz (AVX-512, AMX), 15 GB RAM,
Linux, the Claude Code web sandbox. Single runs on a shared VM: treat
differences under ~15% as noise.

### Correctness and quality: all engines are equivalent

- Burn (both backends) and Candle reproduce ONNX Runtime fp32 exactly at
  six decimals: mean and minimum cosine 1.000000 over all 108 vectors.
  Retrieval quality is identical (P@1 0.896, R@5 0.889, MRR@10 0.908,
  nDCG@10 0.900).
- The shipped uint8 model is the one that drifts (mean cosine 0.980 vs fp32,
  R@5 0.878 vs 0.889). An fp32 engine would be very slightly *better* on
  quality, not worse.
- Every engine returned one bit-identical vector over 30 trials under CPU
  contention on this AVX-512/AMX host (official ONNX Runtime build).

### Speed: Burn and Candle are 4–11× slower than what ships

Default threads (all 4 cores available):

| | ORT uint8 (ships) | ORT fp32 | Burn Flex | Candle | Burn NdArray |
|---|---:|---:|---:|---:|---:|
| Query p50 (ms) | 3.0 | 4.0 | 24.2 | 29.5 | 109.7 |
| Document p50 (ms) | 18.5 | 31.1 | 124.9 | 212.3 | 252.8 |
| Batch-16 p50 (ms) | 321 | 675 | 3498 | 4996 | 6025 |
| Corpus (docs/s) | 50.9 | 26.4 | 4.5 | 3.1 | 2.7 |
| Cores busy during batch | 4.0 | 3.9 | 1.8 | 1.6 | 1.7 |

Ratio of Burn Flex (the best pure-Rust engine) to ORT:

| | vs ORT uint8 | vs ORT fp32 |
|---|---:|---:|
| Query | 8.0× | 6.0× |
| Document | 6.8× | 4.0× |
| Batch-16 | 10.9× | 5.2× |

Two separate causes, visible in the single-thread run:

1. **Per-call overhead dominates short texts.** A ~10-token query takes
   23 ms on Burn Flex and 27 ms on Candle with one thread *or* four;
   ORT does it in 2.5–6.4 ms. The cost is framework dispatch per operation,
   not arithmetic, so more cores do not help.
2. **Kernels are ~3× slower and scale poorly.** Single-threaded batch-16:
   Burn Flex 6.7 s, Candle 6.9 s, ORT fp32 2.3 s (2.9–3.0×). With four cores
   ORT keeps all four busy; Burn and Candle keep 1.6–1.8 busy.

What it means in EngramDB terms:

- `query`: +21 ms per query embedding with Burn Flex. Noticeable in an MCP
  round trip, but not dominant.
- `create`: ~125 ms instead of ~19 ms per memory embedding.
- `reindex` of 1,000 memories: ~3.7 min instead of ~20 s.

### Load time and memory: no blocker

| | ORT uint8 | ORT fp32 | Burn Flex | Candle |
|---|---:|---:|---:|---:|
| Load (ms) | 166 | 319 | 300 | 136 |
| RSS after load (MiB) | 88 | 187 | 155 | 140 |
| Peak RSS (MiB) | 504 | 410 | 287 | 475 |

- Load times are all well under the daemon's tolerances. Candle is fastest
  because it memory-maps the weights.
- Steady-state memory is higher for the fp32 engines (127 MB of weights
  instead of 32 MB). Burn Flex has the *lowest* peak; ORT's peak comes from
  its memory arena during the batch-16 run (not tuned here).

### Size and build cost

| | Baseline | ORT | Burn Flex | Burn NdArray | Candle |
|---|---:|---:|---:|---:|---:|
| Binary (MiB, stripped) | 1.9 | 3.7 | 5.7 | 6.1 | 5.0 |
| Plus external library | – | 21.0 MiB `libonnxruntime.so` | – | – | – |
| Clean release build (s) | 60 | 72 | 201 | 173 | 152 |
| Crates in the dependency tree | 86 | 95 | 218 | 212 | 173 |
| Weights to download (MiB) | – | 32 | 127 | 127 | 127 |

- The pure-Rust engines do remove the external runtime: no `libonnxruntime`
  on any user's machine, no `ldd` entry, no `load-dynamic` probe, no
  "install onnxruntime" step, no AVX-512 build-quality risk.
- They cost +3–4 MiB of binary, +90–140 s of clean build, +87–132 crates,
  and a 4× larger model download (fp32 weights; no integer kernels).

### Verdict for Phase 1

- **Burn Flex is the best pure-Rust option** here: faster than Candle on
  every speed row, lowest peak memory. Burn NdArray is not competitive.
- **Neither is a drop-in replacement on speed.** Burn Flex is 6–11× slower
  than the shipped uint8 model, and 4–6× slower than ONNX Runtime at the same
  precision. Quality and determinism are not the problem.
- The removal of the external dependency is real and complete for this model.
  Whether it is worth the speed cost depends on how much the `create` and
  `reindex` paths matter, and on Phase 2 (reranker, NLI, T5), which have no
  ready Burn ports at all.

### Not measured yet

- Throughput with the daemon's session pool (`cores/2` sessions).
- Burn 0.22 (pre-release) and the LLVM-based `burn-cpu` backend.
- Candle with MKL (adds a native dependency, so out of scope for the goal).
- macOS / Apple Silicon (ORT uses CoreML there; Burn Flex has an AMX path).
