#!/usr/bin/env python3
"""Print the RESULTS.md tables from the variants' results.jsonl files."""
import json
import os
import sys
from pathlib import Path

FLOW = (Path(__file__).resolve().parent.parent.parent / ".claude" / "hillclimb"
        / os.environ.get("MTE_FLOW", "memory-tool-use"))
METRICS = ["pass", "consulted_before_act", "query_before_act", "explicit_create", "implicit_capture",
           "revise", "no_false_create", "no_spurious_revise", "fact_used", "no_stale_fact"]
HEAD = ["pass", "consulted", "query first", "explicit create", "implicit capture", "revise",
        "no false create", "no spurious revise", "fact used", "no stale fact", "revise (repo neutral)", "revise (repo disagrees)"]


def row(variant, model, change):
    rows = [json.loads(l) for l in (FLOW / variant / "results.jsonl").read_text().splitlines() if l.strip()]
    cells = []
    for m in METRICS:
        x = [r["grade"][m] for r in rows if m in r["grade"]]
        cells.append(f"{100 * sum(x) / len(x):.0f}% ({sum(x)}/{len(x)})" if x else "-")
    for tag in ("repo_neutral", "repo_disagrees"):
        x = [r["grade"]["revise"] for r in rows if "revise" in r["grade"] and tag in r["tags"]]
        cells.append(f"{100 * sum(x) / len(x):.0f}% ({sum(x)}/{len(x)})" if x else "-")
    gaps = [r["meta"]["gaps"] for r in rows]
    reads = sum(1 for g in gaps if g["direct_store_access"] or g["bash_workarounds"])
    gets = sum(1 for g in gaps for c in g["memory_calls"] if c["op"] == "get")
    cost = sum(r["cost_usd"] for r in rows) / len(rows)
    tok = sum(r["in_tokens"] or 0 for r in rows) / len(rows) / 1000
    return f"| {variant} | {model} | {change} | " + " | ".join(cells) + f" | {reads} | {gets} | {cost:.3f} | {tok:.0f}k |"


def table(spec):
    out = ["| Variant | Model | Change | " + " | ".join(HEAD) + " | store-read runs | get calls | $/case | input tokens |",
           "|" + "---|" * (len(HEAD) + 7)]
    out += [row(*s) for s in spec if (FLOW / s[0] / "results.jsonl").exists()]
    return "\n".join(out)


if __name__ == "__main__":
    spec = json.loads(sys.argv[1])
    print(table(spec))
