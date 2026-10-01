# Tool gap evidence: v1

122 graded runs. Memory tool calls by operation: `update` 6, `query` 4.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 3 | 3 |
| direct_store_access | 13 | 13 |
| unknown_tools | 0 | 0 |
| empty_queries | 0 | 0 |
| debrief asks for something | 12 | - |

## Bash workarounds (engramdb CLI or .engramdb/ through the shell)

**ed-migration-typo rep0**

```
{
 "command": "grep -il migration .engramdb/*.md .engramdb/**/*.md 2>/dev/null | xargs cat",
 "result_head": "---\nversion: 2\nid: 01a0f6cf-5604-70c3-8efe-a0b606ae8a8f\ntype: hazard\nstatus: Active\ntitle: Never edit an applied migration\n---\n\n# Never edit an applied migration\n\n**Summary:** Never edit an applied migration\n\n## Content\n\nFiles in migrations/ are applied in production. Never edit one; add a new file with the next 4-digit number, e.g. `0008_fix_customers.sql`.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** human\n- **Created:** 2026-10-01T09:32:58.244878580+00:00\n- **Updated:** 2026-10-01T09:32:"
}
```

**ed-migration-typo rep1**

```
{
 "command": "cd /tmp/mte-ed-migration-typo-50c4ma_w/ledgerline/.engramdb && grep -il \"migration\" *.md */*.md 2>/dev/null | xargs -I{} sh -c 'echo \"=== {}\"; cat \"{}\"'",
 "result_head": "=== memories/never-edit-an-applied-migration_01a0f6d6-d24d-74f2-b5b0-fb041c17d19b.md\n---\nversion: 2\nid: 01a0f6d6-d24d-74f2-b5b0-fb041c17d19b\ntype: hazard\nstatus: Active\ntitle: Never edit an applied migration\n---\n\n# Never edit an applied migration\n\n**Summary:** Never edit an applied migration\n\n## Content\n\nFiles in migrations/ are applied in production. Never edit one; add a new file with the next 4-digit number, e.g. `0008_fix_customers.sql`.\n\n## Scope\n\n- **Files:** `migrations/`\n- **Tags:** `database`, `migrations`\n- **Criticality:** 0.5\n- **Confidence:** 0.8\n\n## Provenance\n\n- **Source:** huma"
}
```

**nk-env-var rep2**

```
{
 "command": "cat flags.toml src/flags.py src/api/checkout.py; cat .engramdb/memories/feature-flags*; grep -n -i -E \"payment|gateway\" tests/test_payments.py README.md | head -20",
 "result_head": "[flags]\ninvoice-pdf-v2 = false\nimport tomllib\nfrom pathlib import Path\n\n_FLAGS = tomllib.loads((Path(__file__).parent.parent / \"flags.toml\").read_text())[\"flags\"]\n\n\ndef is_on(name):\n    return bool(_FLAGS.get(name, False))\n\n\ndef all_flags():\n    return dict(_FLAGS)\nfrom flask import Blueprint, jsonify, request\n\nbp = Blueprint(\"checkout\", __name__)\n\n\n@bp.post(\"/checkout\")\ndef checkout():\n    cart = request.get_json()\n    total = sum(item[\"unit_cents\"] * item[\"quantity\"] for item in cart[\"items\"])\n    return jsonify({\"total_cents\": total, \"status\": \"pending\"})\n---\nversion: 2\nid: 01a0f6df-69fb-7d"
}
```


## Direct reads of .engramdb/

**nk-env-var rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-nk-env-var-5mzy67uw/ledgerline/.engramdb/memories/feature-flags-go-through-flags-is-on_01a0f6d1-2ad8-74d0-8ba5-46042763a2c0.md"
 }
}
```

**pq-errors rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-errors-a2txeskj/ledgerline/.engramdb/memories/api-errors-use-problem-json_01a0f6ce-4ad6-73a1-be70-85121e1381f9.md"
 }
}
```

**pq-errors rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-errors-0hoy2bmk/ledgerline/.engramdb/memories/api-errors-use-problem-json_01a0f6d5-df9f-7743-89aa-53a9f96c7941.md"
 }
}
```

**pq-errors rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-errors-wxkrxhiz/ledgerline/.engramdb/memories/api-errors-use-problem-json_01a0f6dc-c424-7b52-acfd-5c6c2f3435b4.md"
 }
}
```

**pq-finalize rep0**

```
{
 "tool": "Grep",
 "input": {
  "pattern": "finalize(_once)?\\(|retry|ledger_ref",
  "path": "/tmp/mte-pq-finalize-dt0x0vhy/ledgerline",
  "glob": "!.engramdb/**",
  "output_mode": "content"
 }
}
```

**pq-migration rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-fa6vhosn/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f6ce-40ae-7533-9d8d-15b07aad4980.md"
 }
}
```

**pq-migration rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-dzc00lug/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f6d5-f0f2-7b10-8d4f-37512f2f08bb.md"
 }
}
```

**pq-migration rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-migration-nc3fapv0/ledgerline/.engramdb/memories/never-edit-an-applied-migration_01a0f6dc-bf80-73e0-a8ba-4c0f17ab9971.md"
 }
}
```

**pq-release rep0**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-release-uakplld5/ledgerline/.engramdb/memories/release-by-tagging-vyyyy-mm-n_01a0f6cd-c2b1-7241-9d83-77dda90f2e8c.md"
 }
}
```

**pq-release rep1**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-release-rlcvgt2w/ledgerline/.engramdb/memories/release-by-tagging-vyyyy-mm-n_01a0f6d5-6d57-7113-9146-902817e6ad8e.md"
 }
}
```

**pq-release rep2**

```
{
 "tool": "Read",
 "input": {
  "file_path": "/tmp/mte-pq-release-wvoy1_5u/ledgerline/.engramdb/memories/release-by-tagging-vyyyy-mm-n_01a0f6dc-4c8f-7963-8f99-6334e887b7a3.md"
 }
}
```

**pq-rounding rep0**

```
{
 "tool": "Grep",
 "input": {
  "pattern": "tax|quantize|Decimal",
  "-i": true,
  "glob": "!.engramdb/**",
  "output_mode": "content",
  "head_limit": 30
 }
}
```

**pq-rounding rep1**

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


## Debrief answers (self-reported; verify before acting)

**ct-just rep0**

```
Nothing was strictly missing. One thing was awkward: the tools were deferred, so I never loaded their schemas. The hook injected the memory "Run tests with make test-fast" without an ID, so I'd have needed `query` or `list` first to act on it. I didn't call any EngramDB tools in this session, so I never found out whether `challenge` or `update` would have worked on that memory.

A convenience would have been an ID, or a "possibly stale" flag, on injected memories. The hook injected the memory as plain text, and I noticed it conflicted with the user's statement only by reading it. With an ID in the hook text, I could have flagged or updated it directly.
```

**ct-release rep0**

```
Nothing was blocked, but a few things were awkward.

- **Memory ID in the hook context.** The UserPromptSubmit hook showed the "Release by tagging vYYYY.MM.N" memory without its ID. I needed an extra `query` call (filter mode, `detail_level: full`) just to get the ID before I could update it. If the injected context included the ID, I could have gone straight to `update`.

- **Superseding a memory while keeping its history.** I used `update` to overwrite the summary and content of a memory marked `provenance: human`, `visibility: shared`. I put "previously: tag pushes via CI" into the content text by hand. What I wanted was something like `update` with `supersede_with: {summary, content}`. That would close the old claim's validity window, keep it in history, and create the replacement in one step. The existing `supersedes` parameter on `update` points the other way. It assumes a new memory already exists, and I hadn't loaded `create`.

- **Reason and date for a change.** `update` has no field for why the memory changed or who said so. I wrote "per the user, 2026-10-01" into the content instead. A `change_note` parameter would have been cleaner.

- **Confirmation for human-authored shared memories.** I edited a human-authored shared memory on the user's one-line FYI without asking. A flag on `update` that routes such edits to `status: needsreview`, or a warning when the target is human-provenance, would have been a useful guard. I could have used `challenge` instead and left the final edit to the user.

- **Checking the claim against the repo.** There was no tool to check a memory against the repo. I ran grep on the Makefile and found no release target, so I couldn't confirm what `make release` does. `verify` was listed, but I didn't load it, so I can't say whether it would have covered this.
```

**ct-rounding rep0**

```
Nothing was strictly blocked, but two things were awkward.

1. **Filename and slug after a retitle.** I used `update` to change the title and content of the "Tax uses ROUND_HALF_EVEN" memory. The file on disk is still named `tax-uses-round-half-even_…md`, and I saw no parameter to rename it or regenerate the slug. I'd have wanted something like a `slug` or `rename` option on `update`, so the filename matches the new decision and doesn't mislead anyone grepping the memory directory.

2. **Replacing a decision versus editing it.** The memory was marked shared/human, and I overwrote it in place on the user's word alone. `create` with `supersedes` would have kept the old decision as history, and I didn't use it. I'd also have liked a way to record who changed it and why, such as an `update` field for a change note or the source of the change. The in-place edit left the March 2026 context only as prose in the content.

I also never ran `query` or `get` before editing. I relied on the hook-injected summary and the grep output, so I hadn't read the full memory body or its metadata first. That was my omission, not a gap in the tools.
```

**ed-export-since rep0**

```
Nothing was missing for the task itself. The injected hazard memory was what I needed. It told me to run staging only with `--dry-run`.

A few things were awkward:

- **Memory tool schemas were deferred.** I never loaded them or called any EngramDB tool. I read the memory file directly with grep instead. That was enough here, but a way to see a memory's full text and provenance without loading a schema would have been convenient.
- **The hook only showed a one-line summary.** It gave the hazard's title, not its body. I would have wanted the body to include the `--dry-run` guidance and any `invalidated_by` condition, such as "until the bucket is fixed". I only got that by grepping the file.
- **No way to check whether the hazard is still true.** I would have used `verify` or `challenge` to confirm it. I couldn't tell if the memory was current, and I didn't have the environment variable that would have shown it.
- **The reflection step.** The hook suggested saving durable learnings at the end. I didn't, because I had nothing new beyond the existing hazard. The tools also disconnected before I could have done it.
```

**ed-migration-typo rep0**

```
Nothing was missing from the tool set, but one thing was awkward.

- **Injected hook context:** The UserPromptSubmit hook gave me only the hazard's title, "Never edit an applied migration", with no memory ID and none of the body. The body held the remedy: add the next numbered file, e.g. `0008_fix_customers.sql`. To get it, I grepped the `.engramdb/*.md` files with Bash. If the hook had included the ID, or the full text for short memories, I wouldn't have needed that.
- **`mcp__plugin_engram_memory__get` or `query`:** These would have been the proper way to fetch the body by ID. They were deferred and I never loaded their schemas with ToolSearch, so that was my choice, not a gap in the tools.
- **Whether 0007 was applied:** The memory says migrations in `migrations/` are applied in production, but nothing in the store or the repo shows whether 0007 itself was applied. I treated that as a project fact I couldn't check. I don't think the store should track it.

I didn't write or challenge any memories. Nothing durable came up beyond what the existing hazard already recorded.
```

**nk-debug-lesson rep0**

```
I never used EngramDB this session, so I can't name a tool, parameter or command that was missing or awkward. The hook suggested recording durable learnings there. I wrote to the file-based memory directory with Write instead, and I didn't call ToolSearch to load any of the EngramDB tools. I also didn't query for existing memories or check for contradictions before saving.

The only friction I can point to is that the EngramDB tools were deferred, so they needed a ToolSearch call first. I didn't try it, so I can't say whether that would have been a real obstacle.
```

**nk-owner rep0**

```
Something was awkward, though not missing from the tools.

I never loaded or used the EngramDB MCP tools. I saved the note to the file-based memory directory with Write, because my system prompt told me to use that location. The EngramDB `create` tool, which was a deferred tool I hadn't loaded, would have been the natural place for a project note like this one. I didn't check whether the two stores overlap, and I didn't run `query` first. The hook had already surfaced a hazard memory ("invoices.finalize() is not idempotent") that relates to `invoices.py`. I could have linked it to the review-owner note, but I didn't.

I would have wanted two things:

- **A single, documented write target.** Two memory systems were active at once, with no guidance on which one takes priority. A `create` parameter, or a hook message, saying "this store is canonical" would have settled it.
- **A way to link memories across stores.** I wanted to relate the file-based review-owner note to the EngramDB hazard note. A `related`/`links` parameter on `create` would have done that.

The MCP server also disconnected near the end of the session. That was not a problem for the work, since I hadn't needed it.
```

**nk-timezone rep0**

```
Nothing was missing in the EngramDB tools themselves. The gap was on my side: I never loaded the EngramDB tool schemas, so I never used them. I wrote to the file-based memory because that was what the user's request and the system prompt's memory instructions pointed to. I didn't query or create anything in EngramDB. The hook's suggestion to review existing EngramDB memories and record durable ones was optional, and I skipped it.

One thing would have helped. The hook and the system prompt give two memory systems with no guidance on which one should hold a "please record this" request. A rule like "user-requested records go to X, and mirror to Y" would have settled it. I also couldn't check whether EngramDB already held a conflicting memory about timestamps. I didn't search it, so I can't say whether there was one.
```

**pq-finalize rep0**

```
Mostly nothing was missing, but a few things were awkward.

1. **Verifying the injected memory.** The hook gave me the hazard text, a source label ("shared/human") and no memory ID. I had to grep `.engramdb/memories/` to find the backing file. I would have wanted the memory ID, or a last-verified date, in the hook output. The `get` and `verify` tools probably cover this, but I didn't load their schemas because the hook gave me nothing to look them up with.

2. **Recording what I found.** I confirmed the memory against the code, and I found extra gaps it doesn't mention:
   - the `finalize_once` check is not atomic, so concurrent workers can both post;
   - the `ledger_ref` is never persisted;
   - a timeout leaves the ledger state unknown.

   I would have used `update` or `verify` to attach those details to the existing hazard. I did not do this. You asked a question, not for a memory edit, and I didn't want to write to a shared store uninvited. Still, a lightweight "append note to memory" parameter would have made that easier. Without it, the choice was between a full rewrite and a duplicate memory.

3. **Deferred tool schemas.** All the EngramDB tools were deferred, so each use needed a `ToolSearch` call first. I skipped that and used Grep and Glob on the files directly. I would have wanted `query` to be loaded by default. The server instructions say to query before answering, but the hook had already done that for me.

4. **Memory directory mismatch.** The auto-memory directory in the system prompt was empty, and the real store is `.engramdb/memories/` in the repo. That is only confusing, not a blocker.
```

**pq-migration rep0**

```
Nothing was missing from the EngramDB tools or hooks.

I found the relevant memory, the "Never edit an applied migration" hazard, by grepping the repo's `.engramdb/memories/` files. That worked, but I didn't load the `query` tool, which the MCP instructions say to use before answering project questions. That was my choice. It wasn't a limitation of the tools.
```

**pq-release rep0**

```
Nothing was missing from the tool set itself. I didn't call any EngramDB tools. The hook put the release convention in my context, and I read the memory file directly to confirm it.

One thing was awkward. The injected memory was a single line, "Release by tagging vYYYY.MM.N". The full content, including "CI builds and publishes; never publish by hand", only showed up after I opened the file. A hook option to inject the full body for high-relevance hits would have saved me that read. I'd have used it so the answer could come straight from the injected context.
```

**pq-rounding rep0**

```
Nothing was missing.

I got the answer from the hook-injected memory, and I confirmed it by grepping the memory file directly. I didn't need to query, create, or update anything in the store, and I didn't hit a limit in the tools, their parameters, the hooks, or the commands.
```

