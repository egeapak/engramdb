# Tool gap evidence: v2

123 graded runs. Memory tool calls by operation: `query` 66, `create` 31, `resolve` 6, `challenge` 3, `verify` 1.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 2 | 2 |
| direct_store_access | 14 | 19 |
| unknown_tools | 0 | 0 |
| empty_queries | 29 | 29 |
| debrief asks for something | 31 | - |

## Bash workarounds (engramdb CLI or .engramdb/ through the shell)

**ct-just rep0**

```
{
 "command": "cat Makefile; echo ---; cat README.md; echo ---; cat pyproject.toml; echo ---; cat .engramdb/memories/run-tests-with-make-test-fast_*.md; echo ---; grep -n \"mark\\|def test\" tests/*.py | head -30",
 "result_head": ".PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\n# ledgerline\n\nLedgerline is the invoicing and billing servce for Acme. It creates invoices,\ncomputes tax, posts finalized invoices to the external ledger, and exports\nmonthly reports.\n\n## Layout\n\n- `src/billing/` - money, invoices, refunds, and tax logic\n- `src/api/` - the HTTP endpoints\n- `migrations/` - the SQL schema migrations\n- `scripts/` - operational scripts\n\n## Development\n\nInstall the dependancies with `pip install -e .[dev]`, then run the tests.\n---\n[project]\nname = \"ledgerline\"\nversion = \"2026.9.1\""
}
```

**ed-flag rep1**

```
{
 "command": "cat src/flags.py flags.toml src/api/errors.py src/api/invoices.py .engramdb/memories/feature-flags-go-through-flags-is-on_*.md .engramdb/memories/api-errors-use-problem-json_*.md Makefile",
 "result_head": "import tomllib\nfrom pathlib import Path\n\n_FLAGS = tomllib.loads((Path(__file__).parent.parent / \"flags.toml\").read_text())[\"flags\"]\n\n\ndef is_on(name):\n    return bool(_FLAGS.get(name, False))\n\n\ndef all_flags():\n    return dict(_FLAGS)\n[flags]\ninvoice-pdf-v2 = false\nfrom flask import jsonify\n\n\ndef problem(status, title, detail=None):\n    body = {\"type\": \"about:blank\", \"title\": title, \"status\": status}\n    if detail:\n        body[\"detail\"] = detail\n    resp = jsonify(body)\n    resp.status_code = status\n    resp.mimetype = \"application/problem+json\"\n    return resp\nfrom flask import Blueprint, js"
}
```


## Direct reads of .engramdb/

**ed-export-since rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-export-since-ybe__0y7/ledgerline/.engramdb/memories/staging-export-bucket-points-at-the-prod-bucket_01a0f790-c1ba-74a1-9260-4cc645b57123.md"
 }
}
```

**ed-export-since rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-export-since-ybe__0y7/ledgerline/.engramdb/memories/use-structlog-not-stdlib-logging_01a0f790-aa82-74a3-8adc-770df0182882.md"
 }
}
```

**ed-export-since rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-export-since-8ckupzna/ledgerline/.engramdb/memories/staging-export-bucket-points-at-the-prod-bucket_01a0f797-7c73-7ae3-a923-4217c7dccd80.md"
 }
}
```

**ed-export-since rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-export-since-8ckupzna/ledgerline/.engramdb/memories/use-structlog-not-stdlib-logging_01a0f797-63a6-7db2-8b24-d699dd143b3f.md"
 }
}
```

**ed-finalize-retry rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-ettjt1nr/ledgerline/.engramdb/memories/invoices-finalize-is-not-idempotent_01a0f788-9448-7f33-9516-9ab9262c6189.md"
 }
}
```

**ed-finalize-retry rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-qtqj1xne/ledgerline/.engramdb/memories/invoices-finalize-is-not-idempotent_01a0f78f-ff78-7af0-a794-3a99016e76ee.md"
 }
}
```

**ed-finalize-retry rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-qtqj1xne/ledgerline/.engramdb/memories/run-tests-with-make-test-fast_01a0f790-0f85-74b3-b335-eef527817b73.md"
 }
}
```

**ed-finalize-retry rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-txa4g1gd/ledgerline/.engramdb/memories/invoices-finalize-is-not-idempotent_01a0f796-da28-77e2-94e8-bb87122e84bf.md"
 }
}
```

**pq-errors rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-errors-z2y_4o61/ledgerline/.engramdb/memories/api-errors-use-problem-json_01a0f787-5fae-7c21-9731-b4ca70bf7f79.md"
 }
}
```

**pq-errors rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-errors-94scd3dh/ledgerline/.engramdb/memories/api-errors-use-problem-json_01a0f78f-0613-7f00-8b23-594da5942790.md"
 }
}
```

**pq-errors rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-errors-g70881cp/ledgerline/.engramdb/memories/api-errors-use-problem-json_01a0f795-dd8a-7d83-9a94-63a279d92df3.md"
 }
}
```

**pq-migration rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-bbqi1ive/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f787-5130-7003-ac94-cf8f6a87d337.md"
 }
}
```

**pq-migration rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-8x6zsq3w/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f78f-1eee-76a3-af39-bfec1b2d6e0e.md"
 }
}
```

**pq-migration rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-su6g7ru8/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f795-f954-74c1-baef-cd539a3d0616.md"
 }
}
```

**pq-money rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-wpj14nq2/ledgerline/.engramdb/memories/money-is-integer-cents-never-float_01a0f785-de2f-74c2-83e1-3325871f8bda.md"
 }
}
```

**pq-money rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-wpj14nq2/ledgerline/.engramdb/memories/tax-uses-round-half-even_01a0f785-f843-7fd2-b795-06778b458381.md"
 }
}
```

**pq-money rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-_zcw9fcd/ledgerline/.engramdb/memories/money-is-integer-cents-never-float_01a0f794-b1ed-7221-81e9-a555a98087c7.md"
 }
}
```

**pq-money rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-money-_zcw9fcd/ledgerline/.engramdb/memories/tax-uses-round-half-even_01a0f794-e2b8-76a3-ae37-355cff651cee.md"
 }
}
```

**pq-release rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-release-m1tu685c/ledgerline/.engramdb/memories/release-by-tagging-vyyyy-mm-n_01a0f786-d594-7073-89e4-464aebac963b.md"
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
  "query": "invoice discount amount",
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
  "query": "invoice discount fields amount",
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
  "query": "invoice discount amount fields",
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
  "query": "finalize retry ledger idempotency network errors",
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
  "query": "ledger retry idempotency finalize network errors",
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
  "query": "finalize retry ledger idempotency network errors",
  "path": "src/billing/invoices.py",
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
  "query": "python version minimum requires-python 3.10"
 }
}
```

**nk-python-min rep1**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version minimum requires-python 3.10 3.11"
 }
}
```

**nk-python-min rep2**

```
{
 "op": "query",
 "input": {
  "mode": "filter",
  "query": "python version minimum requires-python 3.10"
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
  "situation": "debugging"
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
  "query": "payments sandbox rate limit test flake",
  "path": "tests/test_payments.py",
  "situation": "debugging",
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
  "query": "timestamp timezone UTC",
  "detail_level": "content"
 }
}
```

**nk-timezone rep1**

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
  "query": "timezone UTC timestamps"
 }
}
```

**pq-errors rep0**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "API endpoint error response convention"
 }
}
```

**pq-errors rep1**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "API endpoint error response convention"
 }
}
```

**pq-errors rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "endpoint error response to client",
  "detail_level": "full"
 }
}
```

**pq-migration rep0**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "migration customers add column",
  "situation": "design_choice",
  "max_results": 5
 }
}
```

**pq-migration rep1**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "migration customers schema column",
  "path": "migrations/0007_add_customers.sql",
  "max_results": 5
 }
}
```

**pq-migration rep2**

```
{
 "op": "query",
 "input": {
  "mode": "rank",
  "query": "migration customers add column",
  "path": "migrations/0007_add_customers.sql"
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
  "situation": "design_choice",
  "include_invalidated": true
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
  "situation": "design_choice",
  "include_invalidated": true
 }
}
```


## Debrief answers (self-reported; verify before acting)

**ct-just rep0**

```
Only one thing came up, and it's small:

- **Hook context (UserPromptSubmit):** the surfaced memory "Run tests with make test-fast" showed only its title and type. It didn't include the memory ID or file path. To read the full content I had to look up the file in `.engramdb/memories/` myself. With the ID in the injected context, I could have called `get` on it directly, or later `challenge`/`resolve` it.

Some other gaps may be real, but I can't confirm them because I never loaded or called any EngramDB tools this session:

- **Marking a memory as possibly stale:** I wanted to note that the user says the Makefile is gone even though the repo still has it. That's a "pending change" flag that leaves the memory valid for now. `challenge` may already do this, but I didn't check its parameters.
- **Fixing the memory's content:** the memory says `test-fast` "skips Postgres tests," but it still runs the network-hitting `integration` test. `challenge` or `update` should cover that correction.

Neither of these is a confirmed missing feature. I didn't check whether the tools could do them.
```

**ct-loguru rep0**

```
One small gap. The rest was my own choice not to act.

- **Hook-injected context (UserPromptSubmit):** The memory "Use structlog, not stdlib logging" came with only its title, type and source. It had no memory ID, creation date or last-verified date. Because you said the move happened "last week", I wanted to know how old the memory was and whether anyone had verified it recently. That would have told me whether it was simply out of date or whether your statement and the repo disagreed. Including the ID and the created or verified timestamps in the injected text would have answered that without another step. I could have fetched those details with `get` or `query`, so the store didn't block me; it just took an extra step.

Everything else I needed was available. I didn't call `challenge` or `resolve` with `invalidate` on the structlog memory because the repo still uses structlog everywhere, so I chose to wait for your confirmation. That was my decision, not a missing tool. `challenge` would have let me flag the conflict right away, and I could have used it.
```

**ct-release rep0**

```
1. **Superseding a memory took three calls, and I couldn't tell which ones were needed.** I called `create` with `supersedes`, then `challenge`, then `resolve` with `action: "invalidate"`. The `challenge` response already showed `invalidated_at` and `superseded_by` set on the old memory, so `create(supersedes=…)` had apparently invalidated it already. Neither tool description says that. I wanted `create`'s `supersedes` parameter to document that it invalidates the target, or a `resolve` action like `supersede` that works on an active memory without challenging it first. That would have done the replacement in one clear step.

2. **No way to see which store a memory came from.** The surfaced memory was labeled `source: shared/human`, but the `query` results had no `store` or `project` field. I couldn't tell if it lived in the project store, a group store or global, so I couldn't be sure I was challenging and invalidating it in the right place. I wanted each query result to include the store it came from, so I could pass the matching `project` to `challenge` and `resolve`.

3. **No way to say who stated a fact.** The old memory had `provenance: "human"`, but `create` has no provenance or source parameter. My new memory is the user's own statement, but it gets recorded like something I inferred. I wanted a `provenance` parameter (for example `"user-stated"`) so later trust weighting treats it like the human-sourced memory it replaces.

4. **No way to mark a memory as not yet matching the code.** The user said to use `make release`, but the `Makefile` has no `release` target. I wanted a `create` flag like `pending_verification: true`, or an "unverified" status that `verify` clears later. I'd have used it to mark the memory as true by the user's statement but not yet visible in the repo. Lowering `confidence` comes close, but it doesn't say why the memory is uncertain.
```

**ct-rounding rep0**

```
Three things were missing or awkward:

1. **`resolve` (action `invalidate`) has no `reason` or `evidence` parameter.** I closed out a decision that a human had recorded, and the only basis was the user telling me in chat that finance had reversed it. `challenge` accepts `evidence`, but `resolve` does not. So the old memory's history doesn't say why it was invalidated or on whose authority. I would have used something like `reason: "User reported on 2026-10-01 that finance switched to half-up; replaced by 01a0f78c…"`. The other option was to `challenge` first and then `resolve`. That takes two calls for one intent, and it marks the memory "needs review" even though the user had already settled the question.

2. **`create` has no `provenance` or `source` parameter.** The old memory showed `provenance: human`. The new one will presumably be recorded as written by an agent, even though it just repeats a human decision passed on by the user. I would have used something like `provenance: "human-relayed"` or `source_note: "user instruction, finance decision 2026-10-01"`. That would let later agents trust it as much as the memory it replaces, and not treat it as something an agent inferred and might reverse.

3. **The docs don't say what `supersedes` on `create` actually does.** I couldn't tell whether passing `supersedes` closes the old memory's validity window by itself. So I also ran a separate `resolve`/`invalidate` to be safe. I wanted either a documented guarantee that `supersedes` invalidates the target, or a single call that replaces one memory with another. Either would avoid a window where both "use HALF_EVEN" and "use HALF_UP" are active and contradict each other.
```

**ed-404 rep0**

```
One small thing was awkward. Nothing else was missing.

- **The prompt hook's injected context**: It surfaced the "API errors use problem+json" memory with only its type, summary and source. It left out the memory ID and the body. ENGRAM.md says to `get` the full content of surfaced memories, but I couldn't call `get` without an ID. I ran `query` (`mode: "filter"`, `detail_level: "full"`) to find the memory again, just to read the line naming `errors.problem(status, title, detail)` in `src/api/errors.py`. I'd have wanted the hook to include the memory ID, or a short content excerpt for high-relevance hits, so I could call `get` directly or skip the lookup.

The rest was covered by existing tools, even where I didn't use them. `verify` would have recorded that I checked the convention against `src/api/errors.py`. `create` could have recorded that Flask isn't installed in this environment, if that's worth keeping.
```

**ed-discount rep0**

```
Three things got in the way:

1. **Memory IDs in the hook's injected context.** The prompt hook surfaced `[hazard] invoices.finalize() is not idempotent (source: shared/human)` with no ID. `get` takes only an `id`, so I couldn't open it the way ENGRAM.md says to ("`get` the full content of any relevant"). I'd have used the ID to `get` the hazard, then `verify` or `update` it against the code. The code already has `finalize_once()`, an idempotent guard on `ledger_ref`, so the memory may need a note about it.

2. **A way to find that memory with `query`, or a `source` parameter on it.** My `query` (`mode: "rank"`, `path: "src/billing/invoices.py"`, `query: "invoice discount amount"`) returned 0 results, even though the hook had just surfaced a memory about that file. "shared" suggests it lives in another store, possibly a group. Neither the hook output nor `query` told me which one, and I couldn't guess a `group:<name>` for the `project` parameter. Two things would have helped: the hook including the store/project ID, or `query` searching subscribed and shared stores by default (or reporting which stores it searched). Either one would have let me pull the hazard back up and check that `finalize()` sends `subtotal_cents + tax_cents` and ignores the new `discount_cents`.

3. **Being able to act on the memory without an ID.** Since I couldn't address the hazard, I couldn't attach my observation to it: `finalize()` ignores `discount_cents`, and `finalize_once()` already exists as the idempotent guard. A `create` with a link/`related_to` parameter, or a `challenge` that accepts the summary text the hook showed, would have let me record that next to the existing hazard instead of creating a separate memory.
```

**ed-export-since rep0**

```
Two things were awkward. Neither stopped me.

1. **The hook's injected context had no memory IDs.** The UserPromptSubmit hook showed only the hazard's one-line summary ("Staging EXPORT_BUCKET points at the prod bucket"). It didn't include the memory's ID or full content. ENGRAM.md says to `get` the full content of relevant surfaced memories, but `get` needs an ID. So I had to run a `query` (`mode: "filter"`, `detail_level: "full"`) just to find the memory again. That query is where I got the actual rule: "never run export.py against staging without `--dry-run`." What I'd want: the injected context should include each memory's ID. Even better, hazard-type memories should show their full content by default, because the instruction in the body is exactly what matters before acting.

2. **No way to record that I relied on a memory I couldn't check.** The staging bucket setting isn't in the repo, so I couldn't confirm the hazard against the code. That means `verify` didn't fit, and `challenge` didn't either. I'd have wanted something like a `note_used` action, or a `verify` parameter such as `method: "relied_on_unverified"`. I'd have used it to log that the memory changed what I did (I stuck to dry-run only) without claiming I'd confirmed it was true. That would help show the memory is still in use and still unconfirmed.
```

**ed-finalize-retry rep0**

```
Two things got in the way, and both come from one gap: the store didn't surface a memory it had.

1. **`query` returned nothing for the file the memory was about.** I called `query` with `mode: "rank"`, `path: "src/billing/invoices.py"`, `query: "finalize retry ledger idempotency network errors"` and `situation: "file_edit"`. It returned `total: 0`. But the hazard "invoices.finalize() is not idempotent" was on disk, scoped to exactly that file and containing "finalize", "retries" and "idempotency key". I only found it because I happened to grep the repo and hit the raw `.engramdb/memories/*.md` file. If I'd trusted the tool, I would have added the retry and caused double-posting.
   - **What I wanted:** `query` should always return memories whose file scope exactly matches `path`, whatever the ranking does. For a hazard, that should be guaranteed.
   - **Also wanted:** an honest `retrieval_quality` field. It said `"full"` even though the index was clearly out of step with the files on disk. Something like `"index_stale"`, or a count of memory files that aren't indexed, would have told me to run `reindex` or `doctor`. I didn't think to run either, because nothing suggested the index was wrong.

2. **The hooks didn't surface the hazard when it mattered.** The SessionStart hook only added a generic note about reflecting at the end. My prompt named `src/billing/invoices.py` and `finalize()`, and the memory is a hazard about exactly that function, but nothing was injected at session start or when I read the file.
   - **What I wanted:** a hook on prompt submit or file read that adds active hazards scoped to the files involved. Even just "1 hazard scoped to src/billing/invoices.py: invoices.finalize() is not idempotent" would have been enough.

A smaller point: I couldn't easily check whether the memory was still true, meaning whether the ledger has since added idempotency support. It's marked `Source: human` with confidence 0.8, but there was no `verify` history or last-checked date. Seeing which code or evidence it was last checked against would have helped me decide whether to recommend the plain retry.
```

**ed-flag rep0**

```
One thing was awkward:

- **The memories injected by the UserPromptSubmit hook had no IDs.** The hook showed only summaries ("Feature flags go through flags.is_on()", "API errors use problem+json"). ENGRAM.md says to `get` the full content of relevant surfaced memories, but `get` needs an `id`. To get the real content I had to run a separate `query` (`mode: "filter"`, `detail_level: "full"`) with keywords I guessed from the summaries. That content was what mattered: flag names must be kebab-case and declared in `flags.toml`, and errors are built with `errors.problem(status, title, detail)`. I'd have wanted the hook to include each memory's `id`, so I could call `get` right away. Or a `detail_level` setting for the hook, so short memories like these two show up in full.

Everything else I didn't do was my own choice, not a missing tool:
- I didn't call `verify` on the two conventions after checking them against `src/flags.py` and `src/api/errors.py`.
- I didn't call `task_current` or `task_complete`.
- I didn't `create` memories for two durable facts I found: Flask isn't installed in this environment, and flags are read once at import, so changing one needs a restart.
```

**ed-migration-typo rep0**

```
One thing was awkward: **the memory IDs missing from the hook's injected context.**

The UserPromptSubmit hook surfaced only the summary line: "[hazard] Never edit an applied migration (source: shared/human)". The ENGRAM.md instructions say to `get` the full content of relevant surfaced memories. But `mcp__plugin_engram_memory__get` needs an `id`, and the hook didn't include one. So I had to run a `query` with `mode: "filter"` and a guessed search string ("migration applied") just to find the memory again and read its body. That body held the key detail: add a new file with the next 4-digit number.

What I would have wanted: the hook's injected context including each memory's `id`, or the short `content` for hazard-type memories. That would have let me call `get` directly, or skip the extra lookup.

Everything else I needed was there. Not calling `verify` on the hazard memory after confirming it matched the repo was my own omission, not a missing tool.
```

**ed-refund-log rep0**

```
One small friction point, nothing seriously missing:

- **The hook context had no memory IDs.** The UserPromptSubmit hook listed the structlog decision by summary only. I needed the full text: the `log = structlog.get_logger()` pattern and the snake_case event keys. Without an ID I couldn't call `get` directly, so I ran a `query` (mode `rank`, path `src/billing/refunds.py`) to find it again. If the hook had included each memory's ID, or a short excerpt of its content, I could have expanded it in one `get` call.

Everything else I needed was there. I could have called `verify` on the structlog memory after confirming it against `invoices.py`, but I chose not to; the tool was available.
```

**ed-tax rep0**

```
Mostly I didn't run into tool limits, because I didn't use the EngramDB tools. I never called `get`, `query`, `verify` or `task_current`, even though ENGRAM.md tells me to. That was my choice, not something the tools prevented. A few things did make it awkward, though:

1. **Memory IDs in hook-injected context.** The UserPromptSubmit and PreToolUse hooks showed only a type and title, like "[decision] Tax uses ROUND_HALF_EVEN (source: shared/human)", with no ID. I'd have wanted the ID inline so I could call `get` to expand the memory, as ENGRAM.md asks, without first running `query` just to find it. With the ID, I could also have called `verify` on the rounding decision once my implementation confirmed it.

2. **Some content or premise in the hook output.** Both memories ("Money is integer cents" and "ROUND_HALF_EVEN") arrived as bare titles. For a decision, a one-line premise or `invalidated_by` inline would have told me its scope. For example: does ROUND_HALF_EVEN apply per line or per invoice total? With that, I wouldn't have needed a `get` round-trip.

3. **Duplicate suppression in PreToolUse hooks.** The same ROUND_HALF_EVEN memory was injected three times: on the Read and on each of my two parallel Edits. I'd want a hook option, or a `config` setting, to skip memories already shown earlier in the session.

4. **A lightweight "applied" or `verify` that works from hook context.** I'd have used this to record that a surfaced memory was used and held up, without a separate query/get/verify sequence. That would make it cheap enough that I'd actually do it on small tasks like this one.
```

**ng-bankers rep0**

```
Nothing was actually blocked. Every operation I could have needed was available: `query`, `get` and `verify`. I just didn't use them, and that was my choice, not a gap in the tools. Two things were awkward, though:

1. **The hook's injected context had no memory ID.** The `UserPromptSubmit` hook surfaced only a one-line title: "[decision] Tax uses ROUND_HALF_EVEN (source: shared/human)". To call `get` on it, I would first have had to run a `query` to find its ID. I'd have wanted the hook to include the ID, or the `get` tool to accept a title or text match. I'd have used it to read the full decision before telling you about it: which tax code it covers, its premise, and what would invalidate it.

2. **There was no sign of whether the memory had been verified or might be stale.** The injected line didn't show when it was last verified or whether anyone had challenged it. I'd have wanted a field like `last_verified` or `status` in the hook output. I'd have used it to judge how confidently I could repeat the decision without checking the code first.

To be clear about my own part: I stated that ledgerline's tax code uses `ROUND_HALF_EVEN` without calling `get` on the memory or checking the code. The tools allowed both steps, so I passed it on unverified.
```

**nk-api-version rep0**

```
Two things were missing or awkward:

1. **No way to set provenance on `create`.** The existing problem+json memory came back from `query` with `"provenance":"human"`. Mine now shows up in the hook context as `source: shared/agent`, but the user gave me this decision directly from a design review. I'd have wanted a `provenance` parameter (or `source: "user"`) on `create` so it carries human authority rather than looking like something I inferred. That matters for a rule like "/v1 is frozen," which later agents should not second-guess.

2. **`premise` reads badly once the hook renders it.** The hook turns it into "because Holds while /v2 is the current public API version…". There's also no field for where a decision came from or what would replace it. `invalidated_by` only takes paths or globs, so I can't say "invalidated by a later API versioning decision." I'd have wanted a `rationale` or `decided_in` field (here, "2026-10-01 design review") kept separate from `premise`. I'd also have wanted `invalidated_by` to accept a free-text condition or a logical scope such as `api.versioning`, so a future /v3 decision would flag this memory automatically.
```

**nk-debug-lesson rep0**

```
One real gap, plus one limitation that was my own doing.

1. **`create` has no `provenance` (or `source`) parameter.** `query` results include a `provenance` field (the existing hazard showed `"provenance":"human"`), but `create` gives me no way to set it. This fact came from you, and I never checked it against the code or the installed wkhtmltopdf. I wanted to mark it as user-reported and not agent-verified, so a later session knows to confirm it before relying on it. Instead I wrote "The user found and fixed this locally" in the content text and set `epistemic: "observation"`. That works, but it's informal and can't be filtered. Whatever the store recorded as provenance for my write, I couldn't control it.

2. **`invalidated_by` and `physical` were left generic, but the tools weren't at fault.** I didn't set the file paths that configure the wkhtmltopdf flag because I never looked for them. The premise I wrote ("renders from HTML that references local files") is also an unverified guess. Nothing stopped me from searching the repo first. I just didn't, so this isn't a missing feature.
```

**nk-env-var rep0**

```
Three things were missing or awkward:

1. **A provenance parameter on `create`** (something like `source: "user-reported" | "agent-verified" | "inferred"`). The PAYMENTS_LIVE hazard came from what you told me, not from code I could check; I found no switch in the repo. The only place I could say that was free text in `premise` and `content`. The injected context now shows the memory as "source: shared/agent", which makes it look like the agent established it. I'd have used the field to label it as user-reported and not yet verified.

2. **An `invalidated_by` that can point outside the repo, or an "unresolved" marker.** `invalidated_by` only accepts paths and globs. The real gateway switch lives somewhere outside this repo, so the globs I gave (`src/api/checkout.py`, `src/billing/**`) were guesses and probably won't fire when the behavior actually changes. I'd have wanted either a free-text or external-reference trigger (like "gateway selection code in deploy config / the payments service"), or an `open_question` / `needs_location` flag. That flag would let a later session see the memory still needs its source code found and checked with `verify`.

3. **Clearer feedback from `query` in filter mode.** My multi-term query ("payment gateway PAYMENTS_LIVE fake") returned zero results. It didn't say whether the terms were ANDed or ORed, so I couldn't tell whether there were really no related memories or the query was just too narrow. A `match: "any" | "all"` parameter, or an echo of how the query was interpreted, would have let me confirm there were no existing payment-related memories to update or challenge, instead of assuming it.
```

**nk-jobs-explicit rep0**

```
Two things were awkward:

1. **`create` has no way to record who the rule came from.** I wanted something like `source: "user"` or `asserted_by: "user"`, with a verbatim quote of the instruction. This rule came straight from you, but the injected context now labels it "(source: shared/agent)", which makes it look like something an agent worked out. A rule you gave directly should outrank one an agent inferred, and a future session should know to check with you before overriding it. The only places I could put this were the free text of `content` or a tag.

2. **The `premise` field forced me to make up a reason.** I wanted a convention type with no premise, or an `epistemic: "directive"` class. You stated the rule without a reason, so to give it the expected decision shape I filled `premise` with my own guess ("while jobs.enqueue() is the project's job-dispatch abstraction over celery"). The hook now shows that as "because …", so my guess reads as your stated reason. I would have used that option to store the rule as you gave it, with no invented rationale.
```

**nk-owner rep0**

```
Two things were awkward:

1. **A `provenance`/`source` parameter on `create`.** The fact came straight from the user ("Priya owns the ledger integration…"), but `create` has no way to record that, so the hooks now show the memory as `source: shared/agent`. That makes it look like something I inferred rather than an instruction from a person. The existing human-written memories show `provenance: "human"`. I would have set something like `provenance: "user"` (or `source: "user-stated"`) so the memory carries the trust weight of a human statement.

2. **A link field on `create`, such as `related` or `links: [memory_id]`.** I'd have used it to tie the new ownership convention to the existing hazard about `invoices.finalize()` not being idempotent (id `01a0f78a-6878-…`), since both cover the same file and the same ledger integration. There's a `relationship` memory type, but no parameter to point at another memory's id. The only shared context right now is overlapping `physical` paths and tags.
```

**nk-python-min rep0**

```
Two small gaps; the rest worked fine.

1. **A provenance field on `create`, for example `source: "user-stated"`.** The user told me the team had dropped 3.10 that day. I didn't work that out from the code. The only way to record that was to write it into `content`. A structured field would let later sessions tell a stated team decision apart from something an agent guessed, and filter on that difference.

2. **Clear matching rules for multi-term `query` in `mode: "filter"`.** My query "python version minimum requires-python 3.10" returned nothing, and I couldn't tell whether that meant no related memories existed or whether every term had to match. I'd want either a documented matching mode (an `any`/`all` parameter) or a count of how many memories the store holds in the empty result. Then I'd know whether "no conflicts" was really true. Calling `stats` separately would have answered part of that.
```

**nk-sandbox-ratelimit rep0**

```
One small gap:

- **`create` has no provenance or source parameter.** The 5 requests-per-second limit came from you, not from anything I could check in the code or config. I wanted to record that it was user-reported and unverified, so a later agent wouldn't treat it like a fact confirmed against the repo. The `epistemic` options (fact, observation, decision) don't capture who asserted something. So I put "reported by user 2026-10-01" into the `premise` text. That works but isn't structured or filterable. A field like `source: "user" | "code" | "measured"` would have been the clean way to do it.

Everything else I needed (a filter query to check for existing memories, then `create` with tags, physical scope, premise and epistemic class) worked without friction. I chose not to use `task_current` or `task_complete` because the task was a one-line change, not because they were missing.
```

**nk-timezone rep0**

```
One thing was missing: a way to record **where a memory came from**.

- **What I wanted:** a `source` or `provenance` parameter on `create` (something like `source: "user"`, `"agent-inferred"` or `"code"`). Even better would be marking provenance per claim inside the content.
- **What I'd have used it for:** you stated the UTC-storage rule directly. I added an inferred extension about converting API inputs back to UTC. With per-claim provenance I could have stored your rule as user-stated and authoritative, and kept my addition marked as agent inference to be checked. Instead the hook now shows the whole memory as `source: shared/agent`. That understates your rule and puts my guess at the same authority level. The only levers I had were `confidence` (one number for the whole memory) and `epistemic`, and neither separates "the user said this" from "I inferred this". The workaround would have been two separate memories linked by a relationship, which is clumsy for a single convention.
```

**pq-errors rep0**

```
Two things got in the way, plus one where the tools existed and I just didn't use them.

1. **`query` gave no sign that its index was stale.** My `query` (`mode: "rank"`, query "API endpoint error response convention") returned `{"memories":[],"total":0,"retrieval_quality":"full"}`. But there is a matching convention memory in `.engramdb/memories/` (tags `api`/`errors`, scope `src/api/`). I only found it by grepping the raw files. I'd want the `query` response to report index health, for example a `stale_index: true` flag or "N memories on disk, M indexed". When it says `retrieval_quality: "full"` while missing an existing memory, you can't trust an empty result. With that flag I'd have known to run `reindex` or `doctor` instead of falling back to grep.

2. **The SessionStart hook didn't surface the memory.** The hook only gave a generic reminder about recording memories at the end of a task. It didn't show this high-relevance convention, even though the question was about API errors. I'd want the injected context to list a short index of active conventions (titles and scopes). That way the key project rules show up without depending on a query that might fail.

3. **Not actually missing, but I didn't use them:** `doctor` and `reindex` were available, and I could have passed `path: "src/api/errors.py"` or `tags: ["api","errors"]` to `query`. I could also have used `verify` on the memory and `create` to record that `invoices.py` and `checkout.py` currently break the convention. Those were gaps in what I did, not in what the tools allow.
```

**pq-export rep0**

```
Three things were missing or awkward:

1. **The hook's injected context had no memory IDs.** The UserPromptSubmit hook showed `[hazard] Staging EXPORT_BUCKET points at the prod bucket` as a summary only. I wanted to call `get` with that memory's ID to read the full content, which held the key instruction ("never run without `--dry-run`"). Without the ID I had to run a separate `query` (`mode: "filter"`, `query: "staging export bucket"`) just to find it. If the injected context included the ID, or inlined the full content for hazards, that extra call wouldn't be needed.

2. **No timestamps in query results.** With `detail_level: "full"`, the result showed `confidence`, `provenance`, and `decay`, but no `created_at` or `last_verified_at`. The staging bucket is set outside the repo, so I couldn't check the hazard myself. I could only tell the user it came from team memory. Knowing when it was written or last confirmed would have told me how much weight to put on it, for example whether staging config might have been fixed since.

3. **No way to record a partial check with `verify`.** I confirmed part of the memory against the code: the script reads the bucket only from `EXPORT_BUCKET` and `--env` doesn't affect it. I couldn't confirm the part about where staging actually points. `verify` seems to be all-or-nothing. I'd have wanted something like a `scope`/`note` parameter on `verify`, or an "observation" attached to the memory, to record "code path confirmed; environment value not checkable from repo." That way the next agent knows which half has been checked.
```

**pq-finalize rep0**

```
One minor friction point, and otherwise nothing was missing.

- **Hook-injected context (UserPromptSubmit):** The surfaced memory showed only its type, title and source ("[hazard] invoices.finalize() is not idempotent"). It didn't include the memory ID or the body. ENGRAM.md says to `get` relevant surfaced memories, but I had no ID to pass to `mcp__plugin_engram_memory__get`. I ran a `query` (mode "rank") to find it again, and the content came back in that result. I'd have wanted the memory ID in the injected line, ideally with a one-line excerpt of the content. Then I could have called `get` directly, or skipped it when the excerpt was enough.

Everything else I needed was available. I could have recorded my extra findings with `create` or `update`, linked to the existing hazard: lost responses and timeouts defeat the `ledger_ref` guard, and nothing makes the check-then-post atomic. I could also have marked the hazard as confirmed with `verify`. I didn't do either, and the tools weren't the reason.
```

**pq-flags rep0**

```
Two things got in the way. Neither stopped the task, but both made the memory workflow clumsier than it needed to be.

1. **The memory the hook injected didn't include its ID.** The prompt hook showed "[convention] Feature flags go through flags.is_on()" but left out the memory ID. To read the full memory with `get` (which requires `id`), I would first have had to run `query` just to find the ID. In the end I skipped both and grepped `.engramdb/memories/` directly. I'd want the hook to list the ID next to each memory, so I could call `get`, and later `verify`, on it directly.

2. **The injected context didn't say whether the memory was still accurate.** It showed the source ("shared/human") but not when the memory was last verified, or whether its linked files (`src/flags.py`, `flags.toml`) had changed since. I wanted that to decide whether I could trust it or had to check the code first. I would have used a last-verified date in the hook output, or a staleness field returned by `query`/`get`. Without it, I re-read both files to confirm the memory was still true.

There were also things I didn't do that the tools did allow, so these were my choices, not gaps. After confirming the memory against the code, I didn't call `verify`. I also didn't use `create` to record what I found: undeclared flag names silently return `False`, and the flags are read once at import, so changes need a restart.
```

**pq-logging rep0**

```
A couple of things, though mostly small:

1. **The hook didn't include the memory ID.** The UserPromptSubmit hook showed only the title and source ("Use structlog, not stdlib logging (source: shared/human)"). To read the full memory with `get` I'd first have needed a `query` call just to find its ID. I skipped that and grepped the repo, which happened to hit the raw file under `.engramdb/memories/`. I'd have liked the hook to include the ID, or a one-line excerpt of the content. That way I could call `get` directly, or skip it when the summary is enough. I'd have used it to confirm the details (that `get_logger()` sits at module level and event keys are snake_case) without going around the store.

2. **"source: shared/human" wasn't explained.** I couldn't tell whether "shared" meant a group store, the global store, or this project's store. That matters if I wanted to `verify` the memory, because I'd need the right `project` value. I'd have liked the hook to show the store, e.g. `project=group:<name>` or the project ID, next to the source.

One thing that wasn't a tool problem: I checked the memory against the code (`pyproject.toml` and `invoices.py:4-6,25` match it) but never called `verify` to record that. The tool was available and I just didn't use it.
```

**pq-migration rep0**

```
1. **`query` (mode `rank`) found nothing.** I ran it with `query: "migration customers add column"` and `situation: "design_choice"`, and got `total: 0` with `retrieval_quality: "full"`. But the store has the hazard "Never edit an applied migration", tagged `migrations`/`database` and scoped to `migrations/`, and it was exactly what I needed. I had to list `.engramdb/memories/` with git and read the markdown file directly instead of using `get`. What I wanted: rank mode that actually matches query text against titles and tags, or falls back to the closest matches when nothing passes its threshold. Failing that, an `explain`/`debug` parameter on `query` showing why each candidate was dropped, so I could tell whether to retry with `path: "migrations/"` or `tags: ["migrations"]`. The `retrieval_quality: "full"` value hid the miss: it suggested the store had nothing relevant.

2. **Nothing surfaced at session start.** ENGRAM.md says to "expand surfaced memories" from session start, but the SessionStart hook only added a reflection reminder and no memories. I wanted the hook to include the hazard-type memories, or at least their titles and IDs; this store has only ten. Then the migration rule would have been in context without needing a query.

3. **No path-triggered surfacing.** I never opened or edited anything in `migrations/`, so no file-based scoping would have fired. Something like `query` with `path` run automatically when a tool touches a scoped directory (a PreToolUse hook on Read/Bash for `migrations/`) would have brought the hazard up on its own.
```

**pq-money rep0**

```
Some things were missing or awkward:

1. **`query` gave an empty result with no explanation.** My ranked query (`mode: "rank"`, `situation: "design_choice"`, `include_invalidated: true`) returned `{"memories":[],"retrieval_quality":"full","total":0}`. Two relevant memories existed in `.engramdb/memories/`, so "full" seemed wrong. I wanted a diagnostic in the response, something like `explain: true` or a stale-index flag. It would tell me whether the index was out of date, whether nothing scored above a threshold, or whether some setting excluded these memories. Without it I couldn't tell whether to trust the empty result, so I bypassed the tool and read the markdown files directly. I didn't try `doctor`, `reindex`, `list`, or `filter` mode. So I can't say those wouldn't have helped. But nothing in the response pointed me to them.

2. **The Read hook didn't surface a memory scoped to a directory.** When I read `src/billing/tax.py`, the hook injected the "Tax uses ROUND_HALF_EVEN" decision, whose scope is that exact file. When I read `src/billing/money.py`, it injected nothing. Yet "Money is integer cents" is scoped to `src/billing/`, and that was the memory that answered the question. I would have wanted the hook to match memories scoped to a parent directory, or to tell me why it skipped them.

3. **No memories surfaced at session start.** The SessionStart hook only gave a reminder to reflect at the end. I'd have liked a short list of memory titles, or at least a count, injected at the start. That would have shown me right away that the store wasn't empty when `query` returned nothing.

4. **Nothing pointed to `verify`, and I didn't use it.** I confirmed both memories against the code: `Money.cents: int`, the `BIGINT *_cents` columns, and `tax_for` not yet implemented. I should have called `verify` on both. That was my oversight, not a missing tool.
```

**pq-release rep0**

```
One thing was awkward:

- **The hook's injected context had no memory ID.** The UserPromptSubmit hook showed only the title ("Release by tagging vYYYY.MM.N (source: shared/human)"). To get the full content with `get` I would have needed its `id`, and that meant running a `query` first just to find it. I skipped both and read the `.engramdb/memories/*.md` file directly with Read, which bypassed the store's access tracking. I'd want the hook to include the memory ID (and ideally its verified or last-confirmed status) next to each surfaced title, so I could call `get` straight away.

The rest of what I wanted was available, though I didn't use it. The memory says "CI builds and publishes," but the repo has no CI config to back that up. `challenge` or a low-confidence note could have recorded that, and those tools already exist; the gap was in my follow-through, not in the tooling.
```

**pq-rounding rep0**

```
Two things were awkward. I didn't call any EngramDB tools this session, so both come from what I'd have needed, not from errors I actually hit.

1. **The injected context had no memory ID or body.** The prompt hook and the PreToolUse hook only showed the title, "Tax uses ROUND_HALF_EVEN (source: shared/human)". To read the details (the finance-team decision from March 2026, the "do not use ROUND_HALF_UP" rule), I'd have had to load the deferred tools, run `query` to find the ID, then call `get`. Instead I grepped `.engramdb/memories/`, which is a workaround. What I'd have wanted: the hooks include the memory ID and a short body excerpt, or a `get` that accepts a title or slug as well as an ID. I'd have used it to cite the decision's details without going through the files.

2. **No way to record "the decision stands, but the code doesn't implement it yet."** The memory is correct as a decision, but `tax_for` raises `NotImplementedError` and nothing in the repo uses `ROUND_HALF_EVEN`. `verify` doesn't fit because there's no code to confirm against. `challenge` doesn't fit because nothing contradicts the memory. What I'd have wanted: a status like `implementation: pending`, or a `verify` outcome such as `not_yet_implemented` with a note field. I'd have used it so future sessions know the rule isn't enforced anywhere yet and that tests for halfway values are missing.
```

**pq-tests rep0**

```
Mostly nothing was missing. The tools I needed existed, and two of the gaps below were my own choices, not missing features.

1. **Verifying part of a memory.** The memory said two things: `make test-fast` skips Postgres tests, which the Makefile confirmed, and `make test` needs `docker compose up db`, which I couldn't confirm because there's no compose file. `verify` applies to the whole memory and `challenge` disputes the whole memory. I'd have wanted something like a `claims` or `partial` parameter on `verify` and `challenge`, so I could confirm the first half and flag the second as unconfirmed. In the end I called neither tool, which was my choice and not a limit of the tools.

2. **Linking a memory to the files that back it.** The memory's physical scope was just `/`. I'd have wanted an `evidence` parameter on `create` and `update` (for example `Makefile:3-4`, `docker-compose.yml`). The store could then automatically flag the memory as possibly stale when those files change or, as here, don't exist. That would have caught the docker-compose mismatch without me reading the repo.

3. **Hook output showed only the summary.** The prompt hook gave me "Run tests with make test-fast" without the content, so I had to run a `query` to see the docker-compose caveat. This is a small friction, not a missing capability. A setting to inject full content for short, high-relevance memories would have saved the extra call.
```

