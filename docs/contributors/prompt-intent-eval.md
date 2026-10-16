# Prompt-intent prototype: evaluation

Status: **prototype, not wired into the hook.** The UserPromptSubmit hook still
uses the keyword tables and still searches on every prompt.

## What is being decided

The UserPromptSubmit hook makes two decisions about the prompt text before it
queries memories:

1. **Situation.** Is the user debugging a failure, weighing a design choice, or
   neither? The answer sets the `[retrieval.scoring.situation]` multipliers and
   the order in which memory classes are rendered.
2. **Context gate.** Does the prompt carry task content? Turn control such as
   "ok", "continue" or "thanks" has nothing to retrieve against. Today the hook
   searches for it anyway.

| Prompt | Gate | Situation |
|---|---|---|
| "the tests hang and never finish on my machine" | search | debugging |
| "should we keep the ledger as JSONL or move it to sqlite" | search | design choice |
| "add a --json flag to harvest list" | search | none |
| "ok, now add a test for the empty prompt case" | search | none |
| "thanks" | skip | — |
| "yes please go ahead with the plan" | skip | — |

A skipped prompt gets no situation, because nothing is retrieved for it.

## Methods compared

| Decision | Method | Where |
|---|---|---|
| Situation | Keyword tables, first match wins (shipped) | `prompt_intent::keyword_situation` |
| Situation | Nearest-seed prototype over MiniLM embeddings | `prompt_intent::PrototypeClassifier` |
| Gate | Always search (shipped) | — |
| Gate | Nearest-seed prototype, with a length guard | `prompt_intent::PrototypeClassifier` |

The prototype embeds 48 labelled seed prompts once (12 per class: debugging,
design, other, no-context). For a prompt, each class score is the mean of the
top-3 cosine similarities to that class's seeds. The gate skips only when the
no-context score beats the best task score by more than `gate_margin` (default
0.05), and only when the prompt has at most `max_skip_words` words (default 7).
`top_k = 3` and `gate_margin = 0.05` were set before the first evaluation run.
The length guard was added after it, and its default of 7 is the longest
turn-control prompt in this set ("yes please go ahead with the plan").

## Data

`examples/data/prompt_intent_eval.json`: 150 hand-labelled prompts.

- Situation: 37 debugging, 33 design choice, 80 none.
- Gate: 120 need context, 30 are turn control.
- Tags: `keyword` (a table word is present and correct, 18), `paraphrase`
  (intent with no table word, 49), `trap` (a table word is present but wrong,
  17), `short` (four words or fewer, 38), `mixed` (turn control plus a task, 10).

**Caveat.** The same author wrote the seeds and the labels, and
no prompt comes from a real session. Read the absolute numbers as optimistic.
The comparison between methods on the tagged subsets is the more reliable
signal. A real evaluation needs prompts sampled from harvested transcripts.

## Results

Model `onnx/all-MiniLM-L12-v2-u8` (the default), CPU, 4 cores, release build.
Two runs gave identical decisions.

### 1. Situation

| Method | Accuracy | Macro-F1 | keyword | paraphrase | trap | short | mixed |
|---|---|---|---|---|---|---|---|
| Keyword tables (shipped) | 54.0% (81/150) | 0.443 | 18/18 | **0/49** | **0/17** | 33/38 | 7/10 |
| Prototype, top_k = 3 | **86.0%** (129/150) | **0.856** | 17/18 | 44/49 | 10/17 | 37/38 | 8/10 |
| Prototype, top_k = 1 | 84.7% | 0.836 | | | | | |
| Prototype, top_k = 5 | 86.7% | 0.864 | | | | | |

Confusion (rows = gold label, columns = predicted debugging / design / none):

| | Keyword | Prototype (top_k = 3) |
|---|---|---|
| debugging | 10 / 0 / 27 | 36 / 0 / 1 |
| design_choice | 0 / 8 / 25 | 1 / 28 / 4 |
| none | 10 / 7 / 63 | 7 / 8 / 65 |

- The keyword tables recognize only prompts that contain a table word. They
  miss every paraphrase ("the tests hang and never finish", "pros and cons?"),
  and they are wrong on every trap ("add an error variant", "redesign the
  landing page").
- The prototype's remaining errors are mostly "none" prompts that sit near a
  situation: task prompts that mention errors or design ("translate the error
  messages", "implement the design we agreed on"). Seven of the 17 traps still
  fail.
- A wrong situation is a mild error: it changes score multipliers, not which
  memories can be found.

### 2. Context gate

A **wrong skip** hides memories the user needed. A **missed skip** only costs
one unneeded search, which is what happens today.

| Method | Accuracy | Skips caught (of 30) | Wrong skips (of 120) |
|---|---|---|---|
| Always search (shipped) | 80.0% | 0 | 0 |
| Prototype, margin 0.00, no guard | 94.7% | 30 | 8 |
| Prototype, margin 0.05, no guard | 97.3% | 29 | 3 |
| Prototype, margin 0.10, no guard | 97.3% | 27 | 1 |
| Prototype, margin 0.05, guard 3 words | 95.3% | 25 | 2 |
| Prototype, margin 0.05, guard 5 words | 97.3% | 28 | 2 |
| Prototype, margin 0.05, guard 7 words (default) | **98.0%** | 29 | 2 |
| Prototype, margin 0.10, guard 7 words | 97.3% | 27 | 1 |

The length guard removes the one long wrong skip, "looks good, push it and open
a PR" (8 words), and costs no caught skip at 7 words. A tighter guard costs
caught skips: at 3 words it also keeps "thank you so much" and "that's it for
today, thanks".

The two wrong skips left at the default are short commands: "push it" and "now
the docs". No length guard can separate them from "ok go" or "sure thing".
They need short task seeds in the `Other` class, or a larger margin (0.10 has
one wrong skip but catches two fewer skips).

### Latency

| Step | Time |
|---|---|
| Model load (in-process, files in the OS page cache) | 199–213 ms |
| Seed embedding (once per process) | 94–103 ms |
| Prompt embedding, warm | p50 2.9–3.3 ms, p95 6.1–8.2 ms |
| Classification from the vector | 12–19 µs |

The first run after a container restart loaded the model in about 4 s, because
the model file was not yet in the OS page cache.

A hook process that loads the model itself pays about 300 ms per prompt, which
is why the hook loads no model today. Through the shared daemon, the per-prompt
cost is the warm embedding plus a socket round trip. The seed vectors must then
be cached by the daemon (keyed by `model_id`), or every hook pays the 100 ms
seed embedding again.

## Conclusions

1. **Situation: the prototype is a clear improvement** (54% to 86%), with the
   largest gain on prompts that state an intent without a table word. It needs
   an embedding, so it is only usable in the hook through the daemon, with
   `keyword_situation` as the fallback.
2. **Gate: the prototype with the length guard is usable, but not yet safe
   enough to ship.** It catches 29 of 30 turn-control prompts and wrongly skips
   2 of 120 task prompts, both 2–3 word commands. Fix those with short task
   seeds before wiring it in, and re-measure on real prompts, because the seeds
   would then be informed by this evaluation's errors.
3. **Before wiring either one in,** build an evaluation set from real prompts
   (harvested transcripts) and re-measure. The current set is small and
   written by the same author as the seeds.

## Reproduce

```bash
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so   # see CLAUDE.md
cargo run --release --example prompt_intent_eval
```

The example prints the per-prompt errors for both prototype decisions after
the tables above.
