# Tool gap evidence: v12

123 graded runs. Memory tool calls by operation: `query` 42, `create` 35, `verify` 6, `challenge` 5, `resolve` 3, `get` 3, `update` 2.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 1 | 1 |
| direct_store_access | 5 | 6 |
| unknown_tools | 0 | 0 |
| empty_queries | 24 | 24 |
| debrief asks for something | 19 | - |

## Bash workarounds (engramdb CLI or .engramdb/ through the shell)

**pq-migration rep2**

```
{
 "command": "cat .engramdb/memories/never-edit-an-applied-migration_*.md; echo ----; for f in migrations/*.sql; do echo \"== $f\"; cat \"$f\"; done; echo ----; cat README.md Makefile; grep -rn -i \"customer\\|migrat\" src scripts tests pyproject.toml | head -30",
 "result_head": "---\nversion: 2\nid: 01a0f88c-bebf-7721-80df-3de4f942c0d4\ntype: hazard\nstatus: Active\ntitle: Never edit an applied migration\n---\n\n# Never edit an applied migration\n\n**Summary:** Never edit an applied migration\n\n## Content\n\nFiles in migrations/ are applied in production. Never edit one; add a new file with the next 4-digit number, e.g. `0008_fix_customers.sql`.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T17:39:28.575203362+00:00\n- **Updated:** 2026-10-01T17:39:"
}
```


## Direct reads of .engramdb/

**ed-finalize-retry rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-pumurwuy/ledgerline/.engramdb/memories/invoices-finalize-is-not-idempotent_01a0f882-138b-7ae2-90f1-550a0acd78bf.md"
 }
}
```

**ed-finalize-retry rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-kam2g_as/ledgerline/.engramdb/memories/invoices-finalize-is-not-idempotent_01a0f88d-707e-78b1-9062-7879e7feb935.md"
 }
}
```

**pq-errors rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-errors-6q2q8660/ledgerline/.engramdb/memories/api-errors-use-problem-json_01a0f887-ac5f-7cc1-87e8-83500f468e36.md"
 }
}
```

**pq-migration rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-1qskltud/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f881-22ad-70a3-8b9d-6cbf06e758a6.md"
 }
}
```

**pq-money rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-9_8182gn/ledgerline/.engramdb/memories/money-is-integer-cents-never-float_01a0f87f-f0c6-71e2-ba31-9d5990b39ce0.md"
 }
}
```

**pq-money rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-9_8182gn/ledgerline/.engramdb/memories/tax-uses-round-half-even_01a0f880-0b07-7f21-8f52-8cf15e2ef32b.md"
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
  "query": "finalize retry network errors invoices",
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
  "query": "python version minimum 3.10"
 }
}
```

**nk-python-min rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version minimum 3.10"
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
  "query": "timezone UTC timestamps"
 }
}
```

**pq-errors rep1**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "endpoint error response client convention"
 }
}
```

**pq-migration rep0**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "database migration schema customers column"
 }
}
```

**pq-migration rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "migration schema customers column"
 }
}
```

**pq-money rep0**

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

**ct-release rep0**

```
Three things were awkward. None of them blocked me.

1. **`create` with `supersedes`: the response didn't say what happened to the old memory.** When I created the `make release` memory with `supersedes: [01a0f885-110e…]`, the response returned only the new id. It didn't say whether the old tag-push convention had been invalidated. So I made a separate `resolve` call with `action: "invalidate"` and `superseded_by` just to be sure. That call may have been redundant. I'd want `create` to report the superseded memories' new state, e.g. `superseded: [{id, invalidated: true}]`, so I know whether a follow-up is needed.

2. **`resolve` description vs. behaviour.** The description says it resolves a "challenged or needs_review memory". The old memory was neither, so I wasn't sure `invalidate` would work without a `challenge` first. It did work. I'd want the description to say directly that `invalidate` works on any active memory, or a dedicated `invalidate(id, superseded_by, reason)` tool. That would also give me a `reason` parameter to record why, e.g. "user said the process changed on 2026-10-01".

3. **No way to set provenance or an "unverified" state on `create`.** The surfaced memory showed `source: shared/human`, but `create` has no matching `source` or provenance parameter. My new memory is something the user told me, and the repo contradicts it: there's no `release` target in the Makefile. I had to write that caveat into the content text and then `update` it after reading the Makefile. I'd want a `source` parameter (e.g. `"user-stated"`) and a status like `pending_verification` or `unverified`, tied to a check such as "Makefile has a `release` target". Then later sessions would see that it's unconfirmed without parsing prose, and `verify` could clear the flag once the target lands.
```

**ct-rounding rep0**

```
Two small things. Neither stopped me from finishing the task.

1. **No reason or evidence on `create(supersedes=…)`.** When I replaced the human-authored ROUND_HALF_EVEN decision, the old entry was simply marked invalid and pointed at the new one. I wanted a `supersede_reason` or `evidence` parameter, like the one `challenge` has. I'd have used it to record *why* the old decision was dropped: "user relayed on 2026-10-01 that Finance switched to half-up." As it stands, that reason only exists inside the new memory's content. Someone looking at the old memory's history sees that it was replaced, but not why. I could have loaded `resolve` with `invalidate` to see if it covers this, but I didn't try.

2. **No way to say who the new decision came from.** The old memory showed `provenance: human`. My replacement went in as an agent-written memory, built from something the user told me secondhand. Finance never confirmed it in the store. I wanted a `provenance` or `source` field on `create`, something like `"user-relayed, unconfirmed by finance"`. Later sessions could then tell how solid the reversal is. The closest option was lowering `confidence`, which doesn't say where the decision came from. I kept the default.

On my own handling: before replacing a human-authored "do not use HALF_UP" decision, I arguably should have checked with the user. No tool was missing there. The hooks surfaced the conflict clearly every time I read or edited the file.
```

**ed-discount rep0**

```
One thing was awkward:

1. **`query` in `mode: "rank"` with `path` returned nothing for a file that has a known memory.** I called `query(mode: "rank", path: "src/billing/invoices.py", situation: "file_edit")`. It returned `{"memories":[], "retrieval_quality":"scope_only", "total":0}`. Yet the prompt hook had just shown the `finalize()` idempotency hazard, and that memory's text names `src/billing/invoices.py`. My guess is the memory has the default physical scope `"/"`, so path proximity didn't match it. Without a query term, rank mode then found nothing. The result didn't say that, and `"scope_only"` wasn't explained.
   - **What I wanted:** either an `include_unscoped` or fallback option on rank queries so root-scoped memories still appear, or a diagnostic in the result. Something like "0 memories scoped under this path; N memories at `/` not ranked" would do.
   - **What I'd have used it for:** before editing, confirming that the hazard was the only relevant memory for this file, rather than wondering whether the empty result meant "nothing else" or "the query missed things." The hook's injected context saved me here, but I couldn't double-check it.

Everything else I wanted, the tools could do. I didn't store a memory because nothing durable beyond task details came up. I also didn't `verify` the hazard memory even though I'd read `finalize_once()`, but that was my choice, not a missing capability.
```

**ed-export-since rep0**

```
Mostly nothing was missing. The hook surfaced the staging/prod bucket hazard at the right moment, and `get` returned the full text. There were two small gaps:

1. **No timestamps or verification history in `get` output.** The result had `provenance: human` and `confidence: 0.8`, but no `created_at`, `valid_from` or last-verified date. Nothing in the repo let me check the claim either: there's no staging config, and the script just reads `EXPORT_BUCKET` with a dev default. I wanted those fields on `get` (or a `history`/`audit` option) to judge whether the hazard was still current before relying on it. Here I treated it as binding anyway, which was the safe choice. For a less clear-cut memory, knowing its age would have changed what I did.

2. **No way to record "this memory was relied on but couldn't be checked."** `verify` means confirmed against the code, and `challenge` means contradicted. Neither fits "it shaped my decision, but I had no way to confirm it." Something like a `verify` parameter with `outcome: "unverifiable"`, or an `observe`/`touch` tool, would have let me note that the hazard came up and was followed but still needs a human to confirm it against real staging config.
```

**ed-finalize-retry rep0**

```
One thing got in the way: a **query came back empty and looked fine.**

- **What happened:** I ran `query` with `mode: "rank"`, `path: "src/billing/invoices.py"`, `situation: "file_edit"`, and query text about finalize, retry and idempotency. It returned `{"memories":[],"retrieval_quality":"full","total":0}`. But there was an active hazard memory scoped to exactly that file ("invoices.finalize() is not idempotent… Never add retries around it"). I only found it because a plain `Grep` over the repo happened to match the `.engramdb/memories/*.md` file. If I'd trusted the query, I would have added the retries the memory warns against.
- **What I'd have wanted:** a way for `query` to tell me its index might be stale or incomplete. For example, `retrieval_quality` could say something other than "full" when there are memory files on disk the index hasn't picked up, or the result could include a count like `unindexed_on_disk: N`. I'd have used that to know I should run `reindex` or `doctor`, or fall back to reading the files directly, instead of treating zero results as "no relevant memories."
- **Caveats:** `reindex` and `doctor` existed as deferred tools, and I didn't load or try them. I also didn't retry with `mode: "filter"`. So I can't tell whether the cause was a stale index or a ranking or scoring problem. Either way, the gap is the same: nothing in the response flagged that anything was wrong.

Nothing else was missing for what I did. `verify` (to confirm the hazard against the code) and `create` (to record the open question about whether the ledger accepts an idempotency key) were available; I just didn't get to them before handing back.
```

**ed-flag rep0**

```
One item, and it was a minor awkwardness rather than something that blocked me:

- **A "related memories" parameter on `create`** (for example `related: [ids]`, or a `relationship` link type). The hazard I saved, saying `make test-fast` currently fails at collection, directly qualifies the existing convention memory "run-tests-with-make-test-fast". `create` only offers `supersedes`, and supersedes is the wrong relationship here: the convention is still correct, the environment is just broken. I'd have linked the two so anyone who sees the convention also sees the caveat. The only option was to mention it in the content text, and I didn't do that either.

Two shortfalls were mine, not the tooling's:
- I didn't run `query` before calling `create` to check for overlap. I could see the test-fast memory's filename in `git ls-files` and should have read it with `get` and checked it.
- I didn't call `verify` on the two injected conventions (flags.is_on and problem+json), even though I confirmed both against the code. `verify` was available through ToolSearch.
```

**ed-tax rep0**

```
Nothing blocked me, and most of what went wrong was my own doing, not a gap in the tools. Two small things:

1. **A `related` field on `create`.** The memory store already had a memory called `run-tests-with-make-test-fast`. I saw it in the file listing but never opened it with `get`, and that was my mistake. My new hazard says that same command fails in this environment. I would have wanted to link the two, so anyone reading the convention would also see the hazard. The only link `create` offers is `supersedes`, and that would wrongly retire the convention. The `relationship` memory type exists, but writing a separate memory just to connect two others is clumsy.

2. **Memories surfaced before shell commands, not just before edits.** Relevant memories appeared when I submitted the prompt and before each `Edit`, but nothing appeared before `Bash`. Running `make test-fast` was exactly the moment the test-command memory mattered. A hook that surfaces memories matching a command (for example "make test", "pytest") would have shown it to me automatically. I could still have found it myself with `query`, but I didn't.

Beyond those two, the existing tools (`query`, `get`, `challenge`, `create` with `premise` and `invalidated_by`) covered everything I needed. I just didn't run a `query` before creating the new memory.
```

**nk-api-version rep0**

```
Two things:

1. **A provenance or attribution parameter on `create`.** The `/v2` decision came from the user, reporting a human design review. But the store recorded it as `source: shared/agent`, and the earlier problem+json memory shows `provenance: "human"`. `create` has no parameter for saying that a human decided this and the agent only wrote it down. That matters for trust ranking and for anyone checking where a decision came from later. I'd have wanted something like `provenance: "human"`, or `decided_by: "design review 2026-10-01"`, kept separate from `origin_task`. I used `origin_task` for that, but it's meant for task scoping, not attribution.

2. **How the `premise` field is rendered in the hook context.** The injected context shows the premise as `— because Holds while the 2026-10-01 design review decision stands…`. The tool description suggests phrasing premises as "while we pin X", which doesn't read correctly after "because". Either the label should match how premises are phrased (for example "valid while: …"), or the `create` docs should say the text follows "because". As it is, I couldn't tell from the description how the text would be displayed.
```

**nk-debug-lesson rep0**

```
One item:

- **`create`: no `provenance` parameter.** Query results include a `provenance` field (for example `"provenance":"human"` on the existing memories), but `create` has no way to set it. This fact came from the user, not from my own check of the code, so I wanted to mark it as human-reported. I passed `provenance: "human"` anyway. The call returned `created: true` with no warning, so I can't tell whether the value was saved or silently dropped. I'd want either a supported `provenance` parameter on `create` and `update`, or an error when a call includes an unknown parameter, so I know whether the attribution took effect. Without that, I had to put "Reported by the user" in the content text as a workaround.
```

**nk-env-var rep0**

```
Three things were missing or awkward:

1. **A `source`/`provenance` parameter on `create`.** The PAYMENTS_LIVE fact came from what the user told me. I never checked it against the code, and couldn't, because the switch isn't in this repo. `epistemic: "observation"` plus a lowered `confidence` was the closest I could get. The memory now shows `source: shared/agent`, which makes it look like an agent found it. I wanted a field like `source: "user-report"` with `verified: false`, so a later reader can tell "the user said so" apart from "an agent confirmed this in the code".

2. **A way to record an open question or follow-up.** I don't know where the gateway is chosen (a dependency, deploy config, or a wrapper script). I wrote that into the memory text as free prose. Something like an `open_questions` parameter, or a `followup` memory type, would let a later session that finds the switch see the question and close it. That later session could then add the file path and turn the observation into a verified fact.

3. **`invalidated_by` that accepts a symbol or text pattern, not just paths and globs.** The memory should go stale when the handling of `PAYMENTS_LIVE` changes. Since I don't know which file reads it, I could only give a `premise` sentence. A pattern-based trigger (e.g. `invalidated_by_pattern: "PAYMENTS_LIVE"`, checked against diffs in the repo and its dependencies) would have let me state that condition precisely.
```

**nk-jobs-explicit rep0**

```
One thing was awkward:

- **`create` has no way to mark a memory as coming from the user.** The memory is now shown as `source: shared/agent`, even though the user stated the rule directly; I didn't infer it. I'd have wanted a parameter like `source: "user"` (or `provenance`/`stated_by`) on `create`. I'd have used it to show this is a binding instruction from the user, not something an agent guessed from the code, so later sessions give it the right weight and don't challenge it just because the repo doesn't match yet. As a workaround, I wrote "stated by user" into `content` and `premise`.

Nothing else was missing for this task. `query` (filter mode) and `create` with `epistemic`, `premise`, `tags` and `logical` covered everything else.
```

**nk-owner rep0**

```
Two things were missing or awkward:

1. **`create` has no `provenance` (or `source`) parameter.** The user told me directly that Priya owns the ledger integration and must review changes to `src/billing/invoices.py`. EngramDB still recorded the memory as `source: shared/agent`. The older `finalize()` hazard shows `shared/human`. I'd have used something like `provenance: "human"` or `stated_by: "user"` to mark it as a human-stated rule rather than something an agent inferred. That affects trust weighting and how readily a later agent would challenge it.

2. **`create` and `update` have no `related` (or `links`) parameter.** The only cross-reference is `supersedes`. I'd have linked the new ownership memory to the existing hazard (`01a0f883-9828-…`). Both cover the same file and ledger integration, so anyone reading one should see the other. The shared `physical` path makes them show up together for edits to that file, but there's no explicit link between them.
```

**nk-python-min rep0**

```
Two things were awkward. Neither blocked the task.

1. **`create` → `invalidated_by` only accepts file paths or globs.** I set it to `pyproject.toml`, but the decision only depends on the `requires-python` line. As it stands, any unrelated edit to that file, like a dependency bump, would mark the memory as possibly stale. I wanted a finer setting, such as a key or regex inside the file (for example `pyproject.toml#project.requires-python` or a `pattern` sub-field). Then the memory would only be flagged when the version floor actually changes.

2. **`create` has no provenance field.** This decision came straight from the user ("We decided today…"), not from anything I worked out from the code. I'd have liked a `source` or `provenance` parameter (e.g. `source: "user-stated"`, with an optional who or where) to record that as structured data. I had to put it in the free-text `premise`. A provenance field would let later sessions weigh it as an authoritative human decision. It would also let `challenge`/`review` tell decisions people stated apart from the agent's own inferences.
```

**nk-sandbox-ratelimit rep0**

```
One small gap; otherwise the tools covered what I needed.

**A provenance field on `create`, e.g. `source: "user-reported" | "verified-in-code" | "observed-in-run"`.** The 5 requests-per-second limit came from the user, and I couldn't check it. In fact the code made it look unlikely to be the whole story, since the test only makes 4 calls. Because `create` has no field for this, I put "As reported by the user on 2026-10-01" in the `premise` text. That works, but other tools can't filter on it. A structured field would let a later session query for unverified claims and confirm or `challenge` them. Lowering the `confidence` parameter, which exists but I didn't use, would have partly covered this, but a number doesn't say why the memory is uncertain.

The rest worked fine: `query` with `mode: "filter"` to check for an existing memory, and `create` with `premise`, `physical` and `tags`. Loading `create` through ToolSearch was an extra step but not a real problem.
```

**pq-finalize rep0**

```
Three things were missing or awkward:

1. **A `note` or `evidence` parameter on `verify`.** When I verified memory `01a0f881…`, I could only stamp it as verified. I'd have wanted to record what I checked it against: `src/billing/invoices.py` lines 21–32 at commit `1112725`. Then a later reader would know which version of the code the memory was confirmed against, rather than only when.

2. **Anchoring a memory to a symbol in `create`/`update`.** The hazard is scoped only by `physical: ["src/billing/invoices.py"]`. I'd have wanted something like `anchors: ["finalize", "finalize_once"]`, so that `doctor` could flag the memory for review when those functions change. As it is, it only gets flagged when anything in the file changes, or not at all.

3. **Not repeating what the hook already injected.** The hook surfaced the hazard with its full text. My `query` (mode `rank`) then returned the same single memory, so the call added nothing. An `exclude_ids` parameter on `query`, or the hook listing which ids it had already shown, would have let me search only for memories I hadn't seen yet, like related conventions on ledger posting or retries.

One smaller point: `verify` was deferred, so I needed a `ToolSearch` round trip before I could call it. The other core tools (`query`, `create`, `challenge`, `update`, `get`) were loaded up front. Since the project instructions ask agents to verify memories as part of normal use, `verify` should probably be loaded up front too.
```

**pq-migration rep0**

```
Three things got in the way:

1. **`query` (`mode: "rank"`) returned nothing, with no explanation.** I searched for "database migration schema customers column" and got `total: 0` with `retrieval_quality: "full"`. Yet a memory tagged `database`/`migrations` and scoped to `migrations/` ("Never edit an applied migration") was in the store. I wanted the empty result to say why. For example, it could have reported that the index was stale or missing and `reindex` was needed, or that rank mode leans on `path`/`logical` and my free-text query barely counted. Without that, I couldn't trust the store. I worked around it by reading `.engramdb/memories/*.md` directly with Read, which skips access tracking and anything else the tool layer does. Part of this is on me: I didn't retry with `mode: "filter"` or `path: "migrations/"`. A hint on empty results would have sent me there straight away.

2. **No memories were surfaced at session start.** ENGRAM.md says memories are "surfaced at session start, on your prompt or before an edit". In this session, the SessionStart hook only added the end-of-task reflection reminder; no memories came with it. For a prompt like "add a column to customers", I'd have wanted the prompt-time hook to surface the migrations hazard without being asked. That would have made my broken query not matter.

3. **I couldn't report a retrieval miss.** I wanted to flag that a relevant memory failed to match a relevant query, so the store's tags or summary could be improved. `challenge` doesn't fit, because it's for memories whose content is wrong, and this memory's content was correct. Something like `query` feedback, or a `verify` call that records "should have matched query X", would have covered it.
```

**pq-money rep0**

```
One real problem, and part of it was my own fault.

1. **`query` with `mode: "rank"` returned nothing.** I searched for "currency amount money representation billing" with `situation: "design_choice"`. It came back with `total: 0` and `retrieval_quality: "full"`. Yet two memories that matched were sitting in `.engramdb/memories/`: "Money is integer cents, never float" and "Tax uses ROUND_HALF_EVEN". I only found them by grepping the repo and reading the files directly, which skipped the store's own access tracking.
   - **What I would have wanted:** rank mode should fall back to keyword matching on `query` when nothing else scores. Or the response should say why it found nothing, for example "no path or scope given; rank needs `path`/`logical`" or "nothing above the score threshold". Instead, `retrieval_quality: "full"` suggested the search had been complete. It was the wrong signal.
   - **My own share:** I didn't retry with `mode: "filter"` (same query, or `tags: ["money"]`), and I didn't pass `path: "src/billing"`. Either would probably have worked. So this is partly my mistake, but the empty "full" result is what made me give up on the tool.

2. **Nothing came up automatically when I asked.** The context said memories are surfaced "on your prompt". This prompt was squarely about money representation, but neither memory appeared before I searched. I would have wanted prompt-time surfacing to catch this case, since it's the most direct lookup of a stored convention there could be.

3. **Possibly `verify` and `create`, and these were mine to do.** I confirmed the integer-cents memory against the code, so it was a candidate for `verify`. I also found that the `Money` type isn't actually used anywhere outside `money.py`, which was worth recording with `create`, or flagging on the convention with `update`. Both tools were available, so nothing was missing; I just didn't use them.
```

**pq-rounding rep0**

```
One thing was awkward:

- **Recording that a decision isn't in the code yet.** The `ROUND_HALF_EVEN` memory matched what the code does, but only because the code does nothing: `tax_for` raises `NotImplementedError` and nothing in the repo uses `ROUND_*` or `quantize`. Neither `verify` nor `challenge` fits that. I'd have wanted a status option on `verify` such as `verify(id, status: "unimplemented", evidence: "src/billing/tax.py:11 raises NotImplementedError")`. That would let later sessions see the decision hasn't been checked against real code. I could have worked around it with `update` and a `tags_add` tag like "unenforced", or a separate `create` hazard memory, but neither says it as clearly.

Apart from that, nothing was missing. The prompt hook brought up the right memory with its id, and `query`, `get` and `create` covered everything else I might have needed. I didn't actually call any EngramDB tools this session; the memory the hook injected, plus checking the code, was enough to answer.
```

**pq-tests rep0**

```
Two things were awkward, both around challenging the testing memory:

1. **`challenge` only flags the whole memory, with no way to suggest a fix.** The memory was half right: the `make test-fast` and `make test` targets matched the Makefile, but the `docker compose up db` step had nothing behind it. `challenge` marks the entire memory as challenged, which could lower the rank of the correct part for later sessions. I'd have wanted something like a `proposed_content` parameter on `challenge`, or a `scope`/`claim` field to say which part is disputed. I'd have used it to attach the corrected text ("both targets run without a DB today; no compose file in repo") as a pending suggestion for the human author to accept. I didn't use `update` to rewrite it myself because the memory was human-written, and quietly overwriting it seemed wrong.

2. **`challenge.source_file` takes only one path.** My evidence came from several places: the Makefile, the missing compose file, the unmarked tests in `tests/`, and no marker registered in `pyproject.toml`. I'd have wanted `source_files` to accept a list, plus a way to record "file absent" as evidence. That would make the challenge checkable later, instead of leaving it as free text in `evidence`.

Nothing else was missing. The hook surfaced the relevant memory with its id, so I didn't need to run a `query`.
```

