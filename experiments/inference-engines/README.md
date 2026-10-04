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

(For the decision, only the runtime rows matter: binary size, external
library, weights. Build time and crate count are recorded for completeness.)

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

See "Overall verdict" at the end, which also covers the pool sweep,
`burn-cpu` and the `burn-onnx` imports.

## Daemon pool throughput (`pool.sh`)

Mirrors `PooledEmbeddingProvider`: N independent sessions, each behind a
`std::sync::Mutex` (as production), requests handed out round-robin; C caller
threads each send one text per request for a 4 s window. Two runs: each
engine's default threads per session (what the daemon does), and 1 thread per
session. Raw tables: `results-pool/summary.md`, `results-pool-1thread/summary.md`.

Best configuration per engine under concurrent load (4–8 callers):

| Engine | Best config | Queries/s | Docs/s | RSS at that pool size (MiB) |
|---|---|---:|---:|---:|
| ORT uint8 | pool 4, 1 thread each | 1,199 | 101 | 257 |
| ORT fp32 | pool 4, 1 thread each | 281 | 43 | 618 |
| Candle | pool 4, 1 thread each | 104 | 15.0 | 533 |
| Burn Flex | pool 4 | 92 | 14.0 | 576–595 |
| Burn NdArray | pool 4 | 46 | 7.0 | 554 |
| *ORT uint8, daemon default today (pool 2, default threads)* | | *246–270* | *20–28* | *159* |

- **Against the best ORT setup, Burn Flex is 13× slower on queries and 7×
  slower on documents.** Against ORT at the same precision (fp32) it is 3×
  slower on both.
- **Pools help the pure-Rust engines** more than they help a single call:
  Burn Flex goes from 31 to 92 queries/s (pool 1 → 4), because its per-call
  overhead is serial and independent sessions run it in parallel. Candle
  behaves the same.
- **Memory scales linearly with pool size** for every engine; fp32 engines
  pay ~145 MiB per extra session, ORT uint8 ~55 MiB.
- Single-caller latency does not improve with pooling for any engine (the
  pool only adds parallel capacity).

### Side finding: the daemon pool was misconfigured (fixed)

**Fixed** on this branch: pooled embedding sessions now get
`cores / pool_size` threads each (`EmbeddingsConfig::session_intra_threads`).
Measured through the production path (`examples/embed_pool_bench.rs`, pool
of 2 on this machine): 1 caller 99.7 → 227 queries/s and 21.3 → 34.9 doc/s;
8 callers 236 → 492 queries/s and 28.9 → 68.8 doc/s; doc p99 at 8 callers
676 → 312 ms. A single session (the CLI) is unchanged. The original
analysis follows.

The daemon's default (pool = `cores/2` = 2, ORT default threads per session)
is the *worst* ORT configuration measured here:

- 1 caller: 27 doc/s vs 56 doc/s for a single session (p50 36 ms vs 18 ms).
- 4–8 callers: 246–270 queries/s and 20–28 doc/s, vs 1,199 and 101 with
  4 sessions × 1 thread.

Each pooled session keeps ORT's default thread pool, so sessions compete for
the same cores, and idle sessions' spinning threads slow the active one. The
NLI/T5 pools already cap `pool × intra_threads ≤ cores`; the embedding pool
does not (`crates/engram-models/src/embeddings/pool.rs` doc comment). The
earlier measurement that motivated `cores/2` was on 8-core Apple Silicon and
was not reproduced here, so re-measure there before changing the policy.

## `burn-cpu` (CubeCL, MLIR/LLVM JIT)

**Runtime dependencies.** LLVM is *not* needed at runtime: the
`tracel-llvm-bundler` build script downloads a prebuilt LLVM 20.1.4 + MLIR
archive from `github.com/tracel-ai/tracel-llvm` releases (checksum-verified)
and links it statically. The finished binary needs only `libc`, `libm`,
`libgcc_s` and **`libstdc++`** (GLIBCXX ≥ 3.4.30, i.e. GCC 12+ on Linux) —
one more system library than Burn Flex, which needs none beyond libc. The
binary is **125 MiB** (vs 5.7 MiB for Burn Flex).

(Sandbox note: the bundler's `reqwest`/rustls download fails on the web
sandbox's proxy CA, like `ort` and `hf-hub`. Pre-place
`linux-x64.tar.xz` and `linux-x64.checksums.json` from the release in
`~/.cache/tracel/` as `tracel-llvm-20.1.4-7-<name>` and build with
`TRACEL_LLVM_BUNDLER_SKIP_CHECKSUM_DOWNLOAD=1`; checksums are still verified.)

**Runtime side effect.** CubeCL writes an autotune cache to the *project's*
`target/` directory by default — it walks up from the current directory to
the nearest `Cargo.toml`. A shipped `engramdb` run inside a user's Rust
repository would write `target/autotune/` into that repository. It is
configurable (`CacheConfig`), but the default is wrong for a shipped binary.

**Speed: not usable in 0.21.** `probe burn-cpu` (one 9-token query, repeated):

| | Burn Flex | burn-cpu (default) | burn-cpu, `CUBECL_AUTOTUNE_LEVEL=minimal` | burn-cpu without fusion |
|---|---:|---:|---:|---:|
| Load | 246 ms | 352 ms | 449 ms | 390 ms |
| First call | 27 ms | **341 s** | **527 s** | **361 s** |
| Later identical calls | 25–31 ms | **~21 s** | **~19–21 s** | **~20 s** |
| RSS | 157 MiB | **4.9 GiB** | **5.1 GiB** | **5.0 GiB** |

- The first call compiles and autotunes every kernel (minutes). Each new
  sequence length is a new shape, so this repeats for new input lengths.
- The steady-state cost is the **matmul kernel**: CubeCL's profiler shows 98
  matmul launches per call averaging 523 ms each, while every other kernel
  type (fused elementwise, reductions, copies) takes 0.2–5 ms. Turning off
  fusion (`burn-cpu-nofusion` feature) or autotune changes nothing, so it is
  the CPU matmul implementation in this release.
- The full `bench` could not complete one round of 48 queries in 12 minutes,
  so `burn-cpu` has no row in the Phase 1 tables.

## Overall verdict (runtime dependencies and runtime speed only)

Build time, build-time downloads and crate count are deliberately ignored.

| Option | Runtime dependencies | Speed vs shipped ORT uint8 (single call / best pool) | Models it can run |
|---|---|---|---|
| ONNX Runtime (today) | external `libonnxruntime.so` (21 MiB, API ≥ 24) the user must install; startup probe; a bad build corrupts quantized vectors (R6/R9) | 1× | all four, quantized |
| **Burn Flex** | **none beyond libc** | **8× / 13× slower** (queries), **7× / 7× slower** (documents) | hand-written BERT: embeddings. Via burn-onnx (fp32): embeddings, reranker, T5 (full-prefix decode). NLI panics on Flex |
| Candle (CPU, pure Rust) | none beyond libc | 10× / 12× slower (queries), 11× / 7× slower (documents) | BERT here; upstream also has XLM-R, DeBERTa-v2, T5 (not tested) |
| Burn NdArray | none beyond libc | 36× / 26× slower (queries) | as Flex, and NLI works (deprecated backend) |
| burn-cpu (CubeCL) | `libstdc++` (GCC 12+); 125 MiB binary; writes autotune cache into the user's `target/` | **~7,000× slower** (≈20 s per query, minutes for the first call, 5 GiB RAM) | not usable in 0.21 |

1. **The runtime-dependency goal is achievable.** Burn Flex and Candle need
   nothing beyond libc, and every EngramDB model family has an fp32 route
   that produces the same numbers as ONNX Runtime (embeddings, reranker,
   NLI, T5 — see `../burn-onnx-import`).
2. **The runtime-speed goal is not.** No pure-Rust engine comes close. The
   gap has two parts: ~3× from slower fp32 kernels and per-op overhead
   (Burn Flex vs ORT fp32, with both pooled), and ~2–4× more from losing
   int8 (ORT uint8 vs ORT fp32). Burn has no integer matmul, so the second
   part cannot be recovered in Burn today.
3. **The `burn-onnx` route is not production-safe yet.** None of the shipped
   quantized files import; the fp32 DeBERTa graph panics on Flex; and the
   KV-cache T5 decoder is silently wrong. Imported models are also slower
   than hand-written ones.
4. **burn-cpu is not an option in 0.21** (matmul kernel), and it would add
   `libstdc++` and a 125 MiB binary.

So switching trades one external library for a 7–13× slowdown on
embeddings and a 7–15× slowdown on the reranker, NLI and T5 (imported fp32
vs the shipped quantized files). Compared with the daemon's current
misconfigured pool (see the side finding), the embedding gap shrank to
1.5–3×; with the pool fixed (default pool of 2) it is back to about 5× (492 vs 92 queries/s, 68.8 vs 14 doc/s).

### Not measured yet

- Burn 0.22 (pre-release); `burn-cpu` and int8 kernels may improve there.
- Candle's XLM-R / DeBERTa-v2 / T5 implementations.
- Candle with MKL or Accelerate (adds a native dependency, against the goal).
- macOS / Apple Silicon (ORT uses CoreML there; Burn Flex has an AMX path).
