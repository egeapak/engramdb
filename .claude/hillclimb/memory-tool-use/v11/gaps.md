# Tool gap evidence: v11

132 graded runs. Memory tool calls by operation: `get` 65, `query` 26, `create` 18, `update` 7, `challenge` 2.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 0 | 0 |
| direct_store_access | 0 | 0 |
| unknown_tools | 0 | 0 |
| empty_queries | 17 | 17 |
| debrief asks for something | 8 | - |

## Queries that returned nothing

**nc-readme rep0**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "README spelling"
 }
}
```

**nc-readme rep1**

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
  "include_invalidated": true
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
  "query": "PAYMENTS_LIVE payment gateway"
 }
}
```

**nk-env-var rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "PAYMENTS_LIVE payment gateway fake",
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
  "query": "PAYMENTS_LIVE payment gateway fake",
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


## Debrief answers (self-reported; verify before acting)

**ct-loguru rep0**

```
Nothing was blocking, but a few things were awkward.

1. **Challenge without a replacement.**
   - `challenge` only flags a memory with evidence. It can't record the successor decision (loguru) in the same call.
   - I would have wanted a `supersedes`-style parameter on `challenge`, or a one-step "challenge and replace".
   - Without it, the store now holds a challenged structlog decision and no loguru decision.
   - I could have used `create` with `supersedes`, or `resolve` with `invalidate`. I held off because it wasn't clear whether the loguru move was project-wide or only partly done. I never loaded `resolve` or `verify`, so I can't say how well they would have fit.

2. **Hook context lacked status.** The prompt hook surfaced the structlog memory as a plain active decision. It showed no premise, no `invalidated_by`, and no age or last-verified date. A "last verified" field or staleness hint would have told me sooner that this decision might be out of date.

3. **Searching the repo picked up memory files.** My `Grep` over the project also matched the markdown files under `.engramdb/memories/`. That is noise. A way to exclude the store from searches, or a documented ignore path, would help. This is a repo-search issue more than a memory-tool one.

4. **Turn-end reflection.** The session-start hook suggested a reflection step at the end. Doing it meant a separate `query` and `create` call, with no single "reflect" call available to me. The `engram:reflect` skill covers this, but I didn't use it.
```

**ct-release rep0**

```
Mostly nothing was missing, but there were two minor awkward spots.

1. **Provenance on update.** The old memory was marked `provenance: human`. I rewrote its content with `update`, and `update` has no `provenance` or `source` parameter. I wanted to record that the change came from the user's statement this session, not from anything I verified in the repo. I put that in the content text instead.

2. **Keeping history when replacing a memory.** I overwrote the content in place, so the old tag-and-CI process is now only described in prose. A cleaner route was `create` with `supersedes`, or `resolve` with invalidate, which keeps the old version as history. I skipped it. `resolve` was a deferred tool I hadn't loaded, and `create` with `supersedes` was available, so that gap was my choice, not a tool limit. A `mode: "replace"` or `keep_history: true` option on `update` would have made the right behavior the easy one.

3. **Checking the claim against the repo.** I couldn't confirm that `make release` exists. A grep found no Makefile, and nothing in the memory tools helps there. I'd have liked `premise` and `invalidated_by` on the updated memory (for example, invalidated by changes to the `Makefile`). Those parameters exist, but I didn't set them.
```

**ct-rounding rep0**

```
There were two minor friction points. Neither was a missing tool.

1. **Update overwrote the old decision instead of preserving it.** I used `update` to rewrite the banker's-rounding memory in place, so the original text was lost. The tools to avoid this were available: `create` with `supersedes`, or `resolve` with `invalidate`, which the project's ENGRAM.md prefers. I chose the wrong one, so that was my error. Still, `update` has no option such as `preserve_history` or `archive_previous` that would snapshot the old content when it's overwritten. A reversal of a human-authored decision is also the case where history matters most.

2. **The memory's provenance and status weren't adjusted.** The memory was marked `human` and `shared`. I couldn't tell whether my edit would keep the human label or mark the new text as agent-authored. I wanted a parameter to record "changed by agent on user's instruction, date X". I also wanted a way to flag that the change came from a chat message and hadn't been confirmed with finance. I didn't use `challenge` or `status: needsreview` for this, though I could have.

The hooks also re-injected the old banker's-rounding text on the `Edit` calls. Those hooks ran before my `update` in the same batch, so that's expected. It would have been clearer if the hook had flagged the memory as conflicting with the user's instruction.
```

**ed-flag rep0**

```
Nothing was blocked. I did hit one minor awkwardness.

The prompt hook truncated the "API error format" memory ("truncated; full text: get ..."), so I needed an extra `get` call to read the rule about `errors.problem(...)`. The `get` worked, so no capability was missing. A hook option for a larger character limit, or full text for memories whose `physical` scope matches the file being edited (here `src/api/`), would have saved that round trip.

I did not use the other memory tools: `query`, `create`, `challenge` and the rest. The hook had already surfaced the two relevant conventions. Nothing durable came up that wasn't already in the repo or the stored memories, and nothing contradicted them.
```

**nk-api-version rep0**

```
One thing was awkward; otherwise nothing was missing.

- **`invalidated_by` on `create`:** It only accepts paths or globs. The decision I saved is invalidated by a future design review changing the versioning policy, not by a file changing. I had to put `src/api/` there as a stand-in. The SessionStart hook now says "revisit if src/api/ changes", so any edit under `src/api/` would flag a decision that edits don't actually affect. I would have wanted a free-text field, something like `invalidated_when: "design review revises API versioning policy"`. I would have used it to state the real condition without tying it to unrelated file changes.

Everything else worked for this task. `query` and `create` did what I needed, and I didn't try the other tools.
```

**nk-env-var rep0**

```
Nothing blocked me, but two things were awkward:

1. **Invalidation by path only.** `invalidated_by` takes paths or globs. The real trigger for this hazard is "the gateway selection or `PAYMENTS_LIVE` handling changes", and I don't know where that code lives, since it isn't in this repo. I put `src/api/checkout.py` as a placeholder, which is a weak proxy. I would have wanted a content-based trigger, such as a pattern like `PAYMENTS_LIVE` that invalidates the memory when it shows up in a diff. A free-text condition would also have worked.

2. **No provenance field.** I wanted to record "user-reported, not verified in code". The only places to put that were the `epistemic: observation` value and a sentence in the content. A `source` field (user-stated, code-verified or inferred) would have made it queryable. I also didn't lower `confidence` to reflect that the claim was unverified, which was my omission, not a tool gap.

I also did only one narrow `query`, so I never looked at related memories such as `export-script-caution`, which is also an environment-variable hazard. That was my choice, not a limitation. A parameter to link related memories explicitly would have helped, though. I didn't need `verify`, `resolve` or `list` in this session.
```

**pq-export rep0**

```
Nothing missing.

The hook injected the hazard memory about `EXPORT_BUCKET` in staging, and that was enough to answer the question. I didn't need to call any memory tools. The injected text for the second memory (the test command) was truncated, but it wasn't relevant to this question, so I didn't need the full text.
```

**pq-test-db rep0**

```
Two things were awkward.

1. **Truncated memory text with no cheap way to check it.** The hook-injected memory about the release process was cut off mid-sentence ("v2026.09.…"). The hook said to use `get` for the full text. That wasn't relevant to this question, so I skipped it. I would have liked the hook to include a staleness or last-verified date for each memory. The test-command note named `docker compose up db`, but the repo has no compose file. I had to find that out with a grep. A `verified_at` or `last_confirmed_against_code` field in the injected context would have shown me how much to trust that note. A `verify` call with a result field would have recorded the mismatch.

2. **Nowhere to record a discrepancy found along the way.** The note says to run `docker compose up db`, but no compose file exists at the repo root. The natural step was `challenge` on that memory, or a `create` for the discrepancy. I didn't use either, and I didn't try `get` or `query`. I also couldn't tell whether the note was wrong or the compose file just lives elsewhere. A `challenge` with a `confidence` or `uncertain` flag would have fit. It would have let me say "unverified, possibly stale" rather than "contradicted".

Apart from those two, nothing else was missing.
```

