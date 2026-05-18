# EngramDB

[![CI](https://github.com/egeapak/engramdb/actions/workflows/ci.yml/badge.svg)](https://github.com/egeapak/engramdb/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Project-scoped persistent memory for coding agents. Stores decisions, hazards, conventions, and context about your codebase so your agent remembers across sessions.

EngramDB gives your AI coding agent a memory layer that persists between conversations. It stores what the agent learns about your project — architectural decisions, known hazards, team conventions, debugging context — and surfaces relevant memories automatically when the agent reads or edits files.

## Features

- **Semantic search** — vector similarity search powered by LanceDB and all-MiniLM-L6-v2 embeddings
- **Automatic context injection** — hooks surface relevant memories when your agent touches files
- **Contradiction detection** — NLI-based challenge system flags conflicting information
- **Memory lifecycle** — criticality scoring, garbage collection, compression of stale memories
- **MCP server** — full Model Context Protocol integration (stdio and SSE transports)
- **Shared embedding daemon** — auto-spawned; loads models once machine-wide instead of once per agent session, with graceful in-process fallback
- **Claude Code plugin** — one-command install with hooks, MCP, and permissions
- **Offline-first** — all embeddings run locally via ONNX Runtime (or optionally Ollama)
- **Multiple output formats** — pretty, JSON, or plain text for scripting

## Quick Start

### Install

Build from source (requires Rust 1.75+):

```bash
cargo install --git https://github.com/egeapak/engramdb
```

### Set up Claude Code integration

The recommended way is via the plugin:

```bash
# Add the marketplace
/plugin marketplace add egeapak/engramdb

# Install the plugin
/plugin install engram@engramdb
```

Or set up manually with hooks and MCP in settings.json:

```bash
engramdb setup --global
```

For project-scoped setup (writes to `<project>/.claude/`):

```bash
engramdb setup
```

### Use the CLI directly

```bash
# Initialize a store in the current project
engramdb init

# Add a memory
engramdb add --type decision --title "Use PostgreSQL for persistence" \
  "Chose PostgreSQL over SQLite for concurrent write support."

# Find memories by keyword (filter mode requires a query signal)
engramdb query --mode filter "database choice"

# Rank memories relevant to a file (rank mode browses by context)
engramdb query --mode rank --path src/db/connection.rs
```

## How It Works

EngramDB stores memories as structured files (TOML + markdown) in `.engramdb/memories/` within your project. Each memory has:

- **Type** — decision, hazard, convention, context, preference, reference
- **Visibility** — project, team, or personal scope
- **Criticality** — 0.0 to 1.0 score that decays over time
- **Embeddings** — vector representations stored in a LanceDB index for semantic search

When integrated with Claude Code:

1. **SessionStart hook** — injects high-criticality memories into the conversation at startup
2. **PreToolUse hook** — when the agent reads/writes/edits a file, relevant memories are surfaced as context
3. **MCP server** — the agent can search, create, update, and manage memories through tool calls

## CLI Reference

| Command | Description |
|---------|-------------|
| `init` | Initialize a new EngramDB store |
| `add` | Add a new memory |
| `get` | Get a memory by ID |
| `query` | Unified search: `--mode filter` requires a query signal; `--mode rank` browses by context |
| `list` | List all memories |
| `update` | Update an existing memory |
| `delete` | Delete a memory |
| `challenge` | Challenge a memory's validity |
| `review` | Interactive review of challenged/stale memories |
| `stats` | Show store statistics |
| `doctor` | Check environment and store health |
| `gc` | Garbage collect low-relevance memories |
| `compress` | List compression candidates |
| `reindex` | Rebuild index and re-embed all memories |
| `migrate` | Migrate memory files to latest format |
| `rollback` | Roll back memory files to previous format |
| `serve` | Start the MCP server |
| `daemon` | Manage the shared embedding daemon (`run`/`status`/`stop`/`restart`) |
| `setup` | Set up Claude Code integration |
| `hook` | Claude Code plugin hook handler |
| `projects` | Manage registered EngramDB projects |
| `completions` | Generate shell completions |

`stats --daemon` shows the embedding daemon's cumulative request metrics
(falling back to the last persisted snapshot when no daemon is running), and
`doctor` includes a **Daemon** section reporting whether it's enabled and
running.

Use `engramdb <command> --help` for detailed options.

## MCP Tools

When running as an MCP server (`engramdb serve`), the following tools are available:

| Tool | Description |
|------|-------------|
| `query` | Unified search/retrieve. `mode: "filter"` narrows by query/logical/path/tags; `mode: "rank"` ranks memories by relevance to a context |
| `create` | Store a new memory |
| `get` | Fetch a specific memory by ID |
| `list` | List all memories with optional filters |
| `update` | Modify an existing memory |
| `delete` | Remove a memory |
| `challenge` | Flag a memory as potentially incorrect |
| `review` | List memories needing review |
| `resolve` | Accept, update, or delete a challenged memory |
| `stats` | Store statistics and health info |
| `doctor` | Environment and store diagnostics |
| `gc` | Garbage collect low-relevance memories |
| `reindex` | Rebuild the vector index |
| `compress_candidates` | List memories eligible for compression |
| `compress_apply` | Merge multiple memories into a summary |

## Configuration

EngramDB reads configuration from `.engramdb/config.toml`:

```toml
[embeddings]
backend = "auto"   # "auto", "onnx", or "ollama"

[scoring]
decay_rate = 0.01  # Daily criticality decay

[gc]
threshold = 0.1    # Minimum criticality to keep

[daemon]
enabled = true            # Delegate embedding/NLI/rerank to the shared daemon
idle_timeout_secs = 900   # Daemon exits after this long with no activity
# socket_path = "/run/user/1000/engramdb/daemon.sock"  # optional override
```

### Embedding backends

- **onnx** (default) — local ONNX Runtime with all-MiniLM-L6-v2, no external dependencies
- **ollama** — uses a local Ollama instance for embeddings (requires `ollama` running)
- **auto** — tries ONNX first, falls back to Ollama

Models are cached in the system cache directory (`~/Library/Caches/engramdb/models` on macOS).

## Embedding daemon

Each `engramdb serve` (stdio MCP) process is one-per-agent-session, so without
coordination every concurrent session loads its own copy of the embedding (and
optional NLI/reranker) models — hundreds of MB and a ~240 ms ONNX init each.

When `[daemon].enabled` is `true` (the default), MCP processes delegate **all**
model work to a single long-lived **daemon** over a per-user Unix domain
socket, so each model loads exactly once machine-wide. Storage stays in the MCP
process (it is already cross-process safe), so only inference is delegated.

- **Auto-spawned on demand.** You never start it manually. When an MCP process
  needs the daemon and none is reachable, it spawns one (`engramdb daemon run`)
  detached, waits briefly, and connects. Concurrent spawns are race-safe (only
  one binds the socket). The daemon exits after `idle_timeout_secs` with no
  activity; **the next MCP run simply spawns a fresh one**.
- **Graceful fallback.** If the daemon is disabled or unreachable, the MCP
  process loads models in-process exactly as before — operations never fail
  because of the daemon.
- **Manage it directly** (rarely needed):

  ```bash
  engramdb daemon status     # running? pid, uptime, request metrics
  engramdb daemon stop       # graceful shutdown (next MCP run respawns it)
  engramdb daemon restart    # stop + start a fresh one
  engramdb daemon run        # run the loop in the foreground (debugging)
  engramdb stats --daemon    # cumulative request metrics (persisted)
  ```

  The socket path resolves with precedence `--socket` flag >
  `ENGRAMDB_DAEMON_SOCKET` env > `[daemon].socket_path` config > the
  default per-user runtime path. `status`/`stop`/`restart`/`run` all
  accept `--socket` to target a non-default daemon.

Daemon request metrics are persisted to the global store's LanceDB, so
`stats --daemon` reports figures even when no daemon is currently running, and
counts stay cumulative across daemon restarts.

## Building from Source

```bash
git clone https://github.com/egeapak/engramdb
cd engramdb
cargo build --release
```

Run tests:

```bash
cargo nextest run
```

## Contributing

Contributions are welcome. Please open an issue to discuss significant changes before submitting a PR.

1. Fork the repository
2. Create a feature branch
3. Ensure `cargo fmt --all` and `cargo clippy --all-targets --all-features -- -D warnings` pass
4. Submit a pull request

## License

MIT License — see [LICENSE](LICENSE) for details.
