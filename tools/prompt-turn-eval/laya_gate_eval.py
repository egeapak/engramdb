"""Score a Laya checkpoint (base or fine-tuned) on prompt_turn_*.json test files.

Usage: laya_gate_eval.py <checkpoint or hub id> <test.json> [<test.json> ...]
Prints accuracy, skips caught and wrong skips at several P(work) cut-offs.
"""
import json, sys, time
import torch
from laya import Agent

QUESTION = {"work": {"type": "noul", "instructions": "Does the user ask the assistant to do work, including approving or choosing work the assistant proposed?"}}
THRESHOLDS = [0.1, 0.2, 0.3, 0.4, 0.5]
torch.set_num_threads(4)

def tail(text, n=300):
    if len(text) <= n:
        return text
    cut = text[-n:]
    i = cut.find(" ")
    return cut[i + 1:] if i >= 0 else cut

agent = Agent(sys.argv[1])
for path in sys.argv[2:]:
    items = json.load(open(path))["items"]
    t = time.time()
    ps = []
    for s in range(0, len(items), 16):
        chunk = items[s:s + 16]
        states = [f"Assistant: {tail(i['last'])}\nUser: {i['prompt']}" for i in chunk]
        for r in agent.predict_batch(states, QUESTION):
            ps.append(r["answers"]["work"]["noul"])
    ms = (time.time() - t) * 1e3 / len(items)
    n = len(items); ns = sum(not i["search"] for i in items)
    print(f"\n{path.split('/')[-1]}: {n} items ({n - ns} search / {ns} skip), {ms:.0f} ms per item batched")
    for th in THRESHOLDS:
        c = ws = sc = 0
        for p, i in zip(ps, items):
            pred = p >= th
            c += pred == i["search"]; ws += i["search"] and not pred; sc += (not i["search"]) and not pred
        print(f"  P(work) >= {th}: acc {100 * c / n:5.1f}%  skips caught {sc:3}/{ns:<3}  wrong skips {ws:3}/{n - ns}")
    json.dump(ps, open(path.split('/')[-1] + "." + sys.argv[1].strip('/').split('/')[-1] + ".probs.json", "w"))
