# The Shared Embedding Daemon

EngramDB's MCP server is launched once per Claude Code session — stdio MCP is fundamentally one-process-per-agent. Without coordination, every concurrent session would load its own copy of the embedding model (and optional NLI / reranker models). That's hundreds of MB of RAM and a ~240 ms ONNX initialization **per session**.

The daemon solves this by being a single long-lived process that loads each model **once** and serves inference to every MCP process over a per-user Unix domain socket. The MCP processes still own storage (LanceDB is already cross-process safe via advisory locks) — only inference is delegated.

You almost never interact with the daemon directly. This page is for when you want to.

## Lifecycle (the short version)

1. The MCP server starts and reads `[daemon]` from `config.toml`.
2. If `enabled = true` (default), it tries to connect to the daemon socket.
3. If no daemon is reachable, the MCP server spawns one (`engramdb daemon run`) detached, waits briefly, and connects. Concurrent spawns are race-safe — only one process binds the socket; others retry the connection.
4. The daemon serves inference until `idle_timeout_secs` (default 15 minutes) elapses with no active connections. Then it exits.
5. The next MCP run spawns a fresh daemon. There is **no daemon to keep running manually.**

If the daemon is disabled (`enabled = false`) or unreachable for any reason, the MCP server loads models in-process and continues — operations never fail because of the daemon. This graceful fallback is part of the contract.

## When you'd want to touch it

- **Inspecting it.** Check whether it's running, what it's been doing, who's connected.
- **Forcing a reload.** You changed `[embeddings].provider`; you want the next MCP call to use the new model immediately.
- **Disabling it.** You want every MCP process self-contained — useful for debugging or for hermetic test environments.
- **Relocating the socket.** The default `sun_path` (~104 bytes) is too long for your home directory layout, or you want to isolate two daemons for testing.

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

Every component — clients, the server itself, `doctor`, `stats` — uses the same resolution helper, so they always agree on which socket the daemon lives at.

## Disabling

In `<project>/.engramdb/config.toml`:

```toml
[daemon]
enabled = false
```

The MCP server will load models in-process and never attempt to spawn or contact a daemon. Existing daemons keep running for other projects.

## Metrics

`engramdb stats --daemon` reports:
- whether a daemon is currently running, with PID and uptime,
- cumulative request counts per operation (embed / nli / rerank),
- p50/p95/p99 latencies per operation,
- bytes transferred,
- model load counts.

Metrics are persisted to the global LanceDB store on every flush (`[stats].flush_interval_secs`), so figures stay accurate **across daemon restarts** and the command shows the last persisted snapshot even when no daemon is currently running.

`engramdb doctor` includes a "Daemon" section that reports whether it's enabled, whether it's running, and how long it's been up.

## Troubleshooting

**`status` reports "not running" but `ps` shows a daemon process.** The daemon you see is probably bound to a different socket. Use `--socket` or check `ENGRAMDB_DAEMON_SOCKET`.

**Daemon fails to start on macOS.** Default socket paths can exceed the 104-byte `sun_path` limit if your home directory is deeply nested or contains spaces. Move the socket: `export ENGRAMDB_DAEMON_SOCKET=/tmp/engramdb-$USER.sock`.

**Daemon won't pick up new model config.** A long-running daemon caches loaded models forever. After changing `[embeddings].provider`, `[nli].model`, or `[rerank].model`, run `engramdb daemon restart`. The next request will load the new model.

**Inference seems slow.** Check `engramdb stats --daemon` for the latency histograms. If they look normal but startup is slow, you're likely paying the cold-start cost on every MCP call — confirm the daemon is staying up (`engramdb daemon status` between calls) and not being killed.

**Want to see what the daemon is doing in real time.** Stop the auto-spawned one (`engramdb daemon stop`) and run `engramdb daemon run` in a terminal — you'll see request logs on stderr. Configure log verbosity with `RUST_LOG=engramdb=debug`.
