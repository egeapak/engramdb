# Tool gap evidence: v3

123 graded runs. Memory tool calls by operation: `query` 49, `create` 20, `challenge` 8, `resolve` 4, `get` 2.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 0 | 0 |
| direct_store_access | 6 | 7 |
| unknown_tools | 0 | 0 |
| empty_queries | 11 | 11 |
| debrief asks for something | 18 | - |

## Direct reads of .engramdb/

**pq-flags rep2**

```
{
 "tool": "Grep",
 "input": {
  "pattern": "checkout",
  "-i": true,
  "glob": "!.engramdb/**",
  "output_mode": "files_with_matches"
 }
}
```

**pq-migration rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-gjsyjkeo/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f79c-9a59-7381-831e-3c754688ad4b.md"
 }
}
```

**pq-migration rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-kughf86u/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f7a8-0e21-70e3-a309-183d8eef5a6a.md"
 }
}
```

**pq-release rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-release-7eov77wh/ledgerline/.engramdb/memories/release-by-tagging-vyyyy-mm-n_01a0f7a2-b30e-7b60-b7cf-e9acb81792a3.md"
 }
}
```

**pq-release rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-release-nazo761k/ledgerline/.engramdb/memories/release-by-tagging-vyyyy-mm-n_01a0f7a7-b6ed-7b43-8c28-d3863f770dc8.md"
 }
}
```

**pq-rounding rep2**

```
{
 "tool": "Grep",
 "input": {
  "pattern": "tax",
  "-i": true,
  "glob": "!.engramdb/**",
  "output_mode": "content",
  "head_limit": 30
 }
}
```

**pq-rounding rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-rounding-7kpzfbho/ledgerline/.engramdb/memories/tax-uses-round-half-even_01a0f7a7-bb8c-7472-8406-7fc37403f65d.md"
 }
}
```


## Queries that returned nothing

**nk-debug-lesson rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "wkhtmltopdf",
  "situation": "debugging"
 }
}
```

**nk-debug-lesson rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "wkhtmltopdf",
  "situation": "debugging"
 }
}
```

**nk-debug-lesson rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "wkhtmltopdf",
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
  "query": "payment gateway fake env"
 }
}
```

**nk-env-var rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payment gateway fake env",
  "situation": "debugging"
 }
}
```

**nk-env-var rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payment gateway fake PAYMENTS_LIVE",
  "situation": "debugging"
 }
}
```

**nk-jobs-explicit rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "jobs.enqueue celery background jobs",
  "max_results": 5
 }
}
```

**nk-sandbox-ratelimit rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payments sandbox rate limit",
  "max_results": 5
 }
}
```

**nk-timezone rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "timestamp UTC timezone",
  "max_results": 5
 }
}
```

**nk-timezone rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "timestamps UTC timezone",
  "max_results": 5
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


## Debrief answers (self-reported; verify before acting)

**ct-loguru rep0**

```
Nothing blocked me, but a few things were awkward.

1. **The hook context had no memory ID.** The UserPromptSubmit hook surfaced "Use structlog, not stdlib logging" with only its type and source. To challenge it I first had to run `query` in filter mode to get the ID. I would have wanted the ID, or a ready-to-use `get` or `challenge` handle, in the injected line.

2. **`challenge` can't carry a replacement.** It only flags a memory as contradicted. To record "we now use loguru" I would have needed a separate `create` with `supersedes`, or `resolve` with `invalidate` and `superseded_by`. I wanted a single step like `challenge` with a `proposed_replacement` parameter, or `resolve` with `action: "supersede"` that creates the new memory at the same time. I left it at the challenge because you hadn't confirmed that structlog was fully retired. The code still uses it in `invoices.py` and `pyproject.toml`.

3. **`query` with `detail_level: "summary"` returned empty `content`.** I used summary mode to keep the call small. I then had to open the memory file under `.engramdb/memories` through my grep results to see the full text, because `get` wasn't in the schemas I had loaded. Returning content in summary mode, or loading `get` alongside `query`, would have avoided that.

4. **The memory had no `premise` or `invalidated_by`.** If it had listed `pyproject.toml` or `src/**` as invalidating paths, it could have flagged itself as stale when the dependency changed. A tool that lists memories whose `invalidated_by` paths changed since their last `verify` would have caught this contradiction without the user telling me.

I didn't run the end-of-task reflection step. The MCP tools were also disconnected by the time of this debrief, so I couldn't write a loguru decision or any other follow-up memory.
```

**ct-release rep0**

```
Only one minor thing was awkward.

- **Memory ID missing from the hook's injected context.** The UserPromptSubmit hook surfaced the "Release by tagging vYYYY.MM.N" memory with its summary and source, but not its ID. To challenge it I first had to run `query` in filter mode with `detail_level: "full"` to get the ID. If the injected memory lines included the ID, I could have called `challenge` directly, saving a round trip.

Nothing else was missing. `challenge` did what I needed to flag the contradiction. I hadn't replaced the memory because I didn't know the details of `make release`. When I do, `create` with `supersedes`, or `resolve` with `invalidate`, covers it.
```

**ct-rounding rep0**

```
A few things were awkward, though none of them blocked the work.

1. **Memory ID missing from hook output.** The hooks surfaced "Tax uses ROUND_HALF_EVEN (source: shared/human)" with no memory ID. I had to run a separate `query` (filter mode, `detail_level: "full"`) just to get the ID I needed for `supersedes` and `resolve`. If the injected context included the ID, I could have gone straight to superseding it.

2. **Superseding took two calls.** I passed `supersedes` to `create`, then also called `resolve` with `action: "invalidate"` and `superseded_by`. I never checked whether `supersedes` already closed the old memory's validity window, so the second call may have been redundant. The tool description doesn't say. A line in the `create` description, or a flag such as `invalidate_superseded: true`, would have made it a single step. A `get` with validity status afterwards would have let me confirm the result. I didn't do that check.

3. **No task bracketing.** `ENGRAM.md` suggests `task_current` and `task_complete`. I skipped them because the change was small. I also didn't set `origin_task` or `generality` on the new decision. That is a choice I made, not a gap in the tools.

4. **Deferred tool schemas.** I had to load the schemas through `ToolSearch` before I could call anything. That is only a small extra step.

Everything else I needed was there: `query` for the old decision, `create` with `premise` and `invalidated_by` for the new one, and `resolve` to retire the old one.
```

**ed-404 rep0**

```
Mostly nothing was missing, but one thing was awkward.

- **Memory ID in the hook context:** The prompt-time hook showed "[convention] API errors use problem+json (source: shared/human)" with no memory ID. The project instructions say to `get` the full content of surfaced memories, but `get` needs an ID. I ran a `query` in filter mode for "problem+json" with `detail_level: "full"` to find the memory and read its content. That worked, but it cost an extra call and relied on guessing a search term. If the hook included the ID, or the file path the memory covers, I could have called `get` directly.

- **Schema loading:** `get` and `query` were deferred, so I had to load their schemas through ToolSearch before using them. This was a minor extra step, not a real gap.

I also didn't call `verify`, although I had confirmed in `src/api/errors.py` that `problem(status, title, detail)` matches the memory. That was my choice, not a limitation of the tools.
```

**ed-finalize-retry rep0**

```
Nothing was missing in the tool design itself. I never used the memory store this session, so I have no concrete limitation to report.

I never loaded the deferred EngramDB tool schemas or called `query` before editing. I also didn't store anything afterward, even though the hook suggested it. The task was small and nothing durable came up, apart from the idempotency-key concern, which is task-specific.

The tools have since disconnected, which didn't affect my work.
```

**ed-flag rep0**

```
Mostly nothing was missing, but the hook's injected context was awkward in one way.

- **Memory IDs and bodies in the hook output:** The UserPromptSubmit hook listed two memories, "Feature flags go through flags.is_on()" and "API errors use problem+json". It gave only the title and a source label, with an empty body and no memory ID. The project instructions say to `get` the full content of relevant memories, but `get` needs an ID. I had loaded `get` through ToolSearch, but I never called it. I read the flags memory by grepping the `.engramdb/memories/` files on disk, which worked only because the files are plaintext in the repo. I would have wanted the ID, or a short body excerpt, in the injected line. I never read the full problem+json memory. I relied on `src/api/errors.py` for that convention instead.

- **Query and create:** I didn't call `query` or `create`. The surfaced memories covered the task, and the repo was small enough to read directly, so that was my choice and not a tool limitation. I also skipped the end-of-task reflection the hook suggested. I judged nothing durable had come up. One candidate was that new flags are added to `flags.toml` and that a disabled gated endpoint returns a 404 problem+json. That is arguably task detail, and it is visible in the code and diff.

- **Tool availability:** At the time of this question the engram tools show as disconnected, and `get` shows as denied by a permission rule. I didn't hit either problem during the task, so it didn't affect my work.
```

**ed-migration-typo rep0**

```
There was one minor awkward point and nothing blocking.

- **Hook-injected memory had no ID.** The UserPromptSubmit hook surfaced "Never edit an applied migration" with only its type, summary and source, and the body was blank. ENGRAM.md says to `get` the full content of relevant memories, but without an ID I couldn't. I ran `query` with `mode: "filter"` and `detail_level: "full"` on the term "migration" instead, which worked but cost an extra call. I would have wanted the memory ID, or a short content excerpt, in the injected context so a direct `get` was possible.

- **No staleness signal on the memory.** The query result showed confidence 0.8 and `status: active`, but no last-verified date. The memory claims migrations are "applied in production", and I had no way to judge how current that was. A `last_verified` field in the results would have helped me decide how much to rely on it. I treated it as authoritative and said so in my answer.

I also didn't write anything back to the store. That was my choice, not a tool limitation: the task produced no new durable knowledge beyond what the existing memory already recorded.
```

**ed-refund-log rep0**

```
Nothing was missing, but one thing was awkward.

The hook-injected memory list showed only summaries and source (for example "[decision] Use structlog, not stdlib logging"). It gave no memory IDs. The project instructions say to `get` the full content of relevant surfaced memories, but `get` needs an ID. So I ran a `query` in filter mode with `detail_level: "full"` and a keyword to find the structlog decision and read its content. That worked, but it cost an extra search. Putting the ID in each injected line would have let me call `get` directly.

I didn't call `verify` on the structlog decision, even though I confirmed it against `invoices.py` and `pyproject.toml`. That was my choice. The tools would have allowed it. I also didn't expand the `invoices.finalize()` hazard, because it didn't touch this change.
```

**ed-tax rep0**

```
Nothing blocked me, but one thing was awkward.

- **Memory IDs in hook context:** The hook-injected memories showed only a type tag and a summary, with no memory ID. The project instructions say to `get` the full content of relevant memories, but `get` needs an ID. I had to run a `query` with `mode: "filter"` and `detail_level: "full"` to find the rounding decision's ID and full text. I would have wanted the hook output to include the ID, for example `[decision] Tax uses ROUND_HALF_EVEN (id: 01a0f79e…)`. That would let me expand a memory in one `get` call.
- **The integer-cents convention:** This surfaced in the prompt-time hook, but I never expanded it. The code I wrote matches it, since I return an `int` of cents. With the ID I would have checked its full text cheaply.
- **Duplicate hook injections:** The same rounding decision was injected on the Read and again on both Edits. This was only noise, not something I needed a new tool for.
- **Deferred tool loading:** The memory tools were deferred, so I had to load their schemas with `ToolSearch` before using them. That cost one extra step but was not a blocker.

I did not use `verify`, `create` or `challenge`. I had no reason to, because the work matched the stored decision and nothing contradicted it.
```

**nc-rename rep0**

```
Nothing blocked me, but I never called an EngramDB tool, so I can't say how the tools behave. I only saw the hook output. There was one small awkward point.

- **Memory IDs in hook context:** The hooks surfaced one memory as `[decision] Tax uses ROUND_HALF_EVEN (source: shared/human)`, with a title and source but no ID or body. To expand it with `get`, I would have needed an ID. The only way to get one would have been a `query` first. I would have liked the hook to include the memory ID, or a short excerpt of the premise. I skipped expanding it because it plainly didn't bear on a variable rename.

The same memory was also injected on the prompt, the Read and the Edit. I'd have liked a "already surfaced this session" suppression, but that's a minor preference.
```

**ng-bankers rep0**

```
Nothing was missing from the EngramDB tools.

The injected context surfaced the "Tax uses ROUND_HALF_EVEN" decision, and `get` would have let me expand it. I didn't, because the question was general and I didn't need the details. I said so in my answer. That was my choice, not a tool limitation.
```

**nk-api-version rep0**

```
Nothing essential was missing. I could query, and I could create the one memory I needed. A few things were awkward, though:

- **Tool loading:** The EngramDB tools were deferred, so I had to load `create` and `query` through ToolSearch before using them. I loaded only those two. Had I wanted `get`, `verify` or `challenge` to check a memory, that would have needed another round trip.
- **Path-based invalidation:** The decision is a policy ("/v2 for public routes, /v1 frozen"), not something tied to particular files. `invalidated_by` only takes paths or globs, so I set it to `src/api/`. That makes the memory flag "revisit if src/api/ changes", which fires on any edit there, not on a real change to the versioning policy. I would have liked a free-text or event-style invalidation trigger, such as "when a /v3 is announced or the freeze is lifted".
- **Duplicate check:** I ran one filter query on "API versioning v1 v2 routes". It returned only the unrelated problem+json convention, with a low score. I judged that to mean no existing versioning memory, but the result gave no clear "no relevant match" signal. A relevance cutoff or an explicit empty result would have made that call easier.
- **Scope:** I wasn't sure whether a decision about public API routes should go in the project store or a shared one. I used the project store, since nothing indicated related repos. A scope hint in the create response would have helped.
```

**nk-debug-lesson rep0**

```
Nothing essential was missing. I had two minor frictions.

- **Duplicate check on `create`:** I only ran a `query` in `mode: "filter"` for the single term "wkhtmltopdf". That wouldn't catch a differently worded memory about PDF generation or test setup. A `rank`-mode query would have covered more, so that gap was my choice rather than a tool limit. It would still have helped if `create` returned the nearest existing memories, or a "possible duplicate of…" warning, so the check and the write happened in one call.
- **Scoping to the code:** I left `physical` and `invalidated_by` unset because I hadn't looked up which file builds the wkhtmltopdf command line. I would have wanted to point `invalidated_by` at that file, so the memory gets flagged if the invocation changes. The parameters exist, so this was my omission. A way to suggest paths from the memory text would have been a convenience.

I didn't use `get`, `verify`, `challenge`, or the task tools. This session didn't need them.
```

**nk-env-var rep0**

```
Nothing missing.

The two tools I used, `query` and `create`, did what I needed. The `query` call was a filter search for existing memories, and `create` stored the hazard note with the `premise`, `epistemic`, `confidence` and `tags` parameters I wanted. I didn't try `get`, `verify`, `challenge` or `task_current`, because nothing in this session called for them.
```

**nk-owner rep0**

```
Yes, there was one gap, and it was my own mistake rather than a tool limitation.

I saved the note about Priya using the file-based memory directory (Write plus a MEMORY.md line). I never loaded or used the EngramDB MCP tools, even though the project instructions say to use EngramDB for persistent memory. The tool I should have used was `create`. I would have set `project` to the current project, with a note that `src/billing/invoices.py` changes need Priya's review. I'd also have wanted to link it to the existing hazard that `invoices.finalize()` is not idempotent. I could have used `query` with `mode: "filter"` first, to check for an existing memory on this file before writing a new one.

The EngramDB tools themselves didn't block anything. I didn't test whether `create` could link memories to each other or attach a file path, so I can't say whether that is possible. The tools were also deferred, so I'd have needed ToolSearch to load their schemas, which is a small extra step. They have since disconnected, so I can't correct the omission now.
```

**nk-sandbox-ratelimit rep0**

```
Nothing blocked me, but one thing was a little awkward.

- **Deferred tool schemas:** `query` and `create` were listed by name only, so I had to call `ToolSearch` with `select:` before I could use them. That added a round trip to a small task. If the two or three core tools (`query`, `create`, `get`) had their schemas loaded up front, I'd have gone straight from the edit to the memory check.

Everything else worked as needed. My duplicate check was a single `filter` query on "payments sandbox rate limit", which came back empty, and `create` accepted `premise`, `epistemic`, `physical` and `tags`. I didn't use `task_current`, `task_complete`, `get` or `verify`, because a one-line test edit didn't call for them.
```

**pq-export rep0**

```
There were two minor frictions. Neither blocked the task.

1. **No memory ID in the hook's injected context.** The hook surfaced the hazard memory as a one-line summary with no ID. The project instructions say to `get` surfaced memories, but `get` needs an ID. I loaded `query` and `get` but never called either. I read the memory by grepping `.engramdb/memories/` instead. I would have wanted the injected context to include the memory ID, or `get` to accept a title or slug. I would have used that to expand the memory directly.

2. **No way to confirm the memory against the real environment.** The hazard is a claim about staging config, which I can't see from the repo. I wanted to tell the user how recently the memory had been confirmed, and I couldn't. `verify` exists, but it only marks a memory as confirmed against code. I would have wanted a "last verified" timestamp in the injected summary, or a `verify` option that records an environment check. I would have used it to say how much to trust the memory.

I never called any EngramDB tool, so I can't say how well they work. The tools also disconnected after I loaded them, and I didn't use them before that.
```

**pq-rounding rep0**

```
Mostly nothing was missing, but I hit two small friction points.

1. **The hook-injected memory had no ID or metadata.** The prompt hook showed only "[decision] Tax uses ROUND_HALF_EVEN (source: shared/human)". `get` needs a memory ID, and the hook line didn't include one. I only found the ID because my grep matched the memory's filename in `.engramdb/memories/`. I would have wanted the ID in the surfaced line. I would also have wanted the memory's `premise`, `invalidated_by` and `verified_at` fields. With those I could have judged how fresh the decision was without a separate `get`.

2. **`verify` doesn't fit a decision whose source is outside the code.** The memory records a decision made with the finance team, and the repo has no tax code to check it against. `verify` assumes you confirmed the memory against the code, so I left it alone. A way to record "re-checked, no contradicting code found, still unconfirmed against the source" would have been useful. That could be a `note` or `status` parameter on `verify`, or a separate lightweight call.

I also never called `get` or `query`. I loaded the schemas for `get` and `verify` and then answered from the grep output, which already contained the memory text. That was my choice, not a tooling limit.
```

