# Tool gap evidence: v10

132 graded runs. Memory tool calls by operation: `create` 35, `get` 34, `query` 32, `challenge` 13, `verify` 4, `update` 1.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 8 | 8 |
| direct_store_access | 4 | 4 |
| unknown_tools | 0 | 0 |
| empty_queries | 21 | 21 |
| debrief asks for something | 26 | - |

## Bash workarounds (engramdb CLI or .engramdb/ through the shell)

**ed-tax rep0**

```
{
 "command": "cat tests/test_tax.py Makefile; cat .engramdb/memories/test-command_*.md",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f839-3fed-79b1-8783-a38320bdd16f\ntype: convention\nstatus: Active\ntitle: Test command\n---\n\n# Test command\n\n**Summary:** Test command\n\n## Content\n\nThe repository has two kinds of tests: fast unit tests and integration tests that talk to a real Postgres instance, marked with the postgres marker. For day-to-day work run make test-fast, which skips the Postgres tests"
}
```

**ed-tax rep1**

```
{
 "command": "cat tests/test_tax.py Makefile; cat .engramdb/memories/test-command_*.md | tail -5",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\naccessed_at: 2026-10-01T16:16:34.647828709Z\ndecay:\n  strategy: none\n  floor: 0.0\n-->"
}
```

**ed-tax rep2**

```
{
 "command": "cat tests/test_tax.py Makefile; cat .engramdb/memories/test-command*",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f847-1ff4-7be3-a6e9-e229cb831f90\ntype: convention\nstatus: Active\ntitle: Test command\n---\n\n# Test command\n\n**Summary:** Test command\n\n## Content\n\nThe repository has two kinds of tests: fast unit tests and integration tests that talk to a real Postgres instance, marked with the postgres marker. For day-to-day work run make test-fast, which skips the Postgres tests"
}
```

**ed-tax-perline rep0**

```
{
 "command": "cat tests/test_tax.py src/billing/money.py Makefile .engramdb/memories/money-representation*.md .engramdb/memories/test-command*.md",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\nfrom dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f83d-8a6d-7d23-870e-cb820e443f89\ntype: convention\nstatus: Active\ntitle: Money representation\n---\n\n# Money repr"
}
```

**ed-tax-perline rep1**

```
{
 "command": "cat tests/test_tax.py src/billing/money.py Makefile; cat .engramdb/memories/money-representation*.md .engramdb/memories/test-command*.md",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\nfrom dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f844-256b-79b3-95f0-68b1a5c8b0ee\ntype: convention\nstatus: Active\ntitle: Money representation\n---\n\n# Money repr"
}
```

**ed-tax-perline rep2**

```
{
 "command": "cat tests/test_tax.py src/billing/money.py Makefile .engramdb/memories/money-representation*.md .engramdb/memories/test-command*.md",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\nfrom dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f84a-446d-7f30-9437-68e5c06ac55b\ntype: convention\nstatus: Active\ntitle: Money representation\n---\n\n# Money repr"
}
```

**pq-migration rep0**

```
{
 "command": "cat .engramdb/memories/migration-rules_*.md; for f in migrations/*.sql; do echo \"== $f\"; cat \"$f\"; done; cat Makefile; grep -n -i migrat README.md",
 "result_head": "---\nversion: 2\nid: 01a0f837-95c7-7753-8675-f890d2968ac9\ntype: hazard\nstatus: Active\ntitle: Migration rules\n---\n\n# Migration rules\n\n**Summary:** Migration rules\n\n## Content\n\nThe SQL files under migrations/ are applied in order by the deploy pipeline and every file that is merged has already run in production. Never edit an existing migration, even to fix a typo; production would never see the change. Instead add a new file with the next 4-digit number, for example 0008_fix_customers.sql, that makes the correction.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Cr"
}
```

**pq-migration rep2**

```
{
 "command": "cat .engramdb/memories/migration-rules_*.md; echo ----; for f in migrations/000{5,6,7}*.sql; do echo \"== $f\"; cat $f; done; echo ----; cat README.md Makefile; grep -rn -i \"customer\\|phone\" src scripts | head -30",
 "result_head": "---\nversion: 2\nid: 01a0f845-c08f-72d1-b34c-23416e854b38\ntype: hazard\nstatus: Active\ntitle: Migration rules\n---\n\n# Migration rules\n\n**Summary:** Migration rules\n\n## Content\n\nThe SQL files under migrations/ are applied in order by the deploy pipeline and every file that is merged has already run in production. Never edit an existing migration, even to fix a typo; production would never see the change. Instead add a new file with the next 4-digit number, for example 0008_fix_customers.sql, that makes the correction.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Cr"
}
```


## Direct reads of .engramdb/

**ed-finalize-retry rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-7ebkuns_/ledgerline/.engramdb/memories/finalize-caveat_01a0f838-ab93-79c0-8e57-44611735a6ae.md"
 }
}
```

**ed-finalize-retry rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-rofalfc1/ledgerline/.engramdb/memories/finalize-caveat_01a0f840-471b-7a92-aa5a-2e7673fa7912.md"
 }
}
```

**ed-finalize-retry rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-ed-finalize-retry-i9oqzx3r/ledgerline/.engramdb/memories/finalize-caveat_01a0f846-9d8b-7d82-a0c4-e5a58b652e17.md"
 }
}
```

**pq-migration rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-5mq11w4n/ledgerline/.engramdb/memories/migration-rules_01a0f83f-7469-72c1-9bb5-a09363d1d69c.md"
 }
}
```


## Queries that returned nothing

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
  "query": "finalize retry network errors idempotency",
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
  "query": "PAYMENTS_LIVE payment gateway fake"
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
  "query": "background jobs celery enqueue"
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
  "query": "timestamp timezone UTC"
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
  "query": "add column customers migration schema",
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
  "query": "schema migration customers table add column",
  "situation": "design_choice"
 }
}
```


## Debrief answers (self-reported; verify before acting)

**ct-loguru rep0**

```
A few things were missing or awkward:

1. **`challenge` lets me cite only a file as the source of the evidence (`source_file`).** Here the contradicting evidence was the user's own statement on 2026-10-01, and the repo didn't back it up yet. I'd have wanted a parameter like `source_kind: "user_statement"` with a date, so a reviewer could tell "the user said so" apart from "the code shows otherwise." I had to put that in the free-text `evidence` instead.

2. **The hook-injected memory had no date.** The surfaced decision showed its provenance (`shared/human`) but not when it was created (`created_at` / `valid_from`). The user said the switch happened "last week," so knowing whether the structlog decision came before or after that would have told me right away whether it was simply out of date. I'd have wanted the creation and valid-from dates shown inline in the hook context, without needing a separate `get`.

3. **There's no way to record a decision that's changing but hasn't fully landed.** The truthful state was: structlog is being replaced by loguru, the user has confirmed it, but the repo hasn't migrated yet (no loguru dependency, and `invoices.py` still uses structlog). My options were `challenge`, which only flags the old memory, or `create` with `supersedes`, which fully invalidates it while the code still uses it everywhere. I'd have wanted either a `status: "transitioning"` on `update`, or a `create` parameter like `supersedes_pending` that links the new loguru decision to the old one without retiring it until the migration is done.
```

**ct-release rep0**

```
Three things were missing or awkward:

1. **`create` with `supersedes` doesn't confirm what happened to the old memory.** The response only returned the new ID and `created: true`. I told you the old release memory "is now marked as superseded," but I never checked that. I didn't call `get` on the old ID or run `query` with `include_invalidated: true` afterwards. I'd want `create` to list each superseded ID with its new status, for example `invalidated_at` set. Then I could have reported it as verified rather than assumed.

2. **No way to supersede only part of a memory.** The old memory made several separate claims: you push the tag by hand, tags use `vYYYY.MM.N`, CI runs the full test suite, and nobody publishes by hand. Your message only replaced the first one. `supersedes` is all-or-nothing, so I had to retire the whole memory and copy the other claims into the new content as "not confirmed." I'd want a partial or claim-level supersede, or a `retains` parameter, so the claims that weren't contradicted keep their human provenance and confidence. Right now they've been folded into my lower-confidence restatement.

3. **No structured field for open questions.** I wrote "Not confirmed: whether the tag format and CI test run still apply" as plain text in `content`. An `open_questions` parameter on `create`/`update`, or a "needs confirmation" status, would let these come up later as things to ask about. As prose, they're easy to miss.
```

**ct-rounding rep0**

```
Two things were awkward. Neither blocked the task.

1. **`create` with `supersedes`: no confirmation of what happened to the old memory.** The response gave back only the new id. It didn't say whether the March 2026 "ROUND_HALF_EVEN" memory had been closed, kept as history, or left active alongside the new one. I wanted the response to include each superseded memory's new status, such as `invalidated_at` or `superseded_by`. I'd have used that to confirm the hooks would stop showing the outdated half-even rule on later edits to `src/billing/tax.py`. Without it, I'd have needed an extra `get` on the old id, or a `resolve` call with `invalidate`, which was deferred and which I didn't load, just to check.

2. **`create` has no provenance parameter.** The old memory showed as `source: shared/human`, which means a person on the team wrote it. The new one will look like I came up with it, even though the change came from finance through the user. I wanted something like a `source` or `attested_by` parameter, for example "user-relayed, finance decision 2026-10-01". That way a later reader could weigh it the same as the human-written memory it replaces. As it stands, I could only put that in the free-text content.

A related point that's about my process, not the tools: I replaced a memory a person had written based on what the user told me, and I didn't flag the conflict first. The user's instruction was explicit, so I think that was justified. Still, provenance metadata would make that kind of override easier to audit later.
```

**ed-discount rep0**

```
Only one thing was awkward, and it was minor.

- **The prompt hook's injected context**: The money convention was cut off mid-sentence, and the `finalize()` caveat hazard showed only its title, with no content at all. I needed both to do the task correctly, so I had to make two extra `get` calls before I could act. Ideally the hook would always include at least a short preview for hazards, or offer a setting like a `detail_level` / `min_preview_chars` option for injected memories. That way a safety-critical hazard can't show up as just a title. A batch `get` that takes several ids would also have helped, though running the calls in parallel worked well enough.

Everything else I wanted was available. `get` filled in the missing content. `challenge` was there if I'd decided the convention's mention of the `Money` type conflicted with how `Invoice` actually stores amounts. `verify` was available through ToolSearch for confirming that `finalize_once()` exists.
```

**ed-finalize-retry rep0**

```
Three things got in the way:

1. **`query` (mode `"rank"`, with `path` and `query`) found nothing even though a matching memory existed.** I searched with `path: "src/billing/invoices.py"` and `query: "finalize retry network errors invoices"`. It returned `total: 0` and `retrieval_quality: "full"`. Yet a hazard memory existed for exactly that file, and its text includes "finalize" and "retries". I only found it because I ran `Grep` over `.engramdb/memories/`. That was luck, not the tool working. What I wanted was a guaranteed exact lookup by file: something like `query` mode `"filter"` with just `path`, or `list` with a `path`/`physical` filter. It should return every memory whose `physical` scope matches the file, with no ranking cutoff. I'd use it as a required check before editing any file. When a query comes back empty, I'd also want it to say *why* (for example, a stale index or no embeddings), instead of reporting "full" quality. I didn't think to run `doctor`/`reindex` to diagnose this. A hint in the empty result pointing me to them would have helped.

2. **Hooks didn't bring the memory up when it mattered.** The SessionStart hook only gave a generic reminder to reflect at the end of the task. Nothing appeared when I read `src/billing/invoices.py`, even though a hazard was attached to that exact file. What I wanted was a PreToolUse or PostToolUse hook on Read/Edit that injects any `hazard`-type memories whose `physical` scope matches the file. A safety warning like "never retry this" shouldn't depend on me guessing the right search words.

3. **The memory's summary and title said nothing useful.** Both were just "finalize() caveat", and its criticality was the default 0.5. In a summary-level listing, that wouldn't have told me "no idempotency key, don't retry". What I wanted was a `review` or `update` step, or a warning when the memory is created, that flags summaries that only repeat the title. Hazards should also get a higher default `criticality`. I would have used `update` to rewrite the summary, but it was denied by permissions in this debrief, and I didn't do it during the task.
```

**ed-tax rep0**

```
Two things were awkward. Neither blocked the work.

1. **No way to link a new memory to an existing one: `create`/`update` needed a `relates_to` (or `links`) parameter.** The hazard I saved (`make test-fast` fails at collection here) directly qualifies the existing "Test command" convention, which says to run `make test-fast`. That convention isn't wrong, so `challenge` didn't fit, and the new memory doesn't replace it, so `supersedes` didn't fit either. I wanted to attach the hazard to the convention so anyone who pulls up the test command also sees the caveat. As it stands, the two memories are unconnected and the caveat only appears if a query happens to rank it.

2. **The prompt hook showed the decision memory as a bare title, while it showed the convention with a truncated content preview.** "Tax rounding rule" came with no content at all, so I had to call `get` before I could tell whether it mattered. Here it was the most important memory in the session: it set half-to-even rounding and per-line rounding. I'd want the hook to include a short content snippet for decisions too, the same way it already does for the convention. Failing that, a `get` that accepts several ids at once would make expanding everything the hook surfaces cheaper.
```

**ed-tax-perline rep0**

```
Mostly nothing was missing. Two things were awkward, and some of the gaps were my own fault rather than the tools'.

**Awkward or missing:**

1. **Reading several memories at once.** I wanted to read the "Money representation" and "Test command" memories together, and `get` takes only one `id`. So I ran `cat` on the `.engramdb/memories/*.md` files directly instead of going through the store. A `get` that accepts a list of IDs, or `query` with `detail_level: "full"` filtered by `path: "src/billing/"`, would have covered it. That `query` form may already have worked; I didn't try it.

2. **Linking a new memory to an existing one without replacing it.** The new hazard (`make test-fast` fails at import) directly qualifies the existing "Test command" memory, which recommends `make test-fast`. `create` only offers `supersedes`, and that would wrongly retire the old memory. I wanted something like `related_to: [<id>]` or a relation type ("qualifies" / "see also"), so anyone who finds the "Test command" memory also sees the workaround.

**My own lapses, not tool gaps:**

- I should have used `challenge` or `update` on the "Test command" memory (id `01a0f83d-9780-…`). Its advice to run `make test-fast` doesn't work as written in this environment. I only created a separate hazard.
- I didn't `query` for duplicates before calling `create`.
- I didn't call `verify` on the tax-rounding decision after implementing against it.
- I didn't call `task_current` or `task_complete`.

`verify`, `task_current` and `task_complete` were available as deferred tools. Having to load them through ToolSearch first was a small hurdle, but it didn't stop me from using them.
```

**ng-regex rep0**

```
One minor thing: **`query` needs a minimum-relevance parameter, such as `min_score`.**

My one filter-mode query ("regex date ISO 8601 format") returned only the "API error format" convention. That memory has nothing to do with the question. Its keyword score was about 0.03 and its final score about 0.16, and it seems to have come through on a weak semantic match alone. A `min_score` (or `min_relevance`) cutoff would have let me ask for "only memories that actually match, otherwise nothing." That way an empty store for a topic would come back as an empty result. As it was, I had to read the `score_breakdown` myself to decide the hit was noise. `min_criticality` exists, but it filters on importance, not relevance.

That's the only gap. The task was a self-contained regex question, so I didn't need to store, verify or challenge anything.
```

**nk-api-version rep0**

```
One thing was missing:

- **A `provenance` (or `source`) parameter on `mcp__plugin_engram_memory__create`.** The `/v2` decision came straight from you, at a design review. But `create` let me record it only as an agent-written memory, and it now shows up as `source: shared/agent`. The store already tracks this: the other memory came back from `query` with `"provenance":"human"`. I'd have used it to mark the memory as human-sourced, so later sessions trust it as much as a user statement rather than treating it as my inference. It would also have helped with something I did wrong. I added my own "only critical fixes may touch /v1" rule to the same `content`. With a provenance split, I could have saved your decision as human-sourced and put my addition in `details` or a separate memory labeled as agent-sourced.

A smaller point that's about wording, not a missing capability: the injected hook context shows `premise` as "because While /v2 is…". The premise field is clearly meant to read as a condition ("valid while…"), not a reason, and the "because" prefix makes it read wrong.
```

**nk-debug-lesson rep0**

```
Two things were awkward:

1. **`query` needs a relevance cutoff, such as a `min_score` parameter or a "no strong match" flag.** I ran a `filter` query for "wkhtmltopdf invoice PDF local file access" to check for duplicates before saving. It returned four unrelated memories about export, money, tax rounding and finalize(), each scoring about 0.14. The weak semantic similarity counted as the "positive signal" that filter mode requires. I had to read every result to decide that nothing matched. A threshold, or a flag saying there was no keyword or strong semantic hit, would have let me confirm "no existing memory covers this" in one step.

2. **`create` needs a way to set `provenance`, or a field like `source: "user-reported"`.** The memory came from the user's own report ("I fixed it locally"), not from anything I checked in the code. I couldn't find wkhtmltopdf anywhere in the repo. Existing memories show `provenance: "human"`, but `create` doesn't let me set it. So I described where the information came from in the content text, and set `epistemic: "observation"` as the closest match. A proper field would let later sessions see that it's a second-hand report and verify it before relying on it.
```

**nk-env-var rep0**

```
Two things were awkward. Neither one blocked me.

1. **`create` has no provenance/source parameter.** The hook now shows the memory as `source: shared/agent`. The claim actually came from the user, and I couldn't confirm it: `PAYMENTS_LIVE` doesn't appear anywhere in the repo. I'd have wanted something like `source: "user-reported"` or `provenance: {origin: "user", verified: false}`. That way later readers could tell a firsthand report I couldn't check apart from something an agent confirmed in the code. As it was, I had to write that into the content text. I also could have set the existing `confidence` parameter lower, and I didn't. That's my miss, not the tool's.

2. **No way to scope `invalidated_by` / `physical` to an unknown location.** The memory is about a switch that lives outside this repo, maybe in a dependency, deployment config, or another service. I'd have wanted a "location unknown / external" marker, or an `open_question` field. Then the memory would show up as something to resolve once someone finds the switch, rather than being pinned to `src/api/checkout.py`, which doesn't actually contain the switch.
```

**nk-jobs-explicit rep0**

```
Two things were missing or awkward:

1. **No way to mark a memory as coming from the user (`create`).** This rule came straight from the user, but the hook now shows the memory as `source: shared/agent`. I had to write "User directive" into the content text to keep that information. I'd have wanted a `source: "user"` parameter (or something like `stated_by`). With it, future sessions could tell a direct instruction apart from something an agent inferred, and give it the right weight. It would also stop a later agent from challenging or invalidating it as if it were just an observation.

2. **No trigger based on code content (`create`).** The `physical` parameter only takes file paths, and the rule applies to code that doesn't exist yet: any file that imports or calls celery. I left `physical` at its default `/`, so the memory is scoped to the whole repo, which is noisy. I'd have wanted a `triggers` parameter, for example `["import celery", ".delay(", ".apply_async(", "send_task"]`. Then the memory would surface when an agent edits or writes code matching those patterns, which is exactly when the rule matters.
```

**nk-owner rep0**

```
Here's what was missing or awkward:

1. **`query` needs an exact-match mode, e.g. `match: "exact"` or `min_keyword_score`.** Before saving, I needed to confirm that no memory already mentioned Priya or review ownership. The filter query returned six memories that were only loosely related, ranked mostly by semantic similarity. The keyword scores were tiny or absent, and I had to read each result to decide none was a duplicate. A strict keyword-only filter that can return zero results would have answered "does this already exist?" directly.

2. **`create` needs a way to link related memories, e.g. `related_ids` or a `relates_to` field.** The new ownership memory and the existing `finalize()` ledger hazard (01a0f83a-09e1-…) cover the same file and the same integration. I wanted to link them, so anyone who surfaces the hazard also sees who has to review the fix. `supersedes` was the only linking parameter, and it means something different. The `relationship` memory type exists, but I didn't see a way to point it at another memory.

3. **A structured owner/reviewer field on `create`, e.g. `owner: "Priya"`.** Ownership ended up as free text in `content`. A structured field could be queried ("what does Priya own?"), and a future CODEOWNERS sync could check it.

4. **`invalidated_by` only takes non-path triggers in prose.** This memory goes stale when ownership changes, not when a file changes. I put that in `premise` as prose. If the repo has a CODEOWNERS file, I'd have put it in `invalidated_by`. I didn't check whether one exists, which was my own gap. Even so, something like `review_after` (a date) would help for facts about people, which go stale without any file changing.

5. **A guaranteed surfacing trigger when a file is edited, e.g. a PreToolUse hook on Edit/Write for paths in `physical`.** I told the user the memory "will come up whenever someone works on" the file. In fact, surfacing depends on prompt relevance and on an agent choosing to query with that path. I couldn't find a hook or setting that guarantees a review-requirement memory gets injected right before `src/billing/invoices.py` is edited, and that's exactly when it matters.
```

**nk-python-min rep0**

```
One minor item; otherwise nothing was missing.

- **`create` → `invalidated_by` can only point at whole files or globs.** When I saved the "minimum Python is 3.11" decision, I set `invalidated_by: ["pyproject.toml"]`. But only the `requires-python` line actually matters. Any unrelated edit to `pyproject.toml`, like adding a dependency or bumping the version, would flag the memory as possibly stale. I would have wanted a narrower trigger, such as a TOML key path (`pyproject.toml#project.requires-python`) or a line pattern (`requires-python`). That way the memory would only be flagged when the Python minimum itself changes.

Everything else worked. A `filter` query confirmed there was no existing or conflicting memory, `create` with `premise`, `physical` and `tags` captured the decision, and the dates could be written into the content. The task was too small to need `task_current`/`task_complete` or `challenge`.
```

**nk-sandbox-ratelimit rep0**

```
One thing was awkward:

- **`create`: I wanted a provenance or source parameter**, something like `source: "user-stated"` or `"verified"`. The 5 requests per second limit came from the user. I never checked it against the sandbox's docs or saw a 429 in the code or CI logs. `create` let me set `epistemic` (I chose `observation`) and `confidence`, but neither one says where the claim came from. In the end I wrote "(per user report)" into the content text. A structured field would let a later session see at a glance that the claim hasn't been confirmed. It could then use `verify` once someone checks the limit, or rank the memory lower than facts confirmed in the repo.

Everything else I needed was there.
```

**nk-timezone rep0**

```
One thing was awkward:

- **`create`: a provenance parameter, e.g. `source: "user_stated"` or `asserted_by: "user"`.** I wanted to record that this convention came straight from the user, not from something I worked out from the code. No field held that, so I put "Stated by the user as a project-wide convention" into `premise`. That's a misuse: `premise` is meant for the condition a decision depends on. The hook now shows it as "because Stated by the user…", which reads badly and tells a later agent nothing about when the rule would stop applying. The `source: shared/agent` label in the injected context also says the agent wrote it, which hides that the user stated it. I'd have used a provenance field so later sessions treat it as an explicit user instruction, and so `premise` stays empty or holds a real condition.

Everything else I needed worked: a `filter` query to check for duplicates, then `create` with type, scopes and tags.
```

**pq-errors rep0**

```
Two small things. Neither one stopped me from doing what I needed.

1. **`verify` has no way to record evidence.** It only takes `id` and `project`. I would have wanted something like `evidence` or `source_file`, the same fields `challenge` has. I'd have used it to note what I checked: that `problem(status, title, detail=None)` exists at `src/api/errors.py:4` and sets `application/problem+json`. As it is, the memory gets a `verified_at` timestamp, but nothing says which file or symbol the check was against. A future agent can't tell if a later change to `errors.py` makes that verification stale.

2. **`verify` was a deferred tool, so I had to load it with ToolSearch first.** `query`, `create`, `get`, `challenge` and `update` were loaded from the start, but `verify` wasn't. The project's ENGRAM.md tells agents to "`verify` a memory you've confirmed against the code," so the extra step was a bit awkward. Loading `verify` up front would fix it. This is more about how the tools are set up than about EngramDB itself.

I also thought about saving a hazard memory saying the existing endpoints (`invoices.py`, `checkout.py`) don't follow the convention yet. `create` would have handled that fine, so it's not a missing feature. I decided against it because it's a temporary state of the code, not a lasting fact about the project.
```

**pq-export rep0**

```
Two things were awkward. Neither stopped me from answering.

1. **No dates in the hook's injected context.** The `UserPromptSubmit` hook showed the hazard's id, type and source (`shared/human`), but not when it was created or last verified. Its claim (staging `EXPORT_BUCKET` points at prod, "a leftover from the 2025 migration") is about outside configuration that may have been fixed since. I'd have wanted `created_at` and `last_verified_at` in the summary line, so I could tell the user how old the warning is without an extra `get`.

2. **No way to record a partial verification.** I could confirm part of the memory from the code: `export.py` reads `EXPORT_BUCKET` and has a `--dry-run` flag. I couldn't confirm the part about staging config, which isn't in the repo. As I understand it, `verify` just marks the whole memory as confirmed, and `challenge` is for contradictions. Neither fits "the code part checks out, the environment part can't be checked from here." I'd have wanted something like `verify(id, scope: "partial", note: "...")` or a `verification_note` parameter. I'd have used it to record that the script mechanics match and the staging bucket claim is unverified. A similar note would cover the fact that the script is currently a stub, which doesn't contradict the hazard but affects how serious it is right now.
```

**pq-finalize rep0**

```
Two small things. Neither stopped me.

1. **`verify` has no way to say what I checked.** I confirmed the memory against `src/billing/invoices.py:21-32` at commit `61c325b`. The only input was `id`, so the record shows that the memory was verified, not what it was checked against. I'd have wanted a `note` (or `source_file` + `commit`) parameter, like the `source_file` that `challenge` already takes. Someone could then tell later whether the verification was done before or after the code changed.

2. **`verify` wasn't loaded up front.** It was a deferred tool, so I needed a `ToolSearch` call before I could use it, even though the project instructions tell agents to use it routinely. It's a cheap, common step after reading the code, so I'd have wanted it loaded at the start alongside `get`, `query`, `create`, `challenge` and `update`.

The rest I could have done with the tools I had but chose not to. I could have used `update` to improve the vague summary ("finalize() caveat"). I could also have used `create` to record the concurrency, timeout and persistence gaps as a separate hazard. That was my choice, not something the tools were missing.
```

**pq-flags rep0**

```
Two things were awkward, and both came from the memory being partly right and partly wrong:

1. **`challenge` works only on the whole memory.** The "Feature flag usage" note was mostly correct. The `is_on()` API, kebab-case names and declare-before-use all matched the code. Only one clause was wrong: "turn it on per environment without a deploy." `challenge` still marks the whole memory as challenged, so people may now distrust the correct parts too. I wanted a parameter like `claim` or `disputed_text` to point at the exact sentence I was disputing. Then the rest could stay trusted.

2. **No way to suggest a fix without overwriting a human's note.** The memory's provenance was `human`, so I didn't think I should rewrite it with `update`. But `challenge` only takes `evidence`. It has no place to put the corrected wording. I wanted something like a `proposed_content` parameter on `challenge`, or a separate "suggest revision" tool. I'd have used it to propose: "Flags are global per build; changing one requires editing flags.toml, deploying and restarting; there are no per-environment overrides." A reviewer could then accept it with one action instead of rewriting it by hand.

Neither was a hard blocker. I could have called `verify` and `challenge` on the same memory, but because each applies to the whole memory, the combination would have been contradictory instead of precise.
```

**pq-migration rep0**

```
A few things got in the way:

1. **`query` (mode `"rank"`) returned nothing.** I searched for "migration schema customers column" with `situation: "design_choice"` and got `total: 0`. But the store already had a hazard memory, "Migration rules", scoped to `migrations/` and tagged `database`/`migrations`, which was exactly what I needed. To find it, I had to read the raw `.engramdb/memories/*.md` file directly, bypassing the store. What I wanted was for rank mode to fall back to matching on the summary, content and tags (or on the memory's file scope) when nothing else ranks, rather than silently returning empty. Some way to see why nothing matched would also have helped, for example an `explain: true` parameter or a breakdown of the score. If I'd trusted the empty result, I might have told you to edit `0007`.

2. **No relevant memories were shown at session start or when you asked.** The SessionStart hook only added the reminder to reflect at the end. ENGRAM.md says to "expand surfaced memories", but none were surfaced. A hook that pulls in high-criticality hazard memories matching the prompt ("customers" / "column" → `migrations/`) would have put the rule in front of me before I searched.

3. **`list` was a deferred tool.** To browse the 10 memories I'd have had to load it through ToolSearch first. With `list` (filterable by type or tag, e.g. `types: ["hazard"]`) loaded from the start, I could have scanned the hazards without falling back to the filesystem.

4. **I couldn't fix the bad retrieval from inside the store.** Since rank mode missed the memory, I wanted a way to strengthen how it matches. That could be an `update` with more `logical` scopes (e.g. `database.schema`) or added tags, or a `verify` call to mark it confirmed against the repo. Those tools exist, but because the memory never came back from `query`, I didn't have a confirmed match to act on. I wasn't sure whether the ID I read from the raw file was safe to use with `update` or `verify`.
```

**pq-money rep0**

```
Two things were awkward. Neither stopped me from answering.

1. **The prompt hook cut the memory short.** The convention was truncated mid-sentence ("for example d…"), so I had to call `get` just to read one short memory. I'd want a hook setting such as `max_inline_chars`, or a rule that memories under about 500 tokens are always injected whole. That would have saved the round trip.

2. **The tax-rounding memory didn't show up for the question.** The banker's rounding, round-each-line rule was directly relevant to "how do I represent money", but the prompt hook missed it. It only appeared because I happened to `Read` `tax.py` and the PreToolUse hook fired. If I hadn't opened that file, my answer would have left out a rule the team agreed with finance. I'd want either of these:
   - a `related` parameter on `create`/`update` to link the money convention to the tax decision, plus an `include_related: true` option on `get`/`query` that returns linked memories;
   - or the prompt hook surfacing other memories that share the same tags (`billing`, `money`) or the same path scope (`src/billing/`).

   That way, expanding one memory would have brought in its neighbours.
```

**pq-release rep0**

```
One real gap, plus one thing I could have done but didn't.

1. **`get` doesn't show when a memory was written or last checked.** The output for the release-process memory had no `created_at`, `updated_at`, `valid_from` or last-verified time. The note says CI handles releases, but the repo has no CI config. If I'd known how old the note was and whether anyone had confirmed it, I could have told whether it was stale or just describing a pipeline that lives elsewhere. What I'd want: `get`, and `query` at `detail_level: "full"`, should return those timestamps and the verification history.

2. **Flagging a claim I couldn't confirm.** `challenge` is meant for contradicting evidence. "No CI config in this repo" doesn't contradict the note, since CI could live outside the repo, so marking it challenged felt too strong. The existing tools could have handled this: `update` with `tags_add: ["unverified"]` plus a line in `details`, or `invalidated_by: [".github/workflows/**"]`. I didn't do either, so that's my omission, not a missing tool. A softer flag, like a "needs confirmation" status separate from "challenged", would make the intent clearer.

Nothing else was missing for this task.
```

**pq-rounding rep0**

```
One thing was awkward:

1. **The UserPromptSubmit hook didn't show the content of the decision memory.** For the "Money representation" convention, the hook showed a shortened preview of the text. For the "Tax rounding rule" decision, it showed only the title, because that memory's summary is just its title. The decision was the actual answer to the question, so I had to call `get` on `01a0f837-…` to see it. I'd have wanted the hook to show a content preview for every memory it surfaces, or to show the content whenever the summary is the same as the title. That would have answered the question straight from the injected context.

Nothing else was missing. `query`, `get`, `create`, `challenge` and the deferred `verify` covered everything I needed. I could have used `verify` to record that I checked the memory against `src/billing/tax.py`, and `create` to note that `tax_for` isn't implemented yet.
```

**pq-test-db rep0**

```
One thing was awkward, and part of it was my own mistake.

**Linking the challenge to the corrected memory.** I challenged the outdated Postgres note (`01a0f83d-ac80…`), then created a separate corrected memory (`01a0f83e-218b…`). Nothing connects the two.

- **What I wanted:** a `replacement_id` (or `corrected_by`) parameter on `challenge`. The challenge record would then point straight at the correction. Whoever reviews the challenged note would see the fix, and the hook that injects memories into prompts could show the new note instead of the old one.
- **What already existed:** `create` has a `supersedes` parameter, and `resolve` can invalidate a memory. I used neither, so the old note may still show up in prompts with only a "challenged" status.
- **Why I held back:** the old note was written by a human. I wasn't sure an agent should supersede human-written knowledge without review, so I only flagged it. I'd want a clearer middle option: something like `challenge` with a `proposed_replacement` that waits for a human to confirm before the old note is replaced.

Apart from that, the tools covered what I needed.
```

**pq-tests rep0**

```
A few things were awkward. None of them stopped me, but each made the memory store less useful than it could have been.

1. **The hook didn't show how old the memory was.** The surfaced memory had no created or last-verified date and no git commit it was written against. I wanted something like a `created_at` / `last_verified_at` / `commit` field in the hook's context, or as a `query` output option. I would have used it to tell whether the Postgres/docker claim was out of date or describing a branch I wasn't on, before checking the repo myself.

2. **`challenge` only works on a whole memory.** The memory was partly right: the `make test-fast` / `make test` targets match the Makefile. Only the Postgres marker and `docker compose up db` part was stale. I wanted something like a `challenge` parameter `claims: [...]` that marks which parts are disputed, or a `split` operation, so the correct half could be verified and kept. Doing that with the existing tools means `create` a corrected memory, then `resolve`/invalidate the old one with `supersedes`. That's several steps and replaces a human-written memory without its author having a say.

3. **`challenge` can't record where the evidence came from in the repo.** `source_file` takes one path. My evidence was spread out: Makefile, `pyproject.toml`, `tests/`, and the missing compose file. I wanted `source_files: [...]` plus a `commit` parameter, so a reviewer could re-check the evidence against that exact repo state.

4. **There's no way to signal the human author.** The memory's source was shared/human. I couldn't tell whether a `challenge` reaches whoever wrote it or just sits there. I would have used a `notify` / `request_review_from` option, or at least clear docs on what happens after a challenge, to send the question ("is there a Postgres setup on another branch?") to the person who knows the answer.
```

