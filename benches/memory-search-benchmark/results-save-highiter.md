# EngramDB Memory Search & Lookup Benchmark

**Workload**: save. **Store**: reset to empty before each cell. **Timed iterations/cell**: 50.

MCP-driven matrix: real `engramdb serve` sessions over stdio JSON-RPC, exercising the `create` tool (save a memory). `create` takes the per-project write lock and embeds in the background; each cell starts from an empty store.

- **execution**: `in_process` (each session loads its own ONNX embedding model) vs `daemon` (shared embedding host, model resident once)
- **target**: `local` project store vs `global` cross-project store
- **ops/iter**: mixed search+lookup operations per iteration
- **sessions**: parallel MCP sessions running the same workload

Latency is per-op (warm). Memory is resident set size (RSS) sampled after warmup; `rss_total` sums all session processes plus the shared daemon.

## Full matrix

| exec | target | ops | sess | startup (ms) | time-to-first-result (ms) | warm p50 (ms) | warm p95 (ms) | iter wall p50 (ms) | thru (ops/s) | sess RSS (MB) | daemon RSS (MB) | total RSS (MB) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| in_process | local | 1 | 1 | 1115.3 | 1131.8 | 26.24 | 45.06 | 27.17 | 34.6 | 391.2 | 0.0 | 391.2 |
| in_process | local | 1 | 2 | 1068.4 | 1116.6 | 63.88 | 115.28 | 83.29 | 24.3 | 783.5 | 0.0 | 783.5 |
| in_process | local | 1 | 4 | 1062.1 | 1140.5 | 184.92 | 390.37 | 258.25 | 14.9 | 1572.0 | 0.0 | 1572.0 |
| in_process | local | 2 | 1 | 1024.5 | 1042.4 | 38.04 | 80.66 | 81.52 | 22.6 | 392.7 | 0.0 | 392.7 |
| in_process | local | 2 | 2 | 1012.5 | 1053.1 | 117.48 | 214.0 | 286.72 | 14.6 | 785.8 | 0.0 | 785.8 |
| in_process | local | 2 | 4 | 1094.2 | 1156.6 | 381.85 | 839.38 | 937.73 | 7.7 | 1576.1 | 0.0 | 1576.1 |
| in_process | local | 4 | 1 | 1071.7 | 1092.6 | 68.34 | 121.88 | 275.53 | 14.2 | 394.3 | 0.0 | 394.3 |
| in_process | local | 4 | 2 | 1114.2 | 1174.1 | 225.93 | 503.22 | 949.88 | 7.8 | 789.5 | 0.0 | 789.5 |
| in_process | local | 4 | 4 | 1062.1 | 1224.0 | 958.6 | 2102.08 | 4522.77 | 3.6 | 1581.5 | 0.0 | 1581.5 |
| in_process | global | 1 | 1 | 1060.8 | 1079.0 | 26.13 | 43.83 | 26.5 | 36.3 | 390.8 | 0.0 | 390.8 |
| in_process | global | 1 | 2 | 1098.8 | 1126.1 | 61.36 | 115.84 | 82.42 | 24.6 | 782.8 | 0.0 | 782.8 |
| in_process | global | 1 | 4 | 1207.5 | 1258.8 | 220.15 | 451.82 | 327.9 | 12.6 | 1569.3 | 0.0 | 1569.3 |
| in_process | global | 2 | 1 | 1006.8 | 1036.4 | 40.87 | 76.6 | 87.78 | 22.6 | 393.4 | 0.0 | 393.4 |
| in_process | global | 2 | 2 | 1049.2 | 1106.9 | 113.42 | 264.18 | 260.19 | 13.5 | 786.8 | 0.0 | 786.8 |
| in_process | global | 2 | 4 | 1074.4 | 1183.1 | 433.98 | 893.63 | 1226.86 | 7.2 | 1576.3 | 0.0 | 1576.3 |
| in_process | global | 4 | 1 | 1015.0 | 1047.0 | 68.11 | 139.49 | 279.31 | 12.8 | 394.1 | 0.0 | 394.1 |
| in_process | global | 4 | 2 | 1133.7 | 1205.7 | 276.68 | 520.2 | 1210.22 | 6.7 | 789.1 | 0.0 | 789.1 |
| in_process | global | 4 | 4 | 1083.9 | 1224.2 | 888.05 | 2107.66 | 4442.29 | 3.6 | 1580.1 | 0.0 | 1580.1 |
| daemon | local | 1 | 1 | 12.4 | 26.5 | 25.3 | 47.41 | 25.7 | 36.1 | 40.1 | 378.2 | 418.3 |
| daemon | local | 1 | 2 | 10.2 | 32.4 | 63.16 | 121.17 | 80.92 | 24.1 | 80.7 | 379.1 | 459.9 |
| daemon | local | 1 | 4 | 11.7 | 48.1 | 192.88 | 401.65 | 282.57 | 14.8 | 161.9 | 379.6 | 541.5 |
| daemon | local | 2 | 1 | 10.1 | 23.7 | 40.83 | 105.91 | 81.86 | 19.4 | 41.0 | 379.7 | 420.7 |
| daemon | local | 2 | 2 | 13.6 | 50.3 | 151.74 | 267.48 | 367.77 | 11.4 | 81.9 | 379.7 | 461.6 |
| daemon | local | 2 | 4 | 12.4 | 98.7 | 415.63 | 778.34 | 1070.99 | 7.6 | 165.4 | 379.7 | 545.1 |
| daemon | local | 4 | 1 | 10.2 | 25.2 | 67.44 | 142.25 | 270.92 | 13.3 | 41.7 | 380.1 | 421.8 |
| daemon | local | 4 | 2 | 10.9 | 60.0 | 255.29 | 524.58 | 1104.01 | 7.1 | 84.0 | 380.1 | 464.1 |
| daemon | local | 4 | 4 | 12.3 | 143.9 | 940.34 | 2110.75 | 4407.12 | 3.5 | 168.9 | 380.1 | 549.0 |
| daemon | global | 1 | 1 | 10.7 | 28.1 | 29.88 | 53.17 | 30.23 | 30.9 | 40.3 | 384.3 | 424.5 |
| daemon | global | 1 | 2 | 11.2 | 35.0 | 79.3 | 151.64 | 102.77 | 20.0 | 80.4 | 384.3 | 464.7 |
| daemon | global | 1 | 4 | 12.9 | 60.0 | 216.99 | 460.6 | 317.61 | 12.7 | 162.6 | 384.3 | 546.9 |
| daemon | global | 2 | 1 | 11.2 | 27.3 | 52.76 | 90.21 | 110.38 | 18.3 | 40.8 | 384.3 | 425.1 |
| daemon | global | 2 | 2 | 11.1 | 46.2 | 140.48 | 266.34 | 321.83 | 12.3 | 81.9 | 384.3 | 466.2 |
| daemon | global | 2 | 4 | 13.2 | 98.7 | 438.03 | 889.98 | 1164.81 | 7.1 | 165.5 | 384.3 | 549.8 |
| daemon | global | 4 | 1 | 11.5 | 29.1 | 86.21 | 143.56 | 346.66 | 11.6 | 41.5 | 384.3 | 425.8 |
| daemon | global | 4 | 2 | 10.2 | 61.8 | 237.33 | 507.04 | 1022.01 | 7.5 | 83.9 | 384.3 | 468.2 |
| daemon | global | 4 | 4 | 13.5 | 139.9 | 870.25 | 2131.79 | 3799.52 | 3.6 | 169.5 | 384.3 | 553.8 |

## Memory scaling: in_process vs daemon (local, ops/iter=1)

| sessions | in_process total RSS (MB) | daemon total RSS (MB) | RSS saved (MB) | savings % |
|---|---|---|---|---|
| 1 | 391.2 | 418.3 | -27.1 | -6.9% |
| 2 | 783.5 | 459.9 | 323.6 | 41.3% |
| 4 | 1572.0 | 541.5 | 1030.5 | 65.6% |

## Cold-start amortization: spawn -> first result (sessions=1)

Time from launching an MCP session to its first search result. In-process pays the embedding-model load every session; the daemon loads once machine-wide so sessions just connect.

| target | ops | in_process ttfr (ms) | daemon ttfr (ms) | speedup |
|---|---|---|---|---|
| local | 1 | 1131.8 | 26.5 | 42.71x |
| local | 2 | 1042.4 | 23.7 | 43.98x |
| local | 4 | 1092.6 | 25.2 | 43.36x |
| global | 1 | 1079.0 | 28.1 | 38.4x |
| global | 2 | 1036.4 | 27.3 | 37.96x |
| global | 4 | 1047.0 | 29.1 | 35.98x |

## Throughput under parallel sessions (local, ops/iter=4)

| sessions | in_process (ops/s) | daemon (ops/s) |
|---|---|---|
| 1 | 14.2 | 13.3 |
| 2 | 7.8 | 7.1 |
| 4 | 3.6 | 3.5 |

## Notes

- `cold 1st op` for `in_process` includes one-time ONNX model load; the daemon pays that once machine-wide, so its cold op is just an IPC round-trip plus inference.

- `lookup` (`get`) ops do no embedding, so search-heavy workloads show the largest daemon benefit; lookup-only latency is dominated by disk + IPC.

- Memory savings grow with session count: in_process RSS scales ~linearly with sessions (one model per session); daemon keeps a single resident model.

