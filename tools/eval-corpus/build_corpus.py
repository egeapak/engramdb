#!/usr/bin/env python3
"""Build a retrieval-eval corpus from this project's own history and docs.

Memories are derived *mechanically* — nothing about the documents being scored
is hand-tuned — so the only authored artefact is the query set that ships
alongside. Deterministic: same repo state in, same corpus out.

    python3 tools/eval-corpus/build_corpus.py [--out examples/data/engramdb_eval_memories.json]
"""
import argparse, collections, json, pathlib, re, subprocess, sys

REPO = pathlib.Path(__file__).resolve().parents[2]

TYPE_BY_PREFIX = {
    "feat": "decision", "fix": "hazard", "perf": "insight", "refactor": "decision",
    "docs": "insight", "test": "convention", "chore": "convention", "ci": "convention",
    "build": "convention", "style": "convention",
}
DOCS = [
    "docs/contributors/architecture.md", "docs/contributors/testing.md",
    "docs/contributors/code-organization.md", "docs/contributors/extending.md",
    "docs/contributors/parallelization-simd.md",
    "docs/contributors/embedding-model-alternatives.md",
    "docs/contributors/query-latency-profile.md",
    "docs/contributors/turbovec-evaluation.md", ".claude/CLAUDE.md",
]
MIN_COMMIT_BODY = 250
MIN_DOC_SECTION = 400
MAX_CONTENT = 1800


def sh(*a):
    return subprocess.run(a, cwd=REPO, capture_output=True, text=True).stdout


def clean(t):
    t = re.sub(r"Co-Authored-By:.*", "", t, flags=re.I)
    t = re.sub(r"Claude-Session:.*", "", t, flags=re.I)
    t = re.sub(r"🤖 Generated with.*", "", t)
    t = re.sub(r"https://claude\.ai/\S*", "", t)
    return re.sub(r"\n{3,}", "\n\n", t).strip()


def summarize(text, limit=200):
    """Mirror the store's 200-char summary cap."""
    flat = " ".join(text.split())
    if len(flat) <= limit:
        return flat
    cut = flat[:limit]
    dot = cut.rfind(". ")
    return (cut[:dot + 1] if dot > 60 else cut).strip()


def from_commits(mems):
    for h in sh("git", "log", "--no-merges", "--format=%H").split():
        subj = sh("git", "log", "-1", "--format=%s", h).strip()
        body = clean(sh("git", "log", "-1", "--format=%b", h))
        if len(body) < MIN_COMMIT_BODY:
            continue
        m = re.match(r"^(\w+)(?:\(([^)]+)\))?!?:\s*(.+)$", subj)
        if not m:
            continue
        prefix, scope, rest = m.group(1), m.group(2), m.group(3)
        rest = re.sub(r"\s*\(#\d+\)$", "", rest).strip()
        mems.append({
            "id": f"c{len(mems):03d}",
            "type": TYPE_BY_PREFIX.get(prefix, "insight"),
            "title": rest[:120],
            "summary": summarize(body),
            "content": body[:MAX_CONTENT],
            "tags": sorted({t for t in re.split(r"[,\s]+", scope or "") if t} | {prefix}),
            "logical": [f"engramdb.{scope.split(',')[0]}" if scope else "engramdb"],
            "source": f"commit {h[:8]}",
        })


def from_docs(mems):
    for rel in DOCS:
        p = REPO / rel
        if not p.exists():
            continue
        for part in re.split(r"\n(?=#{2,3} )", p.read_text()):
            lines = part.splitlines()
            if not lines:
                continue
            title = re.sub(r"^#+\s*", "", lines[0]).strip()
            body = clean("\n".join(lines[1:]))
            body = re.sub(r"```[\s\S]*?```", "", body)          # code blocks
            body = re.sub(r"^\s*\|.*\|\s*$", "", body, flags=re.M)  # tables
            body = re.sub(r"\n{3,}", "\n\n", body).strip()
            if len(body) < MIN_DOC_SECTION or not title or len(title) > 110:
                continue
            stem = pathlib.Path(rel).stem.replace(".", "")
            mems.append({
                "id": f"d{len(mems):03d}",
                "type": "convention" if ("test" in rel or "CLAUDE" in rel) else "insight",
                "title": title[:120],
                "summary": summarize(body),
                "content": body[:MAX_CONTENT],
                "tags": sorted({stem.split("-")[0], "docs"}),
                "logical": [f"engramdb.docs.{stem}"],
                "source": rel,
            })


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="tools/eval-corpus/memories.json")
    args = ap.parse_args()

    mems = []
    from_commits(mems)
    from_docs(mems)

    out = REPO / args.out
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps({"memories": mems}, indent=1) + "\n")

    print(f"memories: {len(mems)} -> {args.out}")
    print("by type:", dict(collections.Counter(m["type"] for m in mems)))
    print("from commits:", sum(1 for m in mems if m["source"].startswith("commit")))
    print("from docs:", sum(1 for m in mems if not m["source"].startswith("commit")))
    lens = sorted(len(m["content"]) for m in mems)
    print("content len p50:", lens[len(lens) // 2])


if __name__ == "__main__":
    sys.exit(main())
