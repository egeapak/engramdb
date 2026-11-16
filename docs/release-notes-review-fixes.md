# Release notes draft — fixes from the November 2026 code review

> Draft for the release that ships the fixes from the November 2026 code
> review. This page lists every change a user, an agent or a contributor can
> notice. Read the "Breaking" section before you upgrade. The security changes
> have their own page with the full text:
> [release-notes-security.md](./release-notes-security.md).

## Breaking

- **`[security]` is read only from the global config file.** A `[security]`
  section in a project's `.engramdb/config.toml` loads but grants nothing.
  EngramDB warns once per file and `engramdb doctor` reports it. Move the keys
  to `<global config dir>/config.toml`. See
  [release-notes-security.md](./release-notes-security.md).
- **Cross-project MCP writes are off by default.** `allow_cross_project_writes`
  now defaults to `false`. An MCP write tool called with `project` set to
  another registered project is refused until the global file sets it to
  `true`. The session's own project, its worktrees, group stores, the global
  store and the CLI are not affected.
- **MCP `projects_link` and `projects_unlink` are gated.** A link that touches
  a project outside the session's own hierarchy needs both
  `allow_cross_project_writes` and `allow_all_projects_harvest`; an unlink
  outside it needs `allow_cross_project_writes`. `engramdb setup` no longer
  approves these two tools, and it removes an approval an earlier version
  wrote, so Claude Code asks before an agent runs them.
- **`[nli].model` must be a built-in model**, unless the global file sets
  `[security] allow_custom_model_repos = true`. Any other repository leaves
  NLI off for that project. After you change the setting, run
  `engramdb daemon restart`.
- **A `supersedes` id must name exactly one existing memory.** On `create` and
  `update` (CLI and MCP), each `supersedes` entry is a full id or a unique
  prefix. An entry that matches no memory, or more than one, now fails the
  whole call before anything is written. Before, it was stored as given and
  superseded nothing. `update` keeps entries the memory already lists, so
  re-sending a list that names a since-deleted memory does not fail. A memory
  can no longer supersede itself, also not through `resolve` with
  `superseded_by`.
- **Out-of-range decay durations are refused.** A `decay_ttl` or
  `decay_half_life` above 1,000 years, or an expiry date outside the
  supported range, fails with a validation error. Before, it could abort the
  process. In the config, an out-of-range retention means "nothing is that
  old", and an out-of-range half-life falls back to 90 days with a warning.

## Config

- **Retired keys.** `[scope_proximity]`, `[logical_bonus]`,
  `[thresholds].needs_review` and `[search].semantic_weight` were never read
  by any code. They still parse, also when a section sets only some of its
  keys (that used to fail the whole file). When your config sets one,
  EngramDB warns once per file and `engramdb doctor` reports a warning under
  "Config file". Delete them to stop the warning. See
  [configuration.md](./users/configuration.md#notes-on-selected-sections).
- **`search_semantic_weight` is gone from the `config` output** (CLI and MCP
  `config` tool).
- **`[thresholds].compress` is now read.** It is the default threshold of
  `engramdb compress` and of the MCP `compress_candidates` tool. The default
  is still 0.4, so only a config that sets it sees a change.

## Storage and worktrees

- **A git-tracked `.engramdb/` in a worktree is left alone.** When a
  repository commits its memories, each linked worktree has `.engramdb/` as an
  ordinary checkout. Earlier versions copied its files into the main worktree
  and then deleted the directory. Now EngramDB asks git. It moves and removes
  only a directory that git reports as untracked. A tracked directory, or one
  git could not be asked about, gets no writes, no deletes and no copies.
- **A `.git` file must be confirmed by git before EngramDB follows it.** A
  directory is treated as a linked worktree only when git's own back-link
  agrees. Otherwise it is a plain directory, and EngramDB warns once and names
  `git worktree repair`. A worktree you moved by hand is in that state until
  you repair it.
- **Per-store data directories are owner-only.** `projects/<id>/`,
  `groups/<id>/` and `global/` under the data directory are created with mode
  `0700`, and an existing one you own is tightened on the next write. A
  read-only open changes no mode. `doctor` warns when other accounts can read
  the store's data directory.
- **An id with a file in both the shared and the personal directory** now
  resolves the same way everywhere: the newest `updated_at` wins, a tie goes to
  the personal copy, and a copy that does not parse loses. `doctor` reports
  each such id ("Shared/personal duplicates"), and `reindex` names the shadowed
  file.
- **Only `.md` files are memory files.** An editor backup such as
  `<id>.md.orig` is no longer read, updated or deleted as the memory.
- **A memory with a hand-written (non-UUID) id stays reachable.** Its file is
  no longer named with a title slug that hid the id. `reindex` renames such a
  file.
- **A file with no `Created` field** gets its file time instead of the time of
  each read, so its timestamps no longer change between reads.
- **`reindex --force` keeps the vectors of a file that does not parse** when
  the model is unchanged. A model change drops them, and says so.
- **The maintenance pass rebuilds an index that lost every row** while the
  memory files are there. The SessionStart notice names `engramdb reindex`
  instead.
- **`gc` reports files that do not parse** apart from stale index entries, and
  never offers them for deletion.

## Hooks and Claude Code

- **Hooks with unparseable arguments exit 0.** A hook invocation whose flags
  this binary does not know (for example, a newer `plugin.json`) prints one
  warning and exits 0 without running. Before, it exited 2, which Claude Code
  treats as a blocking error on every prompt. A hook also exits 0 when no
  registry can be resolved or when Claude Code has closed its output.
- **Hook input is capped at 32 MiB.** A larger event is cut, gives no context,
  and is warned about.
- **A session that opts out of maintenance** (`ENGRAMDB_DISABLE_AUTO_MAINTENANCE`,
  `ENGRAMDB_AUTO_MAINTENANCE_INTERVAL_SECS`) runs its hooks in-process when the
  daemon was started with different values, so the opt-out holds.
- **`engramdb setup`** keeps the mode of the files it writes (a `0600`
  `~/.claude.json` stays `0600`; a new one is created `0600`), writes through a
  symlink instead of replacing it, and redoes its merge once when another
  program changed the file meanwhile. Its fallback message now says that the
  MCP entry goes to `~/.claude.json`, and shows why the plugin install failed.
  `doctor` names the documented `/plugin` commands. See
  [claude-code.md](./users/claude-code.md#option-b-engramdb-setup-no-plugin).
- **MCP text is marked and escaped.** Memory read tools start with a `trust`
  field, and harness-style tags in stored text come back escaped. See
  [release-notes-security.md](./release-notes-security.md).
- **A memory created just before an MCP session ends keeps its vectors.** The
  session waits (up to 10 s) for background embedding at shutdown, and warns,
  naming `engramdb reindex --embeddings-only`, when some is still running.
  `doctor` has a new "Memory vectors" check for live memories with none.
- **A `query` no longer waits for index compaction.** Compaction runs in the
  background, also for worktree sessions.

## Daemon

- **The client checks who serves the daemon socket or pipe.** A socket or pipe
  served by another user is refused. Hooks and models then run in-process, and
  `doctor` reports it.
- **A newer running daemon is left alone.** An older client no longer spawns a
  daemon over it or keeps it awake. It runs in-process and checks again after
  the idle timeout (at least 10 minutes).
- **Upgrades replace the old daemon reliably.** The client waits up to 5 s for
  an evicted older daemon to stop before it starts the new one.
- **A spawned daemon starts in its own folder**, not in the checkout of the
  session that started it.
- **`daemon.log` stays capped at 1 MiB while the daemon runs**, and a
  persistent accept error no longer spins a core.
- **Models a session loaded during a daemon outage are unloaded** once the
  daemon serves the session again, after the daemon's idle window.

## Models

- **Model files are checked against SHA-256 digests.** A built-in model file
  that does not match is not loaded, and the model falls back as if it were
  missing. `doctor`'s "Embedding model cache" check names the file.
- **ONNX Runtime is never loaded by bare name.** The search uses absolute
  paths only, and a relative `ORT_DYLIB_PATH` must exist under the
  executable's directory. See [installation.md](./users/installation.md).
- **`stats` and `init` know every embedding provider** that the config
  accepts. Before, eight documented providers read as "unknown provider" in
  `stats`.

## Harvest

- **Sessions whose indexing fails no longer block the queue.** A failed
  session waits 24 hours and is then tried after every session that never
  failed. `doctor` counts them apart from the backlog. `harvest index <id>`
  ignores the wait.
- **Ledger compaction is safe against a second compactor and a crash.** A
  compacted ledger keeps a record of each session removed in the last 365
  days, so an older line cannot bring the session back.
- **A state file with a damaged byte** is read with that line skipped, instead
  of folding as empty.
- **`harvest ledger export`** writes the file only when the whole transcript
  copy decoded, so a damaged copy no longer leaves a short transcript behind.
  The exported file is owner-only.
- **`harvest ledger prune`** reports copies it chose but could not delete, and
  the `--older-than 0d` hint names a command that works.
- **`add --editor`** writes its draft to a private temp file (mode `0600`) and
  deletes it on every exit path.

## Dashboard

- **`layout_width` below 40 is a 400.** `/graph` accepts a layout width from 40
  to 20,000 (a whole multiple of 40). `layout_width=0` used to pass. The page
  asks for 800 when it could not measure its box.
- **Scope names in `top_scope=`, `from_scope=`, `to_scope=` and `touches=` are
  percent-escaped.** A scope named `-` is `%2D` and a comma is `%2C`; a bad
  escape is a 400. Old links keep working unless a scope name contains `%`.
- **Daily usage buckets are labelled UTC.**
- **Overview counts are the store's**: challenged memories past the list cap,
  live memories in use, and the relation total the Relations tab lists.
- **The Health page lists files that do not parse**, and says to fix the file
  and run `engramdb reindex` (not `doctor --fix`, which cannot repair one).
- **A cached layout read from disk is verified** (SHA-256 trailer, finite
  numbers). Each cached layout is computed once more after the upgrade.
- Zoom and pan survive a width change of the graph box, `]` and `[` start
  from the ends when no memory is focused, a relation reviewed elsewhere ends
  settled, and the page serves its own icon.

## Contributors

- **The Rust toolchain is pinned to 1.97.0** in `rust-toolchain.toml`. rustup
  installs it on the first `cargo` call. CI and the release workflow use the
  same version. Dependabot proposes bumps.
- **The dashboard description moved** from `.claude/CLAUDE.md` to
  [contributors/dashboard.md](./contributors/dashboard.md). CLAUDE.md keeps
  the rules that guard known defects.
