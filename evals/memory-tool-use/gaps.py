#!/usr/bin/env python3
"""Summarize evidence of missing EngramDB tools, parameters, or commands.

Reads <variant>/results.jsonl (the `meta.gaps` and `meta.debrief` of each row)
and writes <variant>/gaps.md. Signals, strongest first:

- mcp_errors          a memory tool call failed: wrong parameter, validation error
- bash_workarounds    Claude ran `engramdb ...` or touched .engramdb/ through Bash,
                      i.e. it went around the MCP tools
- direct_store_access Read/Grep/Glob on .engramdb/ files
- unknown_tools       a tool name Claude tried that does not exist
- empty_queries       a query that returned nothing (retrieval miss or bad terms)
- debrief             Claude's own answer to "what did you want and not have?"
                      (self-report: a lead to check, not proof)

Usage: python3 gaps.py [variant]   (default: baseline)
"""

import collections
import json
import sys
from pathlib import Path

FLOW_DIR = Path(__file__).resolve().parent.parent.parent / ".claude" / "hillclimb" / "memory-tool-use"


def fence(text):
    ticks = "```"
    while ticks in text:
        ticks += "`"
    return f"{ticks}\n{text}\n{ticks}"


def main():
    variant = sys.argv[1] if len(sys.argv) > 1 else "baseline"
    out_dir = FLOW_DIR / variant
    rows = [json.loads(l) for l in (out_dir / "results.jsonl").read_text().splitlines() if l.strip()]
    rows.sort(key=lambda r: (r["prompt_id"], r["rep"]))

    ops = collections.Counter()
    by_kind = collections.defaultdict(list)
    debriefs = []
    for r in rows:
        key = f"{r['prompt_id']} rep{r['rep']}"
        g = r["meta"]["gaps"]
        ops.update(c["op"] for c in g["memory_calls"])
        for kind in ("mcp_errors", "bash_workarounds", "direct_store_access", "unknown_tools", "empty_queries"):
            for item in g[kind]:
                by_kind[kind].append((key, item))
        d = (r["meta"].get("debrief") or "").strip()
        if d and d.lower().rstrip(".") != "nothing missing":
            debriefs.append((key, d))

    n = len(rows)
    o = [f"# Tool gap evidence: {variant}", "",
         f"{n} graded runs. Memory tool calls by operation: "
         + (", ".join(f"`{k}` {v}" for k, v in ops.most_common()) or "none") + ".", "",
         "| signal | runs with it | occurrences |", "|---|---|---|"]
    for kind in ("mcp_errors", "bash_workarounds", "direct_store_access", "unknown_tools", "empty_queries"):
        items = by_kind[kind]
        o.append(f"| {kind} | {len({k for k, _ in items})} | {len(items)} |")
    o.append(f"| debrief asks for something | {len(debriefs)} | - |")

    titles = {
        "mcp_errors": "MCP call errors",
        "bash_workarounds": "Bash workarounds (engramdb CLI or .engramdb/ through the shell)",
        "direct_store_access": "Direct reads of .engramdb/",
        "unknown_tools": "Tool names that do not exist",
        "empty_queries": "Queries that returned nothing",
    }
    for kind, title in titles.items():
        items = by_kind[kind]
        if not items:
            continue
        o += ["", f"## {title}", ""]
        for key, item in items:
            o += [f"**{key}**", "", fence(json.dumps(item, indent=1)), ""]
    if debriefs:
        o += ["", "## Debrief answers (self-reported; verify before acting)", ""]
        for key, d in debriefs:
            o += [f"**{key}**", "", fence(d), ""]
    (out_dir / "gaps.md").write_text("\n".join(o) + "\n")
    print(f"wrote {out_dir / 'gaps.md'}")


if __name__ == "__main__":
    main()
