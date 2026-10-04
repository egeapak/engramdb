| Model | Inputs | Agreement (Burn imported fp32 vs ONNX Runtime) |
|---|---|---|
| Reranker (jina-v1-turbo) | 8 queries × 10 docs | max abs logit diff 3.10e-6; top-1 same as ORT fp32 8/8, as shipped uint8 8/8 |

| Workload | Load ms (Burn / ORT fp32 / ORT shipped) | Burn p50 ms | ORT fp32 p50 ms | ORT shipped p50 ms | Burn vs ORT fp32 | Burn vs ORT shipped |
|---|---:|---:|---:|---:|---:|---:|
| Reranker, batch of 10 pairs (512 tokens) | 195 / 329 / 110 | 7869.5 | 825.0 | 542.0 | 9.5× | 14.5× |
