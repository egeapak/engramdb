#!/usr/bin/env python3
"""Retrieval probe: replay real agent queries against the seeded eval fixture.

queries.json holds every `query` call agents made during the memory-tool-use
eval (default fixture), each labeled with the case's target memory, or with
no target for cases where no stored memory is relevant.

Each query runs through the CLI (`engramdb query --format json`) against a
freshly seeded copy of evals/memory-tool-use/fixture, with the default
config. The report gives, per mode the query is run in:

    recall        target memory anywhere in the results
    first         target memory ranked first
    empty         no results at all
    weak          every result marked below_threshold (rank fallback)
    noise         non-target results per labeled query
    no-match n    results per query that has no relevant memory

Usage:
    python3 probe.py --bin target/release            # both modes, as the agent asked
    python3 probe.py --bin /tmp/mte-snapshots/v12/bin --mode rank
"""

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
import types
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
EVAL = HERE.parent / "memory-tool-use"
sys.path.insert(0, str(EVAL))
import run  # noqa: E402  (seeding helpers)


def seed(bin_dir):
    run.SNAPSHOT["bin"] = Path(bin_dir).resolve()
    args = types.SimpleNamespace(ort_dylib="/tmp/onnxruntime-linux-x64-1.24.2/lib/libonnxruntime.so")
    tmp = Path(tempfile.mkdtemp(prefix="retrieval-probe-"))
    env = run.case_env(tmp, args)
    ws, ids = run.setup_workspace(tmp, env)
    return tmp, ws, env, {v: k for k, v in ids.items()}


def query(ws, env, key_of, q, mode):
    cmd = ["engramdb", "query", "--format", "json", "--mode", mode, "--query", q["query"]]
    if q.get("path"):
        cmd += ["--path", q["path"]]
    if q.get("situation"):
        cmd += ["--situation", q["situation"]]
    p = subprocess.run(cmd, cwd=ws, env=env, capture_output=True, text=True, timeout=120)
    j = json.loads(p.stdout)
    return [{"key": key_of.get(m["memory"]["id"]), "weak": bool(m.get("below_threshold"))} for m in j["memories"]]


def report(label, rows):
    lab = [r for r in rows if r["target"]]
    neg = [r for r in rows if not r["target"]]
    if not lab:
        return
    keys = lambda r: [m["key"] for m in r["got"]]
    recall = sum(r["target"] in keys(r) for r in lab) / len(lab)
    first = sum(bool(r["got"]) and r["got"][0]["key"] == r["target"] for r in lab) / len(lab)
    empty = sum(not r["got"] for r in lab) / len(lab)
    weak = sum(bool(r["got"]) and all(m["weak"] for m in r["got"]) for r in lab) / len(lab)
    noise = sum(sum(k != r["target"] for k in keys(r)) for r in lab) / len(lab)
    neg_n = sum(len(r["got"]) for r in neg) / len(neg) if neg else 0.0
    print(f"{label:<28} n={len(lab):3}  recall {recall:5.0%}  first {first:5.0%}  empty {empty:5.0%}  "
          f"weak {weak:5.0%}  noise {noise:4.1f}  | no-match n={len(neg)} avg results {neg_n:4.1f}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--bin", required=True, help="directory holding the engramdb binary to probe")
    ap.add_argument("--mode", choices=["asked", "rank", "filter", "both"], default="both",
                    help="asked: the mode the agent used; rank/filter: force one; both: report each")
    ap.add_argument("--out", help="write per-query results as JSON here")
    args = ap.parse_args()

    queries = json.loads((HERE / "queries.json").read_text())
    tmp, ws, env, key_of = seed(args.bin)
    modes = {"both": ["rank", "filter"], "asked": [None]}.get(args.mode, [args.mode])
    results = {}
    try:
        for mode in modes:
            def one(q, mode=mode):
                m = mode or q["mode"]
                return {**q, "ran_as": m, "got": query(ws, env, key_of, q, m)}
            with ThreadPoolExecutor(3) as pool:
                results[mode or "asked"] = list(pool.map(one, queries))
    finally:
        subprocess.run(["engramdb", "daemon", "stop"], env=env, capture_output=True)
        shutil.rmtree(tmp, ignore_errors=True)

    for label, rows in results.items():
        report(f"all queries as {label}", rows)
        for asked in ("rank", "filter"):
            report(f"  agent asked {asked}", [r for r in rows if r["mode"] == asked])
    if args.out:
        Path(args.out).write_text(json.dumps(results, indent=1))


if __name__ == "__main__":
    main()
