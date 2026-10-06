# Tool gap evidence: v6

123 graded runs. Memory tool calls by operation: `query` 46, `create` 33, `get` 9, `challenge` 5, `verify` 2.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 2 | 2 |
| direct_store_access | 5 | 6 |
| unknown_tools | 0 | 0 |
| empty_queries | 26 | 26 |
| debrief asks for something | 16 | - |

## Bash workarounds (engramdb CLI or .engramdb/ through the shell)

**ed-tax rep1**

```
{
 "command": "cat tests/test_tax.py Makefile; cat .engramdb/memories/run-tests*",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f81c-7e51-7be2-8185-db28661b9a9a\ntype: convention\nstatus: Active\ntitle: Run tests with make test-fast\n---\n\n# Run tests with make test-fast\n\n**Summary:** Run tests with make test-fast\n\n## Content\n\nRun `make test-fast` for the unit suite (skips Postgres tests). `make test` runs everything but needs `docker compose up db` first.\n\n## Scope\n\n- **Files:** `/`\n- **Tags"
}
```

**pq-migration rep2**

```
{
 "command": "cat .engramdb/memories/never-edit-an-applied-migration_*.md; echo ----; cat migrations/0007_add_customers.sql migrations/0006_index_customer.sql; echo ----; cat README.md Makefile; grep -rn -i \"migrat\\|customers\" src scripts tests pyproject.toml | head -30",
 "result_head": "---\nversion: 2\nid: 01a0f821-6eb5-73e3-9dab-246596a94648\ntype: hazard\nstatus: Active\ntitle: Never edit an applied migration\n---\n\n# Never edit an applied migration\n\n**Summary:** Never edit an applied migration\n\n## Content\n\nFiles in migrations/ are applied in production. Never edit one; add a new file with the next 4-digit number, e.g. `0008_fix_customers.sql`.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T15:42:15.733934725+00:00\n- **Updated:** 2026-10-01T15:42:"
}
```


## Direct reads of .engramdb/

**ed-finalize-retry rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-rwdojg3i/ledgerline/.engramdb/memories/invoices-finalize-is-not-idempotent_01a0f813-d299-7c60-a841-7ebd86b2cccd.md"
 }
}
```

**ed-finalize-retry rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-j546_tpq/ledgerline/.engramdb/memories/invoices-finalize-is-not-idempotent_01a0f822-46b8-71c0-9780-aec1504a598d.md"
 }
}
```

**pq-migration rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-9t1mqdq7/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f812-c35e-7282-aba5-805651e4a81e.md"
 }
}
```

**pq-migration rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-cnhzjnft/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f81b-1630-7dd1-9a9c-f4a63af8bc9a.md"
 }
}
```

**pq-money rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-wqtq90xx/ledgerline/.engramdb/memories/money-is-integer-cents-never-float_01a0f820-5e08-7353-b11a-983c774d13be.md"
 }
}
```

**pq-money rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-wqtq90xx/ledgerline/.engramdb/memories/tax-uses-round-half-even_01a0f820-8635-7b70-9d67-c6b0cbd7e621.md"
 }
}
```


## Queries that returned nothing

**ed-discount rep0**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "path": "src/billing/invoices.py",
  "query": "invoice discount money decimal",
  "situation": "file_edit"
 }
}
```

**ed-discount rep1**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "path": "src/billing/invoices.py",
  "situation": "file_edit"
 }
}
```

**ed-discount rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "path": "src/billing/invoices.py",
  "situation": "file_edit"
 }
}
```

**ed-finalize-retry rep0**

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
  "query": "finalize retry network errors idempotency",
  "situation": "file_edit"
 }
}
```

**ng-regex rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "regex date ISO 8601 validation",
  "max_results": 5
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
  "query": "PAYMENTS_LIVE payment gateway fake"
 }
}
```

**nk-jobs-explicit rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "celery background jobs enqueue"
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
  "query": "python version minimum 3.10"
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
  "mode": "filter",
  "query": "payments sandbox rate limit"
 }
}
```

**nk-sandbox-ratelimit rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "path": "tests/test_payments.py",
  "query": "payments sandbox rate limit test flakes",
  "situation": "file_edit"
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
  "query": "timestamp timezone UTC"
 }
}
```

**nk-timezone rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "timestamp UTC timezone"
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
  "query": "migration schema customers column",
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
  "query": "database migration schema customers column",
  "situation": "design_choice"
 }
}
```

**pq-money rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "currency amount money representation billing",
  "situation": "design_choice"
 }
}
```


## Debrief answers (self-reported; verify before acting)

**ct-just rep0**

```
Two minor gaps. Neither one blocked the task.

1. **`challenge` has no structured way to say where the evidence came from, and no field for a proposed fix.** Its only source field is `source_file`. Here the contradiction came from something the user said, and the repo actually still agreed with the memory. I had to cram that into the free-text `evidence` and put `Makefile` in `source_file`, which is misleading. I'd have wanted two things:
   - an `evidence_source` field (for example "user statement" or "repo file")
   - a `proposed_content` field, holding something like "run the fast tests with `just <recipe>` (wraps `pytest -m "not postgres" -q`)"

   Then whoever reviews the challenge would see the suggested replacement, not just a "this is wrong" flag.

2. **There was no "pending, valid until X" state.** The memory is still true for this checkout but probably about to become wrong. I'd have liked to mark it as pending invalidation, set to trigger when `justfile` appears or `Makefile` disappears. I could probably have come close with `update` and its `invalidated_by: ["Makefile", "justfile"]`, so this is partly my oversight rather than a missing tool. But `challenge` alone flattens "disputed by the user but still matches the repo" into a plain "challenged" status.

Otherwise the hook-injected memory, with its id and provenance, was enough to spot the conflict straight away.
```

**ct-loguru rep0**

```
Two things were awkward. Neither stopped me from finishing.

1. **`create` with `supersedes` gave no confirmation that the old memory was closed.** I passed `supersedes: ["01a0f816-…"]` to replace the old structlog decision. The response only returned `{id, created, summary}`, so I couldn't tell whether the old memory's validity window had actually been closed. It could just as easily have stayed active alongside the new one. I'd have wanted the response to list what was invalidated, something like `superseded: [{id, invalidated_at}]`. The alternative was calling `get` on the old ID afterwards to check, which is an extra round trip I didn't make.

2. **No provenance parameter on `create`.** The injected memory showed `source: shared/human`, but `create` has no field like `source` or `provenance`. I wanted to record that the new loguru memory comes from something the user told me ("user-stated, 2026-10-01"), not from something the agent saw in the code. That matters here because the code still uses structlog, so the new memory can't be checked against the repo yet. I worked around it by putting this in `premise`, which is a bit of a misuse of that field.

Everything else I needed (`query`, `get`, `create`, `challenge`) was there.
```

**ct-release rep0**

```
Three things were missing or awkward:

1. **`create` doesn't report what `supersedes` did.** I passed `supersedes: [old id]`, but the response only returned `{"id", "created": true, "summary"}`. It didn't say whether the old memory's validity window was actually closed. I wanted a field like `invalidated: [ids]`, or a warning if the supersede didn't apply, so I could confirm the old tag-push convention would stop showing up without running another `get` or `query` with `include_invalidated`.

2. **`get` doesn't say which store a memory lives in.** The hook labelled the old memory `source: shared/human`, but `get` returned no store or project field. I couldn't tell whether it was in the project store or a group/global store. That matters because I wrote the replacement to the default project store. If the original sits in a shared group store, I don't know whether superseding across stores works, or whether I should have written the new memory to that group with `project: "group:<name>"`. I'd want `get` and `query` results to include a `store` field (project ID, `group:<name>`, or `global`).

3. **`create` has no way to record provenance.** The old memory carried `provenance: "human"`. My replacement repeats something the user told me directly, but `create` has no `provenance` or `source` parameter, so it presumably gets stored as agent-authored. That makes a human-stated convention look like it was replaced by an agent's guess. I'd want something like `provenance: "user-stated"` so later readers can weigh it correctly.
```

**ct-rounding rep0**

```
Two things were missing or awkward:

1. **Confirmation of what `supersedes` did (`create` response).** I passed `supersedes: ["01a0f817-…"]` to retire the old ROUND_HALF_EVEN decision. The response only returned the new id, `created: true` and the summary. It didn't say whether the old memory's validity window was actually closed, so I never confirmed the stale decision would stop surfacing. I would have wanted the response to include something like `superseded: [{id, invalidated_at}]`, or a warning if an id didn't resolve. Without it, I'd have needed an extra `get` call (or `resolve` with `invalidate`, which was deferred behind ToolSearch) to be sure future sessions wouldn't keep injecting "Do not use ROUND_HALF_UP."

2. **A way to record where an override came from (a `source`/`provenance` parameter on `create`).** The old memory was tagged `source: shared/human`, which carries more authority than an agent note. My replacement overrides it based on the user's message, but the only way I could record that was in the prose ("Finance changed their mind…"). I'd have wanted a field like `provenance: "user instruction, 2026-10-01"` or `authority: human-relayed`. That way the new memory wouldn't rank as a lower-trust agent note than the human decision it replaces, and future readers would see that a person, not the agent, made the change.
```

**ed-finalize-retry rep0**

```
There was one real gap, plus one awkward point that was partly my own doing.

1. **`query` (`mode: "rank"`, `path: "src/billing/invoices.py"`) missed a memory scoped to that exact file.** I wanted to find any hazards or decisions for `invoices.py` before editing it. The hazard "invoices.finalize() is not idempotent" was scoped to that file, but the query returned `total: 0`. I only found it by grepping `.engramdb/memories/` directly. What I wanted was a path-scoped lookup that always returns memories whose physical scope matches the given path, whatever the ranking or index state. I didn't try `mode: "filter"` with `path`, so I can't say whether it would have worked. But the rank mode presents itself as the way to orient before editing a file, and that's where it failed.

2. **The hooks don't surface file-scoped hazards automatically.** The SessionStart hook only added a generic reminder to reflect at the end. I'd have wanted a hook that runs when a file is read or edited and injects the active hazards scoped to that file. For a "never add retries here" warning, depending on the agent to query is the weak link, and as item 1 shows, the query can miss.

3. **Not actually missing:** `doctor`, `reindex` and `verify` were available as deferred tools. I could have loaded them with ToolSearch to diagnose the empty result, rebuild the index and mark the hazard as confirmed against the code. I didn't use them, so that's on me, not a missing capability.
```

**nk-api-version rep0**

```
Two things were missing or awkward:

1. **A provenance parameter on `create`.** The `/v2` decision came straight from the user, out of their design review. But `create` has no way to say that, so the memory was saved as agent-sourced (the hook now shows "source: shared/agent"). The older problem+json memory shows `provenance: "human"`, so the store tracks this field, but `create` doesn't let me set it. I would have used something like `provenance: "human"` or `"user-stated"`. Then later sessions could weigh it as a firm decision from the team, not something an agent worked out.

2. **A rationale field separate from `premise` on `create`.** The user gave the decision but not the reason for it. `premise` is the only place to explain a decision, and the session-start hook prints it after the word "because". So my premise ("while the decision stands") shows up as the circular "because While the 2026-10-01 design review decision … stands." I would have used an optional `rationale` field, left empty here, so the hook only says "because …" when there's a real reason. `premise` would then only hold what would invalidate the decision.
```

**nk-debug-lesson rep0**

```
One thing was awkward:

- **A `provenance` (or `source`) parameter on `create`.** Query results include a `provenance` field (the existing memory showed `"provenance":"human"`), but `create` doesn't let me set it. The memory I saved came only from what you told me. I never checked how the repo calls wkhtmltopdf or whether the fix is committed. I wanted to mark it as "user-reported, not verified against code" so a later agent would know to confirm it before relying on it. As it is, I could only put that caveat in the content text, and nothing that filters or ranks memories can use it there.

Nothing else was missing. I left the memory without a file path because I didn't look up where wkhtmltopdf is called, not because the tools prevented it.
```

**nk-env-var rep0**

```
Three things were missing or awkward:

1. **A provenance parameter on `create`, something like `source: "user-reported"`.** The hazard came only from what you told me, and I couldn't confirm it in the code because `PAYMENTS_LIVE` isn't in the repo. The only way to show that was to lower `confidence` and explain it in the `content` text. The surfaced memory now shows `source: shared/agent`, which makes it look like I found it myself. I'd have used this field to mark it as user-reported and not yet confirmed in code.

2. **An `invalidated_by` option that isn't a file path.** `invalidated_by` only takes paths or globs, but this memory depends on something outside the tracked files: an environment variable and wherever the fallback actually lives. I'd have wanted a trigger like "the term `PAYMENTS_LIVE` appears in the repo," so the memory gets flagged for review once someone finds or documents the switch. That would also make my "not in this repo" note go stale on its own instead of quietly becoming wrong.

3. **A way to scope `physical` to a concept rather than a file.** I attached the memory to `src/api/checkout.py` and `tests/test_payments.py` only because they were the nearest payment files, not because the behaviour lives there. A scope for an environment variable or config key would have matched the hazard better. `logical` scopes helped a little, but path-based ranking will still tie the memory to the wrong files.
```

**nk-jobs-explicit rep0**

```
Two things were awkward:

1. **`create` has no provenance parameter.** I'd have wanted something like `source: "user"` or `stated_by: "user"`. This convention came directly from the user, but the surfaced memory is labelled `source: shared/agent`. A user mandate should carry more weight than something an agent inferred. Without that parameter, all I could do was write "stated by user" into the content text.

2. **No good way to anchor a memory to code that doesn't exist yet.** The repo has no `jobs.enqueue()` and no celery usage. So I had no real file path to give `physical` or `invalidated_by`. I'd have wanted either a `status: "planned"` value on `create`, or `invalidated_by` accepting a symbol pattern like `jobs.enqueue`. Then the memory could be checked once the wrapper appears, or flagged if it never does.

Minor: the injected context puts "because" in front of the `premise` text, which made mine read "because While the project…". That's partly my wording, but a note in the parameter description saying the premise is shown after "because" would have prevented it.
```

**nk-owner rep0**

```
1. **A provenance or source parameter on `create`.** Priya's ownership came straight from the user, but the memory is now labeled `source: shared/agent`, which makes it look like something I inferred. I would have set something like `provenance: "user-stated"` so later readers can see it came from a human and trust it accordingly.

2. **A structured link field on `create`/`update`, such as `related: [<id>]`.** I wanted to connect the new ownership convention to the existing hazard that `finalize()` is not idempotent. The only option was to paste the hazard's ID into the `content` text, which nothing can follow or query. With a real link, pulling up one memory would bring up the other, and either one could be checked or retired with the other in view.

3. **A tighter `query` filter mode, or a dedicated duplicate check such as `find_similar`.** Before creating the memory, I ran `query` with `mode: "filter"` and the terms "Priya ledger owner review" to make sure no existing memory already covered it. It returned all six memories, including unrelated ones about releases, tax rounding and logging, with nearly identical low scores. So I couldn't easily tell "no match" apart from "weak match." I wanted either a filter that only returns memories with a real keyword match, or a call that answers "does something like this already exist?" with a clear yes or no.
```

**nk-python-min rep0**

```
One thing was awkward; nothing blocked the task.

**`create` → `invalidated_by` only works at the file level.** I set `invalidated_by: ["pyproject.toml"]`, but the decision only depends on the `requires-python` line. As written, any edit to that file, such as a routine dependency bump, would probably flag the memory as possibly stale. I'd have wanted a narrower option, like a key path (`pyproject.toml#project.requires-python`) or a regex on the file's contents. I'd have used it so the memory is only flagged when the Python floor actually changes.

Everything else I needed was there. The `filter` query showed there was no existing memory to contradict, and `premise` let me state when the decision would stop holding.
```

**nk-timezone rep0**

```
One thing was missing.

- **`create` has no way to say where a memory came from.** The user told me this rule directly. But the memory now appears in the injected context as `source: shared/agent`, as if I had worked it out myself from the code. I'd have used a parameter like `source: "user"` (or `provenance: "user-stated"`) to record that it's a team rule the user gave. That would let future sessions treat it as authoritative. It would also tell them that if the code disagrees, the code is what needs fixing, not the memory. The nearest workarounds are writing "user-stated" into the content or adding a tag. Neither is a real provenance field, and the hook output wouldn't show either one as the source.
```

**pq-errors rep0**

```
One minor point of friction:

- **`mcp__plugin_engram_memory__verify` was deferred.** After checking the "API errors use problem+json" memory (`01a0f812…`) against `src/api/errors.py`, I wanted to mark it verified. But `verify` was one of the deferred tools, so I'd have needed an extra ToolSearch call to load it first. I didn't make that call, so I told you the memory matched the code without recording that check in the store. I'd have wanted `verify` loaded by default alongside `query`, `get`, `create`, `update` and `challenge`, since checking a surfaced memory against the code is a normal part of answering a question. The same applies, to a lesser degree, to `task_current` and `task_complete`, which ENGRAM.md tells agents to use routinely.

Everything else I needed was there. `query` in filter mode found the relevant memory right away. I could also have used `create` to record a hazard that `src/api/invoices.py` and `src/api/checkout.py` don't yet follow the convention. Not doing that was my choice; no tool was missing.
```

**pq-finalize rep0**

```
A few things were missing or awkward:

1. **`verify` has no `evidence` or `note` parameter.** `challenge` takes `evidence` and `source_file`, but `verify` only takes `id`. I checked the memory against `src/billing/invoices.py:21-32`, and I'd have used `evidence`/`source_file` to record what I checked, so a later reader knows the verification covered `finalize`/`finalize_once` and not, say, the persistence layer.

2. **No way to tie a verification to a code version.** `verify` stamps `verified_at` but takes no commit or file hash. I'd have used something like `verify(id, at_commit: "16858dc")` so the store can tell when the verified code has changed since, rather than relying on time alone.

3. **`verify` was a deferred tool.** `get`, `query`, `create` and `challenge` were loaded up front, but `verify` needed a `ToolSearch` round-trip first. ENGRAM.md tells agents to verify confirmed memories, so having it loaded from the start would remove that step.

4. **No way to add a caveat to an existing memory without rewriting it.** The timeout gap I found refines the existing hazard: `finalize_once()` isn't fully safe either. My choices were to replace `content` with `update`, or `create` a separate memory linked by tags. I'd have wanted something like `update(id, append_details: ...)` or a `relates_to`/`refines` link parameter on `create`. That would attach the caveat to the original without touching the human-written text.

5. **Memory files show up in code searches.** `.engramdb/memories/*.md` sits in the working tree, so my first repo-wide `Grep` returned the memory files alongside the code. This isn't a tool parameter. An `engramdb` setup option that writes a `.ignore`/`.rgignore` entry, or keeps the store outside the tree, would have avoided it.
```

**pq-migration rep0**

```
Three things got in the way, though part of the first one was my own doing:

1. **`query` with `mode: "rank"` returned nothing even though a matching memory existed.** I searched with `query: "migration schema customers column"` and `situation: "design_choice"` and got `total: 0`. But the store had a hazard called "Never edit an applied migration", scoped to `migrations/` and tagged `migrations`. I only found it by reading the file in `.engramdb/memories/` directly, which skips the tool entirely.
   - **My part:** I didn't pass `path: "migrations/"` or `tags: ["migrations"]`. I didn't retry with `mode: "filter"`. I didn't try the deferred `doctor` or `reindex` tools to check whether the index was stale.
   - **What I'd want:** rank mode falling back to plain keyword matching against summaries and tags when nothing scores. When it does return zero, I'd want a short reason, such as "index empty or stale, run reindex" or "no path/scope given". The `retrieval_quality: "full"` it returned made the empty result look trustworthy.

2. **The SessionStart hook didn't show any memories.** `ENGRAM.md` says to "expand surfaced memories" from session start, but the hook only added a reminder to reflect at the end. I'd want it to list at least the high-criticality hazards, or their titles. Then "never edit an applied migration" would have been visible before I touched `migrations/`.

3. **Smaller: `verify`, `task_current` and `task_complete` aren't loaded by default.** They have to be fetched with ToolSearch first. Having `verify` ready would have made it easy to confirm the migration hazard against the actual file layout after I'd checked it. I skipped it, partly because of the extra step.

I didn't record the `emial` typo as a memory. That was my choice, not something the tools stopped me from doing.
```

**pq-tests rep0**

```
Two things were awkward:

1. **`challenge` has no way to suggest a correction or mark only part of a memory as stale.** I would have wanted a parameter like `suggested_content` or `stale_part`. The memory had two claims. The first, that `make test-fast` skips Postgres tests, matched the Makefile. The second, that `make test` needs `docker compose up db` first, had nothing behind it in the repo. `challenge` could only flag the whole memory, so the true part now looks doubtful too. My other options were `update`, which would overwrite a note a person wrote, or `create` with `supersedes`, which would replace it with no review. I'd have used the parameter to propose the fixed wording so whoever reviews the challenge can accept it in one step.

2. **The memory the hook showed me had no date on it.** I would have wanted `created_at` and `last_verified` in the injected context, next to the `source: shared/human` label it already had. Without them, I couldn't tell whether the compose setup used to exist and was removed, or was never set up. That's why I flagged the memory as "possibly out of date" rather than wrong. Separately, if the memory had had `invalidated_by` set to `Makefile` and `docker-compose*.yml`, a tool could have caught the mismatch on its own.
```

