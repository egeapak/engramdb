# The Shared Embedding Daemon

> **Changed in this release:** `[daemon]` settings now live in the **global** config file, not in `<project>/.engramdb/config.toml`. See [Configuration](#configuration).

stdio MCP runs one process per agent session. Without coordination, every concurrent session loads its own copy of the embedding (and optional NLI / reranker) models — hundreds of MB and a ~240 ms ONNX init each. The daemon loads each model **once** machine-wide and serves inference to every MCP process over a per-user Unix socket. Storage stays in the MCP process; only inference is delegated. Model-needing CLI commands can use the same daemon too (see [CLI usage](#cli-usage)).

## Lifecycle

MCP auto-spawns the daemon when needed (race-coordinated by an advisory file lock; concurrent spawns are safe). **You never start it manually.**

- **Stays alive while sessions are connected.** Each MCP `serve` process runs a background **heartbeat** that pings the daemon every `idle_timeout_secs / 3` (minimum 30 s). The daemon and every session read `idle_timeout_secs` from the same global config file, so the pings always fit inside the daemon's idle window. Those pings reset the daemon's idle clock, so it stays resident as long as any agent session is running — not just for `idle_timeout_secs` after the last inference.
- **Reaps after the last session ends.** Once every session has exited, the pings stop and the daemon exits after `idle_timeout_secs` (default 15 minutes) of inactivity.
- **Self-heals.** If the daemon dies or is replaced (idle-exit, crash, manual restart, or a protocol-version change), the heartbeat re-spawns a fresh one and live sessions transparently route to it on their next request — no need to restart the agent.

If the daemon is disabled (`enabled = false`) or unreachable for any reason, MCP **and** the CLI load models in-process. **Operations never fail because of the daemon** — that's the contract.

## CLI usage

Model-needing CLI commands (`query`, `add` / `create`, `update`, `reindex --embeddings-only`) use a **running** daemon when one is reachable, so a warm daemon turns each command's ~240 ms cold model load into a sub-millisecond socket round-trip.

- **Connect-only by default.** The CLI uses a daemon only if one is *already* running; it does **not** spawn one (a one-shot command shouldn't leave a 15-minute daemon behind). With no daemon reachable, it loads models in-process.
- `--spawn-daemon` lets the CLI spawn (and warm) a daemon when none is running.
- `--in-process` — or `ENGRAMDB_IN_PROCESS=1`, or `use_for_cli = false` in the global `[daemon]` — forces in-process loading and never contacts a daemon.
- `enabled = false` in the global `[daemon]` disables the daemon for both CLI and MCP.

Precedence (highest first): `--in-process` flag → `ENGRAMDB_IN_PROCESS` env → `[daemon].use_for_cli` → `--spawn-daemon` → default connect-only.

## Commands

```bash
# Show whether a daemon is running, plus heartbeat + request metrics
engramdb daemon status

# Ask it to exit gracefully (the next MCP run will spawn a fresh one)
engramdb daemon stop

# Stop + start a fresh daemon (useful after changing model config)
engramdb daemon restart

# Run the loop in the foreground — for debugging the daemon itself
engramdb daemon run

# Cumulative request metrics (persisted to LanceDB, works even when no daemon is running)
engramdb stats --daemon
```

Each command accepts `--socket <path>` to target a non-default socket. `run` and `restart` also take `--idle-timeout <secs>`. Without the flags, every `daemon` subcommand reads the global config, in whatever directory you run it. An auto-spawned daemon gets only `--socket`; it reads `idle_timeout_secs` from the global config itself.

## Configuration

The daemon is one process shared by every project and session of a user. Its settings therefore come from one place: the `[daemon]` section of the **global config file**.

| Platform | Global config file |
|----------|--------------------|
| Linux | `$XDG_CONFIG_HOME/engramdb/config.toml` (default `~/.config/engramdb/config.toml`) |
| macOS | `~/Library/Application Support/engramdb/config.toml` |
| Any | `$ENGRAMDB_CONFIG_DIR/config.toml` when `ENGRAMDB_CONFIG_DIR` is set |

```toml
[daemon]
enabled = true            # false: MCP and CLI both load models in-process
idle_timeout_secs = 900   # 15 min idle → daemon exits (minimum 60)
use_for_cli = true        # false: CLI never uses the daemon (MCP still does)
# socket_path = "/run/user/1000/engramdb/daemon.sock"   # optional override
```

A missing file means the defaults above. The global file holds only `[daemon]`. Model settings (`[embeddings]`, `[nli]`, `[rerank]`, `[title]`) stay in each project's `.engramdb/config.toml`, because vectors are stored per project. Each request sends the project's model settings to the daemon, so one daemon serves projects that use different models.

`enabled` and `use_for_cli` are global too, so the daemon is used the same way in every project. To skip the daemon for one command or one session, use `--in-process` or `ENGRAMDB_IN_PROCESS=1`.

### Moving from a project `[daemon]` section

Older versions read `[daemon]` from each project's `.engramdb/config.toml`. That made the daemon depend on whichever project spawned it first, and two projects with different `socket_path` values started two daemons. A project config that still has `[daemon]` keys still loads, but the keys are **ignored**:

- EngramDB logs a warning once per project config, naming each key and the global file that replaces it.
- `engramdb doctor` shows a warning on the project's "Config file" check.

To fix this, move the keys to the global file and delete the `[daemon]` section from the project config.

## Socket resolution

Highest priority wins:

1. `--socket <path>` CLI flag
2. `ENGRAMDB_DAEMON_SOCKET` environment variable
3. `socket_path` in the global `[daemon]` section
4. The default per-user path: `$XDG_RUNTIME_DIR/engramdb/daemon.sock` (Linux), else `<cache dir>/engramdb/daemon/daemon.sock` (macOS: `~/Library/Caches/engramdb/daemon/daemon.sock`), else `/tmp/engramdb-<uid>/daemon.sock`

On Unix the endpoint is a Unix domain socket at the resolved path. On Windows the daemon uses a named pipe instead: the resolved path is mapped to `\\.\pipe\engramdb-<hash>` (an explicit `\\.\pipe\...` value is used as-is).

## The daemon folder

The folder that holds the socket is the daemon's private folder. It holds three files:

- `daemon.sock` — the socket (or the name a Windows pipe is derived from).
- `daemon.reclaim.lock` — the lock that serializes taking over a stale socket.
- `daemon.log` — the log of an auto-spawned daemon. `engramdb doctor` shows its last lines.

On Unix, the daemon creates the folder with mode `0700`. If the folder already exists, it must be owned by you and have mode `0700`. Otherwise the daemon refuses to start, and models load in-process. The error names the folder and the fix (for example `chmod 700 <folder>`). An overridden socket moves the whole folder: `ENGRAMDB_DAEMON_SOCKET=/path/to/dir/d.sock` puts the lock and the log in `/path/to/dir/` too.

Under `$XDG_RUNTIME_DIR` the folder is cleared when you log out, so the log is too. The log only needs to explain the most recent daemon failure, so this is acceptable.

## Disabling

In the global config file (see [Configuration](#configuration)):

```toml
[daemon]
enabled = false        # disable the daemon entirely — MCP and CLI both load in-process
# use_for_cli = false  # keep the daemon for MCP, but never use it from the CLI
```

With `enabled = false` the MCP server and the CLI both load models in-process and never contact a daemon. To disable it for one session only, set `ENGRAMDB_IN_PROCESS=1`.

## Metrics

`engramdb stats --daemon` reports request counts and p50/p95/p99 latencies per operation (embed / nli / rerank), plus uptime and model-load counts. Metrics are persisted to the global LanceDB store, so figures stay accurate **across daemon restarts** and the command shows the last persisted snapshot even when no daemon is running.

`engramdb daemon status` additionally shows a `pings: N (last Xs ago)` line — the heartbeat activity for the *currently running* daemon. Unlike the request metrics above, ping counts are in-memory only (not persisted across restarts), so they appear only while a daemon is live.

## Troubleshooting

See [troubleshooting.md](./troubleshooting.md#daemon).
