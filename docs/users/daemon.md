# The Shared Embedding Daemon

stdio MCP runs one process per agent session. Without coordination, every concurrent session loads its own copy of the embedding (and optional NLI / reranker) models — hundreds of MB and a ~240 ms ONNX init each. The daemon loads each model **once** machine-wide and serves inference to every MCP process over a per-user Unix socket. Storage stays in the MCP process; only inference is delegated. Model-needing CLI commands can use the same daemon too (see [CLI usage](#cli-usage)).

## Lifecycle

MCP auto-spawns the daemon when needed (race-coordinated by an advisory file lock; concurrent spawns are safe). **You never start it manually.**

- **Stays alive while sessions are connected.** Each MCP `serve` process runs a background **heartbeat** that pings the daemon every `idle_timeout_secs / 3` (minimum 30 s). Those pings reset the daemon's idle clock, so it stays resident as long as any agent session is running — not just for `idle_timeout_secs` after the last inference.
- **Reaps after the last session ends.** Once every session has exited, the pings stop and the daemon exits after `idle_timeout_secs` (default 15 minutes) of inactivity.
- **Self-heals.** If the daemon dies or is replaced (idle-exit, crash, manual restart, or a protocol-version change), the heartbeat re-spawns a fresh one and live sessions transparently route to it on their next request — no need to restart the agent.

If the daemon is disabled (`enabled = false`) or unreachable for any reason, MCP **and** the CLI load models in-process. **Operations never fail because of the daemon** — that's the contract.

## CLI usage

Model-needing CLI commands (`query`, `add` / `create`, `update`, `reindex --embeddings-only`) use a **running** daemon when one is reachable, so a warm daemon turns each command's ~240 ms cold model load into a sub-millisecond socket round-trip.

- **Connect-only by default.** The CLI uses a daemon only if one is *already* running; it does **not** spawn one (a one-shot command shouldn't leave a 15-minute daemon behind). With no daemon reachable, it loads models in-process.
- `--spawn-daemon` lets the CLI spawn (and warm) a daemon when none is running.
- `--in-process` — or `ENGRAMDB_IN_PROCESS=1`, or `[daemon].use_for_cli = false` — forces in-process loading and never contacts a daemon.
- `[daemon].enabled = false` disables the daemon for both CLI and MCP.

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

Each command accepts `--socket <path>` to target a non-default socket. `run` and `restart` also take `--idle-timeout <secs>`.

## Socket resolution

Highest priority wins:

1. `--socket <path>` CLI flag
2. `ENGRAMDB_DAEMON_SOCKET` environment variable
3. `[daemon].socket_path` in `config.toml`
4. The default per-user path: `$XDG_RUNTIME_DIR/engramdb/daemon.sock` (Linux) or under the per-user cache dir (macOS)

## Disabling

In `<project>/.engramdb/config.toml`:

```toml
[daemon]
enabled = false        # disable the daemon entirely — MCP and CLI both load in-process
# use_for_cli = false  # keep the daemon for MCP, but never use it from the CLI
```

With `enabled = false` the MCP server and the CLI both load models in-process and never contact a daemon.

## Metrics

`engramdb stats --daemon` reports request counts and p50/p95/p99 latencies per operation (embed / nli / rerank), plus uptime and model-load counts. Metrics are persisted to the global LanceDB store, so figures stay accurate **across daemon restarts** and the command shows the last persisted snapshot even when no daemon is running.

`engramdb daemon status` additionally shows a `pings: N (last Xs ago)` line — the heartbeat activity for the *currently running* daemon. Unlike the request metrics above, ping counts are in-memory only (not persisted across restarts), so they appear only while a daemon is live.

## Troubleshooting

See [troubleshooting.md](./troubleshooting.md#daemon).
