# Installation

EngramDB is a single Rust binary. Once installed, it covers the CLI, the MCP server, the embedding daemon, and the Claude Code hook handlers.

## Prerequisites

- **Rust 1.75 or later** to build from source. Get it from <https://rustup.rs>.
- **protoc** (Protocol Buffers compiler) — required by LanceDB's build.
  - macOS: `brew install protobuf`
  - Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`
  - Fedora: `sudo dnf install protobuf-compiler`
- Outbound network access on first run to download the embedding model (~90 MB). After that, engramdb is fully offline.

A reasonably recent x86_64 or aarch64 Linux/macOS/Windows machine. No GPU needed — the default embedding model runs on CPU via ONNX Runtime.

## Install

### From the GitHub repository

```bash
cargo install --git https://github.com/egeapak/engramdb
```

This builds with default features (`ollama` enabled). The binary lands in `~/.cargo/bin/engramdb`.

### Build from a local checkout

```bash
git clone https://github.com/egeapak/engramdb
cd engramdb
cargo build --release
# binary at target/release/engramdb
```

To install your local build onto your `PATH`:

```bash
cargo install --path .
```

### Feature flags

| Flag | Default | What it does |
|------|---------|--------------|
| `ollama` | on | Adds the Ollama embedding backend (uses `reqwest`). Turn off for a pure-ONNX, offline-only build with no extra deps: `cargo install --git ... --no-default-features`. |
| `coreml` | off | Apple Core ML execution provider for ONNX models (Neural Engine / GPU). macOS only. |
| `xnnpack` | off | XNNPACK CPU execution provider for ONNX. Useful for A/B benchmarking. |

## Verify

```bash
engramdb --version
engramdb doctor
```

`doctor` runs an environment check: it reports which embedding backend is available, where models are cached, whether the daemon is reachable, and platform-specific paths. If you see warnings about missing models, that's normal before your first store is created — the model downloads when first used.

## What gets installed where

EngramDB writes to platform-standard locations via the `dirs` crate. None of these are configurable through the CLI, but they all honor environment variables for tests / unusual setups.

| Purpose | macOS | Linux | Env override |
|---------|-------|-------|--------------|
| Models (embeddings, NLI, reranker) | `~/Library/Caches/engramdb/models/` | `~/.cache/engramdb/models/` | — |
| Global config | `~/Library/Application Support/engramdb/` | `~/.config/engramdb/` | `ENGRAMDB_CONFIG_DIR` |
| Global data + project registry | `~/Library/Application Support/engramdb/` | `~/.local/share/engramdb/` | `ENGRAMDB_DATA_DIR` |
| Daemon socket | `$XDG_RUNTIME_DIR/engramdb/daemon.sock` (Linux) or the cache dir (macOS) | same | `ENGRAMDB_DAEMON_SOCKET` |

Per-project state lives in `<project>/.engramdb/`:
- `manifest.toml` — project name, embedding fingerprint
- `config.toml` — scoring/retrieval/daemon config (optional)
- `memories/` — TOML-frontmatter markdown files (one per memory)

Personal-visibility memories and the LanceDB vector index live under `<global_data_dir>/projects/<project_id>/{personal,lancedb}/`. The project ID is a 16-char SHA-256-derived hex string. See [projects-and-worktrees.md](./projects-and-worktrees.md).

## Uninstall

```bash
cargo uninstall engramdb
# Optionally also remove all data:
rm -rf ~/.local/share/engramdb ~/.config/engramdb ~/.cache/engramdb   # Linux
rm -rf "~/Library/Application Support/engramdb" "~/Library/Caches/engramdb"   # macOS
```

Per-project `.engramdb/` directories must be removed manually from each project.

## Next steps

- **[quickstart.md](./quickstart.md)** — set up your first store and add a memory.
- **[claude-code.md](./claude-code.md)** — wire engramdb into Claude Code via the plugin or `engramdb setup`.
