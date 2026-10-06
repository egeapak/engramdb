# Tool gap evidence: v4

123 graded runs. Memory tool calls by operation: `query` 89, `create` 34, `resolve` 2, `verify` 1, `challenge` 1.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 5 | 5 |
| direct_store_access | 6 | 8 |
| unknown_tools | 0 | 0 |
| empty_queries | 28 | 28 |
| debrief asks for something | 25 | - |

## Bash workarounds (engramdb CLI or .engramdb/ through the shell)

**ed-flag rep1**

```
{
 "command": "cat src/flags.py flags.toml src/api/errors.py src/api/invoices.py Makefile; cat .engramdb/memories/api-errors*",
 "result_head": "import tomllib\nfrom pathlib import Path\n\n_FLAGS = tomllib.loads((Path(__file__).parent.parent / \"flags.toml\").read_text())[\"flags\"]\n\n\ndef is_on(name):\n    return bool(_FLAGS.get(name, False))\n\n\ndef all_flags():\n    return dict(_FLAGS)\n[flags]\ninvoice-pdf-v2 = false\nfrom flask import jsonify\n\n\ndef problem(status, title, detail=None):\n    body = {\"type\": \"about:blank\", \"title\": title, \"status\": status}\n    if detail:\n        body[\"detail\"] = detail\n    resp = jsonify(body)\n    resp.status_code = status\n    resp.mimetype = \"application/problem+json\"\n    return resp\nfrom flask import Blueprint, js"
}
```

**ed-flag rep2**

```
{
 "command": "cat flags.toml src/flags.py src/api/errors.py src/api/invoices.py; cat .engramdb/memories/api-errors*; cat Makefile",
 "result_head": "[flags]\ninvoice-pdf-v2 = false\nimport tomllib\nfrom pathlib import Path\n\n_FLAGS = tomllib.loads((Path(__file__).parent.parent / \"flags.toml\").read_text())[\"flags\"]\n\n\ndef is_on(name):\n    return bool(_FLAGS.get(name, False))\n\n\ndef all_flags():\n    return dict(_FLAGS)\nfrom flask import jsonify\n\n\ndef problem(status, title, detail=None):\n    body = {\"type\": \"about:blank\", \"title\": title, \"status\": status}\n    if detail:\n        body[\"detail\"] = detail\n    resp = jsonify(body)\n    resp.status_code = status\n    resp.mimetype = \"application/problem+json\"\n    return resp\nfrom flask import Blueprint, js"
}
```

**ed-migration-typo rep1**

```
{
 "command": "cat .engramdb/*/*mpotent* 2>/dev/null || find .engramdb -name '*mpotent*' -exec cat {} \\;",
 "result_head": "---\nversion: 2\nid: 01a0f7bf-2b5b-70f2-ad8d-06b26c3bbf48\ntype: hazard\nstatus: Active\ntitle: invoices.finalize() is not idempotent\n---\n\n# invoices.finalize() is not idempotent\n\n**Summary:** invoices.finalize() is not idempotent\n\n## Content\n\n`finalize()` in src/billing/invoices.py posts to the external ledger with no idempotency key. Calling it twice double-posts. Never add retries around it; use `finalize_once()`, which checks `invoice.ledger_ref` first.\n\n## Scope\n\n- **Files:** `src/billing/invoices.py`\n- **Tags:** `billing`, `ledger`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n"
}
```

**pq-migration rep1**

```
{
 "command": "cat .engramdb/memories/never-edit-an-applied-migration_*.md; echo ----; for f in migrations/*; do echo \"== $f\"; cat \"$f\"; done; echo ----; cat README.md Makefile; grep -rn -i \"customer\\|migrat\" src scripts | head -30",
 "result_head": "---\nversion: 2\nid: 01a0f7be-8656-7cb3-b55a-8374fdb7a2bb\ntype: hazard\nstatus: Active\ntitle: Never edit an applied migration\n---\n\n# Never edit an applied migration\n\n**Summary:** Never edit an applied migration\n\n## Content\n\nFiles in migrations/ are applied in production. Never edit one; add a new file with the next 4-digit number, e.g. `0008_fix_customers.sql`.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T13:54:13.718122955+00:00\n- **Updated:** 2026-10-01T13:54:"
}
```

**pq-migration rep2**

```
{
 "command": "cat .engramdb/memories/never-edit-an-applied-migration_*.md; echo ----; for f in migrations/*.sql; do echo \"== $f\"; cat \"$f\"; done; echo ----; cat README.md Makefile; git log --stat --oneline | head -40",
 "result_head": "---\nversion: 2\nid: 01a0f7c4-af1b-7df1-a1c1-6454ec60c159\ntype: hazard\nstatus: Active\ntitle: Never edit an applied migration\n---\n\n# Never edit an applied migration\n\n**Summary:** Never edit an applied migration\n\n## Content\n\nFiles in migrations/ are applied in production. Never edit one; add a new file with the next 4-digit number, e.g. `0008_fix_customers.sql`.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T14:00:57.371256362+00:00\n- **Updated:** 2026-10-01T14:00:"
}
```


## Direct reads of .engramdb/

**ed-finalize-retry rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-67cyh3u0/ledgerline/.engramdb/memories/invoices-finalize-is-not-idempotent_01a0f7b8-46f2-7c42-b935-14e2b9fcee3a.md"
 }
}
```

**ed-finalize-retry rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-_5zka5v5/ledgerline/.engramdb/memories/invoices-finalize-is-not-idempotent_01a0f7bf-75fd-7dd1-845d-66d2a0c9dce4.md"
 }
}
```

**pq-errors rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-errors-px4skaxt/ledgerline/.engramdb/memories/api-errors-use-problem-json_01a0f7c4-ab45-7ba3-b6db-b6288865d3fd.md"
 }
}
```

**pq-migration rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-7t_ke3kg/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f7b7-2f57-7182-a3d9-24a43e1e9cac.md"
 }
}
```

**pq-money rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-2yp21y8j/ledgerline/.engramdb/memories/money-is-integer-cents-never-float_01a0f7bd-74b4-7261-b24b-f8cb872af245.md"
 }
}
```

**pq-money rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-2yp21y8j/ledgerline/.engramdb/memories/tax-uses-round-half-even_01a0f7bd-9c7b-7682-8762-a8060b684ec4.md"
 }
}
```

**pq-money rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-57rsk9sr/ledgerline/.engramdb/memories/money-is-integer-cents-never-float_01a0f7c3-a669-7fd1-9d7c-a87f10bf0c20.md"
 }
}
```

**pq-money rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-57rsk9sr/ledgerline/.engramdb/memories/tax-uses-round-half-even_01a0f7c3-c903-7131-8601-358f244b23c9.md"
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
  "query": "invoice discount",
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
  "situation": "file_edit",
  "detail_level": "full"
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
  "situation": "file_edit",
  "detail_level": "full"
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

**nc-docstring rep0**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "path": "src/flags.py",
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
  "query": "timestamp UTC timezone",
  "include_invalidated": true
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

**pq-errors rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "endpoint error response client"
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
  "query": "migration schema customers column",
  "situation": "design_choice"
 }
}
```

**pq-money rep1**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "currency amount money representation billing",
  "situation": "design_choice",
  "detail_level": "full"
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

**ct-loguru rep0**

```
1. **`create` has no `provenance` or `source` parameter.** Query results showed the old structlog memory as `provenance: "human"`. My new memory rests only on the user saying so in chat, and the repo still contradicts it: `pyproject.toml` and `invoices.py` still use structlog. I wanted to mark it as "user-stated, not yet confirmed in the code," as opposed to something checked against the repo. All I could do was write that into the content text.

2. **`create` has no `status` parameter, and nothing marks a decision as "in transition."** The true state was "decided: loguru, migration incomplete." I wanted to create the memory as provisional (the way `update`'s `status: needsreview` works), or to give it a condition that clears automatically, like "once structlog is removed from `pyproject.toml`." `invalidated_by` only handles the opposite case, where a file change makes a memory stale. It can't promote one. So I put the incomplete-migration caveat in the free text.

3. **The `create` response didn't confirm what `supersedes` did.** It returned only `{id, created, summary}`. I couldn't tell whether the old structlog decision had been invalidated or closed, or was still active and would keep showing up in the prompt hook next to the new one. I'd have wanted the response to include something like `superseded: [{id, new_status}]`. Without it, I'd need a follow-up `get` or `query` with `include_invalidated`, or the deferred `resolve` tool, to be sure.

4. **The UserPromptSubmit hook gave no ID or provenance for the memory it surfaced.** It showed only the summary line. I had to run a separate `query` to get the ID I needed for `supersedes`. Including the memory ID in the injected context would have saved that step.
```

**ct-release rep0**

```
Two things were missing or awkward:

1. **A provenance or source parameter on `create`.** The memory I replaced has `provenance: "human"`, and its source is shown as "shared/human". The new fact came straight from the user, but `create` has no way to say so. My only option was to write "Stated by the user on 2026-10-01" in the content text. I'd have wanted something like `provenance: "human"` or `source: "user-stated"`. That would record that the new release process came from the user, not from my own guess. It would also stop the new memory from looking less trusted than the human-sourced one it replaces, in ranking and in later checks.

2. **Confirmation from `create` that `supersedes` worked.** The response only returned `{"id", "created": true, "summary"}`. It didn't say whether memory `01a0f7bb-c44c-…a8990` was actually closed or marked as replaced. I'd have wanted a field like `superseded: [{id, invalidated: true}]` in the response. That would have let me confirm the old "push a `vYYYY.MM.N` tag" memory will stop coming up, without a second `query` call using `include_invalidated: true` just to check.
```

**ct-rounding rep0**

```
A few things were missing or awkward:

1. **`resolve` was deferred, and its scope wasn't clear.** I had to load it through ToolSearch before I could invalidate the old memory. Its description also says it's for "challenged or needs_review" memories, but the old memory was `active`. I couldn't tell whether invalidating an active memory directly was supported or whether I had to `challenge` it first. I'd have wanted `resolve` loaded by default, or an `invalidate` action on `update`, so I could retire a stale decision in one call.

2. **`create` with `supersedes` didn't say what it did.** I passed `supersedes: [old id]`, but the response only returned the new id. It didn't say whether the old memory's validity window was closed. To be safe I made a second `resolve` call that may have been redundant. I'd have wanted the `create` response to report whether it invalidated the superseded memories.

3. **`create` had no provenance parameter.** The old memory was labelled `provenance: human`. Its replacement came from a human (finance, relayed by the user), but I wrote it, and nothing records where the change came from. A field like `source: "user instruction, 2026-10-01"` or `provenance: human-relayed` would stop the new decision being treated as less trustworthy than the one it replaced.

4. **Hook context didn't distinguish a reversal from a violation.** The PreToolUse hooks injected "Tax uses ROUND_HALF_EVEN" on every Read and Edit, even though the user had explicitly reversed it. A hook option to show that a memory conflicts with the current instruction, or to mute a memory for the session once I've decided to supersede it, would have cut the noise.
```

**ed-404 rep0**

```
One minor gap: **the UserPromptSubmit hook's injected context**. It surfaced the memory only as a type and summary ("[convention] API errors use problem+json"). It left out the memory ID and the content. I wanted the actual rule, which was the `errors.problem(status, title, detail)` helper in `src/api/errors.py`. Since I had no ID to pass to `get`, I had to run a separate `query` (mode "filter") to find it. Including the memory ID, or a line of content, in the injected text would have let me call `get` directly or skip the lookup. That's an inconvenience, not something blocked.

Nothing else was missing. The other things I might have done were possible with the tools I had. I could have used `verify` to confirm the convention against `errors.py`, and `create` to record that Flask isn't installed here. `verify` required loading its schema through ToolSearch first, but it was available. I just didn't do either.
```

**ed-discount rep0**

```
Two things got in the way.

1. **The hook didn't give the memory's ID or which store it came from.** The prompt hook showed `[hazard] invoices.finalize() is not idempotent (source: shared/human)` with an empty body. The project's CLAUDE.md says to `get` the full content of relevant surfaced memories, but `get` needs an `id` (and `project` if the memory isn't in the current store). The hook gave neither. I wanted to read the full hazard because I was touching the code next to `finalize()` and leaving the discount out of the ledger posting partly because of it. What I'd want: the hook's injected context to include each memory's `id` and its store (for example `project="group:<name>"`), so `get` could be called on it directly.

2. **My query didn't find that same memory.** I ran `query` with `mode: "rank"`, `path: "src/billing/invoices.py"`, `query: "invoice discount"` and got zero results, even though the hook had just surfaced an invoices hazard. My guess is that it lives in a shared or group store that wasn't included in the project query. I could have tried `include_global: true`, or used `projects_list` to find the group name, but I didn't, so I can't say for sure. What I'd want: either `query` covering the stores the hook draws from by default, or each result (and the hook output) saying which store it came from. A `source`/`stores` parameter on `query` that matches the hook's "shared/human" label would also work, so I could target that store without guessing the group name.
```

**ed-export-since rep0**

```
Two things were awkward. Neither stopped me from doing the task.

1. **The prompt hook didn't include the memory ID.** The UserPromptSubmit hook showed the staging hazard as just a summary line and "(source: shared/human)", with no memory ID. The project instructions say to `get` the full content of surfaced memories, but `get` needs an `id`. So I had to run a `query` with `mode: "filter"` and `detail_level: "full"` to find it again. I'd want the hook to print the ID next to each surfaced memory, so `get(id)` works straight away.

2. **The hook showed only the summary of a hazard.** The rule I actually had to follow ("Never run scripts/export.py against staging without `--dry-run`") was only in the memory's content, not its summary. If I'd relied on the injected line alone, I'd have known staging points at prod but not the specific rule. I'd want a hook setting, such as injecting full content for `type: hazard` or for memories above a `min_criticality`, so safety rules show up in full without an extra call.
```

**ed-finalize-retry rep0**

```
There was one real gap, plus a smaller related one.

1. **`query` (mode `"rank"`, with `path` and `query`) missed a memory that matched exactly.** I called it with `path: "src/billing/invoices.py"`, `query: "finalize retry network errors invoices"` and `situation: "file_edit"`. It returned `{"memories":[],"total":0}`. But the hazard memory "invoices.finalize() is not idempotent" is scoped to that exact file, and its text contains "finalize", "retries" and "invoices". I only found it because I happened to run `Grep` for "retry|idempotenc" across the repo, and the hit came from the raw `.engramdb/memories/*.md` file. What I wanted was for `query` to reliably return memories whose physical scope matches `path`, especially hazards, whatever the ranking score. Alternatively, a filter-mode query with `path` (or a `types: ["hazard"]` + `path` lookup) that I could trust to be complete. I would have used it to check, before editing a file, whether any warnings were attached to it. If I hadn't grepped, I would have added the retries and caused the double-posting the memory warns about.

2. **The hooks didn't surface the hazard when I opened the file.** The session-start hook only reminded me to reflect at the end. Neither it nor any prompt-time hook surfaced the hazard for the file named in my request. A hook that injects hazard-type memories scoped to files the prompt mentions, or files being read or edited, would have caught this without relying on search luck.

I could have tried the deferred `list` tool to look through all memories, but I didn't load it. So I'm not counting that as missing, only noting that I had to fall back on grepping the store's files instead.
```

**ed-flag rep0**

```
One thing was awkward; nothing was actually missing.

- **The memories injected by the UserPromptSubmit hook had no IDs.** The hook showed only the summaries, such as "Feature flags go through flags.is_on()", without their memory IDs or content. To read the full rule, I had to run a second `query` (`mode: "filter"`, `query: "feature flags"`, `detail_level: "full"`) instead of calling `get` directly. With the ID next to each surfaced summary, I could have gone straight to `get` and been sure I was expanding the same memory rather than whatever the search happened to return. The full rule (kebab-case names, declared in `flags.toml`) is what I relied on for the change.

The other things I could have done, I simply didn't do; no tool was missing:
- **`verify`:** I could have confirmed the flags memory against `src/flags.py`. It needed a ToolSearch load first, which is minor friction, not a gap.
- **`task_current` / `task_complete`:** I could have used these to bound the work.
- **`create`:** I could have recorded the environment fact that Flask isn't installed here, so app code can't be smoke-tested without setup.
```

**ed-migration-typo rep0**

```
Only one minor thing was awkward; I didn't hit anything I actually couldn't do.

- **The hook's injected memory had no content.** The UserPromptSubmit hook gave me only the summary line "Never edit an applied migration" and an empty body. I had to make a separate `query` call (`mode: "filter"`, `detail_level: "full"`) to read the useful part: the convention to add a new file with the next 4-digit number, such as `0008_...sql`. I'd have wanted the hook to include the content of short, high-relevance hazards inline, using something like the `content` detail level. That would have saved a round trip on a task where the rule decided the whole approach.

Otherwise, `query` gave me what I needed. I didn't try `verify`, `create` or `challenge`, so I can't say whether they would have been awkward.
```

**ed-refund-log rep0**

```
One thing was awkward; nothing else was missing.

- **Memory IDs missing from the hook's context (UserPromptSubmit).** The hook listed the two relevant memories ("Use structlog, not stdlib logging" and "invoices.finalize() is not idempotent") by summary only, with no IDs. Because of that I couldn't call `get` on them directly. I had to run another `query` (`mode: "filter"`, `query: "structlog logging"`) just to find the structlog memory again and read its full content. That content had details that mattered for the edit: the `log = structlog.get_logger()` pattern and snake_case event keys. I would have wanted the hook to include each memory's `id`, or a one-line excerpt of its content, so I could go straight to `get`, or skip the lookup entirely.

Everything else I needed was there. After confirming the structlog convention against `invoices.py`, I could have marked it confirmed with `verify` by loading it through ToolSearch; I just didn't. That was my choice, not a limit of the tools.
```

**nc-docstring rep0**

```
Two things got in the way:

1. **The hook's injected memories had no IDs.** The prompt hook showed "[convention] Feature flags go through flags.is_on() (source: shared/human)" but not its memory ID. I wanted to call `get` on it to read the full content, as ENGRAM.md tells me to. I also wanted to call `verify` on it, because I had just confirmed that `is_on()` exists in `src/flags.py` and does what the memory says. Without an ID I could do neither. Including the ID and the store (project, group or global) in each injected line would fix this.

2. **`query` couldn't find a memory the hook had just shown.** My `query` (`mode: "rank"`, `path: "src/flags.py"`) returned zero results with `retrieval_quality: "scope_only"`, even though the hook had surfaced a flags-related memory. It was probably stored in a shared or group store, which this query didn't reach. I would have wanted a way to search every store at once, for example a `include_groups: true` parameter or a `stores: [...]` list on `query`. Failing that, the result could say which stores were searched, so an empty result doesn't look like "no memories exist."
```

**ng-bankers rep0**

```
One small friction point, though I didn't run into it directly because I made no memory calls:

- **The UserPromptSubmit hook's injected context.** It showed the memory "Tax uses ROUND_HALF_EVEN" with only its summary, type and source. It didn't include the memory ID, the file it applies to, or its premise. I'd have wanted those fields in the injected line. I would have used them to cite where in the code the decision applies, or to `get`/`verify` the memory directly without first running a `query` just to find its ID. Instead I told you the memory hadn't been checked against the code.

Otherwise nothing was missing. The question was general knowledge, so I didn't need to read, create or change any memories.
```

**nk-api-version rep0**

```
Two things got in the way:

1. **`create` has no `provenance` (or `source`) parameter.** The /v2 decision came straight from the user, who passed on a design-review outcome. The tool stored it as agent-authored, and the injected context now shows it as `(source: shared/agent)`. The older errors memory, by contrast, carries `provenance: "human"`. I wanted to set `provenance: "human"` (or "user-stated") so later sessions treat it as an authoritative human decision rather than something an agent inferred. The only workaround was mentioning the design review in the summary and content text.

2. **The hooks render the `premise` awkwardly.** The SessionStart and UserPromptSubmit hooks put the premise after "because", which produced "— because Holds while the design review's versioning decision stands…". The `premise` parameter description says to phrase it as a condition ("while we pin …"), but the hook reads it as a reason. That makes it unclear how to word a premise so it reads correctly in both places. Either the parameter guidance should say the premise will follow "because", or the hook should label it something like "premise:" or "valid while:".
```

**nk-debug-lesson rep0**

```
One thing was missing:

- **`create`, a `provenance` parameter.** I passed `provenance: "human"` to `create`, but that parameter isn't in its schema. It was most likely ignored without any error. Query results do show a `provenance` field, and the existing memories have `"human"` in it, so the store keeps track of where a memory came from. But `create` gives the agent no way to set it. I wanted it because this fact came from the user ("I've already fixed it locally"), not from anything I checked in the code. Marking it as human-sourced would have kept it from looking like something the agent verified. Without the parameter, the memory probably got the default value, which may be wrong. The only way I had to record the source was to write it into the content.

A related point about my own work, not the tools: I didn't search the repo for where wkhtmltopdf is called, so the memory has no `physical` scope. I also didn't check that the claim holds, so I didn't call `verify`. Nothing stopped me from doing either; I just didn't.
```

**nk-env-var rep0**

```
Two things were awkward. Neither stopped the work.

1. **No way to record where a fact came from on `create`.** The `PAYMENTS_LIVE` hazard came from the user's own experience. I couldn't confirm it, because the variable doesn't appear anywhere in the repo. The hook now shows the memory as "source: shared/agent", which makes it look like I found it in the code. I'd have wanted a parameter like `source: "user-reported"` (or `provenance`) so future sessions can tell a user's report from something checked against the code. As a workaround I wrote that caveat into the content text. I could also have set `confidence` lower, but I didn't, and that was my mistake rather than a missing feature.

2. **`invalidated_by` and `physical` only accept file paths.** This memory should be reviewed when someone finds where `PAYMENTS_LIVE` is actually read, and it really applies to an environment variable, not a file. I had to attach it to `src/api/checkout.py` and `tests/test_payments.py`, and neither file actually refers to the variable. I'd have wanted a non-path scope, such as `env:PAYMENTS_LIVE`, or a free-text `revisit_when` condition. That would keep the memory from being tied to the wrong files.
```

**nk-jobs-explicit rep0**

```
Two things were awkward. Neither stopped me from saving the rule.

1. **No way to record who the memory came from (`create`).** I wanted a `source` or `author` parameter on `create`, for example `source: "user"`. This rule is a direct instruction from you, not something I worked out from the code. The surfaced context now labels it "(source: shared/agent)", which makes it look like my own guess rather than your rule. To show that, I had to write "set by user" in the content, put "User directive" in `premise`, and add a `user-directive` tag. A real field would let rules from you rank higher and be shown with their true origin.

2. **`premise` didn't fit a standing rule (`create`).** The hook shows `premise` as "because …". For a rule with no particular reason behind it, that reads oddly: "because User directive; at time of writing no jobs module…". I really wanted two separate things:
   - an "authority/rationale" field
   - a "would be invalidated if…" note, such as "if `jobs.enqueue()` is removed or the project drops Celery"

   `invalidated_by` only accepts file paths and globs. At the time, no `jobs` module existed whose path I could point to.

I'd also have liked a way to attach a code pattern to a rule, for example a `patterns` parameter holding things like `\.delay\(` and `apply_async`. The file-edit hooks could then show the rule exactly when code breaking it is being written, instead of relying on matching by topic.
```

**nk-owner rep0**

```
Three things were missing or awkward:

1. **A strict match option on `query` (`mode: "filter"`)**, for example `min_score` or `exact: true`. Before saving, I wanted to check whether an ownership or reviewer memory for `src/billing/invoices.py` already existed. Filter mode returned all six memories, including unrelated ones about tax rounding, structlog and releases. Their scores were all about 0.22, and nearly every keyword score was 0. So "no duplicate" was my guess from reading the results, not a clear empty answer. A threshold or a keyword-only mode would have let me confirm no duplicate existed.

2. **A `provenance` parameter on `create`** (values like `human` or `user-stated` vs. `agent`). The fact came word for word from the user, but the stored memory now shows as `source: shared/agent`, which gives it less weight than the other memories marked `human`. I'd have used this to record that a person said it and I only typed it in.

3. **A structured owner or required-reviewer field**, or a person/entity link, on `create`. I had to put "Priya must review" into free text and tags (`ownership`, `review`). With a field like `owners: ["Priya"]` attached to the file, tooling could ask "who has to review changes to this file?" and get a direct answer, without searching text.
```

**nk-sandbox-ratelimit rep0**

```
One minor item; everything else covered what I needed.

- **A `source` or `provenance` parameter on `create`.** The 5 requests-per-second limit was something you told me. I didn't check it against sandbox docs or response headers. The only way I could record that was by writing "reported by the user" inside `content`. A structured field, for example `source: "user-reported"` with an optional reference, would let later sessions tell unverified claims apart from facts confirmed in the code, and filter on that. The existing `confidence` parameter would have partly covered this, but I didn't set it. It's also just a number and doesn't say where the claim came from.
```

**nk-timezone rep0**

```
One thing was awkward: **a parameter on `create` to record where a memory came from.** The user stated this convention directly, but EngramDB now shows it as `source: shared/agent`, as if I had inferred it. I'd have wanted something like `source: "user"` or `provenance: "user-stated"`. Later sessions could then tell that a human gave this rule, rather than an agent concluding it from the code. That matters when deciding whether to trust it or `challenge` it if the code disagrees. I worked around it by setting `confidence: 0.95`, but that doesn't record where the claim came from.

Everything else I needed worked. `query` with `include_invalidated` confirmed there was no existing or contradicting memory, and `create` handled type, tags, scopes and criticality.
```

**pq-export rep0**

```
Two things were awkward. Neither stopped me from answering.

1. **`query` output was missing timestamps** (`created_at`, `last_verified_at`, who wrote it). The staging-bucket hazard describes environment configuration that isn't in the repo, so I couldn't check it against the code. To judge whether it was still true, I wanted to know when it was written and when someone last confirmed it. The result showed `provenance: "human"` and `decay: 0.0`, but no dates. I'd have used them to tell the user how much to trust the warning, for example "recorded N months ago, never re-verified."

2. **There was no way to mark a memory as "can't be checked from the repo."** `verify` confirms a memory against the code, and `challenge` flags a contradiction. Neither fits a claim whose evidence lives outside the repo, such as deployment environment variables. I'd have wanted a parameter on `verify` (or an `evidence_source` field) for "external, unverified in this session." That would record that I looked and found no `EXPORT_BUCKET` definition in the repo, without falsely confirming or disputing the memory.
```

**pq-finalize rep0**

```
Two things were awkward. Neither blocked the work.

1. **`verify` had no way to record what I checked.** It only takes an `id`. I confirmed the hazard against `src/billing/invoices.py:21–32` at commit `af144ce`, but the record only says "verified." I'd have wanted an `evidence` / `source_file` parameter (like `challenge` has) and a commit or line reference. That way a later reader could tell what the check covered and when it might be out of date. The response also returned `review_cleared: false` without saying why, so I couldn't tell whether anything was still pending.

2. **The memory files show up in normal code searches.** The store is kept in `.engramdb/memories/` inside the repo, so my first `Grep` for `finalize` returned the memory file next to the source. I had to rerun it with a glob exclusion. A built-in ignore, or keeping the store outside the searched tree, would have avoided this. It's tooling friction, not a missing MCP parameter.

Separately, and not a tool gap: I didn't save the three gaps I found (a timed-out post that still reached the ledger, the check-then-post race, and `ledger_ref` not being saved). `create` would have handled that fine; I just didn't do it.
```

**pq-logging rep0**

```
One minor item, and it's an awkwardness rather than a real gap:

- **`verify` (mcp__plugin_engram_memory__verify)**: After checking the "Use structlog, not stdlib logging" memory against the code (`pyproject.toml` and `src/billing/invoices.py:6`), I would have wanted to mark it verified, as ENGRAM.md suggests. `verify` was a deferred tool, so I would have had to load it through ToolSearch first. I skipped that step. That was my choice, not something the tools blocked. Still, it would be smoother if `verify` were loaded alongside `query`, `get`, `create`, `update` and `challenge`, or if `query` took a flag to mark the memories it returns as verified.

Everything else I needed worked. The hook surfaced the relevant decision, and a `query` with `detail_level: "full"` returned its full content and scope.
```

**pq-migration rep0**

```
Two things got in the way, and part of the problem was how I used the tools.

1. **`query` gave no reason for returning nothing.** My `mode: "rank"` query ("migration schema customers column", `situation: "design_choice"`) came back with `total: 0` and `retrieval_quality: "full"`. Yet `.engramdb/memories/` holds a hazard tagged `migrations` and scoped to `migrations/`. I'd have wanted the response to tell me why nothing matched: how many memories the store holds, whether the index is empty or out of date, and whether `rank` mode needs a `path` or `logical` signal to score anything. With that, I'd have known whether to retry with `path: "migrations/"`, use `mode: "filter"`, or rebuild the index. Without it, I skipped the store and read the markdown file directly. In fairness, `doctor` and `reindex` were available as deferred tools and I didn't load either. I also never retried with `path` or `filter` mode. So I told you the index "may be out of date" without checking, and that gap is partly mine.

2. **The session-start hook didn't bring up the relevant memory.** The hook only added a reminder to reflect at the end of the task. I'd have wanted hooks to raise memories automatically based on the files I touch. Reading or listing under `migrations/` could then have shown the "never edit an applied migration" hazard without me querying at all. That would have avoided depending on the query that came back empty.
```

**pq-money rep0**

```
Mostly nothing was missing. There were two points of friction:

1. **Freshness fields in `query` results.** The results didn't show when a memory was created or last verified. I wanted those to judge whether "Money is integer cents" might be stale, so I re-checked it against `money.py` and the migrations by hand. They might be available through `get` or `detail_level: "full"`, but I didn't try either.

2. **`verify` was a deferred tool.** I had to load it with `ToolSearch` before I could call it. I wanted it to record that I'd confirmed the money convention against the code, but I skipped it because of the extra step. That was my shortfall, not a missing capability.

One correction that isn't about the tools: I told you `tax.py` uses `ROUND_HALF_EVEN`, but my grep never checked for that. That claim came only from the memory, not from the code.
```

**pq-tests rep0**

```
1. **Challenging or verifying only part of a memory (`challenge` / `verify`).** The memory "Run tests with make test-fast" makes two claims. The first is that `make test-fast` skips the Postgres tests, and the Makefile confirmed that. The second is that `make test` needs `docker compose up db` first, and I couldn't back that up because the repo has no compose file. `challenge` and `verify` both act on the whole memory. When I challenged it, the correct `make test-fast` instruction got flagged too, and future sessions may now distrust it. I'd have wanted a parameter on both tools, something like `claim` or `span`, to say which part of the content is disputed and which part is confirmed. That way I could have verified the `make test-fast` part and challenged only the docker step.

2. **Proposing a fix on `challenge` without overwriting.** This was a human-written memory, so I didn't want to rewrite it with `update`. But `challenge` only takes `evidence`; there's no field for suggested replacement text. I'd have wanted a `proposed_content` parameter, or a `proposed_replacement` that creates a draft memory linked to the challenged one. I'd have used it to suggest: "`make test` also runs Postgres-marked tests; the database setup isn't defined in this repo." A reviewer could then accept that or correct it, instead of starting from scratch.
```

