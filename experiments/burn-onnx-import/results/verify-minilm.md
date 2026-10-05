| Model | Inputs | Agreement (Burn imported fp32 vs ONNX Runtime) |
|---|---|---|
| MiniLM-L12 embeddings | 20 texts | max abs diff 4.05e-6, min cosine 1.000000 (last_hidden_state) |

| Workload | Load ms (Burn / ORT fp32 / ORT shipped) | Burn p50 ms | ORT fp32 p50 ms | ORT shipped p50 ms | Burn vs ORT fp32 | Burn vs ORT shipped |
|---|---:|---:|---:|---:|---:|---:|
| MiniLM, 7 tokens | 159 / 331 / 112 | 35.5 | 3.9 | 2.8 | 9.0× | 12.6× |
| MiniLM, 186 tokens | 159 / 331 / 112 | 202.7 | 36.4 | 17.1 | 5.6× | 11.9× |
