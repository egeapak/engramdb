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

## Methods compared

| Decision | Method | Where |
|---|---|---|
| Situation | Keyword tables, first match wins (shipped) | `prompt_intent::keyword_situation` |
| Situation | Nearest-seed prototype over MiniLM embeddings | `prompt_intent::PrototypeClassifier` |
| Gate | Always search (shipped) | — |
| Gate | Word-list rule: skip when every word is a turn-control word | `examples/prompt_intent_eval.rs` only |
| Gate | Nearest-seed prototype | `prompt_intent::PrototypeClassifier` |

The prototype embeds 48 labelled seed prompts once (12 per class: debugging,
design, other, no-context). For a prompt, each class score is the mean of the
top-3 cosine similarities to that class's seeds. The gate skips only when the
no-context score beats the best task score by more than `gate_margin` (default
0.05). The defaults (`top_k = 3`, `gate_margin = 0.05`) were set before the
first evaluation run.

## Data

`examples/data/prompt_intent_eval.json`: 150 hand-labelled prompts.

- Situation: 37 debugging, 33 design choice, 80 none.
- Gate: 120 need context, 30 are turn control.
- Tags: `keyword` (a table word is present and correct, 18), `paraphrase`
  (intent with no table word, 49), `trap` (a table word is present but wrong,
  17), `short` (four words or fewer, 38), `mixed` (turn control plus a task, 10).

**Caveat.** The same author wrote the seeds, the word list and the labels, and
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

| Method | Accuracy | Skips caught | Wrong skips |
|---|---|---|---|
| Always search (shipped) | 80.0% | 0/30 | 0/120 |
| Word-list rule | 96.0% | 24/30 | **0/120** |
| Prototype, margin −0.05 | 91.3% | 30/30 | 13/120 |
| Prototype, margin 0.00 | 94.7% | 30/30 | 8/120 |
| Prototype, margin +0.05 (default) | **97.3%** | 29/30 | 3/120 |
| Prototype, margin +0.10 | 97.3% | 27/30 | 1/120 |
| Prototype, margin +0.15 | 94.0% | 22/30 | 1/120 |

The prototype's wrong skips at the default margin are short commands: "push
it", "now the docs", and "looks good, push it and open a PR". MiniLM places
them near the acknowledgement seeds.

The word-list rule makes no wrong skips and needs no model. The word list was
written after the evaluation set, so its 0/120 is the most optimistic number in
this report.

### Latency

| Step | Time |
|---|---|
| Model load (cold, in-process) | 199–213 ms |
| Seed embedding (once per process) | 94–103 ms |
| Prompt embedding, warm | p50 3.2–3.3 ms, p95 7.5–8.2 ms |
| Classification from the vector | ~12 µs |

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
2. **Gate: start with the word-list rule, not the prototype.** It catches most
   turn control with no wrong skips in this set and costs nothing. The
   prototype catches a few more skips but makes wrong skips on short commands.
   That is the costly error.
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
