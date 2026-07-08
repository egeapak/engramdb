# tract-vs-ort — pure-Rust inference feasibility experiment

Standalone experiment (own `[workspace]`, **not** part of the main workspace) comparing
**ONNX Runtime** (the `ort` crate, C++ backend) against **pure-Rust `tract-onnx`** on the
all-MiniLM-L6-v2 sentence-embedding pipeline.

## Why

Intel Mac (`x86_64-apple-darwin`) has **no prebuilt ONNX Runtime 1.24** (API 24), which the
`ort`/`fastembed` stack requires — so official Intel-Mac binaries crash at startup. `tract` is a
pure-Rust ONNX engine (no native library), so a tract-backed provider would build and run on
Intel Mac (and everywhere) with no runtime to install. This measures whether that is viable.

## Run

```bash
ORT_STRATEGY=system ORT_LIB_LOCATION=/path/to/onnxruntime-1.24-libdir \
  cargo run --release
```

Expects the models cached under `~/.cache/engramdb/models/` (int8 + fp32 all-MiniLM, tokenizers).

## Results (this sandbox CPU, single-threaded; representative)

| engine / model | single p50 | batch-8 p50 | cosine vs ORT | tract loads? |
|---|---|---|---|---|
| ort int8 (EngramDB default) | ~14 ms | ~69 ms | — | — |
| **tract int8** | — | — | — | ❌ fails to load |
| ort fp32 | ~21 ms | ~94 ms | — | — |
| **tract fp32** | ~65 ms | ~578 ms | **1.00000** | ✅ optimized |

tract/ort slowdown (fp32): **~3.1× single, ~6.2× batch**.

## Findings

1. **tract cannot load the int8-quantized MiniLM** — shape-inference failure on a dynamic
   `Unsqueeze` (node #293) on both the optimized and typed paths. The tract path must use the
   **fp32 model** (`Qdrant/all-MiniLM-L6-v2-onnx`).

   This is **not** a missing-int8-op problem. The graph uses `DynamicQuantizeLinear ×24`,
   `MatMulInteger ×36`, `DequantizeLinear ×3` — ops tract *implements*. It fails earlier, in
   tract's **ahead-of-time static shape analysis** (its design for optimization/streaming), which
   is stricter than ORT's **runtime, lazy** shape resolution: tract must prove every symbolic dim
   before building the model, and it can't unify the shapes derived through the attention-mask
   `Unsqueeze` chain in *this* (optimum / transformers.js) quantized export. The clean fp32 export
   keeps that subgraph fully symbolic and tract optimizes it fine — so the trigger is the quantized
   export's graph structure + tract's static analyser, not int8 arithmetic. (The `128` constants in
   the graph are embedding-table quantization *zero-points*, not a sequence length.) int8 on tract
   would need a tract-friendly re-export (static/fixed shapes); not worth it when fp32 works.
2. **tract's fp32 output is bit-identical to ONNX Runtime** (cosine 1.00000).
3. **~3× slower** single-encode — acceptable for an on-demand memory store (embed on
   create/query), and it *works* where ORT has no runtime at all.

## Conclusion

`tract` + the **fp32** MiniLM is a viable **Intel-Mac-gated** fallback embedding backend: pure-Rust,
no prebuilt runtime, correct output, ~3× slower. Keep ORT + int8 as the default everywhere it has a
prebuilt runtime; gate tract to platforms that lack one. tract requires concrete input shapes, so a
production provider needs a per-sequence-length runnable cache (ORT handles dynamic shapes natively).
