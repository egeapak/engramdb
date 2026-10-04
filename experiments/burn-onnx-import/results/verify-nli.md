| Model | Inputs | Agreement (Burn imported fp32 vs ONNX Runtime) |
|---|---|---|
| NLI (DeBERTa-v3-xsmall) | 30 pairs | max abs logit diff 5.20e-6; label same as ORT fp32 30/30, as shipped int8 30/30 |

| Workload | Load ms (Burn / ORT fp32 / ORT shipped) | Burn p50 ms | ORT fp32 p50 ms | ORT shipped p50 ms | Burn vs ORT fp32 | Burn vs ORT shipped |
|---|---:|---:|---:|---:|---:|---:|
| NLI (NdArray backend), 1 pair (112 tokens) | 488 / 1138 / 804 | 267.1 | 39.8 | 29.0 | 6.7× | 9.2× |
