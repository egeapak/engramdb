"""Pick a skip threshold on validation probabilities, then score test sets at that threshold.

Usage: gate_threshold_report.py <probs_dir> <checkpoint_name> <val.json> <test.json> [<test.json> ...]
Reads <probs_dir>/<file>.<checkpoint_name>.probs.json as written by laya_gate_eval.py.
Rule: the most accurate threshold whose validation wrong-skip rate is at most 5%.
"""
import json, os, sys

probs_dir, ckpt, val_path, tests = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
# 0.01-0.03 were added after the first two retrained seeds hit the 0.05 floor
# without meeting the 5% rule on validation.
GRID = [0.01, 0.02, 0.03] + [round(0.05 * k, 2) for k in range(1, 20)]

def load(path):
    items = json.load(open(path))["items"]
    ps = json.load(open(os.path.join(probs_dir, f"{os.path.basename(path)}.{ckpt}.probs.json")))
    return items, ps

def counts(items, ps, t):
    c = ws = sc = 0
    for p, i in zip(ps, items):
        pred = p >= t
        c += pred == i["search"]; ws += i["search"] and not pred; sc += (not i["search"]) and not pred
    n = len(items); ns = sum(not i["search"] for i in items)
    return c, ws, sc, n, ns

def line(r):
    c, ws, sc, n, ns = r
    return f"acc {100 * c / n:5.1f}%  wrong skips {ws:3}/{n - ns:<3} ({100 * ws / (n - ns):4.1f}%)  skips caught {sc:3}/{ns}"

vi, vp = load(val_path)
best = None
for t in GRID:
    r = counts(vi, vp, t)
    if r[1] / (r[3] - r[4]) <= 0.05 and (best is None or r[0] > best[1][0]):
        best = (t, r)
if best is None:
    best = (GRID[0], counts(vi, vp, GRID[0]))
    print("no threshold meets 5% wrong skips on validation; using the lowest")
t = best[0]
print(f"{ckpt}: threshold {t} chosen on validation: {line(best[1])}")
for path in tests:
    items, ps = load(path)
    print(f"  {os.path.basename(path):<34} {line(counts(items, ps, t))}")
