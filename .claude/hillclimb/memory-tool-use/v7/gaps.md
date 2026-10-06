# Tool gap evidence: v7

123 graded runs. Memory tool calls by operation: `get` 33, `query` 31, `create` 18, `update` 6, `challenge` 2.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 0 | 0 |
| direct_store_access | 1 | 1 |
| unknown_tools | 0 | 0 |
| empty_queries | 17 | 17 |
| debrief asks for something | 12 | - |

## Direct reads of .engramdb/

**pq-errors rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-errors-swckiqwa/ledgerline/.engramdb/memories/api-errors-use-problem-json_01a0f827-bb7b-7ad3-b213-b1d50ef34a7a.md"
 }
}
```


## Queries that returned nothing

**nc-readme rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "README spelling"
 }
}
```

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
  "query": "payment gateway PAYMENTS_LIVE",
  "include_global": true
 }
}
```

**nk-env-var rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payment gateway PAYMENTS_LIVE",
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
  "query": "payment gateway PAYMENTS_LIVE"
 }
}
```

**nk-jobs-explicit rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "jobs.enqueue celery background jobs"
 }
}
```

**nk-jobs-explicit rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "jobs enqueue celery background jobs"
 }
}
```

**nk-jobs-explicit rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "jobs.enqueue celery background jobs"
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
  "query": "timestamps UTC timezone"
 }
}
```

**nk-timezone rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "timestamps UTC timezone"
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

**pq-errors rep0**

```
{
 "op": "query",
 "input": {
  "query": "endpoint error response",
  "mode": "rank"
 }
}
```


## Debrief answers (self-reported; verify before acting)

**ct-just rep0**

```
Nothing was truly blocked, but a few things were awkward.

- **Flagging a stale memory without editing it.** The surfaced memory "Run tests with make test-fast" was out of date, because the Makefile is being replaced by `just`. I only mentioned it to the user. I never called `challenge` or `update`. In the moment, `challenge` was the right tool, but I didn't know the replacement recipe, so I held off. A way to mark a memory as "likely stale, pending confirmation" with a note would have fit. It would flag the memory without asserting a new fact.

- **Checking a memory against the repo.** The memory said `make test` needs `docker compose up db` and that `test-fast` skips Postgres tests. I found no `postgres` marker in `tests/`. A single tool that checks a memory's claims against the repo and reports mismatches would have helped. I could only do this by hand with grep, and `verify` was in the deferred list anyway.

- **Context injected by the hook.** The hook surfaced the memory with its source ("shared/human"). It didn't say how old the memory was or when it was last verified. A last-verified date and its `invalidated_by` paths, such as the Makefile, would have shown immediately that the Makefile's removal made it suspect.

- **End-of-task reflection.** The session-start hook suggested recording durable learnings at the end. I had no way to tell whether the `just` migration was durable or only the user's passing remark, so I asked the user instead of saving it.
```

**ct-release rep0**

```
Yes, a few things were awkward.

1. **Superseding a memory in place.** The old memory said to push a `vYYYY.MM.N` tag, and the new fact replaced it. I used `update` to overwrite the content and wrote the old process into the text as "Previously: ...". I would have preferred `resolve` with `invalidate`, which the project's ENGRAM.md recommends and which keeps history. `resolve` was a deferred tool that I never loaded, so I took the lossy route. A parameter on `update` such as `supersede_with_history: true` would also have covered it. I would have used it to close the old claim's validity window and record the new one as a separate memory.

2. **Verifying the new claim.** I couldn't find a `release` target in the `Makefile`, so the updated memory is based only on what the user said. I would have wanted a `verify` call, or a `status` or confidence marker on `update`, to record "user-asserted, not yet confirmed in the repo". I also set no `premise` or `invalidated_by` on the updated memory. A suitable value for `invalidated_by` would have been `Makefile`.

3. **Hook context lacking provenance and age.** The injected memory showed `source: shared/human` but no last-updated date. That made it hard to tell whether the memory was stale or the user's statement was a real correction.

4. **A `get` after the injected memory was redundant.** The hook had already shown the full content. I called `get` anyway because ENGRAM.md says to expand surfaced memories. It would help if the hook said whether the injected text was complete or truncated.
```

**ct-rounding rep0**

```
Nothing was strictly missing, but a few things were awkward.

- **Superseding a decision.** `ENGRAM.md` says to prefer `resolve` with `invalidate` over editing when something was true and no longer is, so history stays. I used `update` and overwrote the March ROUND_HALF_EVEN text in place. `create` with `supersedes` was available, and I should have used it. The `resolve` tool was deferred and I never loaded its schema, so I didn't use it either. What I wanted was a single "supersede this decision" action that closes the old memory's validity window and creates the new one, with the old text kept. That is a gap in how I used the tools, not in the tools themselves.

- **Recording why a memory changed.** `update` has no parameter for a change reason or a source, such as "user said finance reversed this on 2026-10-01". I put that in the `content` text instead. A `reason` or `changed_by` field would have kept it separate from the memory's content.

- **Provenance and approval.** The old memory was `shared/human`. I couldn't see whether `update` would keep the human provenance or relabel it as agent-edited. I also had no confirmation step for editing a shared human-authored decision. I did it on the user's say-so and didn't ask them to confirm.

- **Stale hook injection.** The `PreToolUse` hooks kept showing the old "Do not use ROUND_HALF_UP" text on my edits. The `update` call ran in the same batch as the file edits, so the hooks fired before it landed. It wasn't a bug, but the contradiction message was noisy. A hook that knew a memory was being updated in the same batch could have suppressed it.
```

**ed-404 rep0**

```
Nothing missing.

The one memory I used was the problem+json convention. It was surfaced by the hook, and `get` returned its full content. That was enough to do the task. I didn't need to create, update, challenge or verify anything.
```

**ed-migration-typo rep0**

```
Nothing was missing. The injected hazard memory was enough for this task, and `get` returned its full content. I didn't need any other memory operation.
```

**ng-bankers rep0**

```
Nothing missing.

The hook-injected memory (the ROUND_HALF_EVEN decision) was enough to answer the question, and I didn't need to query, create, or update anything.
```

**nk-debug-lesson rep0**

```
Nothing missing.

The two calls I made did what I needed. A filter query for "wkhtmltopdf" confirmed that no memory on the topic existed, and a create call stored the hazard with its premise and tags.
```

**nk-owner rep0**

```
Mostly nothing was blocking, but there were a few awkward points:

1. **Duplicate check before create.** I called `create` without running `query` first. I wanted a way to check for an existing memory on the same topic. The `create` call could have taken a `dedupe` or `if_similar_exists` option, or returned near-duplicate candidates. The hazard memory about `finalize()` already sat on the same file, and I didn't check whether anything related to ownership existed.

2. **Linking to related memories.** I wanted a `related` or `links` parameter on `create`. It would have tied the ownership note to the `finalize()` hazard memory, since both concern `src/billing/invoices.py`. The only connection I had was the shared `physical` path.

3. **Memory type for ownership.** None of the types (decision, convention, hazard, context, intent, relationship, debug, preference) fits "who owns or reviews this" cleanly. I used `convention` with `epistemic: decision`. A dedicated `ownership` or `review-requirement` type would have been a better fit.

4. **`invalidated_by`.** I set it to the file path, which is a poor fit. The memory isn't invalidated when the file changes. It's invalidated when ownership changes, such as a CODEOWNERS edit. There was no way to express "invalidated by a person or team change" instead of a path.

5. **Using the deferred tools.** I didn't load `verify` or `task_current`, and I didn't need them. The project instructions suggest declaring a task, but this request was a one-line note, so it would have been overhead.

6. **Hooks.** The injected context did what I needed. The hazard memory surfaced because the prompt mentioned the file, which was useful.
```

**pq-errors rep0**

```
Yes, one thing was awkward.

My `mode: "rank"` query for "endpoint error response" returned zero memories, even though a relevant convention existed ("API errors use problem+json", tagged `api` and `errors`, scoped to `src/api/`). I only found it because a Grep over the repo happened to match the memory file under `.engramdb/memories/`.

Possible causes:
- The query was only tokenized against summary, content and tags. "Endpoint" and "response" don't appear in that memory, though "error" does.
- Rank mode may need a `path` or `logical` scope signal before it surfaces anything. I passed neither.

What I would have wanted:
- **`query` with a fuzzy or semantic fallback:** if keyword matching returns nothing, it would fall back to embedding or synonym matching. I would have used it to match "endpoint error response" to the "API errors" convention.
- **A diagnostic on empty results:** `query` would return something like `retrieval_quality` plus a reason, for example "no keyword match; try `path`". Right now `"retrieval_quality":"full"` with `total: 0` doesn't say whether the store is empty or the query just missed.
- **Session-start surfacing:** the hook's injected context only prompted me to reflect at the end. It surfaced no relevant memories at the start, which `ENGRAM.md` says it should.

I did not record anything at the end. The memory tools were no longer available, and the session surfaced nothing new or contradictory.
```

**pq-money rep0**

```
One item was awkward, and it wasn't strictly missing.

- **Verifying a memory:** I confirmed the "money is integer cents" memory against `src/billing/money.py`. I would have liked to record that confirmation with `verify`, which is a deferred tool. I didn't load it, so I never tried it. Because I never loaded `verify`, the memory still carries its old confidence and has no last-verified marker. A reviewer or a later session can't tell it was checked. A lighter-weight path from query results (for example a `verify` flag on `query`, or a ready-loaded `verify`) would have made that easy.

Everything else I needed was available: `query` with `mode: "filter"` and `include_global`, and the repo search. There was no field on a memory for linking it to the code symbol it describes (here, `Money`). I would have used that to flag the memory automatically if `Money` changed, though `invalidated_by` paths cover most of that.
```

**pq-release rep0**

```
Nothing missing.

The hook surfaced the one relevant memory, and `get` returned its full content. That was enough to answer the question. I didn't need to create, update, challenge or verify anything.
```

**pq-rounding rep0**

```
Mostly nothing was missing, but there was one awkward spot.

- **Verifying a memory against the code.** The hook injected the memory, and I answered from it without calling `get` or `query`. The memory said ROUND_HALF_EVEN, but my grep of the code found no implementation of it. I wanted to record that the decision was unverified in code, or to flag a possible drift. For that I would have wanted `verify` to return a "no supporting code found" result. I would also have wanted a `last_verified` or `invalidated_by` check that runs automatically. The memory had no `invalidated_by` paths, so nothing told me where to look. `verify` was a deferred tool that I never loaded. A hook line saying the memory was unverified against the code would have made it easier to find.

- **Searching memory files.** My grep matched the memory's markdown file under `.engramdb/memories/`. That was noise, but it was also how I saw the memory's provenance. A `query` parameter like `exclude_memory_files` for code searches wouldn't be a memory tool at all, so this is minor.

I didn't need to create or update any memory. The question was a read-only lookup, and the existing decision already covered it.
```

