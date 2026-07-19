# Release notes draft — epistemic memory classes (0.9.0)

> Draft for the release that ships the epistemic-memory feature
> (spec: `docs/plans/2026-07-19-epistemic-memory-spec.md`).

## Highlights

Every memory now carries an **epistemic class** — `fact`, `observation`, or
`decision` — orthogonal to its memory type. The class defaults from the type
(context/convention/relationship/hazard → fact, debug → observation,
decision/intent/preference → decision), so existing memories need no
migration: they parse to the same class they always implicitly had, and files
that never use the new fields round-trip byte-identical.

On top of the class sit:

- **Bi-temporal validity**: memories can be *invalidated* (window closed via
  supersession, `resolve --action invalidate`, or compression) without being
  deleted. Retrieval and `list` exclude them by default;
  `include_invalidated: true` shows history. GC purges them only after
  `[epistemic] invalidated_retention_days` (0 = keep forever); until then
  they are exempt from low-score GC too.
- **Situation-aware ranking**: hooks and queries can pass a `situation`
  (`session_start`, `file_edit`, `debugging`, `design_choice`) that reweights
  classes — observations surface while debugging, decisions while designing.
- **Validity metadata**: `premise` ("holds because C"), `invalidated_by`
  ("re-check when X"), `origin_task` + `generality: task` for task-scoped
  memories, `derived_from` for consolidated facts.
- **New tools**: `verify` (re-confirm a memory, refreshing fact decay),
  `task_current` / `task_complete` (declare and close task scopes;
  completion demotes task-scoped memories to fast decay),
  `resolve --action invalidate`, and four new Claude Code hook events
  (`UserPromptSubmit`, `PostToolUse`, `SessionEnd`, `PreCompact`).
- **Smarter maintenance**: telemetry-driven promotion of re-confirmed
  task-scoped memories, and observation consolidation (near-duplicate
  clusters merge into a derived fact; opt-in apply via
  `[epistemic] auto_consolidate`).

## Intended behavior changes on upgrade

Everything else is inert for existing data; these four change behavior:

1. **Hook-driven ranking.** Session-start and pre-tool-use hook queries now
   carry a situation, so tuned profiles reorder hook results.
   Opt-out: `[retrieval.scoring.situation] floor = 1.0`.
2. **Per-class challenge penalties.** Stores with no scalar
   `challenge_penalty` configured move from flat 0.10 to 0.15 (fact) /
   0.20 (observation) / 0.05 (decision): challenged facts and observations
   score slightly lower, challenged decisions slightly higher.
   Opt-out: set `challenge_penalty = 0.10`.
3. **`supersedes` closes validity windows** — for new writes only.
   Pre-existing `supersedes` references are never retroactively closed.
4. **`compress` invalidates its sources instead of deleting them.** Sources
   are retained (queryable via `include_invalidated`) until GC retention
   purges them.

## Storage

- LanceDB schema bumps 0.2.0 → 0.3.0; existing stores reindex automatically
  on first open (seconds; vectors preserved, no re-embed).
- Memory files gain optional frontmatter fields (`epistemic`, `valid_while`,
  `valid_from`, `invalidated_at`, `superseded_by`), emitted only when they
  differ from the type-derived default.

## Downgrade caveat

An older binary **reads** new-format files fine (unknown fields are ignored
and degrade to defaults) — but any old-binary **rewrite** of a memory file
(update, challenge, compress) silently drops the new fields, which can
resurrect an invalidated memory. Downgrade is read-safe, not write-safe.
