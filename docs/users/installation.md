# Installation

## Prerequisites

- **Rust 1.75 or later** to build from source. Get it from <https://rustup.rs>.
- **protoc** (Protocol Buffers compiler) — required by LanceDB's build.
  - macOS: `brew install protobuf`
  - Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`
  - Fedora: `sudo dnf install protobuf-compiler`
- Outbound network access on first run to download the embedding model (~90 MB). After that, engramdb is fully offline.

### Platform support

ONNX Runtime is fetched as a prebuilt binary for **Linux (x86_64/aarch64)**, **Windows (x86_64/aarch64)**, and **Apple Silicon macOS (aarch64)**. **Intel Mac (`x86_64-apple-darwin`) has no upstream prebuilt runtime** — the build falls back to a system `libonnxruntime`, which must be **1.24.x** (API 24). A default Homebrew `onnxruntime` (1.23.x) will fail at startup with `The requested API version [24] is not available`; see [troubleshooting.md](./troubleshooting.md#build--install) for the `ORT_STRATEGY=compile` / `ORT_LIB_LOCATION` fix.

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

`doctor` reports the embedding backend, model cache path, daemon reachability, and platform paths. Missing-model warnings before your first store are normal — models download on first use.

## What gets installed where

EngramDB writes to platform-standard locations via the `dirs` crate. Each respects an environment-variable override:

| Purpose | macOS | Linux | Env override |
|---------|-------|-------|--------------|
| Models (embeddings, NLI, reranker) | `~/Library/Caches/engramdb/models/` | `~/.cache/engramdb/models/` | — |
| Global config | `~/Library/Application Support/engramdb/` | `~/.config/engramdb/` | `ENGRAMDB_CONFIG_DIR` |
| Global data + project registry | `~/Library/Application Support/engramdb/` | `~/.local/share/engramdb/` | `ENGRAMDB_DATA_DIR` |
| Daemon endpoint | `$XDG_RUNTIME_DIR/engramdb/daemon.sock` (Linux) or the cache dir (macOS); a named pipe (`\\.\pipe\engramdb-<hash>`) on Windows | same | `ENGRAMDB_DAEMON_SOCKET` |

Per-project state lives in `<project>/.engramdb/`. The vector index and personal-visibility memories live under `<global_data_dir>/projects/<project_id>/`. See [projects-and-worktrees.md](./projects-and-worktrees.md) for the full layout.

## Uninstall

```bash
cargo uninstall engramdb
# Optionally also remove all data:
rm -rf ~/.local/share/engramdb ~/.config/engramdb ~/.cache/engramdb   # Linux
rm -rf "~/Library/Application Support/engramdb" "~/Library/Caches/engramdb"   # macOS
```

Per-project `.engramdb/` directories are not touched — remove them manually.
