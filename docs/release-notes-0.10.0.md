# Release notes — 0.10.0

Changes since 0.9.0. For the install prerequisite (ONNX Runtime ≥ 1.24), see
the release page or [installation.md](./users/installation.md).

## Upgrade checklist

1. **Move any `[daemon]` section to the global config.** See
   [Breaking](#breaking-daemon-is-read-only-from-the-global-config). A
   `[daemon]` section in a project's `.engramdb/config.toml` is now ignored.
2. **Manual `engramdb setup` users: run `engramdb setup` again.** It adds the
   new `PostToolUseFailure` hook and the `Bash` matcher on `PostToolUse`.
   Plugin users get both automatically.
3. **Nothing else is manual.**
   - Each store migrates its schema (0.5.0 → 0.8.0) on first open. This
     rebuilds the index from the memory files in seconds and keeps vectors.
   - A running 0.9.0 daemon is asked to shut down and is replaced.
   - The first query downloads the reranker model (see below).

## Highlights

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
- **Daemon memory is bounded.** Models that no open project uses are unloaded
  after the idle timeout, and freed memory goes back to the OS. ONNX runs are
  split into small batches, so one large request no longer grows the daemon
  to several GiB.

## Breaking: `[daemon]` is read only from the global config

The daemon is one process shared by every project and session of a user, so
its settings now come from one place: the `[daemon]` section of the global
config file.

| Platform | Global config file |
|----------|--------------------|
| Linux | `~/.config/engramdb/config.toml` (or `$XDG_CONFIG_HOME/engramdb/config.toml`) |
| macOS | `~/Library/Application Support/engramdb/config.toml` |
| Any | `$ENGRAMDB_CONFIG_DIR/config.toml` when `ENGRAMDB_CONFIG_DIR` is set |

A missing file means the defaults. Model settings (`[embeddings]`, `[nli]`,
`[rerank]`, `[title]`) stay in each project's config.

If a project config still has `[daemon]`, the project loads, but the keys
(`enabled`, `use_for_cli`, `idle_timeout_secs`, `socket_path`) are ignored.
EngramDB logs a warning once per file, and `engramdb doctor` shows a warning on
the "Config file" check. To skip the daemon for one command or session, use
`--in-process` or `ENGRAMDB_IN_PROCESS=1`.

Why: the first project to spawn the daemon decided the idle timeout for
everyone, and two `socket_path` values meant two daemons, each loading every
model.

## Other changes you may notice

- **`rank` mode no longer returns an empty result** when nothing clears the
  threshold. It returns the best 3, marked `below_threshold: true`, with a
  hint. Scripts that relied on an empty list: set
  `[retrieval] rank_fallback = 0`.
- **Daemon folder.** The socket, the reclaim lock and `daemon.log` now share
  one folder: the socket's folder. The log moved from
  `<data dir>/logs/daemon.log` to `<daemon folder>/daemon.log`.
- **The daemon folder must be private.** On Unix it is created with mode
  `0700`. If it exists with another owner or a looser mode, then the daemon
  refuses to start (models load in-process), and `doctor` reports a failure
  that names the fix. Earlier versions tightened a loose folder silently.
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
- **An unknown hook name exits 0** instead of 2, so a binary older than the
  plugin's hook wiring no longer blocks every prompt. `doctor` reports the
  mismatch.
- **Larger, faster binary.** The release profile moved from `opt-level = "z"`
  to `3` (about 112 MiB, was about 59 MiB; about 11% faster on bulk paths).
- **YAML memory files are read with `serde-saphyr`**, a pure-Rust parser that
  does not panic on malformed input. Existing files keep the same bytes.
- **Faster bulk paths.** Batched updates and reads in `task complete`,
  compression, consolidation and `gc`; cross-project `stats` is about 2× faster.
