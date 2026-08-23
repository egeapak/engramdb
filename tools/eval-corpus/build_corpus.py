#!/usr/bin/env python3
"""Build a retrieval-eval corpus from this project's own history and docs.

Memories are derived *mechanically* — nothing about the documents being scored
is hand-tuned — so the only authored artefact is the query set that ships
alongside. Deterministic: same repo state in, same corpus out.

    python3 tools/eval-corpus/build_corpus.py [--out examples/data/engramdb_eval_memories.json]
"""
import argparse, collections, hashlib, json, math, pathlib, re, subprocess, sys

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


def stable_id(prefix, seed):
    """Content-derived id, so regenerating after new commits land does not
    renumber everything and silently repoint every authored label."""
    return prefix + hashlib.sha256(seed.encode()).hexdigest()[:6]


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
            "id": stable_id("c", h),
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
                "id": stable_id("d", f"{rel}#{title}"),
                "type": "convention" if ("test" in rel or "CLAUDE" in rel) else "insight",
                "title": title[:120],
                "summary": summarize(body),
                "content": body[:MAX_CONTENT],
                "tags": sorted({stem.split("-")[0], "docs"}),
                "logical": [f"engramdb.docs.{stem}"],
                "source": rel,
            })


STOPWORDS = set("""
this that these those with from into over under about after before which while
when where what whom whose there here then than them they their theirs been being
have has had having does did doing done make makes made take takes taken give
gives given only just also even still much many more most less least such same
other another every each both some none only very will would could should must
can may might shall need needs needed use uses used using
whether looks look arrows shows show showing said says say goes going went
thing things way ways lot lots bit bits kind sort part parts side sides
one two three four five first second third last next previous above below
new old good bad big small long short high low full empty real actual
rather instead however therefore because since though although unless
""".split())


def assign_tags(mems, per_memory=4):
    """Give each memory a few *discriminative* tags, TF-IDF style.

    The first version of this generator tagged from the conventional-commit
    prefix and the doc filename, which produced `docs` on 111 of 157 memories
    and `feat`/`fix` on most of the rest. Tags that generic make a tag-only
    query unanswerable — 125 memories share a token with it and only two are
    labelled relevant — so they measure the labeller's arbitrariness, not the
    retriever. A real agent tags a memory with what it is *about*.

    So: score every term by tf-idf against the corpus and keep the top few.
    Rare, topical terms win; corpus-wide filler loses.
    """
    # Candidates come from title+summary only. That is what a human or agent
    # tagging a memory actually draws on, and scoring the whole body instead
    # surfaces incidental words ("arrows", "looks", "whether") that happen to
    # be rare rather than terms the memory is about.
    docs = []
    for m in mems:
        salient = re.findall(r"[a-z][a-z0-9_]{3,}", f"{m['title']} {m['summary']}".lower())
        docs.append(collections.Counter(t for t in salient if t not in STOPWORDS))
    # Document frequency over the whole corpus body, so a term that is common
    # project-wide is discounted even when it is rare in this memory's title.
    df = collections.Counter()
    for m in mems:
        body = set(re.findall(r"[a-z][a-z0-9_]{3,}", f"{m['title']} {m['summary']} {m['content']}".lower()))
        df.update(body)
    n = len(docs)
    for m, d in zip(mems, docs):
        total = sum(d.values()) or 1
        scored = sorted(
            ((cnt / total) * math.log(n / (1 + df[t])), t) for t, cnt in d.items()
        )
        picked, seen = [], set()
        for _, t in reversed(scored):
            # Skip terms that are near-duplicates of one already picked
            # (plural/inflection), so the tag set stays four distinct ideas.
            if any(t.startswith(p[:5]) or p.startswith(t[:5]) for p in picked):
                continue
            if df[t] > n * 0.35:        # still too common to discriminate
                continue
            picked.append(t)
            if len(picked) == per_memory:
                break
        m["tags"] = sorted(picked)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default="tools/eval-corpus/memories.json")
    args = ap.parse_args()

    mems = []
    from_commits(mems)
    from_docs(mems)
    assign_tags(mems)

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
