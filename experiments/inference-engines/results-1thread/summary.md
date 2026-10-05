## Speed

| Engine | Load (ms) | First call (ms) | Query p50 / p95 (ms) | Doc p50 / p95 (ms) | Batch-16 p50 (ms) | Cores busy (batch) | Corpus (docs/s) |
|---|---:|---:|---:|---:|---:|---:|---:|
| ort-u8 | 136 | 3.5 | 2.45 / 3.39 | 31.0 / 51.9 | 1139.5 | 1.0 | 13.1 |
| ort-f32 | 313 | 7.9 | 6.42 / 9.87 | 77.6 / 126.9 | 2255.8 | 1.0 | 7.0 |
| burn-flex | 297 | 30.6 | 22.90 / 33.61 | 210.6 / 312.8 | 6658.7 | 1.0 | 2.3 |
| burn-ndarray | 385 | 135.2 | 113.57 / 128.24 | 278.7 / 422.9 | 7234.3 | 1.4 | 2.2 |
| candle | 123 | 25.9 | 27.05 / 38.04 | 255.0 / 415.0 | 6923.5 | 1.0 | 2.3 |

## Memory and size

| Engine | RSS after load (MiB) | Peak RSS (MiB) | Weights file (MiB) | Binary (MiB) | Clean release build (s) |
|---|---:|---:|---:|---:|---:|
| ort-u8 | 89 | 503 | 32.2 | – | – |
| ort-f32 | 189 | 403 | 126.9 | – | – |
| burn-flex | 155 | 284 | 127.3 | – | – |
| burn-ndarray | 142 | 300 | 127.3 | – | – |
| candle | 140 | 472 | 127.3 | – | – |

## Agreement and quality (reference: ort-f32)

| Engine | Mean cosine | Min cosine | P@1 | R@5 | MRR@10 | nDCG@10 | Determinism (distinct / trials) |
|---|---:|---:|---:|---:|---:|---:|---:|
| ort-u8 | 0.979920 | 0.960785 | 0.896 | 0.878 | 0.908 | 0.898 | 1 / 30 |
| ort-f32 | 1.000000 | 1.000000 | 0.896 | 0.889 | 0.908 | 0.900 | 1 / 30 |
| burn-flex | 1.000000 | 1.000000 | 0.896 | 0.889 | 0.908 | 0.900 | 1 / 30 |
| burn-ndarray | 1.000000 | 1.000000 | 0.896 | 0.889 | 0.908 | 0.900 | 1 / 30 |
| candle | 1.000000 | 1.000000 | 0.896 | 0.889 | 0.908 | 0.900 | 1 / 30 |

4 threads available; mean document length 174 tokens (max 256).
