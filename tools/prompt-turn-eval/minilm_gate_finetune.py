"""Fine-tune all-MiniLM-L12-v2 as a (previous message, prompt) pair classifier for the context gate.

Same protocol as examples/prompt_gate_classifier.rs: the skip threshold is chosen by 5-fold
cross-validation on the training set (folds by index mod 5; at most 5% wrong skips, then the
highest accuracy), the model is retrained on all training pairs, and the held-out set is
scored once. Hyperparameters are fixed, not tuned.
"""
import json, random, sys, time
import numpy as np
import torch
from transformers import AutoTokenizer, AutoModelForSequenceClassification

TRAIN, EVAL = sys.argv[1], sys.argv[2]
SEEDS = [int(s) for s in sys.argv[3].split(",")] if len(sys.argv) > 3 else [0]
EXTRA = sys.argv[4:]
MODEL = "sentence-transformers/all-MiniLM-L12-v2"
EPOCHS, LR, BATCH, MAX_LEN, TAIL = 4, 5e-5, 16, 128, 300
THRESHOLDS = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6]
MAX_WRONG_SKIP_RATE = 0.05
torch.set_num_threads(4)

def tail(text, n=TAIL):
    if len(text) <= n:
        return text
    cut = text[-n:]
    i = cut.find(" ")
    return cut[i + 1:] if i >= 0 else cut

def load(path):
    return json.load(open(path))["items"]

tok = AutoTokenizer.from_pretrained(MODEL)

def encode(items):
    return tok([tail(i["last"]) for i in items], [i["prompt"] for i in items],
               truncation="only_first", max_length=MAX_LEN, padding=True, return_tensors="pt")

def train(items, seed):
    torch.manual_seed(seed); random.seed(seed); np.random.seed(seed)
    model = AutoModelForSequenceClassification.from_pretrained(MODEL, num_labels=2)
    opt = torch.optim.AdamW(model.parameters(), lr=LR, weight_decay=0.01)
    y = torch.tensor([int(i["search"]) for i in items])
    pos = y.float().mean().item()
    weights = torch.tensor([1 / (2 * (1 - pos)), 1 / (2 * pos)])
    loss_fn = torch.nn.CrossEntropyLoss(weight=weights)
    order = list(range(len(items)))
    model.train()
    for _ in range(EPOCHS):
        random.shuffle(order)
        for s in range(0, len(order), BATCH):
            idx = order[s:s + BATCH]
            enc = encode([items[i] for i in idx])
            loss = loss_fn(model(**enc).logits, y[idx])
            opt.zero_grad(); loss.backward(); opt.step()
    model.eval()
    return model

@torch.no_grad()
def probs(model, items):
    out = []
    for s in range(0, len(items), 32):
        logits = model(**encode(items[s:s + 32])).logits
        out += torch.softmax(logits, -1)[:, 1].tolist()
    return out

def counts(ps, items, t):
    c = dict(correct=0, wrong_skips=0, skips_caught=0)
    for p, i in zip(ps, items):
        pred = p >= t
        c["correct"] += pred == i["search"]
        c["wrong_skips"] += i["search"] and not pred
        c["skips_caught"] += (not i["search"]) and (not pred)
    return c

def line(c, items):
    n = len(items); ns = sum(not i["search"] for i in items)
    return (f"acc {100 * c['correct'] / n:5.1f}%  skips caught {c['skips_caught']:3}/{ns:<3}  "
            f"wrong skips {c['wrong_skips']:2}/{n - ns}")

train_items, eval_items = load(TRAIN), load(EVAL)
print(f"{MODEL}: {len(train_items)} training pairs, {len(eval_items)} held-out pairs")
for seed in SEEDS:
    t0 = time.time()
    oof = [0.0] * len(train_items)
    for fold in range(5):
        tr = [x for k, x in enumerate(train_items) if k % 5 != fold]
        m = train(tr, seed)
        te_idx = [k for k in range(len(train_items)) if k % 5 == fold]
        for k, p in zip(te_idx, probs(m, [train_items[k] for k in te_idx])):
            oof[k] = p
    best = None
    for t in THRESHOLDS:
        c = counts(oof, train_items, t)
        if c["wrong_skips"] / 240 <= MAX_WRONG_SKIP_RATE and (best is None or c["correct"] > best[1]["correct"]):
            best = (t, c)
    print(f"\nseed {seed}")
    for t in THRESHOLDS:
        print(f"  cv t {t}: {line(counts(oof, train_items, t), train_items)}")
    if best is None:
        print("  no threshold keeps cross-validated wrong skips at or below 5%; reporting at 0.1")
        best = (0.1, counts(oof, train_items, 0.1))
    t, c = best
    model = train(train_items, seed)
    ps = probs(model, eval_items)
    print(f"  chosen threshold {t}: cv {line(c, train_items)}")
    print(f"  held-out:              {line(counts(ps, eval_items, t), eval_items)}")
    for path in EXTRA:
        ext = load(path)
        eps = probs(model, ext)
        print(f"  {path.split('/')[-1]:<32} {line(counts(eps, ext, t), ext)}")
        for x in THRESHOLDS:
            print(f"      sweep {x}: {line(counts(eps, ext, x), ext)}")
    tags = {}
    for p, i in zip(ps, eval_items):
        for tg in i.get("tags", []):
            a, n = tags.get(tg, (0, 0)); tags[tg] = (a + ((p >= t) == i["search"]), n + 1)
    print("  by tag: " + ", ".join(f"{k} {a}/{n}" for k, (a, n) in sorted(tags.items())))
    print("  held-out sweep: " + "  ".join(f"{x}: {counts(ps, eval_items, x)['correct']}/60 wrong {counts(ps, eval_items, x)['wrong_skips']}" for x in THRESHOLDS))
    for p, i in zip(ps, eval_items):
        if (p >= t) != i["search"]:
            print(f"    gold {'search' if i['search'] else 'skip  '} P {p:.2f}  {i['prompt'][:30]:<30} | {i['last'][-60:]}")
    t1 = time.time()
    enc = encode(eval_items[:1])
    with torch.no_grad():
        for _ in range(20): model(**enc)
    print(f"  time: {t1 - t0:.0f} s for CV + final training; one pair inference {(time.time() - t1) / 20 * 1e3:.1f} ms (PyTorch, fp32)")
