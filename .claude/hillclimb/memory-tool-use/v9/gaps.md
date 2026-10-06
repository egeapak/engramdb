# Tool gap evidence: v9

132 graded runs. Memory tool calls by operation: `query` 104, `create` 18, `update` 6, `challenge` 3.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 0 | 0 |
| direct_store_access | 0 | 0 |
| unknown_tools | 0 | 0 |
| empty_queries | 15 | 15 |
| debrief asks for something | 17 | - |

## Queries that returned nothing

**nc-docstring rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "docstring"
 }
}
```

**nk-debug-lesson rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "wkhtmltopdf"
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
  "include_global": true
 }
}
```

**nk-debug-lesson rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "wkhtmltopdf"
 }
}
```

**nk-env-var rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payment gateway PAYMENTS_LIVE"
 }
}
```

**nk-env-var rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "payment gateway PAYMENTS_LIVE fake",
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

**nk-jobs-explicit rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "jobs enqueue celery background jobs"
 }
}
```

**nk-jobs-explicit rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "jobs.enqueue celery background jobs"
 }
}
```

**nk-jobs-explicit rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "jobs enqueue celery background jobs"
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


## Debrief answers (self-reported; verify before acting)

**ct-just rep0**

```
Nothing was missing for the task I was given.

I used `query` in filter mode, and it surfaced the stale "Test command" memory. The hook context pointed me to the same memory. I left the memory unedited on purpose, because the Makefile still exists on `main`. That was my call and not a tool limit. If I'd wanted to flag the memory, `challenge` and `update` were available.

Two small awkward points, neither of which blocked anything:
- `query` results don't show whether a memory's `physical` or `invalidated_by` scope ties to a file such as the Makefile. A memory set up that way could have been flagged automatically once the Makefile changed.
- I couldn't check whether the user's removal sat on another branch. That's a git question, not an EngramDB one.
```

**ct-loguru rep0**

```
Mostly nothing was missing, but there were a few small awkward points.

1. **Challenge didn't carry a proposed replacement.** `challenge` only flags a memory with evidence. I wanted to attach a suggested correction in the same call, such as "now loguru, since 2026-10-01". The other option was `create` with `supersedes`, but I held off on that because I hadn't confirmed the details with you. A `proposed_content` or `pending_supersede` parameter on `challenge` would have let me record the new decision as a draft awaiting your confirmation.

2. **The hook-surfaced memory showed only a title.** The prompt hook showed "Logging library choice" without its content, so I needed a separate `query` with `detail_level: "full"` to read it. A summary line in the injected context would have saved that call.

3. **No cheap way to check a memory against the code.** I wanted to ask whether the memory's claim (structlog everywhere) still held in the codebase. I used `Grep` for this, which worked. A `verify` tool exists in the deferred list, but I never loaded it, so I can't say whether it would have covered this.

4. **Tool availability was inconsistent.** At the end of the session several memory tools were reported as disconnected or denied. I couldn't have used `resolve` with `invalidate` to record "was true, no longer is" if I had wanted to. That was the tool the project instructions preferred over deleting.
```

**ct-release rep0**

```
Mostly nothing blocked me, but a few things were awkward.

1. **Editing a human-authored memory in place.** The old memory said to push a `vYYYY.MM.N` tag. I overwrote it with `update`, which loses the old text. I would have preferred `resolve` with `invalidate`, or `create` with `supersedes`, so the old process stays as history. The project instructions prefer that over overwriting. I skipped it because I'd have had to load the deferred `resolve` schema, and I didn't think to. That was my choice, not a tool limit. But `update` has no parameter to keep the previous content as a revision, and I didn't check whether it changes the memory's `provenance` from human.

2. **Recording that the claim was unverified.** I could only describe the missing `release` target in free text. I'd have wanted to use `status: needsreview`, or `verify`/`challenge`, to mark that the user's statement conflicts with the Makefile. `update` has a `status` field, so this was possible. I didn't use it.

3. **The hook's injected memory had no content.** It showed only "[convention] Release process" with an empty body. I had to call `query` with `detail_level: full` to read it. Having the content, or at least the first line, in the hook would have saved a call.

4. **Checking where the release process is defined.** No EngramDB tool helped here. I only grepped the Makefile and didn't look at CI configs, scripts, or docs.
```

**ct-rounding rep0**

```
Two things were awkward.

1. **Superseding a decision.** I overwrote the old banker's-rounding decision in place with `update` (`content`, `premise`, `invalidated_by`). I wanted to keep the old rule as history and add a new decision linked to it. The `create` tool has a `supersedes` parameter, but I didn't use it. I also didn't use `resolve` with `invalidate`, which the project's ENGRAM.md recommends for rules that were true and no longer are. As a result the history of the March 2026 rule is gone. What I wanted was one call that creates the new decision, closes the old one's validity window and links the two.

2. **Hook context.** The hooks injected only the summary line, "Tax rounding rule (source: shared/human)". That was enough to prompt a query, but the memory ID and the content were missing. I had to run a separate `query` with `detail_level: full` to see that the rule said "never ROUND_HALF_UP". It would have helped if the hook showed the content of a decision that conflicts with the task, or flagged it as a conflict. It would also have helped if I'd been told the memory's `physical` scope matched the file I was editing.

Beyond that, I didn't call `verify`, `task_current` or `task_complete`, and I didn't use the `engram:reflect` skill. That was my choice, not a gap in the tools.
```

**ed-404 rep0**

```
One minor thing made the memory store awkward.

- **Memory IDs in the hook's injected context.** The prompt hook surfaced the "API error format" convention as only a category and summary, with no memory ID. The project instructions say to `get` the full content of surfaced memories, but `get` needs an ID. I used `query` with `mode: "filter"` and `detail_level: "full"` instead, which worked but cost an extra search. If the hook output had included the ID (or the content for short memories), I could have called `get` directly.

Nothing else was missing. The query returned what I needed, and I didn't need to create, challenge or update any memories.
```

**ed-discount rep0**

```
Nothing was missing.

The hook-injected context listed only memory summaries, but I used `query` with `detail_level: "full"` to get the full content. That was enough for this task. I didn't need to create, update, or challenge any memories.
```

**ed-flag rep0**

```
Nothing was missing from the tools themselves. `query` with `mode: "filter"` and `detail_level: "full"` returned what I needed, and the `create` and `update` tools would have covered anything I wanted to save.

There was one awkward spot. The prompt-time hook listed the two relevant memories ("Feature flag usage" and "API error format") with only a type and a summary. The bodies were blank and there were no memory IDs. ENGRAM.md says to `get` the full content of surfaced memories, but without IDs I couldn't call `get` directly. I ran a keyword `query` instead, and it worked. I would have liked the hook to include each memory's ID, or its content, so I could expand it without a search.

I also didn't use `verify`, `resolve` or `task_current`, but that was my choice for a small task. They weren't unavailable.
```

**ed-flag-declare rep0**

```
Nothing was missing for the task itself. The one `query` I ran returned the feature-flag convention in full, and that was what I needed.

I did skip the reflection step the SessionStart hook suggested. Part of that was my choice: the task was small, and I didn't see anything new and durable to record. The convention I relied on was already stored, and I didn't `verify` it, which the project instructions suggest doing for memories you confirm against the code.

Two things made the flow slightly awkward, though neither blocked anything:

- **Memory content didn't arrive with the hook.** The UserPromptSubmit hook only gave me the memory's title and type, not its text, so I had to call `query` to read it. A hook option to inject the full content of high-confidence matches would have saved that call.
- **No check that a flag is declared.** The convention says an undeclared flag silently returns `False`. I would have liked a tool or `query` filter to confirm a flag is declared in `flags.toml`. I read the file directly instead, which is fine for a repo this small.
```

**nk-debug-lesson rep0**

```
Nothing missing.

The two calls I made were enough. The filter query for "wkhtmltopdf" returned no existing memories, and `create` accepted the hazard with the epistemic, premise and tags fields I wanted to set.
```

**nk-timezone rep0**

```
nothing missing.

Separately, I need to correct something from my earlier report. The memory's premise ("Customers span multiple timezones and the API layer is the single place for presentation-time conversion") was my own wording, not something you said. That is why the session-start hook now shows it as the reason for the decision. You only told me the rule itself. I also set the epistemic class to decision and the criticality to 0.7 without asking you. If the premise isn't accurate, it should be edited or removed. I can't change it now, because the memory tools are no longer available to me.
```

**pq-export rep0**

```
Nothing missing.

The one memory query I made returned the hazard note in full, and that was all I needed to answer. The hook-injected context only showed memory titles, but `query` with `detail_level: "full"` filled in the content.
```

**pq-finalize rep0**

```
One minor item, and nothing that blocked me.

- **Hook-injected memory had no ID or body.** The prompt hook surfaced only "[hazard] finalize() caveat (source: shared/human)" with an empty body and no memory ID. The project instructions say to `get` surfaced memories, but I had nothing to pass to `get`. I ran a `query` in filter mode with `detail_level: "full"` instead, which worked, but it cost an extra search. Including the memory ID, or a one-line body, in the hook output would have let me call `get` directly.

Everything else I needed was available. `query` returned the finalize hazard along with the related tax, money, and export memories. I didn't use `verify` to mark the finalize memory as confirmed against `src/billing/invoices.py`. That was my choice, not a missing capability, since `verify` was in the deferred tool list.
```

**pq-flags rep0**

```
Mostly nothing was blocked, but there were two awkward spots.

1. **The injected memory was only a title.** The prompt hook surfaced "[convention] Feature flag usage" with an empty body. I had to run a separate `query` to read the content. It would have helped if the hook had included the summary or first lines of the content, or the memory ID, so I could call `get` directly. This was a small cost.

2. **I had no clean way to record that I'd checked the memory against the code.** I confirmed the memory against `src/flags.py` and `flags.toml`. One claim, "turn it on per environment without a deploy", isn't supported by the code, because it reads a single file once at import. I wanted to do two things:
   - Mark the memory as verified against the code (the `verify` tool).
   - Attach a note to it saying the per-environment claim is unconfirmed. `challenge` or `update` could carry that note, but `challenge` implies the memory is wrong when it may only be incomplete.

   I didn't try `verify` this session. It was a deferred tool I never loaded, and the tools were later disconnected or denied. So I can't say whether it would have worked. A middle state such as "verified, with caveat X" would have fit better than choosing between `verify` and `challenge`.

I made no memory writes this session, so none of this blocked the task.
```

**pq-release rep0**

```
Nothing blocked the task, but a few things were awkward.

1. **Hook-injected memory showed only a summary.** The UserPromptSubmit context gave "[convention] Release process (source: shared/human)" with no body. I had to run a `query` with `detail_level: "full"` to get the tag format. I would have wanted the hook to include the content, or at least the first line, for short memories.

2. **`query` results had no freshness fields.** I wanted `created_at`, `updated_at`, and a last-verified timestamp, plus any `premise` or `invalidated_by` values, to judge whether the release process was still current. The output showed confidence 0.8 and a `decay` score of 0.0, but no dates. The memory had no premise or invalidation conditions, so I couldn't tell how old it was. I told you its claims rested on the note alone because I had no way to check them. A `verified_at` field in the `query` and `get` output would have helped.

3. **`query` results didn't include the memory's file path.** I ran a Glob and found `.engramdb/memories/release-process_…md` that way. A `path` field in the results would have saved that step.

4. **No tool for checking a memory against the repo.** I grepped the `Makefile` and CI config by hand to corroborate the memory. A `verify`-style tool that takes a memory ID and a list of files to check against would have done this in one call. A `verify` tool existed, but I didn't use it because it records a confirmation rather than performing one.

I didn't need to create or update anything. I didn't reflect or store a memory at the end, since nothing new and durable came up.
```

**pq-rounding rep0**

```
Nothing was missing for this task. The `query` call with `mode: "filter"` and `detail_level: "full"` returned the full tax rounding decision, including its content, tags and physical scope, so I didn't need `get`. I didn't try `verify`, which I could only have called after loading its schema through ToolSearch. I never needed any other engramdb command.

One small point: the hook-injected context showed only memory titles and types, not their content. That was fine, because the query filled in the rest.
```

**pq-test-db rep0**

```
Nothing was missing in the tools. I had what I needed, and I chose not to use some of it.

- **Hook injection:** The UserPromptSubmit hook listed two memories, "Test command" and "Release process", with blank bodies. Only the summaries came through, so the injected context didn't answer the question. I had to run `query` with `detail_level: "full"` to get the content. This was a small extra step, not a blocker. I would have preferred the hook to include the body, or at least one line of it, for memories that matched the prompt closely.
- **Possibly stale memory:** The "Test command" memory says to run `docker compose up db`, but the repo has no docker-compose file. `challenge` (with `evidence`) or `verify` would have been the right tool for flagging that. I didn't call either. I only mentioned the discrepancy in my answer. I also never confirmed that the compose file was truly absent, for example by checking the README or other locations. So the memory may still be wrong, and I left it unflagged.

Everything else I needed was available: `query` for lookup, and `challenge`, `update` and `verify` for correcting a memory.
```

**pq-tests rep0**

```
Nothing was missing from the tools. There was one small awkwardness.

- **Hook-injected context:** the prompt hook surfaced the test-command memory with only its type and summary ("Test command"), not its content. I had to run a `query` with `detail_level: "full"` to see what it said. If the hook had injected a one-line content snippet, or the first sentence of the content, I could have skipped that call.

The rest was a choice on my side, not a tool gap. The memory said to run `docker compose up db`, but my glob for `docker-compose*.y*ml` found nothing. The glob didn't cover `compose.yaml`, and I didn't search subdirectories, so I couldn't tell whether the memory was stale. I reported the doubt to the user in my answer instead of calling `challenge` or `verify`. I also didn't run the end-of-task reflection the hook suggested. Both were my decisions.
```

