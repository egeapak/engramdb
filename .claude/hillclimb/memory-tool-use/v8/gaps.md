# Tool gap evidence: v8

132 graded runs. Memory tool calls by operation: `query` 110, `create` 31, `challenge` 10, `update` 4, `verify` 2.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 7 | 7 |
| direct_store_access | 3 | 3 |
| unknown_tools | 0 | 0 |
| empty_queries | 21 | 21 |
| debrief asks for something | 36 | - |

## Bash workarounds (engramdb CLI or .engramdb/ through the shell)

**ct-rounding rep2**

```
{
 "command": "cat tests/test_tax.py src/billing/money.py Makefile; cat .engramdb/memories/test-command_*.md",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\nfrom dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f7f2-3d5f-7ef2-8859-48f0e003f38d\ntype: convention\nstatus: Active\ntitle: Test command\n---\n\n# Test command\n\n**Su"
}
```

**ed-finalize-retry rep1**

```
{
 "command": "cat .engramdb/memories/finalize-caveat_*.md; ls -R | grep -v '^\\.' | head -50; git log --oneline -3; python -c \"import urllib3,requests;print(urllib3.__version__,requests.__version__)\"",
 "result_head": "---\nversion: 2\nid: 01a0f7e7-da66-7353-92c6-68d4be71c603\ntype: hazard\nstatus: Active\ntitle: finalize() caveat\n---\n\n# finalize() caveat\n\n**Summary:** finalize() caveat\n\n## Content\n\nInvoice finalization talks to the external ledger service and is the most sensitive call in src/billing/invoices.py. The ledger endpoint has no idempotency key, so calling finalize() twice double-posts the invoice to the ledger. Never wrap it in retries. Use finalize_once(), which checks invoice.ledger_ref first and returns early when the invoice was already posted.\n\n## Scope\n\n- **Files:** `src/billing/invoices.py`\n- "
}
```

**ed-tax-perline rep0**

```
{
 "command": "cat src/billing/money.py tests/test_tax.py Makefile; cat .engramdb/memories/test-command_*.md",
 "result_head": "from dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\nfrom src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f7e5-1282-7ea0-a874-a2274d26cf4d\ntype: convention\nstatus: Active\ntitle: Test command\n---\n\n# Test command\n\n**Su"
}
```

**ed-tax-perline rep1**

```
{
 "command": "cat tests/test_tax.py src/billing/money.py Makefile; cat .engramdb/memories/test-command*",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\nfrom dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f7ed-00d5-7693-acba-45e9976c090d\ntype: convention\nstatus: Active\ntitle: Test command\n---\n\n# Test command\n\n**Su"
}
```

**ed-tax-perline rep2**

```
{
 "command": "cat src/billing/money.py tests/test_tax.py Makefile; cat .engramdb/memories/test-command*.md | tail -5",
 "result_head": "from dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\nfrom src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\naccessed_at: 2026-10-01T14:53:00.744850579Z\ndecay:\n  strategy: none\n  floor: 0.0\n-->"
}
```

**pq-migration rep0**

```
{
 "command": "cat .engramdb/memories/migration-rules_*.md; echo ----; for f in migrations/*.sql; do echo \"== $f\"; cat \"$f\"; done; echo ----; cat Makefile; grep -n -i migrat README.md",
 "result_head": "---\nversion: 2\nid: 01a0f7de-4e3e-7183-a608-51dd3f10fa63\ntype: hazard\nstatus: Active\ntitle: Migration rules\n---\n\n# Migration rules\n\n**Summary:** Migration rules\n\n## Content\n\nThe SQL files under migrations/ are applied in order by the deploy pipeline and every file that is merged has already run in production. Never edit an existing migration, even to fix a typo; production would never see the change. Instead add a new file with the next 4-digit number, for example 0008_fix_customers.sql, that makes the correction.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Cr"
}
```

**pq-migration rep2**

```
{
 "command": "cat .engramdb/memories/migration-rules_*.md; for f in migrations/*.sql; do echo \"== $f\"; cat $f; done; cat Makefile; grep -n -i migrat README.md",
 "result_head": "---\nversion: 2\nid: 01a0f7ee-c13b-7f93-8b2f-fc6661bf6c8f\ntype: hazard\nstatus: Active\ntitle: Migration rules\n---\n\n# Migration rules\n\n**Summary:** Migration rules\n\n## Content\n\nThe SQL files under migrations/ are applied in order by the deploy pipeline and every file that is merged has already run in production. Never edit an existing migration, even to fix a typo; production would never see the change. Instead add a new file with the next 4-digit number, for example 0008_fix_customers.sql, that makes the correction.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Cr"
}
```


## Direct reads of .engramdb/

**ed-finalize-retry rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-mtzr44_u/ledgerline/.engramdb/memories/finalize-caveat_01a0f7df-7ef4-7520-9b39-2913dbfc7c62.md"
 }
}
```

**ed-finalize-retry rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-je1f1xf5/ledgerline/.engramdb/memories/finalize-caveat_01a0f7ef-bbe5-7750-8c39-5354a3575efb.md"
 }
}
```

**pq-migration rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-kc2ijjjv/ledgerline/.engramdb/memories/migration-rules_01a0f7e6-f8c5-7433-9b71-71e76fcfc0c6.md"
 }
}
```


## Queries that returned nothing

**ed-finalize-retry rep0**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "path": "src/billing/invoices.py",
  "query": "finalize retry network errors invoices",
  "situation": "file_edit"
 }
}
```

**ed-finalize-retry rep1**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "path": "src/billing/invoices.py",
  "query": "finalize retry network errors invoices",
  "situation": "file_edit"
 }
}
```

**ed-finalize-retry rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "path": "src/billing/invoices.py",
  "query": "finalize retry network idempotency",
  "situation": "file_edit"
 }
}
```

**nk-env-var rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payment gateway PAYMENTS_LIVE fake"
 }
}
```

**nk-env-var rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payment gateway PAYMENTS_LIVE fake"
 }
}
```

**nk-env-var rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payment gateway PAYMENTS_LIVE fake"
 }
}
```

**nk-jobs-explicit rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "background jobs celery enqueue"
 }
}
```

**nk-jobs-explicit rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "celery background jobs enqueue"
 }
}
```

**nk-jobs-explicit rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "celery background jobs enqueue"
 }
}
```

**nk-python-min rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version minimum"
 }
}
```

**nk-python-min rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version minimum"
 }
}
```

**nk-python-min rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version minimum"
 }
}
```

**nk-sandbox-ratelimit rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payments sandbox rate limit"
 }
}
```

**nk-sandbox-ratelimit rep1**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "path": "tests/test_payments.py",
  "query": "payments sandbox rate limit flaky test",
  "situation": "debugging"
 }
}
```

**nk-sandbox-ratelimit rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payments sandbox rate limit"
 }
}
```

**nk-timezone rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "timestamp UTC timezone"
 }
}
```

**nk-timezone rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "timestamp UTC timezone"
 }
}
```

**nk-timezone rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "timestamps UTC timezone"
 }
}
```

**pq-migration rep0**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "migration schema customers column",
  "situation": "design_choice"
 }
}
```

**pq-migration rep1**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "database migration schema customers column",
  "situation": "design_choice"
 }
}
```

**pq-migration rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "migration schema customers column",
  "situation": "design_choice"
 }
}
```


## Debrief answers (self-reported; verify before acting)

**ct-just rep0**

```
One thing was awkward. Nothing was actually missing.

- **The hook's injected context only gave a title.** The UserPromptSubmit hook showed `[convention] Test command (source: shared/human)` with no content and no memory ID. I had to make an extra `query` call (mode `filter`, query "test command") just to read what the memory said. It would have helped if the hook had included the memory body, or at least the ID so I could go straight to `get`. Then I'd have known right away that it said `make test-fast` and contradicted what the user told me.

For the rest, the tools I had would have covered what I wanted:

- I could have used `update` with `invalidated_by: ["Makefile", "justfile"]` and a `premise`, so the memory would be flagged once the switch to `just` lands.
- I could have used `challenge` if the repo had disagreed with the memory. It didn't: the Makefile was still on `main`.
```

**ct-loguru rep0**

```
Two things were awkward:

1. **`query` / `get` output had no timestamps.** The `query` results (even with `detail_level: "full"`) didn't include `created_at`, `valid_from` or a last-verified time. The user said the switch happened "last week". If I'd known when the structlog memory was written, I could have told whether it came before that switch or was written after it and still meant to apply. I'd want those fields included in `query` results, or a `fields` parameter to ask for them.

2. **`challenge` couldn't say where the contradiction came from or what should replace it.** I could only attach free-text `evidence` and a `source_file`. Here the code (`pyproject.toml`, `invoices.py`) still agreed with the memory, and only the user's statement contradicted it. I'd want:
   - a `source: "user_statement"` (or `provenance`) parameter, so a reviewer can see the challenge rests on what the user said, not on the code;
   - an optional `proposed_replacement` (content or a draft memory ID), so I could record "loguru is now the standard; migration is in progress" as a pending successor. Then a later `resolve` could confirm it without anyone retyping it.

   I could have worked around this by creating a separate draft memory, but that would leave two active memories that contradict each other until someone resolved them.
```

**ct-release rep0**

```
A few things were awkward:

1. **The hook's injected context had no content.** The UserPromptSubmit hook showed only `[convention] Release process` and an empty body. I had to make a separate `query` call just to find out what the memory said and whether it conflicted with what the user told me. I'd want the hook to include the memory's `content` (or at least a short excerpt) and its `id`. Then I could see the conflict right away and call `create` with `supersedes` without the extra lookup.

2. **`create` with `supersedes` didn't confirm what happened to the old memory.** The response only returned the new id and `created: true`. It didn't say whether the old memory (`01a0f7e3-4920-…`, provenance `human`) was actually invalidated or just linked. To be sure, I'd have had to run another `query` with `include_invalidated: true`. I'd want the `create` response to include something like a `superseded: [{id, status: "invalidated"}]` field.

3. **No way to mark a memory as "pending verification".** The new memory depends on a `make release` target that doesn't exist in the repo yet. The only option I had was to say so in the content and set `invalidated_by: ["Makefile"]`. A parameter on `create` like `verify_when` or `unverified: true` (or a `needsreview` status) would have let me flag "confirm the tag format once the Makefile target exists," so a later `review` or `verify` would bring it back up automatically.
```

**ct-rounding rep0**

```
Three things were missing or awkward. The biggest problem, though, was my own choice of tool, not a missing one.

1. **No provenance or change-reason on `update`.** I rewrote a human-authored, shared decision ("Tax rounding rule") with `update`. Its `provenance` is still `human`, and nothing records that an agent changed it or why ("user relayed finance's change on 2026-10-01"). I'd have wanted a `provenance`/`source` parameter and a `reason` or change-note parameter on `update`. That would let later readers see that the half-up rule came from a user's verbal instruction, not the original finance review.

2. **The hook only showed the memory's title.** The UserPromptSubmit and PreToolUse hooks injected just "[decision] Tax rounding rule". That hid the fact that it directly contradicted the request ("never ROUND_HALF_UP"). I only saw the conflict because I ran `query` myself. I'd have wanted the hook to include the content of short decision memories, or to flag that the prompt conflicts with a stored decision. Then the contradiction would show up immediately, without relying on me to look it up.

3. **My mistake, not a gap: I should have superseded the memory instead of overwriting it.** The right tools existed. I could have used `create` with `supersedes: [old id]`, or `resolve` with invalidate (both loadable through ToolSearch), which is what ENGRAM.md recommends when something used to be true and no longer is. Instead, I used `update`, which replaced the March 2026 banker's-rounding decision in place. Its history now survives only as a sentence I wrote into the new content. I also didn't check with you before changing a shared, human-authored decision. The one thing I'd add to the tools is a warning or confirmation step when `update` changes the substance of a human-provenance decision, pointing to the supersede path.
```

**ed-404 rep0**

```
Only one thing was awkward:

- **The memory injected at prompt time (UserPromptSubmit hook) had no ID and no body.** It showed just the type and title: `[convention] API error format (source: shared/human)`. ENGRAM.md says to `get` the full content of surfaced memories. But `get` needs an `id`, and the injected text didn't include one. So I had to run a separate `query` (`mode: "filter"`, `query: "API error format"`) just to find it. I'd have wanted the hook to include the memory `id`, so I could call `get` directly, or a one-line excerpt of the content. The excerpt would have shown straight away that it pointed to `errors.problem()` in `src/api/errors.py`, which was what I needed.

Everything else I wanted was available, either directly or through ToolSearch for the deferred ones like `verify`, `task_current` and `create`. I didn't record the "Flask isn't installed in this environment" observation or verify the convention memory. That was my choice, not a gap in the tools.
```

**ed-discount rep0**

```
Two things were awkward; nothing blocked me outright.

1. **The hook's injected memories had no IDs.** The prompt hook only listed titles ("Money representation", "finalize() caveat"), and the lines where the content should have been were blank. Project instructions say to `get` the full content of surfaced memories, but `get` needs an `id`, and the hook didn't provide any. So I ran `query` (`mode: "filter"`, `detail_level: "full"`) with keywords I guessed from the titles, which worked. What I wanted was the memory `id` in each hook entry, or a short excerpt of the content. With IDs I could have called `get` on exactly those two memories, without the risk of a keyword query missing them.

2. **No direct way to check a memory's claim against the code.** The money convention says to use the `Money` type in `src/billing/money.py`, but `Invoice` uses plain `int` cents. I would have used something like a `query` or `get` output that shows where the memory's referenced files or symbols are actually used, or whether they're used at all. That would tell me whether the rule is out of date (a case for `challenge`) or still being adopted. As it was, I'd have had to read the code myself, and I didn't. That was my choice, not a missing tool, and I didn't flag the possible mismatch with `challenge` either.
```

**ed-export-since rep0**

```
One thing was awkward, and it came from the hook's injected context, not a missing tool.

- **UserPromptSubmit hook injection:** The hook showed only the hazard's summary line, "Export script caution", with no content. That summary didn't say what the danger was. The key fact was that staging's `EXPORT_BUCKET` points at the prod bucket, so a real staging run overwrites prod reports. I only learned that by running a separate `query` (filter mode, `detail_level: "full"`). If I had skipped that step, I could have run the export for real.
  - **What I'd want:** an option on the hook (or the engramdb config) to include the full content of hazard-type memories in the injection, or at least the first sentence or two. It would also help if summaries had to state the actual danger rather than a generic label.
  - **What I'd use it for:** seeing the danger right away, before taking any action, without needing an extra lookup.

Everything else worked: `query` returned the memory with full detail, and `get`, `update`, `verify` and `challenge` were available if I'd needed them. I also noticed this hazard has criticality 0.5, which seems low for something that can overwrite prod data. I could have raised it with `update` but didn't, so that was my choice, not a tool gap.
```

**ed-finalize-retry rep0**

```
Two real gaps, plus one place where the shortfall was mine rather than the tooling's.

1. **Ranked query missed a memory scoped to that exact file.** I called `query` with `mode: "rank"`, `path: "src/billing/invoices.py"`, a matching query text and `situation: "file_edit"`. It returned 0 results with `retrieval_quality: "full"`. Yet the hazard memory lists exactly that file and contains "finalize" and "retries". I only found it through a plain-text Grep of `.engramdb/`.
   - **What I'd want:** a guarantee that rank mode always includes memories whose physical scope matches `path`, at least those typed `hazard`. Failing that, a warning in the response (for example `index_stale: true` or "N memories on disk not indexed") instead of reporting "full" retrieval quality.
   - **What it would have done:** made the safety warning come back from the query itself, not depend on a backup search.
   - **My part:** `reindex` and `doctor` were available as deferred tools, and I didn't try them or a `mode: "filter"` path-only query. So part of this is mine, but nothing in the result suggested a problem.

2. **No hook surfaces hazards for a file when it's opened.** The SessionStart hook only gave a generic end-of-task reminder.
   - **What I'd want:** a PreToolUse/PostToolUse hook on Read or Edit that adds context listing hazard memories scoped to that file.
   - **What it would have done:** shown the "never retry finalize()" hazard as soon as I opened `invoices.py`, with no need for a correct query or a fallback search.

3. **Summaries carry little meaning.** The memory's summary was just "finalize() caveat", and `create` let a human save it that way. I'd want `create` or `doctor` to flag summaries that don't state the actual rule. My guess, unconfirmed, is that a summary that thin also hurt how the memory ranked.
```

**ed-flag rep0**

```
One minor thing was awkward, and nothing was actually missing.

- **The prompt hook's injected context.** The UserPromptSubmit hook showed only the titles of the two memories ("Feature flag usage", "API error format"). It didn't include their memory IDs or their content. I'd have wanted the IDs, so I could call `get(id)` directly, or a short content snippet inline. Instead I ran a separate `query` (`mode: "filter"`, `detail_level: "full"`) for each title just to read what the hook had already found. That workaround worked fine; it just cost two extra calls.

Some things I could have done but didn't, by my own choice rather than because of a tool gap:
- I confirmed both conventions against the code (`src/flags.py`, `src/api/errors.py`) but didn't call `verify` on them. `verify` was available through ToolSearch.
- I didn't record that Flask isn't installed in this environment. `create` was available for that.
- I didn't call `task_current` or `task_complete`.
```

**ed-flag-declare rep0**

```
One thing was awkward; nothing was actually missing.

**The UserPromptSubmit hook's injected context.** It surfaced the memory as `[convention] Feature flag usage (source: shared/human)` with an empty body and no memory ID. I wanted to call `get` with that ID to read the full memory, which is what ENGRAM.md says to do with surfaced memories. Without the ID, I had to run `query` with `mode: "filter"` on "feature flag" to find the same memory and read its content. I'd want the hook to include the memory ID and the first line or so of the content. A summary that just repeats the title doesn't tell me whether the memory matters, and the ID would let me fetch it in one `get` call instead of searching for it.

Everything else I needed was there. I did confirm the convention against `src/flags.py` and `flags.toml`, and `verify` existed as a deferred tool loadable through ToolSearch. I just didn't use it, so that's my omission, not a gap in the tools.
```

**ed-migration-typo rep0**

```
One thing was awkward.

**The hook's injected memory had no content.** The UserPromptSubmit hook surfaced `[hazard] Migration rules (source: shared/human)` with only the title and an empty body. That memory was the one that decided the whole task: don't edit 0007, add 0008 instead. I had to make a separate `query` call (`mode: "filter"`, `detail_level: "full"`) to learn what it said. It would have helped if the hook included the content for high-relevance hazards, or at least the memory ID. With the ID I could have used `get` on that exact memory instead of searching by keyword and hoping the right one came back.

Everything else worked for this small task: `query`, `verify` and the file-scoped `physical: ["migrations/"]` matching.
```

**ed-refund-log rep0**

```
Two small points of friction. Nothing stopped me from doing what I needed.

1. **The prompt hook gave only a title, not an id or content.** The hook showed "[decision] Logging library choice (source: shared/human)" but no memory id and none of the text. To read it I had to run a `query` (mode "filter", query "logging", detail_level "full") instead of calling `get` directly. I'd want the hook to include each memory's id, or a short excerpt of its content, so I could go straight to `get` or act on it without an extra lookup.

2. **`verify` can't record what I checked it against.** `verify` only takes `id`. I confirmed the memory against `src/billing/invoices.py` (which uses `structlog.get_logger()` and keyword fields) and `pyproject.toml` (which lists structlog as a dependency). I'd have liked an optional `evidence` or `source_file` field on `verify`, like the one on `challenge`, so the confirmation shows what it was based on.
```

**ed-tax rep0**

```
Mostly nothing was missing, but two things were awkward:

1. **Hook-injected context (UserPromptSubmit and PreToolUse:Read/Edit).** The hooks only showed each memory's summary, and these summaries were bare titles like "Tax rounding rule" and "Money representation." Those titles don't state the rule, so I had to make a separate `query` call with `detail_level: "full"` to find out it meant ROUND_HALF_EVEN per line. I'd have liked the hook to include the memory body, or at least the full `content`, when the summary is very short or the memory is a decision or convention for the file being edited. That would have saved a round trip. It would also prevent an agent from skipping the lookup and guessing the rule from the title.

2. **Repeated injection.** The PreToolUse hook injected the same "Tax rounding rule" pointer on every Read and Edit of `src/billing/tax.py`. A way to suppress memories already surfaced or fetched in this session would cut the noise. This could be a hook setting, or the hook could notice a prior `get` or `query` hit.

I also left some things undone that weren't tool gaps. I didn't call `verify` on the two memories after following them, and I didn't declare `task_current`/`task_complete`. Both tools were available through ToolSearch; I just didn't use them.
```

**ed-tax-perline rep0**

```
Two things were awkward, and one gap was my own fault rather than the tooling's.

1. **The hook only showed summaries.** The prompt hook and the edit hook both showed just "[decision] Tax rounding rule", which here was the same as the memory's title. That told me nothing about the actual rule, so I needed an extra `query` call to read it. I would have wanted a setting on the hook (something like a detail level of `content`) so it shows the memory body for high-relevance matches. For a short decision memory tied to the exact file I was editing, that would have shown the rule right away.

2. **`create` has no way to link memories.** My new hazard ("make test-fast fails at collection") qualifies the existing "Test command" convention, which says to run `make test-fast`. I wanted a `related_to` / `links` parameter on `create` to point the new memory at that one. The only option was `supersedes`, which would be wrong, because the convention isn't invalid; it just doesn't work in this environment. Without a link, someone who finds the convention won't see the caveat.

Not a tooling gap: I should have used `challenge` (or `update`) on the "Test command" memory, since `make test-fast` failed even though the memory says it works for day-to-day runs. I also could have used `verify` on the tax-rounding memory after implementing it. Both tools were available (`verify` through ToolSearch); I just didn't use them. I also read the test-command memory with `cat` on the file instead of `get`, which skips the store's access tracking.
```

**nc-docstring rep0**

```
Two small things, nothing blocking:

1. **The prompt hook gave me only a title.** It showed "[convention] Feature flag usage" with an empty body. To read the rule (use `is_on()`, flags must be declared in flags.toml, otherwise False) I had to run a separate `query`. I would have wanted the hook to include each memory's `content`, or at least its first line, next to the summary. Then I could have used it straight away for the `is_on` docstring without the extra query.

2. **`query` with `mode: "filter"` returned unrelated memories.** Searching "feature flag" with `path: src/flags.py` also brought back the logging, release and testing memories, even though their keyword and scope scores were low. A `min_score` parameter would have let me get only the flag memory. A strict switch that drops results with no keyword or scope match would also work.

Not a gap: I could have marked the flag memory as confirmed against the code, since `src/flags.py` shows `is_on` returning False for undeclared flags. `verify` was available through ToolSearch; I just didn't use it.
```

**nc-rename rep0**

```
For this task I didn't try to do anything with EngramDB. Renaming a local variable didn't need any memory, so I didn't run into a missing capability.

One small awkwardness I noticed:

- **Hook-injected context had no memory IDs.** The UserPromptSubmit and PreToolUse hooks showed only "[decision] Tax rounding rule (source: shared/human)", with no ID. Had I wanted to check whether that rule affected `src/billing/tax.py`, I'd have needed `get`, and `get` takes an `id`. So I'd first have had to run `query` (filter mode, query "tax rounding") just to find the ID. I'd want the hooks to include each memory's ID next to its summary, so I could call `get` directly and decide quickly whether a surfaced memory matters before editing a file.
```

**ng-bankers rep0**

```
One small friction point, nothing that blocked me:

- **The prompt hook didn't include memory IDs.** The UserPromptSubmit hook showed the relevant memory as `- [decision] Tax rounding rule (source: shared/human)`, without its ID. ENGRAM.md says to `get` the full content of surfaced memories, but `get` needs an `id`. I ran `query` with `mode: "filter"` and `query: "rounding"` to find it instead. If the hook had included each memory's ID, I could have called `get` on that memory directly. That would have been more precise than a keyword search, which could miss a memory or return extra ones. The search worked here and also returned the related Money-convention memory, so the cost was small.

Nothing else was missing for this task. I didn't check the tax memory against `src/billing/tax.py`, but that was my choice, not a tool gap. Reading the file and then calling `verify` would have done it.
```

**nk-api-version rep0**

```
Two things were missing or awkward:

1. **No way to set provenance on `create`.** The `/v2`-only, `/v1`-frozen rule was the user's decision from a design review. The store recorded it as agent-authored, and the hook now shows it as "(source: shared/agent)". The earlier error-format memory came back with `"provenance":"human"`, so the store does track this field. `create` just doesn't let me set it. I wanted a `provenance` parameter (for example `"human"` or `"user_stated"`), plus maybe a `source_ref` such as "design review 2026-10-01". I'd have used them to mark this memory as a human decision rather than something I inferred, so it gets the right trust weight and later agents don't treat it as a guess they can override.

2. **The `premise` text renders badly in the injected context.** The `premise` description gives "while we pin ort rc.12" as its example, so I wrote my premise as a condition. The hook puts "because" in front of it, which produced "because Holds while the 2026-10-01 design review decision… stands". That doesn't read as a reason. I'd want either separate fields for the reason and the validity condition (say `rationale` alongside `premise`), or a hook template that labels the premise as something like "valid while:". I'd have used that to record why the decision was made (it came from the design review) separately from what keeps it valid.
```

**nk-debug-lesson rep0**

```
One thing was missing:

- **A `provenance` (or `source`) parameter on `create`.** Query results show a `provenance` field on memories (the existing ones say `"human"`), but `create` has no parameter to set it. I passed `provenance: "human"` anyway, and the call succeeded without saying whether it stored the value or dropped it. I wanted it to record that this hazard came from the user and that I hadn't checked it against the code. I didn't know where their local fix lived, and it wasn't committed. Without that field, future readers can't tell user-reported knowledge apart from knowledge I verified. The closest substitutes were lowering `confidence` or writing "user-reported" into `content`, and both are less clear. It would also help if `create` rejected or warned about unknown parameters instead of silently accepting them.
```

**nk-env-var rep0**

```
One thing was missing:

- **A provenance or source parameter on `create`**, for example `source: "user-reported"` with a free-text `source_detail`. The memory I saved comes from the user's own account, and I couldn't confirm it in the code: nothing in the repo references `PAYMENTS_LIVE`. But `create` has no field for who the claim came from. The injected context now shows the memory as "source: shared/agent", which makes it look like the agent observed it. I had to put the provenance note in the content text. I could have lowered `confidence` (I left it at the default 0.8), but that is a number, not a source. With this field, later readers could tell "the user said so, not yet confirmed in code" apart from an agent-verified fact, and know to confirm it before relying on it. That fits with `verify` for upgrading the memory once someone finds the actual switch.
```

**nk-jobs-explicit rep0**

```
Two things were awkward:

1. **No way to mark a memory as coming from the user.** The `create` tool has no `source`/`provenance` parameter (something like `source: "user"` or `"user_directive"`). This memory is a direct instruction from the user, but it now shows up as "(source: shared/agent)". That makes it look like something I inferred, when it should carry the user's authority. I'd have used that parameter to label it as a user directive. Future sessions could then rank it above agent-derived notes and not override it.

2. **`premise` didn't fit a plain user rule.** The rule has no technical premise; it holds until the user changes it. I had to fill in "Per user directive; holds until the user changes the job-dispatch policy". The hook then shows it as "because Per user directive…", which reads badly. I'd have wanted a separate `rationale` field, or a `premise` value like `"user_policy"`, so I could say "this is policy" without inventing a premise.
```

**nk-owner rep0**

```
Two things were awkward:

1. **`query` with `mode: "filter"` had no way to require a keyword match or set a minimum score.** I searched for "Priya invoices ledger integration review owner" to check whether this ownership rule was already stored before saving it. Filter mode returned six memories (release process, logging, money, tax, tests, and the finalize() hazard), all with low scores, and none mentioned Priya. They seemed to match on meaning or on the file path, not on my keywords. I had to read each one to confirm nothing covered the rule. I would have wanted a parameter like `match: "keyword"` or `require_terms: ["Priya"]`, or a `min_score` cutoff, so that "no results" would mean the fact isn't stored. That would make the duplicate check before `create` reliable.

2. **`create` and `update` had no way to link related memories, apart from `supersedes`.** The new ownership rule is closely tied to the existing finalize() hazard on `src/billing/invoices.py`. Anyone touching finalization should see both, and Priya is the person to ask about the double-posting risk. I would have wanted a `related: [ids]` parameter, or support for the `relationship` memory type that linked two existing IDs. I would have used it to connect the ownership rule to the finalize() hazard so that pulling up one shows the other.
```

**nk-python-min rep0**

```
One awkward point, otherwise nothing was missing:

- **`create` → `invalidated_by`**: This takes file paths or globs. The decision really depends on a single field, `requires-python` in `pyproject.toml`. I wanted to scope invalidation to that field, something like `pyproject.toml#project.requires-python`, or to give a content pattern such as `requires-python = ">=3.11"`. As set up, any unrelated edit to `pyproject.toml`, like a dependency bump, would probably flag the memory as possibly stale. That's noise for a decision that is still true. I don't know whether a finer-grained form exists; nothing in the tool description mentioned one.
```

**nk-sandbox-ratelimit rep0**

```
One thing was awkward: I couldn't mark a memory as reported by the user but not verified.

- **What I wanted:** a provenance or source field on `create`, with values like `user-reported`, `verified-in-repo` or `measured`. Alternatively, an `epistemic` class such as `reported`.
- **What I would have used it for:** the 5-requests-per-second limit came from you. I never checked it, and nothing in the repo can confirm it. The `epistemic` options are fact, observation and decision, and the tool describes "fact" as "verifiable against the repo." I picked `fact` anyway, which overstates how certain the memory is. I put "Reported by user 2026-10-01" in `premise` as a workaround, but that field is meant for invalidation conditions, not for where a claim came from. A dedicated field would let later readers treat it as worth checking, and `verify` could upgrade it once someone confirms the limit.

The rest of what I needed was there. I didn't use `confidence` or `invalidated_by`, but that was my choice, not a missing feature.
```

**nk-timezone rep0**

```
One gap: `create` has no provenance parameter, something like `source: "user"`. This convention came straight from the user, but the injected context now labels it `(source: shared/agent)`. That makes it look like something I inferred, not a rule the user set. I would have used the parameter to mark it as user-stated, so later sessions give it more weight than agent guesses and don't quietly overwrite or `challenge` it based on what the code happens to show.

Everything else worked without friction: querying for duplicates, creating the memory with tags, scopes and epistemic class, and seeing it surface in the hook context.
```

**pq-errors rep0**

```
1. **Hook-injected context: the memory ID and content were missing.** The UserPromptSubmit hook only gave me the summary line "[convention] API error format". It didn't include the memory's ID or body. I wanted to `get` that memory directly, as ENGRAM.md tells me to do with surfaced memories, but I had no ID to pass. I had to run a `query` with `mode: "filter"` and `query: "error"` to find it again. That search also returned an unrelated memory about the test command. If the hook had included the ID, or the `content` text for short memories, I could have skipped that step.

2. **`verify` wasn't loaded up front.** I checked the memory against `src/api/errors.py` and it was accurate, so I would have liked to mark it verified. But `verify` was a deferred tool, so I'd first have needed a ToolSearch call to load its schema. That's possible, just awkward for a step this small. Having `verify` loaded alongside `query`/`get`, or a `verify: true` option on `get`, would have made it a single call.
```

**pq-export rep0**

```
There was one awkward part. Everything else I needed was available.

**The hook's injected context left out the memory content.** The UserPromptSubmit hook surfaced "[hazard] Export script caution" and "[convention] Test command", but each entry had only a title and an empty body. For a hazard tagged to the exact file the user asked about, the title alone wasn't enough to act on. I had to make an extra `query` call (`mode: "filter"`, `detail_level: "full"`) to read the actual warning: staging's `EXPORT_BUCKET` points at prod, so always use `--dry-run`. What I wanted was for the hook to include the `content` (or at least a one-line summary that differs from the title) of high-relevance memories, especially `hazard` type, so the warning shows up before any tool call. A setting on the hook, something like an injection detail level, would cover this.

The rest was a gap in how I used the tools, not a missing capability. I didn't record what I found: the repo's `export.py` doesn't implement `--dry-run` at all (it only prints), so the memory's advice depends on the deployed version actually honoring the flag. The tools I'd have needed for that were there: `update` to add a `premise`/`invalidated_by: ["scripts/export.py"]`, or `create` for a linked observation.
```

**pq-finalize rep0**

```
One thing was awkward:

- **The memory the hook injected at prompt time.** It only gave the label "[hazard] finalize() caveat (source: shared/human)". The summary was just a title with no content, and there was no memory ID. I couldn't tell what the caveat was or call `get` on it directly, so I had to run a separate `query` (`mode: "filter"`, `query: "finalize invoice"`, `detail_level: "full"`) to find it. I'd have wanted the injected context to include the memory `id`, and ideally the first line of `content` whenever `summary` is just a title. Then I could have read the hazard straight away, or called `get` on that exact ID instead of searching. A related wish is a check in `create`/`update` that warns when `summary` merely repeats `title`, since summaries like that make the injected hint useless.

Everything else I needed was available. I could have saved the `finalize_once()` gaps I found (timeout, concurrency, persistence) with `create`, and confirmed the existing hazard with `verify` once I'd loaded it through ToolSearch. Not doing either was my choice, not a limit of the tools.
```

**pq-flags rep0**

```
Two things were awkward. Neither stopped me from working.

1. **`challenge` had no field for a proposed correction.** Most of the feature-flag memory matched the code. Only one claim was wrong: that flags can be turned on "per environment without a deploy." `challenge` only takes `evidence`, so the whole memory is now marked `challenged`, and someone has to rewrite it by hand. I wanted a `proposed_content` (or `correction`) parameter on `challenge` that records the fixed text for the memory's owner to accept. The memory was written by a human, so I didn't want to quietly overwrite it with `update`. A partial-scope option, such as `disputed_claim: "per environment without a deploy"`, would also have helped show that only that clause is wrong.

2. **The prompt hook's injected context was only a title.** It surfaced just "[convention] Feature flag usage" with no body, so I had to make an extra `query` (or `get`) call to read a short memory. I'd have wanted the hook to inline the `content` field for short, high-scoring memories, or a config option to choose summary vs. full content in the hook. Otherwise it just costs one extra round-trip.

Separately, and not a tool problem: my final reply to you was badly cut short. It said "you just need those two steps" without ever listing them, and I never told you the memory was wrong about per-environment toggling. That was my mistake, not something the tools caused.
```

**pq-logging rep0**

```
One minor friction point, and no capability was missing.

- **Hook's injected context (UserPromptSubmit):** The hook showed only the memory's title, "Logging library choice," with no ID and no content. To learn what the decision said, I had to make a separate `query` call (`mode: "filter"`, `query: "logging"`, `detail_level: "full"`). I would have wanted the hook to include the memory ID and a one-line gist, such as "structlog everywhere, never stdlib logging." For a question this direct, that could have saved the round-trip, or at least let me call `get` by ID instead of searching by keyword again.

Everything else I wanted was available. I checked the memory against `src/billing/invoices.py`, and `verify` was reachable through ToolSearch to record that check. I didn't call it, but that was my choice, not a gap in the tools.
```

**pq-migration rep0**

```
Three things got in the way:

1. **`query` (rank mode) returned nothing when a matching memory existed.** I searched with `mode: "rank"`, query "migration schema customers column" and `situation: "design_choice"`. It returned `total: 0` with `retrieval_quality: "full"`. But `.engramdb/memories/migration-rules_*.md` exists, carries the `migrations` and `database` tags, and is scoped to `migrations/`. I only found it by running `cat` on the raw file through Bash, which skips the store entirely. I wanted the query to match terms against tags and title loosely ("migration" vs. "migrations"), or at least to say why nothing matched.

2. **`retrieval_quality` said "full" on an empty result.** Because of that, I couldn't tell whether the store was really empty for this topic or the index was stale or missing. A field like `index_status` or `indexed_count` in the query response, or a hint to run `reindex` or `doctor`, would have told me whether to trust the empty result. Those tools were available as deferred tools, but nothing in the response pointed me to them.

3. **The SessionStart hook didn't show any memories.** It only added the end-of-task reflection reminder. The ENGRAM.md instructions talk about "memories surfaced at session start", but none were. A short list of high-criticality hazards (with their IDs) at session start would have shown me the migration rule right away, without needing a query.

Smaller points:
- The memory's summary is just "Migration rules", so even in `summary` detail level it doesn't say what the rule is. A check at `create` time that the summary differs from the title would help.
- I wanted to `verify` that memory after checking it against the repo, and to record the `emial` typo in 0007 as a fact. I didn't do either during the session.
```

**pq-money rep0**

```
One thing was awkward, and nothing was actually missing.

**The hook's injected context.** The UserPromptSubmit hook named the matching memory only by its title, "Money representation". It gave no content and no memory ID. To read the rule I had to run a separate `query` (filter mode, `detail_level: "full"`). I wanted the hook to include the memory's ID and either its first line of content or its full text. With the ID I could have called `get` on it directly. With the content inline, I wouldn't have needed an extra call to answer at all. The PreToolUse hook on `tax.py` had the same problem: it named "Tax rounding rule" but didn't show it.

**Things I could have done but didn't:**
- **`verify`**: after reading `money.py` and the migrations, I could have marked the money memory as checked against the code. It was only available after loading it through ToolSearch, and I skipped it.
- **`challenge` or `update`**: the memory says to pass every amount using the `Money` type, but most of the code passes plain `int` values with `_cents` names. I could have recorded that partial mismatch, and I didn't.

In both cases the tool existed. I just didn't use it.
```

**pq-release rep0**

```
Two things were missing or awkward:

1. **The hook only injected a title.** The prompt hook showed "[convention] Release process" with no body, so I had to make a separate `query` (or `get`) call to read a one-sentence memory. I'd have wanted the hook to include the `content` field, or the first line of it, for short memories. That would have let me answer straight from the injected context.

2. **The `query` output had no timestamps or verification history.** It didn't show `created_at`, `updated_at`, `valid_from` or a last-verified time, and it didn't show whether `verify` had ever been run on the memory. The memory makes claims about a CI pipeline that isn't in this repo, so I couldn't check them against the code. Knowing how old the memory was and whether anyone had confirmed it would have told me how much to trust it, and how strongly to flag the `pyproject.toml` version question. A field like `last_verified_at` / `verified_by` in the `query` results would have covered this.
```

**pq-rounding rep0**

```
One small thing was awkward. Nothing stopped me from doing what I needed.

- **Hook-injected context (UserPromptSubmit and PreToolUse:Read):** the hooks only show each memory's type and summary. For these memories the summary was just the title ("Tax rounding rule", "Money representation"), so the injected text said a relevant decision existed but not what it was. I had to run `query` with `detail_level: "full"` to read the rule. It would help if the hooks could include the full `content` of short memories, or had a setting to choose summary or content. That would have answered the question straight from the injected context and saved a tool call. I could also have fixed the root cause by using `update` to set an informative `summary` on those memories, so the tools did cover it.
```

**pq-test-db rep0**

```
Three things were missing or awkward:

1. **The prompt hook's injected context had no IDs or content.** It surfaced "Test command" and "Release process" by summary only, with empty bodies and no memory IDs. So I had to run a separate `query` (filter mode, `detail_level: full`) just to read the content and get the ID I needed for `challenge`. I wanted the hook to include each memory's ID and at least its `content`, so I could use or challenge a memory directly.

2. **`challenge` had no way to propose a correction.** I knew which parts of the "Test command" memory were wrong: the Postgres marker, `docker compose up db`, and the claim that `test-fast` skips integration tests. But `challenge` only takes `evidence`. I'd have used a `proposed_content` (or `suggested_replacement`) parameter to attach a corrected draft for a human to accept. That would have avoided two bad options: leaving only a complaint, or writing a competing memory with `create` + `supersedes` before anyone had confirmed the correction. I didn't want to supersede a human-sourced memory on my own, partly because I hadn't checked whether the sandbox is reachable.

3. **`challenge.source_file` accepts only one file.** My evidence came from several places: `tests/test_payments.py`, the `Makefile`, and the absence of any compose file. I'd have wanted `source_files` as a list so the challenge records every file the evidence depends on.
```

**pq-tests rep0**

```
Two things were awkward, plus one gap that was my own fault rather than the tooling's:

1. **The prompt hook only showed the memory's title.** It injected `[convention] Test command (source: shared/human)` with no content. To see what the memory actually said, I had to make a separate `query` call (a `get` by id would have done the same). I would have wanted the hook to include the memory's content, or at least its id and a one-line summary that says something more useful than the title. Then I could have checked it against the Makefile straight away.

2. **`challenge` has no way to suggest a correction.** It takes `evidence` and `source_file`, but not a proposed fix. I would have wanted something like a `proposed_content` (or `suggested_replacement`) parameter. I'd have used it to attach the corrected convention to the challenge: the integration test is marked `integration`, it calls an external payments sandbox rather than Postgres, there's no docker-compose file, and `make test-fast` doesn't exclude that test. A human reviewer could then accept it in one step. I could have written a new memory with `create` plus `supersedes`, but that felt like overriding a human-written memory without review. So I only flagged it, and the correct information now isn't stored anywhere in the memory store.

3. **My own gap:** I could have used `create` to save a separate memory recording the hazard (that `tests/test_payments.py` makes real network calls) without replacing the challenged one. Nothing in the tools stopped me; I just didn't do it.
```

