#!/usr/bin/env python3
"""Render the model x archetype breakdown from embed_matrix's results JSON.

The terminal table embed_matrix prints is aggregate-only, but it already
computes per-archetype metrics into `by_group`. The archetype split is the
part that decides the 384-vs-1024 question: a win concentrated in
`paraphrase`/`buried_fact` is semantic reach the reranker cannot recover
(it only reorders what vector search already surfaced), while a win in
MRR/nDCG with P@1 and R@5 flat is precision the existing cross-encoder
supplies far more cheaply.
"""
import argparse, json, pathlib, sys

ORDER = ["title_echo", "keyword", "tag_only", "natural",
         "paraphrase", "buried_fact", "distractor_trap"]
METRICS = [("p_at_1", "P@1"), ("recall_at_5", "R@5"),
           ("mrr_at_10", "MRR"), ("ndcg_at_10", "nDCG")]


def pick(report, agg):
    """The variant/agg/mode cell to report. `agg=max` matches production:
    LanceIndex::vector_search aggregates chunk scores per memory by max."""
    for variant, aggs in report["results"].items():
        modes = aggs.get(agg)
        if not modes:
            continue
        for mode, res in modes.items():
            return f"{variant}/{agg}/{mode}", res
    return None, None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", default="tools/eval-corpus/results.json")
    ap.add_argument("--baseline", default="minilm-l12-u8")
    ap.add_argument("--agg", default="max", help="chunk aggregation; max matches production")
    a = ap.parse_args()

    data = json.load(open(a.results))
    models = data["models"]
    cells = {}
    for key, m in models.items():
        label, res = pick(m, a.agg)
        if res:
            cells[key] = (m, res)

    if not cells:
        print("no results", file=sys.stderr)
        return 1

    order = [a.baseline] + [k for k in sorted(cells) if k != a.baseline]
    cells = {k: cells[k] for k in order if k in cells}
    print(f"variant: {label}   (agg=max matches LanceIndex::vector_search)\n")
    print("=== OVERALL ===")
    hdr = f"{'model':<18}{'dims':>5}{'ms/text':>9}" + "".join(f"{n:>8}" for _, n in METRICS)
    print(hdr); print("-" * len(hdr))
    for k, (m, res) in cells.items():
        o = res["overall"]
        print(f"{k:<18}{m['dimensions']:>5}{m['ms_per_text']:>9.1f}"
              + "".join(f"{o[f]:>8.3f}" for f, _ in METRICS))

    base = cells.get(a.baseline)
    for field, name in METRICS:
        print(f"\n=== {name} BY ARCHETYPE ===")
        arch = [x for x in ORDER if any(x in r["by_group"] for _, r in cells.values())]
        hdr = f"{'model':<18}" + "".join(f"{x[:9]:>11}" for x in arch)
        print(hdr); print("-" * len(hdr))
        for k, (m, res) in cells.items():
            row = f"{k:<18}"
            for x in arch:
                g = res["by_group"].get(x)
                row += f"{g[field]:>11.3f}" if g else f"{'-':>11}"
            print(row)
        if base:
            print(f"{'Δ vs ' + a.baseline:<18}", end="")
            for x in arch:
                b = base[1]["by_group"].get(x)
                best = max((res["by_group"][x][field]
                            for _, res in cells.values() if x in res["by_group"]),
                           default=None)
                print(f"{best - b[field]:>+11.3f}" if b and best is not None
                      else f"{'-':>11}", end="")
            print("   (best model minus baseline)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
