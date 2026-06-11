#!/usr/bin/env python3
"""Render results.json into a Markdown report with comparison tables."""
import json
import os
import sys
from collections import defaultdict


def load(path):
    return json.loads(open(path).read())


def key(c):
    return (c["execution"], c["target"], c["ops_per_iter"], c["sessions"])


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else "results.json"
    rows = load(path)
    by = {key(c): c for c in rows}

    print("# EngramDB Memory Search & Lookup Benchmark\n")

    # Self-describing header: dataset size + iteration count, if discoverable.
    manifest = {}
    pdir = os.environ.get("BENCH_PROJECTS_DIR")
    if pdir and os.path.exists(os.path.join(pdir, "manifest.json")):
        manifest = json.load(open(os.path.join(pdir, "manifest.json")))
    iters = os.environ.get("BENCH_ITERS")
    workload = rows[0].get("workload", "search") if rows else "search"
    seeded = rows and (rows[0].get("save_seed") or rows[0].get("save_reset") is False)
    # Dataset size matters for search (corpus queried) and seeded saves (index
    # inserted into); for empty-store saves it does not.
    show_dataset = workload == "search" or (workload == "save" and seeded)
    iters_note = f"**Timed iterations/cell**: {iters}." if iters else ""
    if manifest and show_dataset:
        total = (manifest["n_projects"] * manifest["memories_per_project"]
                 + manifest["global_memories"])
        print(f"**Workload**: {workload}. "
              f"**Store**: {manifest['n_projects']} projects × "
              f"{manifest['memories_per_project']} memories + "
              f"{manifest['global_memories']} global = {total} memories. "
              f"{iters_note}\n")
    else:
        print(f"**Workload**: {workload}. **Store**: reset to empty before each "
              f"cell. {iters_note}\n")

    if workload == "save":
        store_note = ("creates insert into a pre-seeded store" if seeded
                      else "each cell starts from an empty store")
        print("MCP-driven matrix: real `engramdb serve` sessions over stdio "
              "JSON-RPC, exercising the `create` tool (save a memory). `create` "
              "takes the per-project write lock and embeds in the background; "
              f"{store_note}.\n")
    else:
        print("MCP-driven matrix: real `engramdb serve` sessions over stdio "
              "JSON-RPC, exercising `query` (search) and `get` (lookup) tools.\n")
    print("- **execution**: `in_process` (each session loads its own ONNX embedding "
          "model) vs `daemon` (shared embedding host, model resident once)\n"
          "- **target**: `local` project store vs `global` cross-project store\n"
          "- **ops/iter**: mixed search+lookup operations per iteration\n"
          "- **sessions**: parallel MCP sessions running the same workload\n")
    print("Latency is per-op (warm). Memory is resident set size (RSS) sampled after "
          "warmup; `rss_total` sums all session processes plus the shared daemon.\n")

    # Full matrix table
    print("## Full matrix\n")
    hdr = ("| exec | target | ops | sess | startup (ms) | time-to-first-result (ms) | "
           "warm p50 (ms) | warm p95 (ms) | iter wall p50 (ms) | thru (ops/s) | "
           "sess RSS (MB) | daemon RSS (MB) | total RSS (MB) |")
    print(hdr)
    print("|" + "---|" * 13)
    for c in rows:
        print(f"| {c['execution']} | {c['target']} | {c['ops_per_iter']} | {c['sessions']} | "
              f"{c['session_startup_ms']} | {c['time_to_first_result_ms']} | "
              f"{c['warm_op_p50_ms']} | {c['warm_op_p95_ms']} | "
              f"{c['iter_wall_p50_ms']} | {c['throughput_ops_s']} | "
              f"{c['rss_sessions_total_mb']} | {c['rss_daemon_mb']} | {c['rss_total_mb']} |")

    # In-process vs daemon: memory scaling with sessions (local, ops=1)
    print("\n## Memory scaling: in_process vs daemon (local, ops/iter=1)\n")
    print("| sessions | in_process total RSS (MB) | daemon total RSS (MB) | RSS saved (MB) | savings % |")
    print("|---|---|---|---|---|")
    for s in (1, 2, 4):
        ip = by.get(("in_process", "local", 1, s))
        dm = by.get(("daemon", "local", 1, s))
        if ip and dm:
            saved = round(ip["rss_total_mb"] - dm["rss_total_mb"], 1)
            pct = round(100 * saved / ip["rss_total_mb"], 1) if ip["rss_total_mb"] else 0
            print(f"| {s} | {ip['rss_total_mb']} | {dm['rss_total_mb']} | {saved} | {pct}% |")

    # Latency: cold-start (model-load) amortization, in_process vs daemon
    print("\n## Cold-start amortization: spawn -> first result (sessions=1)\n")
    print("Time from launching an MCP session to its first search result. "
          "In-process pays the embedding-model load every session; the daemon "
          "loads once machine-wide so sessions just connect.\n")
    print("| target | ops | in_process ttfr (ms) | daemon ttfr (ms) | speedup |")
    print("|---|---|---|---|---|")
    for target in ("local", "global"):
        for ops in (1, 2, 4):
            ip = by.get(("in_process", target, ops, 1))
            dm = by.get(("daemon", target, ops, 1))
            if ip and dm and dm["time_to_first_result_ms"]:
                sp = round(ip["time_to_first_result_ms"] / dm["time_to_first_result_ms"], 2)
                print(f"| {target} | {ops} | {ip['time_to_first_result_ms']} | "
                      f"{dm['time_to_first_result_ms']} | {sp}x |")

    # Throughput under parallelism
    print("\n## Throughput under parallel sessions (local, ops/iter=4)\n")
    print("| sessions | in_process (ops/s) | daemon (ops/s) |")
    print("|---|---|---|")
    for s in (1, 2, 4):
        ip = by.get(("in_process", "local", 4, s))
        dm = by.get(("daemon", "local", 4, s))
        if ip and dm:
            print(f"| {s} | {ip['throughput_ops_s']} | {dm['throughput_ops_s']} |")

    print("\n## Notes\n")
    print("- `cold 1st op` for `in_process` includes one-time ONNX model load; the "
          "daemon pays that once machine-wide, so its cold op is just an IPC round-trip "
          "plus inference.\n")
    print("- `lookup` (`get`) ops do no embedding, so search-heavy workloads show the "
          "largest daemon benefit; lookup-only latency is dominated by disk + IPC.\n")
    print("- Memory savings grow with session count: in_process RSS scales ~linearly "
          "with sessions (one model per session); daemon keeps a single resident model.\n")


if __name__ == "__main__":
    main()
