#!/usr/bin/env python3
"""Check every xhard case: fact terms live only in target memories and never in the fixture;
forbidden terms are absent from the case's targets; targets exist; files named in prompts exist."""
import json
import re
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIX = HERE / "fixture_xhard"
seeds = {m["key"]: m for m in json.loads((HERE / "seed_memories_xhard.json").read_text())}
cases = [json.loads(l) for l in (HERE / "cases_xhard.jsonl").read_text().splitlines() if l.strip()]
files = {p: p.read_text(errors="replace").lower() for p in FIX.rglob("*") if p.is_file() and "__pycache__" not in p.parts}

# Facts the user states in the prompt itself (no memory holds them).
USER_FACTS = {"lc-corrected-notice"}
problems = 0
for c in cases:
    exp = c["expect"]
    targets = set(c.get("targets") or ([c["target"]] if c.get("target") else []))
    for t in targets | ({c["target"]} if c.get("target") else set()):
        assert t in seeds, (c["id"], t)
    terms = list(exp["facts"]) + [t for g in exp.get("facts_all", []) for t in g]
    rows = []
    for t in terms:
        mem = sorted(k for k, m in seeds.items() if t.lower() in (m["title"] + " " + m["content"]).lower())
        fx = sorted(str(p.relative_to(FIX)) for p, s in files.items() if t.lower() in s)
        bad_mem = [k for k in mem if k not in targets]
        if c["id"] not in USER_FACTS and (bad_mem or not mem):
            rows.append(f"    fact {t!r}: memories {mem} (outside targets: {bad_mem})")
        if fx:
            rows.append(f"    fact {t!r}: IN FIXTURE {fx}")
    # each facts_all group must be satisfiable by some target memory
    for g in exp.get("facts_all", []):
        if c["id"] not in USER_FACTS and not any(any(t.lower() in seeds[k]["content"].lower() for t in g) for k in targets):
            # derived terms (e.g. ledger.payout_) are allowed but reported
            rows.append(f"    group {g}: no target memory holds it verbatim (derived)")
    for t in exp.get("forbidden", []):
        tm = [k for k in targets if t.lower() in seeds[k]["content"].lower()]
        if tm:
            rows.append(f"    forbidden {t!r} is in target {tm}")
    for path in re.findall(r"(?:src|services|scripts|tests)/[\w/]+\.py", c["prompt"] + " ".join(c.get("turns", []))):
        if not (FIX / path).exists() and "new " not in c["prompt"] + " ".join(c.get("turns", [])) and not path.startswith(("scripts/payout", "tests/test_bankco", "src/payouts/reserve")):
            rows.append(f"    file {path} missing")
    if rows:
        problems += 1
        print(c["id"])
        print("\n".join(rows))
print(f"{len(cases)} cases, {problems} with notes")
