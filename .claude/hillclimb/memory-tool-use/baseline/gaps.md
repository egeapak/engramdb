# Tool gap evidence: baseline

123 graded runs. Memory tool calls by operation: `query` 32, `create` 19, `challenge` 2, `update` 1.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 12 | 12 |
| direct_store_access | 0 | 0 |
| unknown_tools | 0 | 0 |
| empty_queries | 9 | 9 |
| debrief asks for something | 25 | - |

## Bash workarounds (engramdb CLI or .engramdb/ through the shell)

**ed-export-since rep0**

```
{
 "command": "cat .engramdb/memories/staging-export-bucket*.md README.md Makefile; echo \"EXPORT_BUCKET=$EXPORT_BUCKET\"",
 "result_head": "---\nversion: 2\nid: 01a0f6b8-aa27-7c33-9b3a-ab9a99093237\ntype: hazard\nstatus: Active\ntitle: Staging EXPORT_BUCKET points at the prod bucket\n---\n\n# Staging EXPORT_BUCKET points at the prod bucket\n\n**Summary:** Staging EXPORT_BUCKET points at the prod bucket\n\n## Content\n\nIn staging, `EXPORT_BUCKET` points at the production bucket. Never run scripts/export.py against staging without `--dry-run`.\n\n## Scope\n\n- **Files:** `scripts/export.py`\n- **Tags:** `ops`, `export`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T09:08:12.455657257+00:00\n-"
}
```

**ed-export-since rep1**

```
{
 "command": "cat .engramdb/memories/staging-export*.md .engramdb/memories/use-structlog*.md",
 "result_head": "---\nversion: 2\nid: 01a0f6c0-6812-74d0-9066-f3ed7f88dd57\ntype: hazard\nstatus: Active\ntitle: Staging EXPORT_BUCKET points at the prod bucket\n---\n\n# Staging EXPORT_BUCKET points at the prod bucket\n\n**Summary:** Staging EXPORT_BUCKET points at the prod bucket\n\n## Content\n\nIn staging, `EXPORT_BUCKET` points at the production bucket. Never run scripts/export.py against staging without `--dry-run`.\n\n## Scope\n\n- **Files:** `scripts/export.py`\n- **Tags:** `ops`, `export`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T09:16:39.826204087+00:00\n-"
}
```

**ed-export-since rep2**

```
{
 "command": "cat .engramdb/memories/staging-export-bucket-points-at-the-prod-bucket_*.md README.md Makefile",
 "result_head": "---\nversion: 2\nid: 01a0f6c8-4877-7fc0-85e9-e6f3edcac33c\ntype: hazard\nstatus: Active\ntitle: Staging EXPORT_BUCKET points at the prod bucket\n---\n\n# Staging EXPORT_BUCKET points at the prod bucket\n\n**Summary:** Staging EXPORT_BUCKET points at the prod bucket\n\n## Content\n\nIn staging, `EXPORT_BUCKET` points at the production bucket. Never run scripts/export.py against staging without `--dry-run`.\n\n## Scope\n\n- **Files:** `scripts/export.py`\n- **Tags:** `ops`, `export`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T09:25:16.023855019+00:00\n-"
}
```

**ed-flag rep0**

```
{
 "command": "cat src/flags.py flags.toml src/api/errors.py; cat .engramdb/memories/feature-flags*.md .engramdb/memories/api-errors*.md; grep -n \"errors\\|problem\" src/api/invoices.py",
 "result_head": "import tomllib\nfrom pathlib import Path\n\n_FLAGS = tomllib.loads((Path(__file__).parent.parent / \"flags.toml\").read_text())[\"flags\"]\n\n\ndef is_on(name):\n    return bool(_FLAGS.get(name, False))\n\n\ndef all_flags():\n    return dict(_FLAGS)\n[flags]\ninvoice-pdf-v2 = false\nfrom flask import jsonify\n\n\ndef problem(status, title, detail=None):\n    body = {\"type\": \"about:blank\", \"title\": title, \"status\": status}\n    if detail:\n        body[\"detail\"] = detail\n    resp = jsonify(body)\n    resp.status_code = status\n    resp.mimetype = \"application/problem+json\"\n    return resp\n---\nversion: 2\nid: 01a0f6b7-949"
}
```

**ed-flag rep2**

```
{
 "command": "cat src/flags.py flags.toml src/api/errors.py src/api/invoices.py .engramdb/memories/feature-flags*.md .engramdb/memories/api-errors*.md",
 "result_head": "import tomllib\nfrom pathlib import Path\n\n_FLAGS = tomllib.loads((Path(__file__).parent.parent / \"flags.toml\").read_text())[\"flags\"]\n\n\ndef is_on(name):\n    return bool(_FLAGS.get(name, False))\n\n\ndef all_flags():\n    return dict(_FLAGS)\n[flags]\ninvoice-pdf-v2 = false\nfrom flask import jsonify\n\n\ndef problem(status, title, detail=None):\n    body = {\"type\": \"about:blank\", \"title\": title, \"status\": status}\n    if detail:\n        body[\"detail\"] = detail\n    resp = jsonify(body)\n    resp.status_code = status\n    resp.mimetype = \"application/problem+json\"\n    return resp\nfrom flask import Blueprint, js"
}
```

**ed-migration-typo rep0**

```
{
 "command": "cat .engramdb/*/*migration*.md 2>/dev/null || find .engramdb -name \"*migration*\" -exec cat {} \\;",
 "result_head": "---\nversion: 2\nid: 01a0f6b7-e324-7433-b1b7-9203daa469cd\ntype: hazard\nstatus: Active\ntitle: Never edit an applied migration\n---\n\n# Never edit an applied migration\n\n**Summary:** Never edit an applied migration\n\n## Content\n\nFiles in migrations/ are applied in production. Never edit one; add a new file with the next 4-digit number, e.g. `0008_fix_customers.sql`.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T09:07:21.508237159+00:00\n- **Updated:** 2026-10-01T09:07:"
}
```

**ed-tax rep0**

```
{
 "command": "cat tests/test_tax.py src/billing/money.py Makefile .engramdb/memories/tax-uses* .engramdb/memories/money-is* .engramdb/memories/run-tests*",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\nfrom dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f6ad-ee0f-71f3-be7d-e3fd9f97a430\ntype: decision\nstatus: Active\ntitle: Tax uses ROUND_HALF_EVEN\n---\n\n# Tax uses"
}
```

**ed-tax rep1**

```
{
 "command": "cat tests/test_tax.py Makefile .engramdb/memories/tax-uses-round-half-even*.md .engramdb/memories/run-tests*.md",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f6c0-4ffe-7711-b4ff-04b596d4ee08\ntype: decision\nstatus: Active\ntitle: Tax uses ROUND_HALF_EVEN\n---\n\n# Tax uses ROUND_HALF_EVEN\n\n**Summary:** Tax uses ROUND_HALF_EVEN\n\n## Content\n\nTax amounts are rounded with banker's rounding (`decimal.ROUND_HALF_EVEN`), as decided with the finance team in March 2026. Do not use ROUND_HALF_UP.\n\n## Scope\n\n- **Files:** `src/billin"
}
```

**ed-tax rep2**

```
{
 "command": "cat tests/test_tax.py src/billing/money.py Makefile; cat .engramdb/memories/run-tests*",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\nfrom dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f6c7-f3e3-7360-aab6-0790c71ea479\ntype: convention\nstatus: Active\ntitle: Run tests with make test-fast\n---\n\n# R"
}
```

**pq-migration rep0**

```
{
 "command": "cat .engramdb/memories/never-edit-an-applied-migration_*.md && for f in migrations/000[67]*.sql; do echo \"== $f\"; cat $f; done; grep -n -i migrat Makefile",
 "result_head": "---\nversion: 2\nid: 01a0f6b6-bdef-7293-a456-c63405669296\ntype: hazard\nstatus: Active\ntitle: Never edit an applied migration\n---\n\n# Never edit an applied migration\n\n**Summary:** Never edit an applied migration\n\n## Content\n\nFiles in migrations/ are applied in production. Never edit one; add a new file with the next 4-digit number, e.g. `0008_fix_customers.sql`.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T09:06:06.447199081+00:00\n- **Updated:** 2026-10-01T09:06:"
}
```

**pq-migration rep1**

```
{
 "command": "cat .engramdb/memories/never-edit-an-applied-migration_*.md && for f in migrations/*.sql; do echo \"== $f\"; cat $f; done && cat Makefile && git log --stat --oneline | head -40",
 "result_head": "---\nversion: 2\nid: 01a0f6be-83ed-7141-bcba-7aa388598030\ntype: hazard\nstatus: Active\ntitle: Never edit an applied migration\n---\n\n# Never edit an applied migration\n\n**Summary:** Never edit an applied migration\n\n## Content\n\nFiles in migrations/ are applied in production. Never edit one; add a new file with the next 4-digit number, e.g. `0008_fix_customers.sql`.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T09:14:35.885285337+00:00\n- **Updated:** 2026-10-01T09:14:"
}
```

**pq-release rep2**

```
{
 "command": "cat .engramdb/memories/*.md Makefile README.md pyproject.toml; ls scripts",
 "result_head": "---\nversion: 2\nid: 01a0f6c5-a8f5-7281-b3c3-764384258259\ntype: convention\nstatus: Active\ntitle: API errors use problem+json\n---\n\n# API errors use problem+json\n\n**Summary:** API errors use problem+json\n\n## Content\n\nHTTP error responses use RFC 7807 problem+json built with `errors.problem(status, title, detail)` from src/api/errors.py. Do not return ad-hoc dicts.\n\n## Scope\n\n- **Files:** `src/api/`\n- **Tags:** `api`, `errors`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T09:22:24.117008924+00:00\n- **Updated:** 2026-10-01T09:22:24.1170089"
}
```


## Queries that returned nothing

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
  "query": "payment gateway live sandbox fake env"
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

**nk-python-min rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version minimum requires-python"
 }
}
```

**nk-python-min rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version minimum 3.10 support"
 }
}
```

**nk-python-min rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version minimum 3.10 requires-python"
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
  "query": "timezone UTC timestamps"
 }
}
```

**nk-timezone rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "timestamp timezone UTC"
 }
}
```


## Debrief answers (self-reported; verify before acting)

**ct-just rep0**

```
I didn't call any EngramDB tools or load their schemas this session, so I can't say whether a specific parameter was actually missing. These points come only from what the hook injected and what I would have done next:

1. **Memory ID in the hook's injected context.** The prompt hook showed "[convention] Run tests with make test-fast (source: shared/human)" with no ID. To flag it as possibly out of date I would have needed `challenge` or `update` on that memory, so I'd first have had to run `query` just to find its ID. With the ID in the injected line, I could have gone straight to `challenge(id=…)`.

2. **Which store the memory lives in.** "shared/human" didn't say whether it came from the project store, a `group:<name>` store or `global`. I would have needed that to pass the right `project` value to `challenge` or `update`. Otherwise I might edit the wrong store, or update a copy that other repos depend on.

3. **Created or last-verified date, and a path link, in the injected context.** The user said the Makefile was removed, but the repo still had it. A `last_verified` date and the path the memory depends on (here, `Makefile`) would have told me whether the memory or the user's claim was more likely stale. It would also have let me check the memory against the repo in one step.

4. **A "pending change" state.** What I actually wanted to record was: "the user says the project is moving from make to just, but it hasn't landed in this checkout." That is neither a contradiction I had confirmed nor a new fact. I'd have wanted something like `challenge` with a `pending` or `unconfirmed` status, or a `create` option such as `invalidated_by="justfile lands on main"` attached to the existing memory. Without the schemas I can't confirm whether `challenge` or the `premise`/`invalidated_by` fields already cover this.
```

**ct-release rep0**

```
Two things were awkward:

1. **No `provenance` parameter on `create`.** The new release-process fact came straight from the user, but I had no way to say so. It was stored and is now shown as `source: shared/agent`. The memory it replaced is shown as `source: shared/human`. So a person's own statement now looks less trustworthy than the outdated fact it replaced. I'd have used something like `provenance: "human"` or `"user-stated"` to record that the user said it, not that I inferred it.

2. **`query` results don't say which store a memory lives in.** The old memory came back as "shared", but nothing told me whether it was in this project's store, a group store, or `global`. I passed `supersedes` without knowing whether replacing a memory works across stores, or whether I should have written the new one to the same group store so other repos would see the change too. I'd have used a store field on each result (e.g. `store: "group:<name>"` or a project ID) to decide where to write the new memory and whether `supersedes` would actually take effect.
```

**ct-rounding rep0**

```
Not much was actually missing. Most of the awkwardness came from how I used the tools.

**Gaps or friction in the tooling:**

1. **The hook context didn't include memory IDs.** The UserPromptSubmit and PreToolUse hooks only gave the title ("Tax uses ROUND_HALF_EVEN"). To get the ID for `update`, I grepped the `.engramdb/memories/` filenames. I'd have wanted the injected context to include the ID, or at least the file path. The other option was a `query` round-trip, which I skipped.

2. **`update` silently renamed the memory file.** Changing `title` turned `tax-uses-round-half-even_….md` into `tax-uses-round-half-up_….md`, so git showed a delete plus an untracked file. I'd have wanted the `update` response to report any file rename, or a parameter to keep the filename stable.

3. **I couldn't tell whether provenance was preserved.** The memory was `source: human`, and I rewrote its content. Nothing in `update`'s parameters or response showed whether the change gets attributed to the agent or stays labelled as human. I'd have wanted an explicit provenance or `source` note on `update`, so a human-made decision doesn't look like it was restated by a human.

**What I should have done differently (the tools allowed it):**

- **I overwrote the old decision instead of superseding it.** The better approach was to create a new "Tax uses ROUND_HALF_UP" memory with `supersedes` pointing at the old one. That closes the old memory's validity window, and `query` with `include_invalidated` would still show the March 2026 history. I never loaded `create`'s schema to check its parameters, so the fact that I didn't use that path is on me, not a missing feature.
- **I didn't flag the contradiction before changing a human-sourced shared decision.** The user's request clearly authorized the code change. Rewriting a shared memory that finance agreed to is arguably a separate step, and I should have mentioned it before doing it, or used `challenge` and left the old record alone.
```

**ed-404 rep0**

```
Two things were awkward. Neither stopped the task, and most of what I missed was my own doing.

1. **The injected memory had no content or ID.** The UserPromptSubmit hook gave me only the title "[convention] API errors use problem+json (source: shared/human)". The body was empty and there was no memory ID. I'd have wanted the hook to include the memory's ID and its body or path anchors, such as "use `problem()` in `src/api/errors.py`". With an ID I could have called `get` on it, and with an anchor I wouldn't have needed to grep for the helper. As it was, I found it with Grep, which worked fine. `query` with a path filter on `src/api` would also have done it, but I didn't call it.

2. **Memories that mattered weren't surfaced.** The repo has a memory called "run-tests-with-make-test-fast". When my Flask smoke test failed because Flask isn't installed, that memory would have pointed me to `make test-fast` as the way to verify. The hook only matched on my prompt's wording, so it never came up. I'd have wanted either the hook to re-inject relevant memories after a tool error, or a `query` situation like `situation="verifying change / env missing deps"` that's easy to reach mid-task. To be fair, `query` already existed and I didn't call it. That one is mostly my omission, not a missing feature.

I also didn't record anything at the end. A durable fact like "Flask isn't installed in the default shell; use the project's make targets" would have been worth saving with `create`. Nothing stopped me from doing that.
```

**ed-export-since rep0**

```
One thing was awkward:

- **The UserPromptSubmit hook's injected context.** It gave only the hazard title ("Staging EXPORT_BUCKET points at the prod bucket"). The body was empty, and there was no memory ID. The actual rule ("never run scripts/export.py against staging without `--dry-run`") was only in the memory body. To get it, I read the `.engramdb/memories/...md` file directly with `cat`. I'd have wanted the hook to include each memory's ID and its content, or at least the rule it implies. Then I could have acted on it right away, or called `get` / `verify` with that ID, instead of searching the repo for the file. That matters most for hazards, where the action to take is the whole point.

Everything else I needed, I didn't try to use the tools for, so I can't report other gaps.
```

**ed-finalize-retry rep0**

```
Mostly nothing was missing. There was one awkward spot:

- **No automatic hazard surfacing when I opened the file.** The only hook context I got was the end-of-task reminder from the SessionStart hook. I found the `finalize()` non-idempotency hazard only because I chose to run `query` with `path=src/billing/invoices.py`. I would have wanted a PreToolUse/PostToolUse hook on Read/Edit that injects high-criticality `hazard` memories scoped to that path. That way the warning shows up even if the agent doesn't think to query before editing. Here it was the deciding fact for the whole task, so it shouldn't depend on the agent remembering to look.

A smaller, related point: the hazard says "use `finalize_once()`," but that doesn't fully protect against the lost-response case. I'd have used `update` to refine the memory, or `challenge` to flag the incomplete claim. Those tools exist. I held off only because I was waiting on your choice of approach, so that wasn't a gap in the tooling.
```

**ed-flag rep0**

```
The main gap was in the hook's injected context, not in the tools themselves.

1. **The UserPromptSubmit hook sent titles only.** It injected "Feature flags go through flags.is_on()" and "API errors use problem+json" with no body and no memory ID. The useful details were in the bodies: flag names are kebab-case and must be declared in `flags.toml`, and errors must use `errors.problem(status, title, detail)`. To read them, I ran `cat` on the files under `.engramdb/memories/`. I could have called `mcp__plugin_engram_memory__query` and then `get`, but without IDs that takes two calls just to expand something already shown to me. I would have wanted either:
   - a hook option to include each memory's content or a short excerpt, or
   - the memory ID in each injected line, so one `get` call could fetch it.

2. **No other tool was missing.** At the end I could have used `create` to record a durable environment fact: `make test-fast` currently fails during test collection because `requests` isn't installed and `src` isn't on the import path. I could also have used `challenge` or `update` on the "run tests with make test-fast" memory, since that command doesn't work in this environment. The tools for both existed. I chose not to use them, so that's on me, not the tooling.
```

**ed-migration-typo rep0**

```
One real friction point, plus a smaller one:

1. **Hook-injected context (UserPromptSubmit).** The injected memory showed only the title, "Never edit an applied migration." It didn't include the body or the memory ID. The body held the actionable part: the rule covers all of `migrations/` and new files take the next 4-digit number, e.g. `0008_fix_customers.sql`. To get it, I read the raw markdown file under `.engramdb/`, which bypasses the store. Without an ID I also couldn't go straight to `mcp__plugin_engram_memory__get`. I'd first have had to load and run `query` just to find it. What I wanted was for the hook to include each memory's ID plus its content, or at least a one-line summary that isn't just the title repeated. I'd have used that to apply the convention right away and to cite the memory reliably.

2. **Per-file scope in the memory (`create`/`update` scope fields).** The memory says that *every* file in `migrations/` is applied in production. I couldn't confirm that for `0007` specifically, so I had to hedge and offer to undo my change. What I wanted was a way to record or query which migrations are actually applied, such as a scope or status field per file, or a convention linking to the source of truth. I'd have used it to decide confidently between editing `0007` and adding `0008`. This is mostly a data gap rather than a tool gap, though.
```

**ed-refund-log rep0**

```
One minor thing was awkward; nothing was actually missing.

**The hook's injected context only gave titles.** The UserPromptSubmit hook gave me "Use structlog, not stdlib logging" with no body and no memory ID. I only saw the details, like `log = structlog.get_logger()` and snake_case event keys, because my grep for `structlog` happened to match the raw file in `.engramdb/memories/`. I'd have wanted the hook to include a short body excerpt plus the memory ID, or to point me to `mcp__plugin_engram_memory__get` with that ID. I would have used it to read the full convention before writing the log line, without having to search the store's files on disk. `get` or `query` would probably have worked, so it was more friction than a real gap.

Otherwise, I didn't need to create, update or challenge any memories this session.
```

**ed-tax rep0**

```
Two things were awkward. Neither stopped me from doing the work.

1. **The hook only gave me memory titles.** The UserPromptSubmit and PreToolUse hooks injected lines like "[decision] Tax uses ROUND_HALF_EVEN (source: shared/human)". There was no body and no memory ID, and the summary was just the title again. I needed the details, such as whether ROUND_HALF_EVEN applies per line or per invoice, and whether results must use the `Money` type or plain int cents. So I ran `cat` on the `.engramdb/memories/*.md` files directly instead of using the MCP tools. What I wanted was for the hook to include each memory's ID and its content, or the first few lines of it. Then I could have read it in place, or called `get` with the ID. Without the ID, getting the full text through the tools meant a separate `query` call just to find it.

2. **I couldn't easily add to an existing memory.** The "Run tests with make test-fast" memory turned out to be incomplete. `make test-fast` fails until you run `pip install -e .[dev]`, and `tests/test_payments.py` also needs `requests`. I'd have wanted something like an `update` that adds a note, or a `challenge` that says "incomplete" rather than "contradicted". That would let me add the prerequisite without rewriting the human-written memory or creating a near-duplicate. Since I didn't fetch the tool schemas, I can't say for sure that `update` or `challenge` couldn't already do this. I also didn't record this finding at the end, which was my omission, not a tool limitation.
```

**nk-api-version rep0**

```
Two things were missing or awkward:

1. **No way to set provenance on `mcp__plugin_engram_memory__create`.** The decision came straight from you (the outcome of today's design review), not from my own inference. The existing problem+json memory shows `"provenance":"human"`, but `create` has no `provenance` or `source` parameter. So the new memory comes back as `source: shared/agent`. I'd have wanted something like `provenance: "human"` or `source: "user-stated, design review 2026-10-01"`. That would tell future sessions this is an authoritative team decision and not an agent's guess. It matters most when someone weighs whether to challenge or override it.

2. **The `premise` field displays badly in hook output.** The SessionStart and UserPromptSubmit context shows the decision as "— because Holds while the 2026-10-01 design review's … stands". The hook treats `premise` as if it were the reason for the decision, but it's actually the condition the decision depends on. Nowhere could I record the actual reason; you didn't give one, but normally I would. I'd have wanted a separate `rationale` parameter on `create`, with the hook showing it as "because …" and showing `premise` as "valid while …".
```

**nk-debug-lesson rep0**

```
One thing was missing: a way to record where a memory came from.

**Tool and parameter:** `create` needs something like a `provenance` or `source` parameter, with values such as `user-reported`, `agent-verified` or `agent-inferred`.

**What I'd have used it for:** The wkhtmltopdf fact came entirely from your message. I didn't reproduce the failure or see your fix, which may not be committed. `create` gave me no way to say that. The memory is now stored and shown back as "source: shared/agent", which makes it look like I checked it myself. The best I could do was put the context in the content text and lower `confidence`, but `confidence` measures how sure I am, not where the claim came from. A future session should be able to tell "the user said this" apart from "an agent confirmed this in the repo." Then it would know whether to trust the claim or check it first, for example by looking for the flag where the repo calls wkhtmltopdf.

Everything else I needed worked: `query` with filter mode to check for an existing memory, and `create` with `premise`, `epistemic`, tags and scope.
```

**nk-env-var rep0**

```
I can't fairly say whether anything was missing, because I never used EngramDB this session. I saved the `PAYMENTS_LIVE` hazard to the file-based auto-memory instead (`payments-live-env-var.md` plus its `MEMORY.md` pointer), and I never loaded or called any `mcp__plugin_engram_memory__*` tool. So no EngramDB tool, parameter or command blocked me.

What I can report is an awkward spot I didn't address at the time. The session had two overlapping memory systems: the file-based auto-memory and EngramDB, whose SessionStart hook asked me to record durable facts there. Nothing said which one to use or whether to write to both. I picked one without checking, so the hazard isn't in EngramDB. A future session that relies on EngramDB's `query` won't find it, and I didn't tell the user that.

If I had used EngramDB, I would have wanted these:

- **`create` with a hazard type and a `path` or `tags` anchor.** I would have used it to tie the memory to `tests/test_payments.py` and to a "payments" or "env-var" tag, so that path-based queries surface it when payment code is touched. I don't know if the existing parameters fully cover this, because I never loaded the schema.
- **A way to mark a fact as unconfirmed.** For example, an `epistemic: observation` value with a note that the source of the switch hasn't been found. The user reported the behaviour, but I couldn't find the `PAYMENTS_LIVE` check in the repo, so the memory should say that and not present it as confirmed.
```

**nk-jobs-explicit rep0**

```
I never got far enough to run into a missing EngramDB feature, because I didn't use EngramDB this session. I saved your `jobs.enqueue()` rule only to the file-based auto-memory (`MEMORY.md` plus `background-jobs-via-enqueue.md`). I didn't load the EngramDB tools or call `create` or `query`, so the rule is not in EngramDB.

The one awkward part was having two memory systems side by side. Nothing told me which one is canonical or whether I should write to both. What I wanted was a single way to save the memory once and have it show up in both places, or at least a clear note saying which store wins. I'd have used that to record this rule once without having to pick a store. It may already exist in the tools I didn't look at, so this isn't a confirmed gap in EngramDB's tools.

The rule might also belong in a shared store via `project="group:<name>"` or `"global"`. I couldn't tell whether this repo has sibling repos with the same convention, and that's about context, not a missing tool.
```

**nk-owner rep0**

```
Nothing was missing. To be clear, though, I didn't use EngramDB at all in this session. I saved the Priya/`invoices.py` review-owner note only to the file-based auto-memory. That was my choice, not a gap in the tools: `mcp__plugin_engram_memory__create`, with a path scope on `src/billing/invoices.py`, would have worked. I also never ran `query`. If I had, I could have checked the injected "finalize() is not idempotent" hazard and linked to it properly. Instead, my file memory points to `[[invoices-finalize-not-idempotent]]`, a slug that only exists as a placeholder.
```

**nk-python-min rep0**

```
Nothing was truly blocked. Two small points:

1. **`mcp__plugin_engram_memory__create` has no `source` or provenance parameter.** I'd have used one to record that the "drop Python 3.10" decision came straight from the user, not from something I inferred from the code. `origin_task` comes close, but it describes the task, not who made the decision. In the end I just wrote the date and context into `content`.

2. **There was no clear rule for which memory store to use.** This session had both EngramDB and the file-based memory directory (`MEMORY.md`), and neither the hook's context nor the tool descriptions said which one should hold a project decision like this. That's not a missing parameter, but it made the choice a judgment call. A one-line note in the hook context, like "EngramDB is the primary store for project decisions," would have settled it.
```

**nk-sandbox-ratelimit rep0**

```
I can't name a missing EngramDB tool, because I never tried to use it. I didn't load or call any of its tools. I saved the rate-limit fact to the file-based memory instead (`payments-sandbox-rate-limit.md` plus the `MEMORY.md` index). I didn't query EngramDB for existing memories or create the memory there, even though the hook suggested both. So I didn't run into any limits in its tools, parameters or commands.

What did make it awkward was having two memory systems. The file-based memory and EngramDB both asked to store the same kinds of facts, and neither said which one is canonical or whether they sync. A setting or note that answered that would have told me where the rate-limit fact belonged, so I wouldn't have to choose or save it twice. That's a gap in the guidance, not a missing parameter.
```

**pq-export rep0**

```
Two things were awkward. Neither stopped the task.

1. **The hook's injected context left out the memory body and ID.** The UserPromptSubmit hook only gave the hazard's title: "Staging EXPORT_BUCKET points at the prod bucket." The actual instruction ("Never run scripts/export.py against staging without `--dry-run`") is in the body, and that wasn't included. There was no memory ID either, so I couldn't just call `get`. I ended up grepping `.engramdb/memories/` to find the file. I'd want the hook to include each memory's body (or at least the first line) and its ID, so I could follow up with `get`, `verify` or `challenge` directly.

2. **The injected context didn't say how fresh or verified a memory was.** I couldn't tell when the hazard was written or last checked, so I had to hedge in my answer ("treat the warning as true unless you've confirmed otherwise"). I'd want the hook output to include a last-verified date and a verification status for each memory. That would tell me whether to state the warning as fact or point out it might be stale.

Everything else I needed was there. For example, I could have saved a memory that `--env` doesn't choose the bucket (only `EXPORT_BUCKET` does) using `create`. I just didn't do it.
```

**pq-finalize rep0**

```
A few things were missing or awkward:

1. **The hook's injected context had only the summary.** The UserPromptSubmit hook gave me just the title ("invoices.finalize() is not idempotent"), with no content and no memory ID. I needed the actual guidance ("use `finalize_once()`, never add retries"), so I had to run a separate `query` call. Including the content, or at least the ID so I could call `get`, would have saved that round-trip.

2. **`query` with `mode: "filter"` didn't filter much.** I passed a `query` and an exact `path` (`src/billing/invoices.py`). It still returned unrelated memories, like release tagging and the test commands, with negative rerank scores. I wanted a `min_score` or relevance-cutoff parameter, so filter mode returns only real matches instead of padding up to `max_results`.

3. **`path` matching didn't affect ranking.** The hazard memory's physical scope matched my `path` exactly, but `score_breakdown.scope` came back as 0.0 and `scope_multiplier` as 1.0. Exact path matches should raise the score, or the docs should say how `path` is matched (repo-relative vs. absolute). As it is, I couldn't trust `path` to surface file-specific hazards.

4. **I had no easy way to add a finding to an existing memory.** I found new gaps in the hazard: a timeout or failed parse after the ledger accepts the post leaves `ledger_ref` unset, the check-then-post isn't atomic, and nothing saves `ledger_ref`. Those belong in the existing hazard memory. `update` would mean rewriting the human-written content. I wanted something like `update` with an `append` or `addendum` field, marked with agent provenance, so the original human text stays separate. I didn't record these findings this session.
```

**pq-flags rep0**

```
There was one real gap, plus a smaller one. To be clear up front: I didn't call any EngramDB tools this session, so this is only about what the injected context gave me.

1. **The injected memory had no body and no ID.** The hook gave me only the title, "Feature flags go through flags.is_on()". The useful details were in the body: names are kebab-case, flags must be declared in `flags.toml`, and the relevant files are `src/flags.py` and `flags.toml`. I only saw those because my grep happened to match the raw file under `.engramdb/memories/`. I would have wanted the hook to include either the body or at least the memory ID, so I could fetch it with `get(id)` without a separate `query`. That would have let me use the stored convention directly instead of finding it by accident on disk.

2. **The injected text didn't say how fresh the memory was.** It showed "source: shared/human" but no date it was last checked and no linked files. I would have wanted that shown in the hook output, or a `verified_at` field in the summary line. I would have used it to decide how much to trust the memory compared with the code. In this case the code does match the memory, except that it doesn't enforce the "must be declared" rule.

I didn't find anything missing beyond that. I didn't save the observation that `is_on()` quietly returns `False` for undeclared names. The `create` tool would have let me save it, so not saving it was my choice, not a limitation of the tools.
```

**pq-logging rep0**

```
One small thing was awkward. Nothing else was missing.

- **What the hook injected:** the UserPromptSubmit hook gave only the memory's title and source ("Use structlog, not stdlib logging", shared/human). It didn't include the memory ID or body text. I learned the useful details, like snake_case event names and the `log = structlog.get_logger()` pattern, only because my repo grep happened to match the raw file under `.engramdb/memories/`. I'd have wanted the injected context to include the memory ID, so I could call `get` with that ID directly instead of running a `query` first to find it. A short excerpt of the body would have been even better. I would have used either one to quote the full convention to you from the memory store itself, rather than from an incidental grep of the store's files.

I didn't need to create, update or challenge anything. The memory matched the code (`pyproject.toml` and `invoices.py`), and nothing new and durable came up.
```

**pq-migration rep0**

```
Nothing was missing.

I should be clear about why, though: I barely used the EngramDB tools. I loaded `mcp__plugin_engram_memory__query` but never called it. Instead I read `.engramdb/memories/never-edit-an-applied-migration_*.md` directly with `cat` in Bash. A call like `query` with `mode: "filter"` and `path: "migrations/"` would have returned the same hazard. So I can't point to a limitation of the tools, because I went around them.

I also didn't use `create` at the end to record anything new, such as the `emial` typo in `0007`. That was my choice, not something the tools stopped me from doing.
```

**pq-release rep0**

```
Mostly nothing was missing. To be clear up front: I never called any EngramDB MCP tools this session. I read the memory files directly with Grep, so I can't say for sure how the tools themselves would have behaved.

One thing was awkward, in the **hook's injected context (UserPromptSubmit)**:

- **The injected memory had only its title** ("Release by tagging vYYYY.MM.N"), with no body and no memory ID. The important part, "CI builds and publishes; never publish by hand," was only in the body. I had to grep `.engramdb/memories/` to find it. I'd have wanted the hook to include the memory's **body (or summary) and its ID**. With the ID I could have called `get` directly instead of searching the filesystem. I'd also have liked a **last-verified date** so I could judge whether the note was stale.

A smaller point: I noticed a possible conflict between the memory's example tag `v2026.09.2` (zero-padded month) and `pyproject.toml`'s `2026.9.1`. The `challenge` tool was available for flagging that, but I didn't use it, so this was my omission and not a missing feature.
```

**pq-rounding rep0**

```
One thing was awkward.

**The hook's injected context had no memory ID or body.** The UserPromptSubmit and PreToolUse hooks only gave me the title and source ("[decision] Tax uses ROUND_HALF_EVEN (source: shared/human)"). They left out the memory ID, the file path, the date and the rationale. I wanted the full text, for example "agreed with finance in March 2026, do not use ROUND_HALF_UP", so I could cite it and judge how reliable it was. Without an ID I couldn't go straight to `get`. So I grepped the raw `.engramdb/memories/*.md` files instead of using the MCP tools. Including the memory ID (or path) and a one-line body excerpt in the injected context would have let me call `get(id)`, or skip the lookup entirely.

Otherwise nothing was missing. I didn't load the schemas for `query`, `get`, `update` or `verify`, so I can't say whether they would have fallen short. For example, I might have wanted to note on the decision that `tax_for` isn't implemented yet, so the rule isn't in the code. `update` or `verify` may already cover that.
```

**pq-tests rep0**

```
Nothing was missing in the sense of something I tried and couldn't do. I never called an EngramDB tool this session, and the injected memory ("Run tests with make test-fast") was enough. I confirmed it against the `Makefile`.

One small thing was awkward, though I didn't act on it:

- **No memory ID in the hook's injected context.** The UserPromptSubmit hook showed the memory's text, type and source (`shared/human`) but not its ID. After the `Makefile` confirmed the memory, I could have recorded that with `mcp__plugin_engram_memory__verify`. That tool needs an ID, so I would first have had to run `query` just to find a memory already on screen. If the hook printed each memory's ID next to its text, I could have called `verify` (or `challenge`, if the `Makefile` had disagreed) directly. Because of that extra step I skipped it, and the memory wasn't marked as confirmed.
```

