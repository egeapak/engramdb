#!/usr/bin/env python3
"""Recompute the transcript-derived grades of finished variants in place.

Grades that need the store's before/after snapshot (explicit_create,
implicit_capture, no_false_create) cannot be recomputed after the run, so
they are kept. What is recomputed from raw/<case>_rep<k>.jsonl:
query_before_act, consulted_before_act, no_spurious_revise, the costly
grades, no_false_create (exactly for rows that recorded their new memory
files; for older rows only a false create the transcript shows to be a
replacement is lifted), pass, and the gap signals; and from
raw/<case>_rep<k>.diff: fact_used and no_stale_fact.

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
            "hard": ("seed_memories_hard.json", "cases_hard.jsonl"),
            "xhard": ("seed_memories_xhard.json", "cases_xhard.jsonl")}


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
        g = dict(row["grade"])
        exp = case["expect"]
        if exp["query"] is True:
            g["query_before_act"], g["consulted_before_act"] = grade.consulted(case, steps, hooks)
        seeded = row["meta"].get("seeded_ids") or grade.seeded_ids_from_events(events)
        if exp.get("revise") is False:
            revs = [s for s in tools if grade.memory_op(s["name"])
                    and grade._is_revision(grade.memory_op(s["name"]), s["input"])]
            g["no_spurious_revise"] = int(all(grade.not_spurious(s["input"], seeded, exp) for s in revs))
        if g.get("no_false_create") == 0 and "new_memory_files" not in row:
            # Rows from before new files were recorded: the store snapshot is
            # gone, so only lift a false create the transcript explains, where
            # every memory the run wrote was a replacement of a seeded one.
            writes = [s for s in tools if grade.memory_op(s["name"]) == "create"
                      or (s["name"] in grade.EDIT_TOOLS and ".engramdb/memories" in json.dumps(s["input"]))]
            replaced = grade.replacement_ids(tools, seeded)
            if writes and all(grade.memory_op(s["name"]) == "create" and grade._created_id(s) in replaced
                              for s in writes):
                g["no_false_create"] = 1
        elif "new_memory_files" in row and "no_false_create" in g:
            g["no_false_create"] = int(not grade.false_creates(row["new_memory_files"], tools, seeded))
        # Rows from the redesigned costly cases carry discovery_cost.
        if "costly" in case["tags"] and "discovery_cost" in row:
            row["discovery_cost"] = grade.discovery_cost(tools, exp["attempt_re"], exp["fail_re"])
            if not grade.costly_and_found(tools, exp):
                g.pop("costly_capture", None)
            elif "costly_capture" not in g:
                # Newly graded, and the store snapshot is gone: read the save
                # from the transcript's memory writes.
                g["costly_capture"] = int(any(
                    grade.memory_op(s["name"]) in ("create", "update")
                    and grade._contains_any(json.dumps(s["input"]), exp["create_terms"])
                    for s in tools))
        diff_file = raw.with_suffix(".diff")
        if (exp["facts"] or exp.get("facts_all")) and diff_file.exists():
            answer = "\n".join(e.get("result") or "" for e in events if e.get("type") == "result")
            g["fact_used"] = grade.fact_used(case, answer, diff_file.read_text())
        if case["expect"].get("forbidden") and diff_file.exists():
            g["no_stale_fact"] = int(not grade._contains_any(grade.added_code(diff_file.read_text()),
                                                             case["expect"]["forbidden"]))
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
