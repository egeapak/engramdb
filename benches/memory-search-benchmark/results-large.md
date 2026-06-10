# EngramDB Memory Search & Lookup Benchmark

**Dataset**: 20 projects × 100 memories + 100 global = 2100 memories. **Timed iterations/cell**: 12.

MCP-driven matrix: real `engramdb serve` sessions over stdio JSON-RPC, exercising `query` (search) and `get` (lookup) tools.

- **execution**: `in_process` (each session loads its own ONNX embedding model) vs `daemon` (shared embedding host, model resident once)
- **target**: `local` project store vs `global` cross-project store
- **ops/iter**: mixed search+lookup operations per iteration
- **sessions**: parallel MCP sessions running the same workload

Latency is per-op (warm). Memory is resident set size (RSS) sampled after warmup; `rss_total` sums all session processes plus the shared daemon.

## Full matrix

| exec | target | ops | sess | startup (ms) | time-to-first-result (ms) | warm p50 (ms) | warm p95 (ms) | iter wall p50 (ms) | thru (ops/s) | sess RSS (MB) | daemon RSS (MB) | total RSS (MB) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| in_process | local | 1 | 1 | 1184.0 | 1238.5 | 50.5 | 52.51 | 50.8 | 20.1 | 393.0 | 0.0 | 393.0 |
| in_process | local | 1 | 2 | 1079.1 | 1212.5 | 43.26 | 45.75 | 45.2 | 44.7 | 785.7 | 0.0 | 785.7 |
| in_process | local | 1 | 4 | 1081.2 | 1183.3 | 59.0 | 67.43 | 66.15 | 60.2 | 1569.9 | 0.0 | 1569.9 |
| in_process | local | 2 | 1 | 1003.0 | 1050.6 | 22.37 | 44.73 | 46.29 | 42.8 | 392.7 | 0.0 | 392.7 |
| in_process | local | 2 | 2 | 1120.7 | 1192.9 | 21.08 | 56.93 | 59.03 | 67.8 | 784.4 | 0.0 | 784.4 |
| in_process | local | 2 | 4 | 1067.3 | 1298.6 | 29.83 | 82.18 | 77.71 | 100.6 | 1570.0 | 0.0 | 1570.0 |
| in_process | local | 4 | 1 | 1105.1 | 1158.8 | 23.56 | 79.73 | 133.61 | 29.4 | 397.1 | 0.0 | 397.1 |
| in_process | local | 4 | 2 | 1158.8 | 1278.9 | 24.12 | 91.03 | 156.29 | 50.9 | 795.2 | 0.0 | 795.2 |
| in_process | local | 4 | 4 | 1113.8 | 1356.1 | 26.6 | 102.79 | 177.79 | 90.5 | 1589.5 | 0.0 | 1589.5 |
| in_process | global | 1 | 1 | 1010.3 | 1076.7 | 60.16 | 62.4 | 60.43 | 16.5 | 396.7 | 0.0 | 396.7 |
| in_process | global | 1 | 2 | 997.6 | 1101.1 | 72.54 | 78.4 | 74.91 | 26.4 | 794.2 | 0.0 | 794.2 |
| in_process | global | 1 | 4 | 1074.5 | 1251.1 | 104.58 | 118.8 | 116.62 | 34.9 | 1589.0 | 0.0 | 1589.0 |
| in_process | global | 2 | 1 | 958.4 | 1026.3 | 30.49 | 59.04 | 61.82 | 32.4 | 397.2 | 0.0 | 397.2 |
| in_process | global | 2 | 2 | 1004.4 | 1115.0 | 35.43 | 79.94 | 80.34 | 49.5 | 794.5 | 0.0 | 794.5 |
| in_process | global | 2 | 4 | 1037.6 | 1214.3 | 46.26 | 119.54 | 117.45 | 68.2 | 1589.4 | 0.0 | 1589.4 |
| in_process | global | 4 | 1 | 981.6 | 1055.0 | 30.18 | 61.41 | 126.47 | 31.6 | 397.7 | 0.0 | 397.7 |
| in_process | global | 4 | 2 | 996.2 | 1133.9 | 37.52 | 93.11 | 163.01 | 46.6 | 796.8 | 0.0 | 796.8 |
| in_process | global | 4 | 4 | 1243.2 | 1569.1 | 49.86 | 133.75 | 263.52 | 60.8 | 1593.7 | 0.0 | 1593.7 |
| daemon | local | 1 | 1 | 17.5 | 63.0 | 46.39 | 47.61 | 46.71 | 21.5 | 42.2 | 379.3 | 421.5 |
| daemon | local | 1 | 2 | 16.0 | 82.7 | 51.88 | 57.83 | 57.06 | 35.3 | 83.7 | 379.4 | 463.1 |
| daemon | local | 1 | 4 | 17.9 | 130.5 | 63.81 | 78.28 | 74.25 | 55.4 | 167.9 | 379.4 | 547.4 |
| daemon | local | 2 | 1 | 15.7 | 60.0 | 22.43 | 47.45 | 47.53 | 40.8 | 42.0 | 379.4 | 421.4 |
| daemon | local | 2 | 2 | 20.2 | 99.2 | 26.26 | 59.92 | 60.16 | 65.3 | 84.2 | 379.4 | 463.7 |
| daemon | local | 2 | 4 | 17.1 | 153.0 | 23.75 | 75.26 | 76.37 | 104.0 | 167.2 | 379.4 | 546.7 |
| daemon | local | 4 | 1 | 16.0 | 66.5 | 27.51 | 85.69 | 144.93 | 27.4 | 45.7 | 379.6 | 425.4 |
| daemon | local | 4 | 2 | 17.0 | 141.0 | 25.06 | 101.62 | 164.38 | 48.8 | 91.9 | 379.8 | 471.7 |
| daemon | local | 4 | 4 | 18.4 | 284.3 | 34.77 | 136.68 | 231.47 | 69.7 | 182.1 | 379.8 | 561.9 |
| daemon | global | 1 | 1 | 17.2 | 98.6 | 78.97 | 84.01 | 79.31 | 12.9 | 45.2 | 379.9 | 425.0 |
| daemon | global | 1 | 2 | 16.2 | 117.9 | 88.46 | 103.11 | 95.15 | 21.5 | 90.8 | 379.9 | 470.6 |
| daemon | global | 1 | 4 | 18.4 | 219.0 | 123.67 | 141.07 | 137.2 | 29.2 | 180.7 | 379.9 | 560.6 |
| daemon | global | 2 | 1 | 17.1 | 100.0 | 39.45 | 77.53 | 80.7 | 24.7 | 45.7 | 380.0 | 425.7 |
| daemon | global | 2 | 2 | 16.5 | 145.7 | 41.75 | 98.59 | 98.41 | 40.4 | 90.6 | 380.0 | 470.6 |
| daemon | global | 2 | 4 | 18.5 | 218.3 | 45.69 | 137.51 | 138.4 | 58.0 | 181.1 | 380.0 | 561.1 |
| daemon | global | 4 | 1 | 17.1 | 98.3 | 38.24 | 84.14 | 169.99 | 23.9 | 46.0 | 380.1 | 426.1 |
| daemon | global | 4 | 2 | 16.6 | 184.9 | 41.45 | 101.97 | 207.12 | 39.2 | 94.0 | 380.1 | 474.2 |
| daemon | global | 4 | 4 | 19.6 | 360.4 | 57.01 | 140.16 | 286.6 | 56.1 | 187.4 | 380.2 | 567.6 |

## Memory scaling: in_process vs daemon (local, ops/iter=1)

| sessions | in_process total RSS (MB) | daemon total RSS (MB) | RSS saved (MB) | savings % |
|---|---|---|---|---|
| 1 | 393.0 | 421.5 | -28.5 | -7.3% |
| 2 | 785.7 | 463.1 | 322.6 | 41.1% |
| 4 | 1569.9 | 547.4 | 1022.5 | 65.1% |

## Cold-start amortization: spawn -> first result (sessions=1)

Time from launching an MCP session to its first search result. In-process pays the embedding-model load every session; the daemon loads once machine-wide so sessions just connect.

| target | ops | in_process ttfr (ms) | daemon ttfr (ms) | speedup |
|---|---|---|---|---|
| local | 1 | 1238.5 | 63.0 | 19.66x |
| local | 2 | 1050.6 | 60.0 | 17.51x |
| local | 4 | 1158.8 | 66.5 | 17.43x |
| global | 1 | 1076.7 | 98.6 | 10.92x |
| global | 2 | 1026.3 | 100.0 | 10.26x |
| global | 4 | 1055.0 | 98.3 | 10.73x |

## Throughput under parallel sessions (local, ops/iter=4)

| sessions | in_process (ops/s) | daemon (ops/s) |
|---|---|---|
| 1 | 29.4 | 27.4 |
| 2 | 50.9 | 48.8 |
| 4 | 90.5 | 69.7 |

## Notes

- `cold 1st op` for `in_process` includes one-time ONNX model load; the daemon pays that once machine-wide, so its cold op is just an IPC round-trip plus inference.

- `lookup` (`get`) ops do no embedding, so search-heavy workloads show the largest daemon benefit; lookup-only latency is dominated by disk + IPC.

- Memory savings grow with session count: in_process RSS scales ~linearly with sessions (one model per session); daemon keeps a single resident model.

