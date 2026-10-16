"""Run Laya zero-shot (English checkpoint) on examples/data/prompt_turn_eval.json.

Results are recorded in docs/contributors/prompt-intent-eval.md. Needs `pip install laya==0.4.8`
and CPU torch; downloads the ~843 MB checkpoint from Hugging Face on first run.
"""
import json, sys, time
import torch
from laya import Agent

data = json.load(open(sys.argv[1]))["items"]
torch.set_num_threads(4)
t = time.time()
agent = Agent("convaiinnovations/laya")
print(f"load {time.time() - t:.1f} s")

# Questions fixed before the first run.
QUESTIONS = {
    "work": {"type": "noul", "instructions": "Does the user ask the assistant to do more work, or approve or choose work the assistant proposed?"},
    "action": {"type": "noul", "instructions": "Does the user's reply request an action?"},
}

def state(item, with_ctx):
    if with_ctx:
        return f"Assistant: {item['last']}\nUser: {item['prompt']}"
    return f"User: {item['prompt']}"

agent.predict(state(data[0], True), QUESTIONS)  # warm up
results = {}
for with_ctx in (False, True):
    probs, ms = [], []
    for item in data:
        t = time.time()
        r = agent.predict(state(item, with_ctx), QUESTIONS)
        ms.append((time.time() - t) * 1e3)
        probs.append({q: r["answers"][q]["noul"] for q in QUESTIONS})
    results[with_ctx] = probs
    ms.sort()
    print(f"\n{'with previous message' if with_ctx else 'prompt alone'}: p50 {ms[len(ms)//2]:.0f} ms, p95 {ms[int((len(ms)-1)*0.95)]:.0f} ms per call (both questions)")
    for q in QUESTIONS:
        for th in (0.3, 0.5, 0.7):
            correct = ws = sc = 0
            tags = {}
            for item, p in zip(data, probs):
                pred = p[q] >= th
                hit = pred == item["search"]
                correct += hit
                ws += item["search"] and not pred
                sc += (not item["search"]) and (not pred)
                for tg in item["tags"]:
                    c, n = tags.get(tg, (0, 0)); tags[tg] = (c + hit, n + 1)
            ns = sum(not i["search"] for i in data)
            tag_s = ", ".join(f"{k} {c}/{n}" for k, (c, n) in sorted(tags.items()))
            print(f"  {q:<6} p>={th}  acc {100*correct/len(data):5.1f}%  skips {sc:2}/{ns}  wrong skips {ws:2}/{len(data)-ns}  [{tag_s}]")

print("\nPer item: P(work) alone / with previous message")
for item, a, c in zip(data, results[False], results[True]):
    print(f"  gold {'search' if item['search'] else 'skip  '} {a['work']:.2f} / {c['work']:.2f}  {item['prompt'][:34]:<34} | {item['last'][:60]}")
