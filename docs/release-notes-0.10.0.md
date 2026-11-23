# Release notes — 0.10.0

Changes since 0.9.0. For the install prerequisite (ONNX Runtime ≥ 1.24), see
the release page or [installation.md](./users/installation.md).

Two pages give the full detail of the November 2026 code-review fixes, which
are part of this release:

- [release-notes-review-fixes.md](./release-notes-review-fixes.md): every
  change that a user, an agent or a contributor can notice.
- [release-notes-security.md](./release-notes-security.md): the security
  settings and hardening.

This page gives the upgrade steps, the breaking changes and the highlights.

## Upgrade checklist

1. **Move `[daemon]` and `[security]` to the global config file.** A
   `[daemon]` or `[security]` section in a project's `.engramdb/config.toml`
   is now ignored, with a warning. See [Breaking](#breaking) for the path of
   the global file.
2. **If an MCP agent writes to other projects, allow it in the global file.**
   `allow_cross_project_writes` now defaults to `false`. See
   [release-notes-security.md](./release-notes-security.md).
3. **Manual `engramdb setup` users: run `engramdb setup` again.** It adds the
   new `PostToolUseFailure` hook and the `Bash` matcher on `PostToolUse`, and
   it removes the automatic approval of `projects_link` / `projects_unlink`.
   Plugin users get the hooks automatically.
4. **Nothing else is manual.**
   - Each store migrates its schema (0.5.0 → 0.9.0) on first open. This
     rebuilds the index from the memory files in seconds and keeps vectors.
   - A running 0.9.0 daemon is asked to shut down and is replaced.
   - The first query downloads the reranker model (see below).

## Breaking

| Change | What to do |
|--------|------------|
| `[daemon]` is read only from the global config file. | Move the section. Use `--in-process` or `ENGRAMDB_IN_PROCESS=1` to skip the daemon for one command or session. |
| `[security]` is read only from the global config file. | Move the section. A project file can no longer grant itself access. |
| `allow_cross_project_writes` defaults to `false`. | Set it to `true` in the global file if an MCP agent must write to another registered project. The CLI is not affected. |
| MCP `projects_link` / `projects_unlink` outside the session's own project hierarchy need the cross-project gates. | Set the gates in the global file, or use the CLI. |
| `[nli].model` must be a built-in model. | Set `[security] allow_custom_model_repos = true` in the global file, then run `engramdb daemon restart`. |
| A `supersedes` id must name exactly one existing memory. | Use a full id or a unique prefix. An unknown or ambiguous entry now fails `create` / `update` before anything is written. |
| A `decay_ttl` or `decay_half_life` above 1,000 years is refused. | Use a smaller value. |
| The dashboard returns 400 for `layout_width` below 40. | Only scripts that call the dashboard API are affected. |

The global config file:

| Platform | Global config file |
|----------|--------------------|
| Linux | `~/.config/engramdb/config.toml` (or `$XDG_CONFIG_HOME/engramdb/config.toml`) |
| macOS | `~/Library/Application Support/engramdb/config.toml` |
| Any | `$ENGRAMDB_CONFIG_DIR/config.toml` when `ENGRAMDB_CONFIG_DIR` is set |

A missing file means the defaults. Model settings (`[embeddings]`, `[nli]`,
`[rerank]`, `[title]`) stay in each project's config.

Why `[daemon]` moved: the daemon is one process shared by every project and
session of a user. When `[daemon]` was per project, the first project to
spawn the daemon decided the idle timeout for everyone, and two `socket_path`
values meant two daemons, each loading every model. Why `[security]` moved: a
project config is committed with its repository, so a cloned repository could
grant its own agent access to other projects.

## Highlights

- **Memory relations.** Memories can now be linked with typed relations:
  `contradicts`, `related_to`, `duplicates`, `supports`, `derived_from` and
  `supersedes`. New commands `link`, `unlink` and `links` (CLI and MCP). A
  relation is stored in the memory file, so it travels with the repository.
  EngramDB proposes relations on its own: NLI contradictions as
  `contradicts`, close neighbours as `related_to` or `duplicates`. A proposed
  relation changes no score until someone confirms it. See
  [links.md](./users/links.md).
- **`engramdb dashboard`.** A local web page that shows every store on this
  machine: memories, a graph of their relations, a Relations tab that works as
  a review queue, usage, and store health. It is read-only. With
  `--allow-writes`, it can confirm or reject proposed relations. See
  [dashboard.md](./users/dashboard.md).
- **Per-memory usage.** EngramDB counts how often each memory is returned by a
  query, injected by a hook, and fetched by `get`. These are three separate
  counts, never added together. New: `stats --memory <id>` and
  `stats --top N --by fetched|injected|returned` (CLI and MCP `stats`).
  Promotion reads these counts.
- **Opt-in ranking from relations and usage.** New settings, all off by
  default: `contradicts_penalty`, `supports_boost`, `usage_weight` and
  `expand_links`. With every setting off, ranking is unchanged to the bit.
- **Session harvest.** The `SessionEnd` hook keeps a zstd copy of each Claude
  Code transcript, outside the repository. `/engram:harvest` (or
  `engramdb harvest`) reviews past sessions and saves what they settled as
  memories. `harvest search` finds past conversations by meaning. New MCP
  tools: `harvest_list`, `harvest_show`, `harvest_mark`, `harvest_search`,
  `harvest_ledger`. A memory records the sessions it came from, and that link
  keeps the transcript copy from being pruned. New config sections:
  `[harvest]`, plus `[security] allow_all_projects_harvest` (default `false`).
- **Reranking is on by default.** A quantized cross-encoder
  (`jina-turbo-q`) re-scores the top 10 results, for about 150 ms per query.
  To turn it off, set `[rerank] enabled = false`.
- **Faster text queries.** A text query is 3.4 to 3.9 times faster (p50:
  1,000 memories 26.9 → 8.1 ms; 10,000 memories 233 → 60 ms), with the same
  results.
- **Hooks run in the daemon when one is running.** `user-prompt-submit` drops
  from about 340 ms to about 80 ms; the other hooks gain 4–10 ms. A hook never
  starts a daemon. `engramdb stats` now shows hook call counts, errors and
  p50/p95/p99 times.
- **Better prompt context.** `UserPromptSubmit` infers the situation
  (debugging, design choice) with an embedding classifier, and also searches
  the end of the agent's previous message, so a short reply such as "yes"
  surfaces the right memories. New `[hooks]` keys: `situation_classifier`,
  `previous_message_chars`.
- **Agents use memory better** (measured by a new eval, `evals/memory-tool-use/`).
  Hooks inject each memory once per session, in full
  (`[hooks].preview_chars` 160 → 1000, `prompt_context_budget` 1500 → 3000).
  Core tools are always loaded instead of deferred behind ToolSearch
  (`[mcp].always_load`). The file hook picks memories by nearest enclosing
  scope.
- **Bash retry prompt.** When a Bash command fails and a later run of the same
  command succeeds, the hook asks once to save the cause.
- **Index currency.** Every index row records a digest of the file it was
  built from, and every memory's vectors record what they were embedded from.
  `reindex` now skips memories whose vectors are current. New:
  `reindex --dry-run`, `reindex --force`, `reindex --incremental`, and
  `[index] staleness_check` (`counts` | `size` | `content`, default `size`).
- **`projects discover`** walks a directory tree for `.engramdb/` stores that
  the registry does not know, and offers to register and index each one.
  This helps after a clone, a restore, or a lost `registry.json`.
- **Projects on removable drives are kept.** While a drive is unplugged,
  maintenance in another project no longer removes the drive's projects from
  the registry. When a store is found without a registry entry, the
  SessionStart hook and the MCP server register it again and tell the agent.
- **Daemon memory is bounded.** Models that no open project uses are unloaded
  after the idle timeout, and freed memory goes back to the OS. ONNX runs are
  split into small batches, so one large request no longer grows the daemon
  to several GiB.

## Other changes you may notice

- **`rank` mode no longer returns an empty result** when nothing clears the
  threshold. It returns the best 3, marked `below_threshold: true`, with a
  hint. Scripts that relied on an empty list: set
  `[retrieval] rank_fallback = 0`.
- **Retired config keys warn.** `[scope_proximity]`, `[logical_bonus]`,
  `[thresholds].needs_review` and `[search].semantic_weight` were never read.
  They still parse, but EngramDB warns once per file and `doctor` reports
  them. `[thresholds].compress` is now read.
- **Per-store data directories are owner-only** (mode `0700`).
- **Model files are checked against SHA-256 digests**, and ONNX Runtime is
  never loaded by a bare library name.
- **MCP text is marked and escaped.** Memory read tools start with a `trust`
  field, and harness-style tags in stored text come back escaped.
- **A git-tracked `.engramdb/` in a worktree is left alone.** Earlier versions
  could copy its files into the main worktree and delete the directory.
- **Daemon folder.** The socket, the reclaim lock and `daemon.log` now share
  one folder: the socket's folder. The log moved from
  `<data dir>/logs/daemon.log` to `<daemon folder>/daemon.log`.
- **The daemon folder must be private.** On Unix it is created with mode
  `0700`. If it exists with another owner or a looser mode, then the daemon
  refuses to start (models load in-process), and `doctor` reports a failure
  that names the fix. The client also refuses a socket or pipe that another
  user serves.
- **New default folder without `$XDG_RUNTIME_DIR`:**
  `<cache dir>/engramdb/daemon/` (was `<cache dir>/engramdb/`). A daemon that an
  older version started at the old path exits on its own after its idle
  timeout.
- **Daemon protocol 6.** An older daemon (protocol or build) is asked to shut
  down and is replaced. A newer daemon is left alone, and the client loads
  models in-process.
- **A spawned daemon starts with a minimal environment**, not the environment
  of the session that started it. See
  [daemon.md](./users/daemon.md#hooks) for the variables it keeps.
- **MCP tools are available on a cold start.** `engramdb serve` now answers
  Claude Code's handshake before its slow startup work. Before, a cold start
  could leave a session with hooks but no MCP tools.
- **The HTTP transport refuses browser requests.** `serve --transport sse`
  returns 403 for any request with an `Origin` header. MCP clients send none.
- **Hooks fail open.** An unknown hook name, or flags this binary does not
  know, exit 0 instead of 2, so a binary older than the plugin's hook wiring
  no longer blocks every prompt. `doctor` reports the mismatch.
- **Binary size.** The release profile moved from `opt-level = "z"` to `3`,
  which makes bulk paths about 11% faster. The LanceDB / DataFusion / Arrow
  crates stay built for size, so the binary grows by about a third, not
  double.
  Linux x86-64: about 79 MiB, was 60 MiB; the gzipped download is about
  31 MiB, was 24 MiB.
- **JSON keys are no longer sorted alphabetically.** `--format json` output
  and MCP tool results list an object's keys in the order the fields are
  defined (for example `session_id` first in a harvest listing). Key names
  and values are unchanged, so a script that parses the JSON is not affected;
  one that compares the raw text is. This comes from LanceDB 0.41, which
  turns on `serde_json`'s `preserve_order` feature for the whole build.
- **YAML memory files are read with `serde-saphyr`**, a pure-Rust parser that
  does not panic on malformed input. Existing files keep the same bytes.
- **Faster bulk paths.** Batched updates and reads in `task complete`,
  compression, consolidation and `gc`; cross-project `stats` is about 2× faster.

## Dependencies

- LanceDB 0.31 → 0.41 (Lance 14, Arrow 58).
- fastembed 7.7 with `ort` 2.0.0-rc.15 (ONNX Runtime C API 24).
- rmcp 3.10.
- Rust toolchain pinned in `rust-toolchain.toml` (1.100.0).

## For contributors

- New crate `engram-dashboard`, a front end like `engram-mcp`.
- CI builds the test binaries once and runs them in shards; caches are saved
  only on `master`. The `CI result` job is the one check to require.
- The dashboard description is in
  [contributors/dashboard.md](./contributors/dashboard.md).
