# Installation

## Prerequisites

- **Rust 1.75 or later** to build from source. Get it from <https://rustup.rs>.
- **protoc** (Protocol Buffers compiler) — required by LanceDB's build.
  - macOS: `brew install protobuf`
  - Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`
  - Fedora: `sudo dnf install protobuf-compiler`
- Outbound network access on first run to download the embedding model (~90 MB). After that, engramdb is fully offline.

### Platform support

The default build uses **ONNX Runtime**, fetched as a prebuilt binary for **Linux (x86_64/aarch64)**, **Windows (x86_64/aarch64)**, and **Apple Silicon macOS (aarch64)** — the platforms with official release binaries.

**Intel Mac (`x86_64-apple-darwin`)** has no prebuilt ONNX Runtime 1.24 (the version required; Microsoft dropped x86_64 macOS builds after 1.23.x, and a Homebrew `onnxruntime` 1.23.x crashes at startup with `The requested API version [24] is not available`). On Intel Mac, EngramDB uses the **pure-Rust `tract` embedding backend** instead — no native runtime to install:

- **Prebuilt Intel-Mac release binaries just work** — they ship with the tract backend built in.
- **Building from source on Intel Mac:** use `cargo build --release --bin engramdb --no-default-features --features tract`. A default build there links an unusable ONNX Runtime and emits a build warning pointing you here.

### Building against a system ONNX Runtime (recommended where available)

By default the build downloads a prebuilt ONNX Runtime. That prebuilt executes
quantized models incorrectly on some AVX-512/AMX CPUs — the same text can embed
to an unrelated vector when the machine is busy, and embeddings are persisted.
Building against the ONNX Runtime your package manager installs avoids it:

```bash
brew install onnxruntime          # or your distro's equivalent
cargo build --release -p engram-cli --features system-onnxruntime
```

The feature makes the build locate `libonnxruntime` via `pkg-config`; any
version 1.24 or newer works. Verified with Homebrew's onnxruntime 1.28 and with
Microsoft's official release tarball. On **Intel Mac** this is currently the
only way to get a working ONNX path — Homebrew has an Intel bottle, while no
ONNX Runtime 1.24 prebuilt exists for `x86_64-apple-darwin`; without it the
build falls back to `tract`.

The tract backend uses the **fp32** MiniLM model (the int8 default does not load under tract), embeds at roughly **3× the latency** of ONNX (fine for on-demand memory writes/queries), and disables the optional NLI and T5-title features (ONNX-only); the optional cross-encoder reranker works on tract (`jina-reranker-v1-turbo-en` or `bge-reranker-base`, fp32) but stays off by default. Its vectors are numerically identical to ONNX fp32 (cosine ≈ 1.0). Because the fp32 model has a distinct fingerprint, a store first used on Intel Mac (or moved between an Intel and a non-Intel machine) will prompt a one-time `engramdb reindex --embeddings-only`. See [embeddings.md](./embeddings.md#backends).

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
