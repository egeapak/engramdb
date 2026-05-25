# The Shared Embedding Daemon

stdio MCP runs one process per agent session. Without coordination, every concurrent session loads its own copy of the embedding (and optional NLI / reranker) models — hundreds of MB and a ~240 ms ONNX init each. The daemon loads each model **once** machine-wide and serves inference to every MCP process over a per-user Unix socket. Storage stays in the MCP process; only inference is delegated.

## Lifecycle

MCP auto-spawns the daemon when needed (race-coordinated by an advisory file lock; concurrent spawns are safe). It exits after `idle_timeout_secs` (default 15 minutes) of inactivity; the next MCP run spawns a fresh one. **You never start it manually.**

If the daemon is disabled (`enabled = false`) or unreachable for any reason, the MCP server loads models in-process. **Operations never fail because of the daemon** — that's the contract.

## Commands

```bash
# Show whether a daemon is running, plus request metrics
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
enabled = false
```

The MCP server will load models in-process and never contact a daemon.

## Metrics

`engramdb stats --daemon` reports request counts and p50/p95/p99 latencies per operation (embed / nli / rerank), plus uptime and model-load counts. Metrics are persisted to the global LanceDB store, so figures stay accurate **across daemon restarts** and the command shows the last persisted snapshot even when no daemon is running.

## Troubleshooting

See [troubleshooting.md](./troubleshooting.md#daemon).
