# EngramDB Memory Search & Lookup Benchmark

**Workload**: save. **Store**: 20 projects × 100 memories + 100 global = 2100 memories. **Timed iterations/cell**: 12.

MCP-driven matrix: real `engramdb serve` sessions over stdio JSON-RPC, exercising the `create` tool (save a memory). `create` takes the per-project write lock and embeds in the background; creates insert into a pre-seeded store.

- **execution**: `in_process` (each session loads its own ONNX embedding model) vs `daemon` (shared embedding host, model resident once)
- **target**: `local` project store vs `global` cross-project store
- **ops/iter**: mixed search+lookup operations per iteration
- **sessions**: parallel MCP sessions running the same workload

Latency is per-op (warm). Memory is resident set size (RSS) sampled after warmup; `rss_total` sums all session processes plus the shared daemon.

## Full matrix

| exec | target | ops | sess | startup (ms) | time-to-first-result (ms) | warm p50 (ms) | warm p95 (ms) | iter wall p50 (ms) | thru (ops/s) | sess RSS (MB) | daemon RSS (MB) | total RSS (MB) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| in_process | local | 1 | 1 | 1119.1 | 1172.5 | 65.5 | 81.93 | 65.83 | 15.3 | 397.0 | 0.0 | 397.0 |
| in_process | local | 1 | 2 | 1103.9 | 1186.9 | 127.6 | 148.44 | 139.69 | 13.9 | 793.5 | 0.0 | 793.5 |
| in_process | local | 1 | 4 | 1325.2 | 1513.9 | 292.26 | 428.33 | 402.79 | 10.0 | 1588.9 | 0.0 | 1588.9 |
| in_process | local | 2 | 1 | 1082.9 | 1147.4 | 94.47 | 122.7 | 192.88 | 10.3 | 399.0 | 0.0 | 399.0 |
| in_process | local | 2 | 2 | 1106.6 | 1227.9 | 146.0 | 203.66 | 336.38 | 11.8 | 797.9 | 0.0 | 797.9 |
| in_process | local | 2 | 4 | 1187.1 | 1503.6 | 330.54 | 495.27 | 888.71 | 9.6 | 1598.2 | 0.0 | 1598.2 |
| in_process | local | 4 | 1 | 1106.3 | 1169.1 | 98.01 | 126.57 | 395.75 | 10.4 | 399.5 | 0.0 | 399.5 |
| in_process | local | 4 | 2 | 1031.7 | 1208.9 | 190.21 | 320.2 | 805.34 | 9.1 | 799.9 | 0.0 | 799.9 |
| in_process | local | 4 | 4 | 1143.4 | 1688.9 | 532.31 | 848.64 | 2416.68 | 6.5 | 1604.4 | 0.0 | 1604.4 |
| in_process | global | 1 | 1 | 975.4 | 1040.3 | 88.65 | 93.6 | 89.03 | 11.1 | 397.0 | 0.0 | 397.0 |
| in_process | global | 1 | 2 | 1032.6 | 1180.0 | 163.81 | 196.26 | 183.45 | 10.6 | 793.8 | 0.0 | 793.8 |
| in_process | global | 1 | 4 | 1093.9 | 1296.6 | 297.3 | 443.06 | 420.69 | 9.7 | 1591.1 | 0.0 | 1591.1 |
| in_process | global | 2 | 1 | 1055.6 | 1104.6 | 74.04 | 92.02 | 138.78 | 13.8 | 397.8 | 0.0 | 397.8 |
| in_process | global | 2 | 2 | 1097.2 | 1223.8 | 133.23 | 181.9 | 311.74 | 12.8 | 798.3 | 0.0 | 798.3 |
| in_process | global | 2 | 4 | 1132.9 | 1429.1 | 342.19 | 512.69 | 856.37 | 9.3 | 1599.0 | 0.0 | 1599.0 |
| in_process | global | 4 | 1 | 989.2 | 1049.6 | 97.97 | 116.67 | 386.37 | 10.3 | 400.3 | 0.0 | 400.3 |
| in_process | global | 4 | 2 | 1031.1 | 1213.4 | 197.63 | 287.39 | 874.28 | 9.7 | 801.3 | 0.0 | 801.3 |
| in_process | global | 4 | 4 | 1086.4 | 1648.1 | 474.59 | 645.26 | 2097.46 | 7.8 | 1605.1 | 0.0 | 1605.1 |
| daemon | local | 1 | 1 | 16.7 | 74.5 | 71.11 | 91.64 | 71.46 | 14.0 | 44.9 | 379.0 | 423.9 |
| daemon | local | 1 | 2 | 14.9 | 88.2 | 129.53 | 175.57 | 142.94 | 12.9 | 90.1 | 380.0 | 470.1 |
| daemon | local | 1 | 4 | 20.1 | 207.0 | 286.58 | 426.23 | 390.11 | 10.2 | 182.6 | 380.1 | 562.7 |
| daemon | local | 2 | 1 | 16.0 | 66.7 | 74.77 | 92.73 | 143.81 | 13.6 | 46.3 | 380.2 | 426.5 |
| daemon | local | 2 | 2 | 15.7 | 134.3 | 135.89 | 206.84 | 304.17 | 12.4 | 93.6 | 380.2 | 473.9 |
| daemon | local | 2 | 4 | 14.7 | 367.7 | 379.43 | 547.19 | 918.69 | 8.5 | 189.2 | 380.2 | 569.5 |
| daemon | local | 4 | 1 | 16.3 | 65.6 | 84.74 | 117.52 | 315.11 | 11.5 | 48.3 | 380.2 | 428.6 |
| daemon | local | 4 | 2 | 17.4 | 240.8 | 220.93 | 298.27 | 980.63 | 8.4 | 96.7 | 380.3 | 477.0 |
| daemon | local | 4 | 4 | 18.5 | 536.4 | 507.16 | 735.53 | 2372.04 | 7.0 | 194.4 | 380.3 | 574.8 |
| daemon | global | 1 | 1 | 16.0 | 64.5 | 66.02 | 71.33 | 66.37 | 15.4 | 45.0 | 380.3 | 425.3 |
| daemon | global | 1 | 2 | 15.6 | 86.6 | 113.34 | 152.53 | 127.3 | 15.0 | 90.1 | 380.3 | 470.4 |
| daemon | global | 1 | 4 | 13.5 | 154.9 | 220.89 | 323.57 | 306.13 | 13.2 | 181.9 | 380.3 | 562.2 |
| daemon | global | 2 | 1 | 16.0 | 65.9 | 89.11 | 110.49 | 174.93 | 11.5 | 46.9 | 380.3 | 427.2 |
| daemon | global | 2 | 2 | 15.6 | 121.0 | 133.39 | 165.51 | 298.96 | 13.3 | 94.5 | 380.3 | 474.8 |
| daemon | global | 2 | 4 | 19.0 | 274.2 | 309.16 | 512.34 | 801.5 | 9.8 | 189.2 | 380.3 | 569.6 |
| daemon | global | 4 | 1 | 18.0 | 78.2 | 92.9 | 125.23 | 381.07 | 10.6 | 47.6 | 380.3 | 428.0 |
| daemon | global | 4 | 2 | 19.0 | 251.9 | 204.99 | 295.33 | 870.98 | 9.0 | 97.3 | 380.3 | 477.6 |
| daemon | global | 4 | 4 | 17.4 | 566.3 | 549.55 | 893.78 | 2565.42 | 6.4 | 193.3 | 380.3 | 573.6 |

## Memory scaling: in_process vs daemon (local, ops/iter=1)

| sessions | in_process total RSS (MB) | daemon total RSS (MB) | RSS saved (MB) | savings % |
|---|---|---|---|---|
| 1 | 397.0 | 423.9 | -26.9 | -6.8% |
| 2 | 793.5 | 470.1 | 323.4 | 40.8% |
| 4 | 1588.9 | 562.7 | 1026.2 | 64.6% |

## Cold-start amortization: spawn -> first result (sessions=1)

Time from launching an MCP session to its first search result. In-process pays the embedding-model load every session; the daemon loads once machine-wide so sessions just connect.

| target | ops | in_process ttfr (ms) | daemon ttfr (ms) | speedup |
|---|---|---|---|---|
| local | 1 | 1172.5 | 74.5 | 15.74x |
| local | 2 | 1147.4 | 66.7 | 17.2x |
| local | 4 | 1169.1 | 65.6 | 17.82x |
| global | 1 | 1040.3 | 64.5 | 16.13x |
| global | 2 | 1104.6 | 65.9 | 16.76x |
| global | 4 | 1049.6 | 78.2 | 13.42x |

## Throughput under parallel sessions (local, ops/iter=4)

| sessions | in_process (ops/s) | daemon (ops/s) |
|---|---|---|
| 1 | 10.4 | 11.5 |
| 2 | 9.1 | 8.4 |
| 4 | 6.5 | 7.0 |

## Notes

- `cold 1st op` for `in_process` includes one-time ONNX model load; the daemon pays that once machine-wide, so its cold op is just an IPC round-trip plus inference.

- `lookup` (`get`) ops do no embedding, so search-heavy workloads show the largest daemon benefit; lookup-only latency is dominated by disk + IPC.

- Memory savings grow with session count: in_process RSS scales ~linearly with sessions (one model per session); daemon keeps a single resident model.

