#!/usr/bin/env python3
"""Render results.json into a Markdown report with comparison tables."""
import json
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
    print("MCP-driven matrix: real `engramdb serve` sessions over stdio JSON-RPC, "
          "exercising `query` (search) and `get` (lookup) tools.\n")
    print("- **execution**: `in_process` (each session loads its own ONNX embedding "
          "model) vs `daemon` (shared embedding host, model resident once)\n"
          "- **target**: `local` project store vs `global` cross-project store\n"
          "- **ops/iter**: mixed search+lookup operations per iteration\n"
          "- **sessions**: parallel MCP sessions running the same workload\n")
    print("Latency is per-op (warm). Memory is resident set size (RSS) sampled after "
          "warmup; `rss_total` sums all session processes plus the shared daemon.\n")

    # Full matrix table
    print("## Full matrix\n")
    hdr = ("| exec | target | ops | sess | cold 1st op (ms) | warm p50 (ms) | "
           "warm p95 (ms) | iter wall p50 (ms) | thru (ops/s) | sess RSS (MB) | "
           "daemon RSS (MB) | total RSS (MB) |")
    print(hdr)
    print("|" + "---|" * 12)
    for c in rows:
        print(f"| {c['execution']} | {c['target']} | {c['ops_per_iter']} | {c['sessions']} | "
              f"{c['cold_first_op_ms']} | {c['warm_op_p50_ms']} | {c['warm_op_p95_ms']} | "
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

    # Latency: cold first op (model-load cost) in_process vs daemon
    print("\n## Cold first-op latency: model-load amortization (sessions=1)\n")
    print("| target | ops | in_process cold (ms) | daemon cold (ms) | speedup |")
    print("|---|---|---|---|---|")
    for target in ("local", "global"):
        for ops in (1, 2, 4):
            ip = by.get(("in_process", target, ops, 1))
            dm = by.get(("daemon", target, ops, 1))
            if ip and dm and dm["cold_first_op_ms"]:
                sp = round(ip["cold_first_op_ms"] / dm["cold_first_op_ms"], 2)
                print(f"| {target} | {ops} | {ip['cold_first_op_ms']} | "
                      f"{dm['cold_first_op_ms']} | {sp}x |")

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
