"""Merge prompt_turn_*.json training files and split off a stratified validation set.

Usage: split_gate_data.py <out_dir> <val_fraction> <train.json> [<train.json> ...]
Writes <out_dir>/train.json and <out_dir>/val.json (same format as the inputs). The split
is fixed (random seed 0) so every training seed sees the same data.
"""
import json, os, random, sys

out, frac, paths = sys.argv[1], float(sys.argv[2]), sys.argv[3:]
items = [dict(i, source=os.path.basename(p)) for p in paths for i in json.load(open(p))["items"]]
rng = random.Random(0)
train, val = [], []
for label in (True, False):
    group = [i for i in items if i["search"] is label]
    rng.shuffle(group)
    k = round(len(group) * frac)
    val += group[:k]; train += group[k:]
rng.shuffle(train); rng.shuffle(val)
os.makedirs(out, exist_ok=True)
for name, part in (("train", train), ("val", val)):
    json.dump({"description": f"{name} split of {', '.join(os.path.basename(p) for p in paths)}", "items": part},
              open(os.path.join(out, name + ".json"), "w"), indent=0)
print(f"train {len(train)} ({sum(i['search'] for i in train)} search), val {len(val)} ({sum(i['search'] for i in val)} search)")
