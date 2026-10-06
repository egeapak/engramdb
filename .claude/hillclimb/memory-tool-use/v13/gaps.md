# Tool gap evidence: v13

123 graded runs. Memory tool calls by operation: `query` 32, `create` 22, `get` 18, `update` 6, `challenge` 2.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 0 | 0 |
| direct_store_access | 0 | 0 |
| unknown_tools | 1 | 2 |
| empty_queries | 18 | 18 |
| debrief asks for something | 7 | - |

## Tool names that do not exist

**ed-tax rep1**

```
"bash"
```

**ed-tax rep1**

```
"bash"
```


## Queries that returned nothing

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
  "query": "wkhtmltopdf"
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
  "situation": "debugging"
 }
}
```

**nk-env-var rep0**

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
  "query": "jobs enqueue celery"
 }
}
```

**nk-jobs-explicit rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "jobs enqueue celery"
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

**nk-python-min rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version"
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

**pq-money rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "currency money amount representation",
  "situation": "design_choice"
 }
}
```


## Debrief answers (self-reported; verify before acting)

**ct-loguru rep0**

```
Nothing was truly missing, but a few things were awkward.

- **Challenging a memory that was only partly stale:** `challenge` only flags a memory as challenged. It can't say "superseded by this new decision" or "invalid as of date X". The right action was probably `resolve` with `invalidate`, plus a `create` for the loguru convention with `supersedes`. `resolve` was a deferred tool and I never loaded it, so I used the blunter `challenge`. A single "challenge and propose replacement" call, or a `supersedes_with` parameter on `challenge`, would have fit better.
- **Migration still in progress:** The user said they had moved to loguru, but the code still used structlog. I wanted to record "decision changed to loguru, migration incomplete" without wrongly invalidating a convention the code still follows. That needs a status like "superseded, pending migration", or a `valid_until` parameter on `update`. As it stands, `status` only takes `active`, `needsreview` and `challenged`.
- **Stale memory injected without a warning:** The prompt-time hook surfaced the structlog decision as current guidance. It had no indication that the user's message might contradict it. A hook that flagged likely conflicts between the prompt and a surfaced memory would have helped.
- **No preview of the challenge:** I couldn't do a dry run, or look up which other memories depended on the structlog decision, before challenging it. A parameter on `query` to find memories that link to or depend on a given id would have helped.
```

**ct-release rep0**

```
Mostly nothing was missing, but two things were awkward.

- **Provenance on update:** `update` has no `provenance` or `source` parameter. I rewrote a memory marked `source: shared/human`, based on what the user told me, and the record still shows only the human origin. I wanted a parameter such as `provenance: "agent, user-stated"` or an `updated_by` field. It would have shown that an agent made the edit and where the new claim came from.
- **History on update:** `update` overwrote the old content, "tag `vYYYY.MM.N` and CI publishes", with no record of the earlier version. I could have kept history by creating a new memory and invalidating the old one with `resolve`. I didn't load `resolve`, so that was my choice, not a tool limit. I would still have liked an `update` option such as `keep_previous: true` or `supersede_in_place`. It would have let me record "was true until 2026-10-01" without making two memories.

I also changed a shared memory without confirming the new process with the user or checking the repo. A `challenge` or `verify` flow was available, but I didn't use it.
```

**ct-rounding rep0**

```
There were a few awkward spots, but none of them blocked the task.

1. **In-place edit instead of supersession.**
   - I used `update` to rewrite the human-authored "Tax uses ROUND_HALF_EVEN" decision, so the March 2026 text and its rationale are gone from the memory itself.
   - The better route was `create` with `supersedes: [old id]`, plus `resolve` with `invalidate`, which keeps history.
   - That was my choice. I never loaded `resolve`, and I didn't think to use `supersedes`.
   - I would have wanted an `update` option like `record_change_reason` or `preserve_previous_as_history`. It would have let me change a decision's content while keeping the prior version and a note on why it changed.

2. **Provenance on edited memories.**
   - The memory showed `source: shared/human`. After my edit, I couldn't tell whether it would still read as human-authored.
   - I would have wanted a `provenance` or `last_edited_by` parameter on `update`, set to agent, so readers know a human-origin decision was changed by an agent on the strength of a chat instruction.

3. **No way to flag the change for confirmation.**
   - I changed a shared decision on the user's word alone, with no finance-side confirmation.
   - I would have wanted `update` to accept `status: needsreview` together with a reason field, or `challenge` to take an "I'm superseding this, please confirm" mode. That would mark the new half-up statement as unverified until someone confirms it.

4. **Stale hook injection.**
   - The PreToolUse hooks on my Edit calls still showed the old "Do not use ROUND_HALF_UP" text. They fired before my `update` ran.
   - That is expected ordering. Still, a hook that flags "this memory conflicts with the user's current request" would have been useful at prompt time, because the conflict was visible then.
```

**nk-owner rep0**

```
Nothing was missing for the task I did. I made one `create` call with the type, summary, content, physical scope, tags, epistemic class and `invalidated_by` all supported, and it worked.

I did skip two things that I'd normally do:

- **Duplicate check.** I didn't run `query` before `create`, so I can't say whether a memory about Priya's ownership already existed. That was my omission. The tools allowed it.
- **Related hazard memory.** The hook had surfaced the `finalize()` hazard memory for `src/billing/invoices.py`. I didn't link the new memory to it, and I'm not aware of a link parameter. This is a minor point, because the shared `physical` path already ties them together.
```

**pq-export rep0**

```
Nothing missing.

The hook injected the relevant hazard memory with its full text, so I didn't need to call `query` or `get`. The memory gave me what I needed to answer. I also didn't see anything in the session that called for creating, updating or challenging a memory.
```

**pq-finalize rep0**

```
Nothing missing.

The one memory I needed was already surfaced by the hook, and the filter query returned the same one. That was enough to answer your question. I didn't try to `verify` it, because I only checked the code by reading the file. I also didn't write anything new, since nothing durable came up beyond what the memory already says.
```

**pq-rounding rep0**

```
Nothing blocked me. I never called an EngramDB tool, because the hooks had already surfaced the one relevant memory. I checked it with Grep and Read instead. There were two minor frictions.

- **Staleness information in the injected context.** The hook showed the decision's text and source (`shared/human`). It did not show `premise`, `invalidated_by`, or a last-verified date. The decision dates from March 2026 and the session date was October 2026, so I wanted to know whether anyone had re-checked it. I would have used a last-verified timestamp, or the premise and invalidation fields, in the hook preview to judge how far to trust it. I could have fetched them with `get`, but that is an extra call for something the preview could include.
- **Recording a code check against the memory.** The code check found `tax_for` is still a stub, so the code neither confirms nor contradicts the memory. `verify` could mark a memory as confirmed against code, but that doesn't fit "checked, nothing to compare yet". A "checked, inconclusive" outcome, or a note attached to the memory, would let me record that the code has no rounding yet. I'm not sure it's worth adding, since it's a small case.

I never tried `verify`, so I can't say whether it would have worked here.
```

