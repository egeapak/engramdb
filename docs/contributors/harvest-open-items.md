# Harvest — open items before merge

Tracked findings from the final review passes on
`claude/transcript-memory-extraction-z53uv2`. Each entry names the defect, the
reproduction, and what "done" means. Delete this file when the list is empty.

Baseline at time of writing: HEAD `60217a5`, 2035 tests passing, clippy clean.

## Phase F — security

### F1. Ledger writes follow a symlink (arbitrary file overwrite)

`crates/engram-storage/src/harvest_state.rs` — `let tmp = path.with_extension("json.tmp"); std::fs::write(&tmp, json)?`
opens without `O_NOFOLLOW`, so a symlink planted at
`.engramdb/state/harvested_sessions.json.tmp` redirects the write; the
following `rename` moves the symlink, not the target. Reached by
`mark_harvested`, `set_archive`, `clear_archive_refs`, `adopt_ledger`.

Delivery is reliable and needs no local access: `.engramdb/` is *designed* to
be committed (`memories/` is deliberately tracked), `write_state_gitignore`
never overwrites an existing `.gitignore`, and a committed symlink is checked
out on clone regardless of `.gitignore`. The unattended SessionEnd hook then
fires it, and `[harvest] archive` defaults to `true`.

```bash
mkdir -p hostile/.engramdb/state && cd hostile && git init -q .
printf 'state/\n' > .engramdb/.gitignore
ln -s ../../../../victim.txt .engramdb/state/harvested_sessions.json.tmp
git add -f .engramdb && git commit -qm x && cd .. && git clone -q hostile clone
# symlink is present in the clone; SessionEnd then overwrites victim.txt
```

`crates/engram-storage/src/task_state.rs` has the same pattern and predates
this branch — fix both, and check for other `with_extension("…tmp")` writers.

**Done when:** a planted symlink cannot redirect a ledger or task-state write,
with a test that plants one and asserts the victim file is untouched.

### F2. `harvest_mark` is an ungated cross-project session-id oracle

`crates/engram-mcp/src/server.rs` — `harvest_mark` calls only
`check_cross_project_write` (defaults to **allow**), never
`check_harvest_scope`, then drives `resolve_harvest_session`, which lists the
target project's live transcripts and echoes their ids in its ambiguity error.

```
harvest_list { project: "<other>" }                  -> refused
harvest_mark { session_id: "secret-", project: "<other>" }
  -> "Ambiguous session id 'secret-' — matches 2 sessions: secret-beta, secret-alpha"
```

A single-match prefix is a clean existence oracle; a no-match prefix a clean
negative. Reproduced with `allow_all_projects_harvest = false` in the caller's
own config.

Note the interaction with F6: `harvest_mark` deliberately has no
`all_projects`, and gating its *reads* must not make an archived session
unmarkable. The ledger fallback covers the pruned case.

**Done when:** `harvest_mark` cannot enumerate a project the read gate
refuses, and an MCP test asserts the error leaks no ids.

## Phase G — correctness

### G1. `harvest reset` strands the archive and reports success falsely

`clear_harvested` drops the whole entry, including its `archive` reference,
while the `.zst` stays on disk. Nothing then reaches the file: `harvest show`
says "No session matching", `ledger export` says "No harvest record matching",
`ledger list` does not show it. The success message ("it will be offered
again") is false for a session whose live transcript Claude Code has pruned —
it is offered by nothing.

**Done when:** `reset` either keeps the archive reachable or refuses and says
why; success text matches what actually happens in both cases.

### G2. `merge_entries` loses a deliberate human deferral

"Settled beats `Deferred` regardless of timestamp" was chosen so the SessionEnd
hook's entry cannot overwrite a review. With `Unreviewed` now distinct, that
rule is too broad: a user's later `harvest mark --defer` on the root loses
deterministically to an older settled entry from the sub-project.

**Done when:** the six decision pairs have explicit, tested precedence, and a
deliberate `--defer` survives adoption.

### G3. MCP `harvest_mark` cannot mark what `harvest_show` can read

`harvest_show` accepts `all_projects`; `harvest_mark` does not, so a session an
agent was allowed to digest may be unmarkable and re-offered forever. The CLI
does not have this gap.

**Done when:** anything `harvest_show` can display, `harvest_mark` can settle —
without widening the security gate.

### G4. An over-ceiling record is dropped while the digest claims completeness

`crates/engram-storage/src/transcripts.rs` skips any JSONL record over
`MAX_RECORD_BYTES` (4 MiB) and only `tracing::debug!`s it, below the CLI's
default level. `SessionSummary` carries no skipped count, so `user_turns`
under-counts, `first_prompt` can shift to a different turn, and
`is_complete()` still returns `true`.

This is the same class as the `cap_event` fix (R10): a loss path the digest
does not declare.

**Done when:** a skipped record is surfaced the way `capped_events` is, and
`is_complete()` is false when one occurred.

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
