# Can `burn-onnx` import EngramDB's models?

Every ONNX file EngramDB uses, run through `burn-onnx` 0.21.0 (`onnx2burn`)
in three stages:

1. **Codegen** — ONNX graph to Rust source plus `.bpk` weights (`import.sh`).
2. **Compile** — `cargo check` of the generated code on Burn 0.21 (`import.sh`).
3. **Run and verify** — the generated model against ONNX Runtime 1.24.2 on
   the same real inputs (`verify/`, inputs from `examples/data/embed_eval.json`).

## Results (2026-10-04)

| Model (file) | Shipped? | 1. Codegen | 2. Compile | 3. Run vs ONNX Runtime |
|---|---|---|---|---|
| MiniLM-L12 embeddings, `model_uint8.onnx` | **yes** (default) | ❌ `DynamicQuantizeLinear` unsupported | – | – |
| MiniLM-L12 embeddings, `model.onnx` (fp32) | no | ✅ | ✅ | ✅ exact: max abs diff 4.1e-6, min cosine 1.000000 over 20 texts |
| Reranker jina-v1-turbo, `model_uint8.onnx` | **yes** (default) | ❌ `DynamicQuantizeLinear` | – | – |
| Reranker jina-v1-turbo, `model.onnx` (fp32) | no | ✅ | ✅ | ✅ exact: max logit diff 3.1e-6; same top-1 on 8/8 queries (vs fp32 and vs shipped uint8) |
| NLI DeBERTa-v3-xsmall, `model_quantized.onnx` | **yes** (opt-in) | ❌ `DynamicQuantizeLinear` | – | – |
| NLI DeBERTa-v3-xsmall, `model.onnx` (fp32) | no | ✅ | ✅ | ⚠️ **panics on Flex** (`dtype mismatch (expected I32, got I64)` inside the generated graph). ✅ exact on NdArray: max logit diff 5.2e-6, same label 30/30 |
| T5-small encoder, `encoder_model_quantized.onnx` | **yes** | ❌ `DynamicQuantizeLinear` | – | – |
| T5-small encoder, `encoder_model.onnx` (fp32) | no | ✅ | ✅ | ✅ exact: max abs diff 9.1e-7 |
| T5-small decoder, `decoder_model_quantized.onnx` | **yes** | ❌ `DynamicQuantizeLinear` | – | – |
| T5-small decoder, `decoder_model.onnx` (fp32) | no | ✅ | ✅ | ✅ logits diff 5.7e-5; generated titles identical to ORT fp32 on 10/10 |
| T5-small decoder with KV cache, `decoder_with_past_model.onnx` (fp32) | no | ✅ | ✅ | ❌ **silently wrong**: first token right, then garbage ("order" + blanks). The same decode loop on ONNX Runtime matches 10/10, so the import is at fault |

**Summary.**

- **No file EngramDB ships today imports.** All five quantized files stop at
  the same op, `DynamicQuantizeLinear` (dynamic uint8/int8 quantization). It
  is the only unsupported op in any of them (static check of every graph
  against `SUPPORTED-ONNX-OPS.md`). Even if it were added, burn-onnx lowers
  `MatMulInteger` to a float matmul, so a quantized import would run at fp32
  speed.
- **Every fp32 equivalent imports and compiles**, and four of the six run
  with exact agreement. That covers all four model families (embeddings,
  reranker, NLI, title generation).
- **Two defects found:** the DeBERTa graph panics on the default Flex
  backend (works on the deprecated NdArray backend), and the KV-cache T5
  decoder produces wrong output **without any error**. Any import therefore
  needs a numerical check against a reference before it can be trusted.

## Speed of the imported models

Warm p50 of 10 calls, 4-vCPU Xeon (AVX-512), ONNX Runtime with default
threads. "Shipped" is the quantized file EngramDB uses today.

| Workload | Burn imported fp32 | ORT fp32 | ORT shipped | Burn vs ORT fp32 | Burn vs ORT shipped |
|---|---:|---:|---:|---:|---:|
| MiniLM, 7-token query | 35.5 ms | 3.9 ms | 2.8 ms | 9.0× | 12.6× |
| MiniLM, 186-token document | 202.7 ms | 36.4 ms | 17.1 ms | 5.6× | 11.9× |
| Reranker, 10 pairs (padded to 512 tokens) | 7,870 ms | 825 ms | 542 ms | 9.5× | 14.5× |
| NLI, 1 pair, 112 tokens (NdArray) | 267 ms | 39.8 ms | 29.0 ms | 6.7× | 9.2× |
| T5 title, 128 input tokens (full-prefix decode, as production) | 1,859 ms | 218 ms | 249 ms | 8.5× | 7.5× |

Load times are not a concern (Burn 160–550 ms, ORT 110–1,140 ms).

The imported MiniLM is slower than the hand-written Burn BERT in
`../inference-engines` (35.5 vs 24.2 ms per query, 203 vs 125 ms per
document): generated graphs keep ONNX's decomposed ops where a hand-written
model calls Burn's fused modules.

## Running

```sh
cargo install burn-onnx --version 0.21.0 --bin onnx2burn --locked
./stage_models.sh                              # every ONNX file + tokenizers + ONNX Runtime
./import.sh                                    # stages 1-2 -> results/import.tsv
verify/run.sh                                  # stage 3   -> results/verify-*.md
```

Notes for reproducing:

- Generated code is `no_std`-style and names `alloc::` directly; the crate
  that includes it needs `extern crate alloc;`.
- Pass integer inputs in the backend's own int type (`i32` on Flex). Burn
  keeps the dtype of the data it is given.
- The generated sources and weights (`gen/`, ~3.5 GB) are not committed.
