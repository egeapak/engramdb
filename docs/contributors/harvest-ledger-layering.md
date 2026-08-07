# Harvest: layered ledger, archive index, provenance

Design for splitting the harvest ledger into a fast append-only write buffer
and an indexed store, and for making archived conversations searchable.

Status: agreed, not yet implemented. Lands on
`claude/transcript-memory-extraction-z53uv2` after Phase H.

## The problem with one JSON file

`harvested_sessions.json` is a whole-map read-modify-write on the SessionEnd
hook path. Everything expensive about it follows from that shape: an advisory
`flock`, a merge function with precedence rules across four decision states,
corrupt-file quarantine, and a file that only ever grows. Four defects on this
branch came out of that machinery, not out of the feature.

## The model

State is encoded by *which store an entry is in*, not by a field two readers
have to agree about.

| Store | Decisions | Meaning |
|---|---|---|
| JSONL, in-project | `Unreviewed`, `Deferred` | still offer for review |
| LanceDB, global | `Harvested`, `Skipped` | reviewed; stop offering |

`harvest list` reads only the JSONL. The table is never on the hot path, so
there is no dual-source read and no precedence function to get wrong — which
is the failure this replaces.

Keep the distinction sharp: **archived is not harvested.** The hook archives at
session end, before anyone has looked, so an entry legitimately holds an
`archive` reference while still sitting in the JSONL awaiting review. The
`.zst` is written at SessionEnd; *indexing* happens at harvest.

## Why indexing at harvest time

Embedding at SessionEnd would stall session teardown, which is the one thing
the hook must not do. Harvest is interactive, already slow, and user-initiated,
and the shared daemon makes the embedding cheap. So the cost lands where a user
is already waiting and has asked for the work.

## The drain

Two-phase commit across two stores. Order is **index first, then remove from
the JSONL**, and re-indexing a session id must be idempotent:

- crash after index, before remove -> the next drain re-indexes and removes.
  Harmless.
- the reverse order would lose the entry outright.

Driven from `ops::maintenance`, which already runs a main-worktree pass, is
config-gated, and has `--no-maintenance`. Not from the SessionStart hook: its
injection is capped at `SESSION_CONTEXT_BUDGET` (2000 chars) and that space is
for memories. Not from an LLM hint either — this is deterministic maintenance,
and a step that only happens when a model notices a suggestion is a step that
sometimes silently does not happen.

`doctor` reports pending-index entries. Visibility, not mechanism.

## Provenance

A memory records the archives it was extracted from, so a challenged memory
resolves to the conversation that produced it — which is what the archive was
for.

Archives expire (365 days / 2 GiB, oldest first); memories do not. A memory can
therefore outlive its evidence, and that must read as "evidence expired", never
as a broken link. `doctor` checks for dangling references.

## Sequencing

Three changes, each reviewable on its own:

1. **JSONL + drain plumbing.** Append-only writes; `Harvested`/`Skipped` leave
   the file. Deletes the lock, the merge precedence, and the quarantine path.
2. **Archive index table.** New table in the existing per-project `lancedb/`,
   populated at harvest. `reindex --archive-only` (mirrors
   `--embeddings-only`). Search across past conversations; generated summaries
   replacing the current `first_prompt` preview, which is often "hey" or a
   pasted stack trace.
3. **Provenance + doctor.** Memory -> archive link; doctor checks for
   pending-index entries and dangling references.

(1) and (2) are additive — a new table, nothing rewritten. (3) touches the
memories schema, so it carries a `CURRENT_SCHEMA_VERSION` bump and a backfill
on open, and should be reviewed as such rather than as "just a field".

## Risk, recorded

This lands on a branch that is already 21 commits and eight review passes deep,
and on which three defects were introduced *while fixing other defects*. The
argument for riding along anyway is that harvest is a new feature and shipping
it with the right ergonomics beats migrating users twice. That is a deliberate
trade, made with the risk stated.

Two rules this branch paid for, which apply directly here:

- Never compare paths textually. Go through `compute_project_id` or
  `canonicalize`. Two separate defects came from `==`, one of which deadlocked
  the shipped `--dir .` configuration.
- A loss path the code does not declare is a bug. `capped_events` and
  `skipped_records` exist because the digest claimed completeness it did not
  have; a drain that drops an entry must be equally loud.
