# EngramDB Memory Search & Lookup Benchmark

MCP-driven matrix: real `engramdb serve` sessions over stdio JSON-RPC, exercising `query` (search) and `get` (lookup) tools.

- **execution**: `in_process` (each session loads its own ONNX embedding model) vs `daemon` (shared embedding host, model resident once)
- **target**: `local` project store vs `global` cross-project store
- **ops/iter**: mixed search+lookup operations per iteration
- **sessions**: parallel MCP sessions running the same workload

Latency is per-op (warm). Memory is resident set size (RSS) sampled after warmup; `rss_total` sums all session processes plus the shared daemon.

## Full matrix

| exec | target | ops | sess | startup (ms) | time-to-first-result (ms) | warm p50 (ms) | warm p95 (ms) | iter wall p50 (ms) | thru (ops/s) | sess RSS (MB) | daemon RSS (MB) | total RSS (MB) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| in_process | local | 1 | 1 | 867.3 | 876.3 | 6.01 | 6.65 | 6.19 | 155.2 | 388.7 | 0.0 | 388.7 |
| in_process | local | 1 | 2 | 858.3 | 871.0 | 6.44 | 7.84 | 6.97 | 276.1 | 776.9 | 0.0 | 776.9 |
| in_process | local | 1 | 4 | 1062.8 | 1094.0 | 7.21 | 9.21 | 9.1 | 444.2 | 1553.7 | 0.0 | 1553.7 |
| in_process | local | 2 | 1 | 925.7 | 934.3 | 3.57 | 6.36 | 7.49 | 262.0 | 389.0 | 0.0 | 389.0 |
| in_process | local | 2 | 2 | 821.6 | 841.3 | 3.39 | 6.29 | 7.39 | 531.8 | 778.2 | 0.0 | 778.2 |
| in_process | local | 2 | 4 | 895.4 | 918.7 | 3.76 | 10.57 | 11.87 | 676.7 | 1556.9 | 0.0 | 1556.9 |
| in_process | local | 4 | 1 | 834.1 | 843.4 | 4.41 | 13.67 | 23.95 | 163.5 | 392.1 | 0.0 | 392.1 |
| in_process | local | 4 | 2 | 907.8 | 934.3 | 4.42 | 17.45 | 29.36 | 282.8 | 782.8 | 0.0 | 782.8 |
| in_process | local | 4 | 4 | 959.4 | 1060.7 | 5.71 | 25.5 | 42.72 | 355.5 | 1567.0 | 0.0 | 1567.0 |
| in_process | global | 1 | 1 | 873.2 | 889.4 | 12.76 | 13.73 | 12.95 | 77.1 | 391.2 | 0.0 | 391.2 |
| in_process | global | 1 | 2 | 902.5 | 926.4 | 13.15 | 15.32 | 14.11 | 140.0 | 781.7 | 0.0 | 781.7 |
| in_process | global | 1 | 4 | 880.5 | 936.3 | 17.01 | 19.66 | 19.06 | 206.1 | 1566.4 | 0.0 | 1566.4 |
| in_process | global | 2 | 1 | 882.1 | 898.8 | 6.79 | 12.63 | 13.83 | 143.0 | 391.3 | 0.0 | 391.3 |
| in_process | global | 2 | 2 | 836.5 | 863.7 | 7.43 | 16.72 | 16.9 | 230.4 | 782.0 | 0.0 | 782.0 |
| in_process | global | 2 | 4 | 849.8 | 903.5 | 6.04 | 19.37 | 21.17 | 389.4 | 1565.5 | 0.0 | 1565.5 |
| in_process | global | 4 | 1 | 847.8 | 866.2 | 6.88 | 14.22 | 29.94 | 132.1 | 392.4 | 0.0 | 392.4 |
| in_process | global | 4 | 2 | 826.9 | 858.1 | 7.09 | 16.29 | 29.68 | 256.9 | 785.0 | 0.0 | 785.0 |
| in_process | global | 4 | 4 | 880.4 | 956.9 | 7.84 | 20.76 | 44.3 | 372.6 | 1567.2 | 0.0 | 1567.2 |
| daemon | local | 1 | 1 | 10.9 | 19.4 | 6.59 | 7.3 | 6.79 | 144.6 | 38.2 | 379.7 | 417.9 |
| daemon | local | 1 | 2 | 10.8 | 22.9 | 6.92 | 7.41 | 7.32 | 273.6 | 76.1 | 379.7 | 455.8 |
| daemon | local | 1 | 4 | 12.6 | 35.4 | 8.82 | 12.17 | 11.07 | 348.3 | 153.0 | 379.8 | 532.8 |
| daemon | local | 2 | 1 | 11.6 | 19.8 | 3.8 | 6.95 | 7.82 | 250.8 | 38.3 | 379.8 | 418.1 |
| daemon | local | 2 | 2 | 10.8 | 24.3 | 3.57 | 7.93 | 8.15 | 474.1 | 76.2 | 379.8 | 456.0 |
| daemon | local | 2 | 4 | 13.1 | 36.2 | 4.38 | 10.55 | 11.89 | 646.4 | 152.1 | 379.8 | 531.9 |
| daemon | local | 4 | 1 | 11.3 | 19.8 | 4.06 | 14.25 | 23.92 | 168.3 | 40.1 | 380.0 | 420.1 |
| daemon | local | 4 | 2 | 10.5 | 31.3 | 4.69 | 15.95 | 26.74 | 297.0 | 79.9 | 380.1 | 460.0 |
| daemon | local | 4 | 4 | 13.4 | 64.2 | 5.39 | 23.31 | 37.45 | 418.0 | 160.2 | 380.4 | 540.5 |
| daemon | global | 1 | 1 | 12.1 | 28.1 | 14.66 | 15.36 | 14.87 | 66.9 | 40.0 | 380.8 | 420.8 |
| daemon | global | 1 | 2 | 11.6 | 37.3 | 14.44 | 16.77 | 15.43 | 127.5 | 79.2 | 380.8 | 460.0 |
| daemon | global | 1 | 4 | 12.8 | 53.3 | 19.54 | 22.42 | 21.82 | 179.7 | 158.7 | 380.8 | 539.6 |
| daemon | global | 2 | 1 | 11.5 | 27.9 | 7.55 | 14.28 | 15.75 | 129.1 | 39.6 | 380.9 | 420.4 |
| daemon | global | 2 | 2 | 11.6 | 37.2 | 7.02 | 15.65 | 16.07 | 240.1 | 79.6 | 380.9 | 460.5 |
| daemon | global | 2 | 4 | 13.2 | 56.1 | 8.11 | 21.6 | 23.49 | 335.0 | 159.7 | 380.9 | 540.6 |
| daemon | global | 4 | 1 | 13.3 | 28.6 | 7.05 | 15.36 | 32.19 | 125.1 | 40.2 | 380.9 | 421.1 |
| daemon | global | 4 | 2 | 11.0 | 44.9 | 7.47 | 15.69 | 33.01 | 240.3 | 80.0 | 380.9 | 460.9 |
| daemon | global | 4 | 4 | 12.2 | 75.3 | 8.65 | 23.0 | 43.47 | 352.5 | 159.9 | 380.9 | 540.8 |

## Memory scaling: in_process vs daemon (local, ops/iter=1)

| sessions | in_process total RSS (MB) | daemon total RSS (MB) | RSS saved (MB) | savings % |
|---|---|---|---|---|
| 1 | 388.7 | 417.9 | -29.2 | -7.5% |
| 2 | 776.9 | 455.8 | 321.1 | 41.3% |
| 4 | 1553.7 | 532.8 | 1020.9 | 65.7% |

## Cold-start amortization: spawn -> first result (sessions=1)

Time from launching an MCP session to its first search result. In-process pays the embedding-model load every session; the daemon loads once machine-wide so sessions just connect.

| target | ops | in_process ttfr (ms) | daemon ttfr (ms) | speedup |
|---|---|---|---|---|
| local | 1 | 876.3 | 19.4 | 45.17x |
| local | 2 | 934.3 | 19.8 | 47.19x |
| local | 4 | 843.4 | 19.8 | 42.6x |
| global | 1 | 889.4 | 28.1 | 31.65x |
| global | 2 | 898.8 | 27.9 | 32.22x |
| global | 4 | 866.2 | 28.6 | 30.29x |

## Throughput under parallel sessions (local, ops/iter=4)

| sessions | in_process (ops/s) | daemon (ops/s) |
|---|---|---|
| 1 | 163.5 | 168.3 |
| 2 | 282.8 | 297.0 |
| 4 | 355.5 | 418.0 |

## Notes

- `cold 1st op` for `in_process` includes one-time ONNX model load; the daemon pays that once machine-wide, so its cold op is just an IPC round-trip plus inference.

- `lookup` (`get`) ops do no embedding, so search-heavy workloads show the largest daemon benefit; lookup-only latency is dominated by disk + IPC.

- Memory savings grow with session count: in_process RSS scales ~linearly with sessions (one model per session); daemon keeps a single resident model.

