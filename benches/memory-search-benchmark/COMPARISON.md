# Save vs. Search — benchmark comparison

Both workloads were run through the same MCP-driven matrix (in-process vs
shared daemon × local/global × 1/2/4 ops × 1/2/4 parallel sessions), at matched
iteration counts. Search exercises `query` + `get` (lock-free reads); save
exercises `create` (takes the per-project write lock; embeds in the background).

| run | workload | tool(s) | iters/cell | store |
|---|---|---|---|---|
| `results-highiter` | search | query, get | 50 | 10×10 + 10 global |
| `results-save-highiter` | save | create | 50 | empty (reset per cell) |
| `results-large` | search | query, get | 12 | 20×100 + 100 global (2100) |
| `results-save-large` | save | create | 12 | seeded 100 local / 100 global |

All numbers below are `sessions=1, ops/iter=1, local` unless noted. Hardware:
4-core sandbox, int8 all-MiniLM-L6-v2.

## Cold-start: spawn → first result (model-load amortization)

Identical story for save and search — the embedding model loads once in the
daemon vs once per in-process session, regardless of workload:

| run | in-process | daemon | speedup |
|---|---|---|---|
| search 50it | 1060 ms | 23 ms | 47× |
| save 50it | 1132 ms | 27 ms | 43× |
| search 12it (2100) | 1239 ms | 63 ms | 20× |
| save 12it (seeded) | 1173 ms | 75 ms | 16× |

## Warm latency & throughput vs. parallel sessions

The defining difference. Search is lock-free and **scales up** with sessions;
save takes the per-project write lock and **degrades** with sessions.

| run | mode | p50 s1 | p50 s2 | p50 s4 | thru s1 | thru s2 | thru s4 |
|---|---|---|---|---|---|---|---|
| search 50it | in-process | 7.9 ms | 8.4 ms | 10.0 ms | 123 | 214 | 308 |
| **save 50it** | in-process | 26.2 ms | 63.9 ms | 184.9 ms | 35 | 24 | **15** |
| search 12it (2100) | in-process | 50.5 ms | 43.3 ms | 59.0 ms | 20 | 45 | 60 |
| **save 12it (seeded)** | in-process | 65.5 ms | 127.6 ms | 292.3 ms | 15 | 14 | **10** |

- **Search throughput rises** with parallel sessions (123→308 ops/s); **save
  throughput falls** (35→15 ops/s) — every additional writer just queues on the
  same `flock`.
- **In-process ≈ daemon for warm save latency** (26.2 vs 25.3 ms at s1): `create`
  returns after writing the file + index row and embeds in the *background*, so
  the embedding backend is off the critical path. The daemon does not speed up
  the act of saving — its save win is purely cold-start + memory.
- **Saving costs more than searching** at every point (e.g. 26 ms vs 8 ms warm,
  s1), and **saving into a populated store costs more than into an empty one**
  (65 ms seeded vs 26 ms empty at s1) — the write-lock is held longer while the
  larger index is updated.

## Memory (total RSS, local, ops/iter=1)

Workload-independent — RSS is dominated by the model, not the data or the
operation. The daemon keeps one resident model; in-process loads one per session:

| sessions | in-process | daemon | saved |
|---|---|---|---|
| 1 | ~390 MB | ~420 MB | −8% |
| 2 | ~785 MB | ~460 MB | 41% |
| 4 | ~1570 MB | ~545 MB | **65%** |

## Takeaways

1. **Daemon cold-start and memory wins are the same for saving and searching** —
   they come from model sharing, which both workloads need.
2. **Saving does not benefit from the daemon on the warm path** (async embed
   off the critical path), whereas searching's warm path is also ~parity because
   IPC ≈ a local model call at small corpus sizes.
3. **Concurrency cuts the opposite way**: parallel sessions speed up search
   (lock-free) but slow down saving (per-project write lock). Agents that save
   heavily and concurrently to one project will serialize regardless of daemon.
4. **Both get slower with more data**: search with a larger corpus, save with a
   more populated index.
