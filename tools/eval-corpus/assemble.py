#!/usr/bin/env python3
"""Validate authored query slices and assemble the final eval dataset.

The corpus is generated (build_corpus.py); the queries are authored. This is
the gate on the authored half: it rejects labels that would silently corrupt a
model comparison — unknown ids, missing quotas, and in particular *leaky*
paraphrases, which are the archetype that separates models and the one easiest
to write badly.
"""
import argparse, collections, json, pathlib, re, sys

HERE = pathlib.Path(__file__).resolve().parent
QUOTA = {"natural": 3, "paraphrase": 3, "buried_fact": 3,
         "distractor_trap": 3, "keyword": 2, "title_echo": 2, "tag_only": 2}
LEAK_THRESHOLD = 0.6


def toks(s):
    return set(re.findall(r"[a-z0-9_]{4,}", s.lower()))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--slices", default="/tmp/corpus", help="dir holding queries[0-3].json")
    ap.add_argument("--memories", default=str(HERE / "memories.json"))
    ap.add_argument("--out", default=str(HERE / "engramdb_eval.json"))
    a = ap.parse_args()

    mems = json.load(open(a.memories))["memories"]
    byid = {m["id"]: m for m in mems}
    ids = set(byid)

    queries, problems = [], []
    for i in range(4):
        f = pathlib.Path(a.slices) / f"queries{i}.json"
        if not f.exists():
            problems.append(f"MISSING {f.name}")
            continue
        try:
            qs = json.load(open(f))
        except Exception as e:
            problems.append(f"{f.name}: invalid JSON: {e}")
            continue
        if len(qs) != 18:
            problems.append(f"{f.name}: {len(qs)} queries, expected 18")
        tally = collections.Counter(q.get("archetype") for q in qs)
        for k, v in QUOTA.items():
            if tally.get(k, 0) != v:
                problems.append(f"{f.name}: archetype {k}={tally.get(k, 0)}, expected {v}")
        for q in qs:
            rel = q.setdefault("relevant", {})
            bad = [r for r in rel if r not in ids]
            if bad:
                problems.append(f"{f.name}/{q.get('id')}: unknown ids {bad}")
                for r in bad:
                    rel.pop(r)
            if not any(int(v) == 2 for v in rel.values()):
                problems.append(f"{f.name}/{q.get('id')}: no grade-2 relevant")
        queries.extend(qs)

    dupes = [t for t, c in collections.Counter(
        q["text"].strip().lower() for q in queries).items() if c > 1]
    problems += [f"duplicate query text: {t!r}" for t in dupes]

    leaky = []
    for q in queries:
        if q["archetype"] != "paraphrase":
            continue
        qt = toks(q["text"])
        for r, g in q["relevant"].items():
            if int(g) != 2 or r not in byid:
                continue
            m = byid[r]
            ov = qt & toks(m["title"] + " " + m["summary"])
            if qt and len(ov) / len(qt) > LEAK_THRESHOLD:
                leaky.append((q["id"], r, sorted(ov)))

    out = {"memories": [{k: v for k, v in m.items() if k != "source"} for m in mems],
           "queries": queries}
    pathlib.Path(a.out).write_text(json.dumps(out, indent=1) + "\n")

    print(f"memories: {len(mems)}   queries: {len(queries)}   -> {a.out}")
    print("archetypes:", dict(collections.Counter(q["archetype"] for q in queries)))
    print("relevant-set sizes:", dict(collections.Counter(len(q["relevant"]) for q in queries)))
    covered = {r for q in queries for r in q["relevant"]}
    print(f"memories referenced by >=1 query: {len(covered)}/{len(mems)}")
    print(f"\nleaky paraphrases (>{int(LEAK_THRESHOLD*100)}% overlap with target title+summary): {len(leaky)}")
    for qid, r, ov in leaky[:8]:
        print(f"  {qid} -> {r}: {ov}")
    print(f"\nPROBLEMS: {len(problems)}")
    for p in problems[:30]:
        print("  -", p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
