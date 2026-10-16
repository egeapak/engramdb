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

## Context gate with the previous message

The results above judge a prompt by its own text. That cannot work for short
replies: "yes" after "Should I push this?" asks for work, and "yes" after
"Done, it is pushed" does not. The UserPromptSubmit event carries
`transcript_path`, so the hook can read the agent's previous message.

This section uses a stricter gold label than the 150-prompt set: **approving
or choosing work the agent proposed counts as "search"**, because memories
about that work can matter. Only thanks, praise, acknowledging a finished
result, and "stop"/"wait" are "skip".

Data: `examples/data/prompt_turn_eval.json`, 60 hand-written pairs of
(previous agent message, prompt). 40 are "search" and 20 are "skip".

- `paired` (17): the same prompt appears with another previous message and the
  opposite label. The prompt text alone cannot decide these.
- `refers_back` (30): the prompt does not say what the work is ("push it",
  "now the docs", "revert that").
- `self_contained` (10): the prompt names its own task.
- `no_action` (20): the gold label is "skip".

Prompts of 7 words or fewer go to the model under test. Longer prompts keep
the prototype's decision. Every hypothesis and question below was fixed
before its first run, except the two "previous message alone" hypotheses,
which were added after the dialogue premises failed.

| Method | Accuracy | Skips caught (of 20) | Wrong skips (of 40) |
|---|---|---|---|
| Always search (shipped) | 66.7% | 0 | 0 |
| Prototype gate, prompt only | 65.0% | 20 | 21 |
| NLI, prompt alone ("asks the assistant to do something", ≥ 0.3) | 50.0% | 20 | 30 |
| NLI, prompt alone ("is asking for an action", ≥ 0.7) | 71.7% | 8 | 5 |
| NLI, with previous message (best of 3 hypotheses × 3 cut-offs) | 68.3% | 1 | 0 |
| Prototype + NLI "previous message asks a question" (≥ 0.3) | 70.0% | 20 | 18 |
| Laya, prompt alone ("does the reply request an action?", ≥ 0.3) | 66.7% | 17 | 17 |
| Laya, with previous message (same question, ≥ 0.3) | 65.0% | 17 | 18 |

Full sweeps: `cargo run --release --example prompt_turn_eval` (NLI and
prototype), `tools/prompt-turn-eval/laya_turn_eval.py` (Laya).

What the per-item scores show:

- **The prompt-only prototype wrongly skips half of the work requests** (21 of
  40) under this label. "yes", "go ahead" and "push it" look like turn control
  without the previous message. This confirms that the gate needs context.
- **NLI (`Xenova/nli-deberta-v3-xsmall`) does not separate the classes.** On
  the prompt alone it scores almost every prompt near 0. With the previous
  message it scores almost every pair near 0.9, because the agent's message
  itself describes work. On the previous message alone, it scores "Do you want
  me to push it and open a pull request?" at 0.26 for "asks the user a
  question".
- **Laya zero-shot (`convaiinnovations/laya`, English, 421M parameters) does
  not separate them either.** The previous message helps it on the paired
  items (15 of 17 correct, against 10 of 17 without it). It still misses most
  `refers_back` commands: "push it" 0.12, "now the docs" 0.08, "revert that"
  0.07. Laya's own README reports that zero-shot accuracy is low (0.362 base
  against 0.766 fine-tuned on its typed-decisions benchmark).
- Latency per prompt on 4 CPU cores: NLI 13.5 ms p50 / 21.9 ms p95 per pair;
  Laya 361 ms p50 / 431 ms p95 per call with the previous message, plus 13.8 s
  to load.

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
3. **Gate, revised by the previous-message evaluation: do not skip yet.** Once
   approvals count as work, no model tried here (the prototype, NLI, Laya
   zero-shot) beats "always search" without wrong skips. The prompt alone
   cannot decide, and no available model reads the context well enough.
4. **Use the previous message as query text, not as a gate input.** For a
   short prompt, search with the prompt plus the agent's previous message:
   "push it" after "I committed the fix" then finds memories about pushing.
   The cost of a needless search ("thanks" after "I pushed the branch") is
   limited, because the hook already drops memories it injected earlier in
   the same session. This needs a retrieval evaluation (fixture memories plus
   expected hits), not a classification one.
5. **A trained gate is the path to skipping.** Train a small classifier on
   (previous message, prompt) pairs from harvested transcript copies, labelled
   once offline. That is the same move as Laya's fine-tuning, at the size of
   a classification head on the MiniLM embeddings EngramDB already computes.
6. **Before wiring anything in,** build the evaluation sets from real prompts
   (harvested transcripts) and re-measure. Both current sets are small and
   written by the same author as the seeds.

## Reproduce

```bash
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so   # see CLAUDE.md
cargo run --release --example prompt_intent_eval
cargo run --release --example prompt_turn_eval

# Laya, in a Python 3.10+ virtual environment:
pip install torch --index-url https://download.pytorch.org/whl/cpu
pip install laya==0.4.8
python tools/prompt-turn-eval/laya_turn_eval.py examples/data/prompt_turn_eval.json
```

The example prints the per-prompt errors for both prototype decisions after
the tables above.
