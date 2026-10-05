## Speed

| Engine | Load (ms) | First call (ms) | Query p50 / p95 (ms) | Doc p50 / p95 (ms) | Batch-16 p50 (ms) | Cores busy (batch) | Corpus (docs/s) |
|---|---:|---:|---:|---:|---:|---:|---:|
| ort-u8 | 166 | 10.0 | 3.01 / 4.18 | 18.5 / 26.9 | 320.8 | 4.0 | 50.9 |
| ort-f32 | 319 | 4.7 | 4.02 / 5.62 | 31.1 / 44.3 | 674.5 | 3.9 | 26.4 |
| burn-flex | 300 | 36.2 | 24.23 / 33.05 | 124.9 / 194.6 | 3498.2 | 1.8 | 4.5 |
| burn-ndarray | 351 | 101.6 | 109.72 / 126.34 | 252.8 / 385.7 | 6024.7 | 1.7 | 2.7 |
| candle | 136 | 31.1 | 29.48 / 38.37 | 212.3 / 339.9 | 4995.6 | 1.6 | 3.1 |

## Memory and size

| Engine | RSS after load (MiB) | Peak RSS (MiB) | Weights file (MiB) | Binary (MiB) | Clean release build (s) |
|---|---:|---:|---:|---:|---:|
| ort-u8 | 88 | 504 | 32.2 | 3.7 | 72 |
| ort-f32 | 187 | 410 | 126.9 | 3.7 | 72 |
| burn-flex | 155 | 287 | 127.3 | 5.7 | 201 |
| burn-ndarray | 142 | 358 | 127.3 | 6.1 | 173 |
| candle | 140 | 475 | 127.3 | 5.0 | 152 |

Baseline binary with no engine (tokenizer + harness only): 1.9 MiB, 60 s.

## Agreement and quality (reference: ort-f32)

| Engine | Mean cosine | Min cosine | P@1 | R@5 | MRR@10 | nDCG@10 | Determinism (distinct / trials) |
|---|---:|---:|---:|---:|---:|---:|---:|
| ort-u8 | 0.979920 | 0.960785 | 0.896 | 0.878 | 0.908 | 0.898 | 1 / 30 |
| ort-f32 | 1.000000 | 1.000000 | 0.896 | 0.889 | 0.908 | 0.900 | 1 / 30 |
| burn-flex | 1.000000 | 1.000000 | 0.896 | 0.889 | 0.908 | 0.900 | 1 / 30 |
| burn-ndarray | 1.000000 | 1.000000 | 0.896 | 0.889 | 0.908 | 0.900 | 1 / 30 |
| candle | 1.000000 | 1.000000 | 0.896 | 0.889 | 0.908 | 0.900 | 1 / 30 |

4 threads available; mean document length 174 tokens (max 256).
