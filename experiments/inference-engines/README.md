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

## Results

See `results/summary.md` (generated) and the analysis below.
