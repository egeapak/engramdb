# Tool gap evidence: baseline

5 graded runs. Memory tool calls by operation: none.

| signal | runs with it | occurrences |
|---|---|---|
| mcp_errors | 0 | 0 |
| bash_workarounds | 1 | 1 |
| direct_store_access | 0 | 0 |
| unknown_tools | 0 | 0 |
| empty_queries | 0 | 0 |
| debrief asks for something | 3 | - |

## Bash workarounds (engramdb CLI or .engramdb/ through the shell)

**ed-tax rep0**

```
{
 "command": "cat tests/test_tax.py src/billing/money.py Makefile .engramdb/memories/tax-uses* .engramdb/memories/money-is* .engramdb/memories/run-tests*",
 "result_head": "from src.billing.tax import line_total\n\n\ndef test_line_total():\n    assert line_total([{\"quantity\": 2, \"unit_cents\": 150}]) == 300\nfrom dataclasses import dataclass\n\n\n@dataclass(frozen=True)\nclass Money:\n    cents: int\n    currency: str = \"USD\"\n\n    def __add__(self, other):\n        assert self.currency == other.currency\n        return Money(self.cents + other.cents, self.currency)\n.PHONY: test test-fast\n\ntest-fast:\n\tpytest -m \"not postgres\" -q\n\ntest:\n\tpytest -q\n---\nversion: 2\nid: 01a0f6ad-ee0f-71f3-be7d-e3fd9f97a430\ntype: decision\nstatus: Active\ntitle: Tax uses ROUND_HALF_EVEN\n---\n\n# Tax uses"
}
```


## Debrief answers (self-reported; verify before acting)

**ed-tax rep0**

```
Two things were awkward. Neither stopped me from doing the work.

1. **The hook only gave me memory titles.** The UserPromptSubmit and PreToolUse hooks injected lines like "[decision] Tax uses ROUND_HALF_EVEN (source: shared/human)". There was no body and no memory ID, and the summary was just the title again. I needed the details, such as whether ROUND_HALF_EVEN applies per line or per invoice, and whether results must use the `Money` type or plain int cents. So I ran `cat` on the `.engramdb/memories/*.md` files directly instead of using the MCP tools. What I wanted was for the hook to include each memory's ID and its content, or the first few lines of it. Then I could have read it in place, or called `get` with the ID. Without the ID, getting the full text through the tools meant a separate `query` call just to find it.

2. **I couldn't easily add to an existing memory.** The "Run tests with make test-fast" memory turned out to be incomplete. `make test-fast` fails until you run `pip install -e .[dev]`, and `tests/test_payments.py` also needs `requests`. I'd have wanted something like an `update` that adds a note, or a `challenge` that says "incomplete" rather than "contradicted". That would let me add the prerequisite without rewriting the human-written memory or creating a near-duplicate. Since I didn't fetch the tool schemas, I can't say for sure that `update` or `challenge` couldn't already do this. I also didn't record this finding at the end, which was my omission, not a tool limitation.
```

**nk-env-var rep0**

```
I can't fairly say whether anything was missing, because I never used EngramDB this session. I saved the `PAYMENTS_LIVE` hazard to the file-based auto-memory instead (`payments-live-env-var.md` plus its `MEMORY.md` pointer), and I never loaded or called any `mcp__plugin_engram_memory__*` tool. So no EngramDB tool, parameter or command blocked me.

What I can report is an awkward spot I didn't address at the time. The session had two overlapping memory systems: the file-based auto-memory and EngramDB, whose SessionStart hook asked me to record durable facts there. Nothing said which one to use or whether to write to both. I picked one without checking, so the hazard isn't in EngramDB. A future session that relies on EngramDB's `query` won't find it, and I didn't tell the user that.

If I had used EngramDB, I would have wanted these:

- **`create` with a hazard type and a `path` or `tags` anchor.** I would have used it to tie the memory to `tests/test_payments.py` and to a "payments" or "env-var" tag, so that path-based queries surface it when payment code is touched. I don't know if the existing parameters fully cover this, because I never loaded the schema.
- **A way to mark a fact as unconfirmed.** For example, an `epistemic: observation` value with a note that the source of the switch hasn't been found. The user reported the behaviour, but I couldn't find the `PAYMENTS_LIVE` check in the repo, so the memory should say that and not present it as confirmed.
```

**pq-tests rep0**

```
Nothing was missing in the sense of something I tried and couldn't do. I never called an EngramDB tool this session, and the injected memory ("Run tests with make test-fast") was enough. I confirmed it against the `Makefile`.

One small thing was awkward, though I didn't act on it:

- **No memory ID in the hook's injected context.** The UserPromptSubmit hook showed the memory's text, type and source (`shared/human`) but not its ID. After the `Makefile` confirmed the memory, I could have recorded that with `mcp__plugin_engram_memory__verify`. That tool needs an ID, so I would first have had to run `query` just to find a memory already on screen. If the hook printed each memory's ID next to its text, I could have called `verify` (or `challenge`, if the `Makefile` had disagreed) directly. Because of that extra step I skipped it, and the memory wasn't marked as confirmed.
```

