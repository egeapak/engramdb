# Harvest — open items before merge

Tracked findings from the final review passes on
`claude/transcript-memory-extraction-z53uv2`. Each entry names the defect, the
reproduction, and what "done" means. Delete this file when the list is empty.

Baseline at time of writing: HEAD `60217a5`, 2035 tests passing, clippy clean.

## Phase H — docs and messages

Small, verified against the code at `60217a5`:

1. Three stale `Deferred` claims invalidated by `Unreviewed`: the rustdoc on
   `set_archive` (it creates `Unreviewed`), and two in `src/ops/harvest.rs`
   ("the SessionEnd hook writes a `Deferred` entry for every session it
   archives") plus matching test comments.
2. `PRUNE_AFTER_DAYS = 365` is not configurable and never stated numerically in
   user docs; a `skipped` entry whose archive was pruned silently disappears
   after a year and the session is re-offered.
3. `.claude/CLAUDE.md` omits ledger auto-adoption and the `.json.adopted`
   rename (the most surprising new behavior), still describes the ledger as
   shared only with worktrees (also `projects link` sub-projects), and its
   Claude Code section names only `pre-tool-use` / `session-start` though
   SessionEnd now archives, writes `Unreviewed`, prunes, and reconciles. The
   `commands/` slash-command directory is never mentioned.
4. Neither the empty-list output nor the "No session matching" error mentions
   the archive recovery route (`harvest ledger list` -> `harvest show`), which
   `cli-reference.md` documents as the workflow. `harvest reset`'s "No harvest
   record matching" names no fix at all.
5. `crates/engram-types/src/config.rs` — `allow_all_projects_harvest`'s field
   doc describes only the `all_projects` half; it also gates naming another
   `project`. Its mutating-tool list omits `harvest_mark`.
6. `.claude-plugin/README.md` implies every session is archived (the hook
   returns early unless the project was `init`ed) and says archives live under
   `projects/<project-id>/` when it is the **root** project id.
7. `commands/harvest.md` says `ledger export` "writes the full transcript to a
   file" with no caveat; it fails when no archive exists.
8. Digest header renders "1 long events" — no singular form.
9. `session_scope` walks *down from the root*, so running from a worktree also
   covers the parent and siblings. Three places describe scope as "this project
   plus its sub-projects", which understates it.
10. CLI `harvest list --format json` emits `cwd` / `first_prompt` /
    `git_branch` unsanitized — serde escapes C0, but bidi controls pass
    through. The MCP path sanitizes these.

## Process note

Two defects on this branch came from the same mistake: comparing paths
*textually* when one side is canonicalized and the other is not
(`check_harvest_scope`, then `adopt_ledger` — the latter deadlocked the
shipped `--dir .` configuration). Any new path comparison here should go
through `compute_project_id` or `canonicalize`, never `==`.

`git add -A` is unsafe in this checkout: review agents run concurrently and
their scratch has been committed twice. Stage explicit paths.
