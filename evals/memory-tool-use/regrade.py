#!/usr/bin/env python3
"""Recompute the transcript-derived grades of finished variants in place.

Grades that need the store's before/after snapshot (explicit_create,
implicit_capture, no_false_create) cannot be recomputed after the run, so
they are kept. What is recomputed from raw/<case>_rep<k>.jsonl:
query_before_act, consulted_before_act, pass, and the gap signals.

Usage: python3 regrade.py baseline v1 v2 ...   (no args: every variant dir)
"""

import json
import os
import sys
from pathlib import Path

import grade

HERE = Path(__file__).resolve().parent
FLOW_DIR = (Path(__file__).resolve().parent.parent.parent / ".claude" / "hillclimb"
        / os.environ.get("MTE_FLOW", "memory-tool-use"))
FIXTURES = {"default": ("seed_memories.json", "cases.jsonl"),
            "vague": ("seed_memories_vague.json", "cases_vague.jsonl"),
            "hard": ("seed_memories_hard.json", "cases_hard.jsonl")}


def regrade(variant):
    out_dir = FLOW_DIR / variant
    build = out_dir / "build.json"
    fixture = json.loads(build.read_text()).get("fixture", "default") if build.exists() else "default"
    seeds, cases_file = FIXTURES[fixture]
    grade.set_seed_file(HERE / seeds)
    cases = {c["id"]: c for c in map(json.loads, (HERE / cases_file).read_text().splitlines())}

    rows = [json.loads(l) for l in (out_dir / "results.jsonl").read_text().splitlines() if l.strip()]
    changed = 0
    for row in rows:
        case = cases[row["prompt_id"]]
        raw = out_dir / "raw" / f"{row['prompt_id']}_rep{row['rep']}.jsonl"
        events = [e for e in map(json.loads, raw.read_text().splitlines()) if e.get("type") != "debrief"]
        steps, hooks, init, _ = grade.parse_events(events)
        tools = [s for s in steps if s["kind"] == "tool"]
        row["meta"]["gaps"] = grade.gap_signals(tools, init, hooks)
        first_edit = next((i for i, s in enumerate(tools) if s["name"] in grade.EDIT_TOOLS), None)
        g = dict(row["grade"])
        if case["expect"]["query"] is True:
            g["query_before_act"] = int(any(
                grade.memory_op(s["name"]) in grade.CONSULT and (first_edit is None or i < first_edit)
                for i, s in enumerate(tools)))
            g["consulted_before_act"] = int(g["query_before_act"] or grade.hook_delivered_body(case, hooks))
        if case["expect"].get("revise") is False:
            seeded = row["meta"].get("seeded_ids") or grade.seeded_ids_from_events(events)
            revs = [s for s in tools if grade.memory_op(s["name"])
                    and grade._is_revision(grade.memory_op(s["name"]), s["input"])]
            g["no_spurious_revise"] = int(all(grade._revises_stale_only(s["input"], seeded) for s in revs))
        graded = [v for k, v in g.items() if k not in grade.NOT_IN_PASS and k != "pass"]
        g["pass"] = int(all(graded)) if graded else 1
        changed += g != row["grade"]
        row["grade"] = g
    (out_dir / "results.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
    print(f"{variant}: {len(rows)} rows, {changed} regraded")


def main():
    variants = sys.argv[1:] or sorted(p.name for p in FLOW_DIR.iterdir() if (p / "results.jsonl").exists())
    grade.write_state(FLOW_DIR)
    for v in variants:
        regrade(v)


if __name__ == "__main__":
    main()
