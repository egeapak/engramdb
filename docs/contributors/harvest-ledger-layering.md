# Harvest: layered ledger, archive index, provenance

Spec for splitting the harvest ledger into an append-only write buffer and an
indexed store, and for making past conversations searchable.

Status: agreed. Lands on `claude/transcript-memory-extraction-z53uv2`.

## Terminology

Three different things; two of them are JSONL, which is what made earlier
drafts of this document confusing.

| Term | What it is | Size |
|---|---|---|
| **ledger** | our state log — one line per session: id, stage, decision, timestamps, memory ids, pointer to the copy. **No conversation content.** | bytes/session |
| **transcript copy** | the conversation itself, copied out of `~/.claude/projects/` before Claude Code prunes it | ~1–3 MB/session |
| **digest** | the deterministic, code-generated reduction an agent reads | ~60 KB/session |
| **index row** | summary + embeddings in LanceDB, for search | ~KB/session |

"Archive" is retired as a noun — it meant both the copy and the compressed
stage. Use **transcript copy** for the bytes and **compressed** for the stage.

## Two axes, not one

These are independent and must not be collapsed into one word:

- **stage** (mechanical — where the bytes are): `collected` -> `indexed` -> `compressed`
- **decision** (human — what you concluded): `unreviewed` -> `deferred` | `skipped` | `harvested`

A session can be `compressed` while `deferred`, or `indexed` while `skipped`.
`Harvested` remains the *decision* meaning "a human saved memories from this";
the *stage* for that work is `indexed`.

## Lifecycle

```
collect    verbatim copy + ledger line                     (SessionEnd hook)
digest     deterministic, keeps failure signals            (on demand)
harvest    agent extracts memories + writes a summary      (interactive)
index      embed digest · store+embed summary · pin        (maintenance or harvest)
compress   zstd the copy AND the digest, side by side      (maintenance)
```

Indexing runs at harvest **or** on timeout, so a session that is never reviewed
is still searchable. Compression runs after indexing, so nothing is ever
decompressed in order to index it.

### Why the copy is verbatim

Nothing is stripped at collect time, for two reasons — the second is the one
that matters:

1. It is evidence. A challenged memory must resolve to what was actually said.
2. **You cannot un-drop.** If reduction happened at collect, `reindex
   --archive-only` could never do better than reduction v1. A whole copy keeps
   every later improvement — better summarizer, different chunking, changed
   tool heuristics — a re-derivation away.

Reduction is therefore a *projection* applied per consumer, never a mutation of
the stored copy:

| Consumer | Tool output | Why |
|---|---|---|
| stored copy | untouched | evidence; keeps reindex open |
| index (embedding) | prose only, **plus failures and their error text** | tool noise dilutes the vector |
| digest (agent) | one line per call; dropped first under budget | agent needs the trace, cheaply |
| compress | untouched | pure encoding |

Failures are kept everywhere. `parse_session` already argues this for the
digest — "this command failed and that one worked is frequently the durable
lesson, and it only exists in the result" — and it is *more* true for search,
because "why did the build break in July" is a question about a failure.

## Embedding: two vectors, one row

| Column | Source | Present |
|---|---|---|
| `digest_vec` | deterministic digest | always |
| `summary_vec` | curated summary | after review |

Query both, take the better score, break ties toward the summary — a human
wrote it, so a match there is higher precision.

Two columns rather than one embedding of digest+summary concatenated: editing a
summary would otherwise invalidate the digest vector, re-embedding 60 KB to fix
a typo instead of two sentences.

The **embedding source is the deterministic digest, never the agent's**, so
`reindex --archive-only` is meaningful — an agent-authored digest is not
regenerable by code. The agent's summary is stored and separately embedded, but
it is not what recall depends on.

## Storage layout

Unchanged from today except the ledger format and the new table.

In-project, `<project>/.engramdb/`:
- `memories/*.md` — committed, travels with a clone
- `state/harvest_ledger.jsonl` — append-only, gitignored

Global data dir, `projects/<root-project-id>/`:
- `transcripts/<session-id>.jsonl` — collected copy, `0600`
- `transcripts/<session-id>.jsonl.zst` + `<session-id>.digest.md.zst` — compressed stage
- `lancedb/` — memories table plus the new index table

`<root-project-id>`: worktrees and linked sub-projects resolve to one root, which
is what keeps the ledger and the copies agreeing.

## Ledger: append-only JSONL

One line per state transition, last-write-wins per session id on read. This
replaces a whole-map read-modify-write, and with it the advisory `flock`, the
four-state merge precedence, and the corrupt-file quarantine — four defects on
this branch came out of that machinery rather than out of the feature.

Entries leave the ledger when they reach `compressed`; the index row is then
the record. `harvest list` reads only the ledger, so there is no dual-source
read and no precedence function on the hot path.

Compaction: rewrite the file when line count exceeds live-entry count by a set
factor. `doctor` reports pending-index and pending-compaction counts.

## Retention and pinning

- **index row** — permanent. Cheap, and it is what answers "did we ever discuss
  X". A `skipped` session keeps its row: knowing you already reviewed something
  and found nothing is what stops you reviewing it again.
- **transcript copy** — evictable by budget (365 days / 2 GiB, oldest first)
  **unless memories reference it**. That pin is overridable only by an explicit,
  confirmed operation.

The budget therefore applies to unpinned copies. Pinned bytes beyond it are a
state the user should see, not a limit silently enforced against them: `doctor`
reports "N GiB held by copies backing M memories".

## Provenance

A memory records the sessions it was extracted from, so a challenged memory
resolves to the conversation that produced it. This is also what pins the copy.

## Command surface

| Command | Does |
|---|---|
| `harvest index [<id>\|--all] [--force]` | digest -> embed -> row with empty summary; idempotent |
| `harvest search <query> [-n N] [--since 30d] [--all-projects]` | search indexed conversations |
| `harvest summary <id> [text \| --editor \| --from-file]` | set/replace the curated summary; re-embeds `summary_vec` only |
| `harvest mark <id> … [--summary "…"]` | existing, plus writing the summary in one call |
| `harvest ledger list … [--stage collected\|indexed\|compressed]` | existing, plus stage |
| `reindex --archive-only` | rebuild index rows from stored copies |

`harvest show` is unchanged: search returns an id, `show` already reads from the
copy, so the loop closes.

`harvest summary` is its own verb rather than only a `mark` flag because `mark`
sets the decision — re-running it to fix a typo would rewrite that too.

MCP: **`harvest_search`** (new), and `harvest_mark` gains `summary`.
`harvest_index` is deliberately *not* an MCP tool — it is maintenance, not
something an agent should decide to run.

### Security

`harvest_search` returns conversation text, so it must ride the same
`allow_all_projects_harvest` gate as `harvest_list`, sourced from the caller's
own config. A review already found exactly this hole in `harvest_mark`, where
prefix resolution leaked session ids past the gate. Do not repeat it.

## Implementation steps

Each lands as its own commit, reviewable alone.

**Step 1 — ledger format and drain plumbing.** JSONL, append-only, last-write-
wins; stage field; compaction; entries leave at `compressed`. Deletes the lock,
the merge precedence, and the quarantine path. No LanceDB yet: the drain target
is "removed from ledger". Migration reads the old JSON map once and appends it.

**Step 2 — index table and search.** New table in the existing per-project
`lancedb/`, two vector columns, populated at harvest or on timeout via
`ops::maintenance`. `harvest index`, `harvest search`, `harvest summary`,
`reindex --archive-only`, `harvest_search` MCP tool. Additive — a new table,
nothing rewritten.

**Step 3 — provenance and pinning.** Memory -> session link, pinning against
eviction, `doctor` checks for pending-index, pending-compaction, dangling
references, and pinned bytes over budget. This one touches the **memories**
schema, so it carries a `CURRENT_SCHEMA_VERSION` bump and a backfill on open —
review it as a migration, not as "just a field".

## Open question, to measure not guess

The local summarizer is **t5-small** (`Xenova/t5-small`), used today for
*titling*. On a 60 KB digest expect one short line, not two good sentences. Fine
as a fallback so an unharvested session is not blank; not a substitute for the
agent's summary. Measure before promising it in user docs.

## Rules this branch already paid for

- **Never compare paths textually.** Go through `compute_project_id` or
  `canonicalize`. Two separate defects came from `==`, one of which deadlocked
  the shipped `--dir .` configuration.
- **A loss path the code does not declare is a bug.** `capped_events` and
  `skipped_records` exist because the digest claimed a completeness it did not
  have. A drain that drops an entry must be equally loud.
- **`git add -A` is unsafe here.** Review agents run concurrently in this
  checkout and their scratch has been committed twice. Stage explicit paths.
