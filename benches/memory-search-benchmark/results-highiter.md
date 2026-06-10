# EngramDB Memory Search & Lookup Benchmark

**Dataset**: 10 projects × 10 memories + 10 global = 110 memories. **Timed iterations/cell**: 50.

MCP-driven matrix: real `engramdb serve` sessions over stdio JSON-RPC, exercising `query` (search) and `get` (lookup) tools.

- **execution**: `in_process` (each session loads its own ONNX embedding model) vs `daemon` (shared embedding host, model resident once)
- **target**: `local` project store vs `global` cross-project store
- **ops/iter**: mixed search+lookup operations per iteration
- **sessions**: parallel MCP sessions running the same workload

Latency is per-op (warm). Memory is resident set size (RSS) sampled after warmup; `rss_total` sums all session processes plus the shared daemon.

## Full matrix

| exec | target | ops | sess | startup (ms) | time-to-first-result (ms) | warm p50 (ms) | warm p95 (ms) | iter wall p50 (ms) | thru (ops/s) | sess RSS (MB) | daemon RSS (MB) | total RSS (MB) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| in_process | local | 1 | 1 | 1047.8 | 1059.9 | 7.87 | 8.4 | 8.08 | 122.9 | 388.6 | 0.0 | 388.6 |
| in_process | local | 1 | 2 | 1173.9 | 1196.7 | 8.42 | 10.01 | 9.32 | 214.1 | 777.8 | 0.0 | 777.8 |
| in_process | local | 1 | 4 | 1386.0 | 1413.7 | 9.98 | 14.06 | 12.29 | 308.1 | 1556.9 | 0.0 | 1556.9 |
| in_process | local | 2 | 1 | 1103.2 | 1113.7 | 4.13 | 6.73 | 7.98 | 247.6 | 389.1 | 0.0 | 389.1 |
| in_process | local | 2 | 2 | 1165.8 | 1181.3 | 3.94 | 8.01 | 8.93 | 447.1 | 777.2 | 0.0 | 777.2 |
| in_process | local | 2 | 4 | 1251.5 | 1308.9 | 5.46 | 13.72 | 15.64 | 515.2 | 1555.1 | 0.0 | 1555.1 |
| in_process | local | 4 | 1 | 1123.9 | 1136.0 | 5.03 | 18.17 | 29.07 | 136.3 | 393.0 | 0.0 | 393.0 |
| in_process | local | 4 | 2 | 1048.1 | 1136.1 | 5.69 | 20.51 | 32.96 | 242.2 | 785.1 | 0.0 | 785.1 |
| in_process | local | 4 | 4 | 1214.0 | 1281.8 | 6.66 | 28.2 | 46.7 | 342.6 | 1567.2 | 0.0 | 1567.2 |
| in_process | global | 1 | 1 | 1151.2 | 1175.4 | 17.63 | 19.04 | 17.88 | 55.6 | 391.6 | 0.0 | 391.6 |
| in_process | global | 1 | 2 | 1095.2 | 1132.9 | 19.04 | 21.67 | 20.22 | 98.4 | 783.6 | 0.0 | 783.6 |
| in_process | global | 1 | 4 | 1189.1 | 1244.8 | 25.22 | 30.42 | 29.2 | 136.9 | 1566.2 | 0.0 | 1566.2 |
| in_process | global | 2 | 1 | 1120.8 | 1144.2 | 8.68 | 17.68 | 18.85 | 106.2 | 392.6 | 0.0 | 392.6 |
| in_process | global | 2 | 2 | 1218.6 | 1254.1 | 8.46 | 18.44 | 18.55 | 208.1 | 783.3 | 0.0 | 783.3 |
| in_process | global | 2 | 4 | 1505.9 | 1577.2 | 8.95 | 28.78 | 30.33 | 262.7 | 1567.4 | 0.0 | 1567.4 |
| in_process | global | 4 | 1 | 1131.9 | 1152.3 | 8.36 | 17.97 | 37.22 | 107.3 | 392.0 | 0.0 | 392.0 |
| in_process | global | 4 | 2 | 1192.9 | 1235.7 | 9.08 | 21.47 | 43.04 | 183.0 | 783.9 | 0.0 | 783.9 |
| in_process | global | 4 | 4 | 1249.9 | 1356.4 | 10.08 | 30.67 | 60.64 | 262.8 | 1567.6 | 0.0 | 1567.6 |
| daemon | local | 1 | 1 | 13.0 | 22.7 | 8.6 | 10.07 | 8.89 | 112.4 | 38.2 | 380.0 | 418.1 |
| daemon | local | 1 | 2 | 14.6 | 31.3 | 9.06 | 10.68 | 9.93 | 197.8 | 76.5 | 380.0 | 456.6 |
| daemon | local | 1 | 4 | 12.4 | 41.5 | 11.5 | 14.95 | 14.0 | 280.6 | 152.3 | 380.1 | 532.4 |
| daemon | local | 2 | 1 | 14.5 | 25.1 | 4.62 | 8.68 | 9.83 | 202.0 | 38.5 | 380.1 | 418.7 |
| daemon | local | 2 | 2 | 14.0 | 33.0 | 4.83 | 10.27 | 11.36 | 348.4 | 76.7 | 380.1 | 456.8 |
| daemon | local | 2 | 4 | 15.9 | 47.6 | 5.54 | 14.18 | 15.9 | 500.7 | 152.7 | 380.1 | 532.8 |
| daemon | local | 4 | 1 | 14.1 | 24.4 | 4.81 | 19.22 | 30.9 | 129.3 | 39.6 | 380.3 | 419.9 |
| daemon | local | 4 | 2 | 13.4 | 42.1 | 6.09 | 24.43 | 37.01 | 210.4 | 79.5 | 380.5 | 460.0 |
| daemon | local | 4 | 4 | 17.8 | 77.5 | 7.26 | 30.52 | 49.55 | 321.2 | 160.2 | 380.6 | 540.8 |
| daemon | global | 1 | 1 | 13.5 | 34.2 | 18.74 | 22.23 | 18.95 | 50.8 | 39.9 | 380.8 | 420.7 |
| daemon | global | 1 | 2 | 14.1 | 42.2 | 19.45 | 24.42 | 20.63 | 95.5 | 79.4 | 380.8 | 460.2 |
| daemon | global | 1 | 4 | 14.4 | 66.1 | 27.16 | 32.23 | 31.21 | 129.6 | 159.2 | 380.8 | 540.0 |
| daemon | global | 2 | 1 | 13.6 | 36.1 | 10.47 | 20.15 | 21.3 | 93.3 | 40.1 | 381.0 | 421.1 |
| daemon | global | 2 | 2 | 13.1 | 47.7 | 9.39 | 22.82 | 23.61 | 168.4 | 80.1 | 381.0 | 461.1 |
| daemon | global | 2 | 4 | 15.2 | 70.4 | 10.29 | 31.55 | 32.72 | 244.2 | 158.5 | 381.0 | 539.5 |
| daemon | global | 4 | 1 | 19.6 | 47.4 | 10.55 | 20.0 | 41.28 | 95.9 | 39.8 | 381.1 | 420.9 |
| daemon | global | 4 | 2 | 12.8 | 55.3 | 10.34 | 22.92 | 46.54 | 171.3 | 80.4 | 381.1 | 461.6 |
| daemon | global | 4 | 4 | 14.9 | 101.3 | 13.62 | 32.58 | 64.63 | 246.8 | 160.1 | 381.2 | 541.3 |

## Memory scaling: in_process vs daemon (local, ops/iter=1)

| sessions | in_process total RSS (MB) | daemon total RSS (MB) | RSS saved (MB) | savings % |
|---|---|---|---|---|
| 1 | 388.6 | 418.1 | -29.5 | -7.6% |
| 2 | 777.8 | 456.6 | 321.2 | 41.3% |
| 4 | 1556.9 | 532.4 | 1024.5 | 65.8% |

## Cold-start amortization: spawn -> first result (sessions=1)

Time from launching an MCP session to its first search result. In-process pays the embedding-model load every session; the daemon loads once machine-wide so sessions just connect.

| target | ops | in_process ttfr (ms) | daemon ttfr (ms) | speedup |
|---|---|---|---|---|
| local | 1 | 1059.9 | 22.7 | 46.69x |
| local | 2 | 1113.7 | 25.1 | 44.37x |
| local | 4 | 1136.0 | 24.4 | 46.56x |
| global | 1 | 1175.4 | 34.2 | 34.37x |
| global | 2 | 1144.2 | 36.1 | 31.7x |
| global | 4 | 1152.3 | 47.4 | 24.31x |

## Throughput under parallel sessions (local, ops/iter=4)

| sessions | in_process (ops/s) | daemon (ops/s) |
|---|---|---|
| 1 | 136.3 | 129.3 |
| 2 | 242.2 | 210.4 |
| 4 | 342.6 | 321.2 |

## Notes

- `cold 1st op` for `in_process` includes one-time ONNX model load; the daemon pays that once machine-wide, so its cold op is just an IPC round-trip plus inference.

- `lookup` (`get`) ops do no embedding, so search-heavy workloads show the largest daemon benefit; lookup-only latency is dominated by disk + IPC.

- Memory savings grow with session count: in_process RSS scales ~linearly with sessions (one model per session); daemon keeps a single resident model.

