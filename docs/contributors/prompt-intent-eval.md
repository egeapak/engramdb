# Prompt-intent prototype: evaluation

Status: **the situation classifier is wired into the UserPromptSubmit hook;
the context gate is parked.** The hook uses the classifier when a shared daemon
is already running (keyword tables otherwise), searches on every prompt, and
adds a second search on the tail of the agent's previous message. Its seed
vectors are cached per model in the model cache directory.

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

## Trained gate classifiers

The zero-shot results above left training as the remaining option. This
section trains three classifiers on the same gate label and scores them on
three test sets.

### Data

| Set | File | Pairs (search / skip) | Written by |
|---|---|---|---|
| Training | `examples/data/prompt_turn_train.json` | 360 (240 / 120) | A separate model run that saw no other data file |
| Held-out (small) | `examples/data/prompt_turn_eval.json` | 60 (40 / 20) | The author of this document (the set above) |
| Realistic | `examples/data/prompt_turn_test_realistic.json` | 400 (267 / 133) | A separate model run, brief "realistic sessions across many project types" |
| Adversarial | `examples/data/prompt_turn_test_hard.json` | 300 (200 / 100) | A separate model run, brief "hard cases that mislead a surface-level classifier" |

No test pair repeats a training pair. Short replies do repeat as text ("yes",
"ok"): 102 of the 400 realistic prompts and 67 of the 300 adversarial prompts
also appear in the training set, always with different previous messages.
The adversarial set includes 34 non-English pairs, written in Spanish, German,
Turkish and Hindi in Latin script.

All data is model-written. None of it comes from real sessions.

### Classifiers

1. **Linear head on frozen MiniLM embeddings** (`examples/prompt_gate_classifier.rs`,
   Rust). Logistic regression on the prompt embedding, the previous-message
   embedding (last 300 characters), their product, or a joint embedding of
   both texts. The L2 strength and the threshold are chosen by 5-fold
   cross-validation on the training set: the most accurate setting with at
   most 5% wrong skips. The model is 385–1153 weights.
2. **Fine-tuned MiniLM-L12 pair classifier**
   (`tools/prompt-turn-eval/minilm_gate_finetune.py`). The whole 33M-parameter
   encoder is trained for 4 epochs, with the same threshold rule.
3. **Fine-tuned Laya** (`convaiinnovations/laya`, 421M parameters) with
   `laya-train`, either heads only (`--freeze-encoder`, 6 epochs) or the
   whole model (4 epochs), with one `noul` question. Laya was too slow to
   cross-validate on this CPU, so its threshold was not chosen on training
   data. The table shows a 0.2 cut-off, and `laya_gate_eval.py` prints the
   full sweep.

### Results

Accuracy, then wrong skips (work requests skipped) and skips caught. Always
searching scores 66.7% on every set, with no wrong skips and no skips caught.

| Classifier | Held-out 60 | Realistic 400 | Adversarial 300 |
|---|---|---|---|
| Linear, prompt + previous message | 90.0%, 0/40 wrong, 14/20 caught | 75.2%, 15/267, 49/133 | 73.0%, 6/200, 25/100 |
| Linear, joint text + prompt | 90.0%, 0/40, 14/20 | 78.2%, 11/267, 57/133 | 75.3%, 9/200, 35/100 |
| Fine-tuned MiniLM, seed 0 | 81.7%, 11/40, 20/20 | 85.5%, 55/267, 130/133 | 68.0%, 77/200, 81/100 |
| Fine-tuned MiniLM, seed 1 | 86.7%, 7/40, 19/20 | 89.0%, 8/267, 97/133 | 77.3%, 23/200, 55/100 |
| Laya zero-shot, P ≥ 0.1 | 71.7%, 9/40, 12/20 | 63.0%, 70/267, 55/133 | 56.7%, 59/200, 29/100 |
| Laya, heads only, P ≥ 0.3 | 68.3%, 0/40, 1/20 | 69.8%, 1/267, 13/133 | 68.3%, 2/200, 7/100 |
| **Laya, full fine-tune, P ≥ 0.2** | 90.0%, 5/40, 19/20 | **94.2%, 4/267, 114/133** | 81.3%, 28/200, 72/100 |

Costs on 4 CPU cores:

| Classifier | Training | Inference per pair | Size |
|---|---|---|---|
| Linear head | under 1 s | one extra MiniLM embedding (~3 ms) + a dot product | 1.5–4.6 KB |
| Fine-tuned MiniLM | ~5 min (with CV) | 13 ms (PyTorch fp32) | 33M parameters, ~130 MB fp32 |
| Laya, full fine-tune | 58 min, 8.4 GB RAM | ~200 ms batched (PyTorch fp32) | 421M parameters, ~1.7 GB fp32 |

### What this shows

- **The 60-pair set was too small to judge with.** The linear head scored
  90% with no wrong skips there, and 73–78% with 3–5% wrong skips on the 700
  new pairs. Cross-validation on the training set (72.5%) predicted the
  larger sets better than the small held-out set did.
- **A linear head on frozen embeddings is not enough.** It barely beats
  always searching, because the decision depends on how the reply relates to
  the previous message. Separate embeddings cannot express that relation.
- **Fine-tuning the encoder is necessary, and model size matters.**
  - Fine-tuned MiniLM learns the task but is unstable. The two seeds differ by
    47 wrong skips on the realistic set, and neither meets the 5% rule
    reliably.
  - The fully fine-tuned Laya is the only classifier that is both accurate and
    safe on realistic data: 94.2% accuracy, 1.5% wrong skips, and 86% of
    skips caught.
  - Training only Laya's heads does nothing.
- **Adversarial pairs remain hard for every classifier.** Fine-tuned Laya
  wrongly skips 14% of the adversarial work requests. Most of them are
  "no"/"nope" answering "Did that fix it?" (meaning it is still broken) and
  pasted stack traces after "Pushed the fix". Its missed skips are numbered or
  lettered choices that mean "stop" ("2" after "Reply 1 to continue, 2 to wrap
  up"). The training set has almost none of these patterns, so this looks like
  a gap in training coverage, not a limit of the model.
- **Each Laya and MiniLM configuration was trained once (MiniLM twice).** The
  MiniLM seed spread shows that single runs can mislead. Treat the Laya
  numbers as one sample.

### Retraining Laya with a targeted supplement

The failure patterns above came from the adversarial set. To check that the
fixes are not just fitted to that set, a separate model run wrote two new
files without seeing any other data:

- `examples/data/prompt_turn_train_supplement.json`: 360 training pairs (202
  search, 158 skip) covering nine patterns:
  - negative answers that mean "keep working";
  - "yes" that means "stop";
  - pasted errors and stack traces;
  - numbered or lettered choices that include a stop option;
  - declines;
  - symbol-only replies;
  - other languages;
  - long agent messages;
  - thanks combined with a new request.
- `examples/data/prompt_turn_test_hard2.json`: 300 fresh adversarial test
  pairs (200 / 100). The brief asked for failure modes in general and did
  not name the nine patterns.

The original 360 and the supplement were merged and split once
(`split_gate_data.py`, random seed 0): 612 training pairs and 108 validation
pairs. Laya was fully fine-tuned on the 612 with seeds 0, 1 and 2, using the
same settings as before (`retrain_driver.sh`). Each seed took 27–29 minutes
on 4 cores. The earlier 58-minute run shared the CPU with another job.

At fixed cut-offs (accuracy, wrong skips, skips caught):

| Checkpoint | Realistic 400, P ≥ 0.1 | Adversarial 300, P ≥ 0.1 | Fresh adversarial 300, P ≥ 0.1 | Fresh adversarial 300, P ≥ 0.5 |
|---|---|---|---|---|
| Original data only (seed 0) | 93.8%, 4/267, 112/133 | 80.3%, 22/200, 63/100 | 78.3%, 34/200, 69/100 | 78.7%, 46/200, 82/100 |
| + supplement, seed 0 | 93.8%, 4/267, 112/133 | 85.7%, 20/200, 77/100 | 87.7%, 15/200, 78/100 | 87.0%, 20/200, 81/100 |
| + supplement, seed 1 | 96.0%, 6/267, 123/133 | 87.7%, 29/200, 92/100 | 88.3%, 29/200, 94/100 | 86.0%, 38/200, 96/100 |
| + supplement, seed 2 | 95.5%, 3/267, 118/133 | 85.3%, 24/200, 80/100 | 85.3%, 29/200, 85/100 | 85.0%, 30/200, 85/100 |

With the cut-off chosen on the validation split (the most accurate cut-off
with at most 5% wrong skips, from a grid down to 0.01):

| Checkpoint | Cut-off | Realistic 400 | Fresh adversarial 300 |
|---|---|---|---|
| + supplement, seed 0 | 0.03 | 86.0%, 1/267, 78/133 | 83.0%, 5/200, 54/100 |
| + supplement, seed 1 | 0.02 | 94.8%, 3/267, 115/133 | 87.7%, 18/200, 81/100 |
| + supplement, seed 2 | 0.01 (no cut-off met the rule; never skips) | 66.8%, 0/267, 0/133 | 66.7%, 0/200, 0/100 |
| Original data only | 0.01 (no cut-off met the rule on its 51 unseen validation pairs; never skips) | 66.8%, 0/267, 0/133 | 66.7%, 0/200, 0/100 |

What this shows:

- **The supplement helps on data it did not shape.** On the fresh adversarial
  set, all three seeds beat the original model by 6–10 accuracy points at
  either cut-off. Wrong skips fall from 34 to 15–29 at P ≥ 0.1. The realistic
  set does not get worse.
- **The gate is still not safe.** At the cut-offs that give useful accuracy,
  every seed wrongly skips 7–19% of the work requests in the adversarial
  sets. The safety rule picks very different cut-offs per seed. One seed ends
  up never skipping, and another catches only 54% of the skips.
- **The remaining errors are confident.** All three seeds score some
  correctly labelled work requests below 0.04:
  - "lol no" after "the tests pass, so the Safari bug is fixed, right?"
  - "nah" after "I'm about to delete the legacy endpoints. Want me to keep
    the /v1/health one around?"

  They also score "1" after "Reply 1 to wrap up, or 2 to have me keep going"
  above 0.98. Such replies carry meaning only by mapping onto the exact
  question or option. More data of the same size moved them very little.
- **Seed variance is large.** For the same data, the three seeds differ by up
  to 14 wrong skips and 16 caught skips on the fresh adversarial set at
  P ≥ 0.1. Any shipped gate needs several seeds, and an ensemble or a margin
  chosen across them.

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
5. **A trained gate is not yet safe enough to ship.** Only a fully
   fine-tuned cross-encoder that reads both texts together learns the task.
   Fine-tuned Laya reaches 94–96% on realistic pairs, and a targeted training
   supplement lifts it from 78% to 85–88% on fresh adversarial pairs. But every
   seed still wrongly skips 7–19% of the adversarial work requests at useful
   cut-offs, often with high confidence, and the seeds disagree. Fine-tuned
   MiniLM is about 15 times faster per pair but less stable. The next levers
   are real pairs from harvested transcripts, an ensemble of seeds, and a
   design that searches when unsure, not a larger synthetic set.
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

# Trained gate classifiers
cargo run --release --example prompt_gate_classifier
python tools/prompt-turn-eval/minilm_gate_finetune.py examples/data/prompt_turn_train.json \
    examples/data/prompt_turn_eval.json 0,1 \
    examples/data/prompt_turn_test_realistic.json examples/data/prompt_turn_test_hard.json
python tools/prompt-turn-eval/to_laya_jsonl.py examples/data/prompt_turn_train.json train.jsonl
laya-train --data train.jsonl --base convaiinnovations/laya --out laya-gate \
    --epochs 4 --micro-batch 8 --grad-accum 1 --device cpu --seed 0 --max-len 256
python tools/prompt-turn-eval/laya_gate_eval.py laya-gate examples/data/prompt_turn_eval.json \
    examples/data/prompt_turn_test_realistic.json examples/data/prompt_turn_test_hard.json

# Retraining with the supplement (WORK_DIR holds laya-venv and laya-data)
python tools/prompt-turn-eval/split_gate_data.py $WORK_DIR/laya-data/aug 0.15 \
    examples/data/prompt_turn_train.json examples/data/prompt_turn_train_supplement.json
python tools/prompt-turn-eval/to_laya_jsonl.py $WORK_DIR/laya-data/aug/train.json $WORK_DIR/laya-data/aug/train.jsonl
WORK_DIR=$WORK_DIR tools/prompt-turn-eval/retrain_driver.sh
for s in 0 1 2; do
  python tools/prompt-turn-eval/gate_threshold_report.py $WORK_DIR laya-ckpt-aug-s$s $WORK_DIR/laya-data/aug/val.json \
      examples/data/prompt_turn_test_realistic.json examples/data/prompt_turn_test_hard2.json
done
```

The example prints the per-prompt errors for both prototype decisions after
the tables above.
